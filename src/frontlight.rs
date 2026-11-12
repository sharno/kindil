//! Frontlight control.
//!
//! Amazon's Kindle kernels expose the frontlight as a character device driven
//! with the ioctls in `<linux/frontlight.h>` (FBInk vendors the same header as
//! `eink/frontlight-kindil.h`). There is no sysfs `brightness` node on these
//! devices, so the ioctl interface is the only way in.
//!
//! On the Paperwhite 1 the driver is `drivers/misc/yoshime_fl.c`, which
//! registers a misc device named `yoshime_fl` (minor 162) - so the node is
//! `/dev/yoshime_fl`, as `include/linux/yoshime_fl.h` spells out. The same
//! driver also exposes a read/write `fl_intensity` attribute under the
//! `fl_tps6116x` sysfs class, which we use as a fallback.
//!
//! The Paperwhite 1 is the first Kindle with a frontlight; earlier models have
//! none.

use crate::sys::{self, ioc};
use core::ffi::c_int;

/// The ioctl type character used by the frontlight driver.
pub const FL_MAGIC_NUMBER: core::ffi::c_uint = b'L' as core::ffi::c_uint;

/// The frontlight device node on the Paperwhite 1.
pub const FL_DEV_FILE: &str = "/dev/yoshime_fl";

/// The sysfs class the same driver registers.
pub const FL_SYSFS_CLASS: &str = "/sys/class/fl_tps6116x";

/// The sysfs attribute holding the intensity.
pub const FL_SYSFS_INTENSITY: &str = "/sys/class/fl_tps6116x/fl_intensity";

/// `_IOW('L', 0x01, int)`: set the intensity.
pub const FL_IOCTL_SET_INTENSITY: sys::IoctlRequest = ioc::iow(FL_MAGIC_NUMBER, 0x01, 4);
/// `_IOR('L', 0x02, int)`: get the current intensity.
pub const FL_IOCTL_GET_INTENSITY: sys::IoctlRequest = ioc::ior(FL_MAGIC_NUMBER, 0x02, 4);
/// `_IOR('L', 0x03, int)`: get the maximum intensity.
pub const FL_IOCTL_GET_RANGE_MAX: sys::IoctlRequest = ioc::ior(FL_MAGIC_NUMBER, 0x03, 4);

/// The frontlight, if it is available.
pub struct Frontlight {
    fd: c_int,
    max: i32,
}

impl Frontlight {
    /// Open the frontlight device.
    ///
    /// # Errors
    ///
    /// Fails if `/dev/frontlight` cannot be opened.
    pub fn open() -> Result<Self, Error> {
        let fd = open_device(FL_DEV_FILE)?;
        let mut fl = Frontlight { fd, max: 0 };
        fl.max = fl.max_intensity().unwrap_or(0);
        Ok(fl)
    }

    /// The current intensity, in the driver's own units.
    pub fn intensity(&self) -> Result<i32, Error> {
        let mut value: c_int = 0;
        // SAFETY: `value` is a live `int`, which is what the ioctl writes to.
        let rc = unsafe { sys::ioctl(self.fd, FL_IOCTL_GET_INTENSITY, &mut value) };
        if rc < 0 {
            return Err(Error::Ioctl {
                what: "FL_IOCTL_GET_INTENSITY",
                errno: sys::errno(),
            });
        }
        Ok(value)
    }

    /// The maximum intensity the panel supports.
    pub fn max_intensity(&self) -> Result<i32, Error> {
        let mut value: c_int = 0;
        // SAFETY: as above.
        let rc = unsafe { sys::ioctl(self.fd, FL_IOCTL_GET_RANGE_MAX, &mut value) };
        if rc < 0 {
            return Err(Error::Ioctl {
                what: "FL_IOCTL_GET_RANGE_MAX",
                errno: sys::errno(),
            });
        }
        Ok(value)
    }

    /// Set the intensity.
    ///
    /// The value is clamped to `0..=max`.
    pub fn set_intensity(&self, value: i32) -> Result<(), Error> {
        let value = value.clamp(0, self.max);
        // SAFETY: `value` is an `int` passed by pointer, matching the ioctl.
        let rc = unsafe { sys::ioctl(self.fd, FL_IOCTL_SET_INTENSITY, value) };
        if rc < 0 {
            return Err(Error::Ioctl {
                what: "FL_IOCTL_SET_INTENSITY",
                errno: sys::errno(),
            });
        }
        Ok(())
    }
}

/// Read the frontlight intensity from sysfs, if the device is present.
///
/// The same `yoshime_fl` driver exposes a read/write `fl_intensity` attribute
/// under its `fl_tps6116x` sysfs class.
pub fn sysfs_intensity() -> Option<i32> {
    let mut buf = [0u8; 32];
    let n = sys::read_file(FL_SYSFS_INTENSITY, &mut buf);
    if n == 0 {
        return None;
    }
    let text = String::from_utf8_lossy(&buf[..n]);
    text.trim().parse::<i32>().ok()
}

impl Drop for Frontlight {
    fn drop(&mut self) {
        // SAFETY: `fd` is owned by this value and closed exactly once.
        unsafe {
            sys::close(self.fd);
        }
    }
}

fn open_device(path: &str) -> Result<c_int, Error> {
    let mut bytes = path.as_bytes().to_vec();
    bytes.push(0);
    // SAFETY: `bytes` is a NUL-terminated C string that outlives the call.
    let fd = unsafe {
        sys::open(
            bytes.as_ptr() as *const core::ffi::c_char,
            sys::O_RDWR | sys::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(Error::Open {
            path: path.to_string(),
            errno: sys::errno(),
        });
    }
    Ok(fd)
}

/// Errors from the frontlight layer.
#[derive(Debug)]
pub enum Error {
    /// The device could not be opened.
    Open { path: String, errno: i32 },
    /// An ioctl failed.
    Ioctl { what: &'static str, errno: i32 },
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Open { path, errno } => {
                write!(
                    f,
                    "open({path}) failed: {} ({errno})",
                    sys::strerror(*errno)
                )
            }
            Error::Ioctl { what, errno } => {
                write!(f, "{what} failed: {} ({errno})", sys::strerror(*errno))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_numbers_match_amazon_header() {
        // From Amazon's <linux/frontlight.h>:
        //   _IOW('L', 0x01, int), _IOR('L', 0x02, int), _IOR('L', 0x03, int)
        assert_eq!(FL_IOCTL_SET_INTENSITY, 0x4004_4c01);
        assert_eq!(FL_IOCTL_GET_INTENSITY, 0x8004_4c02);
        assert_eq!(FL_IOCTL_GET_RANGE_MAX, 0x8004_4c03);
    }
}
