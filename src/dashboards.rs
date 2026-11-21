//! Miqat is its own library (a git subtree of sharno/miqat), not a forked layout.
use chrono::{DateTime, NaiveDate, Utc};
use miqat::image::Gray;
use miqat::layout::{Page, WIDTH};

/// The status bar's height in `ui/app.slint` (`StatusRow`): the dashboard
/// shows under it, so Miqat lays its page out in the rest of the panel.
pub const STATUS_BAR: u32 = 56;
/// Miqat's page as the window shows it: 758 × (1024 − [`STATUS_BAR`]).
pub const PAGE: Page = Page::below(STATUS_BAR);
/// The rows of [`PAGE`] the header draws in.
pub const HEADER_HEIGHT: u32 = PAGE.header_height();

/// A dashboard frame, as the window shows it: the header (clock, date, next
/// prayer and countdown) and the body under it are separate images, so a
/// minute that only changes the header redraws only its rows on the panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Split {
    pub header: Gray,
    pub body: Gray,
}

impl Split {
    /// Cuts a whole page at [`HEADER_HEIGHT`].
    pub fn of(page: Gray) -> Self {
        let cut = (page.width * HEADER_HEIGHT.min(page.height)) as usize;
        let mut pixels = page.pixels;
        let body = pixels.split_off(cut);
        Self {
            header: Gray {
                width: page.width,
                height: HEADER_HEIGHT.min(page.height),
                pixels,
            },
            body: Gray {
                width: page.width,
                height: page.height.saturating_sub(HEADER_HEIGHT),
                pixels: body,
            },
        }
    }
}

/// What the body shows that the clock moving on can't change: the day and
/// the prayer counted down to (the timetable highlights it). The weather
/// moves on at most every quarter of an hour, which the full redraws every
/// [`crate::system::FULL_REDRAW_MINUTES`] keep up with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Board {
    date: NaiveDate,
    upcoming: DateTime<Utc>,
}

impl Board {
    pub fn at(config: &miqat::config::Config, now: DateTime<Utc>) -> Option<Self> {
        // The calendar date, which the weather's days follow; the prayer
        // day changes only with the next prayer.
        let date = miqat::prayer::local_date(&config.location, now);
        miqat::prayer::DayTimes::current(&config.location, &config.prayer, now)
            .ok()
            .map(|prayers| Self {
                date,
                upcoming: prayers.upcoming(now).at,
            })
    }
}

/// How much of the dashboard a draw renders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Draw {
    /// The whole page.
    Full,
    /// Only the header, if the body on screen still holds.
    Header,
}

/// What to render for `asked`, with `shown` the board of the last full
/// frame on screen (None before one) and `now` the board as of now: a
/// header-only draw becomes a full one once the day or the next prayer has
/// moved on, since the timetable shows them too.
pub fn draw(asked: Draw, shown: Option<Board>, now: Option<Board>) -> Draw {
    match asked {
        Draw::Header if shown.is_some() && shown == now => Draw::Header,
        Draw::Header | Draw::Full => Draw::Full,
    }
}

pub fn render(
    renderer: &miqat::render::Renderer,
    config: &miqat::config::Config,
    now: DateTime<Utc>,
    sources: &miqat::sources::Sources,
) -> anyhow::Result<Gray> {
    let prayers = miqat::prayer::DayTimes::current(&config.location, &config.prayer, now)?;
    renderer.gray(&frame(config, now, &prayers, sources))
}

fn frame<'a>(
    config: &'a miqat::config::Config,
    now: DateTime<Utc>,
    prayers: &'a miqat::prayer::DayTimes,
    sources: &'a miqat::sources::Sources,
) -> miqat::layout::Frame<'a> {
    miqat::layout::Frame {
        now,
        config,
        prayers,
        weather: sources.weather(),
        radar: sources.radar(),
        page: PAGE,
    }
}

/// The body of the last full frame handed to the window, as Miqat keys it
/// ([`miqat::render::Body`]): while a full redraw's body is the same, its
/// pixels are too, and the window still has them.
///
/// Why it matters: a full redraw rasterises the whole page, about twice a
/// minute tick's work, which keeps a doze wake up past 2 s from its resume;
/// the PW1's battery driver then writes an hour of supposed suspend
/// current off the gauge (src/health.rs, docs/HOW-IT-WORKS.md). Most full
/// redraws (every 5 minutes) find the body as it was: they render only the
/// header, like a tick.
#[derive(Debug, Default)]
pub struct Bodies {
    shown: Option<miqat::render::Body>,
}

