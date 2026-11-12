//! Battery state from sysfs.
//!
//! Which fuel gauge a Kindle has depends on the board, so the reader scans
//! `/sys/class/power_supply/*/capacity` and prefers an entry whose name
//! mentions the battery. A board without a gauge reports [`Battery::Unknown`].

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Charge {
    /// Plugged in: charging or already full.
    Charging,
    Discharging,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Battery {
    Unknown,
    Known { percent: u8, charge: Charge },
}

/// Read the battery from a sysfs root (`/sys` on the device).
pub fn read(root: &Path) -> Battery {
    let mut supplies: Vec<PathBuf> = std::fs::read_dir(root.join("class/power_supply"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect();
    supplies.sort();
    // Some boards also expose `ac` and `usb` supplies without a capacity.
    let named = |path: &&PathBuf| {
        path.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.contains("batt"))
    };
    let Some(supply) = supplies
        .iter()
        .find(named)
        .or_else(|| supplies.iter().find(|p| p.join("capacity").is_file()))
    else {
        return Battery::Unknown;
    };
    let read = |name: &str| std::fs::read_to_string(supply.join(name)).unwrap_or_default();
    match read("capacity").trim().parse::<u8>() {
        Ok(percent) => Battery::Known {
            percent: percent.min(100),
            charge: match read("status").trim().to_ascii_lowercase().as_str() {
                "charging" | "full" => Charge::Charging,
                _ => Charge::Discharging,
            },
        },
        Err(_) => Battery::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn supply(root: &Path, name: &str, capacity: &str, status: &str) {
        let dir = root.join("class/power_supply").join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("capacity"), capacity).unwrap();
        fs::write(dir.join("status"), status).unwrap();
    }

    #[test]
    fn reads_capacity_and_status() {
        let root = tempfile::tempdir().unwrap();
        supply(root.path(), "max17040_battery", "87\n", "Discharging\n");
        assert_eq!(
            read(root.path()),
            Battery::Known {
                percent: 87,
                charge: Charge::Discharging
            }
        );
    }

    #[test]
    fn prefers_the_battery_over_ac_and_clamps() {
        let root = tempfile::tempdir().unwrap();
        supply(root.path(), "ac", "10", "");
        supply(root.path(), "battery", "250", "Full");
        assert_eq!(
            read(root.path()),
            Battery::Known {
                percent: 100,
                charge: Charge::Charging
            }
        );
    }

    #[test]
    fn a_missing_gauge_is_unknown() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(read(root.path()), Battery::Unknown);
    }
}
