//! Software rendering into the EPDC framebuffer.
//!
//! The panel is 8-bit inverted grayscale, so we work in a single "ink" value
//! space where `0` is black and `0xFF` is white, matching the framebuffer's
//! native encoding. That keeps the renderer free of any colour-space
//! conversion: what we draw is what the panel shows.

use crate::fb::Framebuffer;
use crate::font::FONT8X8_BASIC;

/// Ink value for black.
pub const BLACK: u8 = 0x00;
/// Ink value for white.
pub const WHITE: u8 = 0xFF;

/// Height of a font cell in unscaled pixels.
pub const GLYPH_HEIGHT: u32 = 8;
/// Width of a font cell in unscaled pixels.
pub const GLYPH_WIDTH: u32 = 8;

/// The font scale used by the shell. The panel is 1024x758 at 212 DPI, so a
/// scale of 2 gives a 16x16 cell, which is roughly a 11pt text size.
pub const FONT_SCALE: u32 = 2;

/// A text renderer bound to a framebuffer.
pub struct Renderer<'a> {
    fb: &'a mut Framebuffer,
}

impl<'a> Renderer<'a> {
    /// Wrap a framebuffer for drawing.
    pub fn new(fb: &'a mut Framebuffer) -> Self {
        Renderer { fb }
    }

    /// The framebuffer being drawn into.
    pub fn framebuffer(&mut self) -> &mut Framebuffer {
        self.fb
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
        // The font only covers ASCII; anything else becomes a box.
        let glyph = if (ch as u32) < 128 {
            FONT8X8_BASIC[ch as usize]
        } else {
            // A hollow box, drawn as the 0xNN glyph row pattern.
            [0x7e, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x7e]
        };

        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..GLYPH_WIDTH {
                if bits & (1 << col) == 0 {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        self.fb
                            .set_pixel(x + col * scale + dx, y + row as u32 * scale + dy, ink);
                    }
                }
            }
        }
        x + GLYPH_WIDTH * scale
    }

    /// Fill a rectangle.
    pub fn fill_rect(&mut self, x: u32, y: u32, w: u32, h: u32, ink: u8) {
        for dy in 0..h {
            for dx in 0..w {
                self.fb.set_pixel(x + dx, y + dy, ink);
            }
        }
    }

    /// Draw a 1-pixel-wide rectangle outline.
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
        // 'A' in the font8x8_basic font.
        assert_eq!(
            FONT8X8_BASIC['A' as usize],
            [0x0c, 0x1e, 0x33, 0x33, 0x3f, 0x33, 0x33, 0x00]
        );
        // 'W'
        assert_eq!(
            FONT8X8_BASIC['W' as usize],
            [0x63, 0x63, 0x63, 0x6b, 0x7f, 0x77, 0x63, 0x00]
        );
        // Space is blank, '!' is not.
        assert_eq!(FONT8X8_BASIC[' ' as usize], [0x00; 8]);
        assert_ne!(FONT8X8_BASIC['!' as usize], [0x00; 8]);
    }

    #[test]
    fn all_glyph_rows_have_eight_bytes() {
        for (i, g) in FONT8X8_BASIC.iter().enumerate() {
            assert_eq!(g.len(), 8, "glyph {i} has wrong length");
        }
    }
}
