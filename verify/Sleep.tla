-------------------------------- MODULE Sleep --------------------------------
(***************************************************************************)
(* Sleeping, dozing and the battery, as kindil implements them.            *)
(*                                                                         *)
(* The app (src/ui.rs `Ctx::device`) feeds each event to the reducer       *)
(* `Mode::step` (src/system.rs) and runs the effects it returns. Here:     *)
(*                                                                         *)
(*  - `Step` is `Mode::step`, arm for arm, with the same mode, event and   *)
(*    effect names. Each event the app handles is one action named after   *)
(*    it (PowerButton, LightOff(saved), PanelSettled, ...): one call of    *)
(*    `Ctx::device`, and one line of a recorded trace (src/trace.rs).      *)
(*  - The effects (`Ctx::apply`) start workers and timers that deliver     *)
(*    events later: `pend` holds the events on their way, by kind.         *)
(*  - A doze wake (`redraw_doze`, `settle_wake`, `DrawWake`, `wait_drawn`) *)
(*    ends once: by its frame reaching the panel (Done, or Timed when the  *)
(*    panel can't say it finished), by its ceiling timer (NotSeen, which   *)
(*    is logged), or by the doze ending (`DrawWake::begin`).               *)
(*  - The host (src/kindle.rs, src/power.rs, src/suspend.rs): a nap      *)
(*    suspends once and only with its wake alarm armed; `power::sleep`     *)
(*    loops, by its `Why`: on battery with the 6-hourly check alarm (the   *)
(*    battery's sleep powers off without it), judges by the time whether  *)
(*    the alarm woke it (`alarm_woke`) and powers off at 1%; the battery's *)
(*    sleep ends only for the button or a charger (CHARGER_DETECT), and is *)
(*    no sleep at all with a charger in.                                   *)
(*  - The world: the power button, the cable (a charger or a computer)     *)
(*    whose `connected` file appears a little after the plug, the gauge    *)
(*    hovering around 5%, the RTC alarm, the panel, the fetch, the clock.  *)
(*                                                                         *)
(* Constants switch parts of the world on and off, so that each .cfg       *)
(* checks one area at a size that runs in seconds. With OpenWorld only the *)
(* reducer is modelled and any event can arrive at any time.               *)
(*                                                                         *)
(* Timing is abstracted into three assumptions, each a guard below:        *)
(*  - Quiet: the idle timer (2 minutes and more without a touch) comes     *)
(*    after every worker and timer of a few seconds armed before it.       *)
(*  - ChargerSettled: `connected` appears within seconds of the plug,      *)
(*    before anything that takes a minute or more (idle, an alarm).        *)
(*  - FreshExpires: the press that woke the device is reported within      *)
(*    WAKING_PRESS of the resume.                                          *)
(***************************************************************************)
EXTENDS Naturals, Sequences

CONSTANTS
    OpenWorld,       \* any event at any time, the reducer alone (for recorded traces)
    ExternalOpen,    \* the button, the idle timer, the battery and volumd at any time; the
                     \* app's own timers and workers as modelled (for the UI tests' traces)
    Power,           \* chargers and the gauge's low end: else on battery, never below 2%
    Drawing,         \* the doze wake's frame, worker, panel and waiter: else its ceiling only
    Holds,           \* maintenance windows and SSH sessions hold naps off (nap_or_hold)
    Usb,             \* computers and USB drive mode
    Lights,          \* the front light's level can be read (LightOff carries Some)
    IdleSleeps,      \* auto-sleep after minutes, besides Doze
    LowBattery,      \* the gauge goes down to 5% (and to 1% with Power): else it stays above
    CableWakes,      \* plugging or unplugging a cable wakes a suspended PW1
    Races,           \* a press can overtake the nap or sleep it interrupts (see NoRace)
    PressNearAlarm,  \* a press can come in the 2 s before the check alarm (alarm_woke's slack)
    AlarmCanFail,    \* arming the sleep's check alarm can fail (sleep_once, Alarm::Wanted)
    MaxPending       \* bound on events of one kind on their way

AsleepModes == {"AsleepDimming", "AsleepSettling", "AsleepSuspended"}
DozeModes == {"DozeDimming", "DozeSettling", "DozeNapping", "DozeDrawing"}
AwakeModes == {"Awake", "UsbDrive", "Returning"}
Modes == AsleepModes \cup DozeModes \cup AwakeModes

Effects == {"ShowSleep", "LightOff", "AwaitPanel", "Suspend", "Nap", "Redraw",
            "Wake", "Rouse", "Reconnect", "RestoreLight", "ShowUsbDrive",
            "ReleaseStorage", "AwaitDrive", "StorageBack", "DriveNotice"}
Ignored == {"none", "WakingPress", "InUsbDrive", "Stale"}

(* Events on their way: queued by a worker, a timer or the kernel. *)
Kinds == {"PowerButton", "BatteryEmpty", "LightOff", "PanelSettled", "Resumed",
          "SuspendFailed", "DriveComputer", "DriveKindle",
          "DriveSettledCur", "DriveSettledOld"}
(* What comes from outside the app: with ExternalOpen, at any time. *)
ExternalKinds == {"PowerButton", "BatteryEmpty", "DriveComputer", "DriveKindle"}
(* The timers and workers that finish within seconds. *)
ShortKinds == {"LightOff", "PanelSettled", "Resumed", "SuspendFailed",
               "DriveSettledCur", "DriveSettledOld"}

Cables == {"none"} \cup (IF Power THEN {"charger"} ELSE {}) \cup (IF Usb THEN {"computer"} ELSE {})
(* The gauge: above 5%, 2-5% (BATTERY_EMPTY), at most 1% (CRITICAL_PERCENT). *)
Levels == IF ~LowBattery THEN {"ok"}
          ELSE IF Power THEN {"ok", "low", "critical"} ELSE {"ok", "low"}

VARIABLES
    \* What `Ctx::device` sees and does; recorded in traces.
    mode,        \* `Mode`
    light,       \* the Sleep or Doze carries a light level (Some)
    fresh,       \* Awake for less than WAKING_PRESS
    effects,     \* the effects of the last step
    ignored,     \* why the last event changed nothing (`Ignored`), or "none"
    handled,     \* events handled, modulo 4: a trace's unlogged steps are never events
    \* The app around it.
    sleepWhy,    \* what put the app to sleep: "button", "idle", "battery"; "none" out of sleep
    screen,      \* "app", "sleep" or "usb" (`show_system`)
    pend,        \* events on their way, by kind
    autoSleep,   \* Settings: "Doze" or "Minutes"
    dashboard,   \* a Miqat dashboard worker runs
    \* The doze wake under way (`DrawWake`, `settle_wake`).
    wakeOn,      \* begun and not yet ended
    tick,        \* its tagged tick for the worker: "none", "fetching", "queued"
    shown,       \* its frame was handed to the window (`dashboard_shown`)
    accepted,    \* the panel accepted an update after that
    waiter,      \* the `wait_drawn` thread: "none", "frame", "panel", "idle", "sent"
    ceiling,     \* its ceiling timer is armed
    stuck,       \* the dashboard worker hangs for good
    syncing,     \* an `online` worker (Wi-Fi, then network time) is under way
    \* The host and the device.
    cpu,         \* "up", "suspended", "off"
    alarm,       \* the RTC wake alarm: "none", "nap", "check"
    wokeBy,      \* what woke the CPU last: "none", "alarm", "press", "cable"
    due,         \* the sleep under way has a due time (`suspend`'s `due`: on battery)
    napper,      \* the nap's thread (`Ctx::nap`): "idle", "park", "suspended"
    sleeper,     \* the sleep's thread (`power::sleep`): "idle", "park", "suspended",
                 \* "check" (woken by its alarm), "detect" (watching for the button or a charger)
    sleepFor,    \* the sleep's `Why`: "asked" or "battery"
    pressed,     \* the power button was pressed since the sleep last suspended (`presses`)
    holdTimer,   \* a hold's timer (nap_or_hold), or a short nap's (`after_nap`):
                 \* "none", "hold", "short"
    napVoid,     \* the app left the nap under way: its ticket is void (src/suspend.rs)
    sleepVoid,   \* the app left the sleep under way: its ticket is void
    \* The world.
    cable,       \* "none", "charger", "computer"
    connected,   \* the charger's `connected` file reads 1
    level,       \* the gauge, in Levels
    \* Ghosts, for the properties.
    pressWaiting,\* pressed while the app slept or dozed, and not awake since
    overlap,     \* a nap or sleep thread started while the last was still running
    badClaim,    \* a wake ended Done or Timed without its frame accepted by the panel
    longNap,     \* a nap was armed on a clock stepped back since its minute was taken
    misjudged    \* a sleep took a press for its check alarm

observed == <<mode, light, fresh, effects, ignored, handled>>
app == <<sleepWhy, screen, pend, autoSleep, dashboard>>
draw == <<wakeOn, tick, shown, accepted, waiter, ceiling, stuck, syncing>>
host == <<cpu, alarm, wokeBy, due, napper, sleeper, sleepFor, pressed, holdTimer, napVoid, sleepVoid>>
world == <<cable, connected, level>>
ghosts == <<pressWaiting, overlap, badClaim, longNap, misjudged>>
vars == <<observed, app, draw, host, world, ghosts>>

(* What the next steps can do depends on all but the last step's effects, *)
(* why it was ignored and the count, which are there for traces.          *)
View == <<mode, light, fresh, app, draw, host, world, ghosts>>

-----------------------------------------------------------------------------
(* `Mode::step`, arm for arm (src/system.rs). `l` is the light the mode    *)
(* carries, `f` whether it is Awake within WAKING_PRESS, `p` the event's   *)
(* parameter: LightOff's level is Some; DriveNotice's notice gives the     *)
(* storage back; DriveSettled's instant is the Returning's own.            *)

Opt(c, x) == IF c THEN <<x>> ELSE <<>>
(* `Sleep::light`, `Doze::light`: None while the light is being switched off. *)
LightOf(m, l) == IF m \in {"AsleepDimming", "DozeDimming"} THEN FALSE ELSE l
(* `Doze::napped`. *)
Napped(m) == m \in {"DozeNapping", "DozeDrawing"}
(* `wake`. *)
WakeFx(m, l) == <<"Wake">> \o Opt(LightOf(m, l), "RestoreLight")

To(next, l, f, fx) == [ok |-> TRUE, why |-> "none", next |-> next, light |-> l, fresh |-> f, fx |-> fx]
Ign(why) == [ok |-> FALSE, why |-> why, next |-> "", light |-> FALSE, fresh |-> FALSE, fx |-> <<>>]

Step(m, l, f, e, p) ==
    IF m = "Awake" /\ e = "PowerButton" /\ f THEN Ign("WakingPress")
    ELSE IF m = "Awake" /\ e \in {"PowerButton", "Idle", "BatteryEmpty"}
        THEN To("AsleepDimming", FALSE, FALSE, <<"ShowSleep", "LightOff">>)
    ELSE IF m = "Awake" /\ e = "Doze" THEN To("DozeDimming", FALSE, FALSE, <<"LightOff">>)
    ELSE IF m = "DozeDimming" /\ e = "LightOff" THEN To("DozeSettling", p, FALSE, <<"AwaitPanel">>)
    ELSE IF m \in {"DozeSettling", "DozeDrawing"} /\ e = "PanelSettled"
        THEN To("DozeNapping", l, FALSE, <<"Nap">>)
    ELSE IF m = "DozeNapping" /\ e \in {"Resumed", "SuspendFailed"}
        THEN To("DozeDrawing", l, FALSE, <<"Redraw">>)
    ELSE IF m \in DozeModes /\ e = "PowerButton"
        THEN To("Awake", FALSE, TRUE,
                <<"Rouse">> \o Opt(LightOf(m, l), "RestoreLight") \o Opt(Napped(m), "Reconnect"))
    ELSE IF m = "DozeDimming" /\ e = "BatteryEmpty" THEN To("AsleepDimming", FALSE, FALSE, <<"ShowSleep">>)
    ELSE IF m \in {"DozeSettling", "DozeDrawing"} /\ e = "BatteryEmpty"
        THEN To("AsleepSettling", l, FALSE, <<"ShowSleep", "AwaitPanel">>)
    ELSE IF m \in DozeModes /\ e = "DriveComputer"
        THEN To("UsbDrive", FALSE, FALSE, WakeFx(m, l) \o <<"ReleaseStorage", "ShowUsbDrive">>)
    ELSE IF m = "AsleepDimming" /\ e = "LightOff" THEN To("AsleepSettling", p, FALSE, <<"AwaitPanel">>)
    ELSE IF m = "Awake" /\ e = "LightOff" THEN To("Awake", FALSE, f, Opt(p, "RestoreLight"))
    ELSE IF m = "AsleepSettling" /\ e = "PanelSettled" THEN To("AsleepSuspended", l, FALSE, <<"Suspend">>)
    ELSE IF m \in AsleepModes /\ e \in {"PowerButton", "Resumed"}
        THEN To("Awake", FALSE, TRUE, WakeFx(m, l) \o Opt(m = "AsleepSuspended", "Reconnect"))
    ELSE IF m = "AsleepSuspended" /\ e = "SuspendFailed" THEN To(m, l, FALSE, <<>>)
    ELSE IF e = "StorageLeaving" THEN To(m, l, f, <<"ReleaseStorage">>)
    ELSE IF e = "DriveNotice" THEN To(m, l, f, Opt(p, "StorageBack") \o <<"DriveNotice">>)
    ELSE IF m = "Awake" /\ e = "DriveComputer"
        THEN To("UsbDrive", FALSE, FALSE, <<"ReleaseStorage", "ShowUsbDrive">>)
    ELSE IF m \in AsleepModes /\ e = "DriveComputer"
        THEN To("UsbDrive", FALSE, FALSE, WakeFx(m, l) \o <<"ReleaseStorage", "ShowUsbDrive">>)
    ELSE IF m = "Returning" /\ e = "DriveComputer" THEN To("UsbDrive", FALSE, FALSE, <<>>)
    ELSE IF m = "UsbDrive" /\ e = "DriveKindle" THEN To("Returning", FALSE, FALSE, <<"AwaitDrive">>)
    ELSE IF m = "Returning" /\ e = "DriveSettled" /\ p THEN To("Awake", FALSE, TRUE, <<"Wake", "StorageBack">>)
    ELSE IF m \in {"UsbDrive", "Returning"} /\ e = "PowerButton" THEN Ign("InUsbDrive")
    ELSE Ign("Stale")

Has(fx, x) == \E i \in 1..Len(fx) : fx[i] = x

-----------------------------------------------------------------------------
Init ==
    /\ mode = "Awake"          \* Mode::start: not a wake, no press to swallow
    /\ light = FALSE
    /\ fresh = FALSE
    /\ effects = <<>>
    /\ ignored = "none"
    /\ handled = 0
    /\ sleepWhy = "none"
    /\ screen = "app"
    /\ pend = [k \in Kinds |-> 0]
    \* With ExternalOpen the settings, the cable and the gauge make no difference.
    /\ autoSleep \in (IF IdleSleeps /\ ~ExternalOpen THEN {"Doze", "Minutes"} ELSE {"Doze"})
    /\ dashboard \in (IF Drawing THEN BOOLEAN ELSE {FALSE})
    /\ wakeOn = FALSE
    /\ tick = "none"
    /\ shown = FALSE
    /\ accepted = FALSE
    /\ waiter = "none"
    /\ ceiling = FALSE
    /\ stuck = FALSE
    /\ syncing = FALSE
    /\ cpu = "up"
    /\ alarm = "none"
    /\ wokeBy = "none"
    /\ due = FALSE
    /\ napper = "idle"
    /\ sleeper = "idle"
    /\ sleepFor = "asked"
    /\ pressed = FALSE
    /\ holdTimer = "none"
    /\ napVoid = FALSE
    /\ sleepVoid = FALSE
    /\ cable \in (IF ExternalOpen THEN {"none"} ELSE Cables)
    /\ connected = (cable # "none")
    /\ level \in (IF ExternalOpen THEN {"ok"} ELSE Levels)
    /\ pressWaiting = FALSE
    /\ overlap = FALSE
    /\ badClaim = FALSE
    /\ longNap = FALSE
    /\ misjudged = FALSE

Add(f, k, n) == [f EXCEPT ![k] = IF @ + n > MaxPending THEN MaxPending ELSE @ + n]

(* `battery_event` on a read of the gauge and `connected`: refresh_status  *)
(* (the minute clock, a page, a wake's catch_up) or refresh_battery (a     *)
(* full doze redraw). The read is a background thread; what it delivers    *)
(* is on its way in `pend` like any other event.                           *)
Empty == level \in {"low", "critical"} /\ ~connected
Read(p) == IF Empty THEN Add(p, "BatteryEmpty", 1) ELSE p

-----------------------------------------------------------------------------
(* One call of `Ctx::device(e)`: the step, then its effects (`Ctx::apply`). *)

(* `DrawWake::begin`: ShowSleep, ShowUsbDrive, Rouse and Wake end the doze *)
(* wake under way; Redraw (`redraw_doze`) ends it and starts the next: the *)
(* tick for the worker (after a fetch, if online), the `wait_drawn` thread *)
(* and the ceiling timer. Reconnect and an online wake start `online`.     *)
Begins(fx) == \E x \in {"ShowSleep", "ShowUsbDrive", "Rouse", "Wake", "Redraw"} : Has(fx, x)

DrawEffects(fx, win) ==
    /\ syncing' = (syncing \/ Has(fx, "Reconnect") \/ (Has(fx, "Redraw") /\ win = "Online"))
    /\ IF Has(fx, "Redraw") THEN
           /\ wakeOn' = TRUE
           /\ tick' = IF ~dashboard THEN "none" ELSE IF win = "Draw" THEN "queued" ELSE "fetching"
           \* Without a dashboard the redraw is the clock's (`tick_clock`), any panel update after it.
           /\ shown' = (Drawing /\ ~dashboard)
           /\ accepted' = FALSE
           /\ waiter' = IF Drawing THEN "frame" ELSE "none"
           /\ ceiling' = TRUE
       ELSE IF Begins(fx) THEN
           /\ wakeOn' = FALSE
           /\ tick' = "none"
           /\ shown' = FALSE
           /\ accepted' = FALSE
           /\ waiter' = "none"
           /\ ceiling' = FALSE
       ELSE UNCHANGED <<wakeOn, tick, shown, accepted, waiter, ceiling>>

(* Effect::Nap (`nap_or_hold`): held awake (a hold's timer), a nap too     *)
(* short to suspend (`after(left, Resumed)`), or a nap (`Ctx::nap`).       *)
NapWays == {"nap", "short"} \cup (IF Holds THEN {"hold"} ELSE {})

Apply(fx, before, leaving) ==
    \E win \in (IF dashboard THEN {"Draw", "Online"} ELSE {"Draw"}),
       full \in BOOLEAN,
       how \in NapWays :
    LET reads == (Has(fx, "Redraw") /\ (full \/ win = "Online")) \/ Has(fx, "Wake") \/ Has(fx, "Rouse")
        p1 == IF Has(fx, "LightOff") THEN Add(before, "LightOff", 1) ELSE before
        p2 == IF Has(fx, "AwaitPanel") THEN Add(p1, "PanelSettled", 1) ELSE p1
        p3 == p2
        \* AwaitDrive: the timers armed before carry another instant.
        p4 == IF Has(fx, "AwaitDrive")
              THEN [Add(p3, "DriveSettledOld", p3["DriveSettledCur"]) EXCEPT !["DriveSettledCur"] = 1]
              ELSE p3
    IN
    /\ pend' = IF reads THEN Read(p4) ELSE p4
    /\ screen' = IF Has(fx, "ShowUsbDrive") THEN "usb"
                 ELSE IF Has(fx, "ShowSleep") THEN "sleep"
                 ELSE IF Has(fx, "Wake") THEN "app"
                 ELSE screen
    /\ napper' = IF Has(fx, "Nap") /\ how = "nap" THEN "park" ELSE napper
    /\ holdTimer' = IF Has(fx, "Nap") THEN (IF how = "nap" THEN "none" ELSE how) ELSE holdTimer
    /\ sleeper' = IF Has(fx, "Suspend") THEN "park" ELSE sleeper
    \* `leaves_suspend` voids the tickets taken so far; a nap or sleep set out
    \* now takes a good one.
    /\ napVoid' = IF Has(fx, "Nap") /\ how = "nap" THEN FALSE ELSE napVoid \/ leaving
    /\ sleepVoid' = IF Has(fx, "Suspend") THEN FALSE ELSE sleepVoid \/ leaving
    \* Effect::Suspend(why): the sleep's `Why`.
    /\ sleepFor' = IF Has(fx, "Suspend")
                   THEN (IF sleepWhy = "battery" THEN "battery" ELSE "asked")
                   ELSE sleepFor
    \* A thread whose ticket is void ends without suspending: it can overlap.
    /\ overlap' = (overlap \/ (Has(fx, "Nap") /\ how = "nap" /\ napper # "idle"
                                 /\ ~(napVoid \/ leaving))
                           \/ (Has(fx, "Suspend") /\ sleeper # "idle"
                                 /\ ~(sleepVoid \/ leaving)))
    /\ DrawEffects(fx, win)

host_rest == <<cpu, alarm, wokeBy, due, pressed>>

(* `before`: `pend` with the delivered event taken off. *)
Handle(e, p, before) ==
    LET r == Step(mode, light, fresh, e, p) IN
    /\ effects' = r.fx
    /\ ignored' = r.why
    /\ handled' = (handled + 1) % 4
    /\ IF r.ok
       THEN /\ mode' = r.next
            /\ light' = r.light
            /\ fresh' = r.fresh
            /\ sleepWhy' = IF r.next \notin AsleepModes THEN "none"
                           ELSE IF mode \in AsleepModes THEN sleepWhy
                           ELSE CASE e = "PowerButton" -> "button"
                                  [] e = "Idle" -> "idle"
                                  [] OTHER -> "battery"
            /\ pressWaiting' = (pressWaiting /\ r.next \notin AwakeModes)
            /\ Apply(r.fx, before,
                     mode \in {"DozeNapping", "AsleepSuspended"}
                     /\ r.next \notin {"DozeNapping", "AsleepSuspended"})
       ELSE /\ pend' = before
            /\ UNCHANGED <<mode, light, fresh, sleepWhy, pressWaiting, screen, napper,
                           holdTimer, napVoid, sleepVoid, sleeper, sleepFor, overlap, wakeOn, tick, shown, accepted,
                           waiter, ceiling, syncing>>
    /\ UNCHANGED <<autoSleep, dashboard, stuck, host_rest, world, badClaim, longNap, misjudged>>


(* An event of kind `k` on its way is delivered. *)
Deliver(k, e, p) ==
    IF OpenWorld \/ (ExternalOpen /\ k \in ExternalKinds) THEN Handle(e, p, pend)
    ELSE /\ pend[k] > 0
         /\ Handle(e, p, [pend EXCEPT ![k] = @ - 1])

ChargerSettled == connected = (cable # "none")

Quiet == /\ \A k \in ShortKinds : pend[k] = 0
         /\ napper = "idle" /\ sleeper = "idle"
         /\ ChargerSettled

(* A press, or volumd handing the storage to a computer, on its way.      *)
Waking == pend["PowerButton"] > 0 \/ pend["DriveComputer"] > 0

(* Without Races, the app handles a press (or a computer taking the      *)
(* storage) only once the nap or sleep it interrupts has reported back:    *)
(* no nap or sleep thread under way (one that sets out after the press     *)
(* finds the kernel refusing to suspend) and no nap's end on its way. With *)
(* Races, a press can come while a thread parks Wi-Fi or is still          *)
(* returning (catch_up polls the RTC for up to 1.5 s), or before a short   *)
(* nap's timer.                                                            *)
NoRace == \/ Races \/ OpenWorld
          \/ /\ napper = "idle" /\ sleeper = "idle"
             /\ pend["Resumed"] = 0 /\ pend["SuspendFailed"] = 0

PowerButton == cpu = "up" /\ NoRace /\ Deliver("PowerButton", "PowerButton", FALSE)
(* `check_idle` calls `Ctx::device` itself, once a minute while Awake. *)
Idle == /\ cpu = "up"
        /\ OpenWorld \/ ExternalOpen \/ (mode = "Awake" /\ autoSleep = "Minutes" /\ Quiet)
        /\ Handle("Idle", FALSE, pend)
Doze == /\ cpu = "up"
        /\ OpenWorld \/ ExternalOpen \/ (mode = "Awake" /\ autoSleep = "Doze" /\ Quiet)
        /\ Handle("Doze", FALSE, pend)
BatteryEmpty == cpu = "up" /\ Deliver("BatteryEmpty", "BatteryEmpty", FALSE)
LightOff(saved) == cpu = "up" /\ (Lights \/ OpenWorld \/ ~saved) /\ Deliver("LightOff", "LightOff", saved)
PanelSettled == cpu = "up" /\ Deliver("PanelSettled", "PanelSettled", FALSE)
Resumed == cpu = "up" /\ Deliver("Resumed", "Resumed", FALSE)
SuspendFailed == cpu = "up" /\ Deliver("SuspendFailed", "SuspendFailed", FALSE)
DriveComputer == cpu = "up" /\ NoRace /\ Deliver("DriveComputer", "DriveComputer", FALSE)
DriveKindle == cpu = "up" /\ Deliver("DriveKindle", "DriveKindle", FALSE)
(* `current`: the instant is the Returning's own. *)
DriveSettled(current) ==
    /\ cpu = "up"
    /\ IF OpenWorld THEN Handle("DriveSettled", current, pend)
       ELSE \/ current = (mode = "Returning") /\ Deliver("DriveSettledCur", "DriveSettled", current)
            \/ ~current /\ Deliver("DriveSettledOld", "DriveSettled", current)
(* volumd's userstoreIsLikelyToUnMount, and notices of kindil's own drive  *)
(* mode: they change no mode.                                              *)
StorageLeaving == (OpenWorld \/ Usb) /\ cpu = "up" /\ Handle("StorageLeaving", FALSE, pend)
DriveNotice(back) == (OpenWorld \/ Usb) /\ cpu = "up" /\ Handle("DriveNotice", back, pend)

-----------------------------------------------------------------------------
(* Everything else; none of it is a call of `Ctx::device`.                 *)

(* WAKING_PRESS passes, once the press that woke the device is reported.  *)
FreshExpires ==
    /\ mode = "Awake" /\ fresh
    /\ OpenWorld \/ pend["PowerButton"] = 0
    /\ fresh' = FALSE
    /\ UNCHANGED <<mode, light, effects, ignored, handled, app, draw, host, world, ghosts>>

(* The user presses the power button: the kernel resumes a suspended       *)
(* device and reports the press (`power_button`).                          *)
PressButton ==
    /\ ~OpenWorld
    /\ cpu # "off"
    /\ pend["PowerButton"] < MaxPending
    /\ ~ExternalOpen \/ cpu = "suspended"
    /\ pend' = IF ExternalOpen THEN pend ELSE Add(pend, "PowerButton", 1)
    /\ IF cpu = "suspended" THEN cpu' = "up" /\ wokeBy' = "press" ELSE UNCHANGED <<cpu, wokeBy>>
    /\ pressWaiting' = (pressWaiting \/ mode \in AsleepModes \cup DozeModes)
    \* The listener counts the press (`PRESSES`).
    /\ pressed' = (pressed \/ sleeper \in {"suspended", "check", "detect"})
    /\ UNCHANGED <<observed, sleepWhy, screen, autoSleep, dashboard, draw,
                   alarm, due, napper, sleeper, holdTimer, napVoid, sleepVoid, sleepFor, world, overlap, badClaim, longNap,
                   misjudged>>

CableWake ==
    IF CableWakes /\ cpu = "suspended" THEN cpu' = "up" /\ wokeBy' = "cable"
    ELSE UNCHANGED <<cpu, wokeBy>>

(* A charger or a computer is plugged in; the charger driver makes         *)
(* `connected` a little later. volumd hands the storage to a computer.     *)
Plug(c) ==
    /\ ~OpenWorld /\ ~ExternalOpen
    /\ cable = "none" /\ cpu # "off"
    /\ cable' = c
    /\ pend' = IF c = "computer" THEN Add(pend, "DriveComputer", 1) ELSE pend
    /\ CableWake
    /\ UNCHANGED <<observed, sleepWhy, screen, autoSleep, dashboard, draw,
                   alarm, due, napper, sleeper, holdTimer, napVoid, sleepVoid, sleepFor, pressed, connected, level, ghosts>>

Unplug ==
    /\ ~OpenWorld /\ ~ExternalOpen
    /\ cable # "none" /\ cpu # "off"
    /\ cable' = "none"
    /\ connected' = FALSE
    /\ pend' = IF cable = "computer" THEN Add(pend, "DriveKindle", 1) ELSE pend
    /\ CableWake
    /\ UNCHANGED <<observed, sleepWhy, screen, autoSleep, dashboard, draw,
                   alarm, due, napper, sleeper, holdTimer, napVoid, sleepVoid, sleepFor, pressed, level, ghosts>>

ConnectedAppears ==
    /\ ~OpenWorld /\ ~ExternalOpen
    /\ cable # "none" /\ ~connected
    /\ connected' = TRUE
    /\ UNCHANGED <<observed, app, draw, host, cable, level, ghosts>>

(* The gauge moves: around the 5% threshold either way, down to 1% only on *)
(* battery, back up from it only on a charger.                             *)
Gauge ==
    /\ ~OpenWorld /\ ~ExternalOpen
    /\ cpu # "off"
    /\ \E to \in Levels :
        /\ to # level
        /\ \/ {level, to} = {"ok", "low"}
           \/ level = "low" /\ to = "critical" /\ cable = "none"
           \/ level = "critical" /\ to = "low" /\ cable # "none"
        /\ level' = to
    /\ UNCHANGED <<observed, app, draw, host, cable, connected, ghosts>>

AlarmFires ==
    /\ ~OpenWorld
    /\ cpu = "suspended" /\ alarm # "none"
    /\ ChargerSettled
    /\ cpu' = "up" /\ wokeBy' = "alarm"
    /\ UNCHANGED <<observed, app, draw, alarm, due, napper, sleeper, holdTimer, napVoid, sleepVoid, sleepFor, pressed, world, ghosts>>

(* The minute clock (`arm_clock`, not while dozing) or a page refreshes    *)
(* the status bar, reading the battery.                                    *)
RefreshStatus ==
    /\ ~OpenWorld /\ ~ExternalOpen
    /\ cpu = "up" /\ mode \notin DozeModes
    /\ Empty /\ pend["BatteryEmpty"] = 0
    /\ pend' = Read(pend)
    /\ UNCHANGED <<observed, sleepWhy, screen, autoSleep, dashboard, draw, host, world, ghosts>>

(* A suspend can be refused (the kernel, or a nap's alarm that can't be   *)
(* armed) at any time, and is when a press is pending (without Races).     *)
Refusal == TRUE
Aborts == ~Races /\ Waking

(* `Ctx::nap`'s thread, after parking Wi-Fi: `KindleHost::nap` arms the    *)
(* required alarm, reads it back and suspends; or refuses (no alarm, or    *)
(* the kernel said no), and SuspendFailed comes after the minute. With its *)
(* ticket void (the app left the nap) it doesn't suspend, and what it says *)
(* is dropped.                                                             *)
NapperSuspends ==
    /\ ~OpenWorld
    /\ cpu = "up" /\ napper = "park"
    /\ Races \/ ~Waking
    /\ IF napVoid
       THEN napper' = "idle" /\ UNCHANGED <<cpu, alarm, wokeBy>>
       ELSE napper' = "suspended" /\ cpu' = "suspended" /\ alarm' = "nap" /\ wokeBy' = "none"
    /\ UNCHANGED <<observed, app, draw, due, sleeper, holdTimer, napVoid, sleepVoid, sleepFor, pressed, world, ghosts>>

NapperRefused ==
    /\ ~OpenWorld
    /\ cpu = "up" /\ napper = "park"
    /\ Refusal
    /\ napper' = "idle"
    /\ pend' = IF napVoid THEN pend ELSE Add(pend, "SuspendFailed", 1)
    /\ UNCHANGED <<observed, sleepWhy, screen, autoSleep, dashboard, draw,
                   cpu, alarm, wokeBy, due, sleeper, holdTimer, napVoid, sleepVoid, sleepFor, pressed, world, ghosts>>

(* Back from the nap, whatever woke it: the alarm moved away, Resumed. *)
NapperResumes ==
    /\ ~OpenWorld
    /\ cpu = "up" /\ napper = "suspended"
    /\ napper' = "idle" /\ alarm' = "none" /\ wokeBy' = "none"
    /\ pend' = IF napVoid THEN pend ELSE Add(pend, "Resumed", 1)
    /\ UNCHANGED <<observed, sleepWhy, screen, autoSleep, dashboard, draw,
                   cpu, due, sleeper, holdTimer, napVoid, sleepVoid, sleepFor, pressed, world, ghosts>>

(* `power::sleep`'s loop, after parking Wi-Fi (src/power.rs, through     *)
(* src/suspend.rs): the check alarm, CHECK_ASLEEP ahead, on a charger too. *)
(* A sleep the user asked for still sleeps if it can't be armed            *)
(* (`Wake::AlarmOrButton`), until the button; the battery's doesn't        *)
(* (`Wake::Alarm`): no alarm, no suspend, and it powers off. On a charger  *)
(* the battery's isn't a sleep at all (`Slept::Charging`: Resumed).        *)
SleeperSuspends ==
    /\ ~OpenWorld
    /\ cpu = "up" /\ sleeper = "park"
    /\ Races \/ ~Waking \/ (connected /\ sleepFor = "battery")
    /\ IF connected /\ sleepFor = "battery"
       THEN /\ sleeper' = "idle" /\ pend' = Add(pend, "Resumed", 1)
            /\ UNCHANGED <<cpu, alarm, wokeBy, due, pressed>>
       ELSE \E armed \in (IF AlarmCanFail THEN BOOLEAN ELSE {TRUE}) :
            IF ~connected /\ ~armed /\ sleepFor = "battery"
            \* `charger_comes`: off only once no charger turned up in CHARGER_DETECT.
            THEN /\ ChargerSettled
                 /\ cpu' = "off" /\ sleeper' = "idle"
                 /\ UNCHANGED <<alarm, wokeBy, due, pressed, pend>>
            \* A void ticket: no suspend (`Back::cancelled`), and nothing said.
            ELSE IF sleepVoid
            THEN /\ sleeper' = "idle"
                 /\ UNCHANGED <<cpu, alarm, wokeBy, due, pressed, pend>>
            ELSE \* Only an armed alarm can wake it (`Back::armed`).
                 /\ due' = armed
                 /\ alarm' = IF armed THEN "check" ELSE "none"
                 /\ sleeper' = "suspended" /\ cpu' = "suspended" /\ wokeBy' = "none"
                 /\ pressed' = FALSE
                 /\ UNCHANGED pend
    /\ UNCHANGED <<observed, sleepWhy, screen, autoSleep, dashboard, draw, napper, sleepFor,
                   holdTimer, napVoid, sleepVoid, world, ghosts>>

(* The kernel refuses the suspend: the battery's sleep powers off (its     *)
(* check would never come) unless a charger turns up within CHARGER_DETECT *)
(* (`charger_comes`); one the user asked for reports SuspendFailed.        *)
SleeperRefused ==
    /\ ~OpenWorld
    /\ cpu = "up" /\ sleeper = "park"
    /\ Refusal
    \* The battery's sleep looks for a charger first: then there is no suspend.
    /\ ~(connected /\ sleepFor = "battery")
    \* A void ticket never reaches the kernel.
    /\ ~sleepVoid
    /\ sleeper' = "idle"
    /\ IF sleepFor = "battery"
       THEN /\ ChargerSettled
            /\ IF connected THEN cpu' = cpu /\ pend' = Add(pend, "Resumed", 1)
                            ELSE cpu' = "off" /\ pend' = pend
       ELSE cpu' = cpu /\ pend' = Add(pend, "SuspendFailed", 1)
    /\ UNCHANGED <<observed, sleepWhy, screen, autoSleep, dashboard, draw,
                   alarm, wokeBy, due, napper, holdTimer, napVoid, sleepVoid, sleepFor, pressed, world, ghosts>>

(* Back from a sleep: it was the alarm if one was armed (`due`), it is its *)
(* time by `alarm_woke` (from 2 s before), and the power button wasn't     *)
(* pressed since the suspend (`pressed_within` BUTTON_GRACE: the listener  *)
(* counts a press within it). The time alone also says so for a press in  *)
(* the alarm's last 2 s (PressNearAlarm). Else a sleep the user asked for  *)
(* wakes the app (Resumed); the battery's watches for the button or a      *)
(* charger (`woke`).                                                       *)
SleeperResumes ==
    /\ ~OpenWorld
    /\ cpu = "up" /\ sleeper = "suspended"
    /\ alarm' = "none" /\ wokeBy' = "none" /\ due' = FALSE
    \* Left by the app meanwhile (`cancelled`): over, and nothing said.
    /\ IF sleepVoid THEN sleeper' = "idle" /\ pend' = pend /\ UNCHANGED misjudged ELSE
       \E late \in (IF wokeBy = "alarm" THEN {TRUE}
                    ELSE IF alarm = "none" \/ PressNearAlarm THEN BOOLEAN
                    ELSE {FALSE}) :
         /\ IF due /\ late /\ ~pressed
            THEN sleeper' = "check" /\ pend' = pend
            \* (With ExternalOpen the button and the charger are open too: no watch.)
            ELSE IF sleepFor = "battery" /\ ~ExternalOpen
            THEN sleeper' = "detect" /\ pend' = pend
            ELSE sleeper' = "idle" /\ pend' = Add(pend, "Resumed", 1)
         /\ misjudged' = (misjudged \/ (due /\ late /\ ~pressed /\ wokeBy = "press"))
    /\ UNCHANGED <<observed, sleepWhy, screen, autoSleep, dashboard, draw,
                   cpu, napper, holdTimer, napVoid, sleepVoid, sleepFor, pressed, world, pressWaiting, overlap,
                   badClaim, longNap>>

(* `woke`: for CHARGER_DETECT, a press since the suspend or a connected     *)
(* charger wakes the app; neither, and it sleeps on, the charge screen     *)
(* untouched. CHARGER_DETECT outlasts the charger driver (ChargerSettled). *)
SleeperDetects ==
    /\ ~OpenWorld
    /\ cpu = "up" /\ sleeper = "detect"
    /\ ChargerSettled
    /\ IF pressed \/ connected
       THEN sleeper' = "idle" /\ pend' = IF sleepVoid THEN pend ELSE Add(pend, "Resumed", 1)
       ELSE sleeper' = "park" /\ pend' = pend
    /\ UNCHANGED <<observed, sleepWhy, screen, autoSleep, dashboard, draw,
                   cpu, alarm, wokeBy, due, napper, holdTimer, napVoid, sleepVoid, sleepFor, pressed, world, ghosts>>

(* Woken by its alarm: at 1% on battery (`power::critical`, by            *)
(* `connected`) and with no charger within CHARGER_DETECT (`charger_comes`) *)
(* it powers off; otherwise it sleeps again.                               *)
SleeperChecks ==
    /\ ~OpenWorld
    /\ cpu = "up" /\ sleeper = "check"
    /\ ChargerSettled
    /\ IF level = "critical" /\ ~connected
       THEN cpu' = "off" /\ sleeper' = "idle"
       ELSE cpu' = cpu /\ sleeper' = "park"
    /\ UNCHANGED <<observed, app, draw, alarm, wokeBy, due, napper, holdTimer, napVoid, sleepVoid, sleepFor, pressed,
                   world, ghosts>>

(* A hold's timer (nap_or_hold, every HOLD_CHECK), or a short nap's       *)
(* (`after_nap`): if its nap is the one under way and the app is Napping,  *)
(* the minute's wake comes (Resumed) or, the hold over, it naps (with a    *)
(* ticket of its own); otherwise it stops.                                 *)
HoldTimerFires ==
    /\ ~OpenWorld
    /\ cpu = "up" /\ holdTimer # "none"
    /\ holdTimer' = "none"
    /\ IF mode = "DozeNapping"
       THEN \/ pend' = Add(pend, "Resumed", 1) /\ napper' = napper /\ napVoid' = napVoid
            \/ /\ holdTimer = "hold"
               /\ pend' = pend /\ napper = "idle" /\ napper' = "park" /\ napVoid' = FALSE
       ELSE pend' = pend /\ napper' = napper /\ napVoid' = napVoid
    /\ UNCHANGED <<observed, sleepWhy, screen, autoSleep, dashboard, draw,
                   cpu, alarm, wokeBy, due, sleeper, sleepFor, pressed, sleepVoid, world, ghosts>>

(* The dashboard worker renders the wake's tagged tick and hands the frame *)
(* to the window (`dashboard_shown`); or it hangs, for good.               *)
WorkerRenders ==
    /\ ~OpenWorld
    /\ cpu = "up" /\ tick = "queued" /\ ~stuck
    /\ tick' = "none" /\ shown' = TRUE
    /\ UNCHANGED <<observed, app, wakeOn, accepted, waiter, ceiling, stuck, syncing,
                   host, world, ghosts>>

WorkerHangs ==
    /\ ~OpenWorld
    /\ dashboard /\ ~stuck
    /\ stuck' = TRUE
    /\ UNCHANGED <<observed, app, wakeOn, tick, shown, accepted, waiter, ceiling, syncing,
                   host, world, ghosts>>

(* The `online` worker ends: Wi-Fi back, then the clock from network time, *)
(* which may step it back (a nap set out on the old clock then sleeps that *)
(* much longer), then the fetch's tick: Fetch, or Draw without Wi-Fi. Or   *)
(* it never ends (the join or the fetch hangs).                            *)
SyncDone ==
    /\ ~OpenWorld
    /\ cpu = "up" /\ syncing
    /\ syncing' = FALSE
    /\ tick' = IF tick = "fetching" THEN "queued" ELSE tick
    /\ \E stepBack \in BOOLEAN : longNap' = (longNap \/ (stepBack /\ napper = "park"))
    /\ UNCHANGED <<observed, app, wakeOn, shown, accepted, waiter, ceiling, stuck,
                   host, world, pressWaiting, overlap, badClaim, misjudged>>

(* The window's frame reaches the glass: the panel's driver accepts an    *)
(* update; or it never does (no pixel changed, or it refused).            *)
PanelAccepts ==
    /\ ~OpenWorld
    /\ cpu = "up" /\ shown /\ ~accepted
    /\ accepted' = TRUE
    /\ UNCHANGED <<observed, app, wakeOn, tick, shown, waiter, ceiling, stuck, syncing,
                   host, world, ghosts>>

(* `DrawWake::claim`: the first claim for the wake under way ends it and  *)
(* delivers PanelSettled; any other does nothing.                         *)
Claim(o) ==
    IF wakeOn
    THEN /\ wakeOn' = FALSE
         /\ pend' = Add(pend, "PanelSettled", 1)
         /\ badClaim' = (badClaim \/ (o \in {"Done", "Timed"} /\ ~accepted))
    ELSE UNCHANGED <<wakeOn, pend, badClaim>>

(* `wait_drawn`: the frame for this wake handed over, then a panel update  *)
(* after it, then `wait_idle`: Done; or its error, and PANEL_UPDATE later  *)
(* Timed. At the deadline it gives up, saying nothing.                     *)
WaiterSees ==
    /\ ~OpenWorld
    /\ cpu = "up"
    /\ \/ waiter = "frame" /\ shown /\ waiter' = "panel"
       \/ waiter = "panel" /\ accepted /\ waiter' = "idle"
    /\ UNCHANGED <<observed, app, wakeOn, tick, shown, accepted, ceiling, stuck, syncing,
                   host, world, ghosts>>

WaiterEnds ==
    /\ ~OpenWorld
    /\ cpu = "up"
    /\ \/ waiter = "idle" /\ waiter' = "none" /\ Claim("Done")
       \/ waiter = "idle" /\ waiter' = "sent" /\ UNCHANGED <<wakeOn, pend, badClaim>>
       \/ waiter = "sent" /\ waiter' = "none" /\ Claim("Timed")
    /\ UNCHANGED <<observed, sleepWhy, screen, autoSleep, dashboard, tick, shown, accepted,
                   ceiling, stuck, syncing, host, world, pressWaiting, overlap, longNap,
                   misjudged>>

WaiterGivesUp ==
    /\ ~OpenWorld
    /\ cpu = "up" /\ waiter \in {"frame", "panel"}
    /\ waiter' = "none"
    /\ UNCHANGED <<observed, app, wakeOn, tick, shown, accepted, ceiling, stuck, syncing,
                   host, world, ghosts>>

(* The window's ceiling (DRAW_CEILING, ONLINE_CEILING): if it wins the    *)
(* claim, "doze redraw not seen on the panel" is logged (NotSeen).         *)
CeilingFires ==
    /\ ~OpenWorld
    /\ cpu = "up" /\ ceiling
    /\ ceiling' = FALSE
    /\ Claim("NotSeen")
    /\ UNCHANGED <<observed, sleepWhy, screen, autoSleep, dashboard, tick, shown, accepted,
                   waiter, stuck, syncing, host, world, pressWaiting, overlap, longNap,
                   misjudged>>

-----------------------------------------------------------------------------
Events ==
    \/ PowerButton \/ Idle \/ Doze \/ BatteryEmpty \/ PanelSettled \/ Resumed
    \/ SuspendFailed \/ DriveComputer \/ DriveKindle
    \/ \E saved \in BOOLEAN : LightOff(saved)
    \/ \E current \in BOOLEAN : DriveSettled(current)
    \/ StorageLeaving
    \/ \E back \in BOOLEAN : DriveNotice(back)

World ==
    \/ PressButton \/ \E c \in Cables \ {"none"} : Plug(c) \/ Unplug \/ ConnectedAppears
    \/ Gauge \/ AlarmFires \/ RefreshStatus
    \/ NapperSuspends \/ NapperRefused \/ NapperResumes
    \/ SleeperSuspends \/ SleeperRefused \/ SleeperResumes \/ SleeperChecks \/ SleeperDetects
    \/ HoldTimerFires \/ WorkerRenders \/ WorkerHangs \/ SyncDone \/ PanelAccepts
    \/ WaiterSees \/ WaiterEnds \/ WaiterGivesUp \/ CeilingFires

Next == Events \/ FreshExpires \/ World

(* Fair: the app handles what is queued; its threads and timers run while *)
(* the CPU is up; the alarm fires; the charger driver makes `connected`;   *)
(* time passes. Not fair: the user, the cable, the gauge, a refused        *)
(* suspend, a panel that never takes the frame, a fetch that never ends.   *)
Fairness ==
    /\ SF_vars(PowerButton) /\ WF_vars(BatteryEmpty) /\ WF_vars(PanelSettled)
    /\ WF_vars(Resumed) /\ WF_vars(SuspendFailed) /\ SF_vars(DriveComputer)
    /\ WF_vars(DriveKindle) /\ WF_vars(\E saved \in BOOLEAN : LightOff(saved))
    /\ WF_vars(\E current \in BOOLEAN : DriveSettled(current))
    /\ WF_vars(NapperSuspends) /\ WF_vars(NapperResumes)
    /\ WF_vars(Aborts /\ NapperRefused) /\ SF_vars(Aborts /\ SleeperRefused)
    \* A cable going in and out can't hold the sleep's thread back for ever.
    /\ SF_vars(SleeperSuspends) /\ WF_vars(SleeperResumes) /\ SF_vars(SleeperChecks)
    \* CHARGER_DETECT is time passing: it ends, even with a cable going in and out.
    /\ SF_vars(SleeperDetects)
    /\ WF_vars(AlarmFires) /\ WF_vars(ConnectedAppears)
    /\ WF_vars(HoldTimerFires) /\ WF_vars(WorkerRenders) /\ WF_vars(WaiterSees)
    /\ WF_vars(WaiterEnds) /\ WF_vars(CeilingFires) /\ WF_vars(FreshExpires)

Spec == Init /\ [][Next]_vars /\ Fairness

-----------------------------------------------------------------------------
(* Properties.                                                             *)

TypeOK ==
    /\ mode \in Modes
    /\ ignored \in Ignored
    /\ \A i \in 1..Len(effects) : effects[i] \in Effects
    /\ (sleepWhy # "none") = (mode \in AsleepModes)
    /\ sleepWhy \in {"none", "button", "idle", "battery"}
    /\ screen \in {"app", "sleep", "usb"}
    /\ \A k \in Kinds : pend[k] \in 0..MaxPending
    /\ tick \in {"none", "fetching", "queued"}
    /\ waiter \in {"none", "frame", "panel", "idle", "sent"}
    /\ cpu \in {"up", "suspended", "off"}
    /\ alarm \in {"none", "nap", "check"}
    /\ wokeBy \in {"none", "alarm", "press", "cable"}
    /\ napper \in {"idle", "park", "suspended"}
    /\ sleeper \in {"idle", "park", "suspended", "check", "detect"}
    /\ sleepFor \in {"asked", "battery"}
    /\ holdTimer \in {"none", "hold", "short"}
    /\ cable \in Cables /\ level \in Levels
    /\ connected => cable # "none"

(* No nap or sleep thread starts while the last, still good, is running.  *)
OneSuspendAtATime == ~overlap

(* The device suspends only while the app sleeps or naps, never behind an *)
(* app that is awake as far as it knows (the screen on, touch dead).       *)
SuspendedOnlyWhenAppSleeps ==
    cpu = "suspended" => mode \in AsleepModes \cup {"DozeNapping"}

(* Never suspended without a wake alarm, unless asleep on purpose: the     *)
(* button, auto-sleep, or the low battery.                                 *)
AlarmUnlessAsleep ==
    (cpu = "suspended" /\ alarm = "none") => mode \in AsleepModes

(* Asleep on battery, the 6-hourly check alarm is armed: without it the 1% *)
(* power-off never comes.                                                  *)
CheckedAsleepOnBattery ==
    (cpu = "suspended" /\ sleeper = "suspended" /\ cable = "none") => alarm = "check"

(* A nap lasts until its minute, not longer.                               *)
NapsEndOnTheMinute == ~longNap

(* A doze wake that ended Done or Timed had its frame accepted by the     *)
(* panel; any other ended NotSeen, logged, or with the doze.               *)
ClaimedFrameIsOnPanel == ~badClaim

(* The sleep takes the button for the button: never a press for its check *)
(* alarm, which would put the device straight back to sleep.               *)
PressNotTakenForAlarm == ~misjudged

(* Never powered off on a charger.                                         *)
NoPowerOffOnCharger == cpu = "off" => cable = "none"

(* Every doze wake ends.                                                   *)
WakeEnds == wakeOn ~> (~wakeOn \/ cpu = "off")

(* A dozing kindil keeps redrawing: each nap ends, and each redraw.        *)
DozeKeepsRedrawing ==
    /\ (mode = "DozeNapping") ~> (mode # "DozeNapping" \/ cpu = "off")
    /\ (mode = "DozeDrawing") ~> (mode # "DozeDrawing" \/ cpu = "off")

(* A press while the app sleeps or dozes wakes it.                         *)
PressWakes == pressWaiting ~> (~pressWaiting \/ cpu = "off")

(* Plugged in, kindil leaves the low-battery sleep...                      *)
PluggedLeavesLowBattery ==
    (cable # "none" /\ sleepWhy = "battery") ~> (cable = "none" \/ sleepWhy # "battery")

(* ... and plugged in for good, it is out of it for good: neither stuck in *)
(* it nor going back and forth between it and the dashboard.               *)
PluggedStaysOffLowBattery ==
    (<>[](cable # "none")) => <>[](sleepWhy # "battery")

(* The low-battery sleep on battery ends suspended or powered off, not     *)
(* with the CPU up behind the sleep screen.                                *)
LowBatterySleepSuspends ==
    (mode = "AsleepSuspended" /\ sleepWhy = "battery" /\ cpu = "up" /\ cable = "none")
        ~> (cpu # "up" \/ mode # "AsleepSuspended" \/ cable # "none")
=============================================================================
