//! The installer's window: the same steps as `kindil-install install`, with
//! the Kindle looked for every 2 seconds.
//!
//! All state lives on the UI thread ([`State`], rendered by [`render`]);
//! detection, downloads, copying and ejecting run on worker threads and
//! post their results back to it.
use kindil_installer::check::{self, Status};
use kindil_installer::detect::{self, Kindle};
use kindil_installer::install::Event;
use kindil_installer::release::Github;
use kindil_installer::{eject, NEXT_STEPS};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::time::Duration;

slint::include_modules!();

/// What the window shows.
#[derive(Default)]
pub struct State {
    /// The latest release's version, or why it can't be had; None while
    /// GitHub is being asked.
    pub latest: Option<Result<String, String>>,
    pub kindles: Vec<Kindle>,
    /// A scan is running.
    pub scanning: bool,
    /// Downloading, copying or ejecting.
    pub busy: bool,
    pub progress: f32,
    pub progress_text: String,
    pub done: bool,
    /// The Kindle went away after the install (ejected or unplugged).
    pub gone_since_done: bool,
    pub error: String,
    pub eject_text: String,
    pub ejected: bool,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// Changes the state and redraws; on the UI thread.
fn update(ui: &InstallerWindow, change: impl FnOnce(&mut State)) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        change(&mut s);
        render(ui, &s);
    });
}

/// Posts `change` to the UI thread from a worker.
fn post(weak: &slint::Weak<InstallerWindow>, change: impl FnOnce(&mut State) + Send + 'static) {
    let _ = weak.upgrade_in_event_loop(move |ui| update(&ui, change));
}

fn status(status: Status) -> CheckStatus {
    match status {
        Status::Pass => CheckStatus::Pass,
        Status::Fail => CheckStatus::Fail,
        Status::Unknown => CheckStatus::Unknown,
    }
}

/// What the install button says for `kindle` and release `latest`.
pub fn install_label(kindle: Option<&Kindle>, latest: Option<&str>) -> String {
    let Some(latest) = latest else {
        return "Install kindil".into();
    };
    match kindle.and_then(|k| k.installed.as_ref()) {
        None => format!("Install kindil {latest}"),
        Some(i) => match i.version.as_deref() {
            Some(old) if old == latest => format!("Reinstall kindil {latest}"),
            Some(old) => format!("Upgrade kindil {old} → {latest}"),
            None => format!("Upgrade to kindil {latest}"),
        },
    }
}

/// Shows `s` in the window.
pub fn render(ui: &InstallerWindow, s: &State) {
    let kindle = match s.kindles.as_slice() {
        [one] => Some(one),
        _ => None,
    };
    ui.set_found(kindle.is_some());
    ui.set_kindle_status(SharedString::from(match (s.kindles.len(), kindle) {
        (_, Some(k)) => format!("Found a Kindle at {}", k.volume.display()),
        (0, _) if s.done => "The Kindle is unplugged.".to_owned(),
        (0, _) => "Looking for a Kindle…".to_owned(),
        (n, _) => format!("{n} Kindles are plugged in: unplug all but one."),
    }));
    let checks = kindle.map(check::checklist).unwrap_or_default();
    if let Some(k) = kindle {
        ui.set_model(k.model.to_string().into());
        ui.set_firmware(
            k.firmware
                .as_ref()
                .map(|f| f.to_string())
                .unwrap_or_else(|| "unknown (no system/version.txt)".into())
                .into(),
        );
        ui.set_installed(
            match &k.installed {
                None => "not installed".to_owned(),
                Some(i) => {
                    let version = i.version.as_deref().unwrap_or("a version");
                    if i.config.is_empty() {
                        format!("{version} installed")
                    } else {
                        format!(
                            "{version} installed; settings kept ({})",
                            i.config.join(", ")
                        )
                    }
                }
            }
            .into(),
        );
    }
    let ready = !checks.is_empty() && check::ready(&checks, false);
    ui.set_ready(ready);
    ui.set_needs_vouch(!ready && !checks.is_empty() && check::ready(&checks, true));
    let items: Vec<CheckItem> = checks
        .iter()
        .map(|c| CheckItem {
            title: c.title.as_str().into(),
            detail: c.detail.as_str().into(),
            status: status(c.status),
            links: ModelRc::new(VecModel::from(
                c.links
                    .iter()
                    .map(|l| SharedString::from(*l))
                    .collect::<Vec<_>>(),
            )),
        })
        .collect();
    ui.set_checks(ModelRc::new(VecModel::from(items)));
    let latest = s.latest.as_ref().and_then(|l| l.as_ref().ok());
    ui.set_have_release(latest.is_some());
    ui.set_latest(
        match &s.latest {
            None => "Asking GitHub for the latest release…".to_owned(),
            Some(Ok(v)) => format!(
                "Latest release: kindil {v}, signed by kindil's release key. Your settings on the Kindle are kept."
            ),
            Some(Err(e)) => {
                format!("Couldn't reach GitHub ({e}). Check the internet connection, then Look again.")
            }
        }
        .into(),
    );
    ui.set_install_label(install_label(kindle, latest.map(String::as_str)).into());
    ui.set_busy(s.busy);
    ui.set_progress(s.progress);
    ui.set_progress_text(s.progress_text.as_str().into());
    ui.set_done(s.done);
    ui.set_next_steps(NEXT_STEPS.into());
    ui.set_error(s.error.as_str().into());
    ui.set_eject_text(s.eject_text.as_str().into());
    ui.set_ejected(s.ejected);
}

