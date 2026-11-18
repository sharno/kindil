//! The OS duties Amazon's UI performs and kindil takes over while it runs:
//! sleeping on the power button or when idle, dozing between screen updates,
//! and stepping aside while a computer has the user storage over USB.
//!
//! Events arrive from the kernel (power button), volumd or kindil's own USB
//! drive mode (src/drive.rs), the
//! minute clock (idle) and kindil's own workers and timers, so this is a
//! reducer: a pure `step` from (mode, event) to the next mode and the effects
//! to run. The UI runs the effects.
use crate::battery::{Battery, Charge};
use crate::host::Brightness;
use crate::maintenance::Hold;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Waking from suspend takes a power press, and the kernel reports that same
/// press once the device is back. It must not put the device straight back
/// to sleep.
pub const WAKING_PRESS: Duration = Duration::from_secs(3);
/// How long the panel needs to finish drawing the sleep screen. Suspending
/// mid-update leaves a half-drawn image for as long as the device sleeps.
pub const PANEL_SETTLE: Duration = Duration::from_millis(1500);
/// A flaky cable drops and restores USB drive mode every second; the app
/// returns only once the storage has stayed back this long.
pub const DRIVE_SETTLE: Duration = Duration::from_secs(3);
/// Without a touch for this long, a dozing kindil starts napping.
pub const DOZE_AFTER: Duration = Duration::from_secs(120);
/// How long a PW1 panel update takes once the frame is sent (about 1.25 s
/// from a doze wake to the frame sent, measured on 0.7.6). When the driver
/// can't say the update finished, a doze wake naps this long after sending:
/// suspending mid-update can leave the update, and the panel's power, on.
pub const PANEL_UPDATE: Duration = Duration::from_millis(1000);
/// The most a drawing wake stays up when its redraw never reaches the panel
/// (rendering fails, or the dashboard worker is stuck). Normally the wake
/// ends as soon as the panel has the frame: about 1.3 s, plus
/// [`PANEL_UPDATE`] when the driver can't tell.
pub const DRAW_CEILING: Duration = Duration::from_secs(6);
/// While dozing, how often kindil goes online to fetch the weather (radar
/// only comes with it, when rain is expected). Wi-Fi is parked otherwise.
pub const ONLINE_EVERY: chrono::TimeDelta = chrono::TimeDelta::minutes(30);
/// The most an online wake stays up: Wi-Fi joins in up to 15 s after a nap,
/// and a fetch gives up after 20 s. It ends as soon as the fetched frame is
/// on the panel.
pub const ONLINE_CEILING: Duration = Duration::from_secs(60);
/// Shorter naps aren't worth a suspend: kindil waits for the minute awake.
pub const MIN_NAP: Duration = Duration::from_secs(5);
/// A dozing dashboard wakes every minute, but redraws all of itself only
/// once in each block of this many minutes of the wall clock (and when the
/// day or the next prayer changes, or the weather was fetched): the other
/// minutes are [`Redraw::Tick`]s, which redraw only the clock and countdown.
/// The weather on screen changes on quarter hours, which are block starts.
pub const FULL_REDRAW_MINUTES: u32 = 5;
/// How often dozing logs where its energy went ([`DozeLedger`]).
pub const LEDGER_EVERY: chrono::TimeDelta = chrono::TimeDelta::hours(1);
/// At or below this level on battery, kindil sleeps with a charge screen:
/// suspended, the last few percent last for days instead of an hour.
pub const BATTERY_EMPTY: u8 = 5;

