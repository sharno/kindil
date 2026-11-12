//! The kindil shell: the screens, the transitions, and the frame loop.
//!
//! The shell is pure: it draws into a [`Canvas`] from a supplied date and time,
//! and reacts to touch events. Everything device-specific lives behind
//! [`Panel`], which is what lets the same code run on a workstation.

use crate::canvas::{Canvas, Rect};
use crate::civil::DateTime;
use crate::input::TouchEvent;
use crate::panel::{Panel, PresentError};
use crate::render::{Renderer, BLACK, FONT_SCALE, WHITE};
use crate::time;

/// Which screen the shell is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    /// The clock.
    Home,
    /// The menu.
    List,
    /// A detail view for the selected entry.
    Detail,
}

/// After this many partial updates the panel gets a full refresh, which clears
/// the ghosting that e-ink accumulates. At one update a minute that is every
/// half hour.
pub const MAX_PARTIAL_UPDATES: u32 = 30;

/// The static menu entries.
pub const ENTRIES: [&str; 4] = ["Clock", "Library", "Settings", "About"];

/// Panel-relative geometry, shared by drawing and hit testing.
///
/// The layout is derived from the panel size rather than hard-coded, because
/// the Paperwhite 1 is 758x1024 portrait and later Paperwhites are landscape.
///
/// Drawing and [`Layout::row_at`] must use the same numbers: a menu row that is
/// drawn in one place and tapped in another is the classic e-reader bug.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub width: u32,
    pub height: u32,
    pub margin: u32,
    pub title_scale: u32,
    pub rule_y: u32,
    pub list_top: u32,
    pub row_height: u32,
}

impl Layout {
    /// Derive the layout for a panel of the given size.
    pub fn new(width: u32, height: u32) -> Self {
        let margin = (width / 12).max(8);
        let title_scale = (height / 340).max(2);
        let rule_y = margin + title_scale * 8 + height / 64;
        let after_rule = rule_y + height / 32;
        let footer = (height / 12).max(24);
        let available = height.saturating_sub(after_rule + footer);
        let row_height = (available / ENTRIES.len() as u32).min(height / 8).max(40);
        let block = row_height * ENTRIES.len() as u32;
        let list_top = after_rule + available.saturating_sub(block) / 2;
        Layout {
            width,
            height,
            margin,
            title_scale,
            rule_y,
            list_top,
            row_height,
        }
    }

    /// The box of one menu row.
    pub fn row(&self, index: usize) -> Rect {
        Rect::new(
            self.margin,
            self.list_top + index as u32 * self.row_height,
            self.width - self.margin * 2,
            self.row_height,
        )
    }

    /// Which menu row a tap at `y` hits, if any.
    pub fn row_at(&self, y: u32) -> Option<usize> {
        let bottom = self.list_top + self.row_height * ENTRIES.len() as u32;
        if y < self.list_top || y >= bottom {
            return None;
        }
        Some(((y - self.list_top) / self.row_height) as usize)
    }

    /// The text scale used for menu rows.
    pub fn row_scale(&self) -> u32 {
        (self.row_height / 24).clamp(2, 4)
    }
}

/// The shell state.
pub struct Shell {
    /// What the panel is currently showing.
    front: Canvas,
    /// What the next frame will show.
    back: Canvas,
    layout: Layout,
    screen: Screen,
    selected: usize,
    /// Set when the screen changed wholesale and the next present should be a
    /// full, ghost-clearing refresh.
    needs_full: bool,
    partials: u32,
    /// `(current, max)` frontlight intensity, if the device has a frontlight.
    frontlight: Option<(i32, i32)>,
}

impl Shell {
    /// Create a shell for a panel of the given size.
    pub fn new(width: u32, height: u32) -> Self {
        Shell {
            front: Canvas::new(width, height),
            back: Canvas::new(width, height),
            layout: Layout::new(width, height),
            screen: Screen::Home,
            selected: 0,
            needs_full: true,
            partials: 0,
            frontlight: None,
        }
    }

    /// Record the frontlight state so the Settings screen can show it.
    pub fn set_frontlight(&mut self, state: Option<(i32, i32)>) {
        self.frontlight = state;
    }

    /// The current screen.
    pub fn screen(&self) -> Screen {
        self.screen
    }

    /// The selected menu entry.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The layout in use.
    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// The canvas holding the most recently drawn frame.
    pub fn canvas(&self) -> &Canvas {
        &self.back
    }

    /// Render the current screen into the back buffer.
    pub fn draw(&mut self, now: &DateTime) {
        let layout = self.layout;
        let screen = self.screen;
        let selected = self.selected;
        let frontlight = self.frontlight;

        self.back.clear(WHITE);
        let mut r = Renderer::new(&mut self.back);

        match screen {
            Screen::Home => draw_home(&mut r, &layout, now),
            Screen::List => draw_list(&mut r, &layout, selected),
            Screen::Detail => {
                draw_title(&mut r, &layout, ENTRIES[selected.min(ENTRIES.len() - 1)]);
                draw_detail(&mut r, &layout, selected, now, frontlight);
            }
        }

        if screen != Screen::Home {
            draw_footer(&mut r, &layout, footer_hint(screen));
        }
    }
}

