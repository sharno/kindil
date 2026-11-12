//! The kindil shell: a single process that owns the panel, the clock and the
//! touch input.
//!
//! This is the "one process runs the whole system" shape. There is no
//! init, no window manager and no compositor; the shell owns the framebuffer,
//! pushes updates to the EPDC, and reacts to touch events itself.

use crate::epdc::{Epdc, WAVEFORM_MODE_AUTO, WAVEFORM_MODE_GC16};
use crate::fb::Framebuffer;
use crate::frontlight::{self, Frontlight};
use crate::input::TouchEvent;
use crate::render::{Renderer, BLACK, FONT_SCALE, WHITE};
use crate::time;

/// Which screen the shell is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    /// The clock and status.
    Home,
    /// A list of entries.
    List,
    /// A detail view for the selected entry.
    Detail,
}

/// The shell.
pub struct Shell {
    epdc: Epdc,
    fb: Framebuffer,
    screen: Screen,
    selected: usize,
    /// Set when the panel needs a full refresh to clear ghosting.
    needs_full_refresh: bool,
}

/// The top edge of the first list row, as a fraction of the panel height.
const LIST_TOP_FRACTION: f32 = 0.14;
/// The height of one list row, as a fraction of the panel height.
const LIST_ROW_FRACTION: f32 = 0.06;

/// Which list row a tap at `y` selects, if any.
///
/// The rows are laid out proportionally to the panel height, because the
/// Paperwhite 1's framebuffer is 758x1024 portrait while the later models
/// are landscape. Deriving the layout from the framebuffer keeps one code
/// path across all of them.
///
/// This is a pure function of the tap position, so it can be tested without a
/// device.
pub fn row_at(y: u32, panel_height: u32) -> Option<usize> {
    let top = (panel_height as f32 * LIST_TOP_FRACTION) as u32;
    let row = (panel_height as f32 * LIST_ROW_FRACTION) as u32;
    if row == 0 || y < top || y >= top + row * Shell::entries().len() as u32 {
        return None;
    }
    Some(((y - top) / row) as usize)
}

impl Shell {
    /// Create the shell, taking ownership of the panel.
    pub fn new(epdc: Epdc, fb: Framebuffer) -> Self {
        Shell {
            epdc,
            fb,
            screen: Screen::Home,
            selected: 0,
            needs_full_refresh: true,
        }
    }

    /// The current screen.
    pub fn screen(&self) -> Screen {
        self.screen
    }