/// [`Event::BatteryEmpty`] when the battery is that low and not charging.
pub fn battery_event(battery: Battery) -> Option<Event> {
    match battery {
        Battery::Known {
            percent,
            charge: Charge::Discharging,
        } if percent <= BATTERY_EMPTY => Some(Event::BatteryEmpty),
        Battery::Known { .. } | Battery::Unknown => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    PowerButton,
    /// Nobody touched the screen for the auto-sleep time.
    Idle,
    /// Nobody touched the screen for [`DOZE_AFTER`], and kindil dozes.
    Doze,
    /// See [`battery_event`].
    BatteryEmpty,
    /// The front light is off; this was its level, if it could be read.
    LightOff(Option<Brightness>),
    /// The sleep screen, or a dozing redraw, has had time to reach the panel.
    PanelSettled,
    /// The device suspended, or napped, and woke up again.
    Resumed,
    /// The kernel refused to suspend; the device stayed up with the screen asleep.
    SuspendFailed,
    /// volumd, or kindil's own USB drive mode, will hand the user storage to
    /// a computer in a moment.
    StorageLeaving,
    Drive(Drive),
    /// Something to say about USB drive mode run by kindil.
    DriveNotice(crate::drive::Notice),
    /// The storage has been back since this instant, for [`DRIVE_SETTLE`].
    DriveSettled(Instant),
}

/// Who has the user storage (`/mnt/us`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drive {
    /// A computer: USB drive mode.
    Computer,
    Kindle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Awake {
        since: Instant,
    },
    Asleep(Sleep),
    /// The screen stays as it is and keeps updating; between updates the
    /// device naps.
    Dozing(Doze),
    /// A computer has the user storage; kindil shows the USB screen.
    UsbDrive,
    /// The storage is back; waiting to see that it stays.
    Returning {
        since: Instant,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sleep {
    /// The sleep screen is up; the light is being switched off.
    DimmingLight,
    /// Waiting for the panel before suspending.
    Settling { light: Option<Brightness> },
    /// Suspended, or about to be.
    Suspended { light: Option<Brightness> },
}

/// Doze: suspend to RAM between minute updates. E-ink keeps the picture
/// without power, so a wall dashboard keeps showing and updating on a fraction
/// of the battery. Touch can't wake a suspended PW1; the power button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Doze {
    /// The light is being switched off.
    DimmingLight,
    /// Waiting for the panel before the first nap.
    Settling { light: Option<Brightness> },
    /// Suspended until the next minute, or about to be.
    Napping { light: Option<Brightness> },
    /// Back from a nap: redrawing, and online now and then for the weather.
    Drawing { light: Option<Brightness> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    ShowSleep,
    LightOff,
    /// Deliver [`Event::PanelSettled`] after [`PANEL_SETTLE`].
    AwaitPanel,
    Suspend,
    /// Suspend until the next minute, then deliver [`Event::Resumed`].
    Nap,
    /// Update the screen after a nap, then deliver [`Event::PanelSettled`].
    Redraw,
    /// Back to the app.
    Wake,
    /// Back from dozing: the app never left the screen.
    Rouse,
    /// Back from a suspend: bring Wi-Fi back, see [`crate::host::Host::reconnect_wifi`].
    Reconnect,
    RestoreLight(Brightness),
    ShowUsbDrive,
    /// Close everything that holds files on the user storage.
    ReleaseStorage,
    /// Deliver [`Event::DriveSettled`] with this instant after [`DRIVE_SETTLE`].
    AwaitDrive(Instant),
    /// The user storage is back, possibly changed by the computer.
    StorageBack,
    /// Show this.
    DriveNotice(crate::drive::Notice),
}

/// Why an event changed nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ignored {
    /// The press that woke the device, reported after resume.
    WakingPress,
    /// powerd ignores the button in drive mode too: the computer has the storage.
    InUsbDrive,
    /// A worker or timer finished after the mode moved on, or volumd repeated itself.
    Stale,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub next: Mode,
    pub effects: Vec<Effect>,
}

fn to(next: Mode, effects: impl IntoIterator<Item = Effect>) -> Result<Step, Ignored> {
    Ok(Step {
        next,
        effects: effects.into_iter().collect(),
    })
}

fn wake(light: Option<Brightness>) -> impl Iterator<Item = Effect> {
    std::iter::once(Effect::Wake).chain(light.map(Effect::RestoreLight))
}

const FALL_ASLEEP: [Effect; 2] = [Effect::ShowSleep, Effect::LightOff];

impl Mode {
    pub fn start(now: Instant) -> Self {
        // Starting is not a wake: no press to swallow.
        Mode::Awake {
            since: now.checked_sub(WAKING_PRESS).unwrap_or(now),
        }
    }

    pub fn step(self, event: Event, now: Instant) -> Result<Step, Ignored> {
        use Event as E;
        use Mode as M;
        let awake = M::Awake { since: now };
        match (self, event) {
            (M::Awake { since }, E::PowerButton) if now.duration_since(since) < WAKING_PRESS => {
                Err(Ignored::WakingPress)
            }
            (M::Awake { .. }, E::PowerButton | E::Idle | E::BatteryEmpty) => {
                to(M::Asleep(Sleep::DimmingLight), FALL_ASLEEP)
            }
            (M::Awake { .. }, E::Doze) => to(M::Dozing(Doze::DimmingLight), [Effect::LightOff]),
            (M::Dozing(Doze::DimmingLight), E::LightOff(light)) => {
                to(M::Dozing(Doze::Settling { light }), [Effect::AwaitPanel])
            }
            (M::Dozing(Doze::Settling { light } | Doze::Drawing { light }), E::PanelSettled) => {
                to(M::Dozing(Doze::Napping { light }), [Effect::Nap])
            }
            // A refused nap retries after the next redraw window.
            (M::Dozing(Doze::Napping { light }), E::Resumed | E::SuspendFailed) => {
                to(M::Dozing(Doze::Drawing { light }), [Effect::Redraw])
            }
            // Whether it lands before or after the nap returns, the press wakes.
            (M::Dozing(doze), E::PowerButton) => to(
                awake,
                std::iter::once(Effect::Rouse)
                    .chain(doze.light().map(Effect::RestoreLight))
                    .chain(doze.napped().then_some(Effect::Reconnect)),
            ),
            // The light is already off, or on its way: LightOff carries its level.
            (M::Dozing(Doze::DimmingLight), E::BatteryEmpty) => {
                to(M::Asleep(Sleep::DimmingLight), [Effect::ShowSleep])
            }
            (M::Dozing(Doze::Settling { light } | Doze::Drawing { light }), E::BatteryEmpty) => to(
                M::Asleep(Sleep::Settling { light }),
                [Effect::ShowSleep, Effect::AwaitPanel],
            ),
            (M::Dozing(doze), E::Drive(Drive::Computer)) => to(
                M::UsbDrive,
                wake(doze.light()).chain([Effect::ReleaseStorage, Effect::ShowUsbDrive]),
            ),
            (M::Asleep(Sleep::DimmingLight), E::LightOff(light)) => {
                to(M::Asleep(Sleep::Settling { light }), [Effect::AwaitPanel])
            }
            // Woken while the light was going off: put it back.
            (M::Awake { .. }, E::LightOff(light)) => to(self, light.map(Effect::RestoreLight)),
            (M::Asleep(Sleep::Settling { light }), E::PanelSettled) => {
                to(M::Asleep(Sleep::Suspended { light }), [Effect::Suspend])
            }
            (M::Asleep(sleep), E::PowerButton | E::Resumed) => to(
                awake,
                wake(sleep.light()).chain(sleep.suspended().then_some(Effect::Reconnect)),
            ),
            // The sleep screen stays and the light stays off; the next press wakes.
            (M::Asleep(Sleep::Suspended { .. }), E::SuspendFailed) => to(self, []),
            (_, E::StorageLeaving) => to(self, [Effect::ReleaseStorage]),
            (_, E::DriveNotice(notice)) => to(
                self,
                notice
                    .storage_back()
                    .then_some(Effect::StorageBack)
                    .into_iter()
                    .chain([Effect::DriveNotice(notice)]),
            ),
            (M::Awake { .. }, E::Drive(Drive::Computer)) => {
                to(M::UsbDrive, [Effect::ReleaseStorage, Effect::ShowUsbDrive])
            }
            (M::Asleep(sleep), E::Drive(Drive::Computer)) => to(
                M::UsbDrive,
                wake(sleep.light()).chain([Effect::ReleaseStorage, Effect::ShowUsbDrive]),
            ),
            // Back to drive mode before the storage settled: the screen never left.
            (M::Returning { .. }, E::Drive(Drive::Computer)) => to(M::UsbDrive, []),
            (M::UsbDrive, E::Drive(Drive::Kindle)) => {
                to(M::Returning { since: now }, [Effect::AwaitDrive(now)])
            }
            (M::Returning { since }, E::DriveSettled(at)) if at == since => {
                to(awake, [Effect::Wake, Effect::StorageBack])
            }
            (M::UsbDrive | M::Returning { .. }, E::PowerButton) => Err(Ignored::InUsbDrive),
            (M::Awake { .. }, E::PanelSettled | E::Resumed | E::SuspendFailed)
            | (
                M::Awake { .. } | M::Asleep(_) | M::Dozing(_),
                E::Drive(Drive::Kindle) | E::DriveSettled(_),
            )
            | (M::Asleep(_) | M::Dozing(_), E::Idle | E::Doze)
            | (M::Asleep(_), E::BatteryEmpty)
            // The nap's own wake would follow and wake the app; the next
            // redraw reads the battery again.
            | (M::Dozing(Doze::Napping { .. }), E::BatteryEmpty)
            | (
                M::Dozing(Doze::Settling { .. } | Doze::Napping { .. } | Doze::Drawing { .. }),
                E::LightOff(_),
            )
            | (M::Dozing(Doze::DimmingLight | Doze::Napping { .. }), E::PanelSettled)
            | (
                M::Dozing(Doze::DimmingLight | Doze::Settling { .. } | Doze::Drawing { .. }),
                E::Resumed | E::SuspendFailed,
            )
            | (M::Asleep(Sleep::Settling { .. } | Sleep::Suspended { .. }), E::LightOff(_))
            | (
                M::Asleep(Sleep::DimmingLight | Sleep::Settling { .. }),
                E::PanelSettled | E::SuspendFailed,
            )
            | (M::Asleep(Sleep::Suspended { .. }), E::PanelSettled)
            | (M::UsbDrive, E::Drive(Drive::Computer) | E::DriveSettled(_))
            | (M::Returning { .. }, E::Drive(Drive::Kindle) | E::DriveSettled(_))
            | (
                M::UsbDrive | M::Returning { .. },
                E::Idle
                | E::Doze
                | E::BatteryEmpty
                | E::LightOff(_)
                | E::PanelSettled
                | E::Resumed
                | E::SuspendFailed,
            ) => Err(Ignored::Stale),
        }
    }
}

impl Sleep {
    fn light(self) -> Option<Brightness> {
        match self {
            Sleep::DimmingLight => None,
            Sleep::Settling { light } | Sleep::Suspended { light } => light,
        }
    }

    fn suspended(self) -> bool {
        match self {
            Sleep::DimmingLight | Sleep::Settling { .. } => false,
            Sleep::Suspended { .. } => true,
        }
    }
}

impl Doze {
    fn light(self) -> Option<Brightness> {
        match self {
            Doze::DimmingLight => None,
            Doze::Settling { light } | Doze::Napping { light } | Doze::Drawing { light } => light,
        }
    }

    /// The first nap comes after settling; every later state follows one.
    fn napped(self) -> bool {
        match self {
            Doze::DimmingLight | Doze::Settling { .. } => false,
            Doze::Napping { .. } | Doze::Drawing { .. } => true,
        }
    }
}

/// What a dozing wake does besides drawing the minute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Window {
    /// Draw, then nap as soon as the panel has the frame.
    Draw,
    /// Also bring Wi-Fi back and fetch, then nap once the fetched frame is
    /// on the panel.
    Online,
}

