//! The dashboard as SVG, laid out for the Kindle Paperwhite 1 (758×1024, portrait).
//! Pure: everything it shows comes in through [`Frame`].

use std::fmt::{self, Display, Write as _};

use chrono::{DateTime, Datelike, Duration, Timelike, Utc, Weekday};
use chrono_tz::Tz;

use crate::config::{ClockFormat, Config};
use crate::hijri::HijriDate;
use crate::prayer::{DayTimes, Salah, Upcoming};
use crate::radar::RadarImage;
use crate::sources::Snapshot;
use crate::weather::{
    Condition, DayRange, Hour, Light, Quarter, RAIN_QUARTERS, Sky, WET_MM, Weather,
};

pub const WIDTH: u32 = 758;
pub const HEIGHT: u32 = 1024;
/// Where the radar raster goes on a [`Page::FULL`] page when rain is
/// expected; the SVG draws its card on top. See [`Page::radar_top`].
pub const RADAR_LEFT: i64 = 452;
pub const RADAR_TOP: i64 = 638;
pub const RADAR_SIZE: (u32, u32) = (276, 276);

const MARGIN: f64 = 28.0;
const RIGHT: f64 = WIDTH as f64 - MARGIN;
const MUTED: &str = "#666";

/// The rows a [`Page::FULL`] header (clock, date, next prayer, countdown and
/// its divider) draws in, and nothing else does: [`header_svg`] renders just
/// these, the same pixels as the whole page has there. See
/// [`Page::header_height`].
pub const HEADER_HEIGHT: u32 = 300;
/// The timetable starts under the header divider.
const PRAYERS_TOP: f64 = 302.0;
/// The next prayer's highlight starts 3 px into the timetable: below the header.
const _: () = assert!(PRAYERS_TOP + 3.0 > HEADER_HEIGHT as f64);
/// Two columns of three rows: the six times fit in half the height they used to.
const PRAYER_ROW: f64 = 46.0;
const COLUMN_WIDTH: f64 = 336.0;
const COLUMN_GAP: f64 = 30.0;
const PRAYERS_BOTTOM: f64 = PRAYERS_TOP + 3.0 * PRAYER_ROW;
/// Where the hourly strip starts, below the prayer grid's divider.
const HOURLY_TOP: f64 = 460.0;
/// The prayer grid and its divider must clear the hourly strip.
const _: () = assert!(PRAYERS_BOTTOM + 8.0 < HOURLY_TOP);
/// Data older than this gets an "as of" note.
const STALE_AFTER: Duration = Duration::minutes(45);

/// The canvas the dashboard draws on: the whole panel, or the panel under a
/// host's bar across the top (kindil's status bar), [`WIDTH`] wide either
/// way. A shorter page keeps every element at its size and tightens the
/// spacing between them instead: [`Page::MAX_INSET`] rows come out of the
/// header's air, the timetable's rows and the bottom margin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Page {
    height: u32,
}

impl Default for Page {
    fn default() -> Self {
        Self::FULL
    }
}

impl Page {
    /// The whole 758×1024 panel, as Miqat draws on its own.
    pub const FULL: Self = Self { height: HEIGHT };
    /// The most a page can give up and still fit everything.
    pub const MAX_INSET: u32 = 56;

    /// The page under `inset` rows of someone else's, up to [`Self::MAX_INSET`].
    pub const fn below(inset: u32) -> Self {
        let inset = if inset > Self::MAX_INSET {
            Self::MAX_INSET
        } else {
            inset
        };
        Self {
            height: HEIGHT - inset,
        }
    }

    pub const fn height(self) -> u32 {
        self.height
    }

    /// The rows the header draws in: [`HEADER_HEIGHT`] on a full page, less
    /// on a shorter one. [`header_svg`] renders exactly these.
    pub const fn header_height(self) -> u32 {
        HEADER_HEIGHT - self.header_cut()
    }

    /// Where the radar raster goes, in this page's rows.
    pub const fn radar_top(self) -> i64 {
        RADAR_TOP - self.body_cut() as i64
    }

    const fn inset(self) -> u32 {
        HEIGHT - self.height
    }

    /// Of [`Self::MAX_INSET`]: 6 from above the clock, 12 from between the
    /// names and the countdown...
    const fn header_cut(self) -> u32 {
        (self.inset() * 18 + Self::MAX_INSET / 2) / Self::MAX_INSET
    }

    /// ...6 more from the timetable's rows, so everything under it moves
    /// up by 24...
    const fn body_cut(self) -> u32 {
        (self.inset() * 24 + Self::MAX_INSET / 2) / Self::MAX_INSET
    }

    /// ...and the rest from the margin under the rain line.
    fn geometry(self) -> Geometry {
        let inset = f64::from(self.inset());
        let share = inset / f64::from(Self::MAX_INSET);
        let header_cut = f64::from(self.header_cut());
        let body_cut = f64::from(self.body_cut());
        let prayers_top = PRAYERS_TOP - header_cut;
        Geometry {
            names: -6.0 * share,
            hero: -header_cut,
            prayers_top,
            prayer_row: PRAYER_ROW - (body_cut - header_cut) / 3.0,
            body: -body_cut,
            tomorrow: -8.0 * share,
            // 56 under it on a full page, 32 at the most inset.
            footer: f64::from(self.height) - 56.0 + 24.0 * share,
        }
    }
}

