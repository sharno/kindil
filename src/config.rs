//! Device configuration. Dashboard domain settings remain owned by Miqat.
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Dashboard {
    #[default]
    Miqat,
    Clock,
}

// Deliberately no Debug: Wi-Fi credentials must not leak into diagnostics.
#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub dashboard: Dashboard,
    pub miqat_config: PathBuf,
    pub books: PathBuf,
    pub timezone: String,
    pub interface: String,
    pub wifi: bool,
    pub wifi_ssid: Option<String>,
    pub wifi_psk: Option<String>,
    pub ssh: bool,
    pub sync: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            dashboard: Dashboard::Miqat,
            miqat_config: "/mnt/us/miqat.toml".into(),
            books: "/mnt/us/books".into(),
            timezone: "UTC0".into(),
            interface: "wlan0".into(),
            wifi: true,
            wifi_ssid: None,
            wifi_psk: None,
            ssh: true,
            sync: false,
        }
    }
}
impl Config {
    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        if let Some(path) = std::env::var_os("KINDIL_CONFIG") {
            return Self::from_path(Path::new(&path));
        }
        for path in ["/etc/kindil.toml", "/mnt/us/kindil.toml"] {
            if Path::new(path).is_file() {
                return Self::from_path(Path::new(path));
            }
        }
        Ok(Self::default())
    }
    pub fn from_path(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        Self::parse(&std::fs::read_to_string(path)?)
    }
    pub fn parse(text: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let value: Self = toml::from_str(text)?;
        if value.interface.is_empty()
            || value.interface.len() > 15
            || !value
                .interface
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
        {
            return Err("invalid wireless interface".into());
        }
        if value
            .wifi_ssid
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 32 || s.contains(['\n', '\r', '\0']))
        {
            return Err("invalid SSID".into());
        }
        if value.wifi_psk.as_ref().is_some_and(|s| {
            !(8..=63).contains(&s.len()) || !s.is_ascii() || s.bytes().any(|b| b < 32 || b == 127)
        }) {
            return Err("invalid WPA2 passphrase".into());
        }
        if crate::tz::Tz::parse(&value.timezone).is_none() {
            return Err("invalid clock timezone".into());
        }
        Ok(value)
    }
    pub fn tz(&self) -> crate::tz::Tz {
        crate::tz::Tz::parse(&self.timezone).unwrap_or_else(crate::tz::Tz::utc)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn safe_defaults_and_explicit_validation() {
        let c = Config::parse("").unwrap();
        assert!(c.ssh);
        assert!(!c.sync);
        assert_eq!(c.dashboard, Dashboard::Miqat);
        assert!(Config::parse("timezone = 'bogus'").is_err());
        assert!(Config::parse("wifi_psk = 'short'").is_err());
        assert!(Config::parse("unexpected = 1").is_err());
        assert!(Config::parse("interface = '../etc'").is_err());
    }
}