impl Window {
    /// Online at the first wake and then every [`ONLINE_EVERY`] of wall time;
    /// the monotonic clock stops while the device naps. Only a dashboard
    /// fetches: without one there is nothing to go online for. A last time
    /// after `now` (a clock set back without it) is no reason to wait.
    pub fn after_nap(
        last_online: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
        dashboard: bool,
    ) -> Self {
        match last_online {
            _ if !dashboard => Window::Draw,
            Some(last) if last <= now && now - last < ONLINE_EVERY => Window::Draw,
            _ => Window::Online,
        }
    }

    /// The most the wake stays up when its frame never reaches the panel.
    pub fn ceiling(self) -> Duration {
        match self {
            Window::Draw => DRAW_CEILING,
            Window::Online => ONLINE_CEILING,
        }
    }
}

/// When a dozing kindil wakes next, after `now`: on the next whole minute,
/// when the clock on screen turns.
pub fn next_redraw(now: DateTime<Utc>) -> DateTime<Utc> {
    use chrono::{DurationRound, TimeDelta};
    now.duration_trunc(TimeDelta::minutes(1)).unwrap_or(now) + TimeDelta::minutes(1)
}

/// What a dozing wake redraws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Redraw {
    /// Everything, and the battery is read.
    Full,
    /// The clock and countdown only, with nothing else read; the dashboard
    /// still draws in full if the day or the next prayer has changed.
    Tick,
}

impl Redraw {
    /// Full on the first wake and then once in each block of
    /// [`FULL_REDRAW_MINUTES`] of the wall clock: on the block's first
    /// minute, or the first wake after it if that one was missed. `last_full`
    /// is when the last full one was.
    pub fn after_nap(last_full: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Self {
        use chrono::{DurationRound, TimeDelta};
        let every = TimeDelta::minutes(i64::from(FULL_REDRAW_MINUTES));
        let block = now.duration_trunc(every).unwrap_or(now);
        match last_full {
            // A clock set back draws in full rather than ticking until then.
            Some(last) if last >= block && last <= now => Redraw::Tick,
            _ => Redraw::Full,
        }
    }
}

/// What a dozing wake does: whether it is the half hour's (the battery's
/// draw is measured), whether it goes online, and what it redraws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DozeWake {
    pub half_hour: bool,
    pub window: Window,
    pub redraw: Redraw,
}

/// When dozing last went online (or would have, without a dashboard) and
/// redrew in full, on the wall clock.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Schedule {
    online: Option<DateTime<Utc>>,
    full: Option<DateTime<Utc>>,
}

impl Schedule {
    /// A doze's first wake draws in full.
    pub fn doze_began(&mut self) {
        self.full = None;
    }

    /// A wake at `now`, with or without a dashboard to fetch for.
    pub fn wake(&mut self, now: DateTime<Utc>, dashboard: bool) -> DozeWake {
        let half_hour = Window::after_nap(self.online, now, true) == Window::Online;
        let window = Window::after_nap(self.online, now, dashboard);
        if half_hour {
            self.online = Some(now);
        }
        let redraw = match window {
            Window::Online => Redraw::Full,
            Window::Draw => Redraw::after_nap(self.full, now),
        };
        if redraw == Redraw::Full {
            self.full = Some(now);
        }
        DozeWake {
            half_hour,
            window,
            redraw,
        }
    }

    /// Network time stepped the clock by `by`: the last fetch was on the old
    /// clock, so it moves too and the next comes half an hour after it. The
    /// last full redraw stays: a step into another block of minutes draws
    /// that block in full, one set back draws in full at once.
    pub fn clock_stepped(&mut self, by: chrono::TimeDelta) {
        self.online = self.online.map(|at| at + by);
    }
}

/// How a dozing wake's redraw ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settle {
    /// The panel said it finished the frame.
    Done,
    /// The frame was sent, the panel couldn't say when it finished, and the
    /// wake waited [`PANEL_UPDATE`].
    Timed,
    /// The frame never reached the panel before the window's ceiling.
    NotSeen,
}

/// Where a doze's time, and so its energy, went: logged every
/// [`LEDGER_EVERY`], so a battery measurement explains itself. Times are
/// wall-clock: the monotonic clock stops while the device naps, which the
/// monotonic sleep total shows. It notes the clock's corrections too: an
/// RTC catch-up gives back time a resume lost, so it counts as time that
/// passed; a step from network time fixes error that built up before, so
/// the times noted so far move with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DozeLedger {
    since: DateTime<Utc>,
    gauge: Option<crate::drain::MilliampHours>,
    wakes: u32,
    awake: chrono::TimeDelta,
    online: u32,
    /// The wakes that redrew only the clock and countdown, and their time
    /// up; the rest drew in full.
    ticks: u32,
    tick_awake: chrono::TimeDelta,
    /// What the wake under way redraws.
    redraw: Option<Redraw>,
    slept: chrono::TimeDelta,
    slept_monotonic: Duration,
    wifi_up: chrono::TimeDelta,
    settles: [u32; 3],
    /// The gauge's current at each wake, in mA: (sum, count).
    current: (i64, u32),
    /// Maintenance windows opened, and the time held awake instead of
    /// napping (not counted as wakes' time): in all, and for SSH sessions.
    windows: u32,
    held: chrono::TimeDelta,
    ssh_held: chrono::TimeDelta,
    awake_from: Option<DateTime<Utc>>,
    napping_from: Option<(DateTime<Utc>, Instant)>,
    wifi_from: Option<DateTime<Utc>>,
    held_from: Option<(DateTime<Utc>, Hold)>,
    /// Clock corrections: how many, and their sum, from the RTC and from
    /// the network.
    clock_rtc: (u32, chrono::TimeDelta),
    clock_net: (u32, chrono::TimeDelta),
    /// kindil's own DHCP client's counts when the ledger began, and last
    /// heard ([`DozeLedger::dhcp`]).
    dhcp: Option<(crate::dhcpc::Counts, crate::dhcpc::Counts)>,
}