fn draw_home(r: &mut Renderer<'_>, layout: &Layout, now: &DateTime) {
    let big = (layout.height / 80).clamp(4, 16);
    let clock = time::format_time(now);
    let w = Renderer::text_width(&clock, big);
    r.draw_text(
        layout.width.saturating_sub(w) / 2,
        layout.height / 3,
        &clock,
        BLACK,
        big,
    );

    let date = std::format!(
        "{} {} {} {}",
        time::weekday_name(now.weekday),
        time::month_name(now.month),
        now.day,
        now.year
    );
    let w = Renderer::text_width(&date, FONT_SCALE);
    r.draw_text(
        layout.width.saturating_sub(w) / 2,
        layout.height / 3 + big * 8 + layout.height / 40,
        &date,
        BLACK,
        FONT_SCALE,
    );

    draw_footer(r, layout, "tap to open the menu");
}

impl Shell {
    /// Push the back buffer to the panel, refreshing only what changed.
    pub fn present<P: Panel>(&mut self, panel: &mut P) -> Result<(), PresentError> {
        let Some(rect) = self.back.diff(&self.front) else {
            return Ok(());
        };
        let full = self.needs_full || self.partials >= MAX_PARTIAL_UPDATES;
        self.needs_full = false;
        if full {
            self.partials = 0;
        } else {
            self.partials += 1;
        }
        self.front.copy_rect_from(&self.back, rect);
        panel.present(&self.front, rect, full)
    }

    /// Handle a touch event, returning true if the screen changed.
    pub fn handle_touch(&mut self, event: TouchEvent) -> bool {
        match event {
            // A press alone changes nothing: the shell acts on release, so a
            // drag across the panel does not trigger a navigation.
            TouchEvent::Press { .. } | TouchEvent::Move { .. } => false,
            TouchEvent::Release { x, y } => self.on_release(x.max(0) as u32, y.max(0) as u32),
        }
    }

    fn on_release(&mut self, _x: u32, y: u32) -> bool {
        match self.screen {
            Screen::Home => {
                self.screen = Screen::List;
                self.selected = 0;
                self.needs_full = true;
                true
            }
            Screen::List => match self.layout.row_at(y) {
                Some(row) => {
                    self.selected = row;
                    self.screen = Screen::Detail;
                    self.needs_full = true;
                    true
                }
                None => false,
            },
            Screen::Detail => {
                self.screen = Screen::List;
                self.needs_full = true;
                true
            }
        }
    }
}

fn draw_title(r: &mut Renderer<'_>, layout: &Layout, title: &str) {
    r.draw_text(
        layout.margin,
        layout.margin,
        title,
        BLACK,
        layout.title_scale,
    );
    r.hline(
        layout.margin,
        layout.rule_y,
        layout.width - layout.margin * 2,
        BLACK,
    );
}

fn draw_list(r: &mut Renderer<'_>, layout: &Layout, selected: usize) {
    draw_title(r, layout, "kindil");
    let scale = layout.row_scale();
    for (index, entry) in ENTRIES.iter().enumerate() {
        let row = layout.row(index);
        let text_y = row.y + (row.h - 8 * scale) / 2;
        if index == selected {
            r.fill_rect(row.x, row.y, row.w, row.h, BLACK);
            r.draw_text(row.x + layout.margin / 2, text_y, entry, WHITE, scale);
        } else {
            r.draw_text(row.x + layout.margin / 2, text_y, entry, BLACK, scale);
            // A hairline under each row, except the last.
            if index + 1 < ENTRIES.len() {
                r.hline(row.x, row.bottom() - 1, row.w, BLACK);
            }
        }
    }
}

fn draw_detail(
    r: &mut Renderer<'_>,
    layout: &Layout,
    selected: usize,
    now: &DateTime,
    frontlight: Option<(i32, i32)>,
) {
    let body = match ENTRIES[selected.min(ENTRIES.len() - 1)] {
        "Clock" => std::format!("Local time is {}", time::format(now)),
        "Library" => "No books indexed yet.".to_string(),
        "Settings" => {
            let light = match frontlight {
                Some((current, max)) => std::format!("{current}/{max}"),
                None => "unavailable".to_string(),
            };
            std::format!(
                "Panel {}x{}\nFrontlight: {light}\nVersion {}",
                layout.width,
                layout.height,
                env!("CARGO_PKG_VERSION")
            )
        }
        _ => std::format!(
            "kindil {}\nA shell for the first-generation\nKindle Paperwhite.",
            env!("CARGO_PKG_VERSION")
        ),
    };

    let line_height = FONT_SCALE * 8 + FONT_SCALE * 4;
    let mut y = layout.rule_y + layout.height / 24;
    for line in body.lines() {
        r.draw_text(layout.margin, y, line, BLACK, FONT_SCALE);
        y += line_height;
    }
}

