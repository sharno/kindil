//! kindil's launcher on the Kindle, at /var/local/kindil/launcher.
//!
//! - `launcher start`: KUAL's "Start kindil" (bin/start.sh) and the boot
//!   guard start kindil's session through this, detached.
//! - `launcher session`: Amazon's UI and services step aside while kindil
//!   runs and always come back afterwards; crashes restart kindil, updates
//!   get one trial run, a version that keeps crashing makes way for the
//!   previous one.
//! - `launcher boot`: the start-at-boot guard (bin/boot.sh, from
//!   /etc/upstart/kindil.conf): kindil once per boot, and Amazon's UI again
//!   after boots kindil doesn't stay up in.
//!
//! It is the way back to Amazon's UI when kindil isn't well, so it is small,
//! shares no code with kindil and changes rarely: kindil carries it and
//! writes it out only once it has run for two minutes (src/launcher.rs), so
//! only a release that proved itself replaces it. Upstart's tools and eips
//! live in sbin, which SSH sessions lack: every command gets a full PATH.
mod boot;
mod os;
mod session;

use std::path::PathBuf;

/// Amazon's services kindil has no use for: connections (cmd, with tmd and
/// phd), apps, archive, power management helpers, dynamic config, updates,
/// crash dumps and sync. Stopped while kindil runs; Amazon's UI needs them
/// all back.
pub const SERVICES: [&str; 11] = [
    "cmd",
    "appmgrd",
    "archive",
    "dpmd",
    "dynconfig",
    "otaupd",
    "otav3",
    "rcm",
    "stackdumpd",
    "todo",
    "deviced",
];

/// Where everything is; the tests point it at a temporary directory.
pub struct Paths {
    /// kindil's own files on the system storage, which stays when a
    /// computer takes /mnt/us: the running binary, logs, state.
    pub run: PathBuf,
    /// The extension's bin: the installed binary and its update slots.
    pub ext: PathBuf,
    /// kindil's folder on the user storage.
    pub user: PathBuf,
    pub config: PathBuf,
    /// Start at boot while this exists; "standalone" in it skips Amazon's UI.
    pub flag: PathBuf,
    /// Amazon's framework job skips the Java framework while this exists;
    /// kindil then also skips X and the rest of Amazon's UI.
    pub skip: PathBuf,
    /// The charger's switch, which kindil may have left off.
    pub charger: PathBuf,
    /// kindil's own Wi-Fi children (src/net.rs).
    pub wifi_pids: Vec<PathBuf>,
    /// The running session's pid.
    pub pid_file: PathBuf,
    pub boot_id: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        Self {
            run: "/var/local/kindil".into(),
            ext: "/mnt/us/extensions/kindil/bin".into(),
            user: "/mnt/us/kindil".into(),
            config: "/mnt/us/kindil/kindil.toml".into(),
            flag: "/mnt/us/kindil/BOOT".into(),
            skip: "/mnt/us/DONT_START_FRAMEWORK".into(),
            charger: "/sys/devices/platform/aplite_charger.0/charging".into(),
            wifi_pids: vec![
                "/var/run/kindil-udhcpc.pid".into(),
                "/var/run/kindil-wpa.pid".into(),
            ],
            pid_file: "/var/tmp/kindil.pid".into(),
            boot_id: "/proc/sys/kernel/random/boot_id".into(),
        }
    }
}

impl Paths {
    pub fn log(&self) -> PathBuf {
        self.run.join("kindil.log")
    }
}

/// /var/local is 64 MB and also holds Amazon's settings: past this, the log
/// keeps only its last half.
const LOG_MAX: u64 = 256 * 1024;

/// Starts kindil's session, detached, unless one runs.
pub fn start(paths: &Paths, os: &dyn os::Os) {
    if os::read_pid(&paths.pid_file).is_some_and(|pid| os.alive(pid)) {
        return;
    }
    let _ = std::fs::create_dir_all(&paths.run);
    let _ = std::fs::create_dir_all(&paths.user);
    let log = paths.log();
    if std::fs::metadata(&log).is_ok_and(|m| m.len() > LOG_MAX) {
        if let Ok(text) = std::fs::read(&log) {
            let keep = &text[text.len() - (LOG_MAX / 2) as usize..];
            let part = log.with_extension("log.old");
            if std::fs::write(&part, keep).is_ok() {
                let _ = std::fs::rename(&part, &log);
            }
        }
    }
    match os.spawn_session(&log) {
        Some(pid) => {
            let _ = std::fs::write(&paths.pid_file, format!("{pid}\n"));
        }
        None => os::log("can't start kindil's session"),
    }
}

fn main() -> std::process::ExitCode {
    let paths = Paths::default();
    let os = os::Device;
    match std::env::args().nth(1).as_deref() {
        Some("start") => start(&paths, &os),
        Some("session") => return session::session(&paths, &os),
        Some("boot") => boot::boot(&paths, &os),
        Some("--version") => println!("launcher {}", env!("CARGO_PKG_VERSION")),
        _ => {
            eprintln!("usage: launcher start | session | boot | --version");
            return std::process::ExitCode::from(2);
        }
    }
    std::process::ExitCode::SUCCESS
}
