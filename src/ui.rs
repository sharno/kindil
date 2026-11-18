//! Wires the Slint window to the device. Callbacks are one-liners into
//! [`Ctx`]; slow device calls run on worker threads and report back through
//! the event loop, so a timeout never freezes a tap.
use crate::battery::{Battery, Charge};
use crate::config::{Config, Dashboard};
use crate::dashboards::{Board, Draw};
use crate::drain::{Drain, Gauge};
use crate::files::{FileName, Files, Trashed};
use crate::ghosting::{Ghosting, Refresh, Update};
use crate::host::{BootStart, Brightness, DeviceInfo, Host, OwnSsh, Power, Ssh, SshKey, Wifi};
use crate::location::{self, Place};
use crate::maintenance::{Hold, Maintenance, HOLD_CHECK};
use crate::prayer::{self, Setting};
use crate::prefs::Store;
use crate::reader::{self, Book, Layout, Reader};
use crate::sync::Service;
use crate::system::{
    AutoSleep, Doze, DozeLedger, DozeWake, Effect, Event as DeviceEvent, Idle, Mode, Redraw,
    Schedule, Settle, Step, Touches, Window, DRIVE_SETTLE, MIN_NAP, PANEL_SETTLE, PANEL_UPDATE,
};
use crate::update::{self, Check, Release, Updates, Version};
use crate::view;
use crate::wifi::{Credentials, Ssid};
use chrono::{DateTime, Timelike, Utc};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
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

/// What the panel reports about frames reaching the glass; the Kindle's
/// backend has it, previews and tests don't.
#[derive(Clone)]
pub struct Presented {
    /// Frames drawn, each counted once sent to the panel.
    pub frames: Arc<AtomicU64>,
    /// Blocks until the panel has finished its last update; an error if it
    /// can't tell.
    pub wait_idle: fn() -> std::io::Result<()>,
}

pub struct Options {
    pub config: Config,
    pub host: Arc<dyn Host>,
    pub panel: Panel,
    /// With it, a doze wake naps as soon as its redraw is on the panel.
    pub presented: Option<Presented>,
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
    presented: Option<Presented>,
    dashboard: Arc<DashboardFrames>,
    /// Set while dozing: the dashboard worker's own minute clock then draws
    /// and fetches nothing; doze wakes ask for what is due.
    dozing: Arc<AtomicBool>,
    /// Whether the panel's inability to say an update finished was logged.
    panel_error_logged: Arc<AtomicBool>,
    /// Where this doze's time went; shared with the threads that end wakes
    /// and the dashboard worker, which says when a tick drew in full.
    ledger: Arc<Mutex<DozeLedger>>,
}

/// How a wait for a doze wake's redraw ended.
#[derive(Debug)]
enum Drawn {
    /// On the panel, and the panel finished it.
    Done,
    /// Sent to the panel, which can't say when it finishes.
    Sent(std::io::Error),
    /// Not on the panel by the deadline.
    NotSeen,
}

/// The last dashboard frame handed to the window: the doze wake it was drawn
/// for, if any, and the panel frame count then (the panel frame after that
/// has it).
#[derive(Default)]
struct DashboardFrames {
    shown: Mutex<Shown>,
    changed: Condvar,
}

#[derive(Clone, Copy, Default)]
struct Shown {
    wake: Option<u64>,
    frames: u64,
}

/// The doze wake whose redraw is awaited. Its end is claimed once, by the
/// redraw reaching the panel or by the ceiling timer, whichever is first; a
/// claim for an older wake does nothing.
#[derive(Default)]
struct DrawWake {
    current: Mutex<(u64, bool)>,
}

impl DrawWake {
    fn begin(&self) -> u64 {
        let mut current = lock(&self.current);
        *current = (current.0 + 1, false);
        current.0
    }

    fn claim(&self, wake: u64) -> bool {
        let mut current = lock(&self.current);
        let won = current.0 == wake && !current.1;
        current.1 |= won;
        won
    }
}

impl Display {
    fn frames(&self) -> u64 {
        self.presented
            .as_ref()
            .map_or(0, |p| p.frames.load(Ordering::Acquire))
    }

    /// A dashboard frame (or its error) for `wake` was just handed to the
    /// window, on the UI thread, before the loop draws it.
    fn dashboard_shown(&self, wake: Option<u64>) {
        let frames = self.frames();
        *lock(&self.dashboard.shown) = Shown { wake, frames };
        self.dashboard.changed.notify_all();
    }

