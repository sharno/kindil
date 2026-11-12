//! Minimal PW1-only frontlight adapter. Linux owns PWM and electrical limits.
//! ABI: Amazon include/linux/yoshime_fl.h and drivers/misc/yoshime_fl.c.
//! All three ioctl handlers use get_user/put_user with an int pointer.
use std::{
    fs::{File, OpenOptions},
    io,
    os::fd::AsRawFd,
};

pub struct Frontlight {
    file: File,
    /// Driver maximum, read once; always positive.
    max: i32,
}

const GET_MAX: u8 = 3;
const GET: u8 = 2;
const SET: u8 = 1;

impl Frontlight {
    pub fn open() -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/yoshime_fl")?;
        let max = read(&file, GET_MAX)?;
        if max <= 0 {
            return Err(io::Error::other("invalid frontlight range"));
        }
        Ok(Self { file, max })
    }

    pub fn percent(&self) -> io::Result<u8> {
        let value = read(&self.file, GET)?;
        if !(0..=self.max).contains(&value) {
            return Err(io::Error::other("out-of-range driver brightness"));
        }
        Ok(to_percent(value, self.max))
    }

    pub fn set_percent(&self, percent: u8) -> io::Result<()> {
        let value = to_level(percent, self.max);
        // SAFETY: the driver reads one int through this pointer.
        let rc = unsafe {
            libc::ioctl(
                self.file.as_raw_fd(),
                libc::_IOW::<i32>(b'L' as _, SET as _) as _,
                &value,
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

fn read(file: &File, command: u8) -> io::Result<i32> {
    let mut value = 0i32;
    // SAFETY: the driver writes one int through this pointer.
    let rc = unsafe {
        libc::ioctl(
            file.as_raw_fd(),
            libc::_IOR::<i32>(b'L' as _, command as _) as _,
            &mut value,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(value)
}

fn to_level(percent: u8, max: i32) -> i32 {
    (i32::from(percent.min(100)) * max + 50) / 100
}

fn to_percent(level: i32, max: i32) -> u8 {
    ((level * 100 + max / 2) / max) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn values_never_exceed_driver_limits() {
        assert_eq!(to_level(0, 24), 0);
        assert_eq!(to_level(100, 24), 24);
        assert_eq!(to_level(250, 24), 24);
        assert_eq!(to_level(50, 255), 128);
        assert_eq!(to_percent(24, 24), 100);
        assert_eq!(to_percent(12, 24), 50);
    }
}
