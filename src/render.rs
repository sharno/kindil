//! Software rendering into the shadow canvas.
//!
//! The canvas is 8-bit "ink": `0` is black and `0xFF` is white, matching the
//! panel's native inverted grayscale. The renderer therefore needs no
//! colour-space conversion; the framebuffer layer handles the RGB565 variant at
//! blit time.

use crate::canvas::Canvas;
use crate::font::FONT8X8_BASIC;

/// Ink value for black.
pub const BLACK: u8 = 0x00;
/// Ink value for white.
pub const WHITE: u8 = 0xFF;

/// Height of a font cell in unscaled pixels.
pub const GLYPH_HEIGHT: u32 = 8;
/// Width of a font cell in unscaled pixels.
pub const GLYPH_WIDTH: u32 = 8;

/// The base text scale used by the shell. On the Paperwhite's 212 DPI panel a
/// scale of 2 gives a 16x16 cell, roughly 11 pt.
pub const FONT_SCALE: u32 = 2;

/// A text renderer bound to a canvas.
pub struct Renderer<'a> {
    canvas: &'a mut Canvas,
}

impl<'a> Renderer<'a> {
    /// Wrap a canvas for drawing.
    pub fn new(canvas: &'a mut Canvas) -> Self {
        Renderer { canvas }
    }

    /// The canvas being drawn into.
    pub fn canvas(&mut self) -> &mut Canvas {
        self.canvas
    }

    /// Draw a string with the top-left corner at `(x, y)`.
    ///
    /// Returns the x coordinate just past the last glyph, so callers can chain
    /// runs of text without recomputing widths.
    pub fn draw_text(&mut self, x: u32, y: u32, text: &str, ink: u8, scale: u32) -> u32 {
        let mut cx = x;
        for ch in text.chars() {
            cx = self.draw_char(cx, y, ch, ink, scale);
        }
        cx
    }

    /// Draw a single character; returns the x coordinate after it.
    pub fn draw_char(&mut self, x: u32, y: u32, ch: char, ink: u8, scale: u32) -> u32 {
        // The font only covers ASCII; anything else becomes a hollow box.
        let glyph = if (ch as u32) < 128 {
            FONT8X8_BASIC[ch as usize]
        } else {
            [0x7e, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x7e]
        };

        let scale = scale.max(1);
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..GLYPH_WIDTH {
                if bits & (1 << col) == 0 {
                    continue;
                }
                let px = x + col * scale;
                let py = y + row as u32 * scale;
                if scale == 1 {
                    self.canvas.set_pixel(px, py, ink);
                } else {
                    self.canvas.fill_rect(px, py, scale, scale, ink);
                }
            }
        }
        x + GLYPH_WIDTH * scale
    }

    /// Fill a rectangle.
    pub fn fill_rect(&mut self, x: u32, y: u32, w: u32, h: u32, ink: u8) {
        self.canvas.fill_rect(x, y, w, h, ink);
    }

    /// Draw a 1-pixel rectangle outline.
    pub fn stroke_rect(&mut self, x: u32, y: u32, w: u32, h: u32, ink: u8) {
        if w == 0 || h == 0 {
            return;
        }
        self.fill_rect(x, y, w, 1, ink);
        self.fill_rect(x, y + h - 1, w, 1, ink);
        self.fill_rect(x, y, 1, h, ink);
        self.fill_rect(x + w - 1, y, 1, h, ink);
    }

    /// Draw a horizontal line.
    pub fn hline(&mut self, x: u32, y: u32, w: u32, ink: u8) {
        self.fill_rect(x, y, w, 1, ink);
    }

    /// Draw a vertical line.
    pub fn vline(&mut self, x: u32, y: u32, h: u32, ink: u8) {
        self.fill_rect(x, y, 1, h, ink);
    }

    /// Width in pixels of `text` at the given scale.
    pub fn text_width(text: &str, scale: u32) -> u32 {
        text.chars().count() as u32 * GLYPH_WIDTH * scale
    }

    /// Height in pixels of a line of text at the given scale.
    pub fn text_height(scale: u32) -> u32 {
        GLYPH_HEIGHT * scale
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_width_is_glyph_count_times_cell() {
        assert_eq!(Renderer::text_width("", 2), 0);
        assert_eq!(Renderer::text_width("A", 2), 16);
        assert_eq!(Renderer::text_width("hello", 2), 5 * 16);
        assert_eq!(Renderer::text_width("hello", 1), 5 * 8);
    }

    #[test]
    fn text_height_is_cell_height() {
        assert_eq!(Renderer::text_height(1), 8);
        assert_eq!(Renderer::text_height(2), 16);
    }

    #[test]
    fn font_glyphs_are_sane() {
        assert_eq!(
            FONT8X8_BASIC['A' as usize],
            [0x0c, 0x1e, 0x33, 0x33, 0x3f, 0x33, 0x33, 0x00]
        );
        assert_eq!(
            FONT8X8_BASIC['W' as usize],
            [0x63, 0x63, 0x63, 0x6b, 0x7f, 0x77, 0x63, 0x00]
        );
        assert_eq!(FONT8X8_BASIC[' ' as usize], [0x00; 8]);
        assert_ne!(FONT8X8_BASIC['!' as usize], [0x00; 8]);
    }

    #[test]
    fn drawing_text_dirties_only_its_box() {
        let mut canvas = Canvas::new(40, 24);
        let mut r = Renderer::new(&mut canvas);
        let end = r.draw_text(4, 4, "A", BLACK, 2);
        assert_eq!(end, 4 + 16);
        let painted = Canvas::new(40, 24);
        let dirty = r.canvas().diff(&painted).unwrap();
        assert!(dirty.x >= 4 && dirty.y >= 4);
        assert!(dirty.right() <= 20 && dirty.bottom() <= 20);
    }

    #[test]
    fn non_ascii_is_a_box() {
        let mut canvas = Canvas::new(24, 16);
        let mut r = Renderer::new(&mut canvas);
        r.draw_text(0, 0, "\u{0642}", BLACK, 1);
        // The box is 0x7e on the top row: columns 1..=6 are inked, 0 and 7 are not.
        assert_eq!(r.canvas().pixel(0, 0), WHITE);
        assert_eq!(r.canvas().pixel(1, 0), BLACK);
        assert_eq!(r.canvas().pixel(6, 0), BLACK);
        assert_eq!(r.canvas().pixel(7, 0), WHITE);
    }
}