/// Where things go on a [`Page`]: offsets from the full page's positions.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Geometry {
    /// The clock, dates and the next prayer's names.
    names: f64,
    /// The countdown and the header's divider.
    hero: f64,
    prayers_top: f64,
    prayer_row: f64,
    /// Everything from the hourly strip down.
    body: f64,
    /// Tomorrow beside the radar, on top of `body`.
    tomorrow: f64,
    /// The rain line's baseline.
    footer: f64,
}

impl Geometry {
    fn prayers_bottom(&self) -> f64 {
        self.prayers_top + 3.0 * self.prayer_row
    }
}

pub struct Frame<'a> {
    pub now: DateTime<Utc>,
    pub config: &'a Config,
    pub prayers: &'a DayTimes,
    pub weather: Option<&'a Snapshot<Weather>>,
    pub radar: Option<&'a Snapshot<RadarImage>>,
    /// The canvas: [`Page::FULL`] unless a host draws above it.
    pub page: Page,
}

impl Frame<'_> {
    /// The radar earns its space (and its fetch) only when rain is coming.
    pub fn rain_expected(&self) -> bool {
        self.weather
            .is_some_and(|snapshot| snapshot.data.rain_expected(self.now))
    }
}

pub fn svg(frame: &Frame) -> String {
    document(
        frame.page.height(),
        &[header(frame), prayers(frame), weather(frame)].concat(),
    )
}

/// Only the header, on a canvas [`Page::header_height`] tall: the top of
/// [`svg`]'s page, for redrawing the clock and countdown alone. Uses only
/// the frame's time, config, prayers and page.
pub fn header_svg(frame: &Frame) -> String {
    document(frame.page.header_height(), &header(frame))
}

fn document(height: u32, body: &str) -> String {
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{height}" viewBox="0 0 {WIDTH} {height}" font-family="IBM Plex Sans Arabic">{body}</svg>"#
    )
}

/// A small clock, the next prayer's names, then the remaining time as the largest
/// element on the page.
fn header(frame: &Frame) -> String {
    let tz = frame.config.location.timezone;
    let upcoming = frame.prayers.upcoming(frame.now);
    let (english, arabic) = upcoming.prayer.names(frame.prayers.date.weekday());
    let hijri = HijriDate::from_gregorian(
        frame.prayers.islamic_date(frame.now),
        frame.config.prayer.hijri_offset_days,
    );
    let date = frame
        .now
        .with_timezone(&tz)
        .format("%A, %-d %B")
        .to_string();
    let (clock, suffix) = clock_parts(frame.now, tz, frame.config.display.clock);
    let g = frame.page.geometry();
    let y = |full: f64| full + g.names;
    // The left column has to stay clear of the right-anchored prayer name,
    // whose widest form ("Jumu'ah") reaches x 373. The Hijri line ends at 332
    // on the longest month; adding the Arabic form again would run to 516.
    [
        text(MARGIN - 4.0, y(66.0), 72, &clock)
            .weight(700)
            .to_string(),
        suffix.map_or_else(String::new, |s| {
            text(MARGIN + 4.0 + numeric_width(&clock, 72.0), y(66.0), 21, s)
                .weight(600)
                .to_string()
        }),
        text(MARGIN, y(100.0), 22, &date).weight(600).to_string(),
        text(MARGIN, y(130.0), 22, &hijri.english())
            .weight(600)
            .to_string(),
        text(RIGHT, y(48.0), 32, arabic)
            .weight(600)
            .end()
            .to_string(),
        text(RIGHT, y(122.0), 88, english)
            .weight(700)
            .end()
            .to_string(),
        countdown_hero(
            f64::from(WIDTH) / 2.0,
            268.0 + g.hero,
            &countdown(frame.now, upcoming.at),
        ),
        hline(292.0 + g.hero, 2.0),
    ]
    .concat()
}

/// `in 9h 52m` with a small "in", so the remaining time itself can be enormous.
fn countdown_hero(x: f64, baseline: f64, remaining: &str) -> String {
    let (prefix, rest) = remaining.split_once(' ').unwrap_or(("", remaining));
    format!(
        r##"<text x="{x}" y="{baseline}" text-anchor="middle" font-size="36" font-weight="600" fill="{MUTED}"><tspan dy="-20">{prefix} </tspan><tspan dy="20" font-size="150" font-weight="700" fill="#000">{rest}</tspan></text>"##
    )
}

fn prayers(frame: &Frame) -> String {
    let weekday = frame.prayers.date.weekday();
    let upcoming = frame.prayers.upcoming(frame.now);
    let g = frame.page.geometry();
    let mut cells = String::new();
    for (index, (salah, at)) in frame.prayers.board(frame.now).into_iter().enumerate() {
        let (x, top) = cell_position(&g, index);
        cells += &prayer_cell(
            frame,
            (x, top, g.prayer_row),
            salah,
            at,
            weekday,
            RowState::of(salah, at, upcoming, frame.now),
        );
    }
    cells += &hline(g.prayers_bottom() + 8.0, 2.0);
    cells
}

/// Where the n-th prayer sits: two columns of three, filled left to right.
fn cell_position(g: &Geometry, index: usize) -> (f64, f64) {
    let column = index % 2;
    let row = index / 2;
    (
        MARGIN + column as f64 * (COLUMN_WIDTH + COLUMN_GAP),
        g.prayers_top + row as f64 * g.prayer_row,
    )
}

