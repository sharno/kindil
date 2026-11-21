// Salah
//
// See LICENSE for more details.
// Copyright (c) 2019-2022 Farhan Ahmed. All rights reserved.
//

use chrono::Weekday;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Names of all obligatory prayers,
/// sunrise, and Qiyam.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(PartialEq, Eq, Debug, Copy, Clone)]
#[cfg_attr(kani, derive(kani::Arbitrary))]
pub enum Prayer {
    Fajr,
    Sunrise,
    Dhuhr,
    Asr,
    Maghrib,
    Isha,
    /// The start of the last third of the night.
    Qiyam,
    /// The next day's Fajr.
    FajrTomorrow,
}

impl Prayer {
    /// Every time a day's [`PrayerTimes`](crate::PrayerTimes) has, in the
    /// order they come.
    pub const IN_ORDER: [Prayer; 8] = [
        Prayer::Fajr,
        Prayer::Sunrise,
        Prayer::Dhuhr,
        Prayer::Asr,
        Prayer::Maghrib,
        Prayer::Isha,
        Prayer::Qiyam,
        Prayer::FajrTomorrow,
    ];

    /// The name, transliterated into English.
    pub fn name(&self) -> &'static str {
        match self {
            Prayer::Fajr | Prayer::FajrTomorrow => "Fajr",
            Prayer::Sunrise => "Sunrise",
            Prayer::Dhuhr => "Dhuhr",
            Prayer::Asr => "Asr",
            Prayer::Maghrib => "Maghrib",
            Prayer::Isha => "Isha",
            Prayer::Qiyam => "Qiyam",
        }
    }

    /// The name on a day that is `weekday` where the prayer is: Dhuhr is
    /// Jumua on Fridays.
    pub fn name_on(&self, weekday: Weekday) -> &'static str {
        match (self, weekday) {
            (Prayer::Dhuhr, Weekday::Fri) => "Jumua",
            _ => self.name(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prayer_names() {
        // salah 0.7 named Dhuhr Jumua when the computer's clock read a
        // Friday in UTC, whatever the day and place of the prayer.
        assert_eq!(Prayer::Fajr.name(), "Fajr");
        assert_eq!(Prayer::FajrTomorrow.name(), "Fajr");
        assert_eq!(Prayer::Dhuhr.name(), "Dhuhr");
        assert_eq!(Prayer::Dhuhr.name_on(Weekday::Fri), "Jumua");
        assert_eq!(Prayer::Dhuhr.name_on(Weekday::Thu), "Dhuhr");
        assert_eq!(Prayer::Asr.name_on(Weekday::Fri), "Asr");
        assert_eq!(Prayer::Qiyam.name(), "Qiyam");
    }
}
