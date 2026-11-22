//! What Amazon's powerd did besides reading the battery, which kindil now
//! does itself while it runs: charging only at temperatures safe for the
//! cell, and powering off before the battery is spent; and the charger
//! driver's stop at full and restart at 95%, which the driver doesn't keep
//! up while the Kindle dozes ([`govern_charging`]).
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

/// The charger driver's end of charge (Amazon's GPL
/// arch/arm/mach-mx5/aplite_charger.c, `charger_loop`): full at 100% with
/// the charging current under [`EOC_MILLIAMPS`], charging again at
/// [`RESTART_PERCENT`] or below. The driver runs that rule on its own cached
/// gauge values, and only while its work is scheduled: each suspend cancels
/// the work, and only a charger event schedules it again, so a dozing
/// Kindle stops at full and may never restart. kindil runs the same rule on
/// the chip's values (src/gauge.rs) at each of its readings instead.
pub const FULL_PERCENT: u8 = 100;
/// `EOC_BATTERY_CURRENT`.
pub const EOC_MILLIAMPS: i32 = 55;
/// `RESTART_BATTERY_CAPACITY`.
pub const RESTART_PERCENT: u8 = 95;
/// After a restart of a charger kindil didn't stop, how long before it
/// tries again while the chip still says it isn't charging.
pub const RESTART_RETRY: Duration = Duration::from_secs(30 * 60);

/// Why kindil holds the charger off (its `charging` switch at `0`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hold {
    /// Full: until [`RESTART_PERCENT`].
    Full,
    /// Outside powerd's temperature range: until well inside it again.
    Temperature,
}

/// What kindil remembers of the charger between readings. Its memory is
/// checked against the switch at each one: a crashed kindil's successor,
/// or the driver re-enabling charging on a replug, is seen there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Charging {
    pub held: Option<Hold>,
    /// When kindil last restarted a charger that had stopped on its own,
    /// in seconds since the epoch.
    pub restarted: Option<i64>,
}

/// One reading: the charger driver's and the gauge chip's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seen {
    /// `connected` reads 1.
    pub plugged: bool,
    /// The `charging` switch reads 1: plugged in and either charging or
    /// stopped full by the driver itself. Plugged in, it reads 0 only after
    /// a `0` was written to it (kindil's, or powerd's before it).
    pub enabled: bool,
    pub percent: u8,
    /// mA, positive while the chip's FLAGS say charging.
    pub current: i32,
    pub temperature_f: i32,
    /// The PMIC's charger state machine is charging (CHGENS,
    /// src/pmic.rs), if `/dev/pmic` could be read.
    pub running: Option<bool>,
}

impl Seen {
    /// Whether the charger charges: the PMIC says so where it can be
    /// read. The gauge's sign alone misleads near full: a few tens of mA
    /// of charging current under a wake's draw read as discharging.
    pub fn charging(&self) -> bool {
        self.running.unwrap_or(self.current > 0)
    }
}

/// What to do with the charger's `charging` switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// Write `0`.
    Stop(Hold),
    /// Write `1`: the hold is over.
    Resume(Hold),
    /// Write `0`, then `1`, and restart the PMIC's charger: plugged in,
    /// not charging at [`RESTART_PERCENT`] or below, and kindil holds
    /// nothing. The driver or the PMIC stopped by itself (full, or the
    /// charge timer); `0` clears the driver's full flag and timer count,
    /// `1` sets the charge current anew.
    Restart,
    /// Write `1`: unplugged while kindil held the charger off. The driver
    /// forgets "full" on an unplug but not a `0` written to the switch,
    /// which would keep its own end of charge off after the next plug-in.
    Release,
    /// Nothing to write: the switch was found off with no hold of kindil's
    /// (a previous kindil's, or powerd's): held as if full.
    Adopted,
}

