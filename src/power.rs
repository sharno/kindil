//! What Amazon's powerd did besides reading the battery, which kindil now
//! does itself while it runs: charging only at temperatures safe for the
//! cell, and powering off before the battery is spent.
//!
//! The charger charges on its own (verified: it keeps charging with powerd
//! stopped); powerd only paused it outside its temperature range, through
//! the same sysfs switch kindil uses.
use crate::battery::{Battery, Charge};
use std::time::Duration;

/// powerd's charging range on the PW1 (its `charging_temp_min/max`), in °F,
/// the unit the fuel gauge reports.
pub const CHARGE_MIN_F: i32 = 32;
pub const CHARGE_MAX_F: i32 = 113;
/// Back inside the range by this much before charging resumes, so a cell
/// sitting at a limit doesn't switch the charger on and off.
pub const CHARGE_MARGIN_F: i32 = 3;

/// At or below this, on its own power, the Kindle powers off as powerd did:
/// suspended, the last percent would drain the cell flat.
pub const CRITICAL_PERCENT: u8 = 1;

/// Asleep on battery, the Kindle wakes this often to check for
/// [`CRITICAL_PERCENT`] and goes straight back to sleep otherwise.
pub const CHECK_ASLEEP: Duration = Duration::from_secs(6 * 60 * 60);

/// Whether charging should be paused at `temperature_f`, given whether it
/// is paused now.
pub fn pause_charging(temperature_f: i32, paused: bool) -> bool {
    let outside = !(CHARGE_MIN_F..=CHARGE_MAX_F).contains(&temperature_f);
    let near_limit =
        !(CHARGE_MIN_F + CHARGE_MARGIN_F..=CHARGE_MAX_F - CHARGE_MARGIN_F).contains(&temperature_f);
    outside || (paused && near_limit)
}

pub fn critical(battery: Battery) -> bool {
    matches!(
        battery,
        Battery::Known {
            percent,
            charge: Charge::Discharging,
        } if percent <= CRITICAL_PERCENT
    )
}

/// Whether a suspend ended by its alarm (due at `due`, in seconds since the
/// epoch) rather than the power button, judged by the time: the PW1's RTC
/// doesn't read its alarm back.
pub fn alarm_woke(due: i64, now: i64) -> bool {
    now >= due - 2
}

/// How long after a wake the charger may take to say it is connected.
/// Plugging a charger in wakes the PW1 at once, but the charger driver
/// makes its `connected` file seconds later. Measured on 0.15.0 (naps.log
/// and syslog, 2026-11-18): woken at 19:00:47.5, `connected` still missing
/// at 19:00:49.7, and hal's `usbPlugIn` at 19:00:52.
pub const CHARGER_DETECT: Duration = Duration::from_secs(10);
/// How often a wake from the battery's sleep looks for the power button or
/// the charger while [`CHARGER_DETECT`] lasts.
pub const CHARGER_POLL: Duration = Duration::from_millis(100);

/// The device under a sleep: [`sleep`] is written against this, so the
/// order in which a wake, the power button and the charger arrive can be
/// played back in a test.
pub trait Sleeper {
    /// Whether a charger is connected.
    fn plugged(&self) -> bool;
    /// How many times the power button was pressed since kindil started.
    fn presses(&self) -> u64;
    /// Suspends until `wake` (src/suspend.rs); returns once awake.
    fn suspend(&self, wake: crate::suspend::Wake) -> anyhow::Result<()>;
    /// Seconds since the epoch.
    fn now(&self) -> i64;
    fn battery(&self) -> anyhow::Result<Battery>;
    fn wait(&self, time: Duration);
    /// A line for the flight recorder.
    fn note(&self, line: &str);
}

/// How a [`sleep`] ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slept {
    /// Awake for the app: the power button, a charger, or, for a sleep the
    /// user asked for, any wake but its own battery check.
    Woken,
    /// Never suspended: a sleep for the empty battery with a charger in.
    Charging,
    /// The battery is at [`CRITICAL_PERCENT`]: power off.
    Critical,
}

/// What woke a sleep for the empty battery, if its alarm didn't.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Woke {
    PowerButton,
    Charger,
}

