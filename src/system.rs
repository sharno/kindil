//! The OS duties Amazon's UI performs and kindil takes over while it runs:
//! sleeping on the power button or when idle, dozing between screen updates,
//! and stepping aside while a computer has the user storage over USB.
//!
//! Events arrive from the kernel (power button), volumd (USB drive mode), the
//! minute clock (idle) and kindil's own workers and timers, so this is a
//! reducer: a pure `step` from (mode, event) to the next mode and the effects
//! to run. The UI runs the effects.
use crate::host::Brightness;
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
/// Awake after a nap: long enough to render the minute and for the panel to
/// show it.
pub const DRAW_WINDOW: Duration = Duration::from_secs(3);
/// While dozing, how often kindil stays up for Wi-Fi to fetch the weather.
pub const ONLINE_EVERY: chrono::TimeDelta = chrono::TimeDelta::minutes(30);
/// Awake that long when online: Wi-Fi reconnects in about 15 s after a nap.
pub const ONLINE_WINDOW: Duration = Duration::from_secs(45);
/// When, in that window, the dashboard fetches.
pub const FETCH_AFTER: Duration = Duration::from_secs(25);
/// Shorter naps aren't worth a suspend: kindil waits for the minute awake.
pub const MIN_NAP: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    PowerButton,
    /// Nobody touched the screen for the auto-sleep time.
    Idle,
    /// Nobody touched the screen for [`DOZE_AFTER`], and kindil dozes.
    Doze,
    /// The front light is off; this was its level, if it could be read.
    LightOff(Option<Brightness>),
    /// The sleep screen, or a dozing redraw, has had time to reach the panel.
    PanelSettled,
    /// The device suspended, or napped, and woke up again.
    Resumed,
    /// The kernel refused to suspend; the device stayed up with the screen asleep.
    SuspendFailed,
    /// volumd will hand the user storage to a computer in a moment.
    StorageLeaving,
    Drive(Drive),
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
    RestoreLight(Brightness),
    ShowUsbDrive,
    /// Close everything that holds files on the user storage.
    ReleaseStorage,
    /// Deliver [`Event::DriveSettled`] with this instant after [`DRIVE_SETTLE`].
    AwaitDrive(Instant),
    /// The user storage is back, possibly changed by the computer.
    StorageBack,
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
            (M::Awake { .. }, E::PowerButton | E::Idle) => {
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
                std::iter::once(Effect::Rouse).chain(doze.light().map(Effect::RestoreLight)),
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
            (M::Asleep(sleep), E::PowerButton | E::Resumed) => to(awake, wake(sleep.light())),
            // The sleep screen stays and the light stays off; the next press wakes.
            (M::Asleep(Sleep::Suspended { .. }), E::SuspendFailed) => to(self, []),
            (_, E::StorageLeaving) => to(self, [Effect::ReleaseStorage]),
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
}

impl Doze {
    fn light(self) -> Option<Brightness> {
        match self {
            Doze::DimmingLight => None,
            Doze::Settling { light } | Doze::Napping { light } | Doze::Drawing { light } => light,
        }
    }
}

/// What a dozing wake does besides drawing the minute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Window {
    /// Draw, then nap again after [`DRAW_WINDOW`].
    Draw,
    /// Also let Wi-Fi reconnect and fetch, for [`ONLINE_WINDOW`].
    Online,
}

impl Window {
    /// Online at the first wake and then every [`ONLINE_EVERY`] of wall time;
    /// the monotonic clock stops while the device naps.
    pub fn after_nap(last_online: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Self {
        match last_online {
            Some(last) if now - last < ONLINE_EVERY => Window::Draw,
            _ => Window::Online,
        }
    }

    pub fn length(self) -> Duration {
        match self {
            Window::Draw => DRAW_WINDOW,
            Window::Online => ONLINE_WINDOW,
        }
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
                Effect::RestoreLight(light)
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
            ]
        );
    }

    #[test]
    fn dozing_goes_online_every_half_hour_of_wall_time() {
        let t0 = DateTime::from_timestamp(1_794_398_400, 0).unwrap();
        let at = |minutes| t0 + chrono::TimeDelta::minutes(minutes);
        assert_eq!(Window::after_nap(None, t0), Window::Online);
        assert_eq!(Window::after_nap(Some(t0), at(29)), Window::Draw);
        assert_eq!(Window::after_nap(Some(t0), at(30)), Window::Online);
        assert!(FETCH_AFTER < ONLINE_WINDOW);
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
