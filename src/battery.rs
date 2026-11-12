//! Battery state from sysfs.
//!
//! Which fuel gauge a Kindle has depends on the board, so rather than naming
//! one driver the reader scans `/sys/class/power_supply/*/capacity` and prefers
//! an entry whose name mentions the battery. A device without a gauge simply
//! reports `None` and the dashboard omits it.

use std::path::Path;

/// The battery as the dashboard shows it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Battery {
    /// Charge in percent, if a gauge is present.
    pub percent: Option<u8>,
    /// True when the gauge says the battery is charging or full.
    pub charging: bool,
}

impl Battery {
    /// A short label for the status bar.
    pub fn label(&self) -> String {
        match self.percent {
            Some(percent) if self.charging => std::format!("{percent}% +"),
            Some(percent) => std::format!("{percent}%"),
            None => "battery ?".to_string(),
        }
    }
}

/// Read the battery from a sysfs root (`/sys` in production).
pub fn read(root: &Path) -> Battery {
    let Ok(entries) = std::fs::read_dir(root.join("class/power_supply")) else {
        return Battery::default();
    };
    let mut candidates: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    candidates.sort();

    // Prefer a supply whose directory name mentions the battery; some boards
    // also expose `ac` and `usb` supplies that have no capacity.
    let best = candidates
        .iter()
        .find(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.contains("batt"))
        })
        .or_else(|| {
            candidates
                .iter()
                .find(|path| path.join("capacity").is_file())
        });

    let Some(path) = best else {
        return Battery::default();
    };

    let percent = std::fs::read_to_string(path.join("capacity"))
        .ok()
        .and_then(|text| text.trim().parse::<u8>().ok())
        .map(|p| p.min(100));

    let status = std::fs::read_to_string(path.join("status"))
        .ok()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let charging = status == "charging" || status == "full";

    Battery { percent, charging }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tempdir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(std::format!("kindil-battery-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_capacity_and_status() {
        let root = tempdir("read");
        let supply = root.join("class/power_supply/max17040_battery");
        fs::create_dir_all(&supply).unwrap();
        fs::write(supply.join("capacity"), "87\n").unwrap();
        fs::write(supply.join("status"), "Discharging\n").unwrap();

        let battery = read(&root);
        assert_eq!(battery.percent, Some(87));
        assert!(!battery.charging);
        assert_eq!(battery.label(), "87%");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn prefers_the_battery_over_ac() {
        let root = tempdir("prefer");
        let ac = root.join("class/power_supply/ac");
        let battery = root.join("class/power_supply/battery");
        fs::create_dir_all(&ac).unwrap();
        fs::create_dir_all(&battery).unwrap();
        fs::write(ac.join("capacity"), "10").unwrap();
        fs::write(battery.join("capacity"), "55").unwrap();
        fs::write(battery.join("status"), "Charging").unwrap();

        let read_back = read(&root);
        assert_eq!(read_back.percent, Some(55));
        assert!(read_back.charging);
        assert_eq!(read_back.label(), "55% +");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_gauge_is_not_an_error() {
        let root = tempdir("missing");
        let battery = read(&root);
        assert_eq!(battery.percent, None);
        assert_eq!(battery.label(), "battery ?");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn capacity_is_clamped() {
        let root = tempdir("clamp");
        let supply = root.join("class/power_supply/battery");
        fs::create_dir_all(&supply).unwrap();
        fs::write(supply.join("capacity"), "250").unwrap();
        assert_eq!(read(&root).percent, Some(100));
        let _ = fs::remove_dir_all(&root);
    }
}
