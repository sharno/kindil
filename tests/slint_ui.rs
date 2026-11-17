//! The production window and renderer, driven by real taps on a simulated
//! device. Coordinates are the 758×1024 panel's; see ui/app.slint for layout.
use kindil::battery::{Battery, Charge};
use kindil::config::Config;
use kindil::drain::MilliampHours;
use kindil::host::{Host, Simulated};
use kindil::maintenance::Session;
use kindil::preview::{self, drag, pixels, tap};
use kindil::system::{Drive, Event, Touches};
use kindil::ui::{
    App, BootChoice, DashboardKind, Execution, Kindil, Options, Page, Panel, Switch, System,
};
use kindil::wifi::Credentials;
use slint::platform::software_renderer::MinimalSoftwareWindow;
use slint::{ComponentHandle, Model};
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

const TAB_Y: f32 = 978.0;
const HOME_TAB: f32 = 95.0;
const LIBRARY_TAB: f32 = 284.0;
const SETTINGS_TAB: f32 = 663.0;
// The status row's grip, and the quick panel below it.
const STATUS_ROW: (f32, f32) = (380.0, 28.0);
// The front light slider's track (0% to 100%) and row height, in the panel and in Settings.
const PANEL_LIGHT: (f32, f32) = (100.0, 588.0);
const PANEL_LIGHT_Y: f32 = 104.0;
const SETTINGS_LIGHT: (f32, f32) = (100.0, 588.0);
const SETTINGS_LIGHT_Y: f32 = 372.0;
const SLACK: i32 = 2;
// The first saved network's Forget button on the Wi-Fi page.
const FORGET: (f32, f32) = (676.0, 431.0);
// Text size buttons in the reader's panel.
const TEXT_SIZE_Y: f32 = 186.0;
const SMALLER_TEXT: f32 = 533.0;
const LARGER_TEXT: f32 = 702.0;

struct Fixture {
    app: App,
    window: Rc<MinimalSoftwareWindow>,
    dir: tempfile::TempDir,
    /// The panel's fast black-and-white mode, as the app sets it.
    motion: Arc<std::sync::atomic::AtomicBool>,
}

impl Fixture {
    fn new(host: Arc<dyn Host>) -> Self {
        // Half a minute from the next one: dozing always naps, and the
        // minute never turns mid-test.
        Self::with_clock(host, || {
            chrono::DateTime::from_timestamp(1_794_398_430, 0).unwrap()
        })
    }

    fn with_clock(host: Arc<dyn Host>, now: fn() -> chrono::DateTime<chrono::Utc>) -> Self {
        Self::build(host, now, "")
    }

    /// Started with these saved preferences, as after a reboot.
    fn with_prefs(host: Arc<dyn Host>, prefs: &str) -> Self {
        Self::build(
            host,
            || chrono::DateTime::from_timestamp(1_794_398_430, 0).unwrap(),
            prefs,
        )
    }

    fn build(host: Arc<dyn Host>, now: fn() -> chrono::DateTime<chrono::Utc>, prefs: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("prefs.toml"), prefs).unwrap();
        let books = dir.path().join("books");
        std::fs::create_dir(&books).unwrap();
        std::fs::write(
            books.join("A_Real_Book.txt"),
            "A real book\nقنديل\n".repeat(400),
        )
        .unwrap();
        let miqat_config = dir.path().join("miqat.toml");
        std::fs::copy("examples/miqat.toml", &miqat_config).unwrap();
        let config = Config {
            books,
            miqat_config,
            prefs: dir.path().join("prefs.toml"),
            // Absent until Settings → Location writes the clock's zone.
            path: dir.path().join("kindil.toml"),
            ..Config::parse("dashboard = \"clock\"").unwrap()
        };
        let window = preview::install().unwrap();
        let motion = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let app = App::new(Options {
            config,
            host,
            panel: Panel::Eink {
                full_refresh: Arc::default(),
                motion: motion.clone(),
            },
            presented: None,
            execution: Execution::Inline,
            touches: Touches::default(),
            now,
        })
        .unwrap();
        app.refresh_now();
        app.window.show().unwrap();
        // The clock's width moves the slider, so pin it.
        app.window.global::<Kindil>().set_clock("09:41".into());
        Self {
            app,
            window,
            dir,
            motion,
        }
    }

    fn k(&self) -> Kindil<'_> {
        self.app.window.global::<Kindil>()
    }

    fn tap(&self, x: f32, y: f32) {
        let _ = pixels(&self.window);
        tap(&self.window, x, y);
        let _ = pixels(&self.window);
    }

    /// A finger on the track from `from` to `to` percent along it, lifted there.
    /// Returns the light while the finger is still down.
    fn slide(&self, track: (f32, f32), y: f32, from: f32, to: f32) -> i32 {
        use slint::platform::{PointerEventButton, WindowAdapter, WindowEvent};
        let at = |percent: f32| {
            slint::LogicalPosition::new(track.0 + (track.1 - track.0) * percent / 100.0, y)
        };
        let send = |event| {
            self.window.window().dispatch_event(event);
            slint::platform::update_timers_and_animations();
        };
        let _ = pixels(&self.window);
        send(WindowEvent::PointerPressed {
            position: at(from),
            button: PointerEventButton::Left,
        });
        (1..=8).for_each(|step| {
            let percent = from + (to - from) * step as f32 / 8.0;
            send(WindowEvent::PointerMoved {
                position: at(percent),
            });
        });
        let during = self.k().get_brightness();
        send(WindowEvent::PointerReleased {
            position: at(to),
            button: PointerEventButton::Left,
        });
        let _ = pixels(&self.window);
        during
    }

    fn books(&self) -> std::path::PathBuf {
        self.dir.path().join("books")
    }

    fn keyboard_visible(&self) -> bool {
        self.app.window.get_keyboard_visible()
    }
}

/// Whether any pixel in these rows differs from Miqat's frame.
fn frame_differs(
    rendered: &[slint::Rgb8Pixel],
    expected: &[u8],
    rows: std::ops::Range<usize>,
) -> bool {
    rows.flat_map(|y| (0..758).map(move |x| y * 758 + x))
        .any(|i| rendered[i].r != expected[i])
}

