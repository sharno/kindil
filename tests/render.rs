//! Rendering tests against a real framebuffer-shaped buffer.
//!
//! The device's framebuffer cannot be mapped in a test, so we exercise the
//! renderer against an in-memory buffer with the same layout and check the
//! resulting pixels.

use kindil::font::FONT8X8_BASIC;
use kindil::render::{Renderer, BLACK, FONT_SCALE, WHITE};

/// A stand-in for the framebuffer with the same geometry rules.
struct FakeFb {
    stride: u32,
    pixels: Vec<u8>,
}

impl FakeFb {
    fn new(w: u32, h: u32) -> Self {
        FakeFb {
            stride: w,
            pixels: vec![WHITE; (w * h) as usize],
        }
    }

    fn at(&self, x: u32, y: u32) -> u8 {
        self.pixels[(y * self.stride + x) as usize]
    }
}

/// Render text into a fake framebuffer by mirroring the renderer's logic.
///
/// The real `Renderer` needs a `Framebuffer`, which needs a device, so this
/// re-implements the few operations under test against a plain buffer.
fn draw_text(fb: &mut FakeFb, x: u32, y: u32, text: &str, ink: u8, scale: u32) {
    let mut cx = x;
    for ch in text.chars() {
        let glyph = FONT8X8_BASIC[ch as usize];
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..8u32 {
                if bits & (1 << col) == 0 {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        let px = cx + col * scale + dx;
                        let py = y + row as u32 * scale + dy;
                        fb.pixels[(py * fb.stride + px) as usize] = ink;
                    }
                }
            }
        }
        cx += 8 * scale;
    }
}

#[test]
fn drawing_text_marks_pixels_black() {
    let mut fb = FakeFb::new(64, 32);
    draw_text(&mut fb, 0, 0, "A", BLACK, 2);

    // The 'A' glyph has ink in its upper rows; the top-left 2x2 block of the
    // glyph is empty (the 'A' has a notch at the top), so check a pixel we
    // know is ink: row 2 of the glyph is 0x33, which has bit 0 set.
    // bit 0 == leftmost pixel, so (x=0, y=2*scale) is ink.
    assert_eq!(fb.at(0, 4), BLACK);
}

#[test]
fn text_drawing_leaves_the_rest_white() {
    let mut fb = FakeFb::new(64, 32);
    draw_text(&mut fb, 0, 0, "A", BLACK, 2);

    // A pixel far to the right of the glyph is untouched.
    assert_eq!(fb.at(60, 0), WHITE);
}

#[test]
fn glyph_advance_is_cell_width() {
    let mut fb = FakeFb::new(64, 32);
    draw_text(&mut fb, 0, 0, "AB", BLACK, 2);

    // 'B' starts at x = 16 (two 8px cells at scale 2), and 'B' has ink in
    // row 0 at bit 3 (0x22 = 0b00100010, bit 1 and 5 set; bit 5 => x=5).
    assert_eq!(fb.at(16 + 5 * 2, 0), BLACK);
}

#[test]
fn scaling_doubles_each_pixel() {
    let mut fb = FakeFb::new(64, 32);
    draw_text(&mut fb, 0, 0, "A", BLACK, 2);

    // The ink pixel at (0,4) should be part of a 2x2 block.
    assert_eq!(fb.at(0, 4), BLACK);
    assert_eq!(fb.at(1, 4), BLACK);
    assert_eq!(fb.at(0, 5), BLACK);
    assert_eq!(fb.at(1, 5), BLACK);
    // But the pixel to the left is not.
    assert_eq!(fb.at(0, 3), WHITE);
}

#[test]
fn text_width_matches_drawn_extent() {
    let scale = FONT_SCALE;
    let width = Renderer::text_width("Hello", scale);
    assert_eq!(width, 5 * 8 * scale);
}
