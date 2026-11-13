//! The OS duties Amazon's UI performs and kindil takes over while it runs:
//! sleeping on the power button, and stepping aside while a computer has the
//! user storage over USB.
//!
//! Events arrive from the kernel (power button), volumd (USB drive mode) and
//! kindil's own workers (light switched off, suspend returned), so this is a
//! reducer: a pure `step` from (mode, event) to the next mode and the effects
//! to run. The UI runs the effects.
use crate::host::Brightness;
use std::time::{Duration, Instant};

/// Waking from suspend takes a power press, and the kernel reports that same
/// press once the device is back. It must not put the device straight back
/// to sleep.
pub const WAKING_PRESS: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    PowerButton,
    /// The front light is off; this was its level, if it could be read.
    LightOff(Option<Brightness>),
    /// The device suspended and woke up again.
    Resumed,
    /// The kernel refused to suspend; the device stayed up with the screen asleep.
    SuspendFailed,
    /// volumd will hand the user storage to a computer in a moment.
    StorageLeaving,
    Drive(Drive),
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
    /// A computer has the user storage; kindil shows the USB screen.
    UsbDrive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sleep {
    /// The sleep screen is up; the light is being switched off.
    DimmingLight,
    /// Suspended, or about to be.
    Suspending { light: Option<Brightness> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    ShowSleep,
    LightOff,
    Suspend,
    /// Back to the app, restoring the light's level if it was saved.
    Wake {
        light: Option<Brightness>,
    },
    ShowUsbDrive,
    /// Close everything that holds files on the user storage.
    ReleaseStorage,
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
    /// A worker finished after the mode moved on, or volumd repeated itself.
    Stale,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub next: Mode,
    pub effects: Vec<Effect>,
}

fn to(next: Mode, effects: impl Into<Vec<Effect>>) -> Result<Step, Ignored> {
    Ok(Step {
        next,
        effects: effects.into(),
    })
}

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
            (M::Awake { .. }, E::PowerButton) => to(
                M::Asleep(Sleep::DimmingLight),
                [Effect::ShowSleep, Effect::LightOff],
            ),
            (M::Asleep(Sleep::DimmingLight), E::LightOff(light)) => {
                to(M::Asleep(Sleep::Suspending { light }), [Effect::Suspend])
            }
            (M::Asleep(sleep), E::PowerButton | E::Resumed) => to(
                awake,
                [Effect::Wake {
                    light: sleep.light(),
                }],
            ),
            // The sleep screen stays and the light stays off; the next press wakes.
            (M::Asleep(Sleep::Suspending { .. }), E::SuspendFailed) => to(self, []),
            (_, E::StorageLeaving) => to(self, [Effect::ReleaseStorage]),
            (M::Awake { .. }, E::Drive(Drive::Computer)) => {
                to(M::UsbDrive, [Effect::ReleaseStorage, Effect::ShowUsbDrive])
            }
            (M::Asleep(sleep), E::Drive(Drive::Computer)) => to(
                M::UsbDrive,
                [
                    Effect::Wake {
                        light: sleep.light(),
                    },
                    Effect::ReleaseStorage,
                    Effect::ShowUsbDrive,
                ],
            ),
            (M::UsbDrive, E::Drive(Drive::Kindle)) => {
                to(awake, [Effect::Wake { light: None }, Effect::StorageBack])
            }
            (M::UsbDrive, E::PowerButton) => Err(Ignored::InUsbDrive),
            (M::Awake { .. }, E::LightOff(_) | E::Resumed | E::SuspendFailed)
            | (M::Asleep(Sleep::Suspending { .. }), E::LightOff(_))
            | (M::Asleep(Sleep::DimmingLight), E::SuspendFailed)
            | (M::Awake { .. } | M::Asleep(_), E::Drive(Drive::Kindle))
            | (M::UsbDrive, E::LightOff(_) | E::Resumed | E::SuspendFailed)
            | (M::UsbDrive, E::Drive(Drive::Computer)) => Err(Ignored::Stale),
        }
    }
}

impl Sleep {
    fn light(self) -> Option<Brightness> {
        match self {
            Sleep::DimmingLight => None,
            Sleep::Suspending { light } => light,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn run(events: &[(Event, u64)]) -> (Mode, Vec<Effect>) {
        let t0 = Instant::now();
        events.iter().fold(
            (Mode::start(t0), Vec::new()),
            |(mode, mut all), &(event, at)| match mode.step(event, t0 + Duration::from_secs(at)) {
                Ok(step) => {
                    all.extend(step.effects);
                    (step.next, all)
                }
                Err(_) => (mode, all),
            },
        )
    }

    #[test]
    fn a_press_sleeps_and_the_next_wakes_with_the_light_back() {
        let light = Some(Brightness::new(40));
        let (mode, effects) = run(&[
            (Event::PowerButton, 10),
            (Event::LightOff(light), 10),
            (Event::Resumed, 100),
            // The press that woke the device, reported after resume.
            (Event::PowerButton, 101),
        ]);
        assert!(matches!(mode, Mode::Awake { .. }));
        assert_eq!(
            effects,
            [
                Effect::ShowSleep,
                Effect::LightOff,
                Effect::Suspend,
                Effect::Wake { light }
            ]
        );
    }

    #[test]
    fn a_later_press_sleeps_again() {
        let (mode, _) = run(&[
            (Event::PowerButton, 10),
            (Event::LightOff(None), 10),
            (Event::Resumed, 100),
            (Event::PowerButton, 120),
        ]);
        assert_eq!(mode, Mode::Asleep(Sleep::DimmingLight));
    }

    #[test]
    fn usb_drive_mode_wakes_releases_and_returns() {
        let (mode, effects) = run(&[
            (Event::PowerButton, 10),
            (Event::LightOff(None), 10),
            (Event::StorageLeaving, 20),
            (Event::Drive(Drive::Computer), 23),
            (Event::PowerButton, 30),
            (Event::Drive(Drive::Kindle), 90),
        ]);
        assert!(matches!(mode, Mode::Awake { .. }));
        assert_eq!(
            effects,
            [
                Effect::ShowSleep,
                Effect::LightOff,
                Effect::Suspend,
                Effect::ReleaseStorage,
                Effect::Wake { light: None },
                Effect::ReleaseStorage,
                Effect::ShowUsbDrive,
                Effect::Wake { light: None },
                Effect::StorageBack,
            ]
        );
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

    #[test]
    fn volumd_states() {
        assert_eq!(drive_mode(1), Some(Drive::Computer));
        assert_eq!(drive_mode(0), Some(Drive::Kindle));
        assert_eq!(drive_mode(2), None);
    }
}