/// Sleeps until the app should wake, checking the battery every
/// [`CHECK_ASLEEP`] while on battery. A sleep for the empty battery is left
/// only for the power button or a connected charger: any other wake,
/// including a charger's plug-in before its driver says it is connected,
/// waits out [`CHARGER_DETECT`] and sleeps on, with the charge screen
/// untouched. And it is no sleep at all while a charger is in: the decision
/// to sleep was taken on a reading from before it was plugged.
pub fn sleep(device: &impl Sleeper, why: crate::system::Why) -> anyhow::Result<Slept> {
    use crate::suspend::{Asked, Wake};
    use crate::system::Why;
    loop {
        let plugged = device.plugged();
        // A nap's alarm must not wake a sleep the user asked for; on
        // battery, a check of its own every few hours does. Only a sleep
        // the user asked for may go without it: the battery's must check.
        let wake = match (plugged, Asked::of(why)) {
            (false, Some(asked)) => Wake::AlarmOrButton(CHECK_ASLEEP, asked),
            (false, None) => Wake::Alarm(CHECK_ASLEEP),
            (true, Some(asked)) => Wake::Button(asked),
            (true, None) => {
                device.note("sleep: a charger is connected; no sleep for the battery");
                return Ok(Slept::Charging);
            }
        };
        let due = wake
            .alarm()
            .map(|after| device.now() + after.as_secs() as i64);
        let presses = device.presses();
        device.suspend(wake)?;
        if due.is_some_and(|due| alarm_woke(due, device.now())) {
            let battery = device.battery()?;
            if critical(battery) {
                device.note(&format!("sleep: battery critical ({battery:?})"));
                return Ok(Slept::Critical);
            }
            continue;
        }
        match why {
            Why::Asked => return Ok(Slept::Woken),
            Why::BatteryEmpty => match woke(device, presses) {
                Some(woke) => {
                    device.note(&format!("sleep: woken by the {woke:?}"));
                    return Ok(Slept::Woken);
                }
                None => device.note(&format!(
                    "sleep: woken by neither the power button nor a charger in {} s; \
                     sleeping on",
                    CHARGER_DETECT.as_secs()
                )),
            },
        }
    }
}

