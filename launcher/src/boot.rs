//! `launcher boot`: run by bin/boot.sh from /etc/upstart/kindil.conf once
//! powerd, wifid and volumd start, before X and Amazon's UI (jobs installed
//! before 0.6.2 run it whenever Amazon's UI starts instead). Starts kindil
//! once per boot, only while the BOOT flag exists, and stops trying after
//! boots where kindil didn't stay up: then the Kindle boots to Amazon's UI
//! as before. BOOT holding "standalone" also skips Amazon's UI from the next
//! boot on; any boot where kindil doesn't stay up brings it back.
use crate::os::{self, Os};
use crate::{Paths, SERVICES};
use std::time::Duration;

/// Boots in a row where kindil started but didn't stay up for SETTLE.
const LIMIT: u32 = 2;
const SETTLE: Duration = Duration::from_secs(120);
/// How long a boot may take to bring up what kindil waits for.
const PATIENCE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(500);

fn log(message: &str) {
    os::log(&format!("boot: {message}"));
}

fn read(path: &std::path::Path) -> String {
    std::fs::read_to_string(path)
        .map(|text| text.trim().to_owned())
        .unwrap_or_default()
}

/// Waits until `check` passes, for up to PATIENCE.
fn wait_for(os: &dyn Os, check: impl Fn() -> bool) -> bool {
    let polls = PATIENCE.as_millis() / POLL.as_millis();
    for _ in 0..polls {
        if check() {
            return true;
        }
        os.sleep(POLL);
    }
    check()
}

pub fn boot(paths: &Paths, os: &dyn Os) {
    if !paths.flag.exists() {
        return;
    }
    let _ = std::fs::create_dir_all(&paths.run);
    // The boot kindil last started in: the job running again later in that
    // boot (Amazon's UI coming back, with jobs from before 0.6.2) must not
    // start kindil again.
    let booted = paths.run.join("booted");
    let boot = read(&paths.boot_id);
    if read(&booted) == boot {
        return;
    }
    let _ = std::fs::write(&booted, format!("{boot}\n"));
    let tries_file = paths.run.join("boot-tries");
    let tries: u32 = read(&tries_file).parse().unwrap_or(0);
    if tries >= LIMIT {
        log(&format!(
            "kindil didn't stay up for {LIMIT} boots; staying with Amazon's UI \
             (turn start at boot on again in KUAL)"
        ));
        amazon_ui(paths, os);
        return;
    }
    let _ = std::fs::write(&tries_file, format!("{}\n", tries + 1));
    if paths.skip.exists() {
        // kindil draws to the framebuffer itself: X, about 9 s of the boot,
        // and all that waits for it go unused.
        log("skipping X and Amazon's UI");
        os.stop("x");
        // Amazon's boot spinner stops only when X's language picker starts:
        // left running, it keeps drawing over kindil.
        os.stop("early_progress");
        // The session stops powerd, whose work kindil does: once its job is
        // up, or it would start behind kindil's back.
        if !wait_for(os, || os.running("powerd")) {
            log("powerd didn't start");
        }
    } else {
        // Let Amazon's UI get going; the session stops it again.
        if !wait_for(os, || os.running("lab126_gui")) {
            log("Amazon's UI didn't start");
        }
        os.sleep(Duration::from_secs(10));
    }
    // Nothing runs kindil this early in a boot, and a pid file left by the
    // last boot could name an unrelated process now: start would think
    // kindil runs.
    let _ = std::fs::remove_file(&paths.pid_file);
    log("starting kindil");
    crate::start(paths, os);
    os.sleep(SETTLE);
    if os::read_pid(&paths.pid_file).is_some_and(|pid| os.alive(pid)) {
        let _ = std::fs::remove_file(&tries_file);
        log("kindil is up");
        // Proven on this boot: the next one can skip Amazon's UI.
        if read(&paths.flag) == "standalone" {
            let _ = std::fs::write(&paths.skip, b"");
        }
    } else {
        log("kindil didn't stay up");
        amazon_ui(paths, os);
    }
}

