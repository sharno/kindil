//! `launcher session`: runs kindil as an app on the stock OS. Amazon's UI,
//! X, powerd and the services kindil has no use for step aside while it runs
//! and always come back afterwards, whether kindil exits, crashes or is
//! killed. Nothing here survives a reboot, so a reboot is always a way back,
//! except for a standalone start at boot, whose own guard is `launcher boot`.
//!
//! The launcher itself must not die before it restores: it ignores HUP and
//! TERM (stopping Amazon's UI signals its whole tree), and never panics
//! (release builds abort).
use crate::os::{log, Os};
use crate::{Paths, SERVICES};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, UNIX_EPOCH};

/// kindil's exit code after staging an update: run again.
const RESTART: i32 = 75;
/// A crash restarts kindil, unless it crashed this often within WINDOW
/// seconds: then that version is broken, and the one before it (kept by the
/// last update) gets the same chances before Amazon's UI comes back.
const CRASHES: usize = 3;
const WINDOW: u64 = 300;
/// The background stopper's rounds: for a minute, since at boot some
/// services may still be on their way up.
const STOP_ROUNDS: u32 = 12;
const STOP_EVERY: Duration = Duration::from_secs(5);

struct Files {
    /// The copy that runs, on the system storage: in USB drive mode the
    /// computer gets /mnt/us while kindil keeps running.
    bin: PathBuf,
    /// The source `bin` was copied from (see [`current`]).
    from: PathBuf,
    /// Present while a new version is on trial; kindil removes it once it
    /// has run for two minutes (src/update.rs). Still present after it
    /// exits: roll back.
    trying: PathBuf,
    /// A staged update (written by kindil after checking its signature),
    /// and the same while on trial.
    next: PathBuf,
    trial: PathBuf,
    installed: PathBuf,
    previous: PathBuf,
}

impl Files {
    fn new(paths: &Paths) -> Self {
        Self {
            bin: paths.run.join("kindil"),
            from: paths.run.join("kindil.from"),
            trying: paths.run.join("trying"),
            next: paths.ext.join("kindil.next"),
            trial: paths.ext.join("kindil.trying"),
            installed: paths.ext.join("kindil"),
            previous: paths.ext.join("kindil.previous"),
        }
    }

    /// Copies the binary to run: a staged update gets one trial run;
    /// otherwise `slot`, the current binary or the previous one.
    fn pick(&self, slot: &Path) {
        if self.next.is_file() && std::fs::rename(&self.next, &self.trial).is_ok() {
            if self.copy(&self.trial).is_ok() && std::fs::write(&self.trying, b"").is_ok() {
                log("trying the update");
                return;
            }
            let _ = std::fs::remove_file(&self.trial);
        }
        if slot.is_file() && !self.current(slot) {
            if let Err(error) = self.copy(slot) {
                log(&format!("copying {}: {error}", slot.display()));
            }
        }
    }

    /// Comparing the 16 MB binaries takes over 10 s at boot. Instead the copy
    /// records its source and has the source's modification time, set before
    /// the record is written: the same source with the same time means the
    /// copy is current.
    fn current(&self, source: &Path) -> bool {
        std::fs::read_to_string(&self.from).is_ok_and(|from| Path::new(from.trim()) == source)
            && mtime(source).is_some()
            && mtime(source) == mtime(&self.bin)
    }

    /// In place, never beside: /var/local is 64 MB, mostly Amazon's, with
    /// room for one copy of kindil, not two. The old copy is unlinked first
    /// (its space frees once nothing runs it) and the new one written fresh.
    /// Unlinking, not truncating, also keeps this clear of ETXTBSY, which
    /// the earlier write-beside-and-rename avoided: nothing runs the old
    /// copy by then anyway, since kindil has exited and its Wi-Fi children
    /// (whose udhcpc runs the hook from it) are ended before every pick.
    /// The record of the source comes last, so a copy cut short (a full
    /// disk, a power cut) never counts as current; a failed one is removed.
    fn copy(&self, source: &Path) -> std::io::Result<()> {
        let _ = std::fs::remove_file(&self.from);
        // Left by older launchers, which copied beside.
        let _ = std::fs::remove_file(self.bin.with_extension("part"));
        match std::fs::remove_file(&self.bin) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        let written = self.write(source);
        if written.is_err() {
            let _ = std::fs::remove_file(&self.bin);
        }
        written?;
        std::fs::write(&self.from, format!("{}\n", source.display()))
    }

