// Salah
//
// See LICENSE for more details.
// Copyright (c) 2019-2022 Farhan Ahmed. All rights reserved.
//
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(PartialEq, Debug, Copy, Clone)]
#[cfg_attr(kani, derive(kani::Arbitrary))]
/// How the times are rounded to the minute.
pub enum Rounding {
    /// To the nearest minute, half a minute up.
    Nearest,
    /// To the next minute; a time on a minute stays.
    Up,
    /// Not at all: to the second.
    None,
}
