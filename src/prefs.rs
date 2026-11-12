//! Choices made on the device, persisted across restarts.
use crate::config::Dashboard;
use crate::host::Power;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Reading font size in px, 16..=40 in steps of 2.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct FontSize(u8);

impl FontSize {
    pub const MIN: u8 = 16;
    pub const MAX: u8 = 40;
    pub fn px(self) -> u8 {
        self.0
    }
    pub fn larger(self) -> Self {
        Self((self.0 + 2).min(Self::MAX))
    }
    pub fn smaller(self) -> Self {
        Self((self.0 - 2).max(Self::MIN))
    }
}

impl Default for FontSize {
    fn default() -> Self {
        Self(24)
    }
}

impl TryFrom<u8> for FontSize {
    type Error = String;
    fn try_from(px: u8) -> Result<Self, Self::Error> {
        (Self::MIN..=Self::MAX)
            .contains(&px)
            .then_some(Self(px))
            .ok_or_else(|| format!("font size {px} outside {}..={}", Self::MIN, Self::MAX))
    }
}

impl From<FontSize> for u8 {
    fn from(size: FontSize) -> u8 {
        size.0
    }
}

/// How far into a book, 0.0..=1.0; survives repagination at a new font size.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "f32", into = "f32")]
pub struct Progress(f32);

impl Progress {
    pub const START: Self = Self(0.0);
    pub fn of(page: usize, pages: usize) -> Self {
        Self(page as f32 / pages.max(1) as f32)
    }
    pub fn page(self, pages: usize) -> usize {
        ((self.0 * pages as f32) as usize).min(pages.saturating_sub(1))
    }
}

impl TryFrom<f32> for Progress {
    type Error = String;
    fn try_from(value: f32) -> Result<Self, Self::Error> {
        (0.0..=1.0)
            .contains(&value)
            .then_some(Self(value))
            .ok_or_else(|| format!("progress {value} outside 0..=1"))
    }
}

impl From<Progress> for f32 {
    fn from(progress: Progress) -> f32 {
        progress.0
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Prefs {
    pub dashboard: Option<Dashboard>,
    pub font: FontSize,
    pub sync: Power,
    /// Keyed by file name inside the books directory.
    pub positions: BTreeMap<String, Progress>,
}

pub struct Store {
    path: PathBuf,
    pub prefs: Prefs,
}

impl Store {
    /// A missing or unreadable file starts fresh; preferences are not worth failing over.
    pub fn open(path: &Path) -> Self {
        let prefs = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default();
        Self {
            path: path.to_owned(),
            prefs,
        }
    }

    /// Write-then-rename, so a power cut never leaves a half-written file.
    pub fn save(&self) -> anyhow::Result<()> {
        let text = toml::to_string(&self.prefs)?;
        let temp = self.path.with_extension("toml.tmp");
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&temp, text)?;
        std::fs::rename(&temp, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trips_and_rejects_out_of_range() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.toml");
        let mut store = Store::open(&path);
        assert_eq!(store.prefs, Prefs::default());
        store.prefs.dashboard = Some(Dashboard::Clock);
        store.prefs.font = store.prefs.font.larger();
        store
            .prefs
            .positions
            .insert("book.epub".into(), Progress::of(5, 10));
        store.save().unwrap();
        assert_eq!(Store::open(&path).prefs, store.prefs);
        assert!(toml::from_str::<Prefs>("font = 99").is_err());
        assert_eq!(Progress::of(5, 10).page(20), 10);
        assert_eq!(FontSize::default().smaller().px(), 22);
    }
}