    fn write(&self, source: &Path) -> std::io::Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let mut from = std::fs::File::open(source)?;
        let modified = from.metadata()?.modified()?;
        let mut to = std::fs::File::options()
            .write(true)
            .create_new(true)
            .mode(0o755)
            .open(&self.bin)?;
        std::io::copy(&mut from, &mut to)?;
        to.sync_all()?;
        to.set_modified(modified)?;
        // The mode given at creation passes through the umask.
        to.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o755))
    }

    fn drop_trial(&self) {
        let _ = std::fs::remove_file(&self.trying);
        let _ = std::fs::remove_file(&self.trial);
    }
}

/// In whole seconds: the user storage keeps 2 s, the system storage 1 s.
fn mtime(path: &Path) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(modified.duration_since(UNIX_EPOCH).ok()?.as_secs())
}

fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

pub fn session(paths: &Paths, os: &dyn Os) -> ExitCode {
    crate::os::ignore(libc::SIGHUP);
    let files = Files::new(paths);
    // A trial cut short by a reboot or a kill never settled: drop it, so the
    // current binary runs and nothing keeps the untried one.
    if files.trying.exists() {
        files.drop_trial();
        log("dropped an update whose trial was cut short");
    }
    // Event watchers an earlier kindil left behind (orphans, now init's);
    // they ignore TERM.
    os.kill_orphaned_watchers();
    end_children(paths, os);
    let slot = files.installed.clone();
    files.pick(&slot);
    if !executable(&files.bin) {
        log(&format!("missing {}", files.bin.display()));
        return ExitCode::FAILURE;
    }
    // Stopping the GUI job signals its whole tree, which can include us.
    crate::os::ignore(libc::SIGTERM);
    // A standalone boot never started it.
    let gui = os.running("lab126_gui");
    if gui {
        // Let KUAL close its menu before the UI goes away.
        os.sleep(Duration::from_secs(2));
        log("stopping the Kindle UI");
        os.stop("lab126_gui");
        os.sleep(Duration::from_secs(2));
    }
    // kindil draws to the framebuffer itself: X has nothing to do while it
    // runs. The way back starts it again, and Amazon's UI with it.
    if os.running("x") {
        os.stop("x");
    }
    // kindil does powerd's work itself: battery, front light, charging
    // limits, sleep and the power button. Stopped first, and before kindil
    // starts: stopping powerd switches the front light off, which kindil
    // then sets.
    if os.running("powerd") {
        log("stopping powerd");
        os.stop("powerd");
    }
    std::thread::scope(|scope| {
        // The rest in the background, so kindil starts at once. Ended before
        // anything is brought back, so the two never race. Through Builder:
        // scope.spawn panics if no thread can be made; then one round now.
        let (done, rounds) = mpsc::channel::<()>();
        let stopper = std::thread::Builder::new()
            .spawn_scoped(scope, move || stop_services(os, &rounds, STOP_ROUNDS))
            .map_err(|error| {
                log(&format!(
                    "no thread for the stopper ({error}); stopping once"
                ));
                stop_services(os, &mpsc::channel().1, 1);
            });
        // Amazon's screen modules would draw over kindil in USB drive mode
        // and after it; kindil draws its own screens. Only Amazon's UI loads
        // them.
        if gui {
            os.lipc_set("com.lab126.blanket", "unload", "usb");
            os.lipc_set("com.lab126.blanket", "unload", "splash");
        }
        os.clear_screen();
        run(paths, os, &files, slot);
        drop(done);
        if let Ok(stopper) = stopper {
            let _ = stopper.join();
        }
        restore(paths, os, gui);
    });
    ExitCode::SUCCESS
}

