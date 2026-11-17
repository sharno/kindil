//! What must be true before kindil goes on a Kindle, as a checklist the CLI
//! prints and the window shows.
//!
//! kindil runs as root, started from a KUAL extension. Amazon's firmware
//! only runs code Amazon signed, so the Kindle needs a jailbreak (done once,
//! on the Kindle itself) and KUAL. The installer never bundles or downloads
//! either: it points to the MobileRead community's instructions.
use crate::detect::Kindle;
use crate::model::Model;

/// The MobileRead wiki's serial number table.
pub const SERIALS: &str = "https://wiki.mobileread.com/wiki/Kindle_Serial_Numbers";
/// The classic Kindle Touch/PW1/PW2 jailbreak, firmware 5.0.x to 5.4.4.2.
pub const JAILBREAK: &str = "https://www.mobileread.com/forums/showthread.php?t=186645";
/// Downgrading a PW1 from 5.6.1.1 to 5.3.3, for the classic jailbreak.
pub const DOWNGRADE: &str = "https://www.mobileread.com/forums/showthread.php?t=264432";
/// KindleModding's current jailbreak guides (WinterBreak and others).
pub const GUIDES: &str = "https://kindlemodding.org/jailbreaking/";
/// KUAL, the Kindle Unified Application Launcher.
pub const KUAL: &str = "https://www.mobileread.com/forums/showthread.php?t=203326";
/// MRPI, which installs KUAL's booklet and other packages.
pub const MRPI: &str = "https://www.mobileread.com/forums/showthread.php?t=251143";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Pass,
    Fail,
    /// Can't be told from the computer.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub title: String,
    pub status: Status,
    pub detail: String,
    /// Where to read more, or what to do.
    pub links: Vec<&'static str>,
}

/// The checklist for `kindle`; installing needs every item to pass (or an
/// unknown model to be vouched for with `force`).
pub fn checklist(kindle: &Kindle) -> Vec<Check> {
    vec![model(kindle), jailbreak(kindle), kual(kindle)]
}

/// Whether `checks` allow installing; `force` vouches for an unknown model.
pub fn ready(checks: &[Check], force: bool) -> bool {
    checks.iter().all(|c| match c.status {
        Status::Pass => true,
        Status::Unknown => force && c.title == MODEL,
        Status::Fail => false,
    })
}

const MODEL: &str = "Kindle Paperwhite, 1st generation";

fn model(kindle: &Kindle) -> Check {
    let firmware = kindle
        .firmware
        .as_ref()
        .map(|f| format!(", firmware {f}"))
        .unwrap_or_default();
    match &kindle.model {
        Model::Paperwhite1 { .. } => Check {
            title: MODEL.into(),
            status: Status::Pass,
            detail: format!(
                "{}{firmware} (serial {}…)",
                kindle.model,
                kindle
                    .serial
                    .as_deref()
                    .unwrap_or("")
                    .get(..4)
                    .unwrap_or("")
            ),
            links: vec![],
        },
        Model::Other { name } => Check {
            title: MODEL.into(),
            status: Status::Fail,
            detail: format!(
                "This is {name}{firmware}. kindil only runs on the first-generation \
                 Paperwhite (2012): it drives that model's screen, touch, front light \
                 and Wi-Fi directly, and would not work on another Kindle."
            ),
            links: vec![SERIALS],
        },
        Model::Unknown { serial_prefix } => Check {
            title: MODEL.into(),
            status: Status::Unknown,
            detail: format!(
                "The model can't be told from here{}{firmware}. Check the serial number in \
                 Settings → Device Info on the Kindle: a Paperwhite 1's starts with B024, \
                 B01B, B020, B01C, B01D or B01F. If it does, install with --force.",
                serial_prefix
                    .as_ref()
                    .map(|p| format!(" (serial {p}…)"))
                    .unwrap_or_default()
            ),
            links: vec![SERIALS],
        },
    }
}

fn jailbreak(kindle: &Kindle) -> Check {
    let p = &kindle.prereqs;
    if p.jailbroken() {
        let signs: Vec<&str> = p
            .kual
            .iter()
            .chain(&p.jailbreak)
            .map(String::as_str)
            .collect();
        return Check {
            title: "Jailbreak".into(),
            status: Status::Pass,
            detail: format!("Found: {}.", signs.join(", ")),
            links: vec![],
        };
    }
    let links = match &kindle.firmware {
        Some(f) if f.classic_jailbreak_works() => vec![JAILBREAK, GUIDES],
        _ => vec![DOWNGRADE, JAILBREAK, GUIDES],
    };
    let how = match &kindle.firmware {
        Some(f) if f.classic_jailbreak_works() => {
            format!("On firmware {f}, the classic Kindle Touch/PW1 jailbreak applies (first link).")
        }
        Some(f) => format!(
            "On firmware {f}, MobileRead users downgrade to 5.3.3 first (first link), then \
             run the classic jailbreak (second link). Read the threads through before \
             starting: a firmware change can brick a Kindle."
        ),
        None => "Check the firmware in Settings → Device Info: up to 5.4.4.2 the classic \
                 jailbreak applies; on 5.6.1.1 MobileRead users downgrade to 5.3.3 first."
            .to_owned(),
    };
    Check {
        title: "Jailbreak".into(),
        status: Status::Fail,
        detail: format!(
            "No sign of a jailbreak. kindil runs as root, and Amazon's firmware only runs \
             code Amazon signed: a jailbreak lifts that. It is a one-time step done on the \
             Kindle itself, and this installer doesn't do it. {how}"
        ),
        links,
    }
}

fn kual(kindle: &Kindle) -> Check {
    let p = &kindle.prereqs;
    if !p.kual.is_empty() {
        let mut detail = format!("Found: {}.", p.kual.join(", "));
        if !p.extensions_folder {
            detail.push_str(" The extensions folder will be created.");
        }
        return Check {
            title: "KUAL".into(),
            status: Status::Pass,
            detail,
            links: vec![],
        };
    }
    Check {
        title: "KUAL".into(),
        status: Status::Fail,
        detail: "KUAL isn't installed (no documents/KUAL.kual or KUAL-*.azw2). kindil starts \
                 from a KUAL menu: install KUAL after the jailbreak, usually with MRPI."
            .into(),
        links: vec![KUAL, MRPI],
    }
}
