//! The panel abstraction: something that can show a finished frame.
//!
//! The shell renders into a [`Canvas`] and hands the changed rectangle to a
//! [`Panel`]. On the device that is the EPDC framebuffer; on a workstation it is
//! the simulator, which keeps the image in memory and can write it out as a
//! PGM. The shell code is identical in both cases.

use crate::canvas::{Canvas, Rect};
use crate::epdc::{self, Epdc, MxcfbRect, WAVEFORM_MODE_AUTO, WAVEFORM_MODE_GC16};
use crate::fb::{self, Framebuffer};

/// A display that frames can be presented to.
pub trait Panel {
    /// Width in pixels.
    fn width(&self) -> u32;
    /// Height in pixels.
    fn height(&self) -> u32;
    /// Show `canvas`. `rect` is the changed region; `full` asks for a
    /// ghost-clearing full refresh of the whole panel.
    fn present(&mut self, canvas: &Canvas, rect: Rect, full: bool) -> Result<(), PresentError>;
}

/// Why a frame could not be presented.
#[derive(Debug)]
pub enum PresentError {
    /// The framebuffer layer failed.
    Framebuffer(fb::Error),
    /// The EPDC layer failed.
    Epdc(epdc::Error),
}

impl core::fmt::Display for PresentError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PresentError::Framebuffer(e) => write!(f, "framebuffer: {e}"),
            PresentError::Epdc(e) => write!(f, "epdc: {e}"),
        }
    }
}

impl std::error::Error for PresentError {}

impl From<fb::Error> for PresentError {
    fn from(e: fb::Error) -> Self {
        PresentError::Framebuffer(e)
    }
}

impl From<epdc::Error> for PresentError {
    fn from(e: epdc::Error) -> Self {
        PresentError::Epdc(e)
    }
}

/// The Kindle's EPDC panel.
pub struct DevicePanel {
    epdc: Epdc,
    fb: Framebuffer,
}

impl DevicePanel {
    /// Open the framebuffer and map it.
    pub fn open(path: &str) -> Result<Self, PresentError> {
        let epdc = Epdc::open(path).map_err(PresentError::Epdc)?;
        let fb = Framebuffer::map(&epdc)?;
        Ok(DevicePanel { epdc, fb })
    }
}

impl Panel for DevicePanel {
    fn width(&self) -> u32 {
        self.fb.width()
    }

    fn height(&self) -> u32 {
        self.fb.height()
    }

    fn present(&mut self, canvas: &Canvas, rect: Rect, full: bool) -> Result<(), PresentError> {
        let rect = if full {
            Rect::new(0, 0, self.fb.width(), self.fb.height())
        } else {
            rect
        };
        if rect.is_empty() {
            return Ok(());
        }

        self.fb.blit(canvas, rect);

        let region = MxcfbRect {
            top: rect.y,
            left: rect.x,
            width: rect.w,
            height: rect.h,
        };
        // GC16 is the full-fidelity, flashing waveform; it is what clears
        // ghosting. A partial update lets the driver choose (`AUTO`).
        let waveform = if full {
            WAVEFORM_MODE_GC16
        } else {
            WAVEFORM_MODE_AUTO
        };
        self.epdc.send_update(region, waveform, full)?;
        self.epdc.wait_for_update_complete()?;
        Ok(())
    }
}
