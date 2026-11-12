//! The production window and renderer, driven by real taps on a simulated
//! device. Coordinates are the 758×1024 panel's; see ui/app.slint for layout.
use kindil::config::Config;
use kindil::host::{Host, Simulated};
use kindil::preview::{self, drag, pixels, tap};
use kindil::ui::{App, DashboardKind, Execution, Kindil, Options, Page, Panel, Switch};
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

struct Fixture {
    app: App,
    window: Rc<MinimalSoftwareWindow>,
    dir: tempfile::TempDir,
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
        let app = App::new(Options {
            config,
            host,
            panel: Panel::Lcd,
            execution: Execution::Inline,
        })
        .unwrap();
        app.refresh_now();
        app.window.show().unwrap();
        Self { app, window, dir }
    }

    fn k(&self) -> Kindil<'_> {
        self.app.window.global::<Kindil>()
    }

    fn tap(&self, x: f32, y: f32) {
        let _ = pixels(&self.window);
        tap(&self.window, x, y);
        let _ = pixels(&self.window);
    }

    fn books(&self) -> std::path::PathBuf {
        self.dir.path().join("books")
    }

    fn keyboard_visible(&self) -> bool {
        self.app.window.get_keyboard_visible()
    }
}

#[test]
fn miqat_is_pixel_exact_and_taps_reveal_the_tabs() {
    let f = Fixture::new(Arc::new(Simulated::default()));
    let miqat = miqat::config::Config::load(Path::new("examples/miqat.toml")).unwrap();
    let noon = chrono::DateTime::from_timestamp(1_794_398_400, 0).unwrap();
    let gray = kindil::dashboards::preview(&miqat, noon).unwrap();
    let expected = gray.pixels.clone();
    f.app.set_miqat(gray);
    f.k().set_dashboard(DashboardKind::Miqat);
    let rendered = pixels(&f.window);
    assert!(
        rendered
            .iter()
            .zip(&expected)
            .all(|(p, g)| p.r == *g && p.g == *g && p.b == *g),
        "the dashboard must be Miqat's frame, without chrome or scaling"
    );
    f.tap(380.0, 500.0);
    assert!(f.k().get_chrome_visible());
    f.tap(LIBRARY_TAB, TAB_Y);
    assert_eq!(f.k().get_page(), Page::Library);
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
    f.tap(700.0, 380.0); // front light +
    assert_eq!(f.k().get_brightness(), 50);
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

/// Records what the UI asked to join, so no passphrase has to be exposed.
#[derive(Default)]
struct Recording {
    device: Simulated,
    joined: Mutex<Vec<(String, bool)>>,
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
    // Ink extent of the status-bar date, left of the Wi-Fi and battery icons.
    let span = |date: &str| {
        f.k().set_date(date.into());
        let frame = pixels(&f.window);
        let xs: Vec<usize> = (20..500)
            .filter(|x| (8..48).any(|y| frame[y * 758 + x].r < 128))
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
fn lists_follow_the_swipe_with_few_panel_updates() {
    use slint::platform::{PointerEventButton, WindowAdapter, WindowEvent};
    let f = Fixture::new(Arc::new(Simulated::default()));
    f.tap(SETTINGS_TAB, TAB_Y);
    let at = |y| slint::LogicalPosition::new(380.0, y);
    let send = |event| {
        f.window.window().dispatch_event(event);
        slint::platform::update_timers_and_animations();
    };
    // The list, left of the scroll bar, below the header.
    let list = |frame: &[slint::Rgb8Pixel], y: usize| frame[y * 758..y * 758 + 740].to_vec();
    let bar = |frame: &[slint::Rgb8Pixel]| {
        (152..932)
            .map(|y| frame[y * 758 + 740..y * 758 + 758].to_vec())
            .collect::<Vec<_>>()
    };
    let before = pixels(&f.window);
    send(WindowEvent::PointerPressed {
        position: at(800.0),
        button: PointerEventButton::Left,
    });
    [700.0, 550.0, 400.0]
        .into_iter()
        .for_each(|y| send(WindowEvent::PointerMoved { position: at(y) }));
    let moving = pixels(&f.window);
    assert!(
        (152..932).all(|y| list(&moving, y) == list(&before, y)),
        "the list must not redraw on every finger movement"
    );
    assert!(
        bar(&moving) != bar(&before),
        "the scroll bar follows the finger"
    );
    std::thread::sleep(std::time::Duration::from_millis(450));
    let caught_up = pixels(&f.window);
    assert!(
        (152..532).all(|y| list(&caught_up, y) == list(&before, y + 400)),
        "the list catches up with the finger while it is down"
    );
    send(WindowEvent::PointerReleased {
        position: at(400.0),
        button: PointerEventButton::Left,
    });
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
