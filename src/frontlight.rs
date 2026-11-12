//! The PW1 front light through powerd's `flIntensity` (0..=`flMaxIntensity`).
//! powerd owns the hardware and its limits; kindil only asks for a level.
//! Firmware 5.6.1.1 has no `/dev/yoshime_fl`, so there is no driver to call.
use crate::host::Brightness;
use crate::lipc;

const POWERD: &str = "com.lab126.powerd";

/// powerd's level range, read once; the maximum is always positive.
pub struct Frontlight {
    max: i32,
}

impl Frontlight {
    pub fn open() -> anyhow::Result<Self> {
        let max: i32 = lipc::get(POWERD, "flMaxIntensity")?.parse()?;
        anyhow::ensure!(max > 0, "front light reports no range");
        Ok(Self { max })
    }

    pub fn brightness(&self) -> anyhow::Result<Brightness> {
        let level: i32 = lipc::get(POWERD, "flIntensity")?.parse()?;
        Ok(Brightness::new(to_percent(level, self.max)))
    }

    pub fn set(&self, brightness: Brightness) -> anyhow::Result<()> {
        let level = to_level(brightness.percent(), self.max);
        lipc::set(POWERD, "flIntensity", &level.to_string())
    }
}

fn to_level(percent: u8, max: i32) -> i32 {
    (i32::from(percent.min(100)) * max + 50) / 100
}

fn to_percent(level: i32, max: i32) -> u8 {
    ((level.clamp(0, max) * 100 + max / 2) / max) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn levels_stay_within_powerds_range() {
        assert_eq!(to_level(0, 24), 0);
        assert_eq!(to_level(100, 24), 24);
        assert_eq!(to_level(250, 24), 24);
        assert_eq!(to_percent(24, 24), 100);
        assert_eq!(to_percent(12, 24), 50);
        assert_eq!(to_percent(99, 24), 100);
    }
}