#[test]
fn miqat_is_pixel_exact_and_taps_reveal_both_bars() {
    let f = Fixture::new(Arc::new(Simulated::default()));
    let miqat = miqat::config::Config::load(Path::new("examples/miqat.toml")).unwrap();
    let noon = chrono::DateTime::from_timestamp(1_794_398_400, 0).unwrap();
    let gray = kindil::dashboards::preview(&miqat, noon).unwrap();
    let expected = gray.pixels.clone();
    f.app.set_miqat(gray);
    f.k().set_dashboard(DashboardKind::Miqat);
    let rendered = pixels(&f.window);
    // Only the battery badge, in the strip between the clock and the prayer
    // name, is drawn over the frame.
    let outside_badge = |frame: &[slint::Rgb8Pixel]| {
        frame.iter().enumerate().all(|(i, p)| {
            let (x, y) = (i % 758, i / 758);
            (300..460).contains(&x) && (4..48).contains(&y) || p.r == expected[i]
        })
    };
    assert!(
        outside_badge(&rendered),
        "the dashboard must be Miqat's frame, without chrome or scaling"
    );
    assert!(
        frame_differs(&rendered, &expected, 8..44),
        "the battery stays visible without a tap"
    );

    f.tap(380.0, 500.0);
    assert!(f.k().get_chrome_visible());
    assert!(
        f.k().get_panel_open(),
        "the tap opens the panel with the bars"
    );
    let revealed = pixels(&f.window);
    assert!(
        frame_differs(&revealed, &expected, 0..56),
        "the tap shows the status row"
    );
    assert!(
        frame_differs(&revealed, &expected, 60..150),
        "the tap shows the quick panel"
    );
    assert!(
        frame_differs(&revealed, &expected, 932..1024),
        "the tap shows the tab bar"
    );

    // Dragging the slider sets the light once, on release, and the panel
    // stays up meanwhile.
    assert_eq!(f.k().get_brightness(), 40);
    let during = f.slide(PANEL_LIGHT, PANEL_LIGHT_Y, 40.0, 80.0);
    assert_eq!(during, 40, "the light is not set on every finger movement");
    assert!((f.k().get_brightness() - 80).abs() <= SLACK);
    f.tap(
        PANEL_LIGHT.0 + (PANEL_LIGHT.1 - PANEL_LIGHT.0) * 0.25,
        PANEL_LIGHT_Y,
    );
    assert!(
        (f.k().get_brightness() - 25).abs() <= SLACK,
        "a tap on the track jumps there, got {}",
        f.k().get_brightness()
    );
    f.slide(PANEL_LIGHT, PANEL_LIGHT_Y, 25.0, -50.0);
    assert_eq!(f.k().get_brightness(), 0, "dragging past the end clamps");
    assert!(f.k().get_panel_open());
    assert!(
        f.k().get_activity() > 0,
        "using the slider restarts the hide timer"
    );

    // A tap outside puts away the panel, and the bars with it.
    f.tap(380.0, 600.0);
    assert!(!f.k().get_panel_open());
    assert!(!f.k().get_chrome_visible());

    f.tap(380.0, 500.0);
    f.tap(STATUS_ROW.0, STATUS_ROW.1);
    assert!(!f.k().get_panel_open(), "the status row closes the panel");
    assert!(f.k().get_chrome_visible());
    f.tap(LIBRARY_TAB, TAB_Y);
    assert_eq!(f.k().get_page(), Page::Library);
}

#[test]
fn a_minute_tick_redraws_only_the_header_rows() {
    use slint::platform::software_renderer::RepaintBufferType;
    let f = Fixture::new(Arc::new(Simulated::default()));
    let miqat = miqat::config::Config::load(Path::new("examples/miqat.toml")).unwrap();
    let noon = chrono::DateTime::from_timestamp(1_794_398_400, 0).unwrap();
    let minute = noon + chrono::TimeDelta::minutes(1);
    f.app
        .set_miqat(kindil::dashboards::preview(&miqat, noon).unwrap());
    f.k().set_dashboard(DashboardKind::Miqat);
    // As the Kindle backend renders: into the buffer the last frame left,
    // reporting the region that changed.
    let mut buffer = vec![slint::Rgb8Pixel::default(); 758 * 1024];
    let mut render = || {
        let mut dirty = None;
        slint::platform::update_timers_and_animations();
        f.window.draw_if_needed(|renderer| {
            renderer.set_repaint_buffer_type(RepaintBufferType::ReusedBuffer);
            let region = renderer.render(&mut buffer, 758);
            dirty = Some((region.bounding_box_origin(), region.bounding_box_size()));
        });
        dirty
    };
    f.app.window.window().request_redraw();
    render();
    f.app
        .set_miqat_header(kindil::dashboards::preview_header(&miqat, minute).unwrap());
    assert_eq!(
        render(),
        Some((
            slint::PhysicalPosition::new(0, 0),
            slint::PhysicalSize::new(758, miqat::layout::HEADER_HEIGHT)
        )),
        "only the header's rows go to the panel"
    );
    // The header patched over the last page is the next minute's page.
    let expected = kindil::dashboards::preview(&miqat, minute).unwrap().pixels;
    let badge = |i: usize| (300..460).contains(&(i % 758)) && (4..48).contains(&(i / 758));
    assert!(
        buffer
            .iter()
            .enumerate()
            .all(|(i, p)| badge(i) || p.r == expected[i]),
        "the patched frame is the next minute's page, pixel for pixel"
    );
}

/// Host timings of the window's share of a doze wake: `cargo test --release
/// --test slint_ui window_render_cost -- --ignored --nocapture`.
#[test]
#[ignore = "a timing, not a check"]
fn window_render_cost() {
    use slint::platform::software_renderer::RepaintBufferType;
    let f = Fixture::new(Arc::new(Simulated::default()));
    let miqat = miqat::config::Config::load(Path::new("examples/miqat.toml")).unwrap();
    let noon = chrono::DateTime::from_timestamp(1_794_398_400, 0).unwrap();
    let page = kindil::dashboards::preview(&miqat, noon).unwrap();
    let header = kindil::dashboards::preview_header(&miqat, noon).unwrap();
    f.app.set_miqat(page.clone());
    f.k().set_dashboard(DashboardKind::Miqat);
    let mut buffer = vec![slint::Rgb8Pixel::default(); 758 * 1024];
    let mut render = || {
        slint::platform::update_timers_and_animations();
        f.window.draw_if_needed(|renderer| {
            renderer.set_repaint_buffer_type(RepaintBufferType::ReusedBuffer);
            std::hint::black_box(renderer.render(&mut buffer, 758));
        });
    };
    f.app.window.window().request_redraw();
    render();
    let rounds = 20;
    let start = std::time::Instant::now();
    (0..rounds).for_each(|_| {
        f.app.set_miqat(page.clone());
        render();
    });
    let full = start.elapsed() / rounds;
    let start = std::time::Instant::now();
    (0..rounds).for_each(|_| {
        f.app.set_miqat_header(header.clone());
        render();
    });
    let tick = start.elapsed() / rounds;
    println!("window: full page set and rendered {full:?}, header only {tick:?}");
}

#[test]
fn the_status_row_drops_a_panel_over_the_page() {
    let f = Fixture::new(Arc::new(Simulated::default()));
    f.tap(LIBRARY_TAB, TAB_Y);
    let closed = pixels(&f.window);
    f.tap(STATUS_ROW.0, STATUS_ROW.1);
    assert!(f.k().get_panel_open());
    let open = pixels(&f.window);
    assert!(
        (56..160).any(|y| (0..758).any(|x| open[y * 758 + x] != closed[y * 758 + x])),
        "the panel draws over the page"
    );
    assert!(
        (300..900).all(|y| (0..758).all(|x| open[y * 758 + x] == closed[y * 758 + x])),
        "the panel covers the page instead of pushing it down"
    );
    // The page behind does not react to the tap that puts the panel away.
    f.tap(300.0, 208.0);
    assert!(!f.k().get_panel_open());
    assert_eq!(f.k().get_page(), Page::Library);
    f.tap(STATUS_ROW.0, STATUS_ROW.1);
    f.tap(SETTINGS_TAB, TAB_Y);
    assert_eq!(f.k().get_page(), Page::Settings);
    assert!(!f.k().get_panel_open(), "going elsewhere puts it away");
}

