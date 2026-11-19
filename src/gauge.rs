//! The PW1's fuel gauge, read from the chip at the moment of asking.
//!
//! The gauge is a TI bq27000-family chip (bq27200 register map) at I2C
//! address 0x55, run by Lab126's `yoshi_battery` driver. The driver's sysfs
//! files (`battery_capacity`, `battery_mAH`, `battery_current`,
//! `battery_temperature`) only print values it cached the last time its
//! polling work ran: every 20 s, and only while awake. The work is
//! cancelled at each suspend and scheduled 20 s after each resume, so a
//! dozing Kindle, awake about 1.5 s a minute, refreshes them almost only in
//! its 2-minute maintenance windows: hours old, then a jump. The chip itself
//! keeps measuring through suspend.
//!
//! The driver also has a register window: writing a register number (in
//! hex) to `battreg` stores it in a variable, and reading `battreg_value`
//! makes one SMBus byte read of that register from the chip, then and
//! there, printing `Battery Register <decimal>` and ` Value: 0x<hex>`. Each
//! read is one whole I2C transaction under the adapter's lock, so it can't
//! interleave with the driver's own; only the register number is shared,
//! by whoever writes `battreg`. kindil writes it under [`SELECT`], and checks
//! that the register printed back is the one it asked for.
//!
//! Registers and scaling, as the driver reads them (Amazon's GPL release,
//! drivers/power/yoshi_battery.c; the bq27200 datasheet, SLUS710): two-byte
//! values are little endian, in units of 3.57 µV·h (or µV) over the 20 mΩ
//! sense resistor, i.e. `raw × 357 / 2000` mAh (mA).
//!
//! | Register | Name | kindil reads it as |
//! |---|---|---|
//! | 0x06–0x07 | TEMP | 0.25 K: `((raw + 2) / 4 − 273)` °C, then °F |
//! | 0x0A | FLAGS | bit 7, CHGS: the current is charging, else discharging |
//! | 0x10–0x11 | CACT | remaining charge, mAh (the driver's `battery_mAH`) |
//! | 0x14–0x15 | AI | average current, mA, unsigned (sign from CHGS) |
//! | 0x2C | CSOC | compensated state of charge, % (`battery_capacity`) |
//!
//! kindil only reads: it never writes `battreg_value`, which would write
//! the chip.
use crate::drain::MilliampHours;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The driver's sysfs directory.
pub const DIR: &str = "/sys/devices/system/yoshi_battery/yoshi_battery0";

/// Held from writing `battreg` until `battreg_value` is read.
static SELECT: Mutex<()> = Mutex::new(());

const TEMP: u8 = 0x06;
const FLAGS: u8 = 0x0A;
const CACT: u8 = 0x10;
const AI: u8 = 0x14;
const CSOC: u8 = 0x2C;
/// FLAGS' CHGS bit.
const CHARGING: u8 = 0x80;

/// One byte of the gauge's registers.
pub trait Registers {
    fn byte(&self, register: u8) -> anyhow::Result<u8>;
}

/// The chip, through the driver's register window.
pub struct Window {
    dir: PathBuf,
}

impl Window {
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
        }
    }
}

impl Default for Window {
    fn default() -> Self {
        Self::new(Path::new(DIR))
    }
}

impl Registers for Window {
    fn byte(&self, register: u8) -> anyhow::Result<u8> {
        let _select = SELECT.lock().unwrap_or_else(|p| p.into_inner());
        std::fs::write(self.dir.join("battreg"), format!("{register:x}"))?;
        // Opened anew: the driver reads the chip on each read of the file.
        let text = std::fs::read_to_string(self.dir.join("battreg_value"))?;
        parse_value(&text, register)
    }
}

/// The value in `battreg_value`'s text, if it is `register`'s: another
/// writer of `battreg` in between would have made it another's, and a
/// failed I2C read prints its negative error code as a 32-bit hex number.
pub fn parse_value(text: &str, register: u8) -> anyhow::Result<u8> {
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.trim().strip_prefix(name))
            .map(str::trim)
    };
    let read: u32 = field("Battery Register")
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("unexpected battreg_value {text:?}"))?;
    anyhow::ensure!(
        read == u32::from(register),
        "battreg_value is register {read}, not {register}: battreg changed meanwhile"
    );
    let value = field("Value:")
        .and_then(|v| v.strip_prefix("0x"))
        .and_then(|v| u32::from_str_radix(v, 16).ok())
        .ok_or_else(|| anyhow::anyhow!("unexpected battreg_value {text:?}"))?;
    u8::try_from(value).map_err(|_| {
        anyhow::anyhow!(
            "reading gauge register {register:#04x} failed (error {})",
            value as i32
        )
    })
}

