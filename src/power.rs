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

    #[test]
    fn the_alarm_is_told_from_the_power_button_by_the_time() {
        assert!(alarm_woke(1000, 1000));
        assert!(alarm_woke(1000, 999), "a second's slack");
        assert!(alarm_woke(1000, 1500));
        assert!(!alarm_woke(1000, 400), "pressed long before the alarm");
    }
}
