//! How fast dozing empties the battery, from the fuel gauge's remaining charge
//! at the start and end of a stretch on battery.

use chrono::{DateTime, TimeDelta, Utc};

/// The fuel gauge's remaining charge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MilliampHours(pub u32);

/// A gauge reading on battery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gauge {
    pub at: DateTime<Utc>,
    pub left: MilliampHours,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Drain {
    pub milliamps: u32,
    pub over: TimeDelta,
    pub left: MilliampHours,
}

/// The gauge counts whole mAh and stalls while suspended, so a shorter
/// stretch says little.
pub const MIN_STRETCH: TimeDelta = TimeDelta::minutes(20);

impl Drain {
    /// None when the stretch is too short, or the charge went up (plugged in).
    pub fn between(from: Gauge, to: Gauge) -> Option<Drain> {
        let over = to.at - from.at;
        let used = from.left.0.checked_sub(to.left.0)?;
        (over >= MIN_STRETCH).then(|| Drain {
            milliamps: u32::try_from(i64::from(used) * 3600 / over.num_seconds())
                .unwrap_or(u32::MAX),
            over,
            left: to.left,
        })
    }

    /// Hours until empty at this rate; None while it uses nothing measurable.
    pub fn hours_left(self) -> Option<u32> {
        self.left.0.checked_div(self.milliamps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gauge(minutes: i64, mah: u32) -> Gauge {
        Gauge {
            at: DateTime::from_timestamp(1_794_398_400 + minutes * 60, 0).unwrap(),
            left: MilliampHours(mah),
        }
    }

    #[test]
    fn half_an_hour_of_doze() {
        let drain = Drain::between(gauge(0, 623), gauge(30, 600)).unwrap();
        assert_eq!(drain.milliamps, 46);
        assert_eq!(drain.hours_left(), Some(13));
    }

    #[test]
    fn short_or_charging_stretches_say_nothing() {
        assert_eq!(Drain::between(gauge(0, 623), gauge(10, 610)), None);
        assert_eq!(Drain::between(gauge(0, 600), gauge(30, 623)), None);
    }

    #[test]
    fn nothing_measurable_has_no_end() {
        let drain = Drain::between(gauge(0, 600), gauge(30, 600)).unwrap();
        assert_eq!((drain.milliamps, drain.hours_left()), (0, None));
    }
}
