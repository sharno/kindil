//! Mapped framebuffer access for the EPDC panel.
//!
//! The i.MX50 EPDC driver exposes `/dev/fb0` as a regular fbdev device. It
//! overrides `fb_mmap` to remap the panel's physical memory with
//! `remap_pfn_range`, so the buffer is mapped with `MAP_SHARED` at the
//! `smem_start` address held in `fb_fix_screeninfo`.
//!
//! The Paperwhite 1 panel is **758x1024 portrait** (later Paperwhites are
//! landscape) and the lab126 driver runs it in 8-bit grayscale, inverted:
//! a pixel value of `0` is black and `0xFF` is white. The driver compensates
//! with its PxP LUT, so from user space we simply write `0` for black and
//! `0xFF` for white - the same convention the canvas uses.
//!
//! The `bpp=8` choice comes from the kernel command line
//! (`video=mxcepdcfb:E60,bpp=8`); without it the driver defaults to 16-bit
//! RGB565. We read the real value from `fb_var_screeninfo` rather than
//! assuming, and support both.

use crate::canvas::{Canvas, Rect};
use crate::epdc::{Epdc, MxcfbRect};
use crate::sys;

/// `FBIOGET_VSCREENINFO`
pub const FBIOGET_VSCREENINFO: sys::IoctlRequest = 0x4600;
/// `FBIOGET_FSCREENINFO`
pub const FBIOGET_FSCREENINFO: sys::IoctlRequest = 0x4602;

/// `GRAYSCALE_8BIT_INVERTED`: 8bpp grayscale where 0 is black and 255 is white.
pub const GRAYSCALE_8BIT_INVERTED: u32 = 0x2;

/// Mirrors `struct fb_bitfield`.
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
/// the 32-bit ARM target and 8 on a 64-bit host. They are spelled as `u32` so
/// the struct always matches the target.
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

    /// The native 16-bit value for an ink level (`0` black, `0xFF` white).
    pub const fn encode(self, ink: u8) -> u16 {
        match self {
            PixelFormat::Grayscale8Inverted => ink as u16,
            // 565 white is 0xFFFF, black 0x0000; the grey ramp is `ink * 257`.
            PixelFormat::Rgb565 => (ink as u16) * 0x0101,
        }
    }
}

/// A mapped framebuffer.
pub struct Framebuffer {
    ptr: *mut u8,
    len: usize,
    width: u32,
    height: u32,
    stride: u32,
    format: PixelFormat,
}

