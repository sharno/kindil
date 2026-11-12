//! Touch input via evdev.
//!
//! The Paperwhite 1's touch controller is a Cypress `cyttsp` on I2C, driven by
//! the lab126 `drivers/input/touchscreen/cyttsp.c`. It reports **protocol A**
//! multitouch: `ABS_MT_POSITION_X`/`ABS_MT_POSITION_Y` (mirrored by
//! `ABS_X`/`ABS_Y`) carry the coordinates, and `BTN_TOUCH` marks contact and
//! release. We track a single contact, which is all a tap-driven shell needs.
//!
//! # The layout that bit us
//!
//! `struct input_event` embeds a `struct timeval`, whose `time_t`/`suseconds_t`
//! are 4 bytes on the ARM32 target and 8 on the x86_64 host. A struct spelled
//! with `i64` therefore compiles and tests fine on the host at 24 bytes, while
//! the device's kernel writes **16 bytes per event**. Every `read` then returns
//! a short count and the old code discarded the event: touch was dead on
//! hardware and the host tests still passed.
//!
//! The fields are `c_long` here, so the struct is 16 bytes on ARM32 and 24 on
//! the host, matching each C library's `struct input_event` exactly.

use crate::sys::{self, ioc, PollFd};
use core::ffi::{c_int, c_long};

/// `EV_SYN`
pub const EV_SYN: u16 = 0x00;
/// `EV_KEY`
pub const EV_KEY: u16 = 0x01;
/// `EV_ABS`
pub const EV_ABS: u16 = 0x03;

/// `ABS_X`
pub const ABS_X: u16 = 0x00;
/// `ABS_Y`
pub const ABS_Y: u16 = 0x01;
/// `ABS_MT_POSITION_X`
pub const ABS_MT_POSITION_X: u16 = 0x35;
/// `ABS_MT_POSITION_Y`
pub const ABS_MT_POSITION_Y: u16 = 0x36;
/// `BTN_TOUCH`
pub const BTN_TOUCH: u16 = 0x14a;

/// `EVIOCGVERSION`
pub const EVIOCGVERSION: sys::IoctlRequest = ioc::ior(b'E' as _, 0x01, 4);
/// `EVIOCGID`
pub const EVIOCGID: sys::IoctlRequest = ioc::ior(b'E' as _, 0x02, 8);

/// `EVIOCGNAME(len)` for a 128-byte buffer.
pub const fn eviocgname(len: u32) -> sys::IoctlRequest {
    ioc::ior(b'E' as _, 0x06, len)
}

/// `EVIOCGBIT(ev, len)`: the bitmap of codes the device supports for `ev`.
pub const fn eviocgbit(ev: u16, len: u32) -> sys::IoctlRequest {
    ioc::ior(b'E' as _, 0x20 + ev as u32, len)
}

/// The `input_event` structure, exactly as the kernel defines it.
///
/// `c_long` for the timeval fields is load-bearing: 4 bytes each on ARM32.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct InputEvent {
    pub time_sec: c_long,
    pub time_usec: c_long,
    pub kind: u16,
    pub code: u16,
    pub value: i32,
}

/// A touch event, in panel pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchEvent {
    /// The finger went down at `(x, y)`.
    Press { x: i32, y: i32 },
    /// The finger lifted at `(x, y)`.
    Release { x: i32, y: i32 },
    /// The finger moved to `(x, y)` while down.
    Move { x: i32, y: i32 },
}

/// The protocol-A state machine, independent of any file descriptor.
///
/// Kept separate from [`Touch`] so it can be tested without a device.
#[derive(Default)]
pub struct Decoder {
    x: i32,
    y: i32,
    down: bool,
}

impl Decoder {
    /// Feed one evdev event, returning a touch event if it completes one.
    pub fn feed(&mut self, kind: u16, code: u16, value: i32) -> Option<TouchEvent> {
        match kind {
            EV_ABS => match code {
                ABS_X | ABS_MT_POSITION_X => {
                    self.x = value;
                    self.moved()
                }
                ABS_Y | ABS_MT_POSITION_Y => {
                    self.y = value;
                    self.moved()
                }
                _ => None,
            },
            EV_KEY if code == BTN_TOUCH => {
                if value != 0 {
                    self.down = true;
                    Some(TouchEvent::Press {
                        x: self.x,
                        y: self.y,
                    })
                } else {
                    self.down = false;
                    Some(TouchEvent::Release {
                        x: self.x,
                        y: self.y,
                    })
                }
            }
            _ => None,
        }
    }

