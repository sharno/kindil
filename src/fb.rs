//! Framebuffer access for the Kindle's EPDC panel.
//!
//! The i.MX50 EPDC driver exposes `/dev/fb0` as a regular fbdev device. It
//! overrides `fb_mmap` to remap the panel's physical memory with
//! `remap_pfn_range`, so the buffer is mapped with `MAP_SHARED` at the
//! `smem_start` physical address held in `fb_fix_screeninfo`.
//!
//! The Paperwhite 1 panel is 1024x758 and the lab126 driver runs it in
//! **8-bit grayscale, inverted** (`GRAYSCALE_8BIT_INVERTED`): a pixel value of
//! `0` is black and `0xFF` is white, which is the opposite of the usual
//! grayscale convention. The driver compensates internally via the PxP LUT
//! (it toggles `PXP_LUT_INVERT` when the format is inverted), so from
//! user space we simply write `0` for black and `0xFF` for white.
//!
//! Note the `bpp=8` selection happens on the kernel command line
//! (`video=mxcepdcfb:E60,bpp=8`); the driver defaults to 16bpp if it is not
//! given. We read the real value out of `fb_var_screeninfo` rather than
//! assuming, and refuse to run if it is not the 8-bit grayscale mode we can
//! render into.

use crate::epdc::{Epdc, MxcfbRect};
use crate::sys;

/// `FBIOGET_VSCREENINFO` - read the variable screen information.
///
/// These fbdev ioctls are plain constants in `uapi/linux/fb.h`; they do not
/// encode a struct size, so they are the same on every architecture.
pub const FBIOGET_VSCREENINFO: sys::IoctlRequest = 0x4600;
/// `FBIOGET_FSCREENINFO` - read the fixed screen information.
pub const FBIOGET_FSCREENINFO: sys::IoctlRequest = 0x4602;

/// `FBIOGET_FBLANK` - non-zero when the display is blanked.
pub const FBIOGET_FBLANK: sys::IoctlRequest = 0x5411;

/// `GRAYSCALE_8BIT_INVERTED`: 8bpp grayscale where 0 is black and 255 is white.
pub const GRAYSCALE_8BIT_INVERTED: u32 = 0x2;

/// Mirrors the head of `struct fb_bitfield`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FbBitfield {
    pub offset: u32,
    pub length: u32,
    pub msb_right: u32,
}

/// Mirrors `struct fb_var_screeninfo`.
///
/// Every field is 32-bit, so this layout is identical on the 32-bit ARM target
/// and on the 64-bit host.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FbVarScreenInfo {
    pub xres: u32,
    pub yres: u32,
    pub xres_virtual: u32,
    pub yres_virtual: u32,
    pub xoffset: u32,
    pub yoffset: u32,
    pub bits_per_pixel: u32,
    pub grayscale: u32,
    pub red: FbBitfield,
    pub green: FbBitfield,
    pub blue: FbBitfield,
    pub transp: FbBitfield,
    pub nonstd: u32,
    pub activate: u32,
    pub height: u32,
    pub width: u32,
    pub accel_flags: u32,
    pub pixclock: u32,
    pub left_margin: u32,
    pub right_margin: u32,
    pub upper_margin: u32,
    pub lower_margin: u32,
    pub hsync_len: u32,
    pub vsync_len: u32,
    pub sync: u32,
    pub vmode: u32,
    pub rotate: u32,
    pub colorspace: u32,
    pub reserved: [u32; 4],
}

/// Mirrors `struct fb_fix_screeninfo`.
///
/// The two `unsigned long` fields (`smem_start`, `mmio_start`) are 4 bytes on
/// the 32-bit ARM target and 8 bytes on a 64-bit host. We therefore spell
/// them as `u32` and document the difference, rather than `usize`, so the
/// struct has the same size on both.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FbFixScreenInfo {
    pub id: [u8; 16],
    pub smem_start: u32,
    pub smem_len: u32,
    pub type_: u32,
    pub type_aux: u32,
    pub visual: u32,
    pub xpanstep: u16,
    pub ypanstep: u16,
    pub ywrapstep: u32,
    pub line_length: u32,
    pub mmio_start: u32,
    pub mmio_len: u32,
    pub accel: u32,
    pub capabilities: u16,
    pub reserved: [u16; 2],
}

