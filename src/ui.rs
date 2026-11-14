//! Wires the Slint window to the device. Callbacks are one-liners into
//! [`Ctx`]; slow device calls run on worker threads and report back through
//! the event loop, so a timeout never freezes a tap.
use crate::battery::{Battery, Charge};
use crate::config::{Config, Dashboard};
use crate::drain::{Drain, Gauge};
use crate::files::{FileName, Files, Trashed};
use crate::ghosting::{Ghosting, Refresh, Update};
use crate::host::{BootStart, Brightness, DeviceInfo, Host, Power, Ssh, Wifi};
use crate::prefs::Store;
use crate::reader::{self, Book, Layout, Reader};
use crate::sync::Service;
use crate::system::{
    AutoSleep, Effect, Event as DeviceEvent, Idle, Mode, Step, Touches, Window, DRIVE_SETTLE,
    FETCH_AFTER, MIN_NAP, PANEL_SETTLE,
};
use crate::update::{self, Check, Release, Updates, Version};
use crate::view;
use crate::wifi::{Credentials, Ssid};
use chrono::{DateTime, Timelike, Utc};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

slint::include_modules!();

/// The display's switches.
#[derive(Clone)]
pub enum Panel {
    Eink {
        /// Set it and the next drawn frame is a full GC16 refresh.
        full_refresh: Arc<AtomicBool>,
        /// Set while content moves: fast black-and-white frames.
        motion: Arc<AtomicBool>,
    },
    /// Desktop windows and previews have nothing to clean.
    Lcd,
}

/// Where slow device calls run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Execution {
    /// Worker threads; results come back through the event loop.
    Background,
    /// On the caller's thread: previews and tests have no event loop to post to.
    Inline,
}

pub struct Options {
    pub config: Config,
    pub host: Arc<dyn Host>,
    pub panel: Panel,
    pub execution: Execution,
    /// Touches seen by the panel, for auto-sleep.
    pub touches: Touches,
    /// The wall clock: `Utc::now`, or a fixed instant in tests.
    pub now: fn() -> DateTime<Utc>,
}

/// The e-ink ghosting policy applied to a panel. Send, so workers can use it.
#[derive(Clone)]
struct Display {
    panel: Panel,
    ghosting: Arc<Mutex<Ghosting>>,
}

impl Display {
    fn updated(&self, ui: &AppWindow, update: Update) {
        let refresh = {
            let mut ghosting = lock(&self.ghosting);
            let (next, refresh) = ghosting.after(update);
            *ghosting = next;
            refresh
        };
        match (refresh, &self.panel) {
            (Refresh::Full, Panel::Eink { full_refresh, .. }) => {
                full_refresh.store(true, Ordering::Relaxed);
                ui.window().request_redraw();
            }
            (Refresh::Full, Panel::Lcd) | (Refresh::Partial, _) => {}
        }
    }

    fn motion(&self, ui: &AppWindow, motion: Motion) {
        match &self.panel {
            Panel::Eink { motion: flag, .. } => {
                flag.store(motion == Motion::Moving, Ordering::Relaxed);
                ui.window().request_redraw();
            }
            Panel::Lcd => {}
        }
    }
}

enum Reading {
    Closed,
    Open { book: Book, reader: Reader },
}

#[derive(Clone, Copy)]
enum Direction {
    Forward,
    Back,
}

enum Worker {
    Idle,
    /// Takes ticks besides its own minute clock, which stops while dozing.
    Started(mpsc::Sender<Tick>),
}

/// What the dashboard worker does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tick {
    /// Render the minute now; no network, which may still be down after a nap.
    Draw,
    /// Fetch what is due, then render.
    Fetch,
}

/// UI-thread state. Borrowed briefly inside one callback, never across a
/// call back into Slint that could re-enter Rust.
struct State {
    store: Store,
    books: Vec<Book>,
    reading: Reading,
    sync: Service,
    trashed: Option<Trashed>,
    miqat: Worker,
    system: Mode,
    idle: Idle,
    /// When dozing last stayed up for the network.
    online: Option<DateTime<Utc>>,
}

/// A poisoned lock still holds the last value written, which is all kindil needs.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// How the app ended, for the launcher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// Back to Amazon's UI.
    Quit,
    /// An update is staged: the launcher starts kindil again.
    Restart,
}

/// Device events from any thread, queued for the UI thread.
#[derive(Clone)]
struct Events {
    sender: mpsc::Sender<DeviceEvent>,
    queue: Rc<mpsc::Receiver<DeviceEvent>>,
}

#[derive(Clone)]
struct Ctx {
    weak: slint::Weak<AppWindow>,
    host: Arc<dyn Host>,
    config: Rc<Config>,
    state: Rc<RefCell<State>>,
    display: Display,
    execution: Execution,
    events: Events,
    touches: Touches,
    /// Shared with the workers that check and install.
    updates: Arc<Mutex<Updates>>,
    exit: Arc<Mutex<Exit>>,
    /// The gauge at this doze's last online window, on battery; shared with
    /// the worker that reads the next one.
    doze_gauge: Arc<Mutex<Option<Gauge>>>,
    /// Timers under [`Execution::Inline`]: tests fire them one round at a time.
    timers: Rc<RefCell<Vec<DeviceEvent>>>,
    now: fn() -> DateTime<Utc>,
}

pub struct App {
    pub window: AppWindow,
    ctx: Ctx,
    clock: Rc<slint::Timer>,
    sync_poll: slint::Timer,
}

impl From<Dashboard> for DashboardKind {
    fn from(dashboard: Dashboard) -> Self {
        match dashboard {
            Dashboard::Miqat => DashboardKind::Miqat,
            Dashboard::Clock => DashboardKind::Clock,
        }
    }
}