#[test]
fn the_reader_panel_sets_text_size_and_light() {
    let f = Fixture::new(Arc::new(Simulated::default()));
    f.tap(LIBRARY_TAB, TAB_Y);
    f.tap(300.0, 208.0);
    assert_eq!(f.k().get_page(), Page::Reader);
    f.tap(STATUS_ROW.0, STATUS_ROW.1);
    let size = f.k().get_font_size();
    f.tap(LARGER_TEXT, TEXT_SIZE_Y);
    assert!(f.k().get_font_size() > size);
    f.tap(SMALLER_TEXT, TEXT_SIZE_Y);
    f.tap(SMALLER_TEXT, TEXT_SIZE_Y);
    assert!(f.k().get_font_size() < size);
    assert!(f.k().get_panel_open(), "the panel stays while adjusting");
    f.slide(PANEL_LIGHT, PANEL_LIGHT_Y, 40.0, 100.0);
    assert_eq!(f.k().get_brightness(), 100, "the end of the track is 100%");
}

#[test]
fn reading_turns_pages_and_remembers_the_position() {
    let f = Fixture::new(Arc::new(Simulated::default()));
    f.tap(LIBRARY_TAB, TAB_Y);
    assert_eq!(f.k().get_books().row_count(), 1);
    assert_eq!(f.k().get_books().row_data(0).unwrap().title, "A Real Book");
    f.tap(300.0, 208.0);
    assert_eq!(f.k().get_page(), Page::Reader);
    assert!(f.k().get_book_text().contains("A real book"));
    assert!(f.k().get_book_position().starts_with("1 of"));
    f.tap(600.0, 500.0); // right two-thirds: forward
    f.tap(600.0, 500.0);
    assert!(f.k().get_book_position().starts_with("3 of"));
    f.tap(100.0, 500.0); // left third: back
    assert!(f.k().get_book_position().starts_with("2 of"));
    let prefs = std::fs::read_to_string(f.dir.path().join("prefs.toml")).unwrap();
    assert!(prefs.contains("A_Real_Book.txt"), "{prefs}");

    // A larger font repaginates but keeps the place in the book.
    let before = f.k().get_book_progress();
    f.k().invoke_font_larger();
    f.k().invoke_font_larger();
    assert_eq!(f.k().get_font_size(), 28);
    assert!((f.k().get_book_progress() - before).abs() < 0.02);

    // Reopening resumes where reading stopped.
    f.tap(HOME_TAB, TAB_Y);
    f.tap(LIBRARY_TAB, TAB_Y);
    f.tap(300.0, 208.0);
    assert!(!f.k().get_book_position().starts_with("1 of"));
}

#[test]
fn rename_with_the_touch_keyboard_then_trash_and_undo() {
    let f = Fixture::new(Arc::new(Simulated::default()));
    f.tap(LIBRARY_TAB, TAB_Y);
    f.tap(716.0, 208.0); // "···"
    assert_eq!(f.k().get_selected_book(), 0);
    assert_eq!(f.k().get_new_name(), "A_Real_Book.txt");
    f.k().set_new_name("".into());
    f.tap(380.0, 786.0); // file name field
    assert!(f.keyboard_visible(), "the field opens the keyboard");
    [
        (109.0, 742.0),
        (595.0, 870.0),
        (354.0, 742.0),
        (161.0, 870.0),
        (354.0, 742.0),
    ]
    .into_iter()
    .for_each(|(x, y)| f.tap(x, y));
    assert_eq!(f.k().get_new_name(), "q.txt");
    f.tap(721.0, 806.0); // Return
    assert!(!f.keyboard_visible());
    f.tap(570.0, 870.0); // Rename
    assert!(f.books().join("q.txt").is_file());
    assert_eq!(f.k().get_selected_book(), -1);

    f.tap(716.0, 208.0);
    f.tap(200.0, 870.0); // Move to Trash
    assert!(
        f.books().join("q.txt").is_file(),
        "the first tap only arms it"
    );
    f.tap(200.0, 870.0);
    assert!(!f.books().join("q.txt").exists());
    assert!(f.k().get_notice_undo());
    f.k().invoke_undo();
    assert!(f.books().join("q.txt").is_file());
}

#[test]
fn prayer_times_choices_are_written_into_miqats_file() {
    let f = Fixture::new(Arc::new(Simulated::default()));
    let path = f.dir.path().join("miqat.toml");
    let original = std::fs::read_to_string(&path).unwrap();
    f.tap(SETTINGS_TAB, TAB_Y);
    assert_eq!(
        f.k().get_prayer_summary(),
        "Muslim World League · Standard Asr"
    );
    f.tap(380.0, 300.0); // Prayer times row
    assert_eq!(f.k().get_page(), Page::Prayer);
    assert_eq!(f.k().get_prayer_error(), "");
    assert_eq!(f.k().get_prayer_methods().row_count(), 12);
    assert_eq!(f.k().get_prayer_method(), 0);
    let karachi = f.k().get_prayer_methods().row_data(3).unwrap();
    assert_eq!(karachi.title, "Karachi (University of Islamic Sciences)");
    assert!(karachi.times.starts_with("Fajr "), "{}", karachi.times);
    assert!(karachi.times.contains(" · Isha "), "{}", karachi.times);

    f.tap(380.0, 540.0); // Karachi
    assert_eq!(f.k().get_prayer_method(), 3);
    let saved = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        saved,
        original.replace("method = \"muslim_world_league\"", "method = \"karachi\""),
        "only the method changes; comments and other keys stay"
    );
    f.k().invoke_choose_asr_school(1);
    assert_eq!(f.k().get_asr_school(), 1);
    assert_eq!(
        f.k().get_prayer_summary(),
        "Karachi (University of Islamic Sciences) · Hanafi Asr"
    );
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.contains("madhab = \"hanafi\""), "{saved}");
    assert!(saved.contains("# shafi | hanafi (later Asr)"), "{saved}");

    // A broken file is shown, and never written.
    let broken = saved.replace("[radar]", "[radar");
    std::fs::write(&path, &broken).unwrap();
    f.k().invoke_go(Page::Prayer);
    assert!(f.k().get_prayer_error().contains("miqat.toml"));
    assert_eq!(f.k().get_prayer_methods().row_count(), 0);
    f.k().invoke_choose_prayer_method(0);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
    assert!(f.k().get_notice().starts_with("Prayer times not saved"));
    std::fs::remove_file(&path).unwrap();
    f.k().invoke_go(Page::Settings);
    f.k().invoke_choose_prayer_method(0);
    assert!(!path.exists());
}

