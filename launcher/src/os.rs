//! What the launcher does to the device beyond files, behind a trait so the
//! tests can play the device: upstart's jobs, running kindil, signals.
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

pub trait Os: Sync {
    /// Starting or running, by upstart's goal.
    fn running(&self, job: &str) -> bool;
    fn start(&self, job: &str);
    fn stop(&self, job: &str);
    fn sleep(&self, time: Duration);
    /// Seconds on the wall clock.
    fn now(&self) -> u64;
    /// Runs `binary` with `args` to its end; its exit code, 128 + the
    /// signal if one killed it.
    fn run(&self, binary: &Path, args: &[&str]) -> i32;
    /// `binary --version`, if it runs.
    fn version(&self, binary: &Path) -> Option<String>;
    /// `lipc-set-prop SERVICE PROPERTY VALUE`.
    fn lipc_set(&self, service: &str, property: &str, value: &str);
    /// Clears the e-ink panel.
    fn clear_screen(&self);
    /// Ends the process `pid_file` names and confirms it is gone: TERM,
    /// then KILL after 3 s. Removes the file.
    fn end_pid_file(&self, pid_file: &Path);
    /// Kills lipc-wait-event processes an earlier kindil left to init.
    fn kill_orphaned_watchers(&self);
    /// Whether the process `pid` is alive.
    fn alive(&self, pid: i32) -> bool;
    /// Starts `launcher session` detached, its output appended to `log`;
    /// its pid.
    fn spawn_session(&self, log: &Path) -> Option<i32>;
    /// Whether /proc/modules lists `module`.
    fn module_loaded(&self, module: &str) -> bool;
    /// `modprobe -r MODULE`.
    fn unload_module(&self, module: &str);
}

/// The PW1 itself.
pub struct Device;

const PATH: &str = "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

fn quiet(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .env("PATH", PATH)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn kill(pid: i32, signal: i32) {
    // SAFETY: plain kill(2).
    unsafe { libc::kill(pid, signal) };
}

/// Runs `spawn` again while it fails with ETXTBSY: a file just written can
/// stay open a moment in a child another thread forked and hasn't yet
/// exec'd (the tests write and run scripts in parallel).
pub fn retrying<T>(mut spawn: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut tries = 0;
    loop {
        match spawn() {
            Err(error) if error.raw_os_error() == Some(libc::ETXTBSY) && tries < 50 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            result => return result,
        }
    }
}

/// Sets `signal` to its default in a child about to exec: the launcher
/// ignores some that kindil must get.
fn default_signals(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: signal(2) is async-signal-safe.
    unsafe {
        command.pre_exec(|| {
            libc::signal(libc::SIGTERM, libc::SIG_DFL);
            libc::signal(libc::SIGHUP, libc::SIG_DFL);
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            Ok(())
        });
    }
}

impl Os for Device {
    fn running(&self, job: &str) -> bool {
        Command::new("/sbin/status")
            .arg(job)
            .stderr(Stdio::null())
            .output()
            .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains("start/"))
    }

    fn start(&self, job: &str) {
        quiet("/sbin/start", &[job]);
    }

    fn stop(&self, job: &str) {
        quiet("/sbin/stop", &[job]);
    }

    fn sleep(&self, time: Duration) {
        std::thread::sleep(time);
    }

    fn now(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }

    fn run(&self, binary: &Path, args: &[&str]) -> i32 {
        use std::os::unix::process::ExitStatusExt;
        let mut command = Command::new(binary);
        command.args(args).env("PATH", PATH).stdin(Stdio::null());
        default_signals(&mut command);
        match retrying(|| command.status()) {
            Ok(status) => status
                .code()
                .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)),
            Err(_) => 127,
        }
    }

    fn version(&self, binary: &Path) -> Option<String> {
        version(binary)
    }

    fn lipc_set(&self, service: &str, property: &str, value: &str) {
        quiet("lipc-set-prop", &[service, property, value]);
    }

    fn clear_screen(&self) {
        quiet("eips", &["-c"]);
    }

    fn end_pid_file(&self, pid_file: &Path) {
        let pid = read_pid(pid_file);
        let _ = std::fs::remove_file(pid_file);
        let Some(pid) = pid.filter(|&pid| self.alive(pid)) else {
            return;
        };
        kill(pid, libc::SIGTERM);
        for _ in 0..30 {
            if !self.alive(pid) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        kill(pid, libc::SIGKILL);
    }

    fn kill_orphaned_watchers(&self) {
        for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<i32>().ok())
            else {
                continue;
            };
            let stat = std::fs::read_to_string(entry.path().join("stat")).unwrap_or_default();
            // "pid (comm) state ppid ..."; they ignore TERM.
            let ppid = stat
                .rsplit_once(") ")
                .and_then(|(_, rest)| rest.split_whitespace().nth(1));
            if stat.contains("(lipc-wait-event)") && ppid == Some("1") {
                kill(pid, libc::SIGKILL);
            }
        }
    }

    fn alive(&self, pid: i32) -> bool {
        // SAFETY: signal 0 only checks that the process exists.
        pid > 0 && unsafe { libc::kill(pid, 0) == 0 }
    }

    fn spawn_session(&self, log: &Path) -> Option<i32> {
        use std::os::unix::process::CommandExt;
        let exe = std::env::current_exe().ok()?;
        let out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .ok()?;
        let err = out.try_clone().ok()?;
        let mut command = Command::new(exe);
        command
            .arg("session")
            .env("PATH", PATH)
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err);
        // Its own session: KUAL returns at once, and nothing of KUAL's
        // process tree takes it down.
        // SAFETY: setsid(2) is async-signal-safe.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let child = command.spawn().ok()?;
        i32::try_from(child.id()).ok()
    }

    fn module_loaded(&self, module: &str) -> bool {
        std::fs::read_to_string("/proc/modules").is_ok_and(|text| {
            text.lines()
                .any(|line| line.split_whitespace().next() == Some(module))
        })
    }

    fn unload_module(&self, module: &str) {
        quiet("modprobe", &["-r", module]);
    }
}