/// How a [`Change`] is carried out: the `charging` switch's writes, in
/// order, then whether the PMIC's charger state machine is restarted
/// (src/pmic.rs). Setting the charge current alone restarts nothing once
/// the PMIC has stopped at the end of a charge or at its charge timer:
/// every way back to charging restarts it, as the driver does on a
/// plug-in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Writes {
    pub switch: &'static [&'static str],
    pub restart: bool,
}

impl Change {
    pub fn writes(self) -> Writes {
        let (switch, restart): (&'static [&'static str], bool) = match self {
            Change::Stop(_) => (&["0"], false),
            Change::Resume(_) => (&["1"], true),
            Change::Restart => (&["0", "1"], true),
            // Unplugged: the next plug-in restarts it.
            Change::Release => (&["1"], false),
            Change::Adopted => (&[], false),
        };
        Writes { switch, restart }
    }
}

/// kindil's charging policy, one reading at a time: powerd's temperature
/// range ([`pause_charging`]) first, then the driver's end of charge and
/// restart ([`FULL_PERCENT`], [`RESTART_PERCENT`]). Pure, so every reading
/// can be played back in a test; src/kindle.rs writes the switch.
pub fn govern_charging(state: Charging, seen: Seen, now: i64) -> (Charging, Option<Change>) {
    if !seen.plugged {
        // The driver resets itself on the plug events; kindil starts over.
        return (Charging::default(), state.held.map(|_| Change::Release));
    }
    let mut next = state;
    if seen.charging() {
        // A restart worked, or wasn't needed.
        next.restarted = None;
    }
    let off = !seen.enabled;
    let mut adopted = false;
    match (next.held, off) {
        // On again under kindil's hold: replugged meanwhile, which sets the
        // charge current; the driver's own rule is off while a `0` written
        // to the switch stands, so kindil's applies.
        (Some(_), false) => next.held = None,
        (None, true) => {
            next.held = Some(Hold::Full);
            adopted = true;
        }
        _ => {}
    }
    let pause = pause_charging(seen.temperature_f, next.held == Some(Hold::Temperature));
    let change = match next.held {
        Some(Hold::Temperature) if pause => None,
        Some(Hold::Temperature) => {
            next.held = None;
            // A restart: the next try waits its time.
            next.restarted = Some(now);
            Some(Change::Resume(Hold::Temperature))
        }
        _ if pause => {
            next.held = Some(Hold::Temperature);
            Some(Change::Stop(Hold::Temperature))
        }
        Some(Hold::Full) if seen.percent <= RESTART_PERCENT => {
            next.held = None;
            // A restart, as Change::Restart's: a reading right after it
            // (2026-11-21 18:11:00.612 and .664) doesn't restart again.
            next.restarted = Some(now);
            Some(Change::Resume(Hold::Full))
        }
        Some(Hold::Full) => None,
        None if seen.percent >= FULL_PERCENT && (1..EOC_MILLIAMPS).contains(&seen.current) => {
            next.held = Some(Hold::Full);
            Some(Change::Stop(Hold::Full))
        }
        None if seen.percent <= RESTART_PERCENT
            && !seen.charging()
            && next.restarted.is_none_or(|at| {
                // A clock stepped back counts as time enough.
                now < at || now - at >= RESTART_RETRY.as_secs() as i64
            }) =>
        {
            next.restarted = Some(now);
            Some(Change::Restart)
        }
        None => None,
    };
    (next, change.or(adopted.then_some(Change::Adopted)))
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

/// Whether a suspend could have ended by its alarm (due at `due`, in
/// seconds since the epoch), judged by the time: the PW1's RTC doesn't read
/// its alarm back. Only a suspend with its alarm armed, and no press of the
/// power button within [`BUTTON_GRACE`], counts as the alarm's
/// ([`sleep`]): a press in the alarm's last seconds, or after the time of
/// an alarm that couldn't be armed, is the button's.
pub fn alarm_woke(due: i64, now: i64) -> bool {
    now >= due - 2
}

/// How long after a wake the power button's listener may take to count the
/// press that woke it (the kernel reports it as the device resumes): a wake
/// at its alarm's time waits this long before taking it for the alarm.
pub const BUTTON_GRACE: Duration = Duration::from_secs(2);

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
    /// Suspends until `wake` (src/suspend.rs); returns once awake, with
    /// whether a wake alarm was armed for it.
    fn suspend(&self, wake: crate::suspend::Wake) -> anyhow::Result<bool>;
    /// Seconds since the epoch.
    fn now(&self) -> i64;
    fn battery(&self) -> anyhow::Result<Battery>;
    fn wait(&self, time: Duration);
    /// A line for the flight recorder.
    fn note(&self, line: &str);
    /// Whether the app has woken meanwhile (its ticket is void,
    /// src/suspend.rs): the sleep is over, whatever woke it.
    fn cancelled(&self) -> bool;
}