/// A standalone boot has no Amazon UI to fall back on: bring it back with its
/// framework, now and for the next boots.
fn amazon_ui(paths: &Paths, os: &dyn Os) {
    if !paths.skip.exists() {
        return;
    }
    let _ = std::fs::remove_file(&paths.skip);
    log("starting Amazon's UI with its framework");
    // The session brings back what kindil stopped, unless kindil never got
    // far.
    for job in ["powerd", "wifid"].into_iter().chain(SERVICES) {
        if !os.running(job) {
            os.start(job);
        }
    }
    if !os.running("x") {
        // Stopped by a standalone boot: X brings up the rest of Amazon's UI.
        os.start("x");
    } else if os.running("lab126_gui") {
        os.stop("lab126_gui");
        os.start("lab126_gui");
    }
    // Otherwise Amazon's UI is still to come, and sees the flag gone.
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::os::fake::{Fake, UP};
    use std::path::Path;

    struct Env {
        _dir: tempfile::TempDir,
        paths: Paths,
        os: Fake,
    }

    /// A boot as the job finds it: Amazon's services up, X starting,
    /// Amazon's UI still to come.
    fn boot_jobs() -> Vec<&'static str> {
        let mut jobs: Vec<&str> = SERVICES.to_vec();
        jobs.extend(["powerd", "wifid", "x"]);
        jobs
    }

    impl Env {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let t = dir.path();
            let paths = Paths {
                run: t.join("state"),
                ext: t.join("ext"),
                user: t.join("user"),
                config: t.join("user/kindil.toml"),
                flag: t.join("BOOT"),
                skip: t.join("SKIP"),
                charger: t.join("charging"),
                wifi_pids: vec![],
                pid_file: t.join("pid"),
                boot_id: t.join("boot_id"),
            };
            let os = Fake::with_jobs(&boot_jobs());
            *os.gui_coming.lock().unwrap() = true;
            Self {
                _dir: dir,
                paths,
                os,
            }
        }

        fn flag(&self, text: &str) {
            std::fs::write(&self.paths.flag, text).unwrap();
        }

        fn up(&self, up: bool) {
            *self.os.up.lock().unwrap() = up;
        }

        fn boot(&self, id: &str) {
            std::fs::write(&self.paths.boot_id, format!("{id}\n")).unwrap();
            boot(&self.paths, &self.os);
        }

        fn starts(&self) -> u32 {
            *self.os.sessions.lock().unwrap()
        }

        fn tries(&self) -> String {
            let tries = read(&self.paths.run.join("boot-tries"));
            if tries.is_empty() {
                "0".into()
            } else {
                tries
            }
        }

        fn skip(&self) -> &str {
            if self.paths.skip.exists() {
                "skip"
            } else {
                "framework"
            }
        }

        fn touch(&self, path: &Path) {
            std::fs::write(path, "").unwrap();
        }
    }

    #[test]
    fn without_the_flag_nothing_starts() {
        let env = Env::new();
        env.boot("a");
        assert_eq!(env.starts(), 0);
    }

    #[test]
    fn with_the_flag_kindil_starts_once_per_boot() {
        let env = Env::new();
        env.flag("");
        env.boot("a");
        assert_eq!(
            (env.starts(), env.tries()),
            (1, "0".into()),
            "started and settled"
        );
        env.boot("a");
        assert_eq!(
            env.starts(),
            1,
            "Amazon's UI coming back in the same boot starts nothing"
        );
        env.boot("b");
        assert_eq!(env.starts(), 2, "the next boot starts it again");
    }

    #[test]
    fn after_two_boots_kindil_didnt_stay_up_in_the_kindle_boots_to_amazons_ui() {
        let env = Env::new();
        env.flag("");
        env.up(false);
        env.boot("a");
        env.boot("b");
        assert_eq!((env.starts(), env.tries()), (2, "2".into()), "counted");
        env.up(true);
        env.boot("c");
        assert_eq!(env.starts(), 2);
    }

    #[test]
    fn a_stale_pid_file_from_the_last_boot_doesnt_block_the_start() {
        let env = Env::new();
        env.flag("");
        // A pid from the last boot that some other process has now.
        std::fs::write(&env.paths.pid_file, format!("{UP}\n")).unwrap();
        env.boot("a");
        assert_eq!(env.starts(), 1);
    }

    #[test]
    fn standalone_a_boot_kindil_stays_up_in_skips_the_framework_next_time() {
        let env = Env::new();
        env.flag("standalone\n");
        env.boot("a");
        assert_eq!(env.skip(), "skip");
        let env = Env::new();
        env.flag("");
        env.boot("a");
        assert_eq!(
            env.skip(),
            "framework",
            "after Amazon's UI it keeps starting"
        );
    }

    #[test]
    fn standalone_x_and_amazons_ui_never_start_nor_does_the_spinner_stay() {
        let env = Env::new();
        env.flag("standalone");
        env.touch(&env.paths.skip);
        env.boot("a");
        assert_eq!(env.starts(), 1);
        assert_eq!(env.os.upstart(), "stop x,stop early_progress,");
    }

    #[test]
    fn after_amazons_ui_kindil_waits_for_it() {
        let env = Env::new();
        env.flag("");
        env.boot("a");
        assert_eq!(env.starts(), 1);
        assert_eq!(env.os.upstart(), "");
        assert!(env.os.running("lab126_gui"));
    }

    #[test]
    fn a_standalone_boot_kindil_doesnt_stay_up_in_gets_amazons_full_ui() {
        let env = Env::new();
        env.flag("standalone");
        env.touch(&env.paths.skip);
        env.up(false);
        env.boot("a");
        assert_eq!(env.skip(), "framework");
        assert_eq!(env.os.upstart(), "stop x,stop early_progress,start x,");
        env.boot("a");
        assert_eq!(env.starts(), 1, "which starts nothing more in that boot");
        assert_eq!(env.os.upstart(), "stop x,stop early_progress,start x,");
    }

    #[test]
    fn past_the_failed_boot_limit_amazons_ui_comes_with_its_framework() {
        let env = Env::new();
        env.flag("standalone");
        env.up(false);
        env.boot("a");
        env.boot("b");
        env.touch(&env.paths.skip);
        env.os.clear_upstart();
        env.os.set_jobs(&boot_jobs());
        env.boot("c");
        assert_eq!((env.starts(), env.skip()), (2, "framework"));
        assert_eq!(env.os.upstart(), "", "Amazon's UI is still to come");
        // A job from before 0.6.2 runs once Amazon's UI is already up.
        let mut jobs = boot_jobs();
        jobs.push("lab126_gui");
        env.os.set_jobs(&jobs);
        env.touch(&env.paths.skip);
        env.boot("d");
        assert_eq!(env.skip(), "framework");
        assert_eq!(env.os.upstart(), "stop lab126_gui,start lab126_gui,");
    }
}
