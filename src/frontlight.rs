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
    max: i32,
}
impl Frontlight {
    pub fn open() -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/yoshime_fl")?;
        let mut max = 0i32;
        let rc = unsafe {
            libc::ioctl(
                file.as_raw_fd(),
                libc::_IOR::<i32>(b'L' as _, 3) as _,
                &mut max,
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        if max <= 0 {
            return Err(io::Error::other(
                "invalid frontlight range; control disabled",
            ));
        }
        Ok(Self { file, max })
    }
    pub fn percent(&self) -> io::Result<f32> {
        let mut value = 0i32;
        let rc = unsafe {
            libc::ioctl(
                self.file.as_raw_fd(),
                libc::_IOR::<i32>(b'L' as _, 2) as _,
                &mut value,
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        if !(0..=self.max).contains(&value) {
            return Err(io::Error::other("out-of-range driver brightness"));
        }
        Ok(value as f32 * 100.0 / self.max as f32)
    }
    pub fn set_percent(&self, percent: f32) -> io::Result<()> {
        let value = level(percent, self.max)?;
        let rc = unsafe {
            libc::ioctl(
                self.file.as_raw_fd(),
                libc::_IOW::<i32>(b'L' as _, 1) as _,
                &value,
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}
fn level(percent: f32, max: i32) -> io::Result<i32> {
    if !percent.is_finite() || !(0.0..=100.0).contains(&percent) || max <= 0 {
        return Err(io::Error::other("invalid brightness"));
    }
    Ok((f64::from(percent) * f64::from(max) / 100.0).round() as i32)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn values_never_exceed_driver_limits() {
        assert_eq!(level(0.0, 255).unwrap(), 0);
        assert_eq!(level(100.0, 255).unwrap(), 255);
        assert_eq!(level(50.0, 255).unwrap(), 128);
        assert!(level(f32::NAN, 255).is_err());
        assert!(level(-1.0, 255).is_err());
        assert!(level(101.0, 255).is_err());
    }
}
