//! Supervising the processes kindil needs but does not contain.
//!
//! Three things are better left to battle-tested C than reimplemented:
//!
//! * `wpa_supplicant` owns the WPA2 handshake (there is no sane way to do that
//!   from scratch), and `udhcpc` gets a lease.
//! * `dropbear` is the SSH server. A from-scratch SSH implementation is a
//!   project of its own; supervising a 200 KiB binary is not.
//! * `syncthing` (optional, and heavy for an 800 MHz single core).
//!
//! kindil is PID 1 in the initramfs image, so it starts these, restarts them
//! with exponential backoff, and logs their output under `/var/log/kindil`.
//! A program that is simply not installed is disabled rather than retried
//! forever - an image built without dropbear should not spam the log.

use crate::config::Config;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Where a service is, from the supervisor's point of view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceStatus {
    /// Never started yet.
    Idle,
    /// Running.
    Running,
    /// Waiting to be restarted.
    BackingOff,
    /// The program does not exist, so it will not be started again.
    Disabled,
    /// Deliberately not enabled.
    Off,
}

impl ServiceStatus {
    /// A short label for the Settings screen.
    pub const fn label(self) -> &'static str {
        match self {
            ServiceStatus::Idle => "idle",
            ServiceStatus::Running => "running",
            ServiceStatus::BackingOff => "retrying",
            ServiceStatus::Disabled => "not installed",
            ServiceStatus::Off => "off",
        }
    }
}

/// What to run.
#[derive(Clone, Debug)]
pub struct ServiceSpec {
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
}

impl ServiceSpec {
    /// Build a spec, splitting nothing: `args` are already separate.
    pub fn new(name: &str, program: &str, args: &[&str]) -> Self {
        ServiceSpec {
            name: name.to_string(),
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// The first restart delay, doubled on each failure up to [`MAX_BACKOFF`].
const MIN_BACKOFF: Duration = Duration::from_secs(1);
/// The longest delay between restarts.
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// A run longer than this resets the backoff.
const HEALTHY_RUN: Duration = Duration::from_secs(30);

/// One supervised process.
pub struct Service {
    spec: ServiceSpec,
    child: Option<Child>,
    status: ServiceStatus,
    backoff: Duration,
    next_start: Option<Instant>,
    started: Option<Instant>,
    log_path: PathBuf,
}

impl Service {
    /// Create a service; nothing is started until [`Service::poll`].
    pub fn new(spec: ServiceSpec, log_dir: &Path) -> Self {
        Service {
            log_path: log_dir.join(std::format!("{}.log", spec.name)),
            spec,
            child: None,
            status: ServiceStatus::Idle,
            backoff: MIN_BACKOFF,
            next_start: Some(Instant::now()),
            started: None,
        }
    }

    /// The service name.
    pub fn name(&self) -> &str {
        &self.spec.name
    }

    /// The current status.
    pub fn status(&self) -> ServiceStatus {
        self.status
    }

    /// Start, restart or reap the process as needed.
    pub fn poll(&mut self, now: Instant) {
        if std::fs::metadata(&self.log_path).is_ok_and(|m| m.len() > 256 * 1024) {
            if let Ok(file) = std::fs::OpenOptions::new().write(true).open(&self.log_path) {
                let _ = file.set_len(0);
            }
        }
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.note_exit(&std::format!("exited ({status})"));
                    self.schedule(now);
                }
                Ok(None) => {}
                // Reaped elsewhere (kindil also reaps orphans as PID 1).
                Err(_) => {
                    self.child = None;
                    self.status = ServiceStatus::BackingOff;
                    self.schedule(now);
                }
            }
            return;
        }

        if self.status == ServiceStatus::Disabled {
            return;
        }
        if let Some(at) = self.next_start {
            if now >= at {
                self.next_start = None;
                self.spawn();
            }
        }
    }

    fn spawn(&mut self) {
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
            .ok();
        let mut command = Command::new(&self.spec.program);
        command
            .args(&self.spec.args)
            .stdin(Stdio::null())
            .stdout(match &log {
                Some(file) => file
                    .try_clone()
                    .map(Stdio::from)
                    .unwrap_or_else(|_| Stdio::null()),
                None => Stdio::null(),
            })
            .stderr(match log {
                Some(file) => Stdio::from(file),
                None => Stdio::null(),
            });

        match command.spawn() {
            Ok(child) => {
                self.log(&std::format!("started (pid {})", child.id()));
                self.child = Some(child);
                self.status = ServiceStatus::Running;
                self.started = Some(Instant::now());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.status = ServiceStatus::Disabled;
                self.log(&std::format!(
                    "{} not found; service disabled",
                    self.spec.program
                ));
            }
            Err(e) => {
                self.log(&std::format!("could not start: {e}"));
                self.status = ServiceStatus::BackingOff;
                self.schedule(Instant::now());
            }
        }
    }

    fn note_exit(&mut self, what: &str) {
        if let Some(started) = self.started.take() {
            if started.elapsed() >= HEALTHY_RUN {
                self.backoff = MIN_BACKOFF;
            }
        }
        self.log(&std::format!("{what}; restarting in {:?}", self.backoff));
        self.child = None;
        self.status = ServiceStatus::BackingOff;
    }

    fn schedule(&mut self, now: Instant) {
        self.next_start = Some(now + self.backoff);
        self.backoff = (self.backoff * 2).min(MAX_BACKOFF);
    }

    fn log(&self, message: &str) {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
        {
            let _ = writeln!(file, "{message}");
        }
    }
}

/// Starts and stops the configured services.
pub struct Supervisor {
    services: Vec<Service>,
}

