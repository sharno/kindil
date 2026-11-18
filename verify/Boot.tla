--------------------------------- MODULE Boot ---------------------------------
(***************************************************************************)
(* `launcher boot` (launcher/src/boot.rs, overlay.rs) and its shell        *)
(* fallback (kindle/extensions/kindil/bin/boot.sh), boot after boot.       *)
(*                                                                         *)
(* Each boot: the runtime overlays are applied while their switch is on    *)
(* and fewer than overlay LIMIT boots with them went unproven; then, while *)
(* the BOOT flag exists, the guard starts kindil once, unless LIMIT boots  *)
(* in a row started it and it didn't stay up for SETTLE: then Amazon's UI. *)
(* A standalone flag also skips Amazon's UI (DONT_START_FRAMEWORK) from    *)
(* the boot after one where kindil stayed up; any failing boot brings it   *)
(* back. The overlays prove themselves after SETTLE (upstart still knows   *)
(* their jobs) or are undone. A boot can hang or reboot at any step.       *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
    LIMIT,       \* boot.rs LIMIT
    OVERLAY_LIMIT \* overlay.rs LIMIT

VARIABLES
    flag,        \* /mnt/us/kindil/BOOT: "off", "after" (after Amazon's UI), "standalone"
    skip,        \* /mnt/us/DONT_START_FRAMEWORK
    tries,       \* /var/local/kindil/boot-tries
    ovOn,        \* the OVERLAYS switch
    ovTries,     \* /var/local/kindil/overlay-tries
    step,        \* where this boot is: "overlays", "guard", "settle", "prove", "up"
    decided,     \* `count`'s `Start` for this boot: "nothing", "over", "try"
    bound,       \* this boot applied overlays
    ui,          \* Amazon's UI comes up in this boot
    started,     \* this boot started kindil
    kup,         \* and saw it stay up for SETTLE
    \* Ghosts: boots in a row ...
    failing,     \* ... that started kindil and didn't see it stay up
    dark,        \* ... with neither Amazon's UI nor kindil staying up
    unproven     \* ... that applied overlays and didn't prove them

vars == <<flag, skip, tries, ovOn, ovTries, step, decided, bound, ui, started, kup, failing, dark, unproven>>

Init ==
    /\ flag \in {"off", "after", "standalone"}
    /\ skip \in (IF flag = "standalone" THEN BOOLEAN ELSE {FALSE})
    /\ tries = 0 /\ ovTries = 0
    /\ ovOn \in BOOLEAN
    /\ step = "overlays" /\ decided = "nothing" /\ bound = FALSE /\ started = FALSE /\ kup = FALSE
    \* Amazon's framework job skips its UI while the flag file exists.
    /\ ui = ~skip
    /\ failing = 0 /\ dark = 0 /\ unproven = 0

Min(a, b) == IF a < b THEN a ELSE b

(* `count`, then `overlay::apply`: the boot is counted among kindil's tries *)
(* first (a boot the overlays hang is one kindil didn't stay up in); the     *)
(* overlays are counted before binding, and skipped from their limit on.   *)
Overlays ==
    /\ step = "overlays"
    /\ IF flag = "off" THEN decided' = "nothing" /\ tries' = tries
       ELSE IF tries >= LIMIT THEN decided' = "over" /\ tries' = tries
       ELSE decided' = "try" /\ tries' = tries + 1
    /\ IF ovOn /\ ovTries < OVERLAY_LIMIT
       THEN bound' = TRUE /\ ovTries' = ovTries + 1
       ELSE UNCHANGED <<bound, ovTries>>
    /\ step' = "guard"
    /\ UNCHANGED <<flag, skip, ovOn, ui, started, kup, failing, dark, unproven>>

(* `amazon_ui`: with the skip flag set, it goes and Amazon's UI starts. *)
AmazonUi == IF skip THEN skip' = FALSE /\ ui' = TRUE ELSE UNCHANGED <<skip, ui>>

(* `guard`, up to starting kindil; or bin/boot.sh after the launcher       *)
(* failed: the same as `amazon_ui`.                                        *)
Guard ==
    /\ step = "guard"
    /\ \/ decided = "nothing" /\ step' = "prove" /\ UNCHANGED <<skip, ui, started>>
       \/ decided = "over" /\ AmazonUi /\ step' = "prove" /\ UNCHANGED started
       \/ decided = "try" /\ started' = TRUE /\ step' = "settle" /\ UNCHANGED <<skip, ui>>
    /\ UNCHANGED <<decided, tries, flag, ovOn, ovTries, bound, kup, failing, dark, unproven>>

LauncherFails ==
    /\ step = "guard" /\ flag # "off"
    /\ AmazonUi /\ step' = "prove"
    /\ UNCHANGED <<decided, flag, tries, ovOn, ovTries, bound, started, kup, failing, dark, unproven>>

(* SETTLE later: kindil's session is up (the boot-tries go, a standalone  *)
(* flag skips Amazon's UI from the next boot) or not (Amazon's UI).        *)
Settle ==
    /\ step = "settle"
    /\ \/ tries' = 0 /\ skip' = (skip \/ flag = "standalone") /\ kup' = TRUE /\ UNCHANGED ui
          /\ failing' = 0
       \/ AmazonUi /\ UNCHANGED <<tries, kup>>
          /\ failing' = failing + 1
    /\ step' = "prove"
    /\ UNCHANGED <<decided, flag, ovOn, ovTries, bound, started, dark, unproven>>

(* `overlay::prove`: upstart still knows their jobs (the count goes) or   *)
(* not (undone; the count stays).                                          *)
Prove ==
    /\ step = "prove"
    /\ IF bound
       THEN \/ ovTries' = 0 /\ unproven' = 0
            \/ UNCHANGED ovTries /\ unproven' = unproven + 1
       ELSE UNCHANGED <<ovTries, unproven>>
    /\ step' = "up"
    /\ UNCHANGED <<decided, flag, skip, tries, ovOn, bound, ui, started, kup, failing, dark>>

(* The next boot: after this one finished, or hung, or rebooted at any    *)
(* step once kindil's job ran (a boot that hangs before it isn't kindil's  *)
(* doing). A boot cut short with kindil started counts as failing; one     *)
(* that applied overlays and was cut before proving them, as unproven.     *)
Reboot ==
    /\ step # "overlays"
    \* Between the overlays and the guard, only an overlay can hang it.
    /\ step = "guard" => bound
    /\ step' = "overlays" /\ decided' = "nothing" /\ bound' = FALSE /\ started' = FALSE /\ kup' = FALSE
    /\ ui' = ~skip
    /\ failing' = IF step = "settle" THEN failing + 1 ELSE failing
    /\ dark' = IF ui \/ kup THEN 0 ELSE dark + 1
    /\ unproven' = IF bound /\ step # "up" THEN unproven + 1 ELSE unproven
    /\ UNCHANGED <<flag, skip, tries, ovOn, ovTries>>

(* Settings or KUAL: start at boot (set_boot_start), the overlays' switch; *)
(* once the boot is through.                                               *)
User ==
    /\ step = "up"
    /\ \/ \E f \in {"off", "after", "standalone"} :
            /\ flag' = f /\ tries' = 0
            /\ skip' = (f = "standalone")
            /\ failing' = 0 /\ dark' = 0
            /\ UNCHANGED ovOn
       \/ ovOn' = ~ovOn /\ UNCHANGED <<flag, tries, skip, failing, dark>>
    /\ UNCHANGED <<decided, ovTries, step, bound, ui, started, kup, unproven>>

Next == Overlays \/ Guard \/ LauncherFails \/ Settle \/ Prove \/ Reboot \/ User

(* Bound the counters the user resets: a run of boots is what matters. *)
Spec == Init /\ [][Next]_vars

Small == failing <= LIMIT + 2 /\ dark <= LIMIT + 2 /\ unproven <= OVERLAY_LIMIT + 2

-----------------------------------------------------------------------------
TypeOK ==
    /\ flag \in {"off", "after", "standalone"}
    /\ tries \in 0..LIMIT
    /\ decided \in {"nothing", "over", "try"}
    /\ ovTries \in 0..OVERLAY_LIMIT
    /\ skip => flag = "standalone"

(* At most LIMIT boots in a row start kindil without it staying up. *)
BootLoopBounded == failing <= LIMIT

(* Never more than LIMIT boots in a row with neither Amazon's UI nor      *)
(* kindil staying up: a standalone boot always has a way back.             *)
NeverDarkForLong == dark <= LIMIT

(* What does hold: a boot hung by the overlays isn't one of the guard's.   *)
DarkBounded == dark <= LIMIT + OVERLAY_LIMIT

(* After OVERLAY_LIMIT unproven boots with overlays, the next is stock.   *)
OverlaysBounded == unproven <= OVERLAY_LIMIT
=============================================================================