    /// The currently selected list entry.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The static menu entries.
    fn entries() -> &'static [&'static str] {
        &["Clock", "Library", "Settings", "About"]
    }

    /// Draw the current screen into the framebuffer.
    pub fn draw(&mut self) {
        let entries = Self::entries();
        let selected = self.selected;
        let screen = self.screen;
        let now = time::now();
        let time_text = time::format(&now);

        let (width, height) = (self.fb.width(), self.fb.height());

        {
            let fb = &mut self.fb;
            let mut r = Renderer::new(fb);

            // Background: white paper.
            r.fill_rect(0, 0, width, height, WHITE);

            // Margins and the list geometry scale with the panel.
            let margin = width / 12;
            let list_top = (height as f32 * LIST_TOP_FRACTION) as u32;
            let row_height = (height as f32 * LIST_ROW_FRACTION) as u32;
            let title_height = height / 16;

            match screen {
                Screen::Home => {
                    // A large clock, sized to the panel.
                    let big = (height / 90).max(2);
                    let clock = format!("{:02}:{:02}", now.hour, now.minute);
                    let w = Renderer::text_width(&clock, big);
                    r.draw_text(width.saturating_sub(w) / 2, height / 4, &clock, BLACK, big);

                    let date = format!(
                        "{} {} {:02} {}",
                        time::weekday_name(now.weekday),
                        time::month_name(now.month),
                        now.day,
                        now.year
                    );
                    let w = Renderer::text_width(&date, FONT_SCALE);
                    r.draw_text(
                        width.saturating_sub(w) / 2,
                        height / 2,
                        &date,
                        BLACK,
                        FONT_SCALE,
                    );

                    let hint = "tap for menu";
                    let w = Renderer::text_width(hint, FONT_SCALE);
                    r.draw_text(
                        width.saturating_sub(w) / 2,
                        height - FONT_SCALE * 8 - margin,
                        hint,
                        BLACK,
                        FONT_SCALE,
                    );
                }
                Screen::List => {
                    r.draw_text(margin, margin, "kindil", BLACK, FONT_SCALE + 1);
                    r.hline(margin, margin + title_height, width - margin * 2, BLACK);

                    let mut y = list_top;
                    for (i, entry) in entries.iter().enumerate() {
                        if i == selected {
                            r.fill_rect(
                                margin / 2,
                                y - row_height / 4,
                                width - margin,
                                row_height,
                                BLACK,
                            );
                            r.draw_text(margin, y, entry, WHITE, FONT_SCALE + 1);
                        } else {
                            r.draw_text(margin, y, entry, BLACK, FONT_SCALE + 1);
                        }
                        y += row_height;
                    }
                }
                Screen::Detail => {
                    let title = entries[selected.min(entries.len() - 1)];
                    r.draw_text(margin, margin, title, BLACK, FONT_SCALE + 1);
                    r.hline(margin, margin + title_height, width - margin * 2, BLACK);

                    let body = match title {
                        "Clock" => format!("Local time is {time_text}"),
                        "Library" => "No books indexed yet.".to_string(),
                        "Settings" => format!(
                            "Panel {}x{}\nFrontlight: {}",
                            width,
                            height,
                            frontlight_state()
                        ),
                        _ => "kindil - a shell for the Paperwhite".to_string(),
                    };
                    let mut y = list_top;
                    for line in body.lines() {
                        r.draw_text(margin, y, line, BLACK, FONT_SCALE);
                        y += FONT_SCALE * 8 + FONT_SCALE;
                    }

                    let hint = "tap to go back";
                    let w = Renderer::text_width(hint, FONT_SCALE);
                    r.draw_text(
                        width.saturating_sub(w) / 2,
                        height - FONT_SCALE * 8 - margin,
                        hint,
                        BLACK,
                        FONT_SCALE,
                    );
                }
            }
        }

        self.push_update();
    }

    /// Push the framebuffer to the panel.
    ///
    /// A full refresh is used when the screen changed wholesale, which also
    /// clears the ghosting that e-ink accumulates; otherwise a partial update
    /// of the whole panel is used, which is fast and low-power.
    fn push_update(&mut self) {
        // The update region is in framebuffer coordinates, so it follows the
        // panel's own geometry rather than any hard-coded size.
        let region = self.fb.full_region();
        let full = self.needs_full_refresh;
        self.needs_full_refresh = false;

        // GC16 is the full-fidelity, flashing waveform; it is what clears
        // ghosting. A partial update uses the driver's automatic choice.
        let waveform = if full {
            WAVEFORM_MODE_GC16
        } else {
            WAVEFORM_MODE_AUTO
        };

        if let Err(e) = self.epdc.send_update(region, waveform, full) {
            eprintln!("kindil: send_update failed: {e}");
            return;
        }
        if let Err(e) = self.epdc.wait_for_update_complete() {
            eprintln!("kindil: wait_for_update_complete failed: {e}");
        }
    }

    /// Handle a touch event, returning true if the display needs redrawing.
    pub fn handle_touch(&mut self, event: TouchEvent) -> bool {
        match event {
            // A press alone does not change anything; the shell acts on
            // release so a drag does not trigger a navigation.
            TouchEvent::Press { .. } => false,
            TouchEvent::Release { x, y } => {
                self.on_release(x, y);
                true
            }
            TouchEvent::Move { .. } => false,
        }
    }

    /// React to a tap.
    fn on_release(&mut self, x: i32, y: i32) {
        let x = x as u32;
        let y = y as u32;

        match self.screen {
            Screen::Home => {
                self.screen = Screen::List;
                self.selected = 0;
                self.needs_full_refresh = true;
            }
            Screen::List => {
                if let Some(row) = row_at(y, self.fb.height()) {
                    self.selected = row;
                    self.screen = Screen::Detail;
                    self.needs_full_refresh = true;
                }
            }
            Screen::Detail => {
                self.screen = Screen::List;
                self.needs_full_refresh = true;
            }
        }
        let _ = x;
    }
}

/// Read the frontlight brightness.
///
/// The Kindle exposes the frontlight as a misc character device, `/dev/yoshime_fl`
/// on the Paperwhite 1, driven with the ioctls in `<linux/yoshime_fl.h>`. We
/// try that first and fall back to the driver's sysfs attribute.
fn frontlight_state() -> String {
    match Frontlight::open() {
        Ok(fl) => match (fl.intensity(), fl.max_intensity()) {
            (Ok(level), Ok(max)) => format!("{level}/{max}"),
            (Ok(level), Err(_)) => level.to_string(),
            (Err(e), _) => format!("unavailable ({e})"),
        },
        Err(e) => match frontlight::sysfs_intensity() {
            Some(level) => level.to_string(),
            None => format!("unavailable ({e})"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_are_stable() {
        assert_eq!(Shell::entries().len(), 4);
        assert_eq!(Shell::entries()[0], "Clock");
    }

    #[test]
    fn list_rows_map_taps_to_entries() {
        // The Paperwhite 1's framebuffer is 758x1024 portrait.
        let height = 1024u32;
        let top = (height as f32 * LIST_TOP_FRACTION) as u32;
        let row = (height as f32 * LIST_ROW_FRACTION) as u32;

        assert_eq!(row_at(top, height), Some(0));
        assert_eq!(row_at(top + 10, height), Some(0));
        assert_eq!(row_at(top + row, height), Some(1));
        assert_eq!(row_at(top + row + 30, height), Some(1));
        assert_eq!(row_at(top + row * 3, height), Some(3));
        // Past the last row.
        assert_eq!(row_at(top + row * 4, height), None);
        // Above the list.
        assert_eq!(row_at(top - 1, height), None);
    }
}