#[test]
fn a_searched_city_is_saved_only_once_confirmed() {
    let host = Arc::new(Simulated::default());
    let f = Fixture::new(host.clone());
    let miqat = f.dir.path().join("miqat.toml");
    let kindil = f.dir.path().join("kindil.toml");
    let original = std::fs::read_to_string(&miqat).unwrap();
    f.tap(SETTINGS_TAB, TAB_Y);
    assert_eq!(f.k().get_location_title(), "52.37, 4.90");
    assert_eq!(f.k().get_location_zone(), "Europe/Amsterdam");
    f.tap(380.0, 384.0); // Location row
    assert_eq!(f.k().get_page(), Page::Location);
    assert_eq!(host.lookups(), 0, "nothing is looked up until asked");

    f.k().invoke_go(Page::PlaceSearch);
    f.k().invoke_search_places("Mecca".into());
    assert_eq!(f.k().get_places().row_count(), 1);
    let mecca = f.k().get_places().row_data(0).unwrap();
    assert_eq!(mecca.title, "Mecca, Saudi Arabia");
    assert_eq!(mecca.detail, "Asia/Riyadh · 21.43, 39.83");
    assert_eq!(f.k().get_location_status(), "1 place. Tap it to use it.");
    f.tap(380.0, 300.0); // the result
    assert!(f.k().get_confirm_open());
    assert_eq!(
        f.k().get_confirm_place(),
        "Mecca, Saudi Arabia · Asia/Riyadh"
    );
    assert_eq!(f.k().get_confirm_note(), "");
    assert_eq!(std::fs::read_to_string(&miqat).unwrap(), original);
    assert!(!kindil.exists(), "nothing written before Save");

    // Cancel keeps everything as it was.
    f.k().invoke_cancel_location();
    assert!(!f.k().get_confirm_open());
    f.k().invoke_save_location();
    assert_eq!(std::fs::read_to_string(&miqat).unwrap(), original);

    let utc_clock = f.k().get_clock();
    f.k().invoke_choose_place(0);
    f.k().invoke_save_location();
    assert!(!f.k().get_confirm_open());
    assert_eq!(f.k().get_page(), Page::Location);
    assert_eq!(f.k().get_location_title(), "Mecca, Saudi Arabia");
    assert_eq!(f.k().get_location_zone(), "Asia/Riyadh");
    assert_eq!(f.k().get_notice(), "Location saved: Mecca, Saudi Arabia");
    let saved = std::fs::read_to_string(&miqat).unwrap();
    assert_eq!(
        saved,
        original
            .replace("latitude = 52.3676", "latitude = 21.4266")
            .replace("longitude = 4.9041", "longitude = 39.8256")
            .replace(
                "timezone = \"Europe/Amsterdam\"   # IANA name",
                "timezone = \"Asia/Riyadh\"   # IANA name"
            ),
        "only [location] changes; comments and other keys stay"
    );
    assert_eq!(
        std::fs::read_to_string(&kindil).unwrap(),
        "timezone = \"Asia/Riyadh\"\n"
    );
    // The clock follows at once: Riyadh is three hours ahead of UTC.
    assert_ne!(f.k().get_clock(), utc_clock);
    let riyadh = chrono::DateTime::from_timestamp(1_794_398_430, 0)
        .unwrap()
        .with_timezone(&chrono_tz::Asia::Riyadh)
        .format("%H:%M")
        .to_string();
    assert_eq!(f.k().get_clock(), riyadh);
    f.k().invoke_go(Page::Settings);
    assert_eq!(f.k().get_location_title(), "Mecca, Saudi Arabia");
    // The name has its own file; prefs.toml stays as an older kindil reads it.
    let prefs = std::fs::read_to_string(f.dir.path().join("prefs.toml")).unwrap();
    assert!(
        !prefs.contains("place") && !prefs.contains("Mecca"),
        "{prefs}"
    );
    let place = std::fs::read_to_string(f.dir.path().join("place.toml")).unwrap();
    assert!(place.contains("name = \"Mecca, Saudi Arabia\""), "{place}");
    // A hand edit of [location] stops the name showing.
    let edited = saved.replace("latitude = 21.4266", "latitude = 21.5");
    std::fs::write(&miqat, edited).unwrap();
    f.k().invoke_go(Page::Settings);
    assert_eq!(f.k().get_location_title(), "21.50, 39.83");
}

#[test]
fn the_internet_address_is_asked_only_on_a_tap_and_confirmed() {
    let host = Arc::new(Simulated::default());
    let f = Fixture::new(host.clone());
    let kindil = f.dir.path().join("kindil.toml");
    let comments = "# kindil settings.\ndashboard = \"clock\"   # first run\n";
    std::fs::write(&kindil, comments).unwrap();
    f.k().invoke_go(Page::Settings);
    f.k().invoke_go(Page::Location);
    assert_eq!(host.lookups(), 0);
    f.tap(380.0, 520.0); // Use my approximate location
    assert_eq!(host.lookups(), 1);
    assert!(f.k().get_confirm_open());
    assert_eq!(
        f.k().get_confirm_place(),
        "Amsterdam, North Holland, NL · Europe/Amsterdam"
    );
    assert_eq!(f.k().get_confirm_note(), kindil::ui::IP_NOTE);
    assert!(f.k().get_confirm_note().contains("VPN"));
    assert_eq!(std::fs::read_to_string(&kindil).unwrap(), comments);
    f.tap(570.0, 960.0); // Save
    assert!(!f.k().get_confirm_open());
    assert_eq!(f.k().get_location_title(), "Amsterdam, North Holland, NL");
    assert_eq!(
        std::fs::read_to_string(&kindil).unwrap(),
        format!("{comments}timezone = \"Europe/Amsterdam\"\n")
    );
    assert_eq!(host.lookups(), 1, "and never again by itself");
}

#[test]
fn typed_coordinates_are_checked_then_saved() {
    let f = Fixture::new(Arc::new(Simulated::default()));
    let miqat = f.dir.path().join("miqat.toml");
    let original = std::fs::read_to_string(&miqat).unwrap();
    f.k().invoke_go(Page::Settings);
    f.k().invoke_go(Page::Coordinates);
    assert_eq!(
        f.k().get_typed_zone(),
        "Europe/Amsterdam",
        "starts from the zone in use"
    );
    f.k()
        .invoke_enter_coordinates("89".into(), "0".into(), "UTC".into());
    assert_eq!(
        f.k().get_location_status(),
        "Latitude must be between -85 and 85"
    );
    assert!(!f.k().get_confirm_open());
    f.k()
        .invoke_enter_coordinates("40.7".into(), "-74".into(), "New Yrok".into());
    assert!(f.k().get_location_status().contains("Unknown time zone"));
    f.k().invoke_filter_zones("new york".into());
    let matches: Vec<String> = f.k().get_zone_matches().iter().map(String::from).collect();
    assert_eq!(matches, ["America/New_York"]);
    f.k().invoke_filter_zones("America/New_York".into());
    assert_eq!(
        f.k().get_zone_matches().row_count(),
        0,
        "an exact zone needs no list"
    );
    assert_eq!(std::fs::read_to_string(&miqat).unwrap(), original);

    f.k().invoke_enter_coordinates(
        "40.7128".into(),
        "-74.006".into(),
        "America/New_York".into(),
    );
    assert_eq!(f.k().get_location_status(), "");
    assert_eq!(
        f.k().get_confirm_place(),
        "40.71, -74.01 · America/New_York"
    );
    f.k().invoke_save_location();
    assert_eq!(f.k().get_location_title(), "40.71, -74.01");
    let config = miqat::config::Config::load(&miqat).unwrap();
    assert_eq!(config.location.latitude.degrees(), 40.7128);
    assert_eq!(config.location.timezone, chrono_tz::America::New_York);
}

