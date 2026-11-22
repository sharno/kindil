//! The PW1's PMIC charger (Freescale MC13892), seen and restarted through
//! the PMIC core's `/dev/pmic` (drivers/mxc/pmic/core/pmic-dev.c).
//!
//! The charger is a state machine in the PMIC (datasheet MC13892, "Battery
//! interface and control"): it stops by itself at the end of charge (the
//! charge current below its CHGCURR limit) and when its 120-minute charge
//! timer runs out, and starts again only when the battery falls below
//! BATTCYCL (98% of the charge voltage, with cycling on, as the driver
//! sets it), when the charger is unplugged and plugged in again, or when
//! software sets the self-clearing CHGRESTART bit. Setting the charge
//! current (ICHRG), which is all the charger driver's `charging` switch
//! does (aplite_charger.c `charger_enable`), restarts nothing: on
//! 2026-11-21 kindil's `0`, `1` at 95% left the cell discharging on the
//! charger three times, half an hour apart, until it sagged under
//! BATTCYCL at 92% and the PMIC restarted itself. The driver sets
//! CHGRESTART itself only on a plug-in and on a charge-timer fault.
//!
//! So kindil reads the state machine's CHGENS sense bit ("a 1 in all
//! states of the charger state machine, except when in a fault condition
//! or when at the end of charge") to know whether it charges, rather than
//! the sign of the gauge's average current, and sets CHGRESTART, as the
//! driver does on a plug-in, after switching the charge current back on.
use std::os::fd::AsRawFd;

pub const DEVICE: &str = "/dev/pmic";

/// The MC13892's registers (include/linux/pmic_external.h, `pmic_reg`).
const REG_INT_SENSE0: i32 = 2;
const REG_CHARGE: i32 = 48;
/// Register 2, bit 7: the charger state machine is in a charging state.
const CHGENS: u32 = 1 << 7;
/// Register 2, bit 6: a charger is detected.
const CHGDETS: u32 = 1 << 6;
/// Register 2, bits 8 and 9.
const CHGFAULTS_SHIFT: u32 = 8;
/// Register 48, bits 3 to 6: the charge current DAC.
const ICHRG_SHIFT: u32 = 3;
/// Register 48, bit 20: restarts the charger state machine; self-clearing.
const CHGRESTART: u32 = 1 << 20;

/// `register_info`: the register, its value, a mask the core ignores
/// (`PMIC_WRITE_REG` writes all 24 bits).
#[repr(C)]
struct RegisterInfo {
    reg: i32,
    value: u32,
    mask: u32,
}

/// `_IOWR('P', 0xa0, register_info*)`: the size is a pointer's, as the
/// header writes it, 4 on the PW1.
const PMIC_READ_REG: u32 = ioc_readwrite(0xa0);
/// `_IOWR('P', 0xa1, register_info*)`.
const PMIC_WRITE_REG: u32 = ioc_readwrite(0xa1);

const fn ioc_readwrite(nr: u32) -> u32 {
    (3 << 30) | (4 << 16) | ((b'P' as u32) << 8) | nr
}

/// What the PMIC says of its charger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Charger {
    /// CHGDETS: a charger is detected.
    pub detected: bool,
    /// CHGENS: the state machine is charging (not ended, not faulted).
    pub running: bool,
    /// CHGFAULTS[1:0]: 0 none, 1 charger fault, 2 timeout or battery
    /// drop, 3 battery temperature.
    pub fault: u8,
    /// ICHRG[3:0]: the charge current setting, 0 off.
    pub current_setting: u8,
}

impl Charger {
    /// From register 2 (sense) and register 48 (charger).
    pub fn decode(sense: u32, charge: u32) -> Self {
        Self {
            detected: sense & CHGDETS != 0,
            running: sense & CHGENS != 0,
            fault: ((sense >> CHGFAULTS_SHIFT) & 0b11) as u8,
            current_setting: ((charge >> ICHRG_SHIFT) & 0b1111) as u8,
        }
    }
}

impl std::fmt::Display for Charger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "PMIC charger {}, current setting {}{}",
            if self.running { "charging" } else { "stopped" },
            self.current_setting,
            match self.fault {
                0 => "",
                1 => ", fault: charger",
                2 => ", fault: timer or battery drop",
                _ => ", fault: battery temperature",
            }
        )
    }
}

/// The PMIC core's device.
pub struct Pmic {
    file: std::fs::File,
}

impl Pmic {
    pub fn open() -> std::io::Result<Self> {
        Ok(Self {
            file: std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(DEVICE)?,
        })
    }

    fn ioctl(&self, request: u32, info: &mut RegisterInfo) -> std::io::Result<()> {
        // SAFETY: `info` is the core's `register_info`, alive across the call.
        let r = unsafe {
            libc::ioctl(
                self.file.as_raw_fd(),
                request as _,
                info as *mut RegisterInfo,
            )
        };
        if r < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    fn read(&self, reg: i32) -> std::io::Result<u32> {
        let mut info = RegisterInfo {
            reg,
            value: 0,
            mask: 0,
        };
        self.ioctl(PMIC_READ_REG, &mut info)?;
        Ok(info.value)
    }

    pub fn charger(&self) -> std::io::Result<Charger> {
        Ok(Charger::decode(
            self.read(REG_INT_SENSE0)?,
            self.read(REG_CHARGE)?,
        ))
    }

    /// Sets CHGRESTART, as the charger driver does on a plug-in. The core
    /// writes the whole register, so it is read first and written back
    /// with the bit; read again after, to see that the rest stayed.
    ///
    /// Between the read and the write, the driver could change register 48
    /// (it does on a plug event, a charge-timer fault, or a write to its
    /// switch); kindil writes the switch itself under the same lock as
    /// this, so only the first two could be lost, and both set the charge
    /// current kindil has just set too.
    pub fn restart(&self) -> std::io::Result<Charger> {
        let charge = self.read(REG_CHARGE)?;
        let mut info = RegisterInfo {
            reg: REG_CHARGE,
            value: (charge | CHGRESTART) & 0x00ff_ffff,
            mask: 0x00ff_ffff,
        };
        self.ioctl(PMIC_WRITE_REG, &mut info)?;
        let after = self.read(REG_CHARGE)?;
        if after & !CHGRESTART != charge & !CHGRESTART {
            return Err(std::io::Error::other(format!(
                "register 48 was {charge:#08x} and is {after:#08x}"
            )));
        }
        self.charger()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ioctls_are_the_pmic_cores() {
        assert_eq!(PMIC_READ_REG, 0xC004_50A0);
        assert_eq!(PMIC_WRITE_REG, 0xC004_50A1);
        assert_eq!(std::mem::size_of::<RegisterInfo>(), 12);
    }

    #[test]
    fn decodes_the_sense_and_charger_registers() {
        // Charger in, charging, 500 mA (ICHRG 5), VCHRG 3, CHGRESTART read
        // back as 0.
        let charging = Charger::decode(CHGDETS | CHGENS, (5 << 3) | 0b011);
        assert_eq!(
            charging,
            Charger {
                detected: true,
                running: true,
                fault: 0,
                current_setting: 5,
            }
        );
        // Timed out: stopped, CHGFAULTS 10.
        let timed_out = Charger::decode(CHGDETS | (0b10 << 8), 5 << 3);
        assert!(!timed_out.running);
        assert_eq!(timed_out.fault, 2);
        assert_eq!(
            timed_out.to_string(),
            "PMIC charger stopped, current setting 5, fault: timer or battery drop"
        );
        // The switch at 0: no current setting.
        assert_eq!(Charger::decode(CHGDETS, 0).current_setting, 0);
    }
}