impl Supervisor {
    pub fn empty() -> Self {
        Self {
            services: Vec::new(),
        }
    }
    /// Build the service set from the configuration.
    pub fn from_config(config: &Config, log_dir: &Path) -> Self {
        let _ = std::fs::create_dir_all(log_dir);
        let mut specs = Vec::new();

        if config.wifi {
            if let Some(conf) = write_wpa_config(config) {
                specs.push(ServiceSpec::new(
                    "wpa_supplicant",
                    "/sbin/wpa_supplicant",
                    &["-Dwext", "-i", &config.interface, "-c", &conf],
                ));
                specs.push(ServiceSpec::new(
                    "udhcpc",
                    "/sbin/udhcpc",
                    &["-i", &config.interface, "-f", "-s", "/sbin/kindil-dhcp"],
                ));
            }
        }

        if config.ssh {
            specs.push(ServiceSpec::new(
                "dropbear",
                "/sbin/dropbear",
                &["-F", "-E", "-R", "-s", "-p", "22"],
            ));
        }

        if config.sync {
            specs.push(ServiceSpec::new(
                "syncthing",
                "/usr/bin/syncthing",
                &["-no-browser", "-home=/mnt/us/.syncthing"],
            ));
        }

        Supervisor {
            services: specs
                .into_iter()
                .map(|spec| Service::new(spec, log_dir))
                .collect(),
        }
    }

    /// Poll every service.
    pub fn poll(&mut self, now: Instant) {
        for service in &mut self.services {
            service.poll(now);
        }
    }

    /// True if any service is configured, so the main loop can poll often.
    pub fn is_empty(&self) -> bool {
        self.services.is_empty()
    }

    /// `(name, status)` for the Settings screen.
    pub fn statuses(&self) -> Vec<(&str, ServiceStatus)> {
        self.services
            .iter()
            .map(|s| (s.name(), s.status()))
            .collect()
    }
}

/// Write a `wpa_supplicant.conf` from the configuration.
///
/// The passphrase lives in `/run/kindil/wpa_supplicant.conf` with mode 0600;
/// it is never logged.
fn write_wpa_config(config: &Config) -> Option<String> {
    let dir = Path::new("/run/kindil");
    let _ = std::fs::create_dir_all(dir);
    let path = dir.join("wpa_supplicant.conf");

    let psk = config.wifi_psk.as_deref().unwrap_or("");
    let mut text =
        String::from("ctrl_interface=/run/wpa_supplicant\nupdate_config=1\n\nnetwork={\n");
    if let Some(ssid) = &config.wifi_ssid {
        text.push_str(&std::format!("\tssid=\"{}\"\n", escape(ssid)));
        if psk.is_empty() {
            text.push_str("\tkey_mgmt=NONE\n");
        } else {
            text.push_str(&std::format!("\tpsk=\"{}\"\n", escape(psk)));
        }
        text.push_str("}\n");
    } else {
        text = String::from("ctrl_interface=/run/wpa_supplicant\nupdate_config=1\n");
    }

    let write = || -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        let mut file = tempfile::NamedTempFile::new_in(dir)?;
        file.write_all(text.as_bytes())?;
        file.persist(&path).map_err(|e| e.error)?;
        Ok(())
    };
    match write() {
        Ok(()) => Some(path.to_string_lossy().into_owned()),
        Err(e) => {
            eprintln!("kindil: cannot write {}: {e}", path.display());
            None
        }
    }
}

/// Escape a value for a double-quoted `wpa_supplicant` string.
fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(std::format!(
            "kindil-services-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_missing_program_is_disabled_not_retried() {
        let dir = tempdir("missing");
        let spec = ServiceSpec::new("nope", "/nonexistent/kindil-test", &[]);
        let mut service = Service::new(spec, &dir);
        service.poll(Instant::now());
        assert_eq!(service.status(), ServiceStatus::Disabled);

        // A second poll must not try again.
        service.poll(Instant::now() + Duration::from_secs(120));
        assert_eq!(service.status(), ServiceStatus::Disabled);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_crashing_service_backs_off_and_restarts() {
        let dir = tempdir("crash");
        let spec = ServiceSpec::new("crash", "/bin/sh", &["-c", "exit 3"]);
        let mut service = Service::new(spec, &dir);

        let start = Instant::now();
        service.poll(start);
        assert_eq!(service.status(), ServiceStatus::Running);

        // Wait for it to be reaped and rescheduled.
        let mut saw_backoff = false;
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(20));
            service.poll(Instant::now());
            if service.status() == ServiceStatus::BackingOff {
                saw_backoff = true;
                break;
            }
        }
        assert!(saw_backoff, "the crash was not noticed");

        // The backoff must delay the restart rather than spinning.
        assert!(service.next_start.is_some());
        let log = std::fs::read_to_string(dir.join("crash.log")).unwrap();
        assert!(log.contains("started"), "{log}");
        assert!(log.contains("exited"), "{log}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn escaping_keeps_wpa_strings_intact() {
        assert_eq!(escape("home"), "home");
        assert_eq!(escape("a\"b"), "a\\\"b");
        assert_eq!(escape("a\\b"), "a\\\\b");
    }

    #[test]
    fn no_services_means_an_empty_supervisor() {
        let dir = tempdir("empty");
        let config = Config::parse("ssh = false\nwifi = false\n").unwrap();
        let supervisor = Supervisor::from_config(&config, &dir);
        assert!(supervisor.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ssh_is_on_by_default() {
        let dir = tempdir("ssh");
        let config = Config::parse("wifi = false").unwrap();
        let supervisor = Supervisor::from_config(&config, &dir);
        assert!(!supervisor.is_empty());
        let statuses = supervisor.statuses();
        assert_eq!(statuses[0].0, "dropbear");
        assert_eq!(statuses[0].1, ServiceStatus::Idle);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