/// How a [`sleep`] ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slept {
    /// Awake for the app: the power button, a charger, or, for a sleep the
    /// user asked for, any wake but its own battery check.
    Woken,
    /// Never suspended: a sleep for the empty battery with a charger in.
    Charging,
    /// The battery is at [`CRITICAL_PERCENT`], or its sleep couldn't arm the
    /// check that would catch that: power off.
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
        // A nap's alarm must not wake a sleep the user asked for; a check of
        // its own every few hours does, on a charger too: one unplugged
        // while the Kindle is suspended may not wake it, and on battery
        // only the check powers it off at 1%. Only a sleep the user asked
        // for may go without it: the battery's must check.
        let wake = match (plugged, Asked::of(why)) {
            (_, Some(asked)) => Wake::AlarmOrButton(CHECK_ASLEEP, asked),
            (false, None) => Wake::Alarm(CHECK_ASLEEP),
            (true, None) => {
                device.note("sleep: a charger is connected; no sleep for the battery");
                return Ok(Slept::Charging);
            }
        };
        let due = wake
            .alarm()
            .map(|after| device.now() + after.as_secs() as i64);
        let presses = device.presses();
        let armed = match device.suspend(wake) {
            // A press handled while Wi-Fi parked (or since): the app is up.
            Ok(_) if device.cancelled() => {
                device.note("sleep: the app woke meanwhile");
                return Ok(Slept::Woken);
            }
            Ok(armed) => armed,
            Err(error) if why == Why::BatteryEmpty => {
                // Without its check alarm, nothing would power the Kindle off
                // at 1% and the cell would drain flat: off now, as powerd
                // would at a critical battery; unless a charger is on its way.
                if charger_comes(device) {
                    device.note("sleep: can't sleep, but a charger is connected");
                    return Ok(Slept::Charging);
                }
                device.note(&format!(
                    "sleep: can't sleep with the battery check ({error:#}); powering off"
                ));
                return Ok(Slept::Critical);
            }
            Err(error) => return Err(error),
        };
        // The alarm's only if one was armed, it is its time, and the power
        // button wasn't pressed: the time alone took a press in the alarm's
        // last seconds, or after an alarm that couldn't be armed, for it.
        if armed
            && due.is_some_and(|due| alarm_woke(due, device.now()))
            && !pressed_within(device, presses, BUTTON_GRACE)
        {
            let battery = device.battery()?;
            if critical(battery) {
                // A charger plugged in as the alarm fired isn't connected
                // yet: the gauge read "discharging".
                if charger_comes(device) {
                    device.note("sleep: battery critical, but a charger is connected");
                    continue;
                }
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

/// Whether the power button is pressed since `presses` was read, within
/// `time`.
fn pressed_within(device: &impl Sleeper, presses: u64, time: Duration) -> bool {
    let polls = time.as_millis() / CHARGER_POLL.as_millis();
    for poll in 0..=polls {
        if device.presses() != presses {
            return true;
        }
        if poll < polls {
            device.wait(CHARGER_POLL);
        }
    }
    false
}

/// Whether a charger is connected within [`CHARGER_DETECT`]: asked before
/// powering off, which a charger plugged in moments ago makes needless.
fn charger_comes(device: &impl Sleeper) -> bool {
    let polls = CHARGER_DETECT.as_millis() / CHARGER_POLL.as_millis();
    for poll in 0..=polls {
        if device.plugged() {
            return true;
        }
        if poll < polls {
            device.wait(CHARGER_POLL);
        }
    }
    false
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

    /// A charger in, the switch on, 77 °F.
    fn seen(percent: u8, current: i32) -> Seen {
        Seen {
            plugged: true,
            enabled: true,
            percent,
            current,
            temperature_f: 77,
            running: None,
        }
    }

    /// Plays `readings` through the policy from `state`, with the switch as
    /// the driver would show it after each write (and `seen`'s otherwise);
    /// returns the changes and the state.
    fn play(state: Charging, readings: &[(i64, Seen)]) -> (Vec<Option<Change>>, Charging) {
        let mut state = state;
        let mut switch: Option<bool> = None;
        let mut changes = Vec::new();
        for &(now, reading) in readings {
            let reading = Seen {
                enabled: switch.unwrap_or(reading.enabled) && reading.plugged,
                ..reading
            };
            let (next, change) = govern_charging(state, reading, now);
            switch = match change {
                Some(Change::Stop(_)) => Some(false),
                Some(Change::Resume(_) | Change::Restart | Change::Release) => Some(true),
                Some(Change::Adopted) | None => switch,
            };
            if !reading.plugged {
                switch = None;
            }
            state = next;
            changes.push(change);
        }
        (changes, state)
    }

    #[test]
    fn charging_stops_at_full_and_restarts_at_95_percent() {
        let (changes, state) = play(
            Charging::default(),
            &[
                (0, seen(99, 120)),
                (300, seen(100, 60)),
                (600, seen(100, 40)),
                // Stopped: the device runs on the cell.
                (900, seen(100, -30)),
                (1200, seen(96, -30)),
                (1500, seen(95, -30)),
                (1800, seen(96, 300)),
            ],
        );
        assert_eq!(
            changes,
            [
                None,
                None,
                Some(Change::Stop(Hold::Full)),
                None,
                None,
                Some(Change::Resume(Hold::Full)),
                None,
            ]
        );
        assert_eq!(state, Charging::default());
    }

    #[test]
    fn full_is_the_drivers_rule_a_current_under_55_ma_and_charging() {
        let state = Charging::default();
        let at = |percent, current| govern_charging(state, seen(percent, current), 0).1;
        assert_eq!(at(100, 54), Some(Change::Stop(Hold::Full)));
        assert_eq!(at(100, 1), Some(Change::Stop(Hold::Full)));
        assert_eq!(at(100, 55), None, "still charging hard");
        assert_eq!(at(100, 0), None, "not charging: the driver's `> 0`");
        assert_eq!(at(100, -20), None);
        assert_eq!(at(99, 20), None);
    }

    /// 2026-11-19, 05:04 UTC: on the charger all night, the chip at 90% and
    /// discharging, `connected` 1: the driver had stopped at full and its
    /// work, cancelled by every nap, never restarted it.
    #[test]
    fn a_charger_the_driver_stopped_is_restarted_at_95_percent() {
        let (changes, state) = play(
            Charging::default(),
            &[
                (0, seen(97, -25)),
                (300, seen(90, -25)),
                // Not charging yet: tried again only after RESTART_RETRY.
                (600, seen(90, -25)),
                (2100, seen(89, -25)),
                (2400, seen(89, 400)),
            ],
        );
        assert_eq!(
            changes,
            [
                None,
                Some(Change::Restart),
                None,
                Some(Change::Restart),
                None
            ]
        );
        assert_eq!(state, Charging::default(), "charging: nothing to retry");
        // A clock stepped back doesn't hold the next try off.
        let (_, change) = govern_charging(
            Charging {
                held: None,
                restarted: Some(5_000),
            },
            seen(90, -25),
            4_000,
        );
        assert_eq!(change, Some(Change::Restart));
    }

    #[test]
    fn the_temperature_pause_wins_over_full_and_restart() {
        let hot = |percent, current| Seen {
            temperature_f: CHARGE_MAX_F + 1,
            ..seen(percent, current)
        };
        let warm = |percent, current| Seen {
            temperature_f: CHARGE_MAX_F - 1,
            ..seen(percent, current)
        };
        let (changes, _) = play(
            Charging::default(),
            &[
                (0, seen(100, 30)),
                // Held for full, then too hot: held for the temperature.
                (300, hot(100, -30)),
                // Below 95%, but not cool enough yet.
                (600, warm(90, -30)),
                (900, seen(90, -30)),
            ],
        );
        assert_eq!(
            changes,
            [
                Some(Change::Stop(Hold::Full)),
                Some(Change::Stop(Hold::Temperature)),
                None,
                Some(Change::Resume(Hold::Temperature)),
            ]
        );
        // Too hot at 90% on a stopped charger: no restart, a pause.
        let (_, change) = govern_charging(Charging::default(), hot(90, -30), 0);
        assert_eq!(change, Some(Change::Stop(Hold::Temperature)));
    }

    #[test]
    fn an_unplug_gives_the_switch_back_and_forgets() {
        let unplugged = Seen {
            plugged: false,
            enabled: false,
            ..seen(100, -30)
        };
        let (changes, state) = play(
            Charging::default(),
            &[(0, seen(100, 30)), (300, unplugged), (600, unplugged)],
        );
        assert_eq!(
            changes,
            [Some(Change::Stop(Hold::Full)), Some(Change::Release), None]
        );
        assert_eq!(state, Charging::default());
        // Holding nothing, an unplug writes nothing.
        assert_eq!(govern_charging(Charging::default(), unplugged, 0).1, None);
    }

    /// Unplugged and plugged in again between two readings (5 minutes
    /// apart in a doze): the driver's plug-in sets the charge current, so
    /// the switch reads 1 under kindil's hold, and kindil's rule goes on.
    #[test]
    fn a_replug_between_readings_ends_the_hold() {
        let held = Charging {
            held: Some(Hold::Full),
            restarted: None,
        };
        assert_eq!(
            govern_charging(held, seen(98, 300), 0),
            (Charging::default(), None)
        );
        // At full again: stopped again.
        assert_eq!(
            govern_charging(held, seen(100, 40), 0).1,
            Some(Change::Stop(Hold::Full))
        );
    }

    /// The switch found off with no hold of kindil's: a kindil that crashed
    /// while holding it, or powerd before the launcher stopped it.
    #[test]
    fn a_switch_found_off_is_held_as_full_until_95_percent() {
        let off = |percent| Seen {
            enabled: false,
            ..seen(percent, -30)
        };
        let (state, change) = govern_charging(Charging::default(), off(99), 0);
        assert_eq!(change, Some(Change::Adopted));
        assert_eq!(state.held, Some(Hold::Full));
        assert_eq!(
            govern_charging(Charging::default(), off(80), 0).1,
            Some(Change::Resume(Hold::Full))
        );
        let cold = Seen {
            temperature_f: CHARGE_MIN_F - 1,
            ..off(80)
        };
        assert_eq!(
            govern_charging(Charging::default(), cold, 0).1,
            Some(Change::Stop(Hold::Temperature))
        );
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
        /// The charger goes from then on.
        unplugged: Option<u64>,
        presses: Vec<u64>,
        percent: u8,
        /// Whether its RTC arms the alarms asked for.
        arms: bool,
        /// The app wakes (voids the sleep's ticket) from then on.
        cancelled_at: Option<u64>,
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
                unplugged: None,
                presses: presses.to_vec(),
                percent: 4,
                arms: true,
                cancelled_at: None,
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
                && self.unplugged.is_none_or(|at| self.clock.get() < at)
        }
        fn presses(&self) -> u64 {
            self.presses
                .iter()
                .filter(|&&at| at <= self.clock.get())
                .count() as u64
        }
        fn suspend(&self, wake: crate::suspend::Wake) -> anyhow::Result<bool> {
            self.alarms.borrow_mut().push(wake.alarm());
            self.wakes_asked.borrow_mut().push(wake);
            let armed = match wake {
                crate::suspend::Wake::Alarm(_) if !self.arms => {
                    anyhow::bail!("the alarm doesn't arm")
                }
                _ => self.arms && wake.alarm().is_some(),
            };
            let wake = self
                .wakes
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("asleep with nothing left to wake it"))?;
            self.clock.set(wake);
            Ok(armed)
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
        fn cancelled(&self) -> bool {
            self.cancelled_at.is_some_and(|at| self.clock.get() >= at)
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
        // Each check finds 2% and sleeps on, until the fake can't suspend
        // any more: then, with no check to come, it powers off.
        assert_eq!(sleep(&device, Why::BatteryEmpty).unwrap(), Slept::Critical);
        assert_eq!(device.suspends(), 3);
        assert_eq!(device.notes.borrow().len(), 1, "no wake but the checks");
        assert!(device.notes.borrow()[0].contains("powering off"));
        device = Device::new(&[six_hours], None, &[]);
        device.percent = 1;
        assert_eq!(sleep(&device, Why::BatteryEmpty).unwrap(), Slept::Critical);
    }

    /// verify/SleepPowerOff.cfg's counterexample: at 1%, the check alarm
    /// fires and a charger is plugged in as it does, before its driver
    /// says it is connected. 0.15.0 powered off on the charger.
    #[test]
    fn a_charger_plugged_in_as_the_check_finds_one_percent_keeps_it_on() {
        let six_hours = CHECK_ASLEEP.as_millis() as u64;
        let mut device = Device::new(&[six_hours], Some(six_hours + 2_500), &[]);
        device.percent = 1;
        assert_eq!(sleep(&device, Why::BatteryEmpty).unwrap(), Slept::Charging);
        assert_eq!(
            *device.notes.borrow(),
            [
                "sleep: battery critical, but a charger is connected",
                "sleep: a charger is connected; no sleep for the battery"
            ]
        );
        // And when its suspend fails, the same charger keeps it on.
        let device = Device::new(&[], Some(2_500), &[]);
        assert_eq!(sleep(&device, Why::BatteryEmpty).unwrap(), Slept::Charging);
    }

    #[test]
    fn a_battery_sleep_that_cant_arm_its_check_powers_off_but_an_asked_one_reports_it() {
        // No wake scripted: the fake's suspend fails, as an alarm that won't
        // arm makes the real one fail.
        let device = Device::new(&[], None, &[]);
        assert_eq!(sleep(&device, Why::BatteryEmpty).unwrap(), Slept::Critical);
        assert!(device.notes.borrow()[0].contains("powering off"));
        let device = Device::new(&[], None, &[]);
        assert!(sleep(&device, Why::Asked).is_err());
    }

    #[test]
    fn a_sleep_the_user_asked_for_ends_at_any_wake_but_its_check() {
        let device = Device::new(&[45_000], None, &[]);
        assert_eq!(sleep(&device, Why::Asked).unwrap(), Slept::Woken);
        assert_eq!(device.suspends(), 1);
        // Plugged in, it keeps its check (see below).
        let device = Device::new(&[45_000], Some(0), &[]);
        assert_eq!(sleep(&device, Why::Asked).unwrap(), Slept::Woken);
        assert_eq!(*device.alarms.borrow(), [Some(CHECK_ASLEEP)]);
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

    /// verify/SleepRaces.cfg's counterexample, for the sleep: the battery's
    /// sleep woken by its check, then a press handled by the app before the
    /// sleep suspends again (its ticket void): no second suspend.
    #[test]
    fn a_sleep_the_app_has_left_doesnt_suspend_again() {
        let six_hours = CHECK_ASLEEP.as_millis() as u64;
        let mut device = Device::new(&[six_hours], None, &[]);
        device.cancelled_at = Some(six_hours);
        assert_eq!(sleep(&device, Why::BatteryEmpty).unwrap(), Slept::Woken);
        assert_eq!(device.suspends(), 1);
        assert_eq!(*device.notes.borrow(), ["sleep: the app woke meanwhile"]);
    }

    /// verify/SleepCableQuiet.cfg's counterexample, if unplugging doesn't
    /// wake a suspended PW1 (not measured yet): a sleep the user asked for
    /// on a charger, unplugged while suspended. 0.15.0 slept with no alarm
    /// then, and the 1% power-off never came.
    #[test]
    fn a_sleep_on_a_charger_keeps_its_check_for_when_it_is_unplugged() {
        let six_hours = CHECK_ASLEEP.as_millis() as u64;
        let mut device = Device::new(&[six_hours], Some(0), &[]);
        device.unplugged = Some(60_000);
        device.percent = 1;
        assert_eq!(sleep(&device, Why::Asked).unwrap(), Slept::Critical);
        assert_eq!(device.suspends(), 1);
    }

    /// verify/SleepNearAlarm.cfg's counterexample: the power button pressed
    /// a second before the 6-hourly check was due. 0.15.0 took it for the
    /// alarm by the time, and slept again.
    #[test]
    fn a_press_just_before_the_check_alarm_wakes_the_app() {
        let six_hours = CHECK_ASLEEP.as_millis() as u64;
        let device = Device::new(&[six_hours - 1_000], None, &[six_hours - 1_000]);
        assert_eq!(sleep(&device, Why::Asked).unwrap(), Slept::Woken);
        assert_eq!(device.suspends(), 1);
        // Counted by the listener a moment after the resume: still the button.
        let device = Device::new(&[six_hours], None, &[six_hours + 800]);
        assert_eq!(sleep(&device, Why::Asked).unwrap(), Slept::Woken);
        assert_eq!(device.suspends(), 1);
        // The alarm itself still checks the battery and sleeps on.
        let device = Device::new(
            &[six_hours, six_hours + 60_000],
            None,
            &[six_hours + 60_000],
        );
        assert_eq!(sleep(&device, Why::Asked).unwrap(), Slept::Woken);
        assert_eq!(device.suspends(), 2);
    }

    /// verify/SleepAlarmFails.cfg's counterexample: a sleep the user asked
    /// for whose check alarm couldn't be armed sleeps until the button; a
    /// press after the alarm's time was taken for it.
    #[test]
    fn without_its_alarm_armed_a_press_after_its_time_wakes_the_app() {
        let mut device = Device::new(&[7 * 3_600_000], None, &[7 * 3_600_000]);
        device.arms = false;
        assert_eq!(sleep(&device, Why::Asked).unwrap(), Slept::Woken);
        assert_eq!(device.suspends(), 1);
    }

    #[test]
    fn the_alarm_is_told_from_the_power_button_by_the_time() {
        assert!(alarm_woke(1000, 1000));
        assert!(alarm_woke(1000, 999), "a second's slack");
        assert!(alarm_woke(1000, 1500));
        assert!(!alarm_woke(1000, 400), "pressed long before the alarm");
    }

    /// The PMIC's charger as the datasheet has it: charging while its state
    /// machine runs and the charge current is set; the state machine stops
    /// by itself (here it had, at the end of a charge) and starts again
    /// only on a CHGRESTART, or once the cell sags under BATTCYCL (92% on
    /// the gauge, on 2026-11-21).
    struct Pmic {
        running: bool,
        current_on: bool,
        percent: u8,
    }

    impl Pmic {
        fn seen(&self, pmic_readable: bool) -> Seen {
            let charging = self.running && self.current_on;
            Seen {
                // The gauge as it read on the charger: +0 or -7 mA when not
                // charging, a charge current when it is.
                current: if charging { 150 } else { -7 },
                running: pmic_readable.then_some(self.running),
                ..seen(self.percent, 0)
            }
        }

        fn apply(&mut self, writes: Writes, carried_out: bool) {
            for value in writes.switch {
                self.current_on = *value == "1";
            }
            if writes.restart && carried_out {
                self.running = true;
            }
        }

        /// Five minutes on: a percent more charged, or the sag every
        /// half hour or so.
        fn later(&mut self, minutes: i64) {
            if self.running && self.current_on {
                self.percent = (self.percent + 2).min(100);
            } else if minutes % 30 == 0 {
                self.percent -= 1;
            }
            if self.percent <= 92 {
                // BATTCYCL: the PMIC restarts by itself.
                self.running = true;
            }
        }
    }

    /// Replays 2026-11-21 from 20:50 UTC: stopped at full at 18:55, and the
    /// PMIC's state machine ended. `restarts`: whether kindil's restart of
    /// the state machine is carried out (0.18-0.20 had none). Returns the
    /// changes, by minute from 20:50, and the minute it charged again.
    fn replay(restarts: bool, pmic_readable: bool) -> (Vec<(i64, Change)>, Option<i64>) {
        let mut pmic = Pmic {
            running: false,
            current_on: false,
            percent: 95,
        };
        let mut state = Charging {
            held: Some(Hold::Full),
            restarted: None,
        };
        let mut changes = Vec::new();
        let mut charging_at = None;
        for minute in (0..=120).step_by(5) {
            let seen = Seen {
                enabled: pmic.current_on,
                ..pmic.seen(pmic_readable)
            };
            if seen.current > 0 && charging_at.is_none() {
                charging_at = Some(minute);
            }
            let (next, change) = govern_charging(state, seen, minute * 60);
            state = next;
            if let Some(change) = change {
                pmic.apply(change.writes(), restarts);
                changes.push((minute, change));
            }
            pmic.later(minute + 5);
        }
        (changes, charging_at)
    }

    #[test]
    fn setting_the_current_alone_left_the_cell_discharging_on_its_charger() {
        // As kindil.log and the kernel's gauge lines had it: restarted at
        // 95%, then "stopped on a charger" every 30 minutes, and charging
        // only once the PMIC restarted by itself at 92%, 10 to 25 minutes
        // after a write, never on the reading after one.
        let (changes, charging_at) = replay(false, false);
        assert_eq!(
            changes,
            [
                (0, Change::Resume(Hold::Full)),
                (30, Change::Restart),
                (60, Change::Restart),
            ]
        );
        assert!(charging_at.is_some_and(|at| at >= 60), "{charging_at:?}");
    }

    #[test]
    fn restarting_the_pmics_charger_charges_at_95_percent() {
        for pmic_readable in [true, false] {
            let (changes, charging_at) = replay(true, pmic_readable);
            assert_eq!(changes, [(0, Change::Resume(Hold::Full))]);
            assert_eq!(charging_at, Some(5), "the next reading charges");
        }
    }

    #[test]
    fn the_pmics_word_wins_over_the_gauges_sign() {
        // Charging under a wake's draw: the gauge reads -7 mA.
        let under_load = Seen {
            running: Some(true),
            ..seen(94, -7)
        };
        assert_eq!(govern_charging(Charging::default(), under_load, 0).1, None);
        // Stopped, though the gauge's average still shows the last charge.
        let ended = Seen {
            running: Some(false),
            ..seen(94, 12)
        };
        assert_eq!(
            govern_charging(Charging::default(), ended, 0).1,
            Some(Change::Restart)
        );
    }

    #[test]
    fn every_way_back_to_charging_restarts_the_pmic() {
        assert!(Change::Resume(Hold::Full).writes().restart);
        assert!(Change::Resume(Hold::Temperature).writes().restart);
        assert_eq!(
            Change::Restart.writes(),
            Writes {
                switch: &["0", "1"],
                restart: true
            }
        );
        assert!(!Change::Stop(Hold::Full).writes().restart);
        assert!(!Change::Release.writes().restart, "unplugged");
        assert!(!Change::Adopted.writes().restart);
    }
}