#[test]
fn location_errors_are_shown_and_nothing_is_written() {
    let host = Arc::new(Simulated::default());
    let f = Fixture::new(host.clone());
    let miqat = f.dir.path().join("miqat.toml");
    let kindil = f.dir.path().join("kindil.toml");
    f.k().invoke_go(Page::PlaceSearch);
    f.k().invoke_search_places("Qq".into());
    assert!(f
        .k()
        .get_location_status()
        .starts_with("No places found for “Qq”"));
    assert_eq!(f.k().get_places().row_count(), 0);
    f.k().invoke_search_places(" a ".into());
    assert_eq!(
        f.k().get_location_status(),
        "Type at least two letters of the name."
    );

    host.set_online(false);
    f.k().invoke_search_places("Amsterdam".into());
    assert!(f.k().get_location_status().starts_with("Search failed: "));
    assert!(!f.k().get_location_busy());
    f.k().invoke_go(Page::Location);
    f.k().invoke_locate_by_ip();
    assert!(f
        .k()
        .get_location_status()
        .starts_with("Couldn't find your location: "));
    assert!(!f.k().get_confirm_open());

    // A file that breaks between the question and Save is left alone.
    host.set_online(true);
    f.k().invoke_locate_by_ip();
    let broken = std::fs::read_to_string(&miqat)
        .unwrap()
        .replace("[radar]", "[radar");
    std::fs::write(&miqat, &broken).unwrap();
    f.k().invoke_save_location();
    assert!(f.k().get_location_status().starts_with("Not saved: "));
    assert!(!f.k().get_confirm_open());
    assert_eq!(std::fs::read_to_string(&miqat).unwrap(), broken);
    assert!(!kindil.exists());
    f.k().invoke_go(Page::Settings);
    assert!(f.k().get_location_error().contains("miqat.toml"));
}

#[test]
fn settings_switch_the_simulated_device() {
    let f = Fixture::new(Arc::new(Simulated::default()));
    f.tap(SETTINGS_TAB, TAB_Y);
    assert_eq!(f.k().get_page(), Page::Settings);
    f.tap(380.0, 200.0); // Display row
    assert_eq!(f.k().get_page(), Page::Display);
    f.tap(568.0, 236.0); // Clock
    assert_eq!(f.k().get_dashboard(), DashboardKind::Clock);
    let prefs = std::fs::read_to_string(f.dir.path().join("prefs.toml")).unwrap();
    assert!(prefs.contains("dashboard = \"clock\""), "{prefs}");
    assert_eq!(f.k().get_brightness(), 40);
    f.slide(SETTINGS_LIGHT, SETTINGS_LIGHT_Y, 40.0, 70.0); // front light slider
    assert!((f.k().get_brightness() - 70).abs() <= SLACK);
    f.tap(40.0, 112.0); // back
    assert_eq!(f.k().get_page(), Page::Settings);
    f.tap(380.0, 570.0); // Remote access row
    assert_eq!(f.k().get_page(), Page::Remote);
    assert_eq!(f.k().get_ssh(), Switch::On);
    f.tap(380.0, 210.0); // SSH server row
    assert_eq!(f.k().get_ssh(), Switch::Off);
    assert_eq!(f.k().get_sync(), Switch::Unavailable);
    // The Settings tab goes back to the index from any section.
    f.tap(SETTINGS_TAB, TAB_Y);
    assert_eq!(f.k().get_page(), Page::Settings);
    let facts: Vec<String> = f
        .k()
        .get_facts()
        .iter()
        .map(|fact| fact.label.to_string())
        .collect();
    assert_eq!(
        facts,
        ["Battery", "Memory free", "Storage free", "Up for", "kindil"]
    );
}

#[test]
fn a_chosen_front_light_level_is_saved() {
    let host = Arc::new(Simulated::default());
    let f = Fixture::new(host.clone());
    assert_eq!(f.k().get_brightness(), 40, "never chosen: left as it is");
    f.k().invoke_choose_brightness(70);
    assert_eq!(host.brightness().unwrap().percent(), 70);
    let saved = std::fs::read_to_string(f.dir.path().join("prefs.toml")).unwrap();
    assert!(saved.contains("light = 70"), "saved: {saved}");
}

#[test]
fn a_light_never_chosen_in_kindil_is_adopted_at_start() {
    // As on a first run: no level saved, the light at Amazon's 40%.
    let host = Arc::new(Simulated::default());
    let f = Fixture::new(host.clone());
    let saved = std::fs::read_to_string(f.dir.path().join("prefs.toml")).unwrap();
    assert!(saved.contains("light = 40"), "saved: {saved}");
    assert_eq!(f.k().get_brightness(), 40);
}

#[test]
fn the_front_light_comes_back_to_the_saved_level_at_start() {
    // After a reboot the light is wherever the hardware starts it.
    let host = Arc::new(Simulated::default());
    let f = Fixture::with_prefs(host.clone(), "light = 70\n");
    assert_eq!(host.brightness().unwrap().percent(), 70);
    assert_eq!(f.k().get_brightness(), 70);
}

#[test]
fn the_power_button_sleeps_and_the_light_comes_back() {
    let host = Arc::new(Simulated::default());
    let f = Fixture::new(host.clone());
    // The simulated device wakes as soon as it suspends.
    f.app.device_event(Event::PowerButton);
    assert_eq!(f.k().get_system(), System::Asleep);
    f.app.fire_timers();
    assert_eq!(host.suspends(), 1);
    assert_eq!(f.k().get_system(), System::App);
    assert_eq!(host.brightness().unwrap().percent(), 40);
    // The press that woke it is reported after resume; it must not sleep again.
    f.app.device_event(Event::PowerButton);
    assert_eq!(host.suspends(), 1);
}

#[test]
fn usb_drive_mode_covers_the_app_until_the_storage_is_back() {
    let f = Fixture::new(Arc::new(Simulated::default()));
    f.tap(LIBRARY_TAB, TAB_Y);
    let before = pixels(&f.window);
    f.app.device_event(Event::StorageLeaving);
    f.app.device_event(Event::Drive(Drive::Computer));
    assert_eq!(f.k().get_system(), System::UsbDrive);
    assert!(pixels(&f.window) != before);
    f.tap(380.0, 300.0);
    assert_eq!(f.k().get_page(), Page::Library, "taps don't reach the app");
    std::fs::write(f.books().join("New_Book.txt"), "Copied over USB\n").unwrap();
    f.app.device_event(Event::Drive(Drive::Kindle));
    assert_eq!(
        f.k().get_system(),
        System::UsbDrive,
        "until the storage stays"
    );
    f.app.fire_timers();
    assert_eq!(f.k().get_system(), System::App);
    assert_eq!(f.k().get_books().row_count(), 2, "the library is reloaded");
}

