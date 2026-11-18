//! The naps' flight recorder: a line for how each doze wake ended (what it
//! drew, for which minute, the panel's frame count) and for each step of
//! every suspend (Wi-Fi parked, the alarm armed and read back, suspending,
//! resumed),
//! stamped and written before the step, so after a freeze the last line
//! says where the device stopped. Kept apart from kindil.log, which would
//! drown in three lines a minute: [`MAX_BYTES`] in the file and as much in
//! the one before it, about half a day of naps. The kernel syncs the
//! filesystems on its way into each suspend, so the line written just
//! before one is on the flash when the device sleeps.
use std::io::Write;
use std::path::{Path, PathBuf};

pub const PATH: &str = "/var/local/kindil/naps.log";
/// The file moves to `naps.log.1` past this size.
pub const MAX_BYTES: u64 = 96 * 1024;

pub struct Recorder {
    path: PathBuf,
    max_bytes: u64,
}

impl Recorder {
    pub fn new(path: impl Into<PathBuf>, max_bytes: u64) -> Self {
        Self {
            path: path.into(),
            max_bytes,
        }
    }

    /// The device's recorder.
    pub fn device() -> Self {
        Self::new(PATH, MAX_BYTES)
    }

    fn previous(&self) -> PathBuf {
        let mut name = self.path.as_os_str().to_owned();
        name.push(".1");
        PathBuf::from(name)
    }

    /// Appends `line`, stamped; never fails (a full disk loses the line).
    pub fn note(&self, line: &str) {
        let stamp = crate::logging::stamp(chrono::Utc::now(), crate::logging::zone());
        self.append(&format!("{stamp} {line}\n"));
    }

    fn append(&self, text: &str) {
        if std::fs::metadata(&self.path).is_ok_and(|m| m.len() >= self.max_bytes) {
            let _ = std::fs::rename(&self.path, self.previous());
        }
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .and_then(|mut file| file.write_all(text.as_bytes()));
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_lines_and_keeps_one_older_file() {
        let dir = tempfile::tempdir().unwrap();
        let recorder = Recorder::new(dir.path().join("naps.log"), 64);
        for n in 0..10 {
            recorder.note(&format!("nap {n}: suspending"));
        }
        let now = std::fs::read_to_string(recorder.path()).unwrap();
        let before = std::fs::read_to_string(recorder.previous()).unwrap();
        assert!(now.ends_with("nap 9: suspending\n"), "{now}");
        assert!(before.contains("suspending\n"));
        assert!(now.len() < 64 + 50, "{}", now.len());
        // Nothing else is left beside them.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}