/// A cell at (x, top), `row` tall.
fn prayer_cell(
    frame: &Frame,
    (x, top, row): (f64, f64, f64),
    salah: Salah,
    at: DateTime<Utc>,
    weekday: Weekday,
    state: RowState,
) -> String {
    let tz = frame.config.location.timezone;
    let (english, arabic) = salah.names(weekday);
    let time = format_time(at, tz, frame.config.display.clock);
    let weight = if salah == Salah::Sunrise { 400 } else { 600 };
    let fill = match state {
        RowState::Next => "#fff",
        RowState::Passed => MUTED,
        RowState::Later => "#000",
    };
    let right = x + COLUMN_WIDTH;
    let line = baseline(top, row, 32.0);
    let mut cell = String::new();
    if state == RowState::Next {
        cell += &rect(x - 8.0, top + 3.0, COLUMN_WIDTH + 16.0, row - 8.0, 8.0);
    }
    cell += &[
        text(x, line, 32, english)
            .weight(weight)
            .fill(fill)
            .to_string(),
        text(right - numeric_width(&time, 28.0) - 14.0, line, 24, arabic)
            .weight(weight)
            .end()
            .fill(fill)
            .to_string(),
        text(right, line, 28, &time)
            .weight(700)
            .end()
            .fill(fill)
            .to_string(),
    ]
    .concat();
    cell
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowState {
    Next,
    Passed,
    Later,
}

impl RowState {
    fn of(salah: Salah, at: DateTime<Utc>, upcoming: Upcoming, now: DateTime<Utc>) -> Self {
        match (Upcoming { prayer: salah, at } == upcoming, at <= now) {
            (true, _) => RowState::Next,
            (false, true) => RowState::Passed,
            (false, false) => RowState::Later,
        }
    }
}

fn weather(frame: &Frame) -> String {
    let g = frame.page.geometry();
    let Some(snapshot) = frame.weather else {
        return text(
            WIDTH as f64 / 2.0,
            g.prayers_bottom() + 120.0,
            28,
            "Weather unavailable",
        )
        .middle()
        .fill(MUTED)
        .to_string();
    };
    // Laid out for the full page, then moved up as a whole: the rain line
    // sits on the page's own footer.
    let body = if frame.rain_expected() {
        weather_rain(frame, snapshot, g.tomorrow)
    } else {
        weather_clear(frame, snapshot)
    };
    shift(g.body, &body) + &rain_line(frame, &snapshot.data, g.footer)
}

/// `content` moved down by `dy` (up if negative).
fn shift(dy: f64, content: &str) -> String {
    if dy == 0.0 {
        content.to_string()
    } else {
        format!(r#"<g transform="translate(0 {dy})">{content}</g>"#)
    }
}

/// The current conditions, formatted once for both layouts.
struct Conditions {
    sky: Sky,
    temperature: String,
    label: &'static str,
    feels: String,
    speed: String,
    unit: &'static str,
    gusts: String,
    wind_degrees: f64,
    range: Option<String>,
}

impl Conditions {
    fn new(frame: &Frame, w: &Weather) -> Self {
        let c = &w.current;
        let tz = frame.config.location.timezone;
        let today = frame.now.with_timezone(&tz).date_naive();
        Self {
            sky: c.sky,
            temperature: degrees(c.temperature),
            label: c.sky.condition.label(),
            feels: format!(
                "Feels {} · {:.0}% humidity",
                degrees(c.feels_like),
                c.humidity_percent
            ),
            speed: format!("{:.0}", c.wind.speed),
            unit: frame.config.weather.wind_unit.label(),
            gusts: format!(
                "gusts {:.0} · from {}",
                c.wind.gust,
                compass(c.wind.from_degrees)
            ),
            wind_degrees: c.wind.from_degrees,
            range: w
                .days
                .iter()
                .find(|d| d.date == today)
                .map(|d| format!("H {}  L {}", degrees(d.high), degrees(d.low))),
        }
    }
}

/// Rain is coming: the current conditions share the lower half with a radar
/// card. Tomorrow, under the conditions, moves `tomorrow` more.
fn weather_rain(frame: &Frame, snapshot: &Snapshot<Weather>, tomorrow: f64) -> String {
    let w = &snapshot.data;
    let c = Conditions::new(frame, w);
    let tz = frame.config.location.timezone;
    let clock = frame.config.display.clock;
    let mut body = next_hours(&w.hours, frame.now, 6)
        .iter()
        .enumerate()
        .map(|(i, hour)| hour_cell(hour, MARGIN + 117.0 * i as f64, HOURLY_TOP, tz, clock))
        .collect::<String>();
    body += &hline(566.0, 2.0);
    body += &[
        icon(c.sky, MARGIN - 6.0, 588.0, 96.0),
        text(140.0, 664.0, 82, &c.temperature)
            .weight(700)
            .to_string(),
        text(140.0, 700.0, 30, c.label).weight(600).to_string(),
        text(140.0, 732.0, 21, &c.feels).fill(MUTED).to_string(),
        wind_arrow(c.wind_degrees, 36.0, 748.0, 56.0),
        text(108.0, 792.0, 42, &c.speed).weight(700).to_string(),
        text(116.0 + numeric_width(&c.speed, 42.0), 792.0, 20, c.unit).to_string(),
        text(108.0, 828.0, 20, &c.gusts).to_string(),
        c.range.as_ref().map_or_else(String::new, |r| {
            text(108.0, 862.0, 24, r).weight(600).to_string()
        }),
    ]
    .concat();
    body += &shift(tomorrow, &tomorrow_compact(w, frame.now, tz));
    body += &radar_card(frame);
    body
}

/// No rain: the radar is dropped, so the conditions, wind and tomorrow grow.
fn weather_clear(frame: &Frame, snapshot: &Snapshot<Weather>) -> String {
    let w = &snapshot.data;
    let c = Conditions::new(frame, w);
    let tz = frame.config.location.timezone;
    let clock = frame.config.display.clock;
    let mut body = next_hours(&w.hours, frame.now, 6)
        .iter()
        .enumerate()
        .map(|(i, hour)| hour_cell_big(hour, MARGIN + 117.0 * i as f64, HOURLY_TOP, tz, clock))
        .collect::<String>();
    body += &hline(606.0, 2.0);
    body += &[
        icon(c.sky, MARGIN - 8.0, 612.0, 124.0),
        text(170.0, 728.0, 124, &c.temperature)
            .weight(700)
            .to_string(),
        text(170.0, 778.0, 42, c.label).weight(600).to_string(),
        text(170.0, 814.0, 25, &c.feels).fill(MUTED).to_string(),
        wind_arrow(c.wind_degrees, 470.0, 616.0, 88.0),
        text(566.0, 700.0, 58, &c.speed).weight(700).to_string(),
        text(572.0 + numeric_width(&c.speed, 58.0), 700.0, 24, c.unit).to_string(),
        text(470.0, 738.0, 22, &c.gusts).to_string(),
        c.range.as_ref().map_or_else(String::new, |r| {
            text(470.0, 782.0, 32, r).weight(600).to_string()
        }),
    ]
    .concat();
    body += &hline(840.0, 2.0);
    body += &tomorrow_wide(w, frame.now, tz);
    body
}

/// The rain outlook on `baseline`, with the data's age tucked into the
/// right corner.
fn rain_line(frame: &Frame, w: &Weather, baseline: f64) -> String {
    let tz = frame.config.location.timezone;
    [
        text(
            MARGIN,
            baseline,
            24,
            &rain_outlook(&w.quarters, frame.now, tz, frame.config.display.clock),
        )
        .weight(600)
        .to_string(),
        stale_note(frame).map_or_else(String::new, |s| {
            text(RIGHT, baseline, 18, &s).end().fill(MUTED).to_string()
        }),
    ]
    .concat()
}

fn stale_note(frame: &Frame) -> Option<String> {
    let snapshot = frame.weather?;
    let tz = frame.config.location.timezone;
    (frame.now - snapshot.fetched_at > STALE_AFTER).then(|| {
        format!(
            "as of {}",
            format_time(snapshot.fetched_at, tz, frame.config.display.clock)
        )
    })
}

/// Tomorrow's range and the sky around midday.
fn tomorrow(w: &Weather, now: DateTime<Utc>, tz: Tz) -> Option<(&DayRange, &Hour)> {
    let date = now.with_timezone(&tz).date_naive().succ_opt()?;
    let day = w.days.iter().find(|d| d.date == date)?;
    let hour = w
        .hours
        .iter()
        .filter(|h| h.at.with_timezone(&tz).date_naive() == date)
        .min_by_key(|h| h.at.with_timezone(&tz).hour().abs_diff(13))?;
    Some((day, hour))
}

/// Tomorrow as a single line, for the column beside the radar.
fn tomorrow_compact(w: &Weather, now: DateTime<Utc>, tz: Tz) -> String {
    let Some((day, hour)) = tomorrow(w, now, tz) else {
        return String::new();
    };
    [
        icon(hour.sky, MARGIN - 4.0, 884.0, 48.0),
        text(88.0, 906.0, 22, "Tomorrow")
            .weight(600)
            .fill(MUTED)
            .to_string(),
        text(
            88.0,
            934.0,
            24,
            &format!(
                "{} · H {} L {}",
                hour.sky.condition.label(),
                degrees(day.high),
                degrees(day.low)
            ),
        )
        .weight(600)
        .to_string(),
    ]
    .concat()
}

/// Tomorrow across the full width, when no radar is competing for the space.
fn tomorrow_wide(w: &Weather, now: DateTime<Utc>, tz: Tz) -> String {
    let Some((day, hour)) = tomorrow(w, now, tz) else {
        return String::new();
    };
    [
        icon(hour.sky, MARGIN - 4.0, 856.0, 56.0),
        text(96.0, 884.0, 24, "Tomorrow")
            .weight(600)
            .fill(MUTED)
            .to_string(),
        text(96.0, 922.0, 32, hour.sky.condition.label())
            .weight(600)
            .to_string(),
        text(
            RIGHT,
            922.0,
            40,
            &format!("H {}  L {}", degrees(day.high), degrees(day.low)),
        )
        .weight(700)
        .end()
        .to_string(),
    ]
    .concat()
}

/// The radar card: a bordered square in the bottom-right corner, over the raster
/// the renderer blits at [`RADAR_LEFT`], [`Page::radar_top`] (here at
/// [`RADAR_TOP`], in the body [`weather`] moves up to there).
fn radar_card(frame: &Frame) -> String {
    let (left, top) = (f64::from(RADAR_LEFT as i32), f64::from(RADAR_TOP as i32));
    let (width, height) = RADAR_SIZE;
    let (cx, cy) = (left + f64::from(width) / 2.0, top + f64::from(height) / 2.0);
    let mut card = format!(
        r##"<rect x="{}" y="{}" width="{}" height="{}" fill="none" stroke="#000" stroke-width="2"/>"##,
        left - 2.0,
        top - 2.0,
        f64::from(width) + 4.0,
        f64::from(height) + 4.0
    );
    let Some(snapshot) = frame.radar else {
        card += &text(cx, cy, 24, "Radar unavailable")
            .middle()
            .halo()
            .to_string();
        return card;
    };
    card += &format!(
        r##"<circle cx="{cx}" cy="{cy}" r="8" fill="#fff" stroke="#000" stroke-width="3"/><circle cx="{cx}" cy="{cy}" r="2.5"/>"##
    );
    card += &text(
        left + 8.0,
        top + 26.0,
        22,
        &format!(
            "Radar {}",
            format_time(
                snapshot.data.observed_at,
                frame.config.location.timezone,
                frame.config.display.clock
            )
        ),
    )
    .weight(700)
    .halo()
    .to_string();
    card += &text(
        left + f64::from(width) - 4.0,
        top + f64::from(height) - 8.0,
        11,
        "RainViewer · © OpenStreetMap",
    )
    .end()
    .halo()
    .to_string();
    card
}

fn hour_cell(hour: &Hour, x: f64, top: f64, tz: Tz, clock: ClockFormat) -> String {
    let label = match clock {
        ClockFormat::TwelveHour => hour.at.with_timezone(&tz).format("%-I%P").to_string(),
        ClockFormat::TwentyFourHour => hour.at.with_timezone(&tz).format("%H:00").to_string(),
    };
    let chance = hour
        .precipitation_chance
        .filter(|&p| p >= 10.0)
        .map(|p| format!("{p:.0}%"));
    let middle = x + 50.0;
    [
        text(middle, top + 20.0, 19, &label)
            .middle()
            .fill(MUTED)
            .to_string(),
        icon(hour.sky, middle - 22.0, top + 24.0, 44.0),
        chance.map_or_else(String::new, |c| {
            text(middle + 27.0, top + 58.0, 17, &c)
                .weight(600)
                .to_string()
        }),
        text(middle, top + 90.0, 22, &degrees(hour.temperature))
            .weight(600)
            .middle()
            .to_string(),
    ]
    .concat()
}

/// An hourly cell at the size the no-rain layout can afford.
fn hour_cell_big(hour: &Hour, x: f64, top: f64, tz: Tz, clock: ClockFormat) -> String {
    let label = match clock {
        ClockFormat::TwelveHour => hour.at.with_timezone(&tz).format("%-I%P").to_string(),
        ClockFormat::TwentyFourHour => hour.at.with_timezone(&tz).format("%H:00").to_string(),
    };
    let chance = hour
        .precipitation_chance
        .filter(|&p| p >= 10.0)
        .map(|p| format!("{p:.0}%"));
    let middle = x + 50.0;
    [
        text(middle, top + 28.0, 24, &label)
            .middle()
            .fill(MUTED)
            .to_string(),
        icon(hour.sky, middle - 32.0, top + 36.0, 64.0),
        chance.map_or_else(String::new, |c| {
            text(middle + 38.0, top + 84.0, 20, &c)
                .weight(600)
                .to_string()
        }),
        text(middle, top + 132.0, 34, &degrees(hour.temperature))
            .weight(600)
            .middle()
            .to_string(),
    ]
    .concat()
}

// ---- formatting (pure, tested) ----

fn clock_parts(t: DateTime<Utc>, tz: Tz, clock: ClockFormat) -> (String, Option<&'static str>) {
    let local = t.with_timezone(&tz);
    match clock {
        ClockFormat::TwelveHour => (
            local.format("%-I:%M").to_string(),
            Some(if local.hour() < 12 { "AM" } else { "PM" }),
        ),
        ClockFormat::TwentyFourHour => (local.format("%H:%M").to_string(), None),
    }
}

/// Plex digits are tabular (0.6 em) and the colon narrower; good enough to place a unit
/// after a number without measuring glyphs.
fn numeric_width(number: &str, size: f64) -> f64 {
    number
        .chars()
        .map(|c| if c == ':' { 0.3 } else { 0.6 })
        .sum::<f64>()
        * size
}

pub fn format_time(t: DateTime<Utc>, tz: Tz, clock: ClockFormat) -> String {
    let local = t.with_timezone(&tz);
    match clock {
        ClockFormat::TwelveHour => local.format("%-I:%M").to_string(),
        ClockFormat::TwentyFourHour => local.format("%H:%M").to_string(),
    }
}

/// Whole minutes remaining, rounded up so "in 1m" shows until the moment
/// arrives: any part of a minute counts, a fraction of a second included.
fn countdown(now: DateTime<Utc>, at: DateTime<Utc>) -> String {
    let left = (at - now).max(Duration::zero());
    let whole = left.num_minutes();
    let minutes = whole + i64::from(left > Duration::minutes(whole));
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("in {m}m"),
        (h, 0) => format!("in {h}h"),
        (h, m) => format!("in {h}h {m}m"),
    }
}

fn next_hours(hours: &[Hour], now: DateTime<Utc>, count: usize) -> &[Hour] {
    let start = hours.partition_point(|h| h.at <= now);
    &hours[start..(start + count).min(hours.len())]
}

fn rain_outlook(quarters: &[Quarter], now: DateTime<Utc>, tz: Tz, clock: ClockFormat) -> String {
    let start = quarters.partition_point(|q| q.at + Duration::minutes(15) <= now);
    let window = &quarters[start..(start + RAIN_QUARTERS).min(quarters.len())];
    let wet = |q: &Quarter| q.precipitation_mm >= WET_MM;
    let at = |q: &Quarter| format_time(q.at, tz, clock);
    let Some(first) = window.first() else {
        return "No precipitation forecast".to_string();
    };
    match (
        wet(first),
        window.iter().find(|q| wet(q)),
        window.iter().find(|q| !wet(q)),
    ) {
        (true, _, Some(dry)) => format!("Raining now, easing around {}", at(dry)),
        (true, _, None) => "Rain for the next 2 hours".to_string(),
        (false, Some(rain), _) => format!("Rain likely from {}", at(rain)),
        (false, None, _) => "No rain expected in the next 2 hours".to_string(),
    }
}

fn degrees(value: f64) -> String {
    // `-0°` reads oddly; round first so -0.4 shows as 0.
    let rounded = value.round();
    format!("{}°", if rounded == 0.0 { 0.0 } else { rounded })
}

fn compass(from_degrees: f64) -> &'static str {
    const POINTS: [&str; 16] = [
        "N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW",
        "NW", "NNW",
    ];
    POINTS[((from_degrees.rem_euclid(360.0) / 22.5).round() as usize) % 16]
}

