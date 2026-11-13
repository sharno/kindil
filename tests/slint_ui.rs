//! The production window and renderer, driven by real taps on a simulated
//! device. Coordinates are the 758×1024 panel's; see ui/app.slint for layout.
use kindil::battery::{Battery, Charge};
use kindil::config::Config;
use kindil::host::{Host, Simulated};
use kindil::preview::{self, drag, pixels, tap};
use kindil::system::{Drive, Event, Touches};
use kindil::ui::{App, DashboardKind, Execution, Kindil, Options, Page, Panel, Switch, System};
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
        let dir = tempfile::tempdir().unwrap();
        let books = dir.path().join("books");
        std::fs::create_dir(&books).unwrap();
        std::fs::write(
            books.join("A_Real_Book.txt"),
            "A real book\nقنديل\n".repeat(400),
        )
        .unwrap();
        let config = Config {
            books,
            prefs: dir.path().join("prefs.toml"),
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
            execution: Execution::Inline,
            touches: Touches::default(),
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
fn settings_switch_the_simulated_device() {
    let f = Fixture::new(Arc::new(Simulated::default()));
    f.tap(SETTINGS_TAB, TAB_Y);
    assert_eq!(f.k().get_page(), Page::Settings);
    f.tap(571.0, 246.0); // Clock
    assert_eq!(f.k().get_dashboard(), DashboardKind::Clock);
    let prefs = std::fs::read_to_string(f.dir.path().join("prefs.toml")).unwrap();
    assert!(prefs.contains("dashboard = \"clock\""), "{prefs}");
    assert_eq!(f.k().get_brightness(), 40);
    f.slide(SETTINGS_LIGHT, SETTINGS_LIGHT_Y, 40.0, 70.0); // front light slider
    assert!((f.k().get_brightness() - 70).abs() <= SLACK);
    assert_eq!(f.k().get_ssh(), Switch::On);
    f.tap(380.0, 829.0); // SSH server row
    assert_eq!(f.k().get_ssh(), Switch::Off);
    assert_eq!(f.k().get_sync(), Switch::Unavailable);
    drag(&f.window, 380.0, 900.0, 200.0);
    drag(&f.window, 380.0, 900.0, 200.0);
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
    f.app.device_event(Event::PowerButton);
    assert_eq!(host.brightness().unwrap().percent(), 40);
    f.app.fire_timers();
    assert_eq!(host.naps(), 3, "awake: no more naps");
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
    fn ssh(&self) -> kindil::host::Ssh {
        self.device.ssh()
    }
    fn set_ssh(&self, power: kindil::host::Power) -> anyhow::Result<()> {
        self.device.set_ssh(power)
    }
    fn set_ssh_at_boot(&self, power: kindil::host::Power) -> anyhow::Result<()> {
        self.device.set_ssh_at_boot(power)
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
}

#[test]
fn join_a_network_and_type_arabic() {
    let host = Arc::new(Recording::default());
    let f = Fixture::new(host.clone());
    f.tap(SETTINGS_TAB, TAB_Y);
    f.tap(380.0, 695.0); // Choose a network
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
    f.tap(SETTINGS_TAB, TAB_Y);
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
