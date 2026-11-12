//! The Freescale/NXP EPDC (E Ink Panel Driver Controller) ioctl interface.
//!
//! # Where this ABI comes from
//!
//! The Paperwhite 1 (board "Celeste", SoC "MX50 Yoshime") runs Amazon's
//! lab126 kernel, whose display driver is `drivers/video/mxc/mxc_epdc_fb.c`
//! and whose public interface is `include/linux/mxcfb.h`. This module mirrors
//! the parts of that header we use, checked against the 2.6.31 source.
//!
//! # Layout is load-bearing
//!
//! The ioctl number encodes `sizeof(arg_struct)`, so a differently laid out
//! struct reaches a different ioctl number and the driver answers `ENOTTY`.
//! Everything is `#[repr(C)]` and the sizes, offsets and request numbers are
//! asserted in the tests below.
//!
//! # What the driver already does for us
//!
//! Two things that look like they need setup do not:
//!
//! * The probe path initialises `wv_modes` to the right defaults
//!   (`mxc_epdc_fb.c`: "Set default waveform mode values") and installs
//!   `UPDATE_SCHEME_QUEUE_AND_MERGE` by default.
//! * `MXCFB_SET_*_UPDATE_MODE` is deliberately left alone: automatic mode
//!   recomputes the update region from the dirty pages and overrides the
//!   rectangle we pass, which is exactly what we do not want.
//!
//! Issuing none of those ioctls also removes three ways for startup to fail on
//! a device whose driver differs slightly from the expected one.

use crate::sys;

/// A rectangle in framebuffer coordinates.
///
/// Mirrors `struct mxcfb_rect`. All fields are `__u32`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MxcfbRect {
    pub top: u32,
    pub left: u32,
    pub width: u32,
    pub height: u32,
}

/// Mirrors `struct mxcfb_alt_buffer_data`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MxcfbAltBufferData {
    pub phys_addr: u32,
    pub width: u32,
    pub height: u32,
    pub alt_update_region: MxcfbRect,
}

/// Mirrors `struct mxcfb_update_data` (the 2.6.31 / Lab126 layout).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MxcfbUpdateData {
    pub update_region: MxcfbRect,
    pub waveform_mode: u32,
    pub update_mode: u32,
    pub update_marker: u32,
    /// Lab126: waveform for the black/white pass of a histogram update.
    pub hist_bw_waveform_mode: u32,
    /// Lab126: waveform for the grey pass of a histogram update.
    pub hist_gray_waveform_mode: u32,
    pub temp: i32,
    pub flags: u32,
    pub alt_buffer_data: MxcfbAltBufferData,
}

/// Values for [`MxcfbUpdateData::update_mode`].
pub const UPDATE_MODE_PARTIAL: u32 = 0x0;
pub const UPDATE_MODE_FULL: u32 = 0x1;

/// Waveform modes understood by the 2.6.31 EPDC driver.
pub const WAVEFORM_MODE_INIT: u32 = 0x0;
pub const WAVEFORM_MODE_DU: u32 = 0x1;
pub const WAVEFORM_MODE_GC16: u32 = 0x2;
pub const WAVEFORM_MODE_GC16_FAST: u32 = 0x3;
pub const WAVEFORM_MODE_A2: u32 = 0x4;
pub const WAVEFORM_MODE_GL16: u32 = 0x5;
pub const WAVEFORM_MODE_GL16_FAST: u32 = 0x6;
pub const WAVEFORM_MODE_DU4: u32 = 0x7;
pub const WAVEFORM_MODE_AUTO: u32 = 257;

/// `TEMP_USE_PAPYRUS`: let the driver read the panel temperature from the PMIC.
pub const TEMP_USE_PAPYRUS: i32 = 0x1001;

/// Requests we issue. `_IOW` with a 4-byte argument for the marker, and with
/// the 72-byte update struct for the update itself.
pub mod requests {
    use super::sys::{ioc, IoctlRequest, IOC_TYPE_F};

    /// `_IOW('F', 0x2E, struct mxcfb_update_data)`
    pub const SEND_UPDATE: IoctlRequest = ioc::iow(
        IOC_TYPE_F,
        0x2E,
        core::mem::size_of::<super::MxcfbUpdateData>() as u32,
    );
    /// `_IOW('F', 0x2F, __u32)`
    pub const WAIT_FOR_UPDATE_COMPLETE: IoctlRequest = ioc::iow(IOC_TYPE_F, 0x2F, 4);
}

/// The framebuffer device, plus the EPDC control ioctls.
pub struct Epdc {
    fd: i32,
    marker: u32,
}

impl Epdc {
    /// Open the EPDC framebuffer device.
    ///
    /// # Errors
    ///
    /// Returns an error if the device cannot be opened.
    pub fn open(path: &str) -> Result<Self, Error> {
        Ok(Epdc {
            fd: open_device_rw(path)?,
            marker: 0,
        })
    }

    /// The raw file descriptor of the framebuffer.
    pub fn fd(&self) -> i32 {
        self.fd
    }