impl From<DashboardKind> for Dashboard {
    fn from(kind: DashboardKind) -> Self {
        match kind {
            DashboardKind::Miqat => Dashboard::Miqat,
            DashboardKind::Clock => Dashboard::Clock,
        }
    }
}

/// Everything the Settings screen and status bar show, read in one go.
struct Snapshot {
    wifi: Wifi,
    saved_networks: Vec<String>,
    ssh: Ssh,
    boot_start: BootStart,
    info: DeviceInfo,
    brightness: Option<Brightness>,
}

impl Snapshot {
    fn read(host: &dyn Host) -> Self {
        Self {
            wifi: host.wifi(),
            saved_networks: host.saved_networks(),
            ssh: host.ssh(),
            boot_start: host.boot_start(),
            info: host.info(),
            brightness: host.brightness().ok(),
        }
    }

    fn apply(self, ui: &AppWindow) {
        let k = ui.global::<Kindil>();
        let (level, label) = view::battery(self.info.battery);
        k.set_battery_level(level);
        k.set_battery_label(label.into());
        k.set_battery_empty(crate::system::battery_event(self.info.battery).is_some());
        k.set_wifi_bars(view::wifi_bars(&self.wifi));
        let (wifi, detail) = view::wifi(&self.wifi);
        k.set_wifi(wifi);
        k.set_wifi_detail(detail.into());
        k.set_saved_networks(model(
            self.saved_networks
                .iter()
                .map(|ssid| ssid.as_str().into())
                .collect(),
        ));
        let (server, at_boot, detail) = view::ssh(self.ssh, view::ip(&self.wifi));
        k.set_ssh(server);
        k.set_ssh_at_boot(at_boot);
        k.set_ssh_detail(detail.into());
        let (boot, detail) = view::boot_start(self.boot_start);
        k.set_boot(boot);
        k.set_boot_start_detail(detail.into());
        k.set_facts(model(view::facts(&self.info, env!("CARGO_PKG_VERSION"))));
        k.set_brightness(self.brightness.map_or(-1, |b| i32::from(b.percent())));
    }
}

/// The Settings segment for an auto-sleep choice; -1 for a hand-edited value.
fn sleep_choice(choice: AutoSleep) -> i32 {
    AutoSleep::CHOICES
        .iter()
        .position(|c| *c == choice)
        .and_then(|i| i32::try_from(i).ok())
        .unwrap_or(-1)
}

fn model<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::from(Rc::new(VecModel::from(items)))
}

fn notify(ui: &AppWindow, text: &str) {
    let k = ui.global::<Kindil>();
    k.set_notice_undo(false);
    k.set_notice(text.into());
}

/// Time until the next wall-clock minute, so the clock flips on the minute.
pub fn until_next_minute(now: DateTime<Utc>) -> Duration {
    let into_minute = Duration::new(u64::from(now.second()), now.nanosecond() % 1_000_000_000);
    Duration::from_secs(60).saturating_sub(into_minute) + Duration::from_millis(50)
}

impl App {
    pub fn new(options: Options) -> Result<Self, slint::PlatformError> {
        let window = AppWindow::new()?;
        let store = Store::open(&options.config.prefs);
        let sync = Service::new(options.config.syncthing.clone());
        let dashboard = store.prefs.dashboard.unwrap_or(options.config.dashboard);
        let (sender, queue) = mpsc::channel();
        let ctx = Ctx {
            weak: window.as_weak(),
            host: options.host,
            state: Rc::new(RefCell::new(State {
                store,
                books: Vec::new(),
                reading: Reading::Closed,
                sync,
                trashed: None,
                miqat: Worker::Idle,
                system: Mode::start(Instant::now()),
                idle: Idle::new(options.touches.count(), Instant::now()),
                online: None,
            })),
            display: Display {
                panel: options.panel,
                ghosting: Arc::new(Mutex::new(Ghosting::default())),
            },
            config: Rc::new(options.config),
            execution: options.execution,
            now: options.now,
            events: Events {
                sender,
                queue: Rc::new(queue),
            },
            touches: options.touches,
            updates: Arc::default(),
            exit: Arc::new(Mutex::new(Exit::Quit)),
            doze_gauge: Arc::default(),
            timers: Rc::default(),
        };
        {
            let k = window.global::<Kindil>();
            k.set_dashboard(dashboard.into());
            k.set_version(format!("v{}", env!("CARGO_PKG_VERSION")).into());
            show_updates(&window, &ctx.updates, Updates::Idle);
            k.set_books_root(ctx.config.books.display().to_string().into());
            k.set_font_size(i32::from(ctx.state.borrow().store.prefs.font.px()));
            k.set_sleep_choices(model(
                AutoSleep::CHOICES
                    .iter()
                    .map(|choice| choice.label().into())
                    .collect(),
            ));
            k.set_auto_sleep(sleep_choice(ctx.state.borrow().store.prefs.auto_sleep));
        }
        install_keyboard(&window);
        wire(&window, &ctx);
        ctx.restore_light();
        ctx.reload_library();
        ctx.show_sync();
        ctx.tick_clock();
        Ok(Self {
            window,
            ctx,
            clock: Rc::new(slint::Timer::default()),
            sync_poll: slint::Timer::default(),
        })
    }

    /// Read the device now, on this thread. Previews and tests use this.
    pub fn refresh_now(&self) {
        Snapshot::read(&*self.ctx.host).apply(&self.window);
        self.ctx.show_sync();
    }

