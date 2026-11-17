//! Finding a Kindle plugged in over USB in drive mode, and what is on it.
//!
//! A Kindle in drive mode is a FAT volume with Amazon's own layout: a
//! `documents` folder beside a `system` folder, and on most firmware
//! `system/version.txt`. The volume carries no serial number, so the model
//! comes from the USB device's serial number when [`crate::usb`] can read it
//! (Lab126, vendor 0x1949).
use crate::model::{Firmware, Model};
use std::path::{Path, PathBuf};

/// A Kindle's USB storage and what the installer learned from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kindle {
    /// Where the storage is mounted: `/Volumes/Kindle`, `E:\`.
    pub volume: PathBuf,
    pub firmware: Option<Firmware>,
    /// The serial number, from USB, if read.
    pub serial: Option<String>,
    pub model: Model,
    pub prereqs: Prereqs,
    pub installed: Option<Installed>,
}

/// What the volume shows of the jailbreak and KUAL.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Prereqs {
    /// KUAL's files, e.g. `documents/KUAL.kual` (the booklet) or a
    /// `documents/KUAL-KDK-*.azw2` (the kindlet).
    pub kual: Vec<String>,
    /// Other signs of a jailbreak: MRPI, USBNetwork, a hotfix.
    pub jailbreak: Vec<String>,
    /// `extensions/`, where KUAL finds its extensions.
    pub extensions_folder: bool,
}

impl Prereqs {
    /// KUAL only runs on a jailbroken Kindle, so KUAL is proof of both.
    pub fn jailbroken(&self) -> bool {
        !self.kual.is_empty() || !self.jailbreak.is_empty()
    }
}

/// kindil already on the Kindle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    /// From the binary itself, when it says.
    pub version: Option<String>,
    /// `kindil/miqat.toml` and `kindil/kindil.toml`, which stay as they are.
    pub config: Vec<String>,
}

/// Whether `path` is a Kindle's USB storage.
pub fn looks_like_kindle(path: &Path) -> bool {
    path.join("system/version.txt").is_file()
        || (path.join("documents").is_dir() && path.join("system").is_dir())
}

/// Mounted volumes that could be a Kindle, for this OS.
pub fn candidates() -> Vec<PathBuf> {
    let mut found = Vec::new();
    #[cfg(target_os = "macos")]
    found.extend(children(Path::new("/Volumes")));
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // Desktop automounters: udisks2 under /media/$USER or
        // /run/media/$USER, older ones straight under /media.
        for root in ["/media", "/run/media"] {
            for dir in children(Path::new(root)) {
                found.push(dir.clone());
                found.extend(children(&dir));
            }
        }
        // And any FAT volume mounted by hand.
        if let Ok(mounts) = std::fs::read_to_string("/proc/mounts") {
            found.extend(fat_mounts(&mounts));
        }
    }
    #[cfg(windows)]
    found.extend(
        (b'A'..=b'Z')
            .map(|letter| PathBuf::from(format!("{}:\\", letter as char)))
            .filter(|drive| drive.exists()),
    );
    found.sort();
    found.dedup();
    found
}

#[allow(dead_code)]
fn children(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect()
}

/// FAT mount points in `/proc/mounts`, whose spaces are written `\040`.
#[allow(dead_code)]
fn fat_mounts(mounts: &str) -> Vec<PathBuf> {
    mounts
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let (_device, point, kind) = (fields.next()?, fields.next()?, fields.next()?);
            ["vfat", "msdos", "fat", "fuseblk", "exfat"]
                .contains(&kind)
                .then(|| PathBuf::from(point.replace("\\040", " ")))
        })
        .collect()
}

/// Every Kindle plugged in, its model from USB when that can be told.
pub fn scan() -> Vec<Kindle> {
    let volumes: Vec<PathBuf> = candidates()
        .into_iter()
        .filter(|p| looks_like_kindle(p))
        .collect();
    let serials = crate::usb::kindle_serials();
    volumes
        .iter()
        .map(|volume| inspect(volume, pair(volumes.len(), &serials)))
        .collect()
}

/// One volume and one Lab126 device: they are the same Kindle. With more,
/// which serial is which volume's can't be told.
fn pair(volumes: usize, serials: &[String]) -> Option<&str> {
    match (volumes, serials) {
        (1, [serial]) => Some(serial),
        _ => None,
    }
}

