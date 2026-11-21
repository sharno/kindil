// Salah
//
// See LICENSE for more details.
// Copyright (c) 2019-2022 Farhan Ahmed. All rights reserved.
//

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Time adjustment for all prayer times.
/// The value is specified in *minutes* and
/// can be either positive or negative (up to about 22 days either way, so
/// adding one can't overflow).
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(PartialEq, Debug, Copy, Clone, Default)]
#[cfg_attr(kani, derive(kani::Arbitrary))]
pub struct TimeAdjustment {
    pub fajr: i16,
    pub sunrise: i16,
    pub dhuhr: i16,
    pub asr: i16,
    pub maghrib: i16,
    pub isha: i16,
}

impl TimeAdjustment {
    pub fn new(fajr: i16, sunrise: i16, dhuhr: i16, asr: i16, maghrib: i16, isha: i16) -> Self {
        TimeAdjustment {
            fajr,
            sunrise,
            dhuhr,
            asr,
            maghrib,
            isha,
        }
    }
}

/// Builder struct for the [TimeAdjustment](struct.TimeAdjustment.html).
/// It is recommended to use this for all needed adjustments.
pub struct Adjustment {
    fajr: i16,
    sunrise: i16,
    dhuhr: i16,
    asr: i16,
    maghrib: i16,
    isha: i16,
}

impl Adjustment {
    pub fn new() -> Adjustment {
        Adjustment {
            fajr: 0,
            sunrise: 0,
            dhuhr: 0,
            asr: 0,
            maghrib: 0,
            isha: 0,
        }
    }

    pub fn fajr(&mut self, fajr: i16) -> &mut Adjustment {
        self.fajr = fajr;
        self
    }

    pub fn sunrise(&mut self, sunrise: i16) -> &mut Adjustment {
        self.sunrise = sunrise;
        self
    }

    pub fn dhuhr(&mut self, dhuhr: i16) -> &mut Adjustment {
        self.dhuhr = dhuhr;
        self
    }

    pub fn asr(&mut self, asr: i16) -> &mut Adjustment {
        self.asr = asr;
        self
    }

    pub fn maghrib(&mut self, maghrib: i16) -> &mut Adjustment {
        self.maghrib = maghrib;
        self
    }

    pub fn isha(&mut self, isha: i16) -> &mut Adjustment {
        self.isha = isha;
        self
    }

    pub fn done(&self) -> TimeAdjustment {
        TimeAdjustment {
            fajr: self.fajr,
            sunrise: self.sunrise,
            dhuhr: self.dhuhr,
            asr: self.asr,
            maghrib: self.maghrib,
            isha: self.isha,
        }
    }
}

impl Default for Adjustment {
    fn default() -> Self {
        Self::new()
    }
}