    /// `_IOW('F', 0x2E, struct mxcfb_update_data)`: push an update to the panel.
    ///
    /// `region` is the dirty rectangle to refresh. `waveform` selects the
    /// waveform, and `full` selects a full-panel refresh (which clears ghosting)
    /// rather than a partial one.
    pub fn send_update(
        &mut self,
        region: MxcfbRect,
        waveform: u32,
        full: bool,
    ) -> Result<(), Error> {
        self.marker = self.marker.wrapping_add(1);
        let mut data = MxcfbUpdateData {
            update_region: region,
            waveform_mode: waveform,
            update_mode: if full {
                UPDATE_MODE_FULL
            } else {
                UPDATE_MODE_PARTIAL
            },
            update_marker: self.marker,
            // The Lab126 histogram waveforms: when the requested waveform is
            // `AUTO`, the driver narrows the choice down using these. FBInk
            // uses DU for the black/white pass and GC16_FAST for the grey one.
            hist_bw_waveform_mode: WAVEFORM_MODE_DU,
            hist_gray_waveform_mode: WAVEFORM_MODE_GC16_FAST,
            temp: TEMP_USE_PAPYRUS,
            flags: 0,
            alt_buffer_data: MxcfbAltBufferData::default(),
        };
        // SAFETY: `data` is a `#[repr(C)]` struct matching the kernel's
        // `struct mxcfb_update_data`, and the request number encodes its size.
        // The driver also copies the (possibly adjusted) struct back, so the
        // pointer must be to a mutable local.
        let rc = unsafe {
            sys::ioctl(
                self.fd,
                requests::SEND_UPDATE,
                &mut data as *mut MxcfbUpdateData,
            )
        };
        check(rc, "MXCFB_SEND_UPDATE")
    }

    /// `_IOW('F', 0x2F, __u32)`: block until the panel finishes refreshing.
    ///
    /// The kernel handler does `get_user(update_marker, (__u32 __user *) arg)`,
    /// so the marker is passed **by pointer**, not by value.
    ///
    /// Returns the number of jiffies the driver waited, or 0 if the update had
    /// already completed.
    pub fn wait_for_update_complete(&self) -> Result<i32, Error> {
        let mut marker = self.marker;
        // SAFETY: `marker` is a live `u32` whose address the kernel reads.
        let rc = unsafe {
            sys::ioctl(
                self.fd,
                requests::WAIT_FOR_UPDATE_COMPLETE,
                &mut marker as *mut u32,
            )
        };
        check(rc, "MXCFB_WAIT_FOR_UPDATE_COMPLETE")?;
        Ok(rc)
    }
}

impl Drop for Epdc {
    fn drop(&mut self) {
        // SAFETY: `fd` is owned by this value and closed exactly once.
        unsafe {
            sys::close(self.fd);
        }
    }
}

/// Open a device path for reading and writing.
pub fn open_device_rw(path: &str) -> Result<i32, Error> {
    let c_path = sys::to_cstring(path).ok_or_else(|| Error::Open {
        path: path.to_string(),
        errno: sys::EINVAL,
    })?;
    // SAFETY: `c_path` is a NUL-terminated C string that outlives the call.
    let fd = unsafe { sys::open(c_path.as_ptr() as *const core::ffi::c_char, sys::O_RDWR) };
    if fd < 0 {
        return Err(Error::Open {
            path: path.to_string(),
            errno: sys::errno(),
        });
    }
    Ok(fd)
}

/// An error from the EPDC/framebuffer layer.
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

fn check(rc: i32, what: &'static str) -> Result<(), Error> {
    if rc < 0 {
        Err(Error::Ioctl {
            what,
            errno: sys::errno(),
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    // Sizes verified against `gcc` compiling the real `include/linux/mxcfb.h`
    // from the lab126 2.6.31 tree for 32-bit ARM.
    #[test]
    fn struct_sizes_match_kernel_header() {
        assert_eq!(size_of::<MxcfbRect>(), 16);
        assert_eq!(size_of::<MxcfbAltBufferData>(), 28);
        assert_eq!(size_of::<MxcfbUpdateData>(), 72);
    }

    #[test]
    fn update_data_field_offsets_match_kernel_header() {
        assert_eq!(offset_of!(MxcfbUpdateData, update_region), 0);
        assert_eq!(offset_of!(MxcfbUpdateData, waveform_mode), 16);
        assert_eq!(offset_of!(MxcfbUpdateData, update_mode), 20);
        assert_eq!(offset_of!(MxcfbUpdateData, update_marker), 24);
        assert_eq!(offset_of!(MxcfbUpdateData, hist_bw_waveform_mode), 28);
        assert_eq!(offset_of!(MxcfbUpdateData, hist_gray_waveform_mode), 32);
        assert_eq!(offset_of!(MxcfbUpdateData, temp), 36);
        assert_eq!(offset_of!(MxcfbUpdateData, flags), 40);
        assert_eq!(offset_of!(MxcfbUpdateData, alt_buffer_data), 44);
    }

    #[test]
    fn alignment_is_four_bytes() {
        assert_eq!(align_of::<MxcfbUpdateData>(), 4);
    }

    // Request numbers captured from the real header compiled with the kernel's
    // own `_IOW` macro.
    #[test]
    fn ioctl_numbers_match_kernel_header() {
        assert_eq!(requests::SEND_UPDATE, 0x4048_462e);
        assert_eq!(requests::WAIT_FOR_UPDATE_COMPLETE, 0x4004_462f);
    }
}