impl DozeLedger {
    pub fn new(now: DateTime<Utc>, gauge: Option<crate::drain::MilliampHours>) -> Self {
        Self {
            since: now,
            gauge,
            wakes: 0,
            awake: chrono::TimeDelta::zero(),
            online: 0,
            ticks: 0,
            tick_awake: chrono::TimeDelta::zero(),
            redraw: None,
            slept: chrono::TimeDelta::zero(),
            slept_monotonic: Duration::ZERO,
            wifi_up: chrono::TimeDelta::zero(),
            settles: [0; 3],
            current: (0, 0),
            windows: 0,
            held: chrono::TimeDelta::zero(),
            ssh_held: chrono::TimeDelta::zero(),
            awake_from: None,
            napping_from: None,
            wifi_from: None,
            held_from: None,
            clock_rtc: (0, chrono::TimeDelta::zero()),
            clock_net: (0, chrono::TimeDelta::zero()),
            dhcp: None,
        }
    }

    /// kindil's own DHCP client's counts now (src/dhcpc.rs): the first
    /// call is where the ledger's count starts.
    pub fn dhcp(&mut self, counts: crate::dhcpc::Counts) {
        let from = self.dhcp.map_or(counts, |(from, _)| from);
        self.dhcp = Some((from, counts));
    }

    /// The clock was stepped.
    pub fn clock_corrected(&mut self, correction: crate::clock::Correction) {
        let by = correction.delta();
        if !correction.online() {
            self.clock_rtc.0 += 1;
            self.clock_rtc.1 += by;
            return;
        }
        self.clock_net.0 += 1;
        self.clock_net.1 += by;
        self.since += by;
        for at in [&mut self.awake_from, &mut self.wifi_from]
            .into_iter()
            .flatten()
        {
            *at += by;
        }
        if let Some((at, _)) = &mut self.napping_from {
            *at += by;
        }
        if let Some((at, _)) = &mut self.held_from {
            *at += by;
        }
    }

    /// Whether [`LEDGER_EVERY`] has passed (or the clock went back past its
    /// start).
    pub fn due(&self, now: DateTime<Utc>) -> bool {
        now - self.since >= LEDGER_EVERY || now < self.since
    }

    /// The gauge when the doze began, read once it has been.
    pub fn set_gauge(&mut self, gauge: Option<crate::drain::MilliampHours>) {
        self.gauge = self.gauge.or(gauge);
    }

    /// Back from a nap; `window` and `redraw` say what the wake does.
    pub fn woke(&mut self, now: DateTime<Utc>, monotonic: Instant, window: Window, redraw: Redraw) {
        if let Some((wall, mono)) = self.napping_from.take() {
            self.slept += now - wall;
            self.slept_monotonic += monotonic.saturating_duration_since(mono);
        }
        self.end_hold(now);
        self.wakes += 1;
        if window == Window::Online {
            self.online += 1;
        }
        if redraw == Redraw::Tick {
            self.ticks += 1;
        }
        self.redraw = Some(redraw);
        self.awake_from = Some(now);
    }

    /// The tick under way drew in full after all: the day or the next prayer
    /// changed.
    pub fn drew_full(&mut self) {
        if self.redraw == Some(Redraw::Tick) {
            self.ticks = self.ticks.saturating_sub(1);
            self.redraw = Some(Redraw::Full);
        }
    }

    /// About to nap.
    pub fn napping(&mut self, now: DateTime<Utc>, monotonic: Instant) {
        self.wake_ended(now);
        self.end_hold(now);
        self.napping_from = Some((now, monotonic));
    }

    /// Staying awake instead of napping, from `now` and for this reason
    /// (until the next call, a wake or a nap).
    pub fn holding(&mut self, now: DateTime<Utc>, hold: Hold) {
        self.wake_ended(now);
        self.end_hold(now);
        self.held_from = Some((now, hold));
    }

    /// A maintenance window opens.
    pub fn window_opened(&mut self) {
        self.windows += 1;
    }

    fn wake_ended(&mut self, now: DateTime<Utc>) {
        if let Some(from) = self.awake_from.take() {
            self.awake += now - from;
            if self.redraw == Some(Redraw::Tick) {
                self.tick_awake += now - from;
            }
        }
        self.redraw = None;
    }

    fn end_hold(&mut self, now: DateTime<Utc>) {
        if let Some((from, hold)) = self.held_from.take() {
            self.held += now - from;
            if hold == Hold::Ssh {
                self.ssh_held += now - from;
            }
        }
    }

    pub fn wifi_on(&mut self, now: DateTime<Utc>) {
        self.wifi_from.get_or_insert(now);
    }

    pub fn wifi_off(&mut self, now: DateTime<Utc>) {
        if let Some(from) = self.wifi_from.take() {
            self.wifi_up += now - from;
        }
    }

    pub fn settled(&mut self, settle: Settle) {
        let slot = match settle {
            Settle::Done => 0,
            Settle::Timed => 1,
            Settle::NotSeen => 2,
        };
        self.settles[slot] += 1;
    }

    /// The gauge's current at a wake: mostly the average over the nap just
    /// ended, as the PW1's gauge averages over a few seconds.
    pub fn current_at_wake(&mut self, milliamps: i32) {
        self.current.0 += i64::from(milliamps);
        self.current.1 += 1;
    }

    /// The line to log at `now` with the gauge then; the next ledger starts
    /// from here.
    pub fn close(
        &mut self,
        now: DateTime<Utc>,
        gauge: Option<crate::drain::MilliampHours>,
    ) -> String {
        // What is under way counts up to now here, and from now in the next.
        if let Some(from) = self.awake_from {
            self.awake += now - from;
            if self.redraw == Some(Redraw::Tick) {
                self.tick_awake += now - from;
            }
        }
        if let Some(from) = self.wifi_from {
            self.wifi_up += now - from;
        }
        let held_from = self.held_from;
        self.end_hold(now);
        let over = now - self.since;
        let seconds = |d: chrono::TimeDelta| d.num_milliseconds() as f64 / 1000.0;
        let each = |time: chrono::TimeDelta, wakes: u32| match wakes {
            0 => 0.0,
            n => seconds(time) / f64::from(n),
        };
        let fulls = self.wakes.saturating_sub(self.ticks);
        let used = match (self.gauge, gauge) {
            (Some(from), Some(to)) => {
                let used = i64::from(from.0) - i64::from(to.0);
                let rate = match over.num_seconds() {
                    0 => 0,
                    s => used * 3600 / s,
                };
                format!("{used} mAh ({rate} mA), {} mAh left", to.0)
            }
            (_, Some(to)) => format!("{} mAh left", to.0),
            (_, None) => "no gauge".to_string(),
        };
        let current = match self.current {
            (_, 0) => "none".to_string(),
            (sum, n) => format!("{} mA", sum / i64::from(n)),
        };
        let dhcp = match self.dhcp {
            Some((from, to)) => {
                let exchanges = to.exchanges.saturating_sub(from.exchanges);
                let rate = match over.num_seconds() {
                    s if s > 0 => exchanges as f64 * 3600.0 / s as f64,
                    _ => 0.0,
                };
                format!(
                    ", DHCP {exchanges} exchanges ({rate:.1} an hour, {} messages)",
                    to.packets.saturating_sub(from.packets)
                )
            }
            None => String::new(),
        };
        let line = format!(
            "doze ledger over {} min: awake {:.0} s in {} wakes ({:.1} s each, {} online; \
             {} ticks {:.2} s each, {} full {:.2} s each), asleep {:.0} s (monotonic {} s), \
             Wi-Fi up {:.0} s, held awake {:.0} s ({} maintenance windows, {:.0} s for SSH), \
             panel done {} / timed {} / not seen {}, gauge {used}, current at wake {current}, \
             clock corrected {} times by the RTC ({:+.1} s) and {} by the network ({:+.1} s){dhcp}",
            over.num_minutes(),
            seconds(self.awake),
            self.wakes,
            each(self.awake, self.wakes),
            self.online,
            self.ticks,
            each(self.tick_awake, self.ticks),
            fulls,
            each(self.awake - self.tick_awake, fulls),
            seconds(self.slept),
            self.slept_monotonic.as_secs(),
            seconds(self.wifi_up),
            seconds(self.held),
            self.windows,
            seconds(self.ssh_held),
            self.settles[0],
            self.settles[1],
            self.settles[2],
            self.clock_rtc.0,
            seconds(self.clock_rtc.1),
            self.clock_net.0,
            seconds(self.clock_net.1),
        );
        let (awake_from, napping_from, wifi_from, redraw, dhcp) = (
            self.awake_from,
            self.napping_from,
            self.wifi_from,
            self.redraw,
            self.dhcp,
        );
        *self = Self::new(now, gauge.or(self.gauge));
        self.dhcp = dhcp.map(|(_, to)| (to, to));
        self.awake_from = awake_from.map(|_| now);
        self.redraw = redraw;
        self.napping_from = napping_from;
        self.wifi_from = wifi_from.map(|_| now);
        self.held_from = held_from.map(|(_, hold)| (now, hold));
        line
    }
}

