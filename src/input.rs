//! Touch input via evdev.
//!
//! The Paperwhite 1's touch controller is a Cypress `cyttsp` on I2C, driven by
//! the lab126 `drivers/input/touchscreen/cyttsp.c`. That driver reports
//! **protocol A** multitouch: `ABS_MT_POSITION_X`/`ABS_MT_POSITION_Y` carry
//! the coordinates (mirrored by `ABS_X`/`ABS_Y`), `BTN_TOUCH` marks contact
//! and release, and `ABS_MT_SLOT` selects the finger.
//!
//! We therefore track a single primary contact, which is all a shell needs:
//! read `ABS_X`/`ABS_Y` while `BTN_TOUCH` is down and report the release
//! position as a tap.

use crate::sys::{self, PollFd};
use core::ffi::c_int;

/// `EV_ABS`
pub const EV_ABS: u16 = 0x03;
/// `EV_KEY`
pub const EV_KEY: u16 = 0x01;
/// `EV_SYN`
pub const EV_SYN: u16 = 0x00;

/// `ABS_X`
pub const ABS_X: u16 = 0x00;
/// `ABS_Y`
pub const ABS_Y: u16 = 0x01;
/// `ABS_MT_SLOT`
pub const ABS_MT_SLOT: u16 = 0x2f;
/// `ABS_MT_POSITION_X`
pub const ABS_MT_POSITION_X: u16 = 0x35;
/// `ABS_MT_POSITION_Y`
pub const ABS_MT_POSITION_Y: u16 = 0x36;
/// `BTN_TOUCH`
pub const BTN_TOUCH: u16 = 0x14a;

/// `input_event` type.
pub const EV_MAX: usize = 0x1f;

/// The `input_event` structure, as the kernel defines it.
///
/// On 32-bit ARM `struct timeval` is two 32-bit words, so the layout matches
/// the 64-bit host's `tv_sec`/`tv_usec` pair of `long`s only by accident.
/// We therefore spell the fields out explicitly.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct InputEvent {
    pub time_sec: i64,
    pub time_usec: i64,
    pub kind: u16,
    pub code: u16,
    pub value: i32,
}

/// The `input_id` structure returned by `EVIOCGID`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct InputId {
    pub bustype: u16,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
}

/// `EVIOCGVERSION`
pub const EVIOCGVERSION: sys::IoctlRequest = 0x8008_4501;
/// `EVIOCGID`
pub const EVIOCGID: sys::IoctlRequest = 0x8008_4502;
/// `EVIOCGNAME`
pub const EVIOCGNAME: sys::IoctlRequest = 0x8008_4506;

/// A touch event.
///
/// The coordinates are the raw evdev values. On the Paperwhite 1 the `cyttsp`
/// driver reports them in the framebuffer's own coordinate space - the panel
/// is 758x1024 portrait and the touch axes are X 0..758, Y 0..1024 - so no
/// rescaling or axis swap is needed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchEvent {
    /// The finger went down at `(x, y)`, in panel pixels.
    Press { x: i32, y: i32 },
    /// The finger lifted at `(x, y)`.
    Release { x: i32, y: i32 },
    /// The finger moved to `(x, y)` while down.
    Move { x: i32, y: i32 },
}

/// An evdev touch device.
pub struct Touch {
    fd: c_int,
    x: i32,
    y: i32,
    down: bool,
}

impl Touch {
    /// Open an evdev device by path.
    ///
    /// # Errors
    ///
    /// Fails if the device cannot be opened.
    pub fn open(path: &str) -> Result<Self, Error> {
        let fd = open_read(path)?;
        Ok(Touch {
            fd,
            x: 0,
            y: 0,
            down: false,
        })
    }

    /// The raw file descriptor, for `poll`.
    pub fn fd(&self) -> c_int {
        self.fd
    }

    /// The device name reported by `EVIOCGNAME`, if available.
    pub fn name(&self) -> Option<String> {
        let mut buf = [0u8; 128];
        // SAFETY: `buf` is a live buffer of the size the ioctl expects.
        let rc = unsafe { sys::ioctl(self.fd, EVIOCGNAME, buf.as_mut_ptr()) };
        if rc < 0 {
            return None;
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        Some(String::from_utf8_lossy(&buf[..end]).into_owned())
    }

    /// Block for up to `timeout_ms` and return the next touch event.
    ///
    /// Returns `Ok(None)` on timeout.
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
        // SAFETY: `fds` is a live, single-element array for the call.
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
        // SAFETY: `ev` is a live `input_event`; the driver writes exactly its
        // size into it.
        let n = unsafe {
            sys::read(
                self.fd,
                &mut ev as *mut InputEvent as *mut core::ffi::c_void,
                core::mem::size_of::<InputEvent>(),
            )
        };
        if n < 0 {
            return Err(Error::Read {
                errno: sys::errno(),
            });
        }
        if n != core::mem::size_of::<InputEvent>() as isize {
            // Short read: the device was closed or is not an evdev node.
            return Ok(None);
        }

        Ok(self.translate(&ev))
    }

    /// Turn a raw evdev event into a [`TouchEvent`].
    fn translate(&mut self, ev: &InputEvent) -> Option<TouchEvent> {
        match ev.kind {
            EV_ABS => match ev.code {
                ABS_X | ABS_MT_POSITION_X => {
                    self.x = ev.value;
                    if self.down {
                        Some(TouchEvent::Move {
                            x: self.x,
                            y: self.y,
                        })
                    } else {
                        None
                    }
                }
                ABS_Y | ABS_MT_POSITION_Y => {
                    self.y = ev.value;
                    if self.down {
                        Some(TouchEvent::Move {
                            x: self.x,
                            y: self.y,
                        })
                    } else {
                        None
                    }
                }
                _ => None,
            },
            EV_KEY if ev.code == BTN_TOUCH => {
                if ev.value != 0 {
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
}

fn open_read(path: &str) -> Result<c_int, Error> {
    let mut bytes = path.as_bytes().to_vec();
    bytes.push(0);
    // SAFETY: `bytes` is a NUL-terminated C string that outlives the call.
    let fd = unsafe {
        sys::open(
            bytes.as_ptr() as *const core::ffi::c_char,
            sys::O_RDONLY | sys::O_CLOEXEC,
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
            Error::Poll { errno } => {
                write!(f, "poll failed: {} ({errno})", sys::strerror(*errno))
            }
            Error::Read { errno } => {
                write!(f, "read failed: {} ({errno})", sys::strerror(*errno))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_event_layout_is_32_bit_clean() {
        // 2x 32-bit timeval + u16 + u16 + i32 = 24 bytes on ARM32.
        assert_eq!(core::mem::size_of::<InputEvent>(), 24);
    }

    #[test]
    fn press_and_release_are_reported() {
        let mut t = Touch {
            fd: -1,
            x: 0,
            y: 0,
            down: false,
        };

        let down = InputEvent {
            kind: EV_KEY,
            code: BTN_TOUCH,
            value: 1,
            ..Default::default()
        };
        assert_eq!(t.translate(&down), Some(TouchEvent::Press { x: 0, y: 0 }));

        let move_x = InputEvent {
            kind: EV_ABS,
            code: ABS_X,
            value: 512,
            ..Default::default()
        };
        assert_eq!(
            t.translate(&move_x),
            Some(TouchEvent::Move { x: 512, y: 0 })
        );

        let up = InputEvent {
            kind: EV_KEY,
            code: BTN_TOUCH,
            value: 0,
            ..Default::default()
        };
        assert_eq!(t.translate(&up), Some(TouchEvent::Release { x: 512, y: 0 }));
    }
}
