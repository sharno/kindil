//! Settings → Prayer times: the calculation method and the Asr school, with
//! today's times under each choice, written into Miqat's file in place.
use anyhow::Context;
use chrono::{DateTime, Utc};
use miqat::config::{CalculationMethod, Config as Miqat, Madhab};
use miqat::prayer::{DayTimes, Salah};
use std::path::Path;

/// One of Miqat's calculation methods: its value in `miqat.toml` and the
/// name shown on screen.
pub struct Method {
    pub method: CalculationMethod,
    pub key: &'static str,
    pub label: &'static str,
}

/// Every method Miqat knows, in the order the page lists them.
pub const METHODS: [Method; 12] = [
    Method {
        method: CalculationMethod::MuslimWorldLeague,
        key: "muslim_world_league",
        label: "Muslim World League",
    },
    Method {
        method: CalculationMethod::NorthAmerica,
        key: "north_america",
        label: "Islamic Society of North America (ISNA)",
    },
    Method {
        method: CalculationMethod::Egyptian,
        key: "egyptian",
        label: "Egyptian General Authority",
    },
    Method {
        method: CalculationMethod::Karachi,
        key: "karachi",
        label: "Karachi (University of Islamic Sciences)",
    },
    Method {
        method: CalculationMethod::UmmAlQura,
        key: "umm_al_qura",
        label: "Umm al-Qura, Makkah",
    },
    Method {
        method: CalculationMethod::Dubai,
        key: "dubai",
        label: "Dubai",
    },
    Method {
        method: CalculationMethod::MoonsightingCommittee,
        key: "moonsighting_committee",
        label: "Moonsighting Committee",
    },
    Method {
        method: CalculationMethod::Kuwait,
        key: "kuwait",
        label: "Kuwait",
    },
    Method {
        method: CalculationMethod::Qatar,
        key: "qatar",
        label: "Qatar",
    },
    Method {
        method: CalculationMethod::Singapore,
        key: "singapore",
        label: "Singapore (MUIS)",
    },
    Method {
        method: CalculationMethod::Tehran,
        key: "tehran",
        label: "Tehran (Institute of Geophysics)",
    },
    Method {
        method: CalculationMethod::Turkey,
        key: "turkey",
        label: "Turkey (Diyanet)",
    },
];

/// An Asr school: its `madhab` value in `miqat.toml` and the name shown.
pub struct School {
    pub madhab: Madhab,
    pub key: &'static str,
    pub label: &'static str,
    /// For the Settings index: "Standard Asr".
    pub short: &'static str,
}

pub const SCHOOLS: [School; 2] = [
    School {
        madhab: Madhab::Shafi,
        key: "shafi",
        label: "Standard (Shafi'i, Maliki, Hanbali)",
        short: "Standard Asr",
    },
    School {
        madhab: Madhab::Hanafi,
        key: "hanafi",
        label: "Hanafi",
        short: "Hanafi Asr",
    },
];

/// A choice the owner taps on the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting {
    Method(CalculationMethod),
    Asr(Madhab),
}

impl Setting {
    /// The key under `[prayer]` and its value.
    fn entry(self) -> (&'static str, &'static str) {
        match self {
            Setting::Method(method) => ("method", method_of(method).key),
            Setting::Asr(madhab) => ("madhab", school_of(madhab).key),
        }
    }
}

fn method_of(method: CalculationMethod) -> &'static Method {
    METHODS
        .iter()
        .find(|m| m.method == method)
        .unwrap_or(&METHODS[0])
}

fn school_of(madhab: Madhab) -> &'static School {
    SCHOOLS
        .iter()
        .find(|s| s.madhab == madhab)
        .unwrap_or(&SCHOOLS[0])
}

/// One row: the choice and today's times under it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub label: &'static str,
    pub times: String,
}

/// What the page shows for the file as it is now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    pub methods: Vec<Choice>,
    pub method: usize,
    pub schools: Vec<Choice>,
    pub school: usize,
    /// For the Settings index: "Muslim World League · Standard Asr".
    pub summary: String,
}