pub fn version(binary: &Path) -> Option<String> {
    let out = retrying(|| {
        Command::new(binary)
            .arg("--version")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
    })
    .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

pub fn read_pid(pid_file: &Path) -> Option<i32> {
    std::fs::read_to_string(pid_file)
        .ok()?
        .trim()
        .parse::<i32>()
        .ok()
        .filter(|&pid| pid > 0)
}

/// Ignores `signal` in the launcher itself.
pub fn ignore(signal: i32) {
    // SAFETY: signal(2) with SIG_IGN.
    unsafe { libc::signal(signal, libc::SIG_IGN) };
}

/// A timestamped line on stdout (`start` points it at kindil.log), in the
/// device's time zone.
pub fn log(message: &str) {
    println!("{} {message}", local_time());
}

/// From the system's `date`: musl reads the PW1's zone file (TZif with an
/// empty rule for later times) as UTC. UTC if `date` won't say.
fn local_time() -> String {
    Command::new("date")
        .arg("+%F %T")
        .env("PATH", PATH)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .filter(|time| !time.is_empty())
        .unwrap_or_else(utc_time)
}

fn utc_time() -> String {
    // SAFETY: time(2) and localtime_r(3) with valid pointers.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::gmtime_r(&now, &mut tm);
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}

/// A device for the tests: upstart's jobs in a set, everything done
/// recorded; binaries are real scripts, run for real.
#[cfg(test)]
pub mod fake {
    use super::*;
    use std::collections::BTreeSet;
    use std::sync::Mutex;

    /// The pid a started session gets while kindil stays up.
    pub const UP: i32 = 4242;
    /// The pid of one that didn't.
    pub const GONE: i32 = 999_999;

    #[derive(Default)]
    pub struct Fake {
        pub jobs: Mutex<BTreeSet<String>>,
        /// "stop job", "start job" and "kindil runs", in order.
        pub upstart: Mutex<Vec<String>>,
        /// Whether a started kindil stays up.
        pub up: Mutex<bool>,
        pub sessions: Mutex<u32>,
        /// The jobs running when kindil ran, once the stopper had a go.
        pub during: Mutex<Option<BTreeSet<String>>>,
        /// The pid files ended.
        pub ended: Mutex<Vec<std::path::PathBuf>>,
        /// At boot: Amazon's UI comes up while the launcher waits, if X
        /// runs to bring it.
        pub gui_coming: Mutex<bool>,
        pub modules: Mutex<BTreeSet<String>>,
    }

    impl Fake {
        pub fn with_jobs(jobs: &[&str]) -> Self {
            let fake = Self::default();
            fake.set_jobs(jobs);
            *fake.up.lock().unwrap() = true;
            fake
        }

        pub fn set_jobs(&self, jobs: &[&str]) {
            *self.jobs.lock().unwrap() = jobs.iter().map(|j| (*j).to_owned()).collect();
        }

        pub fn jobs(&self) -> BTreeSet<String> {
            self.jobs.lock().unwrap().clone()
        }

        pub fn upstart(&self) -> String {
            self.upstart
                .lock()
                .unwrap()
                .iter()
                .map(|line| format!("{line},"))
                .collect()
        }

        pub fn clear_upstart(&self) {
            self.upstart.lock().unwrap().clear();
        }
    }

    impl Os for Fake {
        fn running(&self, job: &str) -> bool {
            self.jobs.lock().unwrap().contains(job)
        }

        fn start(&self, job: &str) {
            self.upstart.lock().unwrap().push(format!("start {job}"));
            let mut jobs = self.jobs.lock().unwrap();
            jobs.insert(job.to_owned());
            // Starting X brings Amazon's UI up with it, as upstart does;
            // volumd brings USBNetwork's job (`start on started volumd`).
            if job == "x" {
                jobs.insert("lab126_gui".into());
            }
            if job == "volumd" {
                jobs.insert("usbnet".into());
            }
        }

        fn stop(&self, job: &str) {
            self.upstart.lock().unwrap().push(format!("stop {job}"));
            let mut jobs = self.jobs.lock().unwrap();
            jobs.remove(job);
            // `stop on stopping volumd`.
            if job == "volumd" {
                jobs.remove("usbnet");
                jobs.remove("usbnet-autostart");
            }
        }

        fn sleep(&self, _: Duration) {
            let mut jobs = self.jobs.lock().unwrap();
            if *self.gui_coming.lock().unwrap() && jobs.contains("x") {
                jobs.insert("lab126_gui".into());
            }
        }

        fn now(&self) -> u64 {
            Device.now()
        }

        fn run(&self, binary: &Path, args: &[&str]) -> i32 {
            self.upstart.lock().unwrap().push("kindil runs".into());
            // Gives the background stopper its first round.
            let since = std::time::Instant::now();
            while since.elapsed() < Duration::from_secs(2)
                && crate::SERVICES.iter().any(|job| self.running(job))
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            *self.during.lock().unwrap() = Some(self.jobs());
            Device.run(binary, args)
        }

        fn version(&self, binary: &Path) -> Option<String> {
            version(binary)
        }

        fn lipc_set(&self, _: &str, _: &str, _: &str) {}

        fn clear_screen(&self) {}

        fn end_pid_file(&self, pid_file: &Path) {
            if pid_file.exists() {
                self.ended.lock().unwrap().push(pid_file.to_owned());
            }
            let _ = std::fs::remove_file(pid_file);
        }

        fn kill_orphaned_watchers(&self) {}

        fn alive(&self, pid: i32) -> bool {
            pid == UP
        }

        fn spawn_session(&self, _: &Path) -> Option<i32> {
            *self.sessions.lock().unwrap() += 1;
            Some(if *self.up.lock().unwrap() { UP } else { GONE })
        }

        fn module_loaded(&self, module: &str) -> bool {
            self.modules.lock().unwrap().contains(module)
        }

        fn unload_module(&self, module: &str) {
            self.upstart.lock().unwrap().push(format!("rmmod {module}"));
            self.modules.lock().unwrap().remove(module);
        }
    }
}
