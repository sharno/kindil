//! Bindings for the Freescale/NXP EPDC (E Ink Panel Driver Controller).
//!
//! # Where this ABI comes from
//!
//! The Paperwhite 1 (board "Celeste", SoC "MX50 Yoshime") runs Amazon's
//! lab126 kernel, whose display driver is `drivers/video/mxc/mxc_epdc_fb.c`
//! and whose public interface is `include/linux/mxcfb.h`. We mirror that
//! header exactly:
//!
//! * `include/linux/mxcfb.h` from the Quill-OS `linux-2.6.31-kt` tree, which
//!   is the lab126 2.6.31 tree used for the i.MX50 Kindle family. That header
//!   is byte-identical to the one in the independent `fread-ink/fread-kernel-k4`
//!   tree, so the ABI is not specific to one mirror.
//! * The same struct layouts and ioctl numbers are described in
//!   `NiLuJe/FBInk`'s `eink/mxcfb-kindle.h`, the reference user-space
//!   implementation for these devices.
//!
//! # Layout is load-bearing
//!
//! The ioctl number encodes `sizeof(arg_struct)`, so if any of the structs
//! below were laid out differently on the target than on the kernel's side,
//! the driver would reject the call with `ENOTTY`. Everything is `#[repr(C)]`
//! and the sizes are asserted at compile time in the tests below.
//!
//! Note that `struct mxcfb_update_data` here is the *2.6.31* variant, which
//! still carries the Lab126 `hist_bw_waveform_mode` / `hist_gray_waveform_mode`
//! fields. Later kernels dropped them; the ones that added them back (PW2,
//! PW4, KOA2) use a different struct *size* and therefore different ioctl
//! numbers, which is why they have their own ioctl variants upstream.

use crate::sys::{self, ioc, IOC_TYPE_F};

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
    /// Lab126: default black/white waveform for histogram analysis.
    pub hist_bw_waveform_mode: u32,
    /// Lab126: default grey waveform for histogram analysis.
    pub hist_gray_waveform_mode: u32,
    pub temp: i32,
    pub flags: u32,
    pub alt_buffer_data: MxcfbAltBufferData,
}

/// Mirrors `struct mxcfb_waveform_modes`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MxcfbWaveformModes {
    pub mode_init: i32,
    pub mode_du: i32,
    pub mode_gc4: i32,
    pub mode_gc8: i32,
    pub mode_gc16: i32,
    pub mode_gc16_fast: i32,
    pub mode_gc32: i32,
    pub mode_gl16: i32,
    pub mode_gl16_fast: i32,
    pub mode_a2: i32,
    pub mode_du4: i32,
}

/// Update scheme passed to [`MXCFB_SET_UPDATE_SCHEME`].
pub const UPDATE_SCHEME_SNAPSHOT: u32 = 0;
pub const UPDATE_SCHEME_QUEUE: u32 = 1;
pub const UPDATE_SCHEME_QUEUE_AND_MERGE: u32 = 2;

/// Update mode passed in [`MxcfbUpdateData::update_mode`].
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

/// Flags for [`MxcfbUpdateData::flags`].
pub const EPDC_FLAG_ENABLE_INVERSION: u32 = 0x01;
pub const EPDC_FLAG_FORCE_MONOCHROME: u32 = 0x02;
pub const EPDC_FLAG_USE_CMAP: u32 = 0x04;
pub const EPDC_FLAG_USE_ALT_BUFFER: u32 = 0x100;

/// `TEMP_USE_PAPYRUS`: let the driver pick the temperature from the PMIC.
pub const TEMP_USE_PAPYRUS: i32 = 0x1001;