    /// Clock, status, the dashboard worker, and Syncthing if it was left on.
    pub fn start(&self) {
        self.ctx.host.listen(Box::new(self.ctx.deliver()));
        self.ctx.refresh_status();
        arm_clock(self.clock.clone(), self.ctx.clone());
        let ctx = self.ctx.clone();
        self.sync_poll.start(
            slint::TimerMode::Repeated,
            Duration::from_secs(30),
            move || ctx.poll_sync(),
        );
        if self.window.global::<Kindil>().get_dashboard() == DashboardKind::Miqat {
            self.ctx.start_miqat();
        }
        if self.ctx.state.borrow().store.prefs.sync == Power::On {
            self.ctx.toggle_sync();
        }
        let ctx = self.ctx.clone();
        slint::Timer::single_shot(update::SETTLE, move || ctx.commit_update());
    }

    pub fn exit(&self) -> Exit {
        *lock(&self.ctx.exit)
    }

    pub fn set_miqat(&self, gray: miqat::image::Gray) {
        let k = self.window.global::<Kindil>();
        k.set_miqat_frame(slint::Image::from_rgb8(pixels(gray)));
    }

    /// A power press, USB drive mode, ... as the device would report it.
    pub fn device_event(&self, event: DeviceEvent) {
        self.ctx.device(event);
    }

    /// Under [`Execution::Inline`], delivers the timers armed so far; timers
    /// they arm wait for the next call. Dozing would otherwise loop forever.
    pub fn fire_timers(&self) {
        let due = std::mem::take(&mut *self.ctx.timers.borrow_mut());
        due.into_iter().for_each(|event| self.ctx.device(event));
    }

    /// Stops Syncthing and keeps the reading position before the window goes.
    pub fn shutdown(&self) {
        self.ctx.release_storage();
    }
}

fn show_updates(ui: &AppWindow, state: &Mutex<Updates>, updates: Updates) {
    let (status, button, enabled) = view::updates(&updates, Version::current());
    let k = ui.global::<Kindil>();
    k.set_update_status(status.into());
    k.set_update_button(button.into());
    k.set_update_enabled(enabled);
    *lock(state) = updates;
}

fn install_keyboard(window: &AppWindow) {
    // Slint's MIT virtual keyboard sends keys through the normal input pipeline:
    // Slint owns selection, UTF-8, password masking and cursor editing.
    let weak = window.as_weak();
    window
        .global::<VirtualKeyboardHandler>()
        .on_key_pressed(move |key| {
            let Some(ui) = weak.upgrade() else { return };
            if key == slint::SharedString::from(slint::platform::Key::Escape) {
                ui.invoke_dismiss_keyboard();
                return;
            }
            ui.window()
                .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.clone() });
            ui.window()
                .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: key });
        });
}

fn wire(window: &AppWindow, ctx: &Ctx) {
    let k = window.global::<Kindil>();
    macro_rules! on {
        ($setter:ident => $method:ident($($arg:ident),*)) => {{
            let ctx = ctx.clone();
            k.$setter(move |$($arg),*| ctx.$method($($arg),*));
        }};
    }
    on!(on_navigated => navigated(page));
    on!(on_choose_dashboard => choose_dashboard(kind));
    on!(on_open_book => open_book(index));
    on!(on_rename_book => rename_book(index, name));
    on!(on_trash_book => trash_book(index));
    on!(on_undo => undo());
    on!(on_next_page => next_page());
    on!(on_previous_page => previous_page());
    on!(on_font_larger => font_larger());
    on!(on_font_smaller => font_smaller());
    on!(on_choose_brightness => choose_brightness(percent));
    on!(on_clean_screen => clean_screen());
    on!(on_motion => motion(motion));
    on!(on_toggle_wifi => toggle_wifi());
    on!(on_scan_networks => scan_networks());
    on!(on_join_network => join_network(ssid, password));
    on!(on_forget_network => forget_network(ssid));
    on!(on_toggle_ssh => toggle_ssh());
    on!(on_toggle_ssh_at_boot => toggle_ssh_at_boot());
    on!(on_choose_boot => choose_boot(choice));
    on!(on_toggle_sync => toggle_sync());
    on!(on_exit_app => exit_app());
    on!(on_device_events => device_events());
    on!(on_restart => restart());
    on!(on_power_off => power_off());
    on!(on_choose_auto_sleep => choose_auto_sleep(index));
    on!(on_update => update());
}

fn arm_clock(timer: Rc<slint::Timer>, ctx: Ctx) {
    let next = timer.clone();
    timer.start(
        slint::TimerMode::SingleShot,
        until_next_minute((ctx.now)()),
        move || {
            ctx.tick_clock();
            ctx.refresh_status();
            ctx.check_idle();
            arm_clock(next.clone(), ctx.clone());
        },
    );
}

fn pixels(gray: miqat::image::Gray) -> slint::SharedPixelBuffer<slint::Rgb8Pixel> {
    let mut buffer = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(gray.width, gray.height);
    buffer
        .make_mut_slice()
        .iter_mut()
        .zip(gray.pixels)
        .for_each(|(pixel, g)| *pixel = slint::Rgb8Pixel { r: g, g, b: g });
    buffer
}

impl Ctx {
    fn ui(&self) -> Option<AppWindow> {
        self.weak.upgrade()
    }

