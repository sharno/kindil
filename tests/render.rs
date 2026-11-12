//! Integration tests for the real renderer and the frame path.
//!
//! These drive `Renderer` and `Shell` exactly as the binary does, against a
//! `Canvas` and the in-memory `SimPanel` - no device required.

use kindil::canvas::Canvas;
use kindil::civil::DateTime;
use kindil::input::TouchEvent;
use kindil::render::{Renderer, BLACK, FONT_SCALE, WHITE};
use kindil::shell::Shell;
use kindil::sim::SimPanel;

fn date() -> DateTime {
    DateTime {
        year: 2026,
        month: 11,
        day: 11,
        hour: 22,
        minute: 30,
        second: 0,
        weekday: 3,
    }
}

#[test]
fn drawing_text_marks_pixels_black() {
    let mut canvas = Canvas::new(64, 32);
    let mut r = Renderer::new(&mut canvas);
    r.draw_text(0, 0, "A", BLACK, 2);

    // 'A' is [0x0c, 0x1e, 0x33, ...]; row 2 has bit 0 set, so with scale 2
    // the block at (0, 4) is ink.
    assert_eq!(canvas.pixel(0, 4), BLACK);
    assert_eq!(canvas.pixel(1, 5), BLACK);
}

#[test]
fn text_drawing_leaves_the_rest_white() {
    let mut canvas = Canvas::new(64, 32);
    let mut r = Renderer::new(&mut canvas);
    r.draw_text(0, 0, "A", BLACK, 2);
    assert_eq!(canvas.pixel(60, 0), WHITE);
    assert_eq!(canvas.pixel(0, 0), WHITE);
}

#[test]
fn glyph_advance_is_cell_width() {
    let mut canvas = Canvas::new(64, 32);
    let mut r = Renderer::new(&mut canvas);
    let end = r.draw_text(0, 0, "AB", BLACK, 2);
    assert_eq!(end, 32);
    // 'B' starts at x = 16; its top row 0x7e has bit 1 set.
    assert_eq!(canvas.pixel(18, 0), BLACK);
}

#[test]
fn scaling_doubles_each_pixel() {
    let mut canvas = Canvas::new(64, 32);
    let mut r = Renderer::new(&mut canvas);
    r.draw_text(0, 0, "A", BLACK, 2);
    for (x, y) in [(0, 4), (1, 4), (0, 5), (1, 5)] {
        assert_eq!(canvas.pixel(x, y), BLACK, "({x},{y})");
    }
    assert_eq!(canvas.pixel(0, 3), WHITE);
    // Row 2 of 'A' is 0x33: bits 0,1 and 4,5, so x = 4..7 is the gap.
    assert_eq!(canvas.pixel(4, 4), WHITE);
}

#[test]
fn text_width_matches_drawn_extent() {
    let width = Renderer::text_width("Hello", FONT_SCALE);
    assert_eq!(width, 5 * 8 * FONT_SCALE);

    let mut canvas = Canvas::new(256, 32);
    let mut r = Renderer::new(&mut canvas);
    let end = r.draw_text(0, 0, "Hello", BLACK, FONT_SCALE);
    assert_eq!(end, width);
}

#[test]
fn drawing_past_the_edge_is_clipped_not_wrapped() {
    let mut canvas = Canvas::new(20, 20);
    let mut r = Renderer::new(&mut canvas);
    // 'A' at x = 18 extends past the right edge.
    r.draw_text(18, 0, "A", BLACK, 2);
    // The first column of the glyph's ink row is at x = 18.
    assert_eq!(canvas.pixel(18, 4), BLACK);
    // Nothing wrapped to the next row at x = 0.
    assert_eq!(canvas.pixel(0, 1), WHITE);
    assert_eq!(canvas.pixel(0, 0), WHITE);
}

#[test]
fn a_rendered_frame_reaches_the_panel() {
    let mut shell = Shell::new(758, 1024);
    let mut panel = SimPanel::new(758, 1024);
    shell.draw(&date());
    shell.present(&mut panel).unwrap();

    // The clock is centred around a third of the way down, and the panel is
    // white at the very top.
    assert_eq!(panel.pixels()[0], WHITE);
    let ink = panel.pixels().iter().filter(|&&p| p == BLACK).count();
    assert!(ink > 1000, "the frame has only {ink} black pixels");
}

#[test]
fn tapping_the_panel_navigates() {
    let mut shell = Shell::new(758, 1024);
    let mut panel = SimPanel::new(758, 1024);
    shell.draw(&date());
    shell.present(&mut panel).unwrap();

    assert!(shell.handle_touch(TouchEvent::Release { x: 300, y: 500 }));
    assert_eq!(shell.screen(), kindil::shell::Screen::List);

    shell.draw(&date());
    shell.present(&mut panel).unwrap();

    let layout = shell.layout();
    assert!(shell.handle_touch(TouchEvent::Release {
        x: 300,
        y: (layout.list_top + layout.row_height) as i32,
    }));
    assert_eq!(shell.screen(), kindil::shell::Screen::Detail);
    assert_eq!(shell.selected(), 1);
}