// Ioctl numbers. `size` is `size_of` of the argument struct, which is what
// the kernel compiled into the request number.
//
// The request is typed as `c_ulong`, because that is how the kernel reads it:
// 4 bytes on the 32-bit ARM target, 8 on a 64-bit host. The request
// *numbers* are the same either way.
const MXCFB_SET_WAVEFORM_MODES: sys::IoctlRequest = ioc::iow(
    IOC_TYPE_F,
    0x2B,
    core::mem::size_of::<MxcfbWaveformModes>() as u32,
);
const MXCFB_SET_TEMPERATURE: sys::IoctlRequest = ioc::iow(IOC_TYPE_F, 0x2C, 4);
const MXCFB_SET_AUTO_UPDATE_MODE: sys::IoctlRequest = ioc::iow(IOC_TYPE_F, 0x2D, 4);
const MXCFB_SEND_UPDATE: sys::IoctlRequest = ioc::iow(
    IOC_TYPE_F,
    0x2E,
    core::mem::size_of::<MxcfbUpdateData>() as u32,
);
const MXCFB_WAIT_FOR_UPDATE_COMPLETE: sys::IoctlRequest = ioc::iow(IOC_TYPE_F, 0x2F, 4);
const MXCFB_SET_UPDATE_SCHEME: sys::IoctlRequest = ioc::iow(IOC_TYPE_F, 0x32, 4);

/// The ioctl numbers this module issues, exposed for verification.
pub mod requests {
    use super::sys::IoctlRequest;

    /// `_IOW('F', 0x2B, struct mxcfb_waveform_modes)`
    pub const SET_WAVEFORM_MODES: IoctlRequest = super::MXCFB_SET_WAVEFORM_MODES;
    /// `_IOW('F', 0x2C, int32_t)`
    pub const SET_TEMPERATURE: IoctlRequest = super::MXCFB_SET_TEMPERATURE;
    /// `_IOW('F', 0x2D, __u32)`
    pub const SET_AUTO_UPDATE_MODE: IoctlRequest = super::MXCFB_SET_AUTO_UPDATE_MODE;
    /// `_IOW('F', 0x2E, struct mxcfb_update_data)`
    pub const SEND_UPDATE: IoctlRequest = super::MXCFB_SEND_UPDATE;
    /// `_IOW('F', 0x2F, __u32)`
    pub const WAIT_FOR_UPDATE_COMPLETE: IoctlRequest = super::MXCFB_WAIT_FOR_UPDATE_COMPLETE;
    /// `_IOW('F', 0x32, __u32)`
    pub const SET_UPDATE_SCHEME: IoctlRequest = super::MXCFB_SET_UPDATE_SCHEME;
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
    /// Returns an error if `/dev/fb0` cannot be opened, or if the driver
    /// rejects the initial configuration ioctls.
    pub fn open(path: &str) -> Result<Self, Error> {
        let fd = open_device_rw(path)?;
        let mut epdc = Epdc { fd, marker: 0 };

        // Queue-and-merge lets the driver coalesce overlapping updates, which
        // is what we want for a UI that repaints regions independently.
        epdc.set_update_scheme(UPDATE_SCHEME_QUEUE_AND_MERGE)?;
        // Note that we deliberately do *not* set the auto update mode.
        // `AUTO_UPDATE_MODE_AUTOMATIC_MODE` makes the driver recompute the
        // update region itself from the dirty pages, overriding the region we
        // ask for; `AUTO_UPDATE_MODE_REGION_MODE` (the driver's default) uses
        // the region we pass. FBInk likewise leaves it alone.
        //
        // Install the waveform table.
        //
        // The driver's `wv_modes` table starts out zeroed, and
        // `get_waveform_by_type()` maps our waveform constants through that
        // table. Without this ioctl, asking for `GC16` would resolve to mode
        // 0 (`INIT`), which flashes the whole panel to white. We therefore
        // install the identity mapping, so that the waveform we request is
        // the waveform the driver uses.
        epdc.set_waveform_modes(&MxcfbWaveformModes {
            mode_init: WAVEFORM_MODE_INIT as i32,
            mode_du: WAVEFORM_MODE_DU as i32,
            mode_gc4: WAVEFORM_MODE_GC16 as i32,
            mode_gc8: WAVEFORM_MODE_GC16 as i32,
            mode_gc16: WAVEFORM_MODE_GC16 as i32,
            mode_gc16_fast: WAVEFORM_MODE_GC16_FAST as i32,
            mode_gc32: WAVEFORM_MODE_GC16 as i32,
            mode_gl16: WAVEFORM_MODE_GL16 as i32,
            mode_gl16_fast: WAVEFORM_MODE_GL16_FAST as i32,
            mode_a2: WAVEFORM_MODE_A2 as i32,
            mode_du4: WAVEFORM_MODE_DU4 as i32,
        })?;
        Ok(epdc)
    }