impl Bodies {
    /// Whatever the window shows now, the next full frame renders its body.
    pub fn forget(&mut self) {
        self.shown = None;
    }
}

/// A full frame's body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    /// Rendered anew: the window takes it.
    Drawn(Gray),
    /// The same as the window's ([`Bodies`]): nothing to hand over.
    Same,
}

/// A full frame: the header, rendered, and the body, rendered unless the
/// window has it already.
pub fn render_full(
    renderer: &miqat::render::Renderer,
    config: &miqat::config::Config,
    now: DateTime<Utc>,
    sources: &miqat::sources::Sources,
    bodies: &mut Bodies,
) -> anyhow::Result<(Gray, Body)> {
    let prayers = miqat::prayer::DayTimes::current(&config.location, &config.prayer, now)?;
    let frame = frame(config, now, &prayers, sources);
    let body = renderer.body(&frame);
    if bodies.shown.as_ref() == Some(&body) {
        return Ok((renderer.header_gray(&frame)?, Body::Same));
    }
    // Forgotten until the new one is in hand: a failed render leaves the
    // window's body unknown.
    bodies.shown = None;
    let split = Split::of(renderer.gray(&frame)?);
    bodies.shown = Some(body);
    Ok((split.header, Body::Drawn(split.body)))
}

/// Only the header, [`WIDTH`] × [`HEADER_HEIGHT`]: the same pixels as
/// [`render`]'s page has there.
pub fn render_header(
    renderer: &miqat::render::Renderer,
    config: &miqat::config::Config,
    now: DateTime<Utc>,
) -> anyhow::Result<Gray> {
    let prayers = miqat::prayer::DayTimes::current(&config.location, &config.prayer, now)?;
    let header = renderer.header_gray(&miqat::layout::Frame {
        now,
        config,
        prayers: &prayers,
        weather: None,
        radar: None,
        page: PAGE,
    })?;
    debug_assert_eq!((header.width, header.height), (WIDTH, HEADER_HEIGHT));
    Ok(header)
}

/// Deterministic layout fixture without fetching weather, radar or map tiles.
pub fn preview(
    config: &miqat::config::Config,
    now: DateTime<Utc>,
) -> anyhow::Result<miqat::image::Gray> {
    preview_with(
        &miqat::render::Renderer::new(config.display.contrast),
        config,
        now,
    )
}

/// [`preview`] with rain on the way: the layout with the radar card, over a
/// made-up map (rings of rain round the centre) instead of fetched tiles.
pub fn preview_rain(
    config: &miqat::config::Config,
    now: DateTime<Utc>,
) -> anyhow::Result<miqat::image::Gray> {
    let renderer = miqat::render::Renderer::new(config.display.contrast);
    let mut weather = fixture_weather(config)?;
    weather.quarters = (0..400)
        .map(|i| miqat::weather::Quarter {
            at: now + chrono::TimeDelta::minutes(15 * i - 15),
            precipitation_mm: if i >= 3 { miqat::weather::WET_MM } else { 0.0 },
        })
        .collect();
    let (width, height) = miqat::layout::RADAR_SIZE;
    let map = Gray {
        width,
        height,
        pixels: (0..width * height)
            .map(|i| {
                let (x, y) = (f64::from(i % width), f64::from(i / width));
                let r = (x - 150.0).hypot(y - 124.0);
                match r as u32 {
                    0..40 => 0x55,
                    40..80 => 0x99,
                    80..120 => 0xcc,
                    _ if (i / width) % 46 == 0 || (i % width) % 46 == 0 => 0xbb,
                    _ => 0xf4,
                }
            })
            .collect(),
    };
    let radar = miqat::sources::Snapshot {
        data: miqat::radar::RadarImage {
            observed_at: now - chrono::TimeDelta::minutes(5),
            map,
        },
        fetched_at: now,
    };
    frame_with(&renderer, config, now, weather, Some(&radar))
}