impl Page {
    /// Today's times at the configured location, for each method (Fajr and
    /// Isha, the times methods differ on) and each school (Asr).
    pub fn of(config: &Miqat, now: DateTime<Utc>) -> anyhow::Result<Self> {
        let date = miqat::prayer::local_date(&config.location, now);
        let at = |day: &DayTimes, salah: Salah| {
            miqat::layout::format_time(
                day.at(salah),
                config.location.timezone,
                config.display.clock,
            )
        };
        let methods = METHODS
            .iter()
            .map(|m| {
                let prayer = miqat::config::PrayerConfig {
                    method: m.method,
                    ..config.prayer
                };
                let day = DayTimes::compute(&config.location, &prayer, date)?;
                Ok(Choice {
                    label: m.label,
                    times: format!(
                        "Fajr {} · Isha {}",
                        at(&day, Salah::Fajr),
                        at(&day, Salah::Isha)
                    ),
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let schools = SCHOOLS
            .iter()
            .map(|s| {
                let prayer = miqat::config::PrayerConfig {
                    madhab: s.madhab,
                    ..config.prayer
                };
                let day = DayTimes::compute(&config.location, &prayer, date)?;
                Ok(Choice {
                    label: s.label,
                    times: format!("Asr {}", at(&day, Salah::Asr)),
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let method = METHODS
            .iter()
            .position(|m| m.method == config.prayer.method)
            .unwrap_or(0);
        let school = SCHOOLS
            .iter()
            .position(|s| s.madhab == config.prayer.madhab)
            .unwrap_or(0);
        Ok(Self {
            summary: format!("{} · {}", METHODS[method].label, SCHOOLS[school].short),
            methods,
            method,
            schools,
            school,
        })
    }
}

/// Miqat's file as the dashboard reads it, and the page for it.
pub fn load(path: &Path, now: DateTime<Utc>) -> anyhow::Result<Page> {
    Page::of(&Miqat::load(path)?, now)
}

/// Sets one choice under `[prayer]`, keeping every comment and other key as
/// they are. A missing or broken file is reported and left alone. Written
/// beside the file and renamed over it, so a power cut keeps the old one.
pub fn save(path: &Path, setting: Setting) -> anyhow::Result<()> {
    let text = edit(
        &std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?,
        setting,
    )
    .with_context(|| format!("parsing {}", path.display()))?;
    replace(path, &text).with_context(|| format!("writing {}", path.display()))
}

/// The file's text with `setting` changed; an error if the text isn't a
/// config Miqat accepts, before or after.
pub fn edit(text: &str, setting: Setting) -> anyhow::Result<String> {
    toml::from_str::<Miqat>(text)?;
    let mut doc: toml_edit::DocumentMut = text.parse()?;
    let prayer = doc
        .get_mut("prayer")
        .and_then(toml_edit::Item::as_table_like_mut)
        .context("no [prayer] table")?;
    let (key, value) = setting.entry();
    match prayer.get_mut(key).and_then(toml_edit::Item::as_value_mut) {
        // The value's own spacing and trailing comment stay.
        Some(old) => {
            let decor = old.decor().clone();
            *old = value.into();
            *old.decor_mut() = decor;
        }
        None => {
            prayer.insert(key, toml_edit::value(value));
        }
    }
    let text = doc.to_string();
    let parsed: Miqat = toml::from_str(&text)?;
    let applied = match setting {
        Setting::Method(method) => parsed.prayer.method == method,
        Setting::Asr(madhab) => parsed.prayer.madhab == madhab,
    };
    anyhow::ensure!(applied, "{key} did not change");
    Ok(text)
}

fn replace(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let temp = path.with_extension("toml.tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o644)
        .open(&temp)?;
    file.write_all(text.as_bytes())?;
    // FAT has no journal: the bytes are on the card before the rename.
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MECCA: &str = r#"# Where the dashboard is.
[location]
latitude = 21.4225
longitude = 39.8262
timezone = "Asia/Riyadh"   # IANA name

[prayer]
# muslim_world_league | egyptian | ...
method = "muslim_world_league"   # the mosque's choice
hijri_offset_days = 0           # shift the tabular Hijri date

# Optional per-prayer offsets in minutes.
[prayer.adjust_minutes]
fajr = 0
isha = 2

[display]
clock = "24h"
"#;

    fn noon() -> DateTime<Utc> {
        DateTime::from_timestamp(1_794_398_400, 0).unwrap()
    }

    #[test]
    fn every_method_and_school_is_named_as_miqat_reads_it() {
        assert_eq!(METHODS.len(), 12);
        for m in &METHODS {
            let parsed: miqat::config::PrayerConfig =
                toml::from_str(&format!("method = \"{}\"", m.key)).unwrap();
            assert_eq!(parsed.method, m.method, "{}", m.key);
            assert_eq!(method_of(m.method).key, m.key);
        }
        for s in &SCHOOLS {
            let parsed: miqat::config::PrayerConfig =
                toml::from_str(&format!("method = \"egyptian\"\nmadhab = \"{}\"", s.key)).unwrap();
            assert_eq!(parsed.madhab, s.madhab, "{}", s.key);
        }
        let keys: std::collections::BTreeSet<_> = METHODS.iter().map(|m| m.key).collect();
        assert_eq!(keys.len(), METHODS.len(), "no method listed twice");
    }

    #[test]
    fn editing_keeps_comments_and_other_keys() {
        let text = edit(MECCA, Setting::Method(CalculationMethod::UmmAlQura)).unwrap();
        assert_eq!(
            text,
            MECCA.replace(
                "method = \"muslim_world_league\"   # the mosque's choice",
                "method = \"umm_al_qura\"   # the mosque's choice"
            )
        );
        // An absent madhab is added under [prayer], not under adjust_minutes.
        let text = edit(&text, Setting::Asr(Madhab::Hanafi)).unwrap();
        let config: Miqat = toml::from_str(&text).unwrap();
        assert_eq!(config.prayer.method, CalculationMethod::UmmAlQura);
        assert_eq!(config.prayer.madhab, Madhab::Hanafi);
        assert_eq!(config.prayer.adjust_minutes.isha, 2);
        assert!(text.contains("# Optional per-prayer offsets in minutes."));
        assert!(text.contains("timezone = \"Asia/Riyadh\"   # IANA name"));
        let back = edit(&text, Setting::Asr(Madhab::Shafi)).unwrap();
        assert_eq!(
            toml::from_str::<Miqat>(&back).unwrap().prayer.madhab,
            Madhab::Shafi
        );
    }

    #[test]
    fn a_broken_or_missing_file_is_reported_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("miqat.toml");
        let setting = Setting::Method(CalculationMethod::Karachi);
        assert!(save(&path, setting).is_err());
        assert!(load(&path, noon()).is_err());
        assert!(!path.exists(), "nothing written for a missing file");
        let broken = MECCA.replace("[display]", "[display");
        std::fs::write(&path, &broken).unwrap();
        let error = save(&path, setting).unwrap_err();
        assert!(format!("{error:#}").contains("parsing"), "{error:#}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
        // Valid TOML that Miqat rejects is left alone too.
        let unknown = MECCA.replace("hijri_offset_days", "hijri_ofset_days");
        std::fs::write(&path, &unknown).unwrap();
        assert!(save(&path, setting).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), unknown);
    }

    #[test]
    fn saving_replaces_the_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("miqat.toml");
        std::fs::write(&path, MECCA).unwrap();
        save(&path, Setting::Method(CalculationMethod::Turkey)).unwrap();
        let page = load(&path, noon()).unwrap();
        assert_eq!(METHODS[page.method].method, CalculationMethod::Turkey);
        assert!(!path.with_extension("toml.tmp").exists());
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o600, 0o600, "still the owner's to edit");
    }

    #[test]
    fn times_are_for_the_configured_location() {
        let mecca: Miqat = toml::from_str(MECCA).unwrap();
        let page = Page::of(&mecca, noon()).unwrap();
        assert_eq!(page.methods.len(), 12);
        assert_eq!(page.method, 0);
        assert_eq!(page.school, 0);
        assert_eq!(page.summary, "Muslim World League · Standard Asr");
        // Each row is that method's times with the file's offsets, in
        // Mecca's own time zone.
        let date = miqat::prayer::local_date(&mecca.location, noon());
        let umm_al_qura = miqat::config::PrayerConfig {
            method: CalculationMethod::UmmAlQura,
            ..mecca.prayer
        };
        let day = DayTimes::compute(&mecca.location, &umm_al_qura, date).unwrap();
        let local = |s| {
            day.at(s)
                .with_timezone(&mecca.location.timezone)
                .format("%H:%M")
        };
        assert_eq!(
            page.methods[4].times,
            format!("Fajr {} · Isha {}", local(Salah::Fajr), local(Salah::Isha))
        );
        assert_eq!(page.methods[4].times, "Fajr 05:10 · Isha 19:12");
        // Somewhere else, other times.
        let amsterdam: Miqat = toml::from_str(
            &MECCA
                .replace("21.4225", "52.37")
                .replace("39.8262", "4.90")
                .replace("Asia/Riyadh", "Europe/Amsterdam"),
        )
        .unwrap();
        let there = Page::of(&amsterdam, noon()).unwrap();
        assert_ne!(there.methods[0].times, page.methods[0].times);
        assert_eq!(there.methods[0].times, "Fajr 05:56 · Isha 18:47");
        // Hanafi Asr comes later than the standard one.
        assert!(page.schools[1].times > page.schools[0].times);
    }
}