/// A two-byte register at `low`, `low + 1`: read high, low, high again, as
/// the chip may update it between the two bytes (as Linux's bq27x00 driver
/// does for this family); retried while the high byte moves.
fn word(registers: &impl Registers, low: u8) -> anyhow::Result<u16> {
    for _ in 0..3 {
        let high = registers.byte(low + 1)?;
        let value = registers.byte(low)?;
        if registers.byte(low + 1)? == high {
            return Ok(u16::from_le_bytes([value, high]));
        }
    }
    anyhow::bail!("gauge register {low:#04x} kept changing")
}

/// mAh (or mA) from 3.57 µV·h (µV) over the 20 mΩ sense resistor, in the
/// driver's integer arithmetic.
fn milli(raw: u16) -> u32 {
    u32::from(raw) * 357 / 2000
}

/// °F from 0.25 K, in the driver's integer arithmetic.
fn fahrenheit(raw: u16) -> i32 {
    let celsius = (i32::from(raw) + 2) / 4 - 273;
    celsius * 9 / 5 + 32
}

/// What the gauge says, in the driver's units: what its sysfs files would
/// print had it polled just now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reading {
    /// State of charge, % (`battery_capacity`).
    pub percent: u8,
    /// Remaining charge (`battery_mAH`).
    pub charge: MilliampHours,
    /// Average current, mA, negative on battery (`battery_current`).
    pub current: i32,
    /// The cell's temperature, °F (`battery_temperature`).
    pub temperature: i32,
}

/// Reads the chip now.
pub fn read(registers: &impl Registers) -> anyhow::Result<Reading> {
    let percent = registers.byte(CSOC)?.min(100);
    let charge = MilliampHours(milli(word(registers, CACT)?));
    let average = milli(word(registers, AI)?) as i32;
    let current = if registers.byte(FLAGS)? & CHARGING != 0 {
        average
    } else {
        -average
    };
    let temperature = fahrenheit(word(registers, TEMP)?);
    Ok(Reading {
        percent,
        charge,
        current,
        temperature,
    })
}

/// The driver's cached values, from its sysfs files in `dir`: as old as its
/// last poll. Each is `None` where its file can't be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cached {
    pub percent: Option<u8>,
    pub charge: Option<MilliampHours>,
    pub current: Option<i32>,
    pub temperature: Option<i32>,
}

impl Cached {
    pub fn read(dir: &Path) -> Self {
        let file = |name: &str| std::fs::read_to_string(dir.join(name)).ok();
        fn number<T: std::str::FromStr>(text: Option<String>) -> Option<T> {
            text?.trim().parse().ok()
        }
        Self {
            percent: file("battery_capacity").and_then(|text| parse_capacity(&text)),
            charge: number(file("battery_mAH")).map(MilliampHours),
            current: number(file("battery_current")),
            temperature: number(file("battery_temperature")),
        }
    }
}

impl From<Reading> for Cached {
    fn from(reading: Reading) -> Self {
        Self {
            percent: Some(reading.percent),
            charge: Some(reading.charge),
            current: Some(reading.current),
            temperature: Some(reading.temperature),
        }
    }
}

/// The driver's `battery_capacity` reads `71%`.
pub fn parse_capacity(text: &str) -> Option<u8> {
    let percent: u8 = text.trim().trim_end_matches('%').parse().ok()?;
    Some(percent.min(100))
}