// ---- drawing primitives ----

/// Baseline that vertically centres a line of `size` type in a box `height` tall.
fn baseline(top: f64, height: f64, size: f64) -> f64 {
    top + (height + size * 0.72) / 2.0
}

struct Text<'a> {
    x: f64,
    y: f64,
    size: u32,
    content: &'a str,
    weight: u16,
    anchor: Anchor,
    fill: &'static str,
    outline: Outline,
}

#[derive(Debug, Clone, Copy)]
enum Anchor {
    Start,
    Middle,
    End,
}

#[derive(Debug, Clone, Copy)]
enum Outline {
    None,
    /// A white outline so text stays legible over the map.
    Halo,
}

fn text(x: f64, y: f64, size: u32, content: &str) -> Text<'_> {
    Text {
        x,
        y,
        size,
        content,
        weight: 400,
        anchor: Anchor::Start,
        fill: "#000",
        outline: Outline::None,
    }
}

impl Text<'_> {
    fn weight(self, weight: u16) -> Self {
        Self { weight, ..self }
    }
    fn end(self) -> Self {
        Self {
            anchor: Anchor::End,
            ..self
        }
    }
    fn middle(self) -> Self {
        Self {
            anchor: Anchor::Middle,
            ..self
        }
    }
    fn fill(self, fill: &'static str) -> Self {
        Self { fill, ..self }
    }
    fn halo(self) -> Self {
        Self {
            outline: Outline::Halo,
            ..self
        }
    }
}