/// Takes in a scan's result.
pub fn scanned(s: &mut State, kindles: Vec<Kindle>) {
    s.scanning = false;
    if s.done && kindles.is_empty() {
        s.gone_since_done = true;
    }
    // A Kindle plugged in after an install starts over.
    if s.gone_since_done && !kindles.is_empty() {
        s.done = false;
        s.gone_since_done = false;
        s.ejected = false;
        s.eject_text.clear();
        s.progress = 0.0;
        s.progress_text.clear();
    }
    s.kindles = kindles;
}

fn scan(ui: &InstallerWindow) {
    let start = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let start = !s.scanning && !s.busy;
        s.scanning |= start;
        start
    });
    if !start {
        return;
    }
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let kindles = detect::scan();
        post(&weak, move |s| scanned(s, kindles));
    });
}

fn fetch_latest(ui: &InstallerWindow) {
    update(ui, |s| s.latest = None);
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let latest = Github::default().latest().map_err(|e| format!("{e:#}"));
        post(&weak, move |s| s.latest = Some(latest));
    });
}

fn install(ui: &InstallerWindow) {
    let Some((kindle, version)) = STATE.with(|s| {
        let s = s.borrow();
        let kindle = match s.kindles.as_slice() {
            [one] => one.clone(),
            _ => return None,
        };
        let version = s.latest.clone()?.ok()?;
        Some((kindle, version))
    }) else {
        return;
    };
    update(ui, |s| {
        s.busy = true;
        s.error.clear();
        s.progress = 0.0;
        s.progress_text = format!("Downloading kindil {version}…");
    });
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<()> {
            let mut shown = -1i32;
            let release = Github::default().download(&version, |what, n, total| {
                let (from, span) = if what == "kindil" {
                    (0.0, 0.6)
                } else {
                    (0.6, 0.1)
                };
                let part = total.map(|t| n as f32 / t.max(1) as f32).unwrap_or(0.5);
                let at = from + span * part.min(1.0);
                if (at * 100.0) as i32 != shown {
                    shown = (at * 100.0) as i32;
                    let text = match what {
                        "kindil" => format!("Downloading kindil {version}… {} KB", n / 1024),
                        _ => "Downloading the KUAL launcher files…".to_owned(),
                    };
                    post(&weak, move |s| {
                        s.progress = at;
                        s.progress_text = text;
                    });
                }
            })?;
            post(&weak, |s| {
                s.progress = 0.7;
                s.progress_text = "Downloaded; signature checked. Copying to the Kindle…".into();
            });
            let report = kindil_installer::install::install(&kindle.volume, &release, |event| {
                let (at, text) = match event {
                    Event::Wrote { path, n, of } => (
                        0.7 + 0.3 * *n as f32 / *of as f32,
                        format!("Copied and checked {}", path.display()),
                    ),
                    Event::Kept { path } => (0.7, format!("Kept your {}", path.display())),
                    Event::Removed { path } => (0.7, format!("Removed stale {}", path.display())),
                };
                post(&weak, move |s| {
                    s.progress = at;
                    s.progress_text = text;
                });
            })?;
            let after = detect::inspect(&kindle.volume, kindle.serial.as_deref());
            let summary = format!(
                "kindil {version} is on the Kindle: {} files copied and checked{}.",
                report.written.len(),
                if report.kept.is_empty() {
                    String::new()
                } else {
                    format!(", your settings kept ({} files)", report.kept.len())
                }
            );
            post(&weak, move |s| {
                s.busy = false;
                s.done = true;
                s.progress = 1.0;
                s.progress_text = summary;
                s.kindles = vec![after];
            });
            Ok(())
        })();
        if let Err(error) = result {
            post(&weak, move |s| {
                s.busy = false;
                s.error = format!("Install failed: {error:#}");
            });
        }
    });
}

fn eject_kindle(ui: &InstallerWindow) {
    let Some(volume) = STATE.with(|s| s.borrow().kindles.first().map(|k| k.volume.clone())) else {
        return;
    };
    update(ui, |s| {
        s.busy = true;
        s.eject_text = "Ejecting…".into();
    });
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let result = eject::eject(&volume);
        post(&weak, move |s| {
            s.busy = false;
            match result {
                Ok(()) => {
                    s.ejected = true;
                    s.eject_text =
                        "Ejected: unplug the Kindle, then start kindil from KUAL.".into();
                }
                Err(e) => s.eject_text = format!("Couldn't eject ({e:#}). {}", eject::by_hand()),
            }
        });
    });
}

/// Opens the window and runs until it closes.
pub fn run() -> anyhow::Result<()> {
    let ui = InstallerWindow::new()?;
    STATE.with(|s| render(&ui, &s.borrow()));
    {
        let weak = ui.as_weak();
        ui.on_install(move || {
            if let Some(ui) = weak.upgrade() {
                install(&ui);
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_eject(move || {
            if let Some(ui) = weak.upgrade() {
                eject_kindle(&ui);
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_refresh(move || {
            if let Some(ui) = weak.upgrade() {
                scan(&ui);
                if STATE.with(|s| matches!(s.borrow().latest, Some(Err(_)))) {
                    fetch_latest(&ui);
                }
            }
        });
    }
    ui.on_open_link(|url| {
        let _ = eject::open_url(&url);
    });
    fetch_latest(&ui);
    scan(&ui);
    let timer = slint::Timer::default();
    {
        let weak = ui.as_weak();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_secs(2),
            move || {
                if let Some(ui) = weak.upgrade() {
                    scan(&ui);
                }
            },
        );
    }
    ui.run()?;
    Ok(())
}
