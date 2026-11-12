//! `kindil.toml`, decoded once into types that cannot hold invalid values.
//! Dashboard domain settings (location, prayer method, units) stay in Miqat's file.
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Where the launcher keeps everything on the Kindle's user partition.
pub const DEVICE_CONFIG: &str = "/mnt/us/kindil/kindil.toml";

#[derive(Clone, Copy, Debug, Default, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Dashboard {
    #[default]
    Miqat,
    Clock,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The dashboard shown until the user picks another one (then `prefs` wins).
    #[serde(default)]
    pub dashboard: Dashboard,
    #[serde(default = "defaults::miqat_config")]
    pub miqat_config: PathBuf,
    #[serde(default = "defaults::books")]
    pub books: PathBuf,
    /// Runtime choices (dashboard, font size, reading positions) survive restarts here.
    #[serde(default = "defaults::prefs")]
    pub prefs: PathBuf,
    /// IANA name, e.g. "America/New_York": the clock dashboard and status bar.
    #[serde(default = "defaults::timezone")]
    pub timezone: chrono_tz::Tz,
    /// Absent: the Settings screen shows sync as not installed.
    #[serde(default)]
    pub syncthing: Option<Syncthing>,
}

/// A user-supplied Syncthing build. kindil starts it only when switched on.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Syncthing {
    pub binary: PathBuf,
    pub home: PathBuf,
}

mod defaults {
    use std::path::PathBuf;
    pub fn miqat_config() -> PathBuf {
        "/mnt/us/kindil/miqat.toml".into()
    }
    pub fn books() -> PathBuf {
        "/mnt/us/books".into()
    }
    pub fn prefs() -> PathBuf {
        "/mnt/us/kindil/prefs.toml".into()
    }
    pub fn timezone() -> chrono_tz::Tz {
        chrono_tz::UTC
    }
}

impl Config {
    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// An explicit path must exist; the device default may be absent (all defaults).
    pub fn load(explicit: Option<&Path>) -> anyhow::Result<Self> {
        use anyhow::Context;
        let read = |path: &Path| -> anyhow::Result<Self> {
            let text =
                std::fs::read_to_string(path).with_context(|| format!("{}", path.display()))?;
            Self::parse(&text).with_context(|| format!("{}", path.display()))
        };
        match explicit {
            Some(path) => read(path),
            None if Path::new(DEVICE_CONFIG).is_file() => read(Path::new(DEVICE_CONFIG)),
            None => Ok(Self::parse("")?),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_and_decoding() {
        let c = Config::parse("").unwrap();
        assert_eq!(c.dashboard, Dashboard::Miqat);
        assert_eq!(c.timezone, chrono_tz::UTC);
        assert_eq!(c.syncthing, None);
        let c = Config::parse(
            "timezone = 'America/New_York'\n[syncthing]\nbinary = '/a'\nhome = '/b'\n",
        )
        .unwrap();
        assert_eq!(c.timezone, chrono_tz::America::New_York);
        assert_eq!(c.syncthing.unwrap().binary, PathBuf::from("/a"));
        assert!(Config::parse("timezone = 'bogus'").is_err());
        assert!(Config::parse("unexpected = 1").is_err());
        // PID-1 era keys are rejected rather than silently ignored.
        assert!(Config::parse("ssh = true").is_err());
    }
}