    /// Blocks until the dashboard frame drawn for `wake` is on the panel and
    /// the panel has finished it (`wake` None: any panel frame after
    /// `drawn`); [`Drawn::Sent`] if the panel can't say it finished,
    /// [`Drawn::NotSeen`] at `deadline`.
    fn wait_drawn(&self, wake: Option<u64>, drawn: u64, deadline: Instant) -> Drawn {
        let Some(presented) = &self.presented else {
            return Drawn::NotSeen;
        };
        let left = || deadline.saturating_duration_since(Instant::now());
        let drawn = match wake {
            None => drawn,
            Some(wake) => {
                let (shown, timeout) = self
                    .dashboard
                    .changed
                    .wait_timeout_while(lock(&self.dashboard.shown), left(), |s| {
                        s.wake.is_none_or(|shown| shown < wake)
                    })
                    .unwrap_or_else(|e| e.into_inner());
                if timeout.timed_out() {
                    return Drawn::NotSeen;
                }
                shown.frames
            }
        };
        while presented.frames.load(Ordering::Acquire) <= drawn {
            if left().is_zero() {
                return Drawn::NotSeen;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        match (presented.wait_idle)() {
            Ok(()) => Drawn::Done,
            Err(error) => Drawn::Sent(error),
        }
    }

    fn is_dozing(&self) -> bool {
        self.dozing.load(Ordering::Relaxed)
    }

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
    /// A doze wake's draw carries the wake, which its frame reports back.
    Draw(Option<u64>),
    /// Like [`Tick::Draw`], but only the clock and countdown, unless the day
    /// or the next prayer has changed since the last full frame.
    Minute(Option<u64>),
    /// Fetch what is due, then render; an online doze wake's carries the wake.
    Fetch(Option<u64>),
    /// Read Miqat's file again, as a fresh start would, then render.
    Reload,
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
    /// When dozing last went online and redrew in full.
    schedule: Schedule,
    /// This doze's maintenance windows and SSH sessions.
    maintenance: Maintenance,
    /// Why the last nap was held off, if it was.
    held: Option<Hold>,
    /// Counts naps, so a hold's timer knows its nap is still the one under way.
    naps: u64,
    /// A nap parked Wi-Fi, and nothing has brought it back since.
    wifi_parked: bool,
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
    /// The doze wake whose redraw is awaited.
    draw_wake: Arc<DrawWake>,
    /// Where this doze's time went; shared with the threads that end wakes.
    ledger: Arc<Mutex<DozeLedger>>,
    /// Steps of the clock from network time not yet applied to the doze's
    /// schedule ([`Ctx::apply_clock_steps`]); the workers that sync add them.
    clock_steps: Arc<Mutex<chrono::TimeDelta>>,
    now: fn() -> DateTime<Utc>,
    /// The clock's and status bar's zone: kindil.toml's, until Settings →
    /// Location changes it.
    timezone: Arc<Mutex<chrono_tz::Tz>>,
    /// What Settings → Location found, shared with its workers.
    finding: Arc<Mutex<Finding>>,
}

/// Settings → Location's search results, and the place waiting for Save.
#[derive(Default)]
struct Finding {
    places: Vec<Place>,
    pending: Option<Place>,
}

/// Shown under a place found from the internet address.
pub const IP_NOTE: &str =
    "Found from your internet address; it can be off by tens of kilometres, or wrong behind a VPN.";

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
    own_ssh: OwnSsh,
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
            own_ssh: host.own_ssh(),
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
        k.set_own_ssh_detail(view::own_ssh(&self.own_ssh, view::ip(&self.wifi)).into());
        let keys = |keys: &[SshKey]| model(keys.iter().map(view::ssh_key).collect());
        k.set_ssh_trusted(keys(&self.own_ssh.trusted));
        k.set_ssh_new(keys(&self.own_ssh.new_on_usb));
        let (boot, detail) = view::boot_start(self.boot_start);
        k.set_boot(boot);
        k.set_boot_start_detail(detail.into());
        k.set_facts(model(view::facts(&self.info)));
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
        let timezone = options.config.timezone;
        let ledger = Arc::new(Mutex::new(DozeLedger::new((options.now)(), None)));
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
                schedule: Schedule::default(),
                maintenance: Maintenance::new((options.now)()),
                held: None,
                naps: 0,
                wifi_parked: false,
            })),
            display: Display {
                panel: options.panel,
                ghosting: Arc::new(Mutex::new(Ghosting::default())),
                presented: options.presented,
                dashboard: Arc::default(),
                dozing: Arc::default(),
                panel_error_logged: Arc::default(),
                ledger: ledger.clone(),
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
            draw_wake: Arc::default(),
            ledger,
            clock_steps: Arc::new(Mutex::new(chrono::TimeDelta::zero())),
            timezone: Arc::new(Mutex::new(timezone)),
            finding: Arc::default(),
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

    /// A whole dashboard page.
    pub fn set_miqat(&self, gray: miqat::image::Gray) {
        let split = crate::dashboards::Split::of(gray);
        let k = self.window.global::<Kindil>();
        k.set_miqat_header(slint::Image::from_rgb8(pixels(split.header)));
        k.set_miqat_body(slint::Image::from_rgb8(pixels(split.body)));
    }

    /// Only the dashboard's header, over the body on screen, as a minute
    /// tick draws it.
    pub fn set_miqat_header(&self, gray: miqat::image::Gray) {
        let k = self.window.global::<Kindil>();
        k.set_miqat_header(slint::Image::from_rgb8(pixels(gray)));
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

fn show_prayer(ui: &AppWindow, page: Result<prayer::Page, String>) {
    let k = ui.global::<Kindil>();
    let choices = |choices: Vec<prayer::Choice>| {
        model(
            choices
                .into_iter()
                .map(|c| PrayerChoice {
                    title: c.label.into(),
                    times: c.times.into(),
                })
                .collect(),
        )
    };
    let index = |i: usize| i32::try_from(i).unwrap_or(-1);
    match page {
        Ok(page) => {
            k.set_prayer_error("".into());
            k.set_prayer_summary(page.summary.into());
            k.set_prayer_method(index(page.method));
            k.set_prayer_methods(choices(page.methods));
            k.set_asr_school(index(page.school));
            k.set_asr_schools(choices(page.schools));
        }
        Err(error) => {
            k.set_prayer_error(error.into());
            k.set_prayer_methods(model(Vec::new()));
            k.set_asr_schools(model(Vec::new()));
        }
    }
}

fn show_location(ui: &AppWindow, place: Result<Place, String>) {
    let k = ui.global::<Kindil>();
    match place {
        Ok(place) => {
            k.set_location_error("".into());
            k.set_location_title(place.title().into());
            k.set_location_zone(place.timezone.name().into());
        }
        Err(error) => k.set_location_error(error.into()),
    }
}

/// Opens the sheet asking to save `place`.
fn confirm_location(ui: &AppWindow, finding: &Mutex<Finding>, place: Place, note: &str) {
    let k = ui.global::<Kindil>();
    k.set_confirm_place(place.summary().into());
    k.set_confirm_note(note.into());
    k.set_confirm_open(true);
    lock(finding).pending = Some(place);
}

/// The clock and date in `timezone`.
fn show_clock(ui: &AppWindow, now: DateTime<Utc>, timezone: chrono_tz::Tz) {
    let (clock, date) = view::clock(&now.with_timezone(&timezone));
    let k = ui.global::<Kindil>();
    k.set_clock(clock.into());
    k.set_date(date.into());
}

fn show_updates(ui: &AppWindow, state: &Mutex<Updates>, updates: Updates) {
    let (status, button, enabled) = view::updates(&updates, Version::current());
    let k = ui.global::<Kindil>();
    k.set_update_status(status.into());
    k.set_update_button(button.into());
    k.set_update_enabled(enabled);
    k.set_update_checked(!matches!(updates, Updates::Idle));
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
    on!(on_trust_ssh_keys => trust_ssh_keys());
    on!(on_ignore_ssh_keys => ignore_ssh_keys());
    on!(on_choose_boot => choose_boot(choice));
    on!(on_toggle_sync => toggle_sync());
    on!(on_exit_app => exit_app());
    on!(on_device_events => device_events());
    on!(on_restart => restart());
    on!(on_power_off => power_off());
    on!(on_choose_auto_sleep => choose_auto_sleep(index));
    on!(on_update => update());
    on!(on_choose_prayer_method => choose_prayer_method(index));
    on!(on_choose_asr_school => choose_asr_school(index));
    on!(on_search_places => search_places(name));
    on!(on_choose_place => choose_place(index));
    on!(on_locate_by_ip => locate_by_ip());
    on!(on_enter_coordinates => enter_coordinates(latitude, longitude, zone));
    on!(on_filter_zones => filter_zones(typed));
    on!(on_save_location => save_location());
    on!(on_cancel_location => cancel_location());
}

fn arm_clock(timer: Rc<slint::Timer>, ctx: Ctx) {
    let next = timer.clone();
    timer.start(
        slint::TimerMode::SingleShot,
        until_next_minute((ctx.now)()),
        move || {
            // While dozing, each wake reads what it needs; this clock only
            // runs on the little time the device is up.
            if !ctx.display.is_dozing() {
                ctx.tick_clock();
                ctx.refresh_status();
                ctx.check_idle();
            }
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
        show_clock(&ui, (self.now)(), *lock(&self.timezone));
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
            let k = ui.global::<Kindil>();
            // A question about a place belongs to the page it was asked on.
            k.set_confirm_open(false);
            if !k.get_location_busy() {
                k.set_location_status("".into());
            }
        }
        match page {
            Page::Library => self.reload_library(),
            Page::Settings => {
                self.refresh_status();
                self.show_sync();
                self.show_prayer();
                self.show_location();
            }
            Page::Prayer => self.show_prayer(),
            Page::Location | Page::PlaceSearch => self.show_location(),
            Page::Coordinates => self.start_coordinates(),
            Page::Display
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
            // Never chosen in kindil: adopt the level the light has (Amazon's,
            // on a first run), so every later start sets it without powerd.
            return self.background(
                |host| host.brightness(),
                |ui, level| match level {
                    // Dark at boot isn't a choice: leave it to the next start.
                    Ok(level) if level.percent() > 0 => {
                        let percent = i32::from(level.percent());
                        let k = ui.global::<Kindil>();
                        k.set_brightness(percent);
                        k.invoke_choose_brightness(percent);
                    }
                    Ok(_) | Err(_) => {}
                },
            );
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
        // Loading the driver and joining take a few seconds after switching on.
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

    /// The fingerprints of the new keys on screen: only those are trusted
    /// or ignored, whatever the USB storage holds by the time of the tap.
    fn shown_new_keys(&self) -> Vec<String> {
        let Some(ui) = self.ui() else {
            return Vec::new();
        };
        slint::Model::iter(&ui.global::<Kindil>().get_ssh_new())
            .map(|key| key.fingerprint.to_string())
            .collect()
    }

    fn trust_ssh_keys(&self) {
        let shown = self.shown_new_keys();
        self.background(
            move |host| {
                host.trust_ssh_keys(&shown)
                    .map(|()| (shown.len(), Snapshot::read(host)))
            },
            |ui, result| match result {
                Ok((count, snapshot)) => {
                    snapshot.apply(ui);
                    notify(
                        ui,
                        &match count {
                            1 => "Trusted 1 SSH key".to_string(),
                            n => format!("Trusted {n} SSH keys"),
                        },
                    );
                }
                Err(error) => notify(ui, &format!("SSH keys: {error}")),
            },
        );
    }

    fn ignore_ssh_keys(&self) {
        let shown = self.shown_new_keys();
        self.background(
            move |host| host.ignore_ssh_keys(&shown).map(|()| Snapshot::read(host)),
            |ui, result| match result {
                Ok(snapshot) => snapshot.apply(ui),
                Err(error) => notify(ui, &format!("SSH keys: {error}")),
            },
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
            Err(ignored) => crate::log!("kindil: {event:?} ignored: {ignored:?}"),
            Ok(Step { next, effects }) => {
                self.state.borrow_mut().system = next;
                self.dozing_changed(
                    matches!(mode, Mode::Dozing(_)),
                    matches!(next, Mode::Dozing(_)),
                );
                effects.into_iter().for_each(|effect| self.apply(effect));
            }
        }
    }

    /// A doze starts a ledger of its own, and logs it when it ends.
    fn dozing_changed(&self, was: bool, is: bool) {
        self.display.dozing.store(is, Ordering::Relaxed);
        let now = (self.now)();
        match (was, is) {
            (false, true) => {
                {
                    let mut state = self.state.borrow_mut();
                    // The first wake draws in full.
                    state.schedule.doze_began();
                    // The minutes awake before dozing were a maintenance window.
                    state.maintenance = Maintenance::new(now);
                    state.held = None;
                    state.wifi_parked = false;
                }
                let mut ledger = DozeLedger::new(now, None);
                // Wi-Fi is up until the first nap parks it.
                ledger.wifi_on(now);
                ledger.dhcp(crate::dhcpc::counts());
                *lock(&self.ledger) = ledger;
                let ledger = self.ledger.clone();
                self.background(
                    |host| host.info().charge,
                    move |_, charge| lock(&ledger).set_gauge(charge),
                );
            }
            (true, false) => self.log_ledger(now),
            (false, false) | (true, true) => {}
        }
    }

    /// Logs the doze ledger with the gauge now, and starts the next one.
    fn log_ledger(&self, now: DateTime<Utc>) {
        let ledger = self.ledger.clone();
        self.background(
            |host| host.info().charge,
            move |_, charge| {
                let mut ledger = lock(&ledger);
                ledger.dhcp(crate::dhcpc::counts());
                crate::log!("kindil: {}", ledger.close(now, charge));
            },
        );
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
                    crate::log!(
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

    /// Brings Wi-Fi back after a suspend or a parked doze, then the clock
    /// from network time; `then` runs on the UI thread with the outcome.
    fn reconnect_wifi(&self, then: impl FnOnce(Result<(), ()>) + Send + 'static) {
        self.state.borrow_mut().wifi_parked = false;
        self.online(true, then);
    }

    /// Online, after bringing Wi-Fi back if `reconnect`: first the clock
    /// from network time, so the fetch and its frame have the right time;
    /// `then` runs on the UI thread with whether Wi-Fi is up.
    fn online(&self, reconnect: bool, then: impl FnOnce(Result<(), ()>) + Send + 'static) {
        let ledger = self.ledger.clone();
        let steps = self.clock_steps.clone();
        self.background(
            move |host| {
                let since = Instant::now();
                let up = match reconnect {
                    // Returns once the lease is applied: the sync finds
                    // the route and the name servers.
                    true => host.reconnect_wifi(),
                    false => Ok(()),
                };
                let rejoined = since.elapsed();
                if up.is_ok() {
                    match host.sync_clock() {
                        Ok(Some(correction)) => {
                            crate::log!("kindil: {correction}");
                            lock(&ledger).clock_corrected(correction);
                            *lock(&steps) += correction.delta();
                        }
                        Ok(None) => {}
                        Err(error) => crate::log!("kindil: clock: {error:#}"),
                    }
                }
                crate::log!(
                    "kindil: online: Wi-Fi {} in {:.1} s, clock {:.1} s more",
                    match (reconnect, &up) {
                        (false, _) => "was up",
                        (true, Ok(())) => "back",
                        (true, Err(_)) => "not back",
                    },
                    rejoined.as_secs_f64(),
                    (since.elapsed() - rejoined).as_secs_f64()
                );
                up
            },
            move |_, result| {
                then(result.map_err(|error| crate::log!("kindil: reconnect Wi-Fi: {error}")));
            },
        );
    }

    /// Network time stepped the clock: the doze's schedule, noted on the
    /// old clock, moves with it (the ledger did as it heard).
    fn apply_clock_steps(&self) {
        let by = std::mem::replace(&mut *lock(&self.clock_steps), chrono::TimeDelta::zero());
        if by != chrono::TimeDelta::zero() {
            let mut state = self.state.borrow_mut();
            state.schedule.clock_stepped(by);
            state.maintenance.clock_stepped(by);
        }
    }

    /// Back from a nap: draw what is due (most minutes only the clock and
    /// countdown), go online if a fetch is due, and nap again as soon as the
    /// panel has the frame.
    fn redraw_doze(&self) {
        self.apply_clock_steps();
        let now = (self.now)();
        let dashboard = matches!(self.state.borrow().miqat, Worker::Started(_));
        // Every half hour of wall time: the battery's draw, and the weather.
        let (half_hour, window, redraw, opens, parked) = {
            let mut state = self.state.borrow_mut();
            let DozeWake {
                half_hour,
                window,
                redraw,
            } = state.schedule.wake(now, dashboard);
            // Every two hours the fetch's radio stays up a while.
            let opens = window == Window::Online && state.maintenance.online_wake(now);
            (half_hour, window, redraw, opens, state.wifi_parked)
        };
        // Before the redraw starts: this wake, and the panel frames drawn so
        // far; its dashboard frame is tagged with the wake.
        let wake = self.draw_wake.begin();
        let drawn = self.display.frames();
        let due = {
            let mut ledger = lock(&self.ledger);
            ledger.woke(now, Instant::now(), window, redraw);
            if opens {
                ledger.window_opened();
            }
            ledger.due(now)
        };
        if due {
            self.log_ledger(now);
        }
        self.tick_clock();
        // A tick reads nothing: the battery on screen is at most a few
        // minutes old.
        if redraw == Redraw::Full {
            self.refresh_battery();
        }
        if half_hour {
            self.measure_doze(now);
        }
        match (window, redraw) {
            (Window::Draw, Redraw::Full) => self.tick_miqat(Tick::Draw(Some(wake))),
            (Window::Draw, Redraw::Tick) => self.tick_miqat(Tick::Minute(Some(wake))),
            (Window::Online, _) => {
                // The minute now; the fetched frame, in full, once Wi-Fi is back.
                self.tick_miqat(Tick::Minute(None));
                lock(&self.ledger).wifi_on(now);
                let ticks = self.miqat_ticks();
                let fetch = move |result: Result<(), ()>| {
                    // Without Wi-Fi a fetch only fails: draw, and nap.
                    if let Some(ticks) = ticks {
                        let _ = ticks.send(match result {
                            Ok(()) => Tick::Fetch(Some(wake)),
                            Err(()) => Tick::Draw(Some(wake)),
                        });
                    }
                };
                // Held awake since the last fetch, Wi-Fi never left: joining
                // again would only drop an SSH session.
                if parked {
                    self.reconnect_wifi(fetch);
                } else {
                    self.online(false, fetch);
                }
            }
        }
        self.settle_wake(wake, (now, window, redraw), dashboard, drawn);
    }

    /// The battery after a doze wake: only the gauge, not the whole status.
    fn refresh_battery(&self) {
        let deliver = self.deliver();
        let ledger = self.ledger.clone();
        self.background(
            |host| host.info(),
            move |ui, info| {
                let k = ui.global::<Kindil>();
                let (level, label) = view::battery(info.battery);
                k.set_battery_level(level);
                k.set_battery_label(label.into());
                let empty = crate::system::battery_event(info.battery);
                k.set_battery_empty(empty.is_some());
                if let Some(current) = info.current {
                    lock(&ledger).current_at_wake(current);
                }
                empty.into_iter().for_each(&deliver);
            },
        );
    }

    fn apply(&self, effect: Effect) {
        let Some(ui) = self.ui() else { return };
        match effect {
            // Out of doze, if in it: its wake ends, and its timer with it.
            Effect::ShowSleep => {
                self.draw_wake.begin();
                self.show_system(&ui, System::Asleep);
            }
            Effect::ShowUsbDrive => {
                self.draw_wake.begin();
                self.show_system(&ui, System::UsbDrive);
            }
            Effect::LightOff => self.device_later(|host| {
                let level = host.brightness().ok();
                // A light that won't switch off doesn't stop the device sleeping.
                let _ = host.set_brightness(Brightness::new(0));
                DeviceEvent::LightOff(level)
            }),
            Effect::AwaitPanel => self.after(PANEL_SETTLE, DeviceEvent::PanelSettled),
            Effect::AwaitDrive(since) => self.after(DRIVE_SETTLE, DeviceEvent::DriveSettled(since)),
            // Parked first: wpa_supplicant would join again after the resume.
            Effect::Suspend => self.device_later(|host| {
                host.park_wifi();
                match host.suspend() {
                    Ok(()) => DeviceEvent::Resumed,
                    Err(error) => {
                        crate::log!("kindil: suspend: {error}");
                        DeviceEvent::SuspendFailed
                    }
                }
            }),
            Effect::Nap => {
                let now = (self.now)();
                // On the redraw's minute, plus a second so the clock has turned.
                let until = crate::system::next_redraw(now) + chrono::TimeDelta::seconds(1);
                let nap = {
                    let mut state = self.state.borrow_mut();
                    state.naps += 1;
                    state.naps
                };
                self.nap_or_hold(now, until, nap);
            }
            Effect::Redraw => self.redraw_doze(),
            Effect::Rouse => {
                // Out of doze: its wake ends, and its timer with it.
                self.draw_wake.begin();
                // Awake time would count against the doze.
                *lock(&self.doze_gauge) = None;
                // The fetch follows Wi-Fi's reconnect (or the dashboard's
                // own minute clock, if Wi-Fi never went down).
                self.catch_up(Tick::Draw(None));
                self.state.borrow_mut().idle = Idle::new(self.touches.count(), Instant::now());
            }
            Effect::Wake => {
                self.draw_wake.begin();
                self.show_system(&ui, System::App);
                self.catch_up(Tick::Draw(None));
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
            Effect::Reconnect => {
                let ticks = self.miqat_ticks();
                self.reconnect_wifi(move |result| {
                    if let (Ok(()), Some(ticks)) = (result, ticks) {
                        let _ = ticks.send(Tick::Fetch(None));
                    }
                });
            }
            Effect::ReleaseStorage => self.release_storage(),
            Effect::StorageBack => self.storage_back(),
            Effect::DriveNotice(notice) => {
                if let Some(text) = view::drive_notice(notice) {
                    crate::log!("kindil: {text}");
                    notify(&ui, &text);
                }
            }
        }
    }

    /// Ends a doze wake as soon as its redraw is on the panel (the
    /// dashboard's frame for this wake, if it has one): when the panel says
    /// it finished, or [`PANEL_UPDATE`] after sending when it can't say; at
    /// the window's ceiling if the frame never shows.
    /// `doing` is the wake's time on the clock, and what it does; the end
    /// of each wake is noted for the flight recorder ([`Host::note`]): what
    /// it drew, for which minute, and how its redraw ended.
    fn settle_wake(
        &self,
        wake: u64,
        doing: (DateTime<Utc>, Window, Redraw),
        dashboard: bool,
        drawn: u64,
    ) {
        let (at, window, redraw) = doing;
        let ceiling = window.ceiling();
        let Execution::Background = self.execution else {
            return self.after(ceiling, DeviceEvent::PanelSettled);
        };
        let dashboard = dashboard.then_some(wake);
        let since = Instant::now();
        let claim = {
            let draw_wake = self.draw_wake.clone();
            let ledger = self.ledger.clone();
            let deliver = Arc::new(self.deliver());
            let host = self.host.clone();
            let display = self.display.clone();
            move |settle: Settle| {
                let won = draw_wake.claim(wake);
                if won {
                    host.note(&format!(
                        "wake {wake}: {window:?} {redraw:?} for {}, {settle:?} after {:.1} s, \
                         panel frame {}",
                        crate::logging::stamp(at, crate::logging::zone()),
                        since.elapsed().as_secs_f64(),
                        display.frames()
                    ));
                    if window == Window::Online {
                        crate::log!(
                            "kindil: online wake ended after {:.1} s ({settle:?})",
                            since.elapsed().as_secs_f64()
                        );
                    }
                    lock(&ledger).settled(settle);
                    deliver(DeviceEvent::PanelSettled);
                }
                won
            }
        };
        if self.display.presented.is_some() {
            let display = self.display.clone();
            let claim = claim.clone();
            let deadline = since + ceiling;
            std::thread::spawn(
                move || match display.wait_drawn(dashboard, drawn, deadline) {
                    Drawn::Done => {
                        claim(Settle::Done);
                    }
                    Drawn::Sent(error) => {
                        if !display.panel_error_logged.swap(true, Ordering::Relaxed) {
                            crate::log!(
                                "kindil: doze redraw: the panel can't say when an update finished \
                             ({error}); napping {PANEL_UPDATE:?} after sending instead"
                            );
                        }
                        std::thread::sleep(PANEL_UPDATE);
                        claim(Settle::Timed);
                    }
                    Drawn::NotSeen => {}
                },
            );
        }
        slint::Timer::single_shot(ceiling, move || {
            if claim(Settle::NotSeen) {
                crate::log!(
                    "kindil: doze redraw not seen on the panel; napping at the {ceiling:?} ceiling"
                );
            }
        });
    }

    /// Naps until `until` (the next minute), unless a maintenance window or
    /// an SSH session holds the device awake: then Wi-Fi stays up, the
    /// sessions are looked at every [`HOLD_CHECK`], and the minute's wake
    /// comes without a suspend. `nap` is the nap under way; a hold's timer
    /// stops once another nap, or the end of the doze, has replaced it.
    fn nap_or_hold(&self, now: DateTime<Utc>, until: DateTime<Utc>, nap: u64) {
        self.apply_clock_steps();
        let sessions = self.host.ssh_sessions();
        let (hold, was) = {
            let mut state = self.state.borrow_mut();
            let hold = state.maintenance.hold(now, &sessions);
            (hold, std::mem::replace(&mut state.held, hold))
        };
        if hold != was {
            crate::log!(
                "kindil: doze: {}",
                match hold {
                    Some(Hold::Window) => "maintenance window: awake and online for a while",
                    Some(Hold::Ssh) => "staying awake while an SSH session is open",
                    None => "napping again",
                }
            );
        }
        let left = (until - now).to_std().unwrap_or_default();
        let Some(hold) = hold else {
            return self.nap(now, until, left);
        };
        lock(&self.ledger).holding(now, hold);
        let Execution::Background = self.execution else {
            return self.after(left, DeviceEvent::Resumed);
        };
        let last = left <= HOLD_CHECK;
        let ctx = self.clone();
        slint::Timer::single_shot(left.min(HOLD_CHECK), move || {
            let current = {
                let state = ctx.state.borrow();
                state.naps == nap && matches!(state.system, Mode::Dozing(Doze::Napping { .. }))
            };
            match (current, last) {
                (false, _) => {}
                (true, true) => ctx.device(DeviceEvent::Resumed),
                (true, false) => ctx.nap_or_hold((ctx.now)(), until, nap),
            }
        });
    }

    /// Suspends until `until`, `left` from now, Wi-Fi parked; a nap too
    /// short to be worth it waits awake instead. A nap the host refused (its
    /// alarm couldn't be armed and read back) waits awake for the minute too,
    /// rather than redrawing at once and trying again in a loop.
    fn nap(&self, now: DateTime<Utc>, until: DateTime<Utc>, left: Duration) {
        lock(&self.ledger).napping(now, Instant::now());
        if left < MIN_NAP {
            return self.after(left, DeviceEvent::Resumed);
        }
        lock(&self.ledger).wifi_off(now);
        self.state.borrow_mut().wifi_parked = true;
        let ledger = self.ledger.clone();
        let clock = self.now;
        // Previews and tests have no time to wait in.
        let wait = matches!(self.execution, Execution::Background);
        self.device_later(move |host| {
            // Down until a fetch is due: nothing joins after the resume.
            host.park_wifi();
            match host.nap(until) {
                Ok(()) => {
                    // Before the wake reads the clock: the resume lost time.
                    if let Some(correction) = host.catch_up_clock() {
                        lock(&ledger).clock_corrected(correction);
                    }
                    DeviceEvent::Resumed
                }
                Err(error) => {
                    crate::log!("kindil: nap: {error:#}; awake until the minute");
                    if wait {
                        let left = (until - clock()).to_std().unwrap_or_default();
                        std::thread::sleep(left.min(Duration::from_secs(60)));
                    }
                    DeviceEvent::SuspendFailed
                }
            }
        });
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

    /// The dashboard worker's ticks, for a thread to send on.
    fn miqat_ticks(&self) -> Option<mpsc::Sender<Tick>> {
        match &self.state.borrow().miqat {
            Worker::Started(ticks) => Some(ticks.clone()),
            Worker::Idle => None,
        }
    }

    fn tick_miqat(&self, tick: Tick) {
        if let Worker::Started(ticks) = &self.state.borrow().miqat {
            // A stopped worker has reported its error on screen already.
            let _ = ticks.send(tick);
        }
    }

    fn check_idle(&self) {
        let now = Instant::now();
        let due = {
            let mut state = self.state.borrow_mut();
            // Asleep or dozing already: nothing more to fall into.
            if !matches!(state.system, Mode::Awake { .. }) {
                return;
            }
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
        // kindil's own USB drive mode waits for this before unmounting.
        self.host.storage_released();
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
                        crate::log!("kindil: keeping the update failed, retrying: {error:#}");
                        std::thread::sleep(update::SETTLE);
                    }
                }
            },
            |_, commit| match commit {
                update::Commit::Nothing => {}
                update::Commit::Committed => crate::log!("kindil: update kept"),
            },
        );
    }

    fn exit_app(&self) {
        // Errors only when there is no event loop (previews): nothing to quit.
        let _ = slint::quit_event_loop();
    }

    // ── prayer times ──

    /// Reads Miqat's file for the Prayer times page and its index row.
    fn show_prayer(&self) {
        let path = self.config.miqat_config.clone();
        let now = (self.now)();
        self.background(
            move |_| prayer::load(&path, now),
            |ui, page| show_prayer(ui, page.map_err(|e| format!("{e:#}"))),
        );
    }

    fn choose_prayer_method(&self, index: i32) {
        let method = usize::try_from(index)
            .ok()
            .and_then(|i| prayer::METHODS.get(i));
        if let Some(method) = method {
            self.choose_prayer(Setting::Method(method.method));
        }
    }

    fn choose_asr_school(&self, index: i32) {
        let school = usize::try_from(index)
            .ok()
            .and_then(|i| prayer::SCHOOLS.get(i));
        if let Some(school) = school {
            self.choose_prayer(Setting::Asr(school.madhab));
        }
    }

    /// Writes the choice into Miqat's file, shows the file as it now is, and
    /// has the dashboard read it again.
    fn choose_prayer(&self, setting: Setting) {
        let path = self.config.miqat_config.clone();
        let now = (self.now)();
        let ticks = match &self.state.borrow().miqat {
            Worker::Started(ticks) => Some(ticks.clone()),
            Worker::Idle => None,
        };
        self.background(
            move |_| prayer::save(&path, setting).and_then(|()| prayer::load(&path, now)),
            move |ui, page| match page {
                Ok(page) => {
                    show_prayer(ui, Ok(page));
                    // An idle dashboard reads the file when it starts.
                    if let Some(ticks) = ticks {
                        let _ = ticks.send(Tick::Reload);
                    }
                }
                Err(error) => {
                    let error = format!("{error:#}");
                    notify(ui, &format!("Prayer times not saved: {error}"));
                    ui.global::<Kindil>().set_prayer_error(error.into());
                }
            },
        );
    }

    // ── location ──

    /// Reads Miqat's file for the Location page and its index row.
    fn show_location(&self) {
        let path = self.config.miqat_config.clone();
        let named = location::named_path(&self.config.prefs);
        self.background(
            move |_| location::load(&path, location::read_named(&named).as_ref()),
            |ui, place| show_location(ui, place.map_err(|e| format!("{e:#}"))),
        );
    }

    /// The coordinates page starts from the time zone in use.
    fn start_coordinates(&self) {
        self.show_location();
        let Some(ui) = self.ui() else { return };
        let k = ui.global::<Kindil>();
        if k.get_typed_zone().is_empty() {
            let zone = k.get_location_zone();
            k.set_typed_zone(zone);
        }
        k.set_zone_matches(model(Vec::new()));
    }

    fn search_places(&self, name: slint::SharedString) {
        let Some(ui) = self.ui() else { return };
        let k = ui.global::<Kindil>();
        if k.get_location_busy() {
            return;
        }
        let name = name.trim().to_string();
        if name.chars().count() < 2 {
            k.set_location_status("Type at least two letters of the name.".into());
            return;
        }
        k.set_location_busy(true);
        k.set_location_status(format!("Searching for “{name}”…").into());
        let finding = self.finding.clone();
        self.background(
            {
                let name = name.clone();
                move |host| host.search_places(&name)
            },
            move |ui, result| {
                let k = ui.global::<Kindil>();
                k.set_location_busy(false);
                let (places, status) = match result {
                    Ok(places) => {
                        let status = match places.len() {
                            0 => format!(
                                "No places found for “{name}”. Check the spelling, or try a nearby city."
                            ),
                            1 => "1 place. Tap it to use it.".to_string(),
                            n => format!("{n} places. Tap one to use it."),
                        };
                        (places, status)
                    }
                    Err(error) => (Vec::new(), format!("Search failed: {error:#}")),
                };
                k.set_location_status(status.into());
                k.set_places(model(places.iter().map(view::place).collect()));
                lock(&finding).places = places;
            },
        );
    }

    fn choose_place(&self, index: i32) {
        let Some(ui) = self.ui() else { return };
        let place = usize::try_from(index)
            .ok()
            .and_then(|i| lock(&self.finding).places.get(i).cloned());
        if let Some(place) = place {
            confirm_location(&ui, &self.finding, place, "");
        }
    }

    /// Only ever on the owner's tap: never at start or in the background.
    fn locate_by_ip(&self) {
        let Some(ui) = self.ui() else { return };
        let k = ui.global::<Kindil>();
        if k.get_location_busy() {
            return;
        }
        k.set_location_busy(true);
        k.set_location_status("Asking ipinfo.io where your internet address is…".into());
        let finding = self.finding.clone();
        self.background(
            |host| host.locate_by_ip(),
            move |ui, result| {
                let k = ui.global::<Kindil>();
                k.set_location_busy(false);
                match result {
                    Ok(place) => {
                        k.set_location_status("".into());
                        confirm_location(ui, &finding, place, IP_NOTE);
                    }
                    Err(error) => k.set_location_status(
                        format!("Couldn't find your location: {error:#}").into(),
                    ),
                }
            },
        );
    }

    fn enter_coordinates(
        &self,
        latitude: slint::SharedString,
        longitude: slint::SharedString,
        zone: slint::SharedString,
    ) {
        let Some(ui) = self.ui() else { return };
        match location::typed(&latitude, &longitude, &zone) {
            Ok(place) => {
                ui.global::<Kindil>().set_location_status("".into());
                confirm_location(&ui, &self.finding, place, "");
            }
            Err(error) => ui.global::<Kindil>().set_location_status(error.into()),
        }
    }

    /// The zones holding what was typed; none once it names one exactly.
    fn filter_zones(&self, typed: slint::SharedString) {
        let Some(ui) = self.ui() else { return };
        let exact = typed.trim().parse::<chrono_tz::Tz>().is_ok();
        let matches = if exact {
            Vec::new()
        } else {
            location::zones(&typed, 6)
                .into_iter()
                .map(slint::SharedString::from)
                .collect()
        };
        ui.global::<Kindil>().set_zone_matches(model(matches));
    }

    fn cancel_location(&self) {
        lock(&self.finding).pending = None;
        if let Some(ui) = self.ui() {
            ui.global::<Kindil>().set_confirm_open(false);
        }
    }

    /// Writes the confirmed place into Miqat's file and kindil's, then the
    /// clock and the dashboard follow it.
    fn save_location(&self) {
        let Some(ui) = self.ui() else { return };
        let Some(place) = lock(&self.finding).pending.clone() else {
            return;
        };
        let k = ui.global::<Kindil>();
        if k.get_location_busy() {
            return;
        }
        k.set_location_busy(true);
        let (miqat, kindil) = (self.config.miqat_config.clone(), self.config.path.clone());
        let named_path = location::named_path(&self.config.prefs);
        let ticks = match &self.state.borrow().miqat {
            Worker::Started(ticks) => Some(ticks.clone()),
            Worker::Idle => None,
        };
        let (finding, timezone, now) = (self.finding.clone(), self.timezone.clone(), self.now);
        self.background(
            move |_| {
                location::save(&miqat, &kindil, &place)?;
                // Only the name is lost if this fails: the place is saved.
                if let Some(named) = location::named(&place) {
                    if let Err(error) = location::write_named(&named_path, &named) {
                        crate::log!("kindil: {error:#}");
                    }
                }
                location::load(&miqat, location::read_named(&named_path).as_ref())
            },
            move |ui, saved| {
                let k = ui.global::<Kindil>();
                k.set_location_busy(false);
                match saved {
                    Ok(place) => {
                        lock(&finding).pending = None;
                        *lock(&timezone) = place.timezone;
                        show_clock(ui, now(), place.timezone);
                        notify(ui, &format!("Location saved: {}", place.title()));
                        show_location(ui, Ok(place));
                        k.set_typed_zone("".into());
                        k.invoke_go(Page::Location);
                        // New weather and radar for the new place; an idle
                        // dashboard reads the file when it starts.
                        if let Some(ticks) = ticks {
                            let _ = ticks.send(Tick::Reload);
                        }
                    }
                    Err(error) => {
                        k.set_confirm_open(false);
                        k.set_location_status(format!("Not saved: {error:#}").into());
                    }
                }
            },
        );
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

/// Whether fetched weather and radar still fit after the file changed: the
/// same place, units and map.
fn same_sources(old: &miqat::config::Config, new: &miqat::config::Config) -> bool {
    let (a, b) = (&old.location, &new.location);
    a.latitude == b.latitude
        && a.longitude == b.longitude
        && a.timezone == b.timezone
        && old.radar.zoom == new.radar.zoom
        && old.weather.temperature_unit == new.weather.temperature_unit
        && old.weather.wind_unit == new.weather.wind_unit
}

/// Miqat's file; until it reads, the error is on screen and only a
/// [`Tick::Reload`] tries again. None once kindil has stopped asking.
fn load_miqat(
    path: &std::path::Path,
    weak: &slint::Weak<AppWindow>,
    ticks: &mpsc::Receiver<Tick>,
) -> Option<miqat::config::Config> {
    loop {
        match miqat::config::Config::load(path) {
            Ok(config) => return Some(config),
            Err(error) => {
                let message = format!("Prayer dashboard needs {}: {error}", path.display());
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.global::<Kindil>().set_miqat_error(message.into())
                });
            }
        }
        while ticks.recv().ok()? != Tick::Reload {}
    }
}

/// A rendered dashboard frame: the header, and the body unless only the
/// header was drawn.
type Rendered = (
    slint::SharedPixelBuffer<slint::Rgb8Pixel>,
    Option<slint::SharedPixelBuffer<slint::Rgb8Pixel>>,
);

/// Renders what `asked` calls for at `now`, given the board of the full
/// frame on screen; returns the frame (or its error), what it drew, and the
/// board on screen after it.
fn render_miqat(
    config: &miqat::config::Config,
    renderer: &miqat::render::Renderer,
    sources: &miqat::sources::Sources,
    now: DateTime<Utc>,
    asked: Draw,
    shown: Option<Board>,
) -> (Result<Rendered, String>, Draw, Option<Board>) {
    let board = Board::at(config, now);
    let draw = crate::dashboards::draw(asked, shown, board);
    let frame = match draw {
        Draw::Full => crate::dashboards::render(renderer, config, now, sources).map(|page| {
            let split = crate::dashboards::Split::of(page);
            (pixels(split.header), Some(pixels(split.body)))
        }),
        Draw::Header => {
            crate::dashboards::render_header(renderer, config, now).map(|h| (pixels(h), None))
        }
    }
    .map_err(|e| e.to_string());
    let shown = match (&frame, draw) {
        (Ok(_), Draw::Full) => board,
        (Ok(_), Draw::Header) => shown,
        // Whatever is on screen, the next draw is in full.
        (Err(_), _) => None,
    };
    (frame, draw, shown)
}

/// Owns Miqat's cached weather and radar sources; renders on each minute.
fn miqat_worker(
    path: &std::path::Path,
    weak: &slint::Weak<AppWindow>,
    display: &Display,
    ticks: &mpsc::Receiver<Tick>,
) {
    let Some(mut config) = load_miqat(path, weak, ticks) else {
        return;
    };
    let mut renderer = miqat::render::Renderer::new(config.display.contrast);
    let mut sources = miqat::sources::Sources::new(&config);
    // The board of the full frame on screen.
    let mut shown = None;
    let mut show = |config: &miqat::config::Config,
                    renderer: &miqat::render::Renderer,
                    sources: &miqat::sources::Sources,
                    wake: Option<u64>,
                    asked: Draw| {
        let (frame, drew, board) =
            render_miqat(config, renderer, sources, Utc::now(), asked, shown);
        shown = board;
        let display = display.clone();
        weak.upgrade_in_event_loop(move |ui| {
            let k = ui.global::<Kindil>();
            match frame {
                Ok((header, body)) => {
                    k.set_miqat_header(slint::Image::from_rgb8(header));
                    let update = match body {
                        Some(body) => {
                            k.set_miqat_body(slint::Image::from_rgb8(body));
                            Update::DashboardFrame
                        }
                        None => Update::DashboardHeader,
                    };
                    display.updated(&ui, update);
                }
                Err(error) => k.set_miqat_error(error.into()),
            }
            if wake.is_some() && (asked, drew) == (Draw::Header, Draw::Full) {
                lock(&display.ledger).drew_full();
            }
            display.dashboard_shown(wake);
        })
    };
    // First paint before any network, so a slow fetch never leaves it blank.
    if show(&config, &renderer, &sources, None, Draw::Full).is_err() {
        return;
    }
    let mut tick = Tick::Fetch(None);
    loop {
        let (wake, asked) = match tick {
            Tick::Fetch(wake) => {
                sources.refresh(&config, Utc::now());
                (wake, Draw::Full)
            }
            Tick::Draw(wake) => (wake, Draw::Full),
            Tick::Minute(wake) => (wake, Draw::Header),
            // A file that no longer reads keeps the dashboard as it was.
            Tick::Reload => match miqat::config::Config::load(path) {
                Ok(fresh) => {
                    if !same_sources(&config, &fresh) {
                        sources = miqat::sources::Sources::new(&fresh);
                        sources.refresh(&fresh, Utc::now());
                    }
                    renderer = miqat::render::Renderer::new(fresh.display.contrast);
                    config = fresh;
                    (None, Draw::Full)
                }
                Err(error) => {
                    crate::log!("kindil: reloading {}: {error:#}", path.display());
                    (None, Draw::Full)
                }
            },
        };
        if show(&config, &renderer, &sources, wake, asked).is_err() {
            return;
        }
        tick = loop {
            match ticks.recv_timeout(until_next_minute(Utc::now())) {
                Ok(tick) => break tick,
                // Dozing, wakes ask for draws and fetches: this clock only
                // runs while the device is up, and Wi-Fi is parked.
                Err(mpsc::RecvTimeoutError::Timeout) if display.is_dozing() => {}
                Err(mpsc::RecvTimeoutError::Timeout) => break Tick::Fetch(None),
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wake_is_ended_once_and_only_while_current() {
        let wake = DrawWake::default();
        let first = wake.begin();
        assert!(wake.claim(first), "the redraw or the timer ends it");
        assert!(!wake.claim(first), "never twice");
        let second = wake.begin();
        assert!(
            !wake.claim(first),
            "an old wake's timer can't cut the next one short"
        );
        assert!(wake.claim(second));
    }

    fn display(frames: Arc<AtomicU64>, wait_idle: fn() -> std::io::Result<()>) -> Display {
        Display {
            panel: Panel::Lcd,
            ghosting: Arc::default(),
            presented: Some(Presented { frames, wait_idle }),
            dashboard: Arc::default(),
            dozing: Arc::default(),
            panel_error_logged: Arc::default(),
            ledger: Arc::new(Mutex::new(DozeLedger::new(Utc::now(), None))),
        }
    }

    fn eink() -> (Display, Arc<AtomicU64>) {
        let frames = Arc::new(AtomicU64::new(0));
        (display(frames.clone(), || Ok(())), frames)
    }

    #[test]
    fn waits_for_the_frame_that_draws_its_own_wakes_dashboard() {
        let (display, frames) = eink();
        let deadline = Instant::now() + Duration::from_secs(5);
        let waiter = {
            let display = display.clone();
            std::thread::spawn(move || display.wait_drawn(Some(7), 0, deadline))
        };
        // A frame from before, and a dashboard rendered for an earlier wake
        // or for no wake, don't count.
        frames.fetch_add(1, Ordering::Release);
        display.dashboard_shown(Some(6));
        display.dashboard_shown(None);
        std::thread::sleep(Duration::from_millis(30));
        assert!(!waiter.is_finished());
        display.dashboard_shown(Some(7));
        std::thread::sleep(Duration::from_millis(30));
        assert!(!waiter.is_finished(), "shown, not yet drawn");
        frames.fetch_add(1, Ordering::Release);
        assert!(matches!(waiter.join().unwrap(), Drawn::Done));
    }

    #[test]
    fn a_panel_that_cant_say_it_finished_has_the_frame_sent() {
        let display = display(Arc::new(AtomicU64::new(1)), || {
            Err(std::io::Error::from_raw_os_error(libc::EINVAL))
        });
        // Not the ceiling: the frame is with the panel, which only can't
        // tell when it's done; the wake naps a panel update later.
        assert!(matches!(
            display.wait_drawn(None, 0, Instant::now() + Duration::from_secs(5)),
            Drawn::Sent(_)
        ));
    }

    #[test]
    fn gives_up_at_the_deadline() {
        let (display, _) = eink();
        let soon = Instant::now() + Duration::from_millis(40);
        assert!(matches!(
            display.wait_drawn(Some(1), 0, soon),
            Drawn::NotSeen
        ));
        assert!(matches!(display.wait_drawn(None, 0, soon), Drawn::NotSeen));
        let (no_panel, _) = eink();
        let no_panel = Display {
            presented: None,
            ..no_panel
        };
        assert!(matches!(
            no_panel.wait_drawn(None, 0, Instant::now() + Duration::from_secs(5)),
            Drawn::NotSeen
        ));
    }
    #[test]
    fn a_new_prayer_method_keeps_the_fetched_weather_and_radar() {
        let config: miqat::config::Config =
            toml::from_str(include_str!("../examples/miqat.toml")).unwrap();
        let method = config.prayer.method;
        let mut other = config.clone();
        other.prayer.madhab = miqat::config::Madhab::Hanafi;
        assert!(same_sources(&config, &other));
        let elsewhere: miqat::config::Config =
            toml::from_str(&include_str!("../examples/miqat.toml").replace("52.3676", "21.4225"))
                .unwrap();
        assert_eq!(elsewhere.prayer.method, method);
        assert!(!same_sources(&config, &elsewhere));
    }

    #[test]
    fn clock_ticks_on_the_minute() {
        let now = DateTime::from_timestamp(1_794_398_400 + 45, 250_000_000).unwrap();
        assert_eq!(until_next_minute(now), Duration::from_millis(14_800));
    }
}