impl Display for Text<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let anchor = match self.anchor {
            Anchor::Start => "start",
            Anchor::Middle => "middle",
            Anchor::End => "end",
        };
        let outline = match self.outline {
            Outline::None => "",
            Outline::Halo => {
                r##" stroke="#fff" stroke-width="5" stroke-linejoin="round" paint-order="stroke""##
            }
        };
        write!(
            f,
            r#"<text x="{}" y="{}" font-size="{}" font-weight="{}" text-anchor="{anchor}" fill="{}"{outline}>"#,
            self.x, self.y, self.size, self.weight, self.fill
        )?;
        self.content.chars().try_for_each(|c| match c {
            '&' => f.write_str("&amp;"),
            '<' => f.write_str("&lt;"),
            '>' => f.write_str("&gt;"),
            c => f.write_char(c),
        })?;
        f.write_str("</text>")
    }
}

fn hline(y: f64, width: f64) -> String {
    format!(
        r#"<rect x="0" y="{}" width="{WIDTH}" height="{width}"/>"#,
        y - width / 2.0
    )
}

fn rect(x: f64, y: f64, width: f64, height: f64, radius: f64) -> String {
    format!(r#"<rect x="{x}" y="{y}" width="{width}" height="{height}" rx="{radius}"/>"#)
}

/// An arrow pointing where the wind blows to, in a ring marked N.
fn wind_arrow(from_degrees: f64, x: f64, y: f64, size: f64) -> String {
    let to = (from_degrees + 180.0).rem_euclid(360.0);
    let s = size / 100.0;
    format!(
        r##"<g transform="translate({x} {y}) scale({s})"><circle cx="50" cy="50" r="44" fill="none" stroke="#000" stroke-width="4"/><text x="50" y="22" font-size="16" font-weight="700" text-anchor="middle">N</text><g transform="rotate({to} 50 50)"><path d="M50 20 L66 62 L50 53 L34 62 Z"/><rect x="47" y="52" width="6" height="28"/></g></g>"##
    )
}

/// Line-art weather icons in a 100×100 box.
fn icon(sky: Sky, x: f64, y: f64, size: f64) -> String {
    const SUN: &str = r##"<circle cx="50" cy="50" r="18" fill="#fff" stroke="#000" stroke-width="6"/><g stroke="#000" stroke-width="6" stroke-linecap="round"><path d="M50 12v10M50 78v10M12 50h10M78 50h10M23 23l7 7M70 70l7 7M23 77l7-7M70 30l7-7"/></g>"##;
    const MOON: &str = r##"<path d="M60 18a34 34 0 1 0 24 52a28 28 0 0 1-24-52z" fill="#fff" stroke="#000" stroke-width="6" stroke-linejoin="round"/>"##;
    const CLOUD: &str = r##"<path d="M30 76h44a16 16 0 0 0 0-32a22 22 0 0 0-42-4a18 18 0 0 0-2 36z" fill="#fff" stroke="#000" stroke-width="6" stroke-linejoin="round"/>"##;
    const SMALL_SUN: &str = r##"<g transform="translate(-8 -12) scale(0.7)"><circle cx="50" cy="50" r="18" fill="#fff" stroke="#000" stroke-width="7"/><g stroke="#000" stroke-width="7" stroke-linecap="round"><path d="M50 14v8M14 50h8M24 24l6 6M76 24l-6 6"/></g></g>"##;
    const SMALL_MOON: &str = r##"<g transform="translate(-6 -10) scale(0.65)"><path d="M60 18a34 34 0 1 0 24 52a28 28 0 0 1-24-52z" fill="#fff" stroke="#000" stroke-width="8"/></g>"##;
    const HIGH_CLOUD: &str = r##"<g transform="translate(0 -12)"><path d="M30 76h44a16 16 0 0 0 0-32a22 22 0 0 0-42-4a18 18 0 0 0-2 36z" fill="#fff" stroke="#000" stroke-width="6" stroke-linejoin="round"/></g>"##;
    const RAIN: &str = r##"<g stroke="#000" stroke-width="6" stroke-linecap="round"><path d="M36 74l-6 14M52 74l-6 14M68 74l-6 14"/></g>"##;
    const HEAVY_RAIN: &str = r##"<g stroke="#000" stroke-width="6" stroke-linecap="round"><path d="M30 72l-8 20M44 72l-8 20M58 72l-8 20M72 72l-8 20"/></g>"##;
    const DRIZZLE: &str = r##"<g fill="#000"><circle cx="36" cy="80" r="4"/><circle cx="52" cy="88" r="4"/><circle cx="68" cy="80" r="4"/></g>"##;
    const SNOW: &str = r##"<g stroke="#000" stroke-width="4" stroke-linecap="round"><path d="M34 72v16M27 76l14 8M27 84l14-8M66 72v16M59 76l14 8M59 84l14-8"/></g>"##;
    const BOLT: &str = r##"<path d="M54 64l-12 18h10l-6 16 18-22h-10l6-12z" stroke="#000" stroke-width="2" stroke-linejoin="round"/>"##;
    const FOG: &str = r##"<g stroke="#000" stroke-width="6" stroke-linecap="round"><path d="M18 40h64M12 56h76M22 72h58"/></g>"##;
    let (light, small_light) = match sky.light {
        Light::Day => (SUN, SMALL_SUN),
        Light::Night => (MOON, SMALL_MOON),
    };
    let layers: &[&str] = match sky.condition {
        Condition::Clear | Condition::MainlyClear => &[light],
        Condition::PartlyCloudy => &[small_light, CLOUD],
        Condition::Overcast => &[CLOUD],
        Condition::Fog => &[FOG],
        Condition::Drizzle => &[HIGH_CLOUD, DRIZZLE],
        Condition::LightRain | Condition::Rain => &[HIGH_CLOUD, RAIN],
        Condition::HeavyRain | Condition::FreezingRain => &[HIGH_CLOUD, HEAVY_RAIN],
        Condition::Snow | Condition::HeavySnow => &[HIGH_CLOUD, SNOW],
        Condition::Thunderstorm => &[HIGH_CLOUD, BOLT],
    };
    format!(
        r#"<g transform="translate({x} {y}) scale({})">{}</g>"#,
        size / 100.0,
        layers.concat()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 11, h, m, 0).unwrap()
    }

    fn quarters(mm: &[f64]) -> Vec<Quarter> {
        mm.iter()
            .enumerate()
            .map(|(i, &precipitation_mm)| Quarter {
                at: t(12, 0) + Duration::minutes(15 * i as i64),
                precipitation_mm,
            })
            .collect()
    }

    #[test]
    fn countdown_rounds_up() {
        assert_eq!(
            countdown(t(12, 0), t(12, 0) + Duration::seconds(30)),
            "in 1m"
        );
        assert_eq!(countdown(t(12, 0), t(13, 0)), "in 1h");
        assert_eq!(countdown(t(12, 0), t(14, 5)), "in 2h 5m");
        // A minute and a fraction of a second: two.
        let at = t(12, 2);
        assert_eq!(countdown(at - Duration::milliseconds(60_213), at), "in 2m");
        assert_eq!(countdown(at - Duration::seconds(60), at), "in 1m");
        assert_eq!(countdown(at + Duration::seconds(1), at), "in 0m");
    }

    #[test]
    fn rain_outlook_cases() {
        let utc = chrono_tz::UTC;
        let h24 = ClockFormat::TwentyFourHour;
        let now = t(12, 5);
        assert_eq!(
            rain_outlook(&quarters(&[0.0; 10]), now, utc, h24),
            "No rain expected in the next 2 hours"
        );
        assert_eq!(
            rain_outlook(&quarters(&[0.0, 0.0, 0.5, 1.0]), now, utc, h24),
            "Rain likely from 12:30"
        );
        assert_eq!(
            rain_outlook(&quarters(&[0.4, 0.3, 0.0, 0.0]), now, utc, h24),
            "Raining now, easing around 12:30"
        );
        assert_eq!(
            rain_outlook(&quarters(&[0.4; 8]), now, utc, h24),
            "Rain for the next 2 hours"
        );
        // The 12:00 quarter is current at 12:05; past quarters are ignored.
        assert_eq!(
            rain_outlook(&quarters(&[0.4, 0.0]), t(12, 20), utc, h24),
            "No rain expected in the next 2 hours"
        );
        assert_eq!(
            rain_outlook(&[], now, utc, h24),
            "No precipitation forecast"
        );
    }

    #[test]
    fn compass_points() {
        assert_eq!(compass(0.0), "N");
        assert_eq!(compass(284.0), "WNW");
        assert_eq!(compass(359.0), "N");
    }

    #[test]
    fn twelve_hour_clock() {
        let ams = chrono_tz::Europe::Amsterdam;
        assert_eq!(
            clock_parts(t(13, 5), ams, ClockFormat::TwelveHour),
            ("2:05".into(), Some("PM"))
        );
        assert_eq!(
            format_time(t(13, 5), ams, ClockFormat::TwentyFourHour),
            "14:05"
        );
    }

    #[test]
    fn degrees_avoid_negative_zero() {
        assert_eq!(degrees(-0.4), "0°");
        assert_eq!(degrees(57.3), "57°");
    }

    #[test]
    fn text_is_escaped() {
        assert!(
            text(0.0, 0.0, 10, "a<b&c")
                .to_string()
                .contains("a&lt;b&amp;c")
        );
    }

    /// Any moment, anywhere: the header names the next prayer (as every
    /// solar day around it says, Jumu'ah on Fridays) and counts the whole
    /// minutes to it, rounded up.
    #[test]
    fn the_header_counts_down_to_the_next_prayer() {
        use crate::prayer::oracle;
        for index in 0..oracle::PLACES.len() {
            let config = oracle::place(index);
            let mut random = oracle::Random(index as u64 + 100);
            for second in oracle::moments(index as u64 + 50, 400) {
                // Anywhere in the second, its end included.
                let now = second + Duration::milliseconds(random.below(1_000) as i64);
                let (salah, at, weekday) = oracle::next(&config.location, &config.prayer, now);
                let prayers = DayTimes::current(&config.location, &config.prayer, now).unwrap();
                let header = header_svg(&Frame {
                    now,
                    config: &config,
                    prayers: &prayers,
                    weather: None,
                    radar: None,
                    page: Page::below(56),
                });
                let left = (at - now).num_milliseconds();
                let minutes = (left + 59_999) / 60_000;
                let remaining = match (minutes / 60, minutes % 60) {
                    (0, m) => format!("{m}m"),
                    (h, 0) => format!("{h}h"),
                    (h, m) => format!("{h}h {m}m"),
                };
                let (english, arabic) = salah.names(weekday);
                for shown in [
                    format!(">{remaining}</tspan>"),
                    format!(">{english}</text>"),
                    format!(">{arabic}</text>"),
                ] {
                    assert!(
                        header.contains(&shown),
                        "{now}, {left} ms to {at}: {shown}\n{header}"
                    );
                }
            }
        }
    }

    #[test]
    fn countdown_hero_shrinks_the_prefix_and_grows_the_numbers() {
        let hero = countdown_hero(379.0, 268.0, "in 9h 52m");
        assert!(hero.contains("font-size=\"36\""), "{hero}");
        assert!(hero.contains("font-size=\"150\""), "{hero}");
        assert!(hero.contains(">in </tspan>"), "{hero}");
        assert!(hero.contains(">9h 52m</tspan>"), "{hero}");
    }

    #[test]
    fn prayer_cells_fill_two_columns_of_three() {
        // Six prayers, two columns: the third cell starts a new row.
        let g = Page::FULL.geometry();
        assert_eq!(cell_position(&g, 0), (MARGIN, PRAYERS_TOP));
        assert_eq!(
            cell_position(&g, 1).0,
            MARGIN + COLUMN_WIDTH + COLUMN_GAP,
            "the second column starts after the first"
        );
        assert_eq!(cell_position(&g, 2), (MARGIN, PRAYERS_TOP + PRAYER_ROW));
        assert_eq!(cell_position(&g, 5).1, PRAYERS_TOP + 2.0 * PRAYER_ROW);
    }

    #[test]
    fn a_full_page_is_laid_out_as_ever() {
        let page = Page::default();
        assert_eq!(page, Page::FULL);
        assert_eq!(page, Page::below(0));
        assert_eq!(page.height(), HEIGHT);
        assert_eq!(page.header_height(), HEADER_HEIGHT);
        assert_eq!(page.radar_top(), RADAR_TOP);
        assert_eq!(
            page.geometry(),
            Geometry {
                names: 0.0,
                hero: 0.0,
                prayers_top: PRAYERS_TOP,
                prayer_row: PRAYER_ROW,
                body: 0.0,
                tomorrow: 0.0,
                footer: HEIGHT as f64 - 56.0,
            }
        );
    }

    #[test]
    fn a_page_under_a_bar_tightens_the_spacing_to_fit() {
        // kindil's status bar.
        let page = Page::below(56);
        assert_eq!(page.height(), 968);
        assert_eq!(Page::below(80), page, "no shorter than everything fits in");
        let g = page.geometry();
        assert_eq!(page.header_height(), 282);
        assert_eq!(
            g.prayers_top, 284.0,
            "the timetable starts under the header"
        );
        assert!(g.prayers_top + 3.0 > f64::from(page.header_height()));
        assert_eq!(g.prayer_row, 44.0);
        assert_eq!(g.prayers_bottom() + 8.0, PRAYERS_BOTTOM + 8.0 + g.body);
        assert_eq!(g.body, -24.0);
        assert_eq!(page.radar_top(), RADAR_TOP - 24);
        // The radar card's border and the rain line under it fit on the page.
        let card_bottom = page.radar_top() as f64 + f64::from(RADAR_SIZE.1) + 2.0;
        assert!(card_bottom + 24.0 < g.footer, "{card_bottom} {}", g.footer);
        assert_eq!(g.footer, 936.0);
        // In between, every offset is a whole pixel, so a shifted element
        // draws exactly as it did.
        for inset in 0..=Page::MAX_INSET {
            let page = Page::below(inset);
            let g = page.geometry();
            assert_eq!(g.body.fract(), 0.0);
            assert_eq!(page.header_height() as f64, 300.0 + g.hero);
            assert!(g.footer <= f64::from(page.height()) - 32.0);
        }
    }
}