/// The pixel format the panel is running in.
///
/// The lab126 EPDC driver selects the format from the kernel command line's
/// `video=mxcepdcfb:<panel>,bpp=<n>`: with `bpp=8` the panel runs as 8-bit
/// grayscale, and the driver marks it `GRAYSCALE_8BIT_INVERTED`, so a pixel
/// value of `0` is *black* and `0xFF` is *white*. Without `bpp=8` the driver
/// defaults to 16-bit RGB565, where black is `0x0000` and white is `0xFFFF`.
///
/// We detect the format at runtime rather than assuming it, because the two
/// are both in use on these devices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// 8-bit grayscale, inverted: `0` is black, `0xFF` is white.
    Grayscale8Inverted,
    /// 16-bit RGB565: `0x0000` is black, `0xFFFF` is white.
    Rgb565,
}

impl PixelFormat {
    /// The number of bytes per pixel.
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            PixelFormat::Grayscale8Inverted => 1,
            PixelFormat::Rgb565 => 2,
        }
    }

    /// The native byte value for an ink level.
    ///
    /// `ink` is 0 for black and 0xFF for white, whichever way round the
    /// hardware wants them.
    pub const fn encode(self, ink: u8) -> u16 {
        match self {
            // Inverted grayscale: the value is the ink level directly.
            PixelFormat::Grayscale8Inverted => ink as u16,
            // RGB565: 0xFFFF is white, 0x0000 is black.
            PixelFormat::Rgb565 => (ink as u16) * 0x0101,
        }
    }
}

/// A mapped framebuffer.
pub struct Framebuffer {
    ptr: *mut u8,
    len: usize,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: PixelFormat,
}

impl Framebuffer {
    /// Map the framebuffer of an already-opened EPDC device.
    ///
    /// # Errors
    ///
    /// Fails if the ioctls fail, if the panel is not in 8-bit inverted
    /// grayscale mode, or if the mapping fails.
    pub fn map(epdc: &Epdc) -> Result<Self, Error> {
        let mut var = FbVarScreenInfo {
            reserved: [0; 4],
            ..unsafe { core::mem::zeroed() }
        };
        // SAFETY: `var` is a `#[repr(C)]` struct matching the kernel's
        // `struct fb_var_screeninfo`, which is what this ioctl expects.
        let rc = unsafe {
            sys::ioctl(
                epdc.fd(),
                FBIOGET_VSCREENINFO,
                &mut var as *mut FbVarScreenInfo,
            )
        };
        if rc < 0 {
            return Err(Error::Ioctl {
                what: "FBIOGET_VSCREENINFO",
                errno: sys::errno(),
            });
        }

        let mut fix = FbFixScreenInfo {
            id: [0; 16],
            reserved: [0; 2],
            ..unsafe { core::mem::zeroed() }
        };
        // SAFETY: as above, for `struct fb_fix_screeninfo`.
        let rc = unsafe {
            sys::ioctl(
                epdc.fd(),
                FBIOGET_FSCREENINFO,
                &mut fix as *mut FbFixScreenInfo,
            )
        };
        if rc < 0 {
            return Err(Error::Ioctl {
                what: "FBIOGET_FSCREENINFO",
                errno: sys::errno(),
            });
        }

        // Detect the pixel format. The driver sets `grayscale` only for the
        // 8-bit inverted mode; anything else is RGB565 at the driver's
        // default of 16bpp.
        let format = match (var.bits_per_pixel, var.grayscale) {
            (8, GRAYSCALE_8BIT_INVERTED) => PixelFormat::Grayscale8Inverted,
            (16, _) => PixelFormat::Rgb565,
            (bpp, _) => {
                return Err(Error::UnsupportedFormat {
                    bits_per_pixel: bpp,
                })
            }
        };

        let stride = fix.line_length;
        // The driver maps `fix.smem_len` bytes from `fix.smem_start`, and its
        // `mmap` handler rejects a mapping larger than that. Use the kernel's
        // own value rather than deriving one from the geometry.
        let len = fix.smem_len as usize;
        if len == 0 {
            return Err(Error::UnsupportedFormat {
                bits_per_pixel: var.bits_per_pixel,
            });
        }
        // SAFETY: `ptr` is a valid mapping or `MAP_FAILED`; we check the result
        // before use.
        let ptr = unsafe {
            sys::mmap(
                core::ptr::null_mut(),
                len,
                sys::PROT_READ | sys::PROT_WRITE,
                sys::MAP_SHARED,
                epdc.fd(),
                0,
            )
        };
        if ptr as isize == -1 {
            return Err(Error::Mmap {
                errno: sys::errno(),
            });
        }

        Ok(Framebuffer {
            ptr: ptr as *mut u8,
            len,
            width: var.xres,
            height: var.yres,
            stride,
            format,
        })
    }