/// What is on the Kindle at `volume`; `serial` from USB, if known.
pub fn inspect(volume: &Path, serial: Option<&str>) -> Kindle {
    let firmware = std::fs::read_to_string(volume.join("system/version.txt"))
        .ok()
        .and_then(|text| Firmware::parse(&text));
    let model = match serial {
        Some(serial) => Model::from_serial(serial),
        None => Model::Unknown {
            serial_prefix: None,
        },
    };
    Kindle {
        volume: volume.to_owned(),
        firmware,
        serial: serial.map(str::to_owned),
        model,
        prereqs: prereqs(volume),
        installed: installed(volume),
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

pub fn prereqs(volume: &Path) -> Prereqs {
    let documents = names(&volume.join("documents"));
    let kual = documents
        .iter()
        .filter(|name| {
            let lower = name.to_ascii_lowercase();
            lower == "kual.kual"
                || (lower.starts_with("kual") && lower.ends_with(".azw2"))
                || (lower.starts_with("kindlelauncher") && lower.ends_with(".azw2"))
        })
        .map(|name| format!("documents/{name}"))
        .collect();
    let mut jailbreak = Vec::new();
    for (path, what) in [
        ("extensions/MRInstaller", "MRPI (extensions/MRInstaller)"),
        ("mrpackages", "MRPI's mrpackages folder"),
        ("usbnet", "USBNetwork (usbnet)"),
        ("linkss", "the screensaver hack (linkss)"),
        ("linkfonts", "the fonts hack (linkfonts)"),
    ] {
        if volume.join(path).exists() {
            jailbreak.push(what.to_owned());
        }
    }
    for name in &documents {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".run_hotfix") || lower.starts_with("you are jailbroken") {
            jailbreak.push(format!("documents/{name}"));
        }
    }
    Prereqs {
        kual,
        jailbreak,
        extensions_folder: volume.join("extensions").is_dir(),
    }
}

/// The version kindil's binary names in its `kindil-version=X.Y.Z` marker
/// (`VERSION_MARKER` in src/update.rs, from 0.13.3): the binary is ARM, so
/// it can't be asked. Older releases carry no marker.
pub fn binary_version(binary: &[u8]) -> Option<String> {
    let needle = b"kindil-version=";
    let at = binary.windows(needle.len()).position(|w| w == needle)?;
    let rest = &binary[at + needle.len()..];
    let len = rest
        .iter()
        .take(20)
        .take_while(|b| b.is_ascii_digit() || **b == b'.')
        .count();
    let version = std::str::from_utf8(&rest[..len]).ok()?;
    (version.split('.').count() == 3 && version.split('.').all(|p| !p.is_empty()))
        .then(|| version.to_owned())
}

pub fn installed(volume: &Path) -> Option<Installed> {
    let binary = volume.join("extensions/kindil/bin/kindil");
    if !binary.is_file() && !volume.join("extensions/kindil/menu.json").is_file() {
        return None;
    }
    let version = std::fs::read(&binary).ok().and_then(|b| binary_version(&b));
    let config = ["kindil/miqat.toml", "kindil/kindil.toml"]
        .into_iter()
        .filter(|f| volume.join(f).is_file())
        .map(str::to_owned)
        .collect();
    Some(Installed { version, config })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_only_one_volume_with_one_device() {
        let one = ["B024X".to_owned()];
        assert_eq!(pair(1, &one), Some("B024X"));
        assert_eq!(pair(2, &one), None);
        assert_eq!(pair(1, &[]), None);
        assert_eq!(pair(1, &["a".into(), "b".into()]), None);
    }

    #[test]
    fn reads_fat_mounts() {
        let mounts = "/dev/sda1 / ext4 rw 0 0\n\
                      /dev/sdb1 /mnt/my\\040kindle vfat rw 0 0\n\
                      proc /proc proc rw 0 0\n";
        assert_eq!(fat_mounts(mounts), [PathBuf::from("/mnt/my kindle")]);
    }

    #[test]
    fn finds_the_version_in_the_binary() {
        let mut binary = b"\x7fELF...kindil/0.1.0 is not it...".to_vec();
        binary.extend_from_slice(b"kindil-version=0.13.2\nmore");
        assert_eq!(binary_version(&binary).as_deref(), Some("0.13.2"));
        assert_eq!(binary_version(b"kindil-version=1.2\n"), None);
        assert_eq!(binary_version(b"nothing"), None);
    }
}