impl Framebuffer {
    /// Map the framebuffer of an already-opened EPDC device.
    ///
    /// # Errors
    ///
    /// Fails if the ioctls or the mapping fail, or if the panel is in a pixel
    /// format we cannot render into.
    pub fn map(epdc: &Epdc) -> Result<Self, Error> {
        // SAFETY: every field is a plain integer, so an all-zero value is a
        // valid (if meaningless) `fb_var_screeninfo` for the ioctl to fill in.
        let mut var: FbVarScreenInfo = unsafe { core::mem::zeroed() };
        // SAFETY: `var` matches the kernel's `struct fb_var_screeninfo`.
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

        // SAFETY: as above, for `struct fb_fix_screeninfo`.
        let mut fix: FbFixScreenInfo = unsafe { core::mem::zeroed() };
        // SAFETY: `fix` matches the kernel's `struct fb_fix_screeninfo`.
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

        // The driver marks the 8-bit mode with `grayscale`; 16bpp is RGB565.
        let format = match (var.bits_per_pixel, var.grayscale) {
            (8, GRAYSCALE_8BIT_INVERTED) => PixelFormat::Grayscale8Inverted,
            (16, _) => PixelFormat::Rgb565,
            (bits_per_pixel, grayscale) => {
                return Err(Error::UnsupportedFormat {
                    bits_per_pixel,
                    grayscale,
                })
            }
        };

        // The driver's mmap refuses anything larger than `smem_len`, so use the
        // kernel's own value instead of deriving one from the geometry.
        let len = fix.smem_len as usize;
        if len == 0 || fix.line_length == 0 {
            return Err(Error::UnsupportedFormat {
                bits_per_pixel: var.bits_per_pixel,
                grayscale: var.grayscale,
            });
        }

        // SAFETY: `ptr` is a valid mapping or `MAP_FAILED`; we check below.
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
            stride: fix.line_length,
            format,
        })
    }

    /// Panel width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Panel height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Bytes per row in the mapping (may exceed `width * bpp`).
    pub fn stride(&self) -> u32 {
        self.stride
    }

    /// The mapped pixel format.
    pub fn format(&self) -> PixelFormat {
        self.format
    }

    /// The whole panel as an update region.
    pub fn full_region(&self) -> MxcfbRect {
        MxcfbRect {
            top: 0,
            left: 0,
            width: self.width,
            height: self.height,
        }
    }

    /// Copy a rectangle of the canvas into the mapped panel memory.
    ///
    /// Only the given rectangle is written; nothing else in the panel buffer is
    /// touched, which is what makes a partial update safe.
    pub fn blit(&mut self, canvas: &Canvas, rect: Rect) {
        let rect = rect.clip(
            self.width.min(canvas.width()),
            self.height.min(canvas.height()),
        );
        if rect.is_empty() {
            return;
        }
        let bpp = self.format.bytes_per_pixel();
        for y in rect.y..rect.bottom() {
            let src = &canvas.row(y)[rect.x as usize..rect.right() as usize];
            let dst_start = (y as usize) * (self.stride as usize) + (rect.x as usize) * bpp;
            // SAFETY: `dst_start + rect.w * bpp` is within `len`: the rectangle
            // is clipped to the panel geometry, and `len` is `stride * height`
            // for every fbdev the driver can produce.
            let dst = unsafe {
                core::slice::from_raw_parts_mut(self.ptr.add(dst_start), rect.w as usize * bpp)
            };
            match self.format {
                PixelFormat::Grayscale8Inverted => dst.copy_from_slice(src),
                PixelFormat::Rgb565 => {
                    for (chunk, &ink) in dst.chunks_exact_mut(2).zip(src) {
                        let encoded = PixelFormat::Rgb565.encode(ink);
                        chunk[0] = encoded as u8;
                        chunk[1] = (encoded >> 8) as u8;
                    }
                }
            }
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
    /// The panel is not in a pixel format we can render into.
    UnsupportedFormat { bits_per_pixel: u32, grayscale: u32 },
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
            Error::UnsupportedFormat {
                bits_per_pixel,
                grayscale,
            } => write!(
                f,
                "unsupported framebuffer format: {bits_per_pixel} bpp, grayscale=0x{grayscale:x} \
                 (need 8bpp inverted grayscale or 16bpp RGB565)"
            ),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{offset_of, size_of};

    #[test]
    fn var_screeninfo_layout_is_arch_independent() {
        assert_eq!(size_of::<FbVarScreenInfo>(), 160);
        assert_eq!(offset_of!(FbVarScreenInfo, bits_per_pixel), 24);
        assert_eq!(offset_of!(FbVarScreenInfo, grayscale), 28);
        assert_eq!(offset_of!(FbVarScreenInfo, red), 32);
        assert_eq!(offset_of!(FbVarScreenInfo, reserved), 144);
    }

    #[test]
    fn fix_screeninfo_layout_matches_arm32() {
        // 68 bytes with 4-byte `unsigned long` fields.
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
        assert_eq!(PixelFormat::Grayscale8Inverted.encode(0x00), 0x0000);
        assert_eq!(PixelFormat::Grayscale8Inverted.encode(0xff), 0x00ff);
        assert_eq!(PixelFormat::Grayscale8Inverted.bytes_per_pixel(), 1);
    }

    #[test]
    fn rgb565_encoding_maps_black_and_white() {
        assert_eq!(PixelFormat::Rgb565.encode(0x00), 0x0000);
        assert_eq!(PixelFormat::Rgb565.encode(0xff), 0xffff);
        assert_eq!(PixelFormat::Rgb565.encode(0x80), 0x8080);
        assert_eq!(PixelFormat::Rgb565.bytes_per_pixel(), 2);
    }
}