/// Up to `rounds` rounds STOP_EVERY apart, until `done` ends.
fn stop_services(os: &dyn Os, done: &mpsc::Receiver<()>, rounds: u32) {
    for round in 1..=rounds {
        for job in SERVICES {
            if os.running(job) {
                os.stop(job);
            }
        }
        if round == rounds || done.recv_timeout(STOP_EVERY) != Err(mpsc::RecvTimeoutError::Timeout)
        {
            return;
        }
    }
}

/// Runs kindil until it exits for good.
fn run(paths: &Paths, os: &dyn Os, files: &Files, mut slot: PathBuf) {
    let config = paths.config.to_string_lossy().into_owned();
    let args: Vec<&str> = if paths.config.is_file() {
        vec!["--config", &config, "kindle"]
    } else {
        vec!["kindle"]
    };
    let mut crashes: Vec<u64> = Vec::new();
    loop {
        log(&os.version(&files.bin).unwrap_or_else(|| "kindil ?".into()));
        let status = os.run(&files.bin, &args);
        log(&format!("kindil exited ({status})"));
        if files.trying.exists() {
            // The trial ended before kindil kept it: back to the previous
            // binary.
            files.drop_trial();
            log("the update ended early; back to the previous version");
            if status == 0 {
                break;
            }
        } else {
            match status {
                RESTART => {}
                // Exit to Kindle, or stopped on purpose (Ctrl-C, kill).
                0 | 130 | 143 => break,
                _ => {
                    let now = os.now();
                    crashes.retain(|&t| now.saturating_sub(t) < WINDOW);
                    crashes.push(now);
                    if crashes.len() < CRASHES {
                        log("restarting after a crash");
                    } else if slot == files.installed && files.previous.is_file() {
                        slot = files.previous.clone();
                        crashes.clear();
                        log(&format!(
                            "crashed {CRASHES} times in {WINDOW} s; trying the previous version"
                        ));
                    } else {
                        log(&format!("crashed {CRASHES} times in {WINDOW} s; giving up"));
                        break;
                    }
                    os.sleep(Duration::from_secs(2));
                }
            }
        }
        end_children(paths, os);
        files.pick(&slot);
    }
}

/// kindil's own children (Wi-Fi's, its SSH server) from a run that crashed
/// or was killed: stopped before it runs again, which starts them afresh,
/// and before its binary is replaced, which udhcpc runs as its hook.
fn end_children(paths: &Paths, os: &dyn Os) {
    for pid_file in &paths.child_pids {
        os.end_pid_file(pid_file);
    }
}

/// Amazon's UI back, with everything it needs.
fn restore(paths: &Paths, os: &dyn Os, gui: bool) {
    // A standalone boot skipped Amazon's framework: without it, Amazon's UI
    // is an empty screen. `launcher boot` sets the flag again after a boot
    // kindil stays up.
    let _ = std::fs::remove_file(&paths.skip);
    // kindil may have paused charging for the temperature (src/power.rs);
    // powerd, back now, keeps its own range.
    let _ = std::fs::write(&paths.charger, "1");
    if !os.running("powerd") {
        os.start("powerd");
    }
    if gui {
        os.lipc_set("com.lab126.blanket", "load", "usb");
        os.lipc_set("com.lab126.blanket", "load", "splash");
    }
    log("starting the Kindle UI");
    wifi_back(paths, os);
    // kindil, even one that crashed, may have let its SSH port in.
    os.close_port(crate::SSH_PORT);
    for job in SERVICES {
        if !os.running(job) {
            os.start(job);
        }
    }
    if os.running("x") {
        os.start("lab126_gui");
    } else {
        os.start("x");
    }
    let _ = std::fs::remove_file(&paths.pid_file);
}

