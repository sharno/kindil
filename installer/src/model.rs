//! Which Kindle this is, from its serial number, and which firmware it runs,
//! from `system/version.txt` on its USB storage.
//!
//! The serial number's first four characters (six on 2015 and later models)
//! name the model. The table follows MobileRead's "Kindle Serial Numbers"
//! wiki page (<https://wiki.mobileread.com/wiki/Kindle_Serial_Numbers>),
//! which KindleTool's device list is kept in step with.
use std::fmt;

/// The first-generation Paperwhite (2012, "PW1", board EY21): the only
/// Kindle kindil runs on.
const PAPERWHITE_1: [(&str, &str); 6] = [
    ("B024", "Wi-Fi"),
    ("B01B", "3G + Wi-Fi, U.S."),
    ("B020", "3G + Wi-Fi, Brazil"),
    ("B01C", "3G + Wi-Fi, Canada"),
    ("B01D", "3G + Wi-Fi, Europe"),
    ("B01F", "3G + Wi-Fi, Japan"),
];

/// Older four-character prefixes, to name what was plugged in instead.
const OTHERS: &[(&str, &str)] = &[
    ("B001", "Kindle 1"),
    ("B101", "Kindle 1"),
    ("B002", "Kindle 2"),
    ("B003", "Kindle 2"),
    ("B004", "Kindle DX"),
    ("B005", "Kindle DX"),
    ("B009", "Kindle DX Graphite"),
    ("B006", "Kindle Keyboard (Kindle 3)"),
    ("B008", "Kindle Keyboard (Kindle 3)"),
    ("B00A", "Kindle Keyboard (Kindle 3)"),
    ("B00C", "Kindle Paperwhite test unit (not for sale)"),
    ("B00E", "Kindle 4"),
    ("B023", "Kindle 4"),
    ("9023", "Kindle 4"),
    ("B00F", "Kindle Touch"),
    ("B010", "Kindle Touch"),
    ("B011", "Kindle Touch"),
    ("B012", "Kindle Touch"),
    ("B0D4", "Kindle Paperwhite 2 (2013)"),
    ("90D4", "Kindle Paperwhite 2 (2013)"),
    ("B05A", "Kindle Paperwhite 2 (2013)"),
    ("905A", "Kindle Paperwhite 2 (2013)"),
    ("B0D5", "Kindle Paperwhite 2 (2013)"),
    ("90D5", "Kindle Paperwhite 2 (2013)"),
    ("B0D6", "Kindle Paperwhite 2 (2013)"),
    ("90D6", "Kindle Paperwhite 2 (2013)"),
    ("B0D7", "Kindle Paperwhite 2 (2013)"),
    ("90D7", "Kindle Paperwhite 2 (2013)"),
    ("B0D8", "Kindle Paperwhite 2 (2013)"),
    ("90D8", "Kindle Paperwhite 2 (2013)"),
    ("B0F2", "Kindle Paperwhite 2 (2013)"),
    ("90F2", "Kindle Paperwhite 2 (2013)"),
    ("B017", "Kindle Paperwhite 2 (2013)"),
    ("9017", "Kindle Paperwhite 2 (2013)"),
    ("B060", "Kindle Paperwhite 2 (2013)"),
    ("9060", "Kindle Paperwhite 2 (2013)"),
    ("B062", "Kindle Paperwhite 2 (2013)"),
    ("9062", "Kindle Paperwhite 2 (2013)"),
    ("B05F", "Kindle Paperwhite 2 (2013)"),
    ("905F", "Kindle Paperwhite 2 (2013)"),
    ("B061", "Kindle Paperwhite 2 (2013)"),
    ("9061", "Kindle Paperwhite 2 (2013)"),
    ("B0C6", "Kindle (2014)"),
    ("90C6", "Kindle (2014)"),
    ("B0DD", "Kindle (2014)"),
    ("90DD", "Kindle (2014)"),
    ("B013", "Kindle Voyage"),
    ("9013", "Kindle Voyage"),
    ("B054", "Kindle Voyage"),
    ("9054", "Kindle Voyage"),
    ("B053", "Kindle Voyage"),
    ("9053", "Kindle Voyage"),
    ("B02A", "Kindle Voyage"),
    ("B052", "Kindle Voyage"),
    ("9052", "Kindle Voyage"),
    ("B04F", "Kindle Voyage"),
];

/// Which Kindle a serial number belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Model {
    /// A first-generation Paperwhite, with its variant ("Wi-Fi", ...).
    Paperwhite1 { variant: &'static str },
    /// Another Kindle, named.
    Other { name: String },
    /// No serial number to go by, or one not in the table.
    Unknown { serial_prefix: Option<String> },
}