    /// The raw file descriptor of the framebuffer.
    pub fn fd(&self) -> i32 {
        self.fd
    }

    /// `_IOW('F', 0x32, __u32)`: choose how updates are scheduled.
    pub fn set_update_scheme(&mut self, scheme: u32) -> Result<(), Error> {
        // SAFETY: `scheme` is a `u32`, matching the kernel's `__u32` argument.
        let rc = unsafe { sys::ioctl(self.fd, MXCFB_SET_UPDATE_SCHEME, scheme) };
        check(rc, "MXCFB_SET_UPDATE_SCHEME")
    }

    /// `_IOW('F', 0x2D, __u32)`: 0 = region mode, 1 = automatic full mode.
    pub fn set_auto_update_mode(&mut self, mode: u32) -> Result<(), Error> {
        // SAFETY: `mode` is a `u32`, matching the kernel's `__u32` argument.
        let rc = unsafe { sys::ioctl(self.fd, MXCFB_SET_AUTO_UPDATE_MODE, mode) };
        check(rc, "MXCFB_SET_AUTO_UPDATE_MODE")
    }

    /// `_IOW('F', 0x2C, int32_t)`: set the panel temperature.
    ///
    /// Pass [`TEMP_USE_PAPYRUS`] to let the driver read the PMIC.
    pub fn set_temperature(&mut self, temp: i32) -> Result<(), Error> {
        // SAFETY: `temp` is an `i32`, matching the kernel's `int32_t` argument.
        let rc = unsafe { sys::ioctl(self.fd, MXCFB_SET_TEMPERATURE, temp) };
        check(rc, "MXCFB_SET_TEMPERATURE")
    }

    /// `_IOW('F', 0x2B, struct mxcfb_waveform_modes)`: install waveform modes.
    ///
    /// The driver uses these to pick a waveform when the caller asks for
    /// [`WAVEFORM_MODE_AUTO`].
    pub fn set_waveform_modes(&mut self, modes: &MxcfbWaveformModes) -> Result<(), Error> {
        // SAFETY: `modes` is a pointer to a `#[repr(C)]` struct with the same
        // layout as the kernel's `struct mxcfb_waveform_modes`, and the
        // request number encodes that struct's size.
        let rc = unsafe {
            sys::ioctl(
                self.fd,
                MXCFB_SET_WAVEFORM_MODES,
                modes as *const MxcfbWaveformModes,
            )
        };
        check(rc, "MXCFB_SET_WAVEFORM_MODES")
    }