    /// The mapped buffer.
    pub fn pixels(&mut self) -> &mut [u8] {
        // SAFETY: the mapping is valid for `len` bytes and uniquely owned.
        unsafe { core::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    /// The mapped buffer, read-only.
    pub fn pixels_ref(&self) -> &[u8] {
        // SAFETY: the mapping is valid for `len` bytes and uniquely owned.
        unsafe { core::slice::from_raw_parts(self.ptr, self.len) }
    }

    /// Width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Set one pixel to black (`0`) or white (`0xFF`).
    ///
    /// The ink level is in the panel's own terms: `0` is black and `0xFF` is
    /// white, whatever the underlying pixel format is.
    pub fn set_pixel(&mut self, x: u32, y: u32, value: u8) {
        if x >= self.width || y >= self.height {
            return;
        }
        let idx = y as usize * self.stride as usize + x as usize * self.format.bytes_per_pixel();
        let encoded = self.format.encode(value);
        match self.format {
            PixelFormat::Grayscale8Inverted => {
                self.pixels()[idx] = encoded as u8;
            }
            PixelFormat::Rgb565 => {
                self.pixels()[idx] = encoded as u8;
                self.pixels()[idx + 1] = (encoded >> 8) as u8;
            }
        }
    }

    /// Read one pixel back, in ink terms.
    pub fn pixel(&self, x: u32, y: u32) -> u8 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        let idx = y as usize * self.stride as usize + x as usize * self.format.bytes_per_pixel();
        match self.format {
            PixelFormat::Grayscale8Inverted => self.pixels_ref()[idx],
            PixelFormat::Rgb565 => {
                let lo = self.pixels_ref()[idx] as u16;
                let hi = self.pixels_ref()[idx + 1] as u16;
                (lo | (hi << 8)) as u8
            }
        }
    }

    /// Fill the whole buffer with an ink level.
    ///
    /// `0` is black and `0xFF` is white.
    pub fn fill(&mut self, value: u8) {
        let encoded = self.format.encode(value);
        match self.format {
            PixelFormat::Grayscale8Inverted => self.pixels().fill(encoded as u8),
            PixelFormat::Rgb565 => {
                let lo = encoded as u8;
                let hi = (encoded >> 8) as u8;
                for pair in self.pixels().chunks_exact_mut(2) {
                    pair[0] = lo;
                    pair[1] = hi;
                }
            }
        }
    }

    /// The bounding box of the region we intend to repaint.
    ///
    /// Clamped to the panel, because the driver rejects out-of-bounds regions
    /// with `EINVAL`.
    pub fn full_region(&self) -> MxcfbRect {
        MxcfbRect {
            top: 0,
            left: 0,
            width: self.width,
            height: self.height,
        }
    }
}

impl Drop for Framebuffer {
    fn drop(&mut self) {
        // SAFETY: `ptr`/`len` describe the mapping we created and we drop it once.
        unsafe {
            sys::munmap(self.ptr as *mut core::ffi::c_void, self.len);
        }
    }
}

/// Errors from the framebuffer layer.
#[derive(Debug)]
pub enum Error {
    /// An fbdev ioctl failed.
    Ioctl { what: &'static str, errno: i32 },
    /// `mmap` failed.
    Mmap { errno: i32 },
    /// The panel is not in the 8-bit inverted grayscale mode we render into.
    UnsupportedFormat { bits_per_pixel: u32 },
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Ioctl { what, errno } => {
                write!(f, "{what} failed: {} ({errno})", sys::strerror(*errno))
            }
            Error::Mmap { errno } => {
                write!(f, "fb mmap failed: {} ({errno})", sys::strerror(*errno))
            }
            Error::UnsupportedFormat { bits_per_pixel } => write!(
                f,
                "unsupported framebuffer format: {bits_per_pixel} bpp (need 8bpp inverted grayscale)"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{offset_of, size_of};

    #[test]
    fn var_screeninfo_layout_is_arch_independent() {
        // All-32-bit fields: same size and offsets on ARM32 and x86_64.
        assert_eq!(size_of::<FbVarScreenInfo>(), 160);
        assert_eq!(offset_of!(FbVarScreenInfo, bits_per_pixel), 24);
        assert_eq!(offset_of!(FbVarScreenInfo, grayscale), 28);
        assert_eq!(offset_of!(FbVarScreenInfo, red), 32);
        assert_eq!(offset_of!(FbVarScreenInfo, reserved), 144);
    }

    #[test]
    fn fix_screeninfo_layout_matches_arm32() {
        // `unsigned long` is 4 bytes on the ARM32 target, so the two
        // `unsigned long` fields are 4 bytes there and 8 on a 64-bit host.
        // We spell them as `u32` so the struct matches the target.
        //
        // Verified by compiling the kernel's `struct fb_fix_screeninfo` with
        // 4-byte `unsigned long` fields: size 68, with `line_length` at 44
        // and `mmio_start` at 48.
        assert_eq!(size_of::<FbFixScreenInfo>(), 68);
        assert_eq!(offset_of!(FbFixScreenInfo, id), 0);
        assert_eq!(offset_of!(FbFixScreenInfo, smem_start), 16);
        assert_eq!(offset_of!(FbFixScreenInfo, line_length), 44);
        assert_eq!(offset_of!(FbFixScreenInfo, mmio_start), 48);
    }

    #[test]
    fn fbdev_ioctls_are_plain_constants() {
        assert_eq!(FBIOGET_VSCREENINFO, 0x4600);
        assert_eq!(FBIOGET_FSCREENINFO, 0x4602);
    }

    #[test]
    fn grayscale_encoding_is_direct() {
        // In 8-bit inverted grayscale the value is the ink level itself.
        assert_eq!(PixelFormat::Grayscale8Inverted.encode(0x00), 0x0000);
        assert_eq!(PixelFormat::Grayscale8Inverted.encode(0xff), 0x00ff);
        assert_eq!(PixelFormat::Grayscale8Inverted.bytes_per_pixel(), 1);
    }

    #[test]
    fn rgb565_encoding_maps_black_and_white() {
        // In RGB565, white is 0xFFFF and black is 0x0000.
        assert_eq!(PixelFormat::Rgb565.encode(0x00), 0x0000);
        assert_eq!(PixelFormat::Rgb565.encode(0xff), 0xffff);
        assert_eq!(PixelFormat::Rgb565.bytes_per_pixel(), 2);
    }
}