    fn moved(&self) -> Option<TouchEvent> {
        if self.down {
            Some(TouchEvent::Move {
                x: self.x,
                y: self.y,
            })
        } else {
            None
        }
    }
}

/// An open evdev touch device.
pub struct Touch {
    fd: c_int,
    decoder: Decoder,
}

/// The default evdev path, used when no scan succeeds.
pub const DEFAULT_TOUCH: &str = "/dev/input/event0";

impl Touch {
    /// Open an evdev device by path.
    ///
    /// # Errors
    ///
    /// Fails if the device cannot be opened.
    pub fn open(path: &str) -> Result<Self, Error> {
        Ok(Touch {
            fd: open_read(path)?,
            decoder: Decoder::default(),
        })
    }

    /// Open the touchscreen, scanning `/dev/input/event*`.
    ///
    /// There is no udev in the initramfs and probe order is not guaranteed, so
    /// picking `event0` blindly can select the power button instead. Each
    /// candidate is checked for `BTN_TOUCH` and an absolute X axis.
    pub fn open_default() -> Result<Self, Error> {
        let mut last = Error::Open {
            path: DEFAULT_TOUCH.to_string(),
            errno: sys::ENOENT,
        };
        for index in 0..16 {
            let path = std::format!("/dev/input/event{index}");
            match Touch::open(&path) {
                Ok(touch) => {
                    if touch.is_touchscreen() {
                        return Ok(touch);
                    }
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// The raw file descriptor, for `poll`.
    pub fn fd(&self) -> c_int {
        self.fd
    }

    /// The device name reported by `EVIOCGNAME`.
    pub fn name(&self) -> Option<String> {
        let mut buf = [0u8; 128];
        // SAFETY: `buf` is a live 128-byte buffer, and the request number
        // encodes that same length.
        let rc = unsafe { sys::ioctl(self.fd, eviocgname(buf.len() as u32), buf.as_mut_ptr()) };
        if rc < 0 {
            return None;
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        Some(String::from_utf8_lossy(&buf[..end]).into_owned())
    }

    /// True if the device reports `BTN_TOUCH` and `ABS_X`.
    fn is_touchscreen(&self) -> bool {
        self.has_bit(EV_KEY, BTN_TOUCH) && self.has_bit(EV_ABS, ABS_X)
    }

    fn has_bit(&self, kind: u16, code: u16) -> bool {
        let mut bits = [0u8; 96];
        // SAFETY: `bits` outlives the call and the request encodes its length.
        let rc = unsafe {
            sys::ioctl(
                self.fd,
                eviocgbit(kind, bits.len() as u32),
                bits.as_mut_ptr(),
            )
        };
        if rc < 0 {
            return false;
        }
        let byte = (code / 8) as usize;
        let bit = (code % 8) as u8;
        byte < bits.len() && bits[byte] & (1 << bit) != 0
    }

    /// Block for up to `timeout_ms` and return the next touch event.
    ///
    /// Returns `Ok(None)` on timeout or when the event was not a touch event
    /// (a `SYN_REPORT`, for instance).
    ///
    /// # Errors
    ///
    /// Fails if `poll` or `read` fail.
    pub fn read_event(&mut self, timeout_ms: i32) -> Result<Option<TouchEvent>, Error> {
        let mut fds = PollFd {
            fd: self.fd,
            events: sys::POLLIN,
            revents: 0,
        };
        // SAFETY: `fds` is a live single-element array; `nfds` is a `c_ulong`.
        let rc = unsafe { sys::poll(&mut fds, 1, timeout_ms) };
        if rc < 0 {
            return Err(Error::Poll {
                errno: sys::errno(),
            });
        }
        if rc == 0 {
            return Ok(None);
        }

        let mut ev = InputEvent::default();
        let size = core::mem::size_of::<InputEvent>();
        // SAFETY: `ev` is a live `input_event` of exactly the size the kernel
        // writes for this architecture.
        let n = unsafe { sys::read(self.fd, &mut ev as *mut InputEvent as *mut _, size) };
        if n < 0 {
            return Err(Error::Read {
                errno: sys::errno(),
            });
        }
        if n != size as isize {
            // Short read: not an evdev node, or the device went away.
            return Ok(None);
        }

        Ok(self.decoder.feed(ev.kind, ev.code, ev.value))
    }
}

fn open_read(path: &str) -> Result<c_int, Error> {
    let c_path = sys::to_cstring(path).ok_or_else(|| Error::Open {
        path: path.to_string(),
        errno: sys::EINVAL,
    })?;
    // SAFETY: `c_path` is a NUL-terminated C string that outlives the call.
    let fd = unsafe { sys::open(c_path.as_ptr() as *const core::ffi::c_char, sys::O_RDONLY) };
    if fd < 0 {
        return Err(Error::Open {
            path: path.to_string(),
            errno: sys::errno(),
        });
    }
    Ok(fd)
}

impl Drop for Touch {
    fn drop(&mut self) {
        // SAFETY: `fd` is owned by this value and closed exactly once.
        unsafe {
            sys::close(self.fd);
        }
    }
}

/// Errors from the input layer.
#[derive(Debug)]
pub enum Error {
    /// The device could not be opened.
    Open { path: String, errno: i32 },
    /// `poll` failed.
    Poll { errno: i32 },
    /// `read` failed.
    Read { errno: i32 },
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
            Error::Poll { errno } => write!(f, "poll failed: {} ({errno})", sys::strerror(*errno)),
            Error::Read { errno } => write!(f, "read failed: {} ({errno})", sys::strerror(*errno)),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_event_matches_the_libc_struct_size() {
        // 16 bytes on ARM32 (`long` is 4), 24 on x86_64 (`long` is 8).
        let expected = 2 * core::mem::size_of::<c_long>() + 8;
        assert_eq!(core::mem::size_of::<InputEvent>(), expected);
        #[cfg(target_pointer_width = "32")]
        assert_eq!(core::mem::size_of::<InputEvent>(), 16);
        #[cfg(target_pointer_width = "64")]
        assert_eq!(core::mem::size_of::<InputEvent>(), 24);
    }

    #[test]
    fn press_move_release() {
        let mut d = Decoder::default();
        // Protocol A: coordinates arrive before BTN_TOUCH.
        assert_eq!(d.feed(EV_ABS, ABS_X, 400), None);
        assert_eq!(d.feed(EV_ABS, ABS_Y, 300), None);
        assert_eq!(
            d.feed(EV_KEY, BTN_TOUCH, 1),
            Some(TouchEvent::Press { x: 400, y: 300 })
        );
        assert_eq!(
            d.feed(EV_ABS, ABS_MT_POSITION_X, 410),
            Some(TouchEvent::Move { x: 410, y: 300 })
        );
        assert_eq!(
            d.feed(EV_KEY, BTN_TOUCH, 0),
            Some(TouchEvent::Release { x: 410, y: 300 })
        );
        // A move after release is not reported.
        assert_eq!(d.feed(EV_ABS, ABS_X, 1), None);
    }

    #[test]
    fn syn_and_unknown_events_are_ignored() {
        let mut d = Decoder::default();
        assert_eq!(d.feed(EV_SYN, 0, 0), None);
        assert_eq!(d.feed(EV_KEY, 0x74, 1), None); // KEY_POWER
    }

    #[test]
    fn poll_waits_for_the_timeout() {
        // A negative fd is ignored by poll, so this is a pure sleep. If the
        // `nfds` argument were passed as a 64-bit value instead of `c_ulong`,
        // the timeout would land in the wrong register on ARM32 and poll would
        // return immediately.
        let mut fds = PollFd {
            fd: -1,
            events: sys::POLLIN,
            revents: 0,
        };
        let start = std::time::Instant::now();
        // SAFETY: `fds` is a live single-element array for the call.
        let rc = unsafe { sys::poll(&mut fds, 1, 60) };
        let elapsed = start.elapsed();
        assert_eq!(rc, 0);
        assert!(
            elapsed.as_millis() >= 45,
            "poll returned after {elapsed:?}: the timeout was not passed correctly"
        );
    }
}
