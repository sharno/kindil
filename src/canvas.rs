//! The shadow canvas and the frame differ.
//!
//! Nothing draws straight into the mapped framebuffer. A frame is rendered into
//! an in-memory canvas, and only the rectangle that actually changed is copied
//! to the panel.
//!
//! That matters on e-ink: a partial refresh of a small region takes tens of
//! milliseconds, while a full-panel refresh flashes the screen for about a
//! second and drains the battery. The old clock repainted all 776 KiB and
//! refreshed the whole panel once a second; now a minute change refreshes only
//! the band of pixels the digits occupy.
//!
//! The canvas is always 8-bit "ink": `0` is black, `0xFF` is white. That is the
//! panel's inverted grayscale encoding, so the 8bpp blit is a `memcpy` per row;
//! the RGB565 panel variant is expanded at blit time.

/// A rectangle in canvas coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    /// An empty rectangle at the origin.
    pub const EMPTY: Rect = Rect {
        x: 0,
        y: 0,
        w: 0,
        h: 0,
    };

    /// A rectangle from its top-left corner and size.
    pub const fn new(x: u32, y: u32, w: u32, h: u32) -> Self {
        Rect { x, y, w, h }
    }

    /// True if the rectangle covers no pixels.
    pub const fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    /// The first column past the rectangle.
    pub const fn right(&self) -> u32 {
        self.x + self.w
    }

    /// The first row past the rectangle.
    pub const fn bottom(&self) -> u32 {
        self.y + self.h
    }

    /// The smallest rectangle containing both.
    pub fn union(self, other: Rect) -> Rect {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Rect {
            x,
            y,
            w: self.right().max(other.right()) - x,
            h: self.bottom().max(other.bottom()) - y,
        }
    }

    /// Clip to a `width`x`height` area, returning an empty rectangle when the
    /// two do not overlap.
    pub fn clip(self, width: u32, height: u32) -> Rect {
        let x = self.x.min(width);
        let y = self.y.min(height);
        Rect {
            x,
            y,
            w: self.right().min(width) - x,
            h: self.bottom().min(height) - y,
        }
    }
}

/// An in-memory 8-bit canvas.
pub struct Canvas {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Canvas {
    /// A white canvas of the given size.
    pub fn new(width: u32, height: u32) -> Self {
        Canvas {
            width,
            height,
            pixels: vec![crate::render::WHITE; (width as usize) * (height as usize)],
        }
    }

    /// Canvas width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Canvas height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The whole canvas as bytes, row-major with `width` bytes per row.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// One row of the canvas.
    pub fn row(&self, y: u32) -> &[u8] {
        let start = (y as usize) * (self.width as usize);
        &self.pixels[start..start + self.width as usize]
    }

    /// Read one pixel; out-of-bounds reads are white.
    pub fn pixel(&self, x: u32, y: u32) -> u8 {
        if x >= self.width || y >= self.height {
            return crate::render::WHITE;
        }
        self.pixels[(y as usize) * (self.width as usize) + x as usize]
    }

    /// Set one pixel to an ink level. Out-of-bounds writes are ignored.
    pub fn set_pixel(&mut self, x: u32, y: u32, ink: u8) {
        if x >= self.width || y >= self.height {
            return;
        }
        self.pixels[(y as usize) * (self.width as usize) + x as usize] = ink;
    }

    /// Fill a rectangle with an ink level.
    pub fn fill_rect(&mut self, x: u32, y: u32, w: u32, h: u32, ink: u8) {
        let rect = Rect::new(x, y, w, h).clip(self.width, self.height);
        if rect.is_empty() {
            return;
        }
        for row in rect.y..rect.bottom() {
            let start = (row as usize) * (self.width as usize) + rect.x as usize;
            self.pixels[start..start + rect.w as usize].fill(ink);
        }
    }

    /// Clear the canvas to one ink level.
    pub fn clear(&mut self, ink: u8) {
        self.pixels.fill(ink);
    }

    /// Copy a rectangle from another canvas of the same size.
    pub fn copy_rect_from(&mut self, source: &Canvas, rect: Rect) {
        debug_assert_eq!(self.width, source.width);
        debug_assert_eq!(self.height, source.height);
        let rect = rect.clip(self.width, self.height);
        if rect.is_empty() {
            return;
        }
        for y in rect.y..rect.bottom() {
            let start = (y as usize) * (self.width as usize) + rect.x as usize;
            let end = start + rect.w as usize;
            self.pixels[start..end]
                .copy_from_slice(&source.row(y)[rect.x as usize..rect.right() as usize]);
        }
    }

    /// The bounding box of the pixels that differ from `previous`.
    ///
    /// Rows are compared with `memcmp`, so only the rows that changed are
    /// scanned byte by byte - which is what makes a full redraw cheap to
    /// present.
    pub fn diff(&self, previous: &Canvas) -> Option<Rect> {
        if self.width != previous.width || self.height != previous.height {
            return Some(Rect::new(0, 0, self.width, self.height));
        }
        let mut top: Option<u32> = None;
        let mut bottom = 0;
        let mut left = self.width;
        let mut right = 0;
        for y in 0..self.height {
            let new = self.row(y);
            let old = previous.row(y);
            if new == old {
                continue;
            }
            bottom = y + 1;
            if top.is_none() {
                top = Some(y);
            }
            let mut l = 0usize;
            while new[l] == old[l] {
                l += 1;
            }
            let mut r = new.len() - 1;
            while new[r] == old[r] {
                r -= 1;
            }
            left = left.min(l as u32);
            right = right.max(r as u32 + 1);
        }
        top.map(|top| Rect::new(left, top, right - left, bottom - top))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{BLACK, WHITE};

    fn canvas() -> Canvas {
        Canvas::new(16, 8)
    }

    #[test]
    fn a_new_canvas_is_white() {
        let c = canvas();
        assert_eq!(c.pixel(0, 0), WHITE);
        assert_eq!(c.pixel(15, 7), WHITE);
        assert_eq!(c.pixel(16, 7), WHITE);
    }

    #[test]
    fn fill_clips_to_the_canvas() {
        let mut c = canvas();
        c.fill_rect(10, 6, 100, 100, BLACK);
        assert_eq!(c.pixel(10, 6), BLACK);
        assert_eq!(c.pixel(15, 7), BLACK);
        // Entirely outside: nothing happens.
        c.fill_rect(100, 100, 4, 4, BLACK);
        assert_eq!(c.pixel(15, 7), BLACK);
    }

    #[test]
    fn identical_frames_have_no_diff() {
        let a = canvas();
        let b = canvas();
        assert_eq!(a.diff(&b), None);
    }

    #[test]
    fn diff_is_the_bounding_box_of_the_change() {
        let previous = canvas();
        let mut next = canvas();
        next.fill_rect(4, 2, 3, 2, BLACK);
        assert_eq!(next.diff(&previous), Some(Rect::new(4, 2, 3, 2)));

        next.fill_rect(12, 6, 2, 1, BLACK);
        let diff = next.diff(&previous).unwrap();
        assert_eq!(diff, Rect::new(4, 2, 10, 5));
    }

    #[test]
    fn a_size_change_is_a_full_diff() {
        let a = canvas();
        let b = Canvas::new(8, 8);
        assert_eq!(a.diff(&b), Some(Rect::new(0, 0, 16, 8)));
    }

    #[test]
    fn rect_union_with_empty_is_the_other() {
        let r = Rect::new(1, 2, 3, 4);
        assert_eq!(r.union(Rect::EMPTY), r);
        assert_eq!(Rect::EMPTY.union(r), r);
    }
}