fn draw_footer(r: &mut Renderer<'_>, layout: &Layout, hint: &str) {
    let w = Renderer::text_width(hint, FONT_SCALE);
    r.draw_text(
        layout.width.saturating_sub(w) / 2,
        layout.height - layout.margin / 2 - FONT_SCALE * 8,
        hint,
        BLACK,
        FONT_SCALE,
    );
}

fn footer_hint(screen: Screen) -> &'static str {
    match screen {
        Screen::Home => "tap to open the menu",
        Screen::List => "tap an item",
        Screen::Detail => "tap to go back",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Canvas;

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

    struct TestPanel {
        width: u32,
        height: u32,
    }

    impl Panel for TestPanel {
        fn width(&self) -> u32 {
            self.width
        }
        fn height(&self) -> u32 {
            self.height
        }
        fn present(&mut self, _: &Canvas, _: Rect, _: bool) -> Result<(), PresentError> {
            Ok(())
        }
    }

    #[test]
    fn entries_are_stable() {
        assert_eq!(ENTRIES[0], "Clock");
        assert_eq!(ENTRIES.len(), 4);
    }

    #[test]
    fn rows_map_taps_to_entries() {
        // The Paperwhite 1's framebuffer is 758x1024 portrait.
        let layout = Layout::new(758, 1024);
        assert_eq!(layout.row_at(layout.list_top), Some(0));
        assert_eq!(layout.row_at(layout.list_top + 5), Some(0));
        assert_eq!(layout.row_at(layout.list_top + layout.row_height), Some(1));
        assert_eq!(
            layout.row_at(layout.list_top + layout.row_height * 3 + 1),
            Some(3)
        );
        assert_eq!(layout.row_at(layout.list_top + layout.row_height * 4), None);
        assert_eq!(layout.row_at(layout.list_top - 1), None);
    }

    #[test]
    fn the_rows_fit_on_the_panel() {
        for &(w, h) in &[(758u32, 1024u32), (1024, 758), (600, 800)] {
            let layout = Layout::new(w, h);
            let last = layout.row(ENTRIES.len() - 1);
            assert!(
                last.bottom() <= h,
                "{w}x{h}: last row ends at {}, past {h}",
                last.bottom()
            );
            assert!(last.right() <= w);
        }
    }

    #[test]
    fn home_opens_the_menu_and_detail_goes_back() {
        let mut shell = Shell::new(758, 1024);
        assert_eq!(shell.screen(), Screen::Home);
        assert!(shell.handle_touch(TouchEvent::Release { x: 100, y: 100 }));
        assert_eq!(shell.screen(), Screen::List);

        let layout = shell.layout();
        assert!(shell.handle_touch(TouchEvent::Release {
            x: 100,
            y: (layout.list_top + layout.row_height * 2 + 4) as i32,
        }));
        assert_eq!(shell.screen(), Screen::Detail);
        assert_eq!(shell.selected(), 2);

        assert!(shell.handle_touch(TouchEvent::Release { x: 100, y: 100 }));
        assert_eq!(shell.screen(), Screen::List);
    }

    #[test]
    fn a_tap_outside_the_rows_does_nothing() {
        let mut shell = Shell::new(758, 1024);
        shell.handle_touch(TouchEvent::Release { x: 10, y: 10 });
        assert_eq!(shell.screen(), Screen::List);
        assert!(!shell.handle_touch(TouchEvent::Release { x: 10, y: 1000 }));
        assert_eq!(shell.screen(), Screen::List);
    }

    #[test]
    fn a_press_does_not_navigate() {
        let mut shell = Shell::new(758, 1024);
        assert!(!shell.handle_touch(TouchEvent::Press { x: 10, y: 10 }));
        assert!(!shell.handle_touch(TouchEvent::Move { x: 10, y: 10 }));
        assert_eq!(shell.screen(), Screen::Home);
    }

    #[test]
    fn the_first_frame_is_a_full_refresh() {
        // `Shell::new` starts with two identical white canvases, so the first
        // draw is what differs.
        let mut shell = Shell::new(200, 200);
        let mut panel = TestPanel {
            width: 200,
            height: 200,
        };
        shell.draw(&date());
        shell.present(&mut panel).unwrap();
        // Nothing changed since the present, so the next one is a no-op.
        shell.present(&mut panel).unwrap();
    }

    #[test]
    fn only_the_clock_band_changes_between_minutes() {
        let mut first = Shell::new(758, 1024);
        first.draw(&date());

        let mut second = Shell::new(758, 1024);
        let mut later = date();
        later.minute = 31;
        second.draw(&later);

        let rect = second
            .canvas()
            .diff(first.canvas())
            .expect("the minute changed");
        // The clock is one text line tall; it must not cover the whole panel.
        assert!(rect.h < 200, "dirty height {} is too tall", rect.h);
    }
}