#[test]
fn auto_sleep_is_a_saved_choice() {
    let f = Fixture::new(Arc::new(Simulated::default()));
    assert_eq!(f.k().get_auto_sleep(), 0, "doze, by default");
    f.k().invoke_choose_auto_sleep(3);
    assert_eq!(f.k().get_auto_sleep(), 3);
    let prefs = std::fs::read_to_string(f.dir.path().join("prefs.toml")).unwrap();
    assert!(prefs.contains("minutes = 15"), "{prefs}");
}

#[test]
fn start_at_boot_is_a_choice_once_the_boot_job_is_installed() {
    use kindil::host::{Boot, BootStart};
    let host = Arc::new(Simulated::default());
    let f = Fixture::new(host.clone());
    f.k().invoke_navigated(Page::Power);
    assert_eq!(f.k().get_boot(), BootChoice::Off);
    f.k().invoke_choose_boot(BootChoice::Standalone);
    assert_eq!(f.k().get_boot(), BootChoice::Standalone);
    assert_eq!(host.boot_start(), BootStart::Installed(Boot::Standalone));
    f.k().invoke_choose_boot(BootChoice::AfterAmazon);
    assert_eq!(host.boot_start(), BootStart::Installed(Boot::AfterAmazon));
}

#[test]
fn an_empty_battery_sleeps_with_a_charge_screen() {
    let host = Arc::new(Simulated::default());
    let f = Fixture::new(host.clone());
    host.set_battery(Battery::Known {
        percent: 4,
        charge: Charge::Discharging,
    });
    f.k().invoke_navigated(Page::Settings);
    f.k().invoke_device_events();
    assert_eq!(f.k().get_system(), System::Asleep);
    assert!(
        f.k().get_battery_empty(),
        "the sleep screen asks for a charge"
    );
    f.app.fire_timers();
    assert_eq!(host.suspends(), 1);
}

#[test]
fn dozing_keeps_the_screen_and_naps_each_minute_until_a_press() {
    let host = Arc::new(Simulated::default());
    let f = Fixture::new(host.clone());
    f.app.device_event(Event::Doze);
    assert_eq!(
        host.brightness().unwrap().percent(),
        0,
        "the light goes off"
    );
    // Each round: the panel settles, the device naps, wakes and redraws.
    (1..=3).for_each(|naps| {
        f.app.fire_timers();
        assert_eq!(host.naps(), naps);
    });
    assert_eq!(f.k().get_system(), System::App, "no sleep screen");
    assert_eq!(host.parked_naps(), 3, "Wi-Fi is parked before every nap");
    f.app.device_event(Event::PowerButton);
    assert_eq!(host.brightness().unwrap().percent(), 40);
    assert!(!host.parked(), "the press brings Wi-Fi back");
    f.app.fire_timers();
    assert_eq!(host.naps(), 3, "awake: no more naps");
}

#[test]
fn sleeping_parks_wifi_and_waking_brings_it_back() {
    let host = Arc::new(Simulated::default());
    let f = Fixture::new(host.clone());
    f.app.device_event(Event::PowerButton);
    f.app.fire_timers();
    assert_eq!((host.suspends(), host.parked_naps()), (1, 1));
    assert!(!host.parked(), "the wake reconnects");
}

#[test]
fn dozing_on_battery_measures_its_draw_each_online_window() {
    use std::sync::atomic::{AtomicI64, Ordering};
    static CLOCK: AtomicI64 = AtomicI64::new(1_794_398_430);
    let host = Arc::new(Simulated::default());
    host.set_battery(Battery::Known {
        percent: 50,
        charge: Charge::Discharging,
    });
    host.set_charge(MilliampHours(623));
    let f = Fixture::with_clock(host.clone(), || {
        chrono::DateTime::from_timestamp(CLOCK.load(Ordering::Relaxed), 0).unwrap()
    });
    f.app.device_event(Event::Doze);
    // The first nap's wake goes online and reads the gauge.
    (0..2).for_each(|_| f.app.fire_timers());
    assert_eq!(f.k().get_doze_draw(), "", "one reading is no draw yet");
    CLOCK.fetch_add(30 * 60, Ordering::Relaxed);
    host.set_charge(MilliampHours(620));
    f.app.fire_timers();
    assert_eq!(f.k().get_doze_draw(), "6 mA · 4 days left");
}

#[test]
fn waking_from_a_doze_sets_the_clock_once_wifi_is_back() {
    let host = Arc::new(Simulated::default());
    let f = Fixture::new(host.clone());
    f.app.device_event(Event::Doze);
    (0..3).for_each(|_| f.app.fire_timers());
    // Without a dashboard a doze never goes online: the RTC keeps its time.
    assert_eq!(host.clock_syncs(), 0);
    host.set_clock_off(724.0);
    f.app.device_event(Event::PowerButton);
    assert_eq!(
        host.clock_syncs(),
        1,
        "the press brings Wi-Fi back, and the time"
    );
}

#[test]
fn waking_from_sleep_sets_the_clock_once_wifi_is_back() {
    // The simulated suspend wakes at once.
    let host = Arc::new(Simulated::default());
    let f = Fixture::new(host.clone());
    f.app.device_event(Event::PowerButton);
    f.app.fire_timers();
    assert_eq!((host.suspends(), host.clock_syncs()), (1, 1));
}

#[test]
fn an_open_ssh_session_keeps_a_dozing_kindil_awake_until_it_closes() {
    let host = Arc::new(Simulated::default());
    let f = Fixture::new(host.clone());
    host.set_ssh_sessions(vec![Session {
        local_port: 2222,
        remote: "192.168.1.5:50001".parse().unwrap(),
    }]);
    f.app.device_event(Event::Doze);
    // Rounds alternate: the minute's wake, without a nap, and its redraw.
    (0..4).for_each(|_| f.app.fire_timers());
    assert_eq!(host.naps(), 0, "held awake");
    assert!(!host.parked(), "Wi-Fi stays up");
    host.set_ssh_sessions(Vec::new());
    f.app.fire_timers();
    assert_eq!(host.naps(), 1, "closed: napping again");
    assert!(host.parked());
}

#[test]
fn power_off_reports_a_device_that_cannot() {
    let f = Fixture::new(Arc::new(Simulated::default()));
    f.k().invoke_power_off();
    assert_eq!(f.k().get_system(), System::App);
    assert!(f.k().get_notice().contains("cannot power off"));
}

/// Records what the UI asked to join, so no passphrase has to be exposed.
#[derive(Default)]
struct Recording {
    device: Simulated,
    joined: Mutex<Vec<(String, bool)>>,
    /// What GitHub would report as the latest release.
    latest: Option<kindil::update::Release>,
}