    /// Run a slow device call off the UI thread and apply its result on it.
    fn background<T: Send + 'static>(
        &self,
        work: impl FnOnce(&dyn Host) -> T + Send + 'static,
        done: impl FnOnce(&AppWindow, T) + Send + 'static,
    ) {
        match self.execution {
            Execution::Inline => {
                let value = work(&*self.host);
                if let Some(ui) = self.ui() {
                    done(&ui, value);
                }
            }
            Execution::Background => {
                let host = self.host.clone();
                let weak = self.weak.clone();
                std::thread::spawn(move || {
                    let value = work(&*host);
                    let _ = weak.upgrade_in_event_loop(move |ui| done(&ui, value));
                });
            }
        }
    }

    fn tick_clock(&self) {
        let Some(ui) = self.ui() else { return };
        let (clock, date) = view::clock(&(self.now)().with_timezone(&self.config.timezone));
        let k = ui.global::<Kindil>();
        k.set_clock(clock.into());
        k.set_date(date.into());
    }

    fn refresh_status(&self) {
        let deliver = self.deliver();
        self.background(Snapshot::read, move |ui, snapshot| {
            let empty = crate::system::battery_event(snapshot.info.battery);
            snapshot.apply(ui);
            empty.into_iter().for_each(&deliver);
        });
    }

    /// Re-read the device after a switch, once the change has had time to land.
    fn refresh_after(&self, delay: Duration) {
        self.background(
            move |host| {
                std::thread::sleep(delay);
                Snapshot::read(host)
            },
            |ui, snapshot| snapshot.apply(ui),
        );
    }

    fn navigated(&self, page: Page) {
        if let Some(ui) = self.ui() {
            self.display.updated(&ui, Update::Navigate);
        }
        match page {
            Page::Library => self.reload_library(),
            Page::Settings
            | Page::Display
            | Page::Wifi
            | Page::Remote
            | Page::Power
            | Page::Updates
            | Page::About => {
                self.refresh_status();
                self.show_sync();
            }
            Page::Dashboard | Page::Reader | Page::Networks => {}
        }
    }

    fn choose_dashboard(&self, kind: DashboardKind) {
        let Some(ui) = self.ui() else { return };
        ui.global::<Kindil>().set_dashboard(kind);
        self.save(|prefs| prefs.dashboard = Some(kind.into()));
        if kind == DashboardKind::Miqat {
            self.start_miqat();
        }
    }

    fn save(&self, change: impl FnOnce(&mut crate::prefs::Prefs)) {
        let result = {
            let mut state = self.state.borrow_mut();
            change(&mut state.store.prefs);
            state.store.save()
        };
        if let (Err(error), Some(ui)) = (result, self.ui()) {
            notify(&ui, &format!("Could not save settings: {error}"));
        }
    }

    // ── library ──

    fn reload_library(&self) {
        let Some(ui) = self.ui() else { return };
        let items = {
            let mut state = self.state.borrow_mut();
            state.books = reader::library(&self.config.books);
            state
                .books
                .iter()
                .map(|book| {
                    view::book(
                        book,
                        state.store.prefs.positions.get(&book.file_name()).copied(),
                    )
                })
                .collect()
        };
        ui.global::<Kindil>().set_books(model(items));
    }

    fn book(&self, index: i32) -> Option<Book> {
        let index = usize::try_from(index).ok()?;
        self.state.borrow().books.get(index).cloned()
    }

    fn open_book(&self, index: i32) {
        let (Some(ui), Some(book)) = (self.ui(), self.book(index)) else {
            return;
        };
        let opened = {
            let state = self.state.borrow();
            let layout = Layout::for_font(state.store.prefs.font);
            let saved = state.store.prefs.positions.get(&book.file_name()).copied();
            Reader::open(&book.path, layout).map(|mut reader| {
                if let Some(progress) = saved {
                    reader.seek(progress);
                }
                reader
            })
        };
        match opened {
            Ok(reader) => {
                self.state.borrow_mut().reading = Reading::Open { book, reader };
                self.show_page(&ui);
                ui.global::<Kindil>().invoke_go(Page::Reader);
            }
            Err(error) => notify(&ui, &format!("Cannot open {}: {error}", book.file_name())),
        }
    }

    fn show_page(&self, ui: &AppWindow) {
        let k = ui.global::<Kindil>();
        match &self.state.borrow().reading {
            Reading::Closed => k.set_has_book(false),
            Reading::Open { reader, .. } => {
                k.set_has_book(true);
                k.set_book_title(reader.title.as_str().into());
                k.set_book_text(reader.visible().join("\n").into());
                k.set_book_position(view::position(reader.page(), reader.pages()).into());
                k.set_book_progress(f32::from(reader.progress()));
            }
        }
    }

    fn turn(&self, direction: Direction) {
        let Some(ui) = self.ui() else { return };
        let position = {
            let mut state = self.state.borrow_mut();
            match &mut state.reading {
                Reading::Closed => None,
                Reading::Open { book, reader } => {
                    match direction {
                        Direction::Forward => reader.next(),
                        Direction::Back => reader.previous(),
                    }
                    Some((book.file_name(), reader.progress()))
                }
            }
        };
        let Some((name, progress)) = position else {
            return;
        };
        self.show_page(&ui);
        self.display.updated(&ui, Update::PageTurn);
        self.save(|prefs| {
            prefs.positions.insert(name, progress);
        });
    }

    fn next_page(&self) {
        self.turn(Direction::Forward);
    }

    fn previous_page(&self) {
        self.turn(Direction::Back);
    }

    fn resize_font(&self, change: fn(crate::prefs::FontSize) -> crate::prefs::FontSize) {
        let Some(ui) = self.ui() else { return };
        let font = change(self.state.borrow().store.prefs.font);
        self.save(|prefs| prefs.font = font);
        if let Reading::Open { reader, .. } = &mut self.state.borrow_mut().reading {
            reader.relayout(Layout::for_font(font));
        }
        ui.global::<Kindil>().set_font_size(i32::from(font.px()));
        self.show_page(&ui);
        self.display.updated(&ui, Update::Navigate);
    }

    fn font_larger(&self) {
        self.resize_font(crate::prefs::FontSize::larger);
    }

    fn font_smaller(&self) {
        self.resize_font(crate::prefs::FontSize::smaller);
    }

    fn files(&self) -> std::io::Result<Files> {
        Files::new(&self.config.books)
    }

    fn rename_book(&self, index: i32, name: slint::SharedString) {
        let (Some(ui), Some(book)) = (self.ui(), self.book(index)) else {
            return;
        };
        let result = (|| {
            let old = FileName::try_from(book.file_name().as_str())?;
            let new = FileName::try_from(name.as_str())?;
            self.files()?.rename(&old, &new)
        })();
        match result {
            Ok(()) => {
                let old = book.file_name();
                self.save(|prefs| {
                    if let Some(progress) = prefs.positions.remove(&old) {
                        prefs.positions.insert(name.to_string(), progress);
                    }
                });
                notify(&ui, &format!("Renamed to {name}"));
            }
            Err(error) => notify(&ui, &error.to_string()),
        }
        self.reload_library();
    }

    fn trash_book(&self, index: i32) {
        let (Some(ui), Some(book)) = (self.ui(), self.book(index)) else {
            return;
        };
        let result = FileName::try_from(book.file_name().as_str())
            .and_then(|name| self.files()?.trash(&name));
        match result {
            Ok(trashed) => {
                let k = ui.global::<Kindil>();
                k.set_notice(format!("Moved {} to Trash", trashed.name()).into());
                k.set_notice_undo(true);
                self.state.borrow_mut().trashed = Some(trashed);
            }
            Err(error) => notify(&ui, &error.to_string()),
        }
        self.reload_library();
    }

    fn undo(&self) {
        let Some(ui) = self.ui() else { return };
        let Some(trashed) = self.state.borrow_mut().trashed.take() else {
            return;
        };
        match self.files().and_then(|files| files.restore(&trashed)) {
            Ok(()) => notify(&ui, &format!("Restored {}", trashed.name())),
            Err(error) => notify(&ui, &error.to_string()),
        }
        self.reload_library();
    }

    // ── display ──

    fn choose_brightness(&self, percent: i32) {
        let Some(ui) = self.ui() else { return };
        let level = Brightness::new(percent.clamp(0, 100) as u8);
        let k = ui.global::<Kindil>();
        match self.host.set_brightness(level) {
            Ok(()) => {
                k.set_brightness(i32::from(level.percent()));
                self.save(|prefs| prefs.light = Some(level));
            }
            Err(error) => notify(&ui, &format!("Front light: {error}")),
        }
    }

    /// The light as last chosen; after a reboot nothing else sets it.
    fn restore_light(&self) {
        let Some(level) = self.state.borrow().store.prefs.light else {
            return;
        };
        self.background(
            move |host| host.set_brightness(level),
            move |ui, result| match result {
                Ok(()) => ui
                    .global::<Kindil>()
                    .set_brightness(i32::from(level.percent())),
                Err(error) => notify(ui, &format!("Front light: {error}")),
            },
        );
    }

    fn motion(&self, motion: Motion) {
        if let Some(ui) = self.ui() {
            self.display.motion(&ui, motion);
        }
    }

    fn clean_screen(&self) {
        if let Some(ui) = self.ui() {
            self.display.updated(&ui, Update::Clean);
        }
    }

    // ── network and services ──

    /// The power a switch should move to, if it can be switched at all.
    fn target(switch: Switch) -> Option<Power> {
        match switch {
            Switch::On => Some(Power::Off),
            Switch::Off => Some(Power::On),
            Switch::Unavailable => None,
        }
    }

    fn switch_service(
        &self,
        current: Switch,
        name: &'static str,
        apply: fn(&dyn Host, Power) -> anyhow::Result<()>,
    ) {
        let Some(power) = Self::target(current) else {
            return;
        };
        self.background(
            move |host| apply(host, power).map(|()| Snapshot::read(host)),
            move |ui, result| match result {
                Ok(snapshot) => snapshot.apply(ui),
                Err(error) => notify(ui, &format!("{name}: {error}")),
            },
        );
    }

    fn toggle_wifi(&self) {
        let Some(ui) = self.ui() else { return };
        self.switch_service(ui.global::<Kindil>().get_wifi(), "Wi-Fi", |host, power| {
            host.set_wifi(power)
        });
        // wifid takes a few seconds to associate after switching on.
        self.refresh_after(Duration::from_secs(8));
    }

    fn toggle_ssh(&self) {
        let Some(ui) = self.ui() else { return };
        self.switch_service(ui.global::<Kindil>().get_ssh(), "SSH", |host, power| {
            host.set_ssh(power)
        });
    }

    fn toggle_ssh_at_boot(&self) {
        let Some(ui) = self.ui() else { return };
        self.switch_service(
            ui.global::<Kindil>().get_ssh_at_boot(),
            "SSH at boot",
            |host, power| host.set_ssh_at_boot(power),
        );
    }

    fn choose_boot(&self, choice: BootChoice) {
        let Some(boot) = view::boot(choice) else {
            return;
        };
        self.background(
            move |host| host.set_boot_start(boot).map(|()| Snapshot::read(host)),
            |ui, result| match result {
                Ok(snapshot) => snapshot.apply(ui),
                Err(error) => notify(ui, &format!("Start at boot: {error}")),
            },
        );
    }

    fn scan_networks(&self) {
        let Some(ui) = self.ui() else { return };
        let k = ui.global::<Kindil>();
        if k.get_network_busy() {
            return;
        }
        k.set_network_busy(true);
        k.set_network_status("Scanning…".into());
        self.background(
            |host| host.scan(),
            |ui, result| {
                let k = ui.global::<Kindil>();
                k.set_network_busy(false);
                match result {
                    Ok(networks) => {
                        let saved: Vec<slint::SharedString> =
                            slint::Model::iter(&k.get_saved_networks()).collect();
                        k.set_network_status(
                            match networks.len() {
                                1 => "1 network nearby".to_string(),
                                n => format!("{n} networks nearby"),
                            }
                            .into(),
                        );
                        k.set_networks(model(
                            networks
                                .iter()
                                .map(|network| {
                                    let mut item = view::network(network);
                                    if saved.contains(&item.ssid) {
                                        item.detail = format!("Saved · {}", item.detail).into();
                                    }
                                    item
                                })
                                .collect(),
                        ));
                    }
                    Err(error) => k.set_network_status(error.to_string().into()),
                }
            },
        );
    }

    fn join_network(&self, ssid: slint::SharedString, password: slint::SharedString) {
        let Some(ui) = self.ui() else { return };
        let k = ui.global::<Kindil>();
        let credentials = match Credentials::parse(ssid.to_string(), password.to_string()) {
            Ok(credentials) => credentials,
            Err(error) => {
                k.set_network_status(error.into());
                return;
            }
        };
        k.set_network_busy(true);
        k.set_network_status(format!("Joining {ssid}…").into());
        self.background(
            move |host| host.join(&credentials),
            move |ui, result| {
                let k = ui.global::<Kindil>();
                k.set_network_busy(false);
                k.set_network_status(
                    match result {
                        Ok(()) => format!("Saved {ssid}; joining. Status shows in Settings."),
                        Err(error) => error.to_string(),
                    }
                    .into(),
                );
            },
        );
        self.refresh_after(Duration::from_secs(10));
    }

    fn forget_network(&self, ssid: slint::SharedString) {
        let Ok(ssid) = Ssid::try_from(ssid.to_string()) else {
            return;
        };
        self.background(
            move |host| host.forget_network(&ssid).map(|()| Snapshot::read(host)),
            |ui, result| match result {
                Ok(snapshot) => snapshot.apply(ui),
                Err(error) => notify(ui, &format!("Wi-Fi: {error}")),
            },
        );
    }

    fn show_sync(&self) {
        let Some(ui) = self.ui() else { return };
        let (switch, detail) = view::sync(&self.state.borrow().sync, Instant::now());
        let k = ui.global::<Kindil>();
        k.set_sync(switch);
        k.set_sync_detail(detail.into());
    }

    fn toggle_sync(&self) {
        let power = {
            let mut state = self.state.borrow_mut();
            let sync = std::mem::replace(&mut state.sync, Service::NotInstalled);
            state.sync = sync.toggled();
            match state.sync {
                Service::Running(_) => Power::On,
                Service::NotInstalled | Service::Stopped(_) | Service::Failed(_) => Power::Off,
            }
        };
        self.save(|prefs| prefs.sync = power);
        self.show_sync();
    }

    fn poll_sync(&self) {
        {
            let mut state = self.state.borrow_mut();
            let sync = std::mem::replace(&mut state.sync, Service::NotInstalled);
            state.sync = sync.polled();
        }
        self.show_sync();
    }

    // ── device: sleep, USB drive mode, power ──

    /// Sends device events to the UI thread from anywhere.
    fn deliver(&self) -> impl Fn(DeviceEvent) + Send + Sync + 'static {
        let sender = self.events.sender.clone();
        let weak = self.weak.clone();
        move |event| {
            if sender.send(event).is_ok() {
                let _ =
                    weak.upgrade_in_event_loop(|ui| ui.global::<Kindil>().invoke_device_events());
            }
        }
    }

    fn device_events(&self) {
        std::iter::from_fn(|| self.events.queue.try_recv().ok()).for_each(|e| self.device(e));
    }

    fn device(&self, event: DeviceEvent) {
        let mode = self.state.borrow().system;
        match mode.step(event, Instant::now()) {
            Err(ignored) => eprintln!("kindil: {event:?} ignored: {ignored:?}"),
            Ok(Step { next, effects }) => {
                self.state.borrow_mut().system = next;
                effects.into_iter().for_each(|effect| self.apply(effect));
            }
        }
    }

    /// Runs slow device work off the UI thread and feeds its outcome back as an event.
    fn device_later(&self, work: impl FnOnce(&dyn Host) -> DeviceEvent + Send + 'static) {
        match self.execution {
            Execution::Inline => self.device(work(&*self.host)),
            Execution::Background => {
                let host = self.host.clone();
                let deliver = self.deliver();
                std::thread::spawn(move || deliver(work(&*host)));
            }
        }
    }

    /// Compares the gauge with the last online window's and shows the draw.
    fn measure_doze(&self, now: DateTime<Utc>) {
        let last = self.doze_gauge.clone();
        self.background(
            move |host| {
                let info = host.info();
                match (info.battery, info.charge) {
                    (
                        Battery::Known {
                            charge: Charge::Discharging,
                            ..
                        },
                        Some(left),
                    ) => Some(Gauge { at: now, left }),
                    _ => None,
                }
            },
            move |ui, gauge| {
                let from = std::mem::replace(&mut *lock(&last), gauge);
                if let Some(drain) = from
                    .zip(gauge)
                    .and_then(|(from, to)| Drain::between(from, to))
                {
                    eprintln!(
                        "kindil: doze drew {} mA over {} min, {} mAh left",
                        drain.milliamps,
                        drain.over.num_minutes(),
                        drain.left.0
                    );
                    ui.global::<Kindil>()
                        .set_doze_draw(view::doze_draw(drain).into());
                }
            },
        );
    }

    fn reconnect_wifi(&self) {
        self.background(
            |host| host.reconnect_wifi(),
            |_, result| {
                if let Err(error) = result {
                    eprintln!("kindil: reconnect Wi-Fi: {error}");
                }
            },
        );
    }

    fn apply(&self, effect: Effect) {
        let Some(ui) = self.ui() else { return };
        match effect {
            Effect::ShowSleep => self.show_system(&ui, System::Asleep),
            Effect::ShowUsbDrive => self.show_system(&ui, System::UsbDrive),
            Effect::LightOff => self.device_later(|host| {
                let level = host.brightness().ok();
                // A light that won't switch off doesn't stop the device sleeping.
                let _ = host.set_brightness(Brightness::new(0));
                DeviceEvent::LightOff(level)
            }),
            Effect::AwaitPanel => self.after(PANEL_SETTLE, DeviceEvent::PanelSettled),
            Effect::AwaitDrive(since) => self.after(DRIVE_SETTLE, DeviceEvent::DriveSettled(since)),
            Effect::Suspend => self.device_later(|host| match host.suspend() {
                Ok(()) => DeviceEvent::Resumed,
                Err(error) => {
                    eprintln!("kindil: suspend: {error}");
                    DeviceEvent::SuspendFailed
                }
            }),
            Effect::Nap => {
                // On the minute, plus a second so the clock has turned.
                let nap = until_next_minute((self.now)()) + Duration::from_secs(1);
                if nap < MIN_NAP {
                    self.after(nap, DeviceEvent::Resumed);
                } else {
                    self.device_later(move |host| match host.nap(nap) {
                        Ok(()) => DeviceEvent::Resumed,
                        Err(error) => {
                            eprintln!("kindil: nap: {error}");
                            DeviceEvent::SuspendFailed
                        }
                    });
                }
            }
            Effect::Redraw => {
                let now = (self.now)();
                let window = {
                    let mut state = self.state.borrow_mut();
                    let window = Window::after_nap(state.online, now);
                    if window == Window::Online {
                        state.online = Some(now);
                    }
                    window
                };
                self.catch_up(Tick::Draw);
                if window == Window::Online {
                    self.measure_doze(now);
                    self.reconnect_wifi();
                    let ctx = self.clone();
                    self.later(FETCH_AFTER, move || ctx.tick_miqat(Tick::Fetch));
                }
                self.after(window.length(), DeviceEvent::PanelSettled);
            }
            Effect::Rouse => {
                // Awake time would count against the doze.
                *lock(&self.doze_gauge) = None;
                self.catch_up(Tick::Fetch);
                self.state.borrow_mut().idle = Idle::new(self.touches.count(), Instant::now());
            }
            Effect::Wake => {
                self.show_system(&ui, System::App);
                self.catch_up(Tick::Fetch);
                self.state.borrow_mut().idle = Idle::new(self.touches.count(), Instant::now());
            }
            Effect::RestoreLight(level) => self.background(
                move |host| host.set_brightness(level),
                |ui, result| {
                    if let Err(error) = result {
                        notify(ui, &format!("Front light: {error}"));
                    }
                },
            ),
            Effect::Reconnect => self.reconnect_wifi(),
            Effect::ReleaseStorage => self.release_storage(),
            Effect::StorageBack => self.storage_back(),
        }
    }

    /// Delivers `event` after `delay`; previews and tests deliver it on
    /// [`App::fire_timers`].
    fn after(&self, delay: Duration, event: DeviceEvent) {
        match self.execution {
            Execution::Inline => self.timers.borrow_mut().push(event),
            Execution::Background => {
                let ctx = self.clone();
                slint::Timer::single_shot(delay, move || ctx.device(event));
            }
        }
    }

    /// The clock, battery and dashboard moved on while the device slept.
    fn catch_up(&self, tick: Tick) {
        self.tick_clock();
        self.refresh_status();
        self.tick_miqat(tick);
    }

    fn tick_miqat(&self, tick: Tick) {
        if let Worker::Started(ticks) = &self.state.borrow().miqat {
            // A stopped worker has reported its error on screen already.
            let _ = ticks.send(tick);
        }
    }

    /// Runs `work` on the UI thread after `delay`; never under
    /// [`Execution::Inline`], which has no timers to run it.
    fn later(&self, delay: Duration, work: impl FnOnce() + 'static) {
        match self.execution {
            Execution::Inline => {}
            Execution::Background => slint::Timer::single_shot(delay, work),
        }
    }

    fn check_idle(&self) {
        let now = Instant::now();
        let due = {
            let mut state = self.state.borrow_mut();
            state.idle = state.idle.observe(self.touches.count(), now);
            state.idle.due(state.store.prefs.auto_sleep, now)
        };
        if let Some(event) = due {
            self.device(event);
        }
    }

    fn choose_auto_sleep(&self, index: i32) {
        let Some(choice) = usize::try_from(index)
            .ok()
            .and_then(|i| AutoSleep::CHOICES.get(i).copied())
        else {
            return;
        };
        self.save(|prefs| prefs.auto_sleep = choice);
        if let Some(ui) = self.ui() {
            ui.global::<Kindil>().set_auto_sleep(sleep_choice(choice));
        }
    }

    /// A different screen, so flash the panel clean: this also clears whatever
    /// Amazon's screen modules drew.
    fn show_system(&self, ui: &AppWindow, system: System) {
        ui.global::<Kindil>().set_system(system);
        self.display.updated(ui, Update::Navigate);
    }

    /// Stops Syncthing, which keeps files open on the user storage, and saves
    /// choices while the storage is still there. The Syncthing preference stays.
    fn release_storage(&self) {
        {
            let mut state = self.state.borrow_mut();
            let sync = std::mem::replace(&mut state.sync, Service::NotInstalled);
            state.sync = match sync {
                Service::Running(running) => Service::Stopped(running.stop()),
                other => other,
            };
            let _ = state.store.save();
        }
        self.show_sync();
    }

    /// The computer may have added, renamed or removed books.
    fn storage_back(&self) {
        self.reload_library();
        let resume_sync = {
            let state = self.state.borrow();
            state.store.prefs.sync == Power::On && matches!(state.sync, Service::Stopped(_))
        };
        if resume_sync {
            self.toggle_sync();
        }
    }

    fn restart(&self) {
        self.release_storage();
        self.background(
            |host| host.restart(),
            |ui, result| {
                if let Err(error) = result {
                    notify(ui, &format!("Restart: {error}"));
                }
            },
        );
    }

    fn power_off(&self) {
        let Some(ui) = self.ui() else { return };
        self.release_storage();
        // E-ink keeps this screen with the power off.
        self.show_system(&ui, System::Off);
        let display = self.display.clone();
        self.background(
            |host| host.power_off(),
            move |ui, result| {
                if let Err(error) = result {
                    ui.global::<Kindil>().set_system(System::App);
                    display.updated(ui, Update::Navigate);
                    notify(ui, &format!("Power off: {error}"));
                }
            },
        );
    }

    // ── updates ──

    /// The Updates button: check, or install what a check found.
    fn update(&self) {
        let Some(ui) = self.ui() else { return };
        let updates = lock(&self.updates).clone();
        match updates {
            Updates::Idle | Updates::UpToDate | Updates::Failed(_) => self.check_update(&ui),
            Updates::Available(release) => self.install_update(&ui, release),
            Updates::Checking | Updates::Installing(_) => {}
        }
    }

    fn check_update(&self, ui: &AppWindow) {
        show_updates(ui, &self.updates, Updates::Checking);
        let updates = self.updates.clone();
        self.background(
            |host| host.check_update(),
            move |ui, result| {
                let next = match result {
                    Ok(Check::UpToDate) => Updates::UpToDate,
                    Ok(Check::Available(release)) => Updates::Available(release),
                    Err(error) => Updates::Failed(format!("{error:#}")),
                };
                show_updates(ui, &updates, next);
            },
        );
    }

    fn install_update(&self, ui: &AppWindow, release: Release) {
        show_updates(ui, &self.updates, Updates::Installing(release.version));
        let (updates, exit) = (self.updates.clone(), self.exit.clone());
        self.background(
            move |host| host.stage_update(&release),
            move |ui, result| match result {
                Ok(()) => {
                    *lock(&exit) = Exit::Restart;
                    // Errors only without an event loop (previews).
                    let _ = slint::quit_event_loop();
                }
                Err(error) => show_updates(ui, &updates, Updates::Failed(format!("{error:#}"))),
            },
        );
    }

    /// A version on trial that has run for [`update::SETTLE`] is kept. Retried
    /// while the user partition is away (USB drive mode).
    fn commit_update(&self) {
        self.background(
            |host| loop {
                match host.commit_update() {
                    Ok(commit) => break commit,
                    Err(error) => {
                        eprintln!("kindil: keeping the update failed, retrying: {error:#}");
                        std::thread::sleep(update::SETTLE);
                    }
                }
            },
            |_, commit| match commit {
                update::Commit::Nothing => {}
                update::Commit::Committed => eprintln!("kindil: update kept"),
            },
        );
    }

    fn exit_app(&self) {
        // Errors only when there is no event loop (previews): nothing to quit.
        let _ = slint::quit_event_loop();
    }

    // ── dashboard ──

    fn start_miqat(&self) {
        match (&self.state.borrow().miqat, self.execution) {
            (Worker::Started(_), _) => return,
            // Previews set a deterministic frame themselves; no network.
            (Worker::Idle, Execution::Inline) => return,
            (Worker::Idle, Execution::Background) => {}
        }
        let (sender, ticks) = mpsc::channel();
        self.state.borrow_mut().miqat = Worker::Started(sender);
        let path = self.config.miqat_config.clone();
        let weak = self.weak.clone();
        let display = self.display.clone();
        std::thread::spawn(move || miqat_worker(&path, &weak, &display, &ticks));
    }
}