    /// `_IOW('F', 0x2E, struct mxcfb_update_data)`: push an update to the panel.
    ///
    /// `region` is the dirty rectangle to refresh. `waveform` selects the
    /// waveform (use [`WAVEFORM_MODE_AUTO`] to let the driver decide), and
    /// `full` selects a full-panel refresh (clearing ghosting) versus a
    /// partial one.
    pub fn send_update(
        &mut self,
        region: MxcfbRect,
        waveform: u32,
        full: bool,
    ) -> Result<(), Error> {
        self.marker = self.marker.wrapping_add(1);
        let data = MxcfbUpdateData {
            update_region: region,
            waveform_mode: waveform,
            update_mode: if full {
                UPDATE_MODE_FULL
            } else {
                UPDATE_MODE_PARTIAL
            },
            update_marker: self.marker,
            // These two are the Lab126 histogram waveforms: the driver uses
            // them to colour its decision when the requested waveform is
            // `AUTO`. FBInk uses DU for the black/white pass and GC16_FAST
            // for the grey pass, and so do we.
            hist_bw_waveform_mode: WAVEFORM_MODE_DU,
            hist_gray_waveform_mode: WAVEFORM_MODE_GC16_FAST,
            temp: TEMP_USE_PAPYRUS,
            flags: 0,
            alt_buffer_data: MxcfbAltBufferData::default(),
        };
        // SAFETY: `data` is a `#[repr(C)]` struct matching the kernel's
        // `struct mxcfb_update_data`; the request number encodes its size.
        let rc = unsafe { sys::ioctl(self.fd, MXCFB_SEND_UPDATE, &data as *const MxcfbUpdateData) };
        check(rc, "MXCFB_SEND_UPDATE")
    }

    /// `_IOW('F', 0x2F, __u32)`: block until the panel finishes refreshing.
    ///
    /// # Argument passing
    ///
    /// The kernel's handler does `get_user(update_marker, (__u32 __user *) arg)`,
    /// so the marker must be passed **by pointer**, not by value. On the PW1
    /// this ioctl is the one FBInk calls `MXCFB_WAIT_FOR_UPDATE_COMPLETE_PEARL`
    /// (`_IOW('F', 0x2F, uint32_t)`) - the same request number as the stock
    /// header, because both encode a 4-byte argument. The PW2 variant of this
    /// ioctl uses a different struct and therefore a different number.
    ///
    /// The driver returns the number of jiffies it waited (5000ms timeout), or
    /// 0 if the update had already completed.
    pub fn wait_for_update_complete(&self) -> Result<i32, Error> {
        let mut marker = self.marker;
        // SAFETY: `marker` is a live `u32` whose address we hand to the
        // kernel; the driver copies it out with `get_user`.
        let rc = unsafe {
            sys::ioctl(
                self.fd,
                MXCFB_WAIT_FOR_UPDATE_COMPLETE,
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
        return Err(Error::open(path));
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

impl Error {
    fn open(path: &str) -> Self {
        Error::Open {
            path: path.to_string(),
            errno: sys::errno(),
        }
    }
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

    // Sizes verified against `gcc -o csize csize.c` compiled with the real
    // `include/linux/mxcfb.h` from the lab126 2.6.31 tree.
    #[test]
    fn struct_sizes_match_kernel_header() {
        assert_eq!(size_of::<MxcfbRect>(), 16);
        assert_eq!(size_of::<MxcfbAltBufferData>(), 28);
        assert_eq!(size_of::<MxcfbUpdateData>(), 72);
        assert_eq!(size_of::<MxcfbWaveformModes>(), 44);
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
        assert_eq!(align_of::<MxcfbWaveformModes>(), 4);
    }

    // Request numbers captured from the real header compiled on the host with
    // the kernel's own `_IOW` macro. These are architecture independent here
    // because every argument struct is 32-bit-clean (no pointers, no `long`).
    #[test]
    fn ioctl_numbers_match_kernel_header() {
        assert_eq!(requests::SET_WAVEFORM_MODES, 0x402c_462b);
        assert_eq!(requests::SET_TEMPERATURE, 0x4004_462c);
        assert_eq!(requests::SET_AUTO_UPDATE_MODE, 0x4004_462d);
        assert_eq!(requests::SEND_UPDATE, 0x4048_462e);
        assert_eq!(requests::WAIT_FOR_UPDATE_COMPLETE, 0x4004_462f);
        assert_eq!(requests::SET_UPDATE_SCHEME, 0x4004_4632);
    }
}