/// Whether the last read of the chip failed, so a failure is logged once,
/// and so is the recovery.
static FAILING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The gauge now, from the chip; the driver's cached values if the chip
/// can't be read, with a log line when that starts and when it ends.
pub fn now() -> Cached {
    use std::sync::atomic::Ordering;
    match read(&Window::default()) {
        Ok(reading) => {
            if FAILING.swap(false, Ordering::Relaxed) {
                crate::log!("kindil: gauge: reading the chip again");
            }
            reading.into()
        }
        Err(error) => {
            if !FAILING.swap(true, Ordering::Relaxed) {
                crate::log!(
                    "kindil: gauge: reading the chip: {error:#}; \
                     using the driver's cached values"
                );
            }
            Cached::read(Path::new(DIR))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// Registers as `battreg_value` prints them, read in order; each entry
    /// of `changes` replaces a register's value after that many reads.
    struct Chip {
        values: RefCell<HashMap<u8, u8>>,
        reads: RefCell<Vec<u8>>,
        changes: Vec<(usize, u8, u8)>,
    }

    impl Chip {
        fn new(values: &[(u8, u8)]) -> Self {
            Self {
                values: RefCell::new(values.iter().copied().collect()),
                reads: RefCell::default(),
                changes: Vec::new(),
            }
        }
    }

    impl Registers for Chip {
        fn byte(&self, register: u8) -> anyhow::Result<u8> {
            let count = self.reads.borrow().len();
            for &(at, changed, value) in &self.changes {
                if at == count {
                    self.values.borrow_mut().insert(changed, value);
                }
            }
            self.reads.borrow_mut().push(register);
            let value = self.values.borrow().get(&register).copied();
            // As the driver prints it, through the parser.
            let text = match value {
                Some(value) => format!("Battery Register {register}\n\n Value: 0x{value:x}\n\n"),
                None => format!("Battery Register {register}\n\n Value: 0xfffffffb\n\n"),
            };
            parse_value(&text, register)
        }
    }

    #[test]
    fn parses_the_register_window() {
        let text = "Battery Register 44\n\n Value: 0x64\n\n";
        assert_eq!(parse_value(text, 0x2c).unwrap(), 100);
        assert!(parse_value(text, 0x2b).is_err(), "another register's");
        // -EIO, printed by %x.
        let failed = "Battery Register 44\n\n Value: 0xfffffffb\n\n";
        let error = parse_value(failed, 0x2c).unwrap_err().to_string();
        assert!(error.contains("error -5"), "{error}");
        assert!(parse_value("", 0x2c).is_err());
    }

    #[test]
    fn scales_as_the_driver_does() {
        // The emulator's chip (eink-emulator's imx50-yoshi.c): 25 °C,
        // CACT 3921, read by the PW1's driver as 77 °F and 699 mAh.
        assert_eq!(fahrenheit((25 + 273) * 4), 77);
        assert_eq!(milli(3921), 699);
        assert_eq!(fahrenheit(1191), 77, "24.75 °C rounds as the driver's");
        assert_eq!(fahrenheit(4 * 273), 32);
        assert_eq!(fahrenheit(4 * 263), 14, "below freezing");
    }

    #[test]
    fn reads_percent_charge_signed_current_and_temperature() {
        let regs = [
            (CSOC, 87),
            (CACT, 0x0f),
            (CACT + 1, 0x1c),
            (AI, 0x18),
            (AI + 1, 0x01),
            (FLAGS, 0x00),
            (TEMP, 0xa8),
            (TEMP + 1, 0x04),
        ];
        let reading = read(&Chip::new(&regs)).unwrap();
        assert_eq!(
            reading,
            Reading {
                percent: 87,
                charge: MilliampHours(0x1c0f * 357 / 2000),
                current: -(0x0118 * 357 / 2000),
                temperature: 77,
            }
        );
        let mut charging = regs;
        charging[5] = (FLAGS, CHARGING);
        assert_eq!(
            read(&Chip::new(&charging)).unwrap().current,
            0x0118 * 357 / 2000
        );
    }

    /// The PW1's chip on 2026-11-19 at 02:50:04 UTC, through `battreg_value`,
    /// a second after a doze wake: the driver's files still said 100%, 803
    /// mAh, +26 mA (its last poll, hours before); 20 s later it polled and
    /// said 97%, 782 mAh, 73 °F, as read here.
    #[test]
    fn reads_the_pw1s_chip_as_its_driver_does() {
        let chip = Chip::new(&[
            (0x06, 0xa1),
            (0x07, 0x04),
            (0x0a, 0x14),
            (0x10, 0x21),
            (0x11, 0x11),
            (0x14, 0xaa),
            (0x15, 0x02),
            (0x2c, 0x61),
        ]);
        assert_eq!(
            read(&chip).unwrap(),
            Reading {
                percent: 97,
                charge: MilliampHours(782),
                current: -121,
                temperature: 73,
            }
        );
        // As `battreg_value` printed it.
        let text = "Battery Register 44\n\n Value: 0x61\n\n";
        assert_eq!(parse_value(text, 0x2c).unwrap(), 97);
    }

    #[test]
    fn a_word_rolling_over_between_its_bytes_is_read_again() {
        let mut chip = Chip::new(&[(CACT, 0xff), (CACT + 1, 0x0a)]);
        // After the first high byte: the low byte rolls over into the high.
        chip.changes = vec![(1, CACT, 0x00), (1, CACT + 1, 0x0b)];
        assert_eq!(word(&chip, CACT).unwrap(), 0x0b00);
        assert_eq!(*chip.reads.borrow(), [0x11, 0x10, 0x11, 0x11, 0x10, 0x11]);
    }

    #[test]
    fn a_failed_register_fails_the_reading() {
        let chip = Chip::new(&[(CSOC, 50)]);
        assert!(read(&chip).is_err());
    }

    #[test]
    fn reads_the_drivers_capacity() {
        assert_eq!(parse_capacity("71%\n"), Some(71));
        assert_eq!(parse_capacity("100%"), Some(100));
        assert_eq!(parse_capacity("%"), None);
    }

    #[test]
    fn reads_the_drivers_cached_files() {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in [
            ("battery_capacity", "71%\n"),
            ("battery_mAH", "1003\n"),
            ("battery_current", "-12\n"),
        ] {
            std::fs::write(dir.path().join(name), text).unwrap();
        }
        assert_eq!(
            Cached::read(dir.path()),
            Cached {
                percent: Some(71),
                charge: Some(MilliampHours(1003)),
                current: Some(-12),
                temperature: None,
            }
        );
    }
}
