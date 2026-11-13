//! The PW1 front light through its driver's sysfs file, which takes and
//! reports a raw intensity from 0 (off) to 255. kindil steps through the 25
//! levels Amazon's powerd uses, so a level looks the same either way, and
//! needs no powerd for it.
use crate::host::Brightness;
use std::path::{Path, PathBuf};

const INTENSITY: &str = "/sys/devices/system/fl_tps6116x/fl_tps6116x0/fl_intensity";
/// powerd's levels 0..=24 as raw intensities, measured on a PW1 (5.6.1.1).
/// Its lowest level is dim, not off.
const LEVELS: [u8; 25] = [
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 13, 19, 26, 36, 47, 60, 75, 90, 107, 126, 147, 170, 196, 224,
    255,
];
const TOP: i32 = LEVELS.len() as i32 - 1;

pub struct Frontlight {
    path: PathBuf,
}

impl Frontlight {
    pub fn open() -> anyhow::Result<Self> {
        Self::at(Path::new(INTENSITY))
    }

    pub fn at(path: &Path) -> anyhow::Result<Self> {
        anyhow::ensure!(path.exists(), "no front light at {}", path.display());
        Ok(Self {
            path: path.to_owned(),
        })
    }

    pub fn brightness(&self) -> anyhow::Result<Brightness> {
        let text = std::fs::read_to_string(&self.path)?;
        let raw = parse_intensity(&text)
            .ok_or_else(|| anyhow::anyhow!("unexpected front light reading {text:?}"))?;
        Ok(Brightness::new(to_percent(level_of(raw))))
    }

    pub fn set(&self, brightness: Brightness) -> anyhow::Result<()> {
        let raw = LEVELS[to_level(brightness.percent()) as usize];
        Ok(std::fs::write(&self.path, raw.to_string())?)
    }
}

/// The driver reports `FrontLight(Intensity) = 13`.
fn parse_intensity(text: &str) -> Option<u8> {
    text.rsplit('=').next()?.trim().parse().ok()
}

/// The highest level at or below `raw`; off counts as the lowest.
fn level_of(raw: u8) -> i32 {
    LEVELS.iter().rposition(|&level| level <= raw).unwrap_or(0) as i32
}

fn to_level(percent: u8) -> i32 {
    (i32::from(percent.min(100)) * TOP + 50) / 100
}

fn to_percent(level: i32) -> u8 {
    ((level.clamp(0, TOP) * 100 + TOP / 2) / TOP) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_stay_within_powerds_range() {
        assert_eq!(to_level(0), 0);
        assert_eq!(to_level(100), 24);
        assert_eq!(to_level(250), 24);
        assert_eq!(to_percent(24), 100);
        assert_eq!(to_percent(12), 50);
        assert_eq!(to_percent(99), 100);
    }

    #[test]
    fn reads_the_drivers_intensity() {
        assert_eq!(parse_intensity("FrontLight(Intensity) = 13\n"), Some(13));
        assert_eq!(parse_intensity("255"), Some(255));
        assert_eq!(parse_intensity("FrontLight(Intensity) = \n"), None);
        assert_eq!(level_of(13), 10);
        assert_eq!(level_of(0), 0, "off reads as the lowest level");
        assert_eq!(level_of(50), 14, "between levels rounds down");
    }

    #[test]
    fn a_level_set_reads_back_the_same() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fl_intensity");
        std::fs::write(&path, "FrontLight(Intensity) = 13\n").unwrap();
        let light = Frontlight::at(&path).unwrap();
        assert_eq!(light.brightness().unwrap(), Brightness::new(42));
        for percent in [0, 4, 40, 42, 75, 100] {
            light.set(Brightness::new(percent)).unwrap();
            let back = light.brightness().unwrap().percent();
            assert_eq!(back, to_percent(to_level(percent)), "{percent}%");
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "255");
        assert!(Frontlight::at(&dir.path().join("missing")).is_err());
    }
}