/// Watches for [`CHARGER_DETECT`] after a wake: the power button, pressed
/// since `presses` was read, or a connected charger.
fn woke(device: &impl Sleeper, presses: u64) -> Option<Woke> {
    let polls = CHARGER_DETECT.as_millis() / CHARGER_POLL.as_millis();
    for poll in 0..=polls {
        if device.presses() != presses {
            return Some(Woke::PowerButton);
        }
        if device.plugged() {
            return Some(Woke::Charger);
        }
        if poll < polls {
            device.wait(CHARGER_POLL);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charging_pauses_outside_the_range_and_resumes_inside_the_margin() {
        assert!(!pause_charging(77, false));
        assert!(pause_charging(CHARGE_MAX_F + 1, false), "too hot");
        assert!(pause_charging(CHARGE_MIN_F - 1, false), "too cold");
        assert!(!pause_charging(CHARGE_MAX_F, false), "at the limit is fine");
        // Paused, it waits to be well inside again.
        assert!(pause_charging(CHARGE_MAX_F - 1, true));
        assert!(!pause_charging(CHARGE_MAX_F - CHARGE_MARGIN_F, true));
        assert!(pause_charging(CHARGE_MIN_F + 2, true));
        assert!(!pause_charging(CHARGE_MIN_F + CHARGE_MARGIN_F, true));
    }

    #[test]
    fn only_a_nearly_empty_battery_on_its_own_power_is_critical() {
        let battery = |percent, charge| Battery::Known { percent, charge };
        assert!(critical(battery(1, Charge::Discharging)));
        assert!(critical(battery(0, Charge::Discharging)));
        assert!(!critical(battery(2, Charge::Discharging)));
        assert!(!critical(battery(0, Charge::Charging)), "plugged in");
        assert!(!critical(Battery::Unknown));
    }

    /// A device on a clock in milliseconds: each suspend ends at the next
    /// of `wakes`, the charger is connected from `connected` on, and the
    /// power button is pressed at each of `presses`.
    struct Device {
        clock: std::cell::Cell<u64>,
        wakes: std::cell::RefCell<std::collections::VecDeque<u64>>,
        connected: Option<u64>,
        presses: Vec<u64>,
        percent: u8,
        alarms: std::cell::RefCell<Vec<Option<Duration>>>,
        wakes_asked: std::cell::RefCell<Vec<crate::suspend::Wake>>,
        notes: std::cell::RefCell<Vec<String>>,
    }

    impl Device {
        fn new(wakes: &[u64], connected: Option<u64>, presses: &[u64]) -> Self {
            Self {
                clock: std::cell::Cell::new(0),
                wakes: std::cell::RefCell::new(wakes.iter().copied().collect()),
                connected,
                presses: presses.to_vec(),
                percent: 4,
                alarms: std::cell::RefCell::default(),
                wakes_asked: std::cell::RefCell::default(),
                notes: std::cell::RefCell::default(),
            }
        }

        fn suspends(&self) -> usize {
            self.alarms.borrow().len()
        }
    }

    impl Sleeper for Device {
        fn plugged(&self) -> bool {
            self.connected.is_some_and(|at| self.clock.get() >= at)
        }
        fn presses(&self) -> u64 {
            self.presses
                .iter()
                .filter(|&&at| at <= self.clock.get())
                .count() as u64
        }
        fn suspend(&self, wake: crate::suspend::Wake) -> anyhow::Result<()> {
            self.alarms.borrow_mut().push(wake.alarm());
            self.wakes_asked.borrow_mut().push(wake);
            let wake = self
                .wakes
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("asleep with nothing left to wake it"))?;
            self.clock.set(wake);
            Ok(())
        }
        fn now(&self) -> i64 {
            1_794_000_000 + (self.clock.get() / 1000) as i64
        }
        fn battery(&self) -> anyhow::Result<Battery> {
            Ok(Battery::Known {
                percent: self.percent,
                charge: if self.plugged() {
                    Charge::Charging
                } else {
                    Charge::Discharging
                },
            })
        }
        fn wait(&self, time: Duration) {
            self.clock.set(self.clock.get() + time.as_millis() as u64);
        }
        fn note(&self, line: &str) {
            self.notes.borrow_mut().push(line.to_owned());
        }
    }

    use crate::system::Why;

    #[test]
    fn a_charger_plugged_into_the_battery_sleep_wakes_it_once_connected() {
        // As on 2026-11-18: the plug-in wakes the device 45 s into the
        // sleep; the charger's driver says connected 2.5 s later, and a
        // second wake (the charger's own interrupt) follows. On 0.15.0 each
        // wake showed the dashboard, read "discharging at 4%" and slept
        // again; the last such sleep began after `connected` appeared and
        // so lasted until the power button.
        let device = Device::new(&[45_000, 49_000], Some(47_500), &[]);
        assert_eq!(sleep(&device, Why::BatteryEmpty).unwrap(), Slept::Woken);
        assert!(device.plugged(), "the app's first read finds the charger");
        assert_eq!(device.suspends(), 1, "the app wakes once, for the charger");
        assert_eq!(device.alarms.borrow()[0], Some(CHECK_ASLEEP));
        assert_eq!(*device.notes.borrow(), ["sleep: woken by the Charger"]);
    }

    #[test]
    fn a_sleep_for_the_battery_with_a_charger_in_never_suspends() {
        // The decision to sleep came from a reading taken before the
        // charger was in: 0.15.0 then slept "until the power button".
        let device = Device::new(&[], Some(0), &[]);
        assert_eq!(sleep(&device, Why::BatteryEmpty).unwrap(), Slept::Charging);
        assert_eq!(device.suspends(), 0);
    }

    #[test]
    fn a_wake_by_neither_button_nor_charger_keeps_the_battery_sleep() {
        let device = Device::new(&[45_000, 120_000], None, &[120_000]);
        assert_eq!(sleep(&device, Why::BatteryEmpty).unwrap(), Slept::Woken);
        assert_eq!(device.suspends(), 2, "slept on after the first wake");
        assert_eq!(
            device.notes.borrow().last().map(String::as_str),
            Some("sleep: woken by the PowerButton")
        );
    }

    #[test]
    fn the_battery_sleep_checks_its_alarm_and_powers_off_when_critical() {
        let six_hours = CHECK_ASLEEP.as_millis() as u64;
        let mut device = Device::new(&[six_hours, 2 * six_hours], None, &[]);
        device.percent = 2;
        // Each check finds 2% and sleeps on, until nothing is left to wake it.
        assert!(sleep(&device, Why::BatteryEmpty).is_err());
        assert_eq!(device.suspends(), 3);
        assert!(device.notes.borrow().is_empty(), "no wake but the checks");
        device = Device::new(&[six_hours], None, &[]);
        device.percent = 1;
        assert_eq!(sleep(&device, Why::BatteryEmpty).unwrap(), Slept::Critical);
    }

    #[test]
    fn a_sleep_the_user_asked_for_ends_at_any_wake_but_its_check() {
        let device = Device::new(&[45_000], None, &[]);
        assert_eq!(sleep(&device, Why::Asked).unwrap(), Slept::Woken);
        assert_eq!(device.suspends(), 1);
        // Plugged in, it sleeps until the button, with no check.
        let device = Device::new(&[45_000], Some(0), &[]);
        assert_eq!(sleep(&device, Why::Asked).unwrap(), Slept::Woken);
        assert_eq!(*device.alarms.borrow(), [None]);
    }

    /// The battery's sleep never goes without its check's alarm: no alarm,
    /// no suspend (src/suspend.rs). Only a sleep the user asked for falls
    /// back to the power button.
    #[test]
    fn only_a_sleep_the_user_asked_for_may_go_without_an_alarm() {
        use crate::suspend::{Asked, Wake};
        let device = Device::new(&[45_000], None, &[45_000]);
        sleep(&device, Why::BatteryEmpty).unwrap();
        assert_eq!(*device.wakes_asked.borrow(), [Wake::Alarm(CHECK_ASLEEP)]);
        let asked = Asked::of(Why::Asked).unwrap();
        let device = Device::new(&[45_000], None, &[]);
        sleep(&device, Why::Asked).unwrap();
        assert_eq!(
            *device.wakes_asked.borrow(),
            [Wake::AlarmOrButton(CHECK_ASLEEP, asked)]
        );
        assert_eq!(Asked::of(Why::BatteryEmpty), None);
    }

    #[test]
    fn the_alarm_is_told_from_the_power_button_by_the_time() {
        assert!(alarm_woke(1000, 1000));
        assert!(alarm_woke(1000, 999), "a second's slack");
        assert!(alarm_woke(1000, 1500));
        assert!(!alarm_woke(1000, 400), "pressed long before the alarm");
    }
}
