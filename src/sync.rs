//! Optional Syncthing, started and stopped from Settings.
//!
//! The PW1 has a single 800 MHz core and 256 MiB of RAM, so Syncthing runs at
//! low CPU priority with a single Go thread and an aggressive GC, and only
//! while switched on. kindil does not download it: the user supplies a build
//! in `[syncthing]` (see README for kernel caveats).
use crate::config::Syncthing;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::Instant;

pub struct Stopped(Syncthing);

pub struct Running {
    config: Syncthing,
    child: Child,
    pub since: Instant,
}

pub struct Failed {
    config: Syncthing,
    pub reason: String,
}

pub enum Service {
    NotInstalled,
    Stopped(Stopped),
    Running(Running),
    Failed(Failed),
}

impl Service {
    pub fn new(config: Option<Syncthing>) -> Self {
        match config {
            Some(config) if config.binary.is_file() => Service::Stopped(Stopped(config)),
            _ => Service::NotInstalled,
        }
    }

    /// The Settings switch: starts a stopped/failed service, stops a running one.
    pub fn toggled(self) -> Self {
        match self {
            Service::NotInstalled => Service::NotInstalled,
            Service::Stopped(stopped) => stopped.start(),
            Service::Failed(failed) => failed.retry(),
            Service::Running(running) => Service::Stopped(running.stop()),
        }
    }

    /// Notice a crash; called from a slow UI timer.
    pub fn polled(self) -> Self {
        match self {
            Service::Running(running) => running.poll(),
            other => other,
        }
    }
}

impl Stopped {
    pub fn start(self) -> Service {
        let Stopped(config) = self;
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(config.home.join("kindil-syncthing.log"));
        let mut command = Command::new(&config.binary);
        command
            .args(["serve", "--no-browser", "--no-restart"])
            .arg(format!("--home={}", config.home.display()))
            .env("GOMAXPROCS", "1")
            .env("GOGC", "50")
            .env("STNOUPGRADE", "1")
            .stdin(Stdio::null());
        if let Ok(log) = log.and_then(|f| Ok((f.try_clone()?, f))) {
            command.stdout(log.0).stderr(log.1);
        }
        // SAFETY: nice(2) is async-signal-safe; it only lowers this child's priority
        // so page turns and the dashboard stay responsive while it hashes files.
        unsafe {
            command.pre_exec(|| {
                libc::nice(10);
                Ok(())
            });
        }
        match command.spawn() {
            Ok(child) => Service::Running(Running {
                config,
                child,
                since: Instant::now(),
            }),
            Err(e) => Service::Failed(Failed {
                config,
                reason: e.to_string(),
            }),
        }
    }
}

impl Running {
    pub fn stop(self) -> Stopped {
        let Running {
            config, mut child, ..
        } = self;
        // SIGTERM lets Syncthing close its database; reap off the UI thread.
        // SAFETY: plain kill(2) on our own child's pid.
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
        std::thread::spawn(move || child.wait());
        Stopped(config)
    }

    fn poll(mut self) -> Service {
        match self.child.try_wait() {
            Ok(None) => Service::Running(self),
            Ok(Some(status)) => Service::Failed(Failed {
                config: self.config,
                reason: format!("exited ({status})"),
            }),
            Err(e) => Service::Failed(Failed {
                config: self.config,
                reason: e.to_string(),
            }),
        }
    }
}

impl Failed {
    pub fn retry(self) -> Service {
        Stopped(self.config).start()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_binary_is_not_installed() {
        let config = Syncthing {
            binary: "/nonexistent/syncthing".into(),
            home: "/tmp".into(),
        };
        assert!(matches!(Service::new(Some(config)), Service::NotInstalled));
        assert!(matches!(
            Service::new(None).toggled(),
            Service::NotInstalled
        ));
    }

    #[test]
    fn a_crash_is_reported_and_retried() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        // Exits immediately whatever the arguments: a stand-in crash.
        let binary = home.path().join("crash");
        std::fs::write(&binary, "#!/bin/sh\nexit 3\n").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let config = Syncthing {
            binary,
            home: home.path().into(),
        };
        let started = Service::new(Some(config)).toggled();
        assert!(matches!(started, Service::Running(_)));
        std::thread::sleep(std::time::Duration::from_millis(300));
        let polled = started.polled();
        let Service::Failed(failed) = &polled else {
            panic!("crash not noticed")
        };
        assert!(failed.reason.contains("exited"));
        assert!(matches!(polled.toggled(), Service::Running(_)));
    }
}