/// A line from `lipc-wait-event -m com.lab126.volumd ...`.
pub fn volumd_event(line: &str) -> Option<Event> {
    let mut words = line.split_whitespace();
    match (words.next()?, words.next()) {
        ("userstoreIsLikelyToUnMount", _) => Some(Event::StorageLeaving),
        ("driveModeStateChanged", Some(state)) => drive_mode(state.parse().ok()?).map(Event::Drive),
        _ => None,
    }
}

/// volumd's `driveModeStateChanged` parameter: 2 and 3 are the way in and out.
pub fn drive_mode(state: u8) -> Option<Drive> {
    match state {
        1 => Some(Drive::Computer),
        0 => Some(Drive::Kindle),
        _ => None,
    }
}

/// What the device does by itself when nobody touches it. The power button
/// always sleeps and wakes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoSleep {
    /// A wall dashboard on little battery: keeps updating, napping between
    /// minutes.
    #[default]
    Doze,
    /// Always awake, updating; drains the battery in about a day.
    Never,
    /// The sleep screen after this many minutes.
    Minutes(u16),
}

impl AutoSleep {
    /// The choices Settings offers, in order.
    pub const CHOICES: [AutoSleep; 6] = [
        AutoSleep::Doze,
        AutoSleep::Never,
        AutoSleep::Minutes(5),
        AutoSleep::Minutes(15),
        AutoSleep::Minutes(30),
        AutoSleep::Minutes(60),
    ];

    pub fn label(self) -> String {
        match self {
            AutoSleep::Doze => "Doze".into(),
            AutoSleep::Never => "Never".into(),
            AutoSleep::Minutes(60) => "1 h".into(),
            AutoSleep::Minutes(m) => format!("{m} min"),
        }
    }
}

/// Touches the panel has seen; the backend counts them as they arrive.
#[derive(Clone, Default)]
pub struct Touches(Arc<AtomicU64>);

impl Touches {
    pub fn new(counter: Arc<AtomicU64>) -> Self {
        Self(counter)
    }
    pub fn count(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// How long since the last touch, from samples of the touch count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Idle {
    touches: u64,
    since: Instant,
}

impl Idle {
    pub fn new(touches: u64, now: Instant) -> Self {
        Self {
            touches,
            since: now,
        }
    }

    pub fn observe(self, touches: u64, now: Instant) -> Self {
        if touches == self.touches {
            self
        } else {
            Self::new(touches, now)
        }
    }