impl Host for Recording {
    fn brightness(&self) -> anyhow::Result<kindil::host::Brightness> {
        self.device.brightness()
    }
    fn set_brightness(&self, level: kindil::host::Brightness) -> anyhow::Result<()> {
        self.device.set_brightness(level)
    }
    fn wifi(&self) -> kindil::host::Wifi {
        self.device.wifi()
    }
    fn set_wifi(&self, power: kindil::host::Power) -> anyhow::Result<()> {
        self.device.set_wifi(power)
    }
    fn reconnect_wifi(&self) -> anyhow::Result<()> {
        self.device.reconnect_wifi()
    }
    fn scan(&self) -> anyhow::Result<Vec<kindil::wifi::Network>> {
        self.device.scan()
    }
    fn join(&self, credentials: &Credentials) -> anyhow::Result<()> {
        let entry = match credentials {
            Credentials::Open(ssid) => (ssid.as_str().to_owned(), false),
            Credentials::Wpa(ssid, _) => (ssid.as_str().to_owned(), true),
        };
        self.joined.lock().unwrap().push(entry);
        self.device.join(credentials)
    }
    fn saved_networks(&self) -> Vec<String> {
        self.device.saved_networks()
    }
    fn forget_network(&self, ssid: &kindil::wifi::Ssid) -> anyhow::Result<()> {
        self.device.forget_network(ssid)
    }
    fn ssh(&self) -> kindil::host::Ssh {
        self.device.ssh()
    }
    fn set_ssh(&self, power: kindil::host::Power) -> anyhow::Result<()> {
        self.device.set_ssh(power)
    }
    fn set_ssh_at_boot(&self, power: kindil::host::Power) -> anyhow::Result<()> {
        self.device.set_ssh_at_boot(power)
    }
    fn own_ssh(&self) -> kindil::host::OwnSsh {
        self.device.own_ssh()
    }
    fn trust_ssh_keys(&self, fingerprints: &[String]) -> anyhow::Result<()> {
        self.device.trust_ssh_keys(fingerprints)
    }
    fn ignore_ssh_keys(&self, fingerprints: &[String]) -> anyhow::Result<()> {
        self.device.ignore_ssh_keys(fingerprints)
    }
    fn boot_start(&self) -> kindil::host::BootStart {
        self.device.boot_start()
    }
    fn set_boot_start(&self, boot: kindil::host::Boot) -> anyhow::Result<()> {
        self.device.set_boot_start(boot)
    }
    fn info(&self) -> kindil::host::DeviceInfo {
        self.device.info()
    }
    fn listen(&self, deliver: kindil::host::Deliver) {
        self.device.listen(deliver)
    }
    fn suspend(&self) -> anyhow::Result<()> {
        self.device.suspend()
    }
    fn restart(&self) -> anyhow::Result<()> {
        self.device.restart()
    }
    fn power_off(&self) -> anyhow::Result<()> {
        self.device.power_off()
    }
    fn nap(&self, wake_after: std::time::Duration) -> anyhow::Result<()> {
        self.device.nap(wake_after)
    }
    fn check_update(&self) -> anyhow::Result<kindil::update::Check> {
        Ok(match &self.latest {
            Some(release) => {
                kindil::update::Check::of(kindil::update::Version::current(), release.clone())
            }
            None => kindil::update::Check::UpToDate,
        })
    }
    fn stage_update(&self, release: &kindil::update::Release) -> anyhow::Result<()> {
        self.device.stage_update(release)
    }
    fn commit_update(&self) -> anyhow::Result<kindil::update::Commit> {
        self.device.commit_update()
    }
    fn search_places(&self, name: &str) -> anyhow::Result<Vec<kindil::location::Place>> {
        self.device.search_places(name)
    }
    fn locate_by_ip(&self) -> anyhow::Result<kindil::location::Place> {
        self.device.locate_by_ip()
    }
}

#[test]
fn join_a_network_and_type_arabic() {
    let host = Arc::new(Recording::default());
    let f = Fixture::new(host.clone());
    f.tap(SETTINGS_TAB, TAB_Y);
    f.tap(380.0, 480.0); // Wi-Fi row
    assert_eq!(f.k().get_page(), Page::Wifi);
    f.tap(380.0, 300.0); // Choose a network
    assert_eq!(f.k().get_page(), Page::Networks);
    assert_eq!(f.k().get_networks().row_count(), 3);
    f.tap(380.0, 228.0); // Home
    assert!(f.k().get_join_open());
    assert_eq!(f.k().get_join_ssid(), "Home");

    f.tap(380.0, 869.0); // password field
    assert!(f.keyboard_visible());
    [109.0, 171.0, 232.0, 293.0, 354.0, 415.0, 476.0, 537.0]
        .into_iter()
        .for_each(|x| f.tap(x, 742.0));
    // The sheet sits above the keyboard, so Join stays reachable.
    f.tap(570.0, 633.0);
    assert!(!f.k().get_join_open());
    assert!(!f.keyboard_visible());
    assert_eq!(*host.joined.lock().unwrap(), [("Home".to_string(), true)]);

    // A too-short password never reaches the device.
    f.k().invoke_join_network("Home".into(), "short".into());
    assert_eq!(host.joined.lock().unwrap().len(), 1);
    assert!(f.k().get_network_status().contains("8–63"));

    // Arabic through real key taps: shaping, deletion by scalar.
    f.tap(380.0, 492.0); // Other network…
    assert_eq!(f.k().get_join_ssid(), "");
    f.tap(380.0, 785.0); // network name field
    f.tap(75.0, 934.0); // symbols
    f.tap(75.0, 934.0); // Arabic
    [
        (293.0, 742.0),
        (537.0, 806.0),
        (285.0, 870.0),
        (232.0, 806.0),
        (354.0, 806.0),
    ]
    .into_iter()
    .for_each(|(x, y)| f.tap(x, y));
    assert_eq!(f.k().get_join_ssid(), "قنديل");
    f.tap(721.0, 742.0); // backspace removes one scalar
    assert_eq!(f.k().get_join_ssid(), "قندي");
    if let Some(dir) = std::env::var_os("KINDIL_TEST_PREVIEW_DIR") {
        std::fs::create_dir_all(&dir).unwrap();
        preview::snapshot(&f.window, &Path::new(&dir).join("arabic-keyboard.ppm")).unwrap();
    }
    f.tap(42.0, 742.0); // Esc
    assert!(!f.keyboard_visible());
}

#[test]
fn a_joined_network_is_saved_and_can_be_forgotten() {
    let host = Arc::new(Simulated::default());
    let f = Fixture::new(host.clone());
    f.k()
        .invoke_join_network("Home".into(), "correct horse".into());
    f.app.refresh_now();
    f.tap(SETTINGS_TAB, TAB_Y);
    f.tap(380.0, 480.0); // Wi-Fi row
    assert_eq!(f.k().get_page(), Page::Wifi);
    let saved: Vec<String> = f
        .k()
        .get_saved_networks()
        .iter()
        .map(String::from)
        .collect();
    assert_eq!(saved, ["Home"]);
    if let Some(dir) = std::env::var_os("KINDIL_TEST_PREVIEW_DIR") {
        std::fs::create_dir_all(&dir).unwrap();
        preview::snapshot(&f.window, &Path::new(&dir).join("wifi-saved.ppm")).unwrap();
    }

    // Scanning marks it.
    f.tap(380.0, 300.0); // Choose a network
    let home = f.k().get_networks().row_data(0).unwrap();
    assert_eq!(home.ssid, "Home");
    assert!(home.detail.starts_with("Saved"), "{}", home.detail);
    f.tap(40.0, 112.0); // back

    // Forget takes two taps.
    f.tap(FORGET.0, FORGET.1);
    assert_eq!(host.saved_networks(), ["Home"], "one tap only asks");
    f.tap(FORGET.0, FORGET.1);
    assert!(host.saved_networks().is_empty());
    assert_eq!(f.k().get_saved_networks().row_count(), 0);
}

