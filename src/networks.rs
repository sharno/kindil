//! The Wi-Fi networks kindil joins by itself, saved as plain TOML.
//!
//! Amazon's wifid keeps its own list encrypted, readable by wifid alone; this
//! one lets kindil run Wi-Fi with no wifid at all. Most recently joined first,
//! which is also the order wpa_supplicant prefers them in.
use crate::wifi::{Credentials, Ssid};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// On the system storage, which stays when a computer takes /mnt/us.
pub const PATH: &str = "/var/local/kindil/networks.toml";

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default, rename = "network")]
    networks: Vec<Saved>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    ssid: String,
    /// Absent for an open network.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    password: Option<String>,
}

impl From<&Credentials> for Saved {
    fn from(credentials: &Credentials) -> Self {
        Self {
            ssid: credentials.ssid().as_str().to_owned(),
            password: credentials.password().map(str::to_owned),
        }
    }
}

pub struct Networks {
    path: PathBuf,
    list: Vec<Credentials>,
}

impl Networks {
    /// A missing file is no networks; entries that no longer parse are skipped.
    pub fn open(path: &Path) -> Self {
        let file: File = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default();
        let list = file
            .networks
            .into_iter()
            .filter_map(|saved| {
                Credentials::parse(saved.ssid, saved.password.unwrap_or_default()).ok()
            })
            .collect();
        Self {
            path: path.to_owned(),
            list,
        }
    }

    pub fn list(&self) -> &[Credentials] {
        &self.list
    }

    pub fn ssids(&self) -> Vec<String> {
        self.list
            .iter()
            .map(|c| c.ssid().as_str().to_owned())
            .collect()
    }

    /// Saves `credentials` first in the list, replacing any with its name.
    pub fn add(&mut self, credentials: Credentials) -> anyhow::Result<()> {
        self.list.retain(|c| c.ssid() != credentials.ssid());
        self.list.insert(0, credentials);
        self.save()
    }

    pub fn forget(&mut self, ssid: &Ssid) -> anyhow::Result<()> {
        self.list.retain(|c| c.ssid() != ssid);
        self.save()
    }

    /// Write-then-rename, readable by root only: it holds passwords.
    fn save(&self) -> anyhow::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let file = File {
            networks: self.list.iter().map(Saved::from).collect(),
        };
        let text = toml::to_string(&file)?;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let temp = self.path.with_extension("toml.tmp");
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)?
            .write_all(text.as_bytes())?;
        std::fs::rename(&temp, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wpa(ssid: &str, password: &str) -> Credentials {
        Credentials::parse(ssid.into(), password.into()).unwrap()
    }

    #[test]
    fn saves_most_recent_first_and_forgets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("networks.toml");
        let mut networks = Networks::open(&path);
        assert!(networks.list().is_empty());
        networks.add(wpa("Home", "correct horse")).unwrap();
        networks.add(wpa("Cafe", "")).unwrap();
        networks.add(wpa("Home", "new password")).unwrap();
        let reopened = Networks::open(&path);
        assert_eq!(reopened.ssids(), ["Home", "Cafe"]);
        assert!(reopened.list()[0] == wpa("Home", "new password"));
        assert!(reopened.list()[1] == wpa("Cafe", ""));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("Cafe\"\npassword"),
            "no password for an open network: {text}"
        );

        let mut networks = reopened;
        networks
            .forget(&Ssid::try_from("Home".to_string()).unwrap())
            .unwrap();
        assert_eq!(Networks::open(&path).ssids(), ["Cafe"]);
    }

    #[test]
    fn is_readable_by_root_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("networks.toml");
        Networks::open(&path)
            .add(wpa("Home", "correct horse"))
            .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn skips_entries_that_no_longer_parse() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("networks.toml");
        std::fs::write(
            &path,
            "[[network]]\nssid = \"Home\"\npassword = \"short\"\n[[network]]\nssid = \"Cafe\"\n",
        )
        .unwrap();
        assert_eq!(Networks::open(&path).ssids(), ["Cafe"]);
    }
}