    /// The event the setting calls for by now, if any.
    pub fn due(self, setting: AutoSleep, now: Instant) -> Option<Event> {
        let idle = now.duration_since(self.since);
        match setting {
            AutoSleep::Never => None,
            AutoSleep::Doze => (idle >= DOZE_AFTER).then_some(Event::Doze),
            AutoSleep::Minutes(minutes) => {
                (idle >= Duration::from_secs(60 * u64::from(minutes))).then_some(Event::Idle)
            }
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    /// Runs events at offsets in milliseconds; returns the last mode and all effects.
    fn run_from(t0: Instant, events: &[(Event, u64)]) -> (Mode, Vec<Effect>) {
        events.iter().fold(
            (Mode::start(t0), Vec::new()),
            |(mode, mut all), &(event, at)| match mode.step(event, t0 + Duration::from_millis(at)) {
                Ok(step) => {
                    all.extend(step.effects);
                    (step.next, all)
                }
                Err(_) => (mode, all),
            },
        )
    }

    fn run(events: &[(Event, u64)]) -> (Mode, Vec<Effect>) {
        run_from(Instant::now(), events)
    }

    #[test]
    fn a_press_sleeps_and_the_next_wakes_with_the_light_back() {
        let light = Brightness::new(40);
        let (mode, effects) = run(&[
            (Event::PowerButton, 10_000),
            (Event::LightOff(Some(light)), 10_100),
            (Event::PanelSettled, 11_600),
            (Event::Resumed, 100_000),
            // The press that woke the device, reported after resume.
            (Event::PowerButton, 100_500),
        ]);
        assert!(matches!(mode, Mode::Awake { .. }));
        assert_eq!(
            effects,
            [
                Effect::ShowSleep,
                Effect::LightOff,
                Effect::AwaitPanel,
                Effect::Suspend,
                Effect::Wake,
                Effect::RestoreLight(light),
                Effect::Reconnect,
            ]
        );
    }

    #[test]
    fn a_second_press_before_the_panel_settles_cancels_the_suspend() {
        let light = Brightness::new(40);
        let (mode, effects) = run(&[
            (Event::PowerButton, 10_000),
            (Event::LightOff(Some(light)), 10_100),
            (Event::PowerButton, 10_800),
            (Event::PanelSettled, 11_600),
        ]);
        assert!(matches!(mode, Mode::Awake { .. }));
        assert!(!effects.contains(&Effect::Suspend));
        assert_eq!(effects.last(), Some(&Effect::RestoreLight(light)));
    }

    #[test]
    fn waking_while_the_light_goes_off_puts_it_back() {
        let light = Brightness::new(40);
        let (_, effects) = run(&[
            (Event::PowerButton, 10_000),
            (Event::PowerButton, 10_050),
            (Event::LightOff(Some(light)), 10_100),
        ]);
        assert_eq!(effects.last(), Some(&Effect::RestoreLight(light)));
    }

    #[test]
    fn idle_sleeps_like_a_press() {
        let (mode, _) = run(&[(Event::Idle, 10_000)]);
        assert_eq!(mode, Mode::Asleep(Sleep::DimmingLight));
    }

    #[test]
    fn dozing_naps_and_redraws_each_minute_until_a_press() {
        let light = Brightness::new(40);
        let (mode, effects) = run(&[
            (Event::Doze, 10_000),
            (Event::LightOff(Some(light)), 10_100),
            (Event::PanelSettled, 11_600),
            (Event::Resumed, 12_000),
            (Event::PanelSettled, 15_000),
            (Event::Resumed, 16_000),
            (Event::PanelSettled, 19_000),
            // Pressed while napping; the nap returns after the press arrives.
            (Event::PowerButton, 19_500),
            (Event::Resumed, 19_600),
        ]);
        assert!(matches!(mode, Mode::Awake { .. }));
        assert_eq!(
            effects,
            [
                Effect::LightOff,
                Effect::AwaitPanel,
                Effect::Nap,
                Effect::Redraw,
                Effect::Nap,
                Effect::Redraw,
                Effect::Nap,
                Effect::Rouse,
                Effect::RestoreLight(light),
                Effect::Reconnect,
            ]
        );
    }

    #[test]
    fn only_a_wake_after_a_suspend_reconnects_wifi() {
        let light = Brightness::new(40);
        // Pressed before the first nap: Wi-Fi never went down.
        let (_, effects) = run(&[
            (Event::Doze, 10_000),
            (Event::LightOff(Some(light)), 10_100),
            (Event::PowerButton, 10_500),
        ]);
        assert!(!effects.contains(&Effect::Reconnect));
        // Pressed again before the sleep's suspend.
        let (_, effects) = run(&[
            (Event::PowerButton, 10_000),
            (Event::LightOff(Some(light)), 10_100),
            (Event::PowerButton, 11_000),
        ]);
        assert!(!effects.contains(&Effect::Reconnect));
    }

    #[test]
    fn an_empty_battery_sleeps_from_awake_or_dozing() {
        let light = Brightness::new(40);
        let (mode, effects) = run(&[(Event::BatteryEmpty, 10_000)]);
        assert_eq!(mode, Mode::Asleep(Sleep::DimmingLight));
        assert_eq!(effects, FALL_ASLEEP);
        let (mode, effects) = run(&[
            (Event::Doze, 10_000),
            (Event::LightOff(Some(light)), 10_100),
            (Event::PanelSettled, 11_600),
            (Event::BatteryEmpty, 11_700),
            (Event::Resumed, 12_000),
            (Event::BatteryEmpty, 12_500),
            (Event::PanelSettled, 14_000),
        ]);
        assert_eq!(mode, Mode::Asleep(Sleep::Suspended { light: Some(light) }));
        assert_eq!(
            effects,
            [
                Effect::LightOff,
                Effect::AwaitPanel,
                Effect::Nap,
                Effect::Redraw,
                Effect::ShowSleep,
                Effect::AwaitPanel,
                Effect::Suspend,
            ]
        );
    }

    #[test]
    fn only_a_low_battery_on_its_own_power_is_empty() {
        let battery = |percent, charge| Battery::Known { percent, charge };
        assert_eq!(
            battery_event(battery(BATTERY_EMPTY, Charge::Discharging)),
            Some(Event::BatteryEmpty)
        );
        assert_eq!(
            battery_event(battery(BATTERY_EMPTY + 1, Charge::Discharging)),
            None
        );
        assert_eq!(battery_event(battery(1, Charge::Charging)), None);
        assert_eq!(battery_event(Battery::Unknown), None);
    }

    #[test]
    fn dozing_goes_online_every_half_hour_of_wall_time() {
        let t0 = DateTime::from_timestamp(1_794_398_400, 0).unwrap();
        let at = |minutes| t0 + chrono::TimeDelta::minutes(minutes);
        assert_eq!(Window::after_nap(None, t0, true), Window::Online);
        assert_eq!(Window::after_nap(Some(t0), at(29), true), Window::Draw);
        assert_eq!(Window::after_nap(Some(t0), at(30), true), Window::Online);
        // Nothing to fetch without a dashboard: Wi-Fi stays parked.
        assert_eq!(Window::after_nap(None, t0, false), Window::Draw);
        assert_eq!(Window::after_nap(Some(t0), at(30), false), Window::Draw);
        // Fetched "in the future" (a clock set back without the doze's
        // times): fetch rather than wait for the clock to catch up.
        assert_eq!(Window::after_nap(Some(at(60)), at(1), true), Window::Online);
        // A redraw that never shows can't keep the device up for long.
        assert!(DRAW_CEILING <= Duration::from_secs(6));
        assert!(PANEL_UPDATE < DRAW_CEILING);
    }

    fn wall(hour: u32, minute: u32, second: u32) -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(2026, 11, 16, hour, minute, second)
            .unwrap()
    }

    #[test]
    fn dozing_wakes_on_every_minute() {
        assert_eq!(next_redraw(wall(9, 0, 3)), wall(9, 1, 0));
        assert_eq!(next_redraw(wall(9, 4, 59)), wall(9, 5, 0));
        assert_eq!(next_redraw(wall(9, 5, 0)), wall(9, 6, 0));
    }

    #[test]
    fn minutes_tick_and_each_five_minute_block_redraws_in_full() {
        use Redraw::{Full, Tick};
        // The first wake of a doze draws in full.
        assert_eq!(Redraw::after_nap(None, wall(9, 1, 1)), Full);
        let full = Some(wall(9, 0, 1));
        assert_eq!(Redraw::after_nap(full, wall(9, 1, 1)), Tick);
        assert_eq!(Redraw::after_nap(full, wall(9, 4, 1)), Tick);
        assert_eq!(Redraw::after_nap(full, wall(9, 5, 1)), Full);
        // A wake that slipped past the block's first minute still draws it.
        assert_eq!(Redraw::after_nap(full, wall(9, 6, 1)), Full);
        assert_eq!(Redraw::after_nap(Some(wall(9, 6, 1)), wall(9, 7, 1)), Tick);
        // Within the hour before a prayer too: the tick keeps the countdown exact.
        assert_eq!(Redraw::after_nap(full, wall(9, 2, 1)), Tick);
        // A clock set back doesn't tick until it catches up.
        assert_eq!(Redraw::after_nap(Some(wall(10, 0, 1)), wall(9, 2, 1)), Full);
    }

    /// Dozes for 70 minutes of real time on a clock off by `error`
    /// seconds, which network time corrects at the first (online) wake, as
    /// the UI does: wakes come a second past each minute of the clock.
    /// Returns the real minutes of the online wakes after the step, and
    /// each block of minutes' full redraws.
    fn doze_through_a_step(error: i64) -> (Vec<i64>, Vec<u32>) {
        use chrono::{DurationRound, TimeDelta};
        let real0 = wall(9, 0, 1);
        let mut schedule = Schedule::default();
        schedule.doze_began();
        let first = schedule.wake(real0 + TimeDelta::seconds(error), true);
        assert_eq!((first.window, first.redraw), (Window::Online, Redraw::Full));
        // The sync: the clock was `error` ahead.
        schedule.clock_stepped(TimeDelta::seconds(-error));
        let mut online = Vec::new();
        let mut fulls = std::collections::BTreeMap::<DateTime<Utc>, u32>::new();
        for minute in 1..=70 {
            let now = real0 + TimeDelta::minutes(minute);
            let wake = schedule.wake(now, true);
            assert_eq!(wake.half_hour, wake.window == Window::Online);
            if wake.window == Window::Online {
                online.push(minute);
            }
            let block = now
                .duration_trunc(TimeDelta::minutes(i64::from(FULL_REDRAW_MINUTES)))
                .unwrap();
            *fulls.entry(block).or_default() += u32::from(wake.redraw == Redraw::Full);
        }
        (online, fulls.into_values().collect())
    }

    #[test]
    fn a_clock_step_neither_doubles_nor_skips_a_fetch_or_a_full_redraw() {
        // 724 s fast, set back; 724 s and 40 minutes slow, set forward; and
        // a clock from 1970 after a lost RTC.
        for error in [724, -724, -2_400, -1_794_398_400] {
            let (online, fulls) = doze_through_a_step(error);
            assert_eq!(online, [30, 60], "{error} s off");
            // Each block of five minutes on the right clock draws in full
            // once (the 70 minutes after 9:00:01 end in a block of one).
            assert_eq!(fulls, [1; 15], "{error} s off");
        }
    }

    #[test]
    fn the_ledger_accounts_wakes_naps_and_wifi() {
        use crate::drain::MilliampHours;
        let t0 = wall(1, 0, 0);
        let i0 = Instant::now();
        let secs = |s: i64| chrono::TimeDelta::seconds(s);
        let mut ledger = DozeLedger::new(t0, Some(MilliampHours(500)));
        ledger.napping(t0, i0);
        // One online wake with Wi-Fi up 20 s, then a drawing one.
        ledger.woke(
            t0 + secs(58),
            i0 + Duration::from_secs(1),
            Window::Online,
            Redraw::Full,
        );
        ledger.wifi_on(t0 + secs(58));
        ledger.current_at_wake(-4);
        ledger.wifi_off(t0 + secs(78));
        ledger.settled(Settle::Done);
        ledger.napping(t0 + secs(80), i0 + Duration::from_secs(23));
        ledger.woke(
            t0 + secs(178),
            i0 + Duration::from_secs(24),
            Window::Draw,
            Redraw::Full,
        );
        ledger.current_at_wake(-6);
        ledger.settled(Settle::Timed);
        ledger.napping(t0 + secs(181), i0 + Duration::from_secs(27));
        // Two ticks of half a second; the second drew in full after all.
        ledger.woke(
            t0 + secs(240),
            i0 + Duration::from_secs(28),
            Window::Draw,
            Redraw::Tick,
        );
        ledger.napping(
            t0 + secs(240) + chrono::TimeDelta::milliseconds(500),
            i0 + Duration::from_secs(29),
        );
        ledger.woke(
            t0 + secs(300),
            i0 + Duration::from_secs(30),
            Window::Draw,
            Redraw::Tick,
        );
        ledger.drew_full();
        ledger.napping(t0 + secs(302), i0 + Duration::from_secs(32));
        assert!(!ledger.due(t0 + secs(3599)));
        assert!(ledger.due(t0 + secs(3600)));
        let line = ledger.close(t0 + secs(3600), Some(MilliampHours(490)));
        assert_eq!(
            line,
            "doze ledger over 60 min: awake 28 s in 4 wakes (6.9 s each, 1 online; \
             1 ticks 0.50 s each, 3 full 9.00 s each), asleep 274 s (monotonic 4 s), \
             Wi-Fi up 20 s, held awake 0 s (0 maintenance windows, 0 s for SSH), \
             panel done 1 / timed 1 / not seen 0, gauge 10 mAh (10 mA), \
             490 mAh left, current at wake -5 mA, \
             clock corrected 0 times by the RTC (+0.0 s) and 0 by the network (+0.0 s)"
        );
        // The next hour counts from the last gauge reading.
        let line = ledger.close(t0 + secs(7200), None);
        assert!(line.contains("awake 0 s in 0 wakes"), "{line}");
        assert!(line.contains("no gauge, current at wake none"), "{line}");
    }

    #[test]
    fn the_ledger_counts_dhcp_exchanges_an_hour() {
        use crate::dhcpc::Counts;
        let t0 = wall(1, 0, 0);
        let mut ledger = DozeLedger::new(t0, None);
        let counts = |exchanges, packets| Counts { exchanges, packets };
        ledger.dhcp(counts(5, 9));
        ledger.dhcp(counts(6, 11));
        let line = ledger.close(t0 + chrono::TimeDelta::minutes(30), None);
        assert!(
            line.ends_with(", DHCP 1 exchanges (2.0 an hour, 2 messages)"),
            "{line}"
        );
        // The next ledger counts from there.
        ledger.dhcp(counts(6, 11));
        let line = ledger.close(t0 + chrono::TimeDelta::minutes(90), None);
        assert!(line.ends_with(", DHCP 0 exchanges (0.0 an hour, 0 messages)"));
    }

    #[test]
    fn the_ledger_notes_clock_corrections_and_a_network_step_moves_its_times() {
        use crate::clock::{Correction, Source};
        let t0 = wall(1, 0, 0);
        let i0 = Instant::now();
        let secs = |s: i64| t0 + chrono::TimeDelta::seconds(s);
        let mut ledger = DozeLedger::new(t0, None);
        ledger.napping(t0, i0);
        // Resumes gave back time they lost: it passed, and counts as asleep.
        ledger.clock_corrected(Correction {
            seconds: 1.4,
            source: Source::Rtc,
        });
        ledger.woke(secs(60), i0, Window::Online, Redraw::Full);
        ledger.wifi_on(secs(60));
        // Network time, 10 s into the wake: the clock was 724 s slow.
        ledger.clock_corrected(Correction {
            seconds: 724.0,
            source: Source::Ntp("time.google.com"),
        });
        ledger.wifi_off(secs(724 + 70));
        ledger.napping(secs(724 + 70), i0);
        ledger.clock_corrected(Correction {
            seconds: 1.1,
            source: Source::Rtc,
        });
        // The hour counts from the doze's start on the right clock.
        assert!(!ledger.due(secs(3599)));
        assert!(ledger.due(secs(724 + 3600)));
        let line = ledger.close(secs(724 + 3600), None);
        assert!(
            line.starts_with("doze ledger over 60 min: awake 10 s in 1 wakes"),
            "{line}"
        );
        assert!(line.contains("Wi-Fi up 10 s"), "{line}");
        assert!(
            line.ends_with(
                "clock corrected 2 times by the RTC (+2.5 s) and 1 by the network (+724.0 s)"
            ),
            "{line}"
        );
        // A clock set back before the ledger's start: logged, not left for hours.
        let mut ledger = DozeLedger::new(t0, None);
        assert!(ledger.due(t0 - chrono::TimeDelta::seconds(1)));
        assert!(!ledger.due(t0 + chrono::TimeDelta::seconds(1)));
        ledger.napping(t0, i0);
    }

    #[test]
    fn the_ledger_accounts_maintenance_windows_and_ssh_holds() {
        let t0 = wall(1, 0, 0);
        let i0 = Instant::now();
        let secs = |s: i64| t0 + chrono::TimeDelta::seconds(s);
        let mut ledger = DozeLedger::new(t0, None);
        // An online wake of 20 s opens a window: held to the next minute.
        ledger.woke(secs(1), i0, Window::Online, Redraw::Full);
        ledger.window_opened();
        ledger.holding(secs(21), Hold::Window);
        ledger.woke(secs(61), i0, Window::Draw, Redraw::Tick);
        ledger.holding(secs(62), Hold::Window);
        // The window closes with a session open: held for SSH from 141 s.
        ledger.holding(secs(141), Hold::Ssh);
        ledger.woke(secs(181), i0, Window::Draw, Redraw::Tick);
        ledger.holding(secs(182), Hold::Ssh);
        ledger.napping(secs(192), i0);
        // Held over the close: counted up to it, and from it in the next.
        ledger.woke(secs(241), i0, Window::Draw, Redraw::Full);
        ledger.holding(secs(250), Hold::Ssh);
        let line = ledger.close(secs(260), None);
        assert!(
            line.contains("awake 31 s in 4 wakes (7.8 s each, 1 online; 2 ticks 1.00 s each"),
            "{line}"
        );
        assert!(
            line.contains("held awake 179 s (1 maintenance windows, 60 s for SSH)"),
            "{line}"
        );
        ledger.napping(secs(270), i0);
        let line = ledger.close(secs(300), None);
        assert!(
            line.contains("held awake 10 s (0 maintenance windows, 10 s for SSH)"),
            "{line}"
        );
    }

    #[test]
    fn a_refused_nap_is_retried_after_a_redraw() {
        let (mode, effects) = run(&[
            (Event::Doze, 10_000),
            (Event::LightOff(None), 10_100),
            (Event::PanelSettled, 11_600),
            (Event::SuspendFailed, 11_700),
            (Event::PanelSettled, 14_700),
        ]);
        assert_eq!(mode, Mode::Dozing(Doze::Napping { light: None }));
        assert_eq!(effects[3..], [Effect::Redraw, Effect::Nap]);
    }

    #[test]
    fn usb_while_dozing_brings_the_light_back_first() {
        let light = Brightness::new(40);
        let (mode, effects) = run(&[
            (Event::Doze, 10_000),
            (Event::LightOff(Some(light)), 10_100),
            (Event::Drive(Drive::Computer), 11_000),
        ]);
        assert_eq!(mode, Mode::UsbDrive);
        assert_eq!(
            effects[2..],
            [
                Effect::Wake,
                Effect::RestoreLight(light),
                Effect::ReleaseStorage,
                Effect::ShowUsbDrive
            ]
        );
    }

    #[test]
    fn a_flapping_cable_keeps_the_usb_screen_until_the_storage_stays() {
        let t0 = Instant::now();
        let at = |ms| t0 + Duration::from_millis(ms);
        let (mode, effects) = run_from(
            t0,
            &[
                (Event::Drive(Drive::Computer), 1_000),
                (Event::Drive(Drive::Kindle), 2_000),
                (Event::Drive(Drive::Computer), 3_000),
                (Event::Drive(Drive::Kindle), 4_000),
                // The timer from the first return fires: stale.
                (Event::DriveSettled(at(2_000)), 5_000),
                (Event::DriveSettled(at(4_000)), 7_000),
            ],
        );
        assert!(matches!(mode, Mode::Awake { .. }));
        assert_eq!(
            effects,
            [
                Effect::ReleaseStorage,
                Effect::ShowUsbDrive,
                Effect::AwaitDrive(at(2_000)),
                Effect::AwaitDrive(at(4_000)),
                Effect::Wake,
                Effect::StorageBack,
            ]
        );
    }

    #[test]
    fn usb_while_asleep_wakes_first() {
        let (mode, effects) = run(&[
            (Event::PowerButton, 10_000),
            (Event::LightOff(None), 10_100),
            (Event::PanelSettled, 11_600),
            (Event::StorageLeaving, 20_000),
            (Event::Drive(Drive::Computer), 23_000),
            (Event::PowerButton, 30_000),
        ]);
        assert_eq!(mode, Mode::UsbDrive);
        assert_eq!(
            effects[4..],
            [
                Effect::ReleaseStorage,
                Effect::Wake,
                Effect::ReleaseStorage,
                Effect::ShowUsbDrive
            ]
        );
    }

    #[test]
    fn idle_time_counts_from_the_last_touch() {
        let t0 = Instant::now();
        let idle = Idle::new(5, t0)
            .observe(5, t0 + Duration::from_secs(60))
            .observe(6, t0 + Duration::from_secs(120));
        let setting = AutoSleep::Minutes(5);
        assert_eq!(idle.due(setting, t0 + Duration::from_secs(300)), None);
        assert_eq!(
            idle.due(setting, t0 + Duration::from_secs(420)),
            Some(Event::Idle)
        );
        assert_eq!(
            idle.due(AutoSleep::Never, t0 + Duration::from_secs(99_999)),
            None
        );
        assert_eq!(
            idle.due(AutoSleep::Doze, t0 + Duration::from_secs(200)),
            None
        );
        assert_eq!(
            idle.due(AutoSleep::Doze, t0 + Duration::from_secs(240)),
            Some(Event::Doze)
        );
    }

    #[test]
    fn auto_sleep_labels_and_storage() {
        let labels: Vec<String> = AutoSleep::CHOICES.iter().map(|c| c.label()).collect();
        assert_eq!(
            labels,
            ["Doze", "Never", "5 min", "15 min", "30 min", "1 h"]
        );
        #[derive(Serialize, Deserialize)]
        struct Holder {
            auto_sleep: AutoSleep,
        }
        let text = toml::to_string(&Holder {
            auto_sleep: AutoSleep::Minutes(15),
        })
        .unwrap();
        let back: Holder = toml::from_str(&text).unwrap();
        assert_eq!(back.auto_sleep, AutoSleep::Minutes(15));
        let never: Holder = toml::from_str("auto_sleep = \"never\"").unwrap();
        assert_eq!(never.auto_sleep, AutoSleep::Never);
        let doze: Holder = toml::from_str("auto_sleep = \"doze\"").unwrap();
        assert_eq!(doze.auto_sleep, AutoSleep::Doze);
    }

    #[test]
    fn a_refused_export_brings_the_storage_back_in_any_mode() {
        use crate::drive::{Notice, Refusal};
        let refused = Notice::Refused(Refusal::Gadget);
        let (mode, effects) = run(&[
            (Event::StorageLeaving, 10_000),
            (Event::DriveNotice(refused), 10_500),
        ]);
        assert!(matches!(mode, Mode::Awake { .. }));
        assert_eq!(
            effects,
            [
                Effect::ReleaseStorage,
                Effect::StorageBack,
                Effect::DriveNotice(refused)
            ]
        );
        let (mode, effects) = run(&[
            (Event::Drive(Drive::Computer), 10_000),
            (Event::Drive(Drive::Kindle), 20_000),
            (Event::DriveNotice(Notice::Repaired), 20_100),
        ]);
        assert!(matches!(mode, Mode::Returning { .. }));
        assert_eq!(effects.last(), Some(&Effect::DriveNotice(Notice::Repaired)));
    }

    #[test]
    fn volumd_lines() {
        assert_eq!(
            volumd_event("driveModeStateChanged 1"),
            Some(Event::Drive(Drive::Computer))
        );
        assert_eq!(
            volumd_event("userstoreIsLikelyToUnMount"),
            Some(Event::StorageLeaving)
        );
        assert_eq!(volumd_event("driveModeStateChanged 3"), None);
        assert_eq!(volumd_event("userstoreAvailable"), None);
    }
}
