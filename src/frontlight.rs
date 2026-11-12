//! Frontlight control.
//!
//! On the Paperwhite 1 the driver is `drivers/misc/yoshime_fl.c`, which
//! registers a misc device named `yoshime_fl` (minor 162) - so the node is
//! `/dev/yoshime_fl`, as `include/linux/yoshime_fl.h` spells out. The
//! Paperwhite 1 is the first Kindle with a frontlight; earlier models have none.
//!
//! All three ioctls take a **pointer to an `int`**: the handler does
//! `get_user(val, argp)` / `put_user(val, argp)`. Passing the value directly
//! looks plausible and always fails with `EFAULT`.

use crate::sys::{self, ioc};
use core::ffi::c_int;

/// The ioctl type character used by the frontlight driver.
pub const FL_MAGIC_NUMBER: core::ffi::c_uint = b'L' as core::ffi::c_uint;

/// The frontlight device node on the Paperwhite 1.
pub const FL_DEV_FILE: &str = "/dev/yoshime_fl";

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
    /// Fails if `/dev/yoshime_fl` cannot be opened.
    pub fn open() -> Result<Self, Error> {
        let c_path = sys::to_cstring(FL_DEV_FILE).expect("constant path has no NUL");
        // SAFETY: `c_path` is a NUL-terminated C string that outlives the call.
        let fd = unsafe { sys::open(c_path.as_ptr() as *const core::ffi::c_char, sys::O_RDWR) };
        if fd < 0 {
            return Err(Error::Open {
                path: FL_DEV_FILE.to_string(),
                errno: sys::errno(),
            });
        }
        let mut fl = Frontlight { fd, max: 0 };
        fl.max = fl.max_intensity().unwrap_or(0);
        Ok(fl)
    }

    /// The current intensity, in the driver's own units.
    pub fn intensity(&self) -> Result<i32, Error> {
        let mut value: c_int = 0;
        // SAFETY: `value` is a live `int`, and the handler does `put_user` into
        // the pointer we pass.
        let rc = unsafe { sys::ioctl(self.fd, FL_IOCTL_GET_INTENSITY, &mut value as *mut c_int) };
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
        let rc = unsafe { sys::ioctl(self.fd, FL_IOCTL_GET_RANGE_MAX, &mut value as *mut c_int) };
        if rc < 0 {
            return Err(Error::Ioctl {
                what: "FL_IOCTL_GET_RANGE_MAX",
                errno: sys::errno(),
            });
        }
        Ok(value)
    }

    /// Set the intensity, clamped to `0..=max`.
    pub fn set_intensity(&self, value: i32) -> Result<(), Error> {
        let value = if self.max > 0 {
            value.clamp(0, self.max)
        } else {
            value.max(0)
        };
        // SAFETY: the handler does `get_user` from the pointer we pass, so it
        // must point at a live `int` - not be the value itself.
        let rc = unsafe { sys::ioctl(self.fd, FL_IOCTL_SET_INTENSITY, &value as *const c_int) };
        if rc < 0 {
            return Err(Error::Ioctl {
                what: "FL_IOCTL_SET_INTENSITY",
                errno: sys::errno(),
            });
        }
        Ok(())
    }
}

impl Drop for Frontlight {
    fn drop(&mut self) {
        // SAFETY: `fd` is owned by this value and closed exactly once.
        unsafe {
            sys::close(self.fd);
        }
    }
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

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_numbers_match_the_yoshime_driver() {
        // From Amazon's <linux/frontlight.h>.
        assert_eq!(FL_IOCTL_SET_INTENSITY, 0x4004_4c01);
        assert_eq!(FL_IOCTL_GET_INTENSITY, 0x8004_4c02);
        assert_eq!(FL_IOCTL_GET_RANGE_MAX, 0x8004_4c03);
    }
}