impl Model {
    pub fn from_serial(serial: &str) -> Self {
        let serial = serial.trim().to_ascii_uppercase();
        let prefix = serial.get(..4).unwrap_or(&serial);
        if let Some((_, variant)) = PAPERWHITE_1.iter().find(|(p, _)| *p == prefix) {
            return Model::Paperwhite1 { variant };
        }
        if let Some((_, name)) = OTHERS.iter().find(|(p, _)| *p == prefix) {
            return Model::Other {
                name: (*name).to_owned(),
            };
        }
        // From 2015 on, serial numbers start with G and a six-character
        // model code (G090G1 for the Paperwhite 3, and so on).
        if serial.starts_with('G') && serial.len() >= 6 {
            return Model::Other {
                name: format!("a Kindle from 2015 or later (serial {})", &serial[..6]),
            };
        }
        Model::Unknown {
            serial_prefix: (!serial.is_empty()).then(|| prefix.to_owned()),
        }
    }

    pub fn is_paperwhite1(&self) -> bool {
        matches!(self, Model::Paperwhite1 { .. })
    }
}

impl fmt::Display for Model {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Model::Paperwhite1 { variant } => {
                write!(f, "Kindle Paperwhite, 1st generation (2012, {variant})")
            }
            Model::Other { name } => f.write_str(name),
            Model::Unknown {
                serial_prefix: Some(prefix),
            } => write!(f, "an unknown Kindle (serial {prefix}…)"),
            Model::Unknown {
                serial_prefix: None,
            } => f.write_str("a Kindle (model unknown)"),
        }
    }
}

/// The firmware, as `system/version.txt` gives it:
/// `Kindle 5.6.1.1 (2689890035)` or `Kindle 5.16.2.1.1 (409747 002)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Firmware {
    /// `5.6.1.1`
    pub version: String,
    /// What follows in parentheses, if anything.
    pub build: Option<String>,
}

impl Firmware {
    pub fn parse(text: &str) -> Option<Self> {
        let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
        let rest = line.strip_prefix("Kindle").unwrap_or(line).trim();
        let (version, build) = match rest.split_once('(') {
            Some((version, build)) => (
                version.trim(),
                Some(build.trim_end_matches(')').trim().to_owned()),
            ),
            None => (rest, None),
        };
        let valid = !version.is_empty()
            && version
                .split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()));
        valid.then(|| Firmware {
            version: version.to_owned(),
            build: build.filter(|b| !b.is_empty()),
        })
    }

    /// The version's numbers, for comparing.
    pub fn numbers(&self) -> Vec<u32> {
        self.version
            .split('.')
            .filter_map(|n| n.parse().ok())
            .collect()
    }

    /// At or below 5.4.4.2: the classic Kindle Touch/PW1 jailbreak works as
    /// it is; above (5.6.1.1, the PW1's last), it needs a downgrade first.
    pub fn classic_jailbreak_works(&self) -> bool {
        self.numbers() <= vec![5, 4, 4, 2]
    }
}

impl fmt::Display for Firmware {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_paperwhite_1_by_its_serial() {
        for serial in [
            "B024123456789ABC",
            "b01b0000",
            "B020",
            "B01C",
            "B01D",
            "B01F",
        ] {
            assert!(Model::from_serial(serial).is_paperwhite1(), "{serial}");
        }
        assert_eq!(
            Model::from_serial("B024XXXX").to_string(),
            "Kindle Paperwhite, 1st generation (2012, Wi-Fi)"
        );
    }

    #[test]
    fn names_other_kindles() {
        let other = |serial| match Model::from_serial(serial) {
            Model::Other { name } => name,
            model => panic!("{serial}: {model:?}"),
        };
        assert_eq!(other("90D4AAAA"), "Kindle Paperwhite 2 (2013)");
        assert_eq!(other("B0D4AAAA"), "Kindle Paperwhite 2 (2013)");
        assert_eq!(other("B011AAAA"), "Kindle Touch");
        assert!(other("G090G1AAAAAA").contains("2015 or later"));
        assert_eq!(
            Model::from_serial("ZZZZ1234"),
            Model::Unknown {
                serial_prefix: Some("ZZZZ".into())
            }
        );
        assert_eq!(
            Model::from_serial(""),
            Model::Unknown {
                serial_prefix: None
            }
        );
    }

    #[test]
    fn reads_version_txt() {
        let fw = Firmware::parse("Kindle 5.6.1.1 (2689890035)\n").unwrap();
        assert_eq!(fw.version, "5.6.1.1");
        assert_eq!(fw.build.as_deref(), Some("2689890035"));
        assert!(!fw.classic_jailbreak_works());
        let fw = Firmware::parse("Kindle 5.16.2.1.1 (409747 002)").unwrap();
        assert_eq!(fw.numbers(), [5, 16, 2, 1, 1]);
        assert!(Firmware::parse("Kindle 5.4.4.2")
            .unwrap()
            .classic_jailbreak_works());
        assert!(Firmware::parse("Kindle 5.3.3 (x)")
            .unwrap()
            .classic_jailbreak_works());
        assert!(!Firmware::parse("Kindle 5.4.5")
            .unwrap()
            .classic_jailbreak_works());
        assert_eq!(Firmware::parse(""), None);
        assert_eq!(Firmware::parse("not a version"), None);
    }
}