fn fixture_weather(config: &miqat::config::Config) -> anyhow::Result<miqat::weather::Weather> {
    miqat::weather::decode(
        include_str!("../crates/miqat/tests/fixtures/open-meteo.json"),
        config.location.timezone,
    )
}

fn preview_with(
    renderer: &miqat::render::Renderer,
    config: &miqat::config::Config,
    now: DateTime<Utc>,
) -> anyhow::Result<miqat::image::Gray> {
    frame_with(renderer, config, now, fixture_weather(config)?, None)
}

fn frame_with(
    renderer: &miqat::render::Renderer,
    config: &miqat::config::Config,
    now: DateTime<Utc>,
    weather: miqat::weather::Weather,
    radar: Option<&miqat::sources::Snapshot<miqat::radar::RadarImage>>,
) -> anyhow::Result<miqat::image::Gray> {
    let prayers = miqat::prayer::DayTimes::current(&config.location, &config.prayer, now)?;
    let snapshot = miqat::sources::Snapshot {
        data: weather,
        fetched_at: now,
    };
    renderer.gray(&miqat::layout::Frame {
        now,
        config,
        prayers: &prayers,
        weather: Some(&snapshot),
        radar,
        page: PAGE,
    })
}

/// [`preview`]'s header alone.
pub fn preview_header(
    config: &miqat::config::Config,
    now: DateTime<Utc>,
) -> anyhow::Result<miqat::image::Gray> {
    render_header(
        &miqat::render::Renderer::new(config.display.contrast),
        config,
        now,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, TimeDelta};

    fn config() -> miqat::config::Config {
        toml::from_str(include_str!("../examples/miqat.toml")).unwrap()
    }

    /// A full redraw five minutes on, its body unchanged, renders only the
    /// header, the same as a tick's; the window's body is the one a fresh
    /// render would draw. A new day, or a forgotten body, renders it anew.
    #[test]
    fn a_full_redraw_keeps_a_body_that_hasnt_changed() {
        let config = config();
        let renderer = miqat::render::Renderer::new(config.display.contrast);
        let sources = miqat::sources::Sources::new(&config);
        let noon = DateTime::from_timestamp(1_794_398_400, 0).unwrap();
        let mut bodies = Bodies::default();
        let full = |at, bodies: &mut Bodies| {
            render_full(&renderer, &config, at, &sources, bodies).unwrap()
        };
        let (header, Body::Drawn(body)) = full(noon, &mut bodies) else {
            panic!("the first full frame renders its body");
        };
        assert_eq!(header, render_header(&renderer, &config, noon).unwrap());
        let later = noon + TimeDelta::minutes(5);
        let (header, kept) = full(later, &mut bodies);
        assert_eq!(kept, Body::Same);
        assert_eq!(header, render_header(&renderer, &config, later).unwrap());
        assert_eq!(
            Split::of(render(&renderer, &config, later, &sources).unwrap()).body,
            body,
            "the window's body is the one a full render draws"
        );
        let tomorrow = noon + TimeDelta::days(1);
        assert!(matches!(full(tomorrow, &mut bodies).1, Body::Drawn(_)));
        bodies.forget();
        assert!(matches!(full(tomorrow, &mut bodies).1, Body::Drawn(_)));
    }

    #[test]
    fn a_page_splits_at_the_header() {
        let page = Gray {
            width: 2,
            height: HEADER_HEIGHT + 2,
            pixels: (0..2 * (HEADER_HEIGHT + 2))
                .map(|i| (i % 251) as u8)
                .collect(),
        };
        let split = Split::of(page.clone());
        assert_eq!(split.header.height, HEADER_HEIGHT);
        assert_eq!(split.body.height, 2);
        assert_eq!(
            [split.header.pixels, split.body.pixels].concat(),
            page.pixels
        );
    }

    #[test]
    fn the_page_fits_under_the_status_bar() {
        let config = config();
        let noon = DateTime::from_timestamp(1_794_398_400, 0).unwrap();
        for page in [
            preview(&config, noon).unwrap(),
            preview_rain(&config, noon).unwrap(),
        ] {
            assert_eq!((page.width, page.height), (WIDTH, 1024 - STATUS_BAR));
            let split = Split::of(page);
            assert_eq!(split.header.height, HEADER_HEIGHT);
            assert_eq!(split.header.height + split.body.height, 1024 - STATUS_BAR);
        }
        assert_eq!(HEADER_HEIGHT, 282);
    }

    #[test]
    fn a_minute_redraws_the_header_until_the_next_prayer_or_the_day_changes() {
        let config = config();
        let noon = DateTime::from_timestamp(1_794_398_400, 0).unwrap();
        let shown = Board::at(&config, noon);
        let upcoming = shown.unwrap().upcoming;
        let at = |t| Board::at(&config, t);
        // Minutes later, the same prayer still to come: the header will do.
        let later = upcoming - TimeDelta::minutes(2);
        assert_eq!(draw(Draw::Header, shown, at(later)), Draw::Header);
        // Its time has come: the timetable highlights the next one.
        let passed = upcoming + TimeDelta::seconds(1);
        assert_eq!(draw(Draw::Header, shown, at(passed)), Draw::Full);
        // A new day, or no full frame on screen yet.
        let tomorrow = noon + TimeDelta::days(1);
        assert_eq!(draw(Draw::Header, shown, at(tomorrow)), Draw::Full);
        assert_eq!(draw(Draw::Header, None, at(later)), Draw::Full);
        assert_eq!(draw(Draw::Header, None, None), Draw::Full);
        // A full draw stays full.
        assert_eq!(draw(Draw::Full, shown, at(later)), Draw::Full);
    }

    #[test]
    fn the_header_patched_onto_the_last_frame_is_the_next_minutes_page() {
        let config = config();
        let noon = DateTime::from_timestamp(1_794_398_400, 0).unwrap();
        let minute = noon + TimeDelta::minutes(1);
        assert_eq!(Board::at(&config, noon), Board::at(&config, minute));
        let shown = Split::of(preview(&config, noon).unwrap());
        let header = preview_header(&config, minute).unwrap();
        assert_ne!(header, shown.header, "the clock and countdown moved on");
        let patched = Split {
            header,
            body: shown.body,
        };
        assert_eq!(patched, Split::of(preview(&config, minute).unwrap()));
    }

    /// The countdown and the prayer a header shows at `now`, as minutes.
    fn header_says(config: &miqat::config::Config, now: DateTime<Utc>) -> (String, i64) {
        let prayers =
            miqat::prayer::DayTimes::current(&config.location, &config.prayer, now).unwrap();
        let svg = miqat::layout::header_svg(&miqat::layout::Frame {
            now,
            config,
            prayers: &prayers,
            weather: None,
            radar: None,
            page: PAGE,
        });
        let (english, _) = prayers.upcoming(now).prayer.names(prayers.date.weekday());
        assert!(svg.contains(&format!(">{english}</text>")));
        let hero = r##"font-size="150" font-weight="700" fill="#000">"##;
        let rest = svg
            .split(hero)
            .nth(1)
            .unwrap()
            .split("</tspan>")
            .next()
            .unwrap();
        let minutes = rest
            .split(' ')
            .map(|part| match part.split_at(part.len() - 1) {
                (h, "h") => h.parse::<i64>().unwrap() * 60,
                (m, "m") => m.parse::<i64>().unwrap(),
                other => panic!("{other:?} in {rest}"),
            })
            .sum();
        (english.to_owned(), minutes)
    }

    /// Dozing through an evening in Toronto, on a clock that resumes up to
    /// a second off each nap (put back within the RTC's second) and starts
    /// 724 s slow until the first online wake's sync: from that sync on,
    /// whatever is on the panel at any second names the next prayer and
    /// counts no more than a minute more than is left (the frame stays up
    /// to a minute), never less; but from the moment a prayer comes to the
    /// next wake, that prayer "in 1m" or less.
    #[test]
    fn a_dozing_panel_counts_down_within_its_minute() {
        use crate::system::{next_redraw, Redraw, Schedule, Window};
        use miqat::prayer::DayTimes;
        let config: miqat::config::Config = toml::from_str(
            "[location]\nlatitude = 43.65\nlongitude = -79.38\n\
             timezone = \"America/Toronto\"\n[prayer]\nmethod = \"north_america\"\n",
        )
        .unwrap();
        let mut real = DateTime::parse_from_rfc3339("2026-11-20T18:58:00.300Z")
            .unwrap()
            .to_utc();
        let mut error = TimeDelta::seconds(-724);
        let mut jitter = 0x2026_1120_u64;
        let mut next_jitter = |range_ms: i64| {
            jitter = jitter
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            TimeDelta::milliseconds((jitter >> 33) as i64 % (2 * range_ms + 1) - range_ms)
        };
        let mut schedule = Schedule::default();
        schedule.doze_began();
        let mut synced = false;
        // On the glass since, and the time it was drawn for.
        let mut shown: Option<(DateTime<Utc>, DateTime<Utc>)> = None;
        let mut fulls = 0;
        while real < DateTime::parse_from_rfc3339("2026-11-21T02:00:00Z").unwrap() {
            // A wake: the clock as the device reads it.
            let now = real + error;
            let wake = schedule.wake(now, true);
            fulls += u32::from(wake.redraw == Redraw::Full);
            if wake.window == Window::Online {
                // The sync steps the clock right (the fetch's frame follows).
                error = next_jitter(100);
                synced = true;
            }
            // Rendered a moment later; on the glass a second after that.
            let rendered = real + TimeDelta::milliseconds(600) + error;
            let on_glass = real + TimeDelta::milliseconds(1_600);
            // Until then, the frame before.
            if let (true, Some((since, frame))) = (synced, shown) {
                let mut t = since;
                while t < on_glass {
                    let (name, minutes) = header_says(&config, frame);
                    let day = DayTimes::current(&config.location, &config.prayer, t).unwrap();
                    let next = day.upcoming(t);
                    let left = (next.at - t).num_milliseconds();
                    let truth = (left + 59_999) / 60_000;
                    let (english, _) = next.prayer.names(day.date.weekday());
                    let passed = name != english;
                    assert!(
                        (passed && minutes <= 1)
                            || (!passed && (truth..=truth + 1).contains(&minutes)),
                        "at {t} the panel says {name} in {minutes}m (drawn for {frame}); \
                         {english} is in {truth}m"
                    );
                    t += TimeDelta::seconds(1);
                }
            }
            shown = Some((on_glass, rendered));
            // Awake about 1.5 s, then the nap to the next minute plus a
            // second, on the monotonic clock: as long as the device clock
            // said; the resume leaves it up to a second off, put back
            // within the RTC's (half a second either way).
            let awake = TimeDelta::milliseconds(1_500);
            let until = next_redraw(now + awake) + TimeDelta::seconds(1);
            real += awake + (until - (now + awake));
            let half = TimeDelta::milliseconds(500);
            error = match synced {
                true => (error + next_jitter(1_000)).clamp(-half, half),
                false => error + next_jitter(500),
            };
        }
        assert!(synced);
        assert!(fulls > 7 * 60 / 5, "{fulls}");
    }

    /// Host timings, for the cost of a doze wake: `cargo test --release
    /// dashboards::tests::render_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "a timing, not a check"]
    fn render_cost() {
        let config = config();
        let renderer = miqat::render::Renderer::new(config.display.contrast);
        let sources = miqat::sources::Sources::new(&config);
        let noon = DateTime::from_timestamp(1_794_398_400, 0).unwrap();
        let rounds = 20;
        let time = |f: &dyn Fn(DateTime<Utc>)| {
            f(noon);
            let start = std::time::Instant::now();
            (0..rounds).for_each(|i| f(noon + TimeDelta::minutes(i)));
            start.elapsed() / rounds as u32
        };
        let full = time(&|now| {
            let page = render(&renderer, &config, now, &sources).unwrap();
            std::hint::black_box(Split::of(page));
        });
        let full_weather = time(&|now| {
            std::hint::black_box(Split::of(preview_with(&renderer, &config, now).unwrap()));
        });
        let header = time(&|now| {
            std::hint::black_box(render_header(&renderer, &config, now).unwrap());
        });
        let board = time(&|now| {
            std::hint::black_box(Board::at(&config, now));
        });
        println!(
            "full page (no weather) {full:?}, full page with weather \
             {full_weather:?}, header only {header:?}, board check {board:?}"
        );
    }
}