#[test]
fn arabic_text_is_shaped() {
    let f = Fixture::new(Arc::new(Simulated::default()));
    f.k().set_dashboard(DashboardKind::Clock);
    f.k().set_clock("".into());
    // Ink extent of the clock page's date.
    let span = |date: &str| {
        f.k().set_date(date.into());
        let frame = pixels(&f.window);
        let xs: Vec<usize> = (0..758)
            .filter(|x| (300..720).any(|y| frame[y * 758 + x].r < 128))
            .collect();
        xs.last().unwrap() - xs.first().unwrap()
    };
    // ZWNJ disables contextual joining; a shaped word is narrower.
    let joined = span("قنديل");
    let disconnected = span("ق\u{200c}ن\u{200c}د\u{200c}ي\u{200c}ل");
    assert!(joined < disconnected, "{joined} vs {disconnected}");
}

#[test]
fn every_printable_ascii_key_exists() {
    let f = Fixture::new(Arc::new(Simulated::default()));
    let sets = f
        .app
        .window
        .global::<kindil::ui::VirtualKeyboardHandler>()
        .get_default_key_sets();
    let available: std::collections::HashSet<char> = sets
        .iter()
        .flat_map(|set| set.iter().collect::<Vec<_>>())
        .flat_map(|row| row.iter().collect::<Vec<_>>())
        .flat_map(|key| {
            key.key
                .chars()
                .chain(key.shift_key.chars())
                .collect::<Vec<_>>()
        })
        .chain([' '])
        .collect();
    let missing: Vec<char> = (32u8..=126)
        .map(char::from)
        .filter(|c| !available.contains(c))
        .collect();
    assert!(missing.is_empty(), "missing keys: {missing:?}");
}

#[test]
fn lists_follow_the_finger_in_fast_frames_while_it_moves() {
    use slint::platform::{PointerEventButton, WindowAdapter, WindowEvent};
    use std::sync::atomic::Ordering;
    let f = Fixture::new(Arc::new(Simulated::default()));
    // Enough books to scroll; the library reloads when its tab opens.
    (1..=20).for_each(|n| {
        std::fs::write(
            f.dir.path().join(format!("books/Book_{n:02}.txt")),
            "Text\n",
        )
        .unwrap()
    });
    f.tap(LIBRARY_TAB, TAB_Y);
    let at = |y| slint::LogicalPosition::new(380.0, y);
    let send = |event| {
        f.window.window().dispatch_event(event);
        slint::platform::update_timers_and_animations();
    };
    // The list, left of the scroll bar, below the header.
    let list = |frame: &[slint::Rgb8Pixel], y: usize| frame[y * 758..y * 758 + 740].to_vec();
    let before = pixels(&f.window);
    send(WindowEvent::PointerPressed {
        position: at(800.0),
        button: PointerEventButton::Left,
    });
    [700.0, 550.0, 400.0]
        .into_iter()
        .for_each(|y| send(WindowEvent::PointerMoved { position: at(y) }));
    assert!(
        f.motion.load(Ordering::Relaxed),
        "a drag puts the panel in its fast mode"
    );
    let moving = pixels(&f.window);
    assert!(
        (152..532).all(|y| list(&moving, y) == list(&before, y + 400)),
        "the list follows the finger while it is down"
    );
    send(WindowEvent::PointerReleased {
        position: at(400.0),
        button: PointerEventButton::Left,
    });
    assert!(
        !f.motion.load(Ordering::Relaxed),
        "lifting the finger ends the fast mode, so the panel cleans up"
    );
    let after = pixels(&f.window);
    assert!(
        (152..532).all(|y| list(&after, y) == list(&before, y + 400)),
        "release settles the list where the finger lifted"
    );
    drag(&f.window, 380.0, 300.0, 800.0);
    assert!(
        pixels(&f.window) == before,
        "swiping back down returns to the top"
    );
}

#[test]
fn updates_report_the_latest_release() {
    let f = Fixture::new(Arc::new(Simulated::default()));
    let version = env!("CARGO_PKG_VERSION");
    assert_eq!(f.k().get_update_status(), format!("Version {version}"));
    f.k().invoke_update();
    assert_eq!(
        f.k().get_update_status(),
        format!("{version} is the latest")
    );
    assert_eq!(f.k().get_update_button(), "Check");
    assert!(f.k().get_update_enabled());
}

#[test]
fn an_update_that_cannot_install_says_why_and_kindil_keeps_running() {
    let release = kindil::update::Release::decode(
        r#"{"tag_name": "v999.0.0", "assets": [
            {"name": "kindil", "url": "https://api.github.com/a/1"},
            {"name": "kindil.minisig", "url": "https://api.github.com/a/2"}
        ]}"#,
    )
    .unwrap();
    let f = Fixture::new(Arc::new(Recording {
        latest: Some(release),
        ..Recording::default()
    }));
    f.k().invoke_update();
    assert!(f
        .k()
        .get_update_status()
        .starts_with("999.0.0 is available"));
    assert_eq!(f.k().get_update_button(), "Install");
    f.k().invoke_update();
    assert!(f.k().get_update_status().contains("cannot update"));
    assert_eq!(f.app.exit(), kindil::ui::Exit::Quit);
}

#[test]
fn new_ssh_keys_on_the_usb_storage_wait_to_be_trusted() {
    let host = Arc::new(Simulated::default());
    let f = Fixture::new(host.clone());
    f.k().invoke_go(Page::Remote);
    assert_eq!(f.k().get_ssh_new().row_count(), 1);
    assert_eq!(f.k().get_ssh_trusted().row_count(), 1);
    let new = f.k().get_ssh_new().row_data(0).unwrap();
    assert_eq!(new.title, "RSA · 3072");
    assert!(new.fingerprint.starts_with("SHA256:"));
    assert!(f.k().get_own_ssh_detail().contains("2222"));
    f.k().invoke_trust_ssh_keys();
    assert_eq!(f.k().get_ssh_new().row_count(), 0);
    assert_eq!(f.k().get_ssh_trusted().row_count(), 2);
    assert_eq!(host.own_ssh().trusted.len(), 2);
    assert!(f.k().get_notice().contains("Trusted 1 SSH key"));
}

#[test]
fn ignored_ssh_keys_are_not_offered_again() {
    let host = Arc::new(Simulated::default());
    let f = Fixture::new(host.clone());
    f.k().invoke_go(Page::Remote);
    f.k().invoke_ignore_ssh_keys();
    assert_eq!(f.k().get_ssh_new().row_count(), 0);
    assert_eq!(f.k().get_ssh_trusted().row_count(), 1);
    assert!(host.own_ssh().new_on_usb.is_empty());
}