/// kindil ran Wi-Fi itself (src/net.rs): its DHCP client and wpa_supplicant
/// go, and so does its SSH server (src/ssh.rs) with the sessions open over
/// it, and wifid starts Amazon's supplicant afresh. The driver stays loaded:
/// unloading it can hang, and wifid's driver job is fine with it loaded.
fn wifi_back(paths: &Paths, os: &dyn Os) {
    let mut ran = false;
    for pid_file in &paths.child_pids {
        if pid_file.exists() {
            os.end_pid_file(pid_file);
            ran = true;
        }
    }
    if ran && !os.running("wifid") {
        os.stop("wifis");
    }
    if !os.running("wifid") {
        os.start("wifid");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::os::fake::Fake;
    use std::collections::BTreeSet;

    struct Env {
        dir: tempfile::TempDir,
        paths: Paths,
    }

    impl Env {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let t = dir.path();
            let paths = Paths {
                run: t.join("run"),
                ext: t.join("ext"),
                user: t.join("user"),
                config: t.join("user/kindil.toml"),
                flag: t.join("user/BOOT"),
                skip: t.join("SKIP"),
                charger: t.join("charging"),
                child_pids: vec![t.join("udhcpc.pid"), t.join("wpa.pid"), t.join("sshd.pid")],
                pid_file: t.join("kindil.pid"),
                boot_id: t.join("boot_id"),
            };
            std::fs::create_dir_all(&paths.run).unwrap();
            std::fs::create_dir_all(&paths.ext).unwrap();
            Self { dir, paths }
        }

        fn t(&self) -> &Path {
            self.dir.path()
        }

        fn ext(&self, name: &str) -> PathBuf {
            self.paths.ext.join(name)
        }

        /// A binary that reports `version`, then runs `body`.
        fn fake(&self, path: &Path, version: &str, body: &str) {
            use std::os::unix::fs::PermissionsExt;
            let script = format!(
                "#!/bin/sh\n[ \"$1\" = --version ] && {{ echo \"kindil {version}\"; exit 0; }}\n{body}\n"
            );
            std::fs::write(path, script).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        /// What kindil does once a trial has run for two minutes.
        fn commit(&self) -> String {
            let (e, r) = (self.paths.ext.display(), self.paths.run.display());
            format!(
                "mv {e}/kindil {e}/kindil.previous; mv {e}/kindil.trying {e}/kindil; rm {r}/trying"
            )
        }

        fn files(&self) -> String {
            let mut names: Vec<String> = std::fs::read_dir(&self.paths.ext)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names.join(" ")
        }

        fn version(&self, path: &Path) -> String {
            crate::os::version(path).unwrap_or_default()
        }

        fn running(&self) -> String {
            self.version(&self.paths.run.join("kindil"))
        }

        fn runs(&self) -> String {
            std::fs::read_to_string(self.t().join("runs"))
                .unwrap_or_default()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        }

        fn session(&self, os: &Fake) -> ExitCode {
            session(&self.paths, os)
        }
    }

    fn set(jobs: &[&str]) -> BTreeSet<String> {
        jobs.iter().map(|j| (*j).to_owned()).collect()
    }

    /// Started from KUAL: Amazon's services, X and Amazon's UI are up.
    fn kual() -> Fake {
        let mut jobs: Vec<&str> = SERVICES.to_vec();
        jobs.extend(["powerd", "wifid", "x", "lab126_gui"]);
        Fake::with_jobs(&jobs)
    }

    #[test]
    fn a_crashing_update_rolls_back() {
        let env = Env::new();
        env.fake(&env.ext("kindil"), "1.0.0", "exit 0");
        env.fake(&env.ext("kindil.next"), "2.0.0", "exit 139");
        env.session(&Fake::default());
        assert_eq!(env.files(), "kindil");
        assert_eq!(env.running(), "kindil 1.0.0");
    }

    #[test]
    fn a_settled_update_is_kept() {
        let env = Env::new();
        env.fake(&env.ext("kindil"), "1.0.0", "exit 0");
        env.fake(
            &env.ext("kindil.next"),
            "2.0.0",
            &format!("{}; exit 0", env.commit()),
        );
        env.session(&Fake::default());
        assert_eq!(env.files(), "kindil kindil.previous");
        assert_eq!(env.version(&env.ext("kindil")), "kindil 2.0.0");
    }

    #[test]
    fn exit_75_restarts_into_the_staged_update() {
        let env = Env::new();
        let new = env.t().join("new");
        env.fake(&new, "2.0.0", &format!("{}; exit 0", env.commit()));
        let staged = env.t().join("staged");
        env.fake(
            &env.ext("kindil"),
            "1.0.0",
            &format!(
                "[ -f {s} ] && exit 0; touch {s}; cp {n} {e}; exit 75",
                s = staged.display(),
                n = new.display(),
                e = env.ext("kindil.next").display()
            ),
        );
        env.session(&Fake::default());
        assert_eq!(env.version(&env.ext("kindil")), "kindil 2.0.0");
    }

    #[test]
    fn a_trial_cut_short_by_a_reboot_is_dropped() {
        let env = Env::new();
        env.fake(&env.ext("kindil"), "1.0.0", "exit 0");
        env.fake(&env.ext("kindil.trying"), "2.0.0", "exit 0");
        std::fs::write(env.paths.run.join("trying"), "").unwrap();
        env.session(&Fake::default());
        assert_eq!(env.files(), "kindil");
        assert_eq!(env.running(), "kindil 1.0.0");
    }

    fn counting(env: &Env, word: &str, then: &str) -> String {
        format!("echo {word} >>{}; {then}", env.t().join("runs").display())
    }

    #[test]
    fn a_crash_restarts_kindil() {
        let env = Env::new();
        let runs = env.t().join("runs");
        let body = counting(
            &env,
            "ran",
            &format!("[ $(wc -l <{}) -ge 2 ] && exit 0; exit 101", runs.display()),
        );
        env.fake(&env.ext("kindil"), "1.0.0", &body);
        env.session(&Fake::default());
        assert_eq!(env.runs(), "ran ran");
    }

    #[test]
    fn crashing_over_and_over_returns_to_amazons_ui() {
        let env = Env::new();
        env.fake(
            &env.ext("kindil"),
            "1.0.0",
            &counting(&env, "ran", "exit 139"),
        );
        env.session(&Fake::default());
        assert_eq!(env.runs(), "ran ran ran");
    }

    #[test]
    fn a_version_that_keeps_crashing_falls_back_to_the_previous_one() {
        let env = Env::new();
        env.fake(
            &env.ext("kindil"),
            "2.0.0",
            &counting(&env, "ran", "exit 139"),
        );
        env.fake(
            &env.ext("kindil.previous"),
            "1.0.0",
            &counting(&env, "previous", "exit 0"),
        );
        env.session(&Fake::default());
        assert_eq!(env.runs(), "ran ran ran previous");
        assert_eq!(
            env.files(),
            "kindil kindil.previous",
            "the slots on disk stay"
        );
    }

    #[test]
    fn both_versions_crashing_returns_to_amazons_ui() {
        let env = Env::new();
        env.fake(
            &env.ext("kindil"),
            "2.0.0",
            &counting(&env, "ran", "exit 139"),
        );
        env.fake(
            &env.ext("kindil.previous"),
            "1.0.0",
            &counting(&env, "ran", "exit 139"),
        );
        env.session(&Fake::default());
        assert_eq!(env.runs().split(' ').count(), 6);
    }

    #[test]
    fn a_kill_returns_to_amazons_ui() {
        let env = Env::new();
        env.fake(
            &env.ext("kindil"),
            "1.0.0",
            &counting(&env, "ran", "kill -TERM $$"),
        );
        env.session(&Fake::default());
        assert_eq!(env.runs(), "ran");
    }

    #[test]
    fn leaving_kindil_brings_amazons_framework_back() {
        let env = Env::new();
        std::fs::write(&env.paths.skip, "").unwrap();
        env.fake(&env.ext("kindil"), "1.0.0", "exit 0");
        env.session(&Fake::default());
        assert!(!env.paths.skip.exists());
        assert_eq!(
            std::fs::read_to_string(&env.paths.charger).unwrap(),
            "1",
            "charging on"
        );
    }

    #[test]
    fn an_unchanged_binary_isnt_copied_again_and_a_replaced_one_is() {
        let env = Env::new();
        env.fake(&env.ext("kindil"), "1.0.0", "exit 0");
        env.session(&Fake::default());
        // Same time, different content: only a copy would bring the content
        // back.
        let bin = env.paths.run.join("kindil");
        let modified = std::fs::metadata(&bin).unwrap().modified().unwrap();
        std::fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&bin)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        env.session(&Fake::default());
        assert_eq!(
            std::fs::read_to_string(&bin).unwrap(),
            "#!/bin/sh\nexit 0\n"
        );
        env.fake(&env.ext("kindil"), "1.1.0", "exit 0");
        let later = std::time::SystemTime::now() + Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(env.ext("kindil"))
            .unwrap()
            .set_modified(later)
            .unwrap();
        env.session(&Fake::default());
        assert_eq!(env.running(), "kindil 1.1.0");
    }

    #[test]
    fn a_failed_copy_leaves_nothing_behind_and_isnt_current() {
        let env = Env::new();
        let files = Files::new(&env.paths);
        env.fake(&env.ext("kindil"), "1.0.0", "exit 0");
        files.copy(&env.ext("kindil")).unwrap();
        assert!(files.current(&env.ext("kindil")));
        // A directory opens but won't read: the copy fails once begun.
        let broken = env.ext("broken");
        std::fs::create_dir(&broken).unwrap();
        assert!(files.copy(&broken).is_err());
        assert!(!files.bin.exists() && !files.from.exists());
        assert!(!env.paths.run.join("kindil.part").exists());
        assert!(!files.current(&broken) && !files.current(&env.ext("kindil")));
        // The next pick copies afresh.
        files.pick(&env.ext("kindil"));
        assert_eq!(env.running(), "kindil 1.0.0");
    }

    #[test]
    fn a_copy_left_beside_by_an_older_launcher_goes() {
        let env = Env::new();
        let part = env.paths.run.join("kindil.part");
        std::fs::write(&part, "half a binary").unwrap();
        env.fake(&env.ext("kindil"), "1.0.0", "exit 0");
        env.session(&Fake::default());
        assert!(!part.exists());
        assert_eq!(env.running(), "kindil 1.0.0");
    }

    #[test]
    fn the_wifi_children_end_before_the_binary_they_run_is_replaced() {
        let env = Env::new();
        let wpa = &env.paths.child_pids[1];
        std::fs::write(wpa, "9\n").unwrap();
        env.fake(&env.ext("kindil"), "1.0.0", "exit 0");
        let os = Fake::default();
        // Records whether the binary was there when the children ended.
        *os.on_end.lock().unwrap() = Some(env.paths.run.join("kindil"));
        env.session(&os);
        assert_eq!(*os.ended_before_copy.lock().unwrap(), Some(true));
    }

    #[test]
    fn amazons_ui_powerd_and_services_step_aside_while_kindil_runs_and_come_back() {
        let env = Env::new();
        let os = kual();
        let before = os.jobs();
        env.fake(&env.ext("kindil"), "1.0.0", "exit 0");
        env.session(&os);
        assert_eq!(os.during.lock().unwrap().clone().unwrap(), set(&["wifid"]));
        assert_eq!(os.jobs(), before, "every one of them back");
        let upstart = os.upstart.lock().unwrap().clone();
        let at = |line: &str| upstart.iter().position(|l| l == line).unwrap();
        assert!(at("stop powerd") < at("kindil runs"), "powerd stops first");
    }

    #[test]
    fn after_a_standalone_boot_leaving_starts_everything_amazons_ui_needs() {
        let env = Env::new();
        let os = Fake::default();
        env.fake(&env.ext("kindil"), "1.0.0", "exit 0");
        env.session(&os);
        let mut want: Vec<&str> = SERVICES.to_vec();
        want.extend(["powerd", "wifid", "x", "lab126_gui"]);
        assert_eq!(os.jobs(), set(&want));
    }

    #[test]
    fn leaving_after_kindil_ran_wifi_gives_wifid_a_fresh_supplicant() {
        let env = Env::new();
        let os = Fake::default();
        let [dhcp, wpa] = [&env.paths.child_pids[0], &env.paths.child_pids[1]];
        let body = format!(
            "echo 1 >{}; echo 2 >{}; exit 0",
            dhcp.display(),
            wpa.display()
        );
        env.fake(&env.ext("kindil"), "1.0.0", &body);
        env.session(&os);
        assert_eq!(
            os.upstart
                .lock()
                .unwrap()
                .iter()
                .filter(|l| *l == "stop wifis")
                .count(),
            1
        );
        assert!(os.running("wifid"));
        assert!(!wpa.exists() && !dhcp.exists());
        // And kindil's SSH port is shut again.
        assert_eq!(*os.closed_ports.lock().unwrap(), [2222]);
    }

    #[test]
    fn a_crashed_kindils_ssh_port_is_shut_on_the_way_out() {
        let env = Env::new();
        let os = Fake::default();
        env.fake(&env.ext("kindil"), "1.0.0", "kill -SEGV $$");
        env.session(&os);
        assert!(os.closed_ports.lock().unwrap().contains(&crate::SSH_PORT));
    }

    #[test]
    fn a_run_after_a_crash_doesnt_find_the_crashed_runs_wifi_children() {
        let env = Env::new();
        let (runs, wpa) = (env.t().join("runs"), &env.paths.child_pids[1]);
        let body = format!(
            "echo ran >>{r}; [ -f {w} ] && echo found >>{r}; [ $(grep -c ran {r}) -ge 2 ] && exit 0; \
             echo 9 >{w}; exit 139",
            r = runs.display(),
            w = wpa.display()
        );
        env.fake(&env.ext("kindil"), "1.0.0", &body);
        env.session(&Fake::default());
        assert_eq!(env.runs(), "ran ran");
    }

    #[test]
    fn kindils_ssh_server_ends_before_each_run_and_on_the_way_out() {
        let env = Env::new();
        let os = Fake::default();
        let (runs, ssh) = (env.t().join("runs"), &env.paths.child_pids[2]);
        // Each run leaves its SSH server's pid file behind; the first crashes.
        let body = format!(
            "echo ran >>{r}; [ -f {s} ] && echo found >>{r}; echo 7 >{s}; \
             [ $(grep -c ran {r}) -ge 2 ] && exit 0; exit 139",
            r = runs.display(),
            s = ssh.display()
        );
        env.fake(&env.ext("kindil"), "1.0.0", &body);
        env.session(&os);
        assert_eq!(env.runs(), "ran ran");
        assert!(!ssh.exists());
        let ended = os.ended.lock().unwrap();
        assert_eq!(ended.iter().filter(|p| *p == ssh).count(), 2);
    }

    #[test]
    fn with_no_binary_nothing_is_stopped() {
        let env = Env::new();
        let os = kual();
        let before = os.jobs();
        assert_eq!(env.session(&os), ExitCode::FAILURE);
        assert_eq!(os.jobs(), before);
        assert_eq!(os.upstart(), "");
    }
}