/// Owns Miqat's cached weather and radar sources; renders on each minute.
fn miqat_worker(
    path: &std::path::Path,
    weak: &slint::Weak<AppWindow>,
    display: &Display,
    ticks: &mpsc::Receiver<Tick>,
) {
    let config = match miqat::config::Config::load(path) {
        Ok(config) => config,
        Err(error) => {
            let message = format!("Prayer dashboard needs {}: {error}", path.display());
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.global::<Kindil>().set_miqat_error(message.into())
            });
            return;
        }
    };
    let renderer = miqat::render::Renderer::new(config.display.contrast);
    let mut sources = miqat::sources::Sources::new(&config);
    let show = |sources: &miqat::sources::Sources| {
        let frame = crate::dashboards::render(&renderer, &config, Utc::now(), sources)
            .map(pixels)
            .map_err(|e| e.to_string());
        let display = display.clone();
        weak.upgrade_in_event_loop(move |ui| {
            let k = ui.global::<Kindil>();
            match frame {
                Ok(buffer) => {
                    k.set_miqat_frame(slint::Image::from_rgb8(buffer));
                    display.updated(&ui, Update::DashboardFrame);
                }
                Err(error) => k.set_miqat_error(error.into()),
            }
        })
    };
    // First paint before any network, so a slow fetch never leaves it blank.
    if show(&sources).is_err() {
        return;
    }
    let mut tick = Tick::Fetch;
    loop {
        match tick {
            Tick::Fetch => sources.refresh(&config, Utc::now()),
            Tick::Draw => {}
        }
        if show(&sources).is_err() {
            return;
        }
        tick = match ticks.recv_timeout(until_next_minute(Utc::now())) {
            Ok(tick) => tick,
            Err(mpsc::RecvTimeoutError::Timeout) => Tick::Fetch,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clock_ticks_on_the_minute() {
        let now = DateTime::from_timestamp(1_794_398_400 + 45, 250_000_000).unwrap();
        assert_eq!(until_next_minute(now), Duration::from_millis(14_800));
    }
}
