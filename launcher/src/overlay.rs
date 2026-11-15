//! Runtime overlays: kindil's replacements for a few of Amazon's files, laid
//! over the originals with read-only bind mounts by `launcher boot`. The
//! system partition never changes: a boot without them is stock.
//!
//! - On only while /mnt/us/kindil/OVERLAYS exists (or the same file on
//!   /mnt/base-us, the storage under /mnt/us's FUSE layer), whatever start
//!   at boot says. Deleting it over USB makes the next boot stock.
//! - A file is overlaid only while Amazon's is byte for byte the one its
//!   replacement was made from (other firmware: skipped), never over a
//!   missing file.
//! - Boots that apply overlays count in /var/local/kindil/overlay-tries,
//!   cleared once the boot has run SETTLE with them and upstart still knows
//!   their jobs; from LIMIT on overlays are skipped (`LIMIT` boots in a row
//!   that hung, rebooted or lost a job).
//! - Upstart watches /etc/upstart with inotify, which a bind mount doesn't
//!   wake: overlays are followed by `initctl reload-configuration`.
use crate::os::Os;
use crate::Paths;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Boots in a row that applied overlays and weren't proven. One: Amazon's
/// own monitor shows its customer-service screen after two boots whose UI
/// keeps failing, so a boot an overlay may have broken is followed by a
/// stock one.
pub const LIMIT: u32 = 1;

pub struct Overlay {
    /// The file it replaces, from /.
    pub target: &'static str,
    /// Amazon's file it was made from.
    pub original: &'static [u8],
    pub replacement: &'static [u8],
    /// The upstart job it defines: it must still be known once applied.
    pub job: Option<&'static str>,
}

pub const OVERLAYS: &[Overlay] = &[Overlay {
    // Amazon's UI starting widens /var/local/kindil (chgrp -R javausers,
    // chmod -R g=u): this one leaves it out.
    target: "etc/upstart/framework.conf",
    original: include_bytes!("../overlays/etc/upstart/framework.conf.orig"),
    replacement: include_bytes!("../overlays/etc/upstart/framework.conf"),
    job: Some("framework"),
}];

/// What a boot overlaid, to prove or undo.
pub struct Applied {
    bound: Vec<&'static Overlay>,
    /// os.now() when applied.
    pub at: u64,
}

fn tries_file(paths: &Paths) -> PathBuf {
    paths.run.join("overlay-tries")
}

fn tries(paths: &Paths) -> u32 {
    std::fs::read_to_string(tries_file(paths))
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

/// Where an overlay's copy lives in overlay_dir.
fn source(paths: &Paths, overlay: &Overlay) -> PathBuf {
    paths.overlay_dir.join(overlay.target.replace('/', "_"))
}

/// Writes the replacement root-only on tmpfs, with the original's modes.
fn write_source(paths: &Paths, overlay: &Overlay, target: &Path) -> std::io::Result<PathBuf> {
    let _ = std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&paths.overlay_dir);
    // Also if an earlier launcher in this boot left it wider.
    std::fs::set_permissions(&paths.overlay_dir, std::fs::Permissions::from_mode(0o700))?;
    let mode = std::fs::metadata(target)?.permissions().mode() & 0o7777;
    let path = source(paths, overlay);
    let part = path.with_extension("part");
    std::fs::write(&part, overlay.replacement)?;
    std::fs::set_permissions(&part, std::fs::Permissions::from_mode(mode))?;
    std::fs::rename(&part, &path)?;
    Ok(path)
}

/// Applies every overlay whose original is in place, if the switch is on and
/// the boot limit not reached. All or none: a bind that fails undoes the
/// ones before it.
pub fn apply(
    paths: &Paths,
    os: &dyn Os,
    overlays: &'static [Overlay],
    log: &mut dyn FnMut(String),
) -> Option<Applied> {
    if !paths.overlays_on.iter().any(|switch| switch.exists()) {
        return None;
    }
    let mut pending = Vec::new();
    for overlay in overlays {
        let target = paths.root.join(overlay.target);
        match std::fs::read(&target) {
            Err(_) => log(format!("/{}: missing; not overlaid", overlay.target)),
            // `launcher boot` again in the same boot.
            Ok(bytes) if bytes == overlay.replacement => {}
            Ok(bytes) if bytes == overlay.original => pending.push(overlay),
            Ok(_) => log(format!(
                "/{}: not the file its overlay was made for (other firmware?); not overlaid",
                overlay.target
            )),
        }
    }
    if pending.is_empty() {
        return None;
    }
    let tries = tries(paths);
    if tries >= LIMIT {
        log(format!(
            "the last boot with overlays wasn't proven; skipping them \
             (delete {} to reset)",
            tries_file(paths).display()
        ));
        return None;
    }
    let _ = std::fs::create_dir_all(&paths.run);
    if std::fs::write(tries_file(paths), format!("{}\n", tries + 1)).is_err() {
        // Uncounted, a boot that hangs would apply them again forever.
        log("can't count the boot; overlays skipped".into());
        return None;
    }
    let mut bound: Vec<&'static Overlay> = Vec::new();
    for overlay in pending {
        let target = paths.root.join(overlay.target);
        let result = write_source(paths, overlay, &target)
            .map_err(|e| e.to_string())
            .and_then(|source| os.bind_ro(&source, &target));
        if let Err(error) = result {
            log(format!(
                "/{}: can't overlay ({error}); undoing all",
                overlay.target
            ));
            undo(paths, os, &bound, log);
            return None;
        }
        bound.push(overlay);
    }
    os.reload_upstart();
    for overlay in &bound {
        log(format!("/{}: overlaid", overlay.target));
    }
    Some(Applied {
        bound,
        at: os.now(),
    })
}

/// Unbinds `bound`, last first, and has upstart read the originals again
/// if it may have seen them.
fn undo(paths: &Paths, os: &dyn Os, bound: &[&Overlay], log: &mut dyn FnMut(String)) {
    for overlay in bound.iter().rev() {
        if !os.unmount(&paths.root.join(overlay.target)) {
            log(format!("/{}: can't unbind", overlay.target));
        }
        let _ = std::fs::remove_file(source(paths, overlay));
    }
    if !bound.is_empty() {
        os.reload_upstart();
    }
}

/// Once the boot has run SETTLE with them: proven if upstart still knows
/// every overlaid job, which clears the count; otherwise undone, and the
/// boot counts as one that failed.
pub fn prove(paths: &Paths, os: &dyn Os, applied: &Applied, log: &mut dyn FnMut(String)) {
    let lost: Vec<&str> = applied
        .bound
        .iter()
        .filter_map(|overlay| overlay.job)
        .filter(|job| !os.job_known(job))
        .collect();
    if lost.is_empty() {
        let _ = std::fs::remove_file(tries_file(paths));
        log("overlays proven".into());
    } else {
        log(format!(
            "upstart lost {} with overlays; undoing them",
            lost.join(", ")
        ));
        undo(paths, os, &applied.bound, log);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::os::fake::Fake;

    const A: Overlay = Overlay {
        target: "etc/upstart/a.conf",
        original: b"a stock\n",
        replacement: b"a kindil\n",
        job: Some("a"),
    };
    const B: Overlay = Overlay {
        target: "etc/upstart/b.conf",
        original: b"b stock\n",
        replacement: b"b kindil\n",
        job: None,
    };
    static BOTH: [Overlay; 2] = [A, B];

    pub struct Env {
        _dir: tempfile::TempDir,
        pub paths: Paths,
        pub os: Fake,
        pub said: Vec<String>,
    }

    impl Env {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let t = dir.path();
            let mut paths = crate::boot::tests::paths(t);
            paths.root = t.join("root");
            std::fs::create_dir_all(paths.root.join("etc/upstart")).unwrap();
            for overlay in &BOTH {
                std::fs::write(paths.root.join(overlay.target), overlay.original).unwrap();
            }
            std::fs::write(&paths.overlays_on[0], "").unwrap();
            Self {
                _dir: dir,
                paths,
                os: Fake::default(),
                said: Vec::new(),
            }
        }

        fn apply(&mut self) -> Option<Applied> {
            let said = &mut self.said;
            apply(&self.paths, &self.os, &BOTH, &mut |line| said.push(line))
        }

        fn prove(&mut self, applied: &Applied) {
            let said = &mut self.said;
            prove(&self.paths, &self.os, applied, &mut |line| said.push(line));
        }

        fn target(&self, overlay: &Overlay) -> Vec<u8> {
            std::fs::read(self.paths.root.join(overlay.target)).unwrap()
        }

        fn tries(&self) -> u32 {
            tries(&self.paths)
        }
    }

    #[test]
    fn with_the_switch_off_nothing_happens() {
        let mut env = Env::new();
        std::fs::remove_file(&env.paths.overlays_on[0]).unwrap();
        assert!(env.apply().is_none());
        assert_eq!(env.os.upstart(), "");
        assert_eq!((env.target(&A), env.tries()), (A.original.to_vec(), 0));
        assert!(!env.paths.run.join("overlay-tries").exists());
    }

    #[test]
    fn the_switch_on_the_storage_under_fuse_works_too() {
        let mut env = Env::new();
        std::fs::remove_file(&env.paths.overlays_on[0]).unwrap();
        std::fs::write(&env.paths.overlays_on[1], "").unwrap();
        assert!(env.apply().is_some());
    }

    #[test]
    fn overlays_bind_read_only_copies_and_reload_upstart() {
        let mut env = Env::new();
        let applied = env.apply().unwrap();
        assert_eq!(
            env.os.upstart(),
            "bind etc_upstart_a.conf etc/upstart/a.conf,\
             bind etc_upstart_b.conf etc/upstart/b.conf,reload-configuration,"
        );
        assert_eq!(env.target(&A), A.replacement);
        assert_eq!(env.tries(), 1);
        let dir = std::fs::metadata(&env.paths.overlay_dir).unwrap();
        assert_eq!(dir.permissions().mode() & 0o777, 0o700, "root's only");
        assert_eq!(applied.bound.len(), 2);
    }

    #[test]
    fn a_copy_keeps_the_originals_modes() {
        let mut env = Env::new();
        let target = env.paths.root.join(A.target);
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
        env.apply().unwrap();
        let copy = std::fs::metadata(source(&env.paths, &A)).unwrap();
        assert_eq!(copy.permissions().mode() & 0o7777, 0o640);
    }

    #[test]
    fn a_file_other_than_the_one_an_overlay_was_made_for_is_left() {
        let mut env = Env::new();
        std::fs::write(env.paths.root.join(A.target), "other firmware\n").unwrap();
        env.apply().unwrap();
        assert_eq!(env.target(&A), b"other firmware\n");
        assert_eq!(env.target(&B), B.replacement, "the others still apply");
        assert!(
            env.said[0].contains("a.conf: not the file"),
            "{:?}",
            env.said
        );
    }

    #[test]
    fn a_missing_file_is_never_overlaid() {
        let mut env = Env::new();
        std::fs::remove_file(env.paths.root.join(A.target)).unwrap();
        std::fs::remove_file(env.paths.root.join(B.target)).unwrap();
        assert!(env.apply().is_none());
        assert_eq!(env.os.upstart(), "");
        assert!(!env.paths.root.join(A.target).exists());
        assert_eq!(env.tries(), 0, "a boot with nothing to apply doesn't count");
    }

    #[test]
    fn at_the_limit_overlays_are_skipped_and_logged() {
        let mut env = Env::new();
        std::fs::create_dir_all(&env.paths.run).unwrap();
        std::fs::write(env.paths.run.join("overlay-tries"), "2\n").unwrap();
        assert!(env.apply().is_none());
        assert_eq!(env.os.upstart(), "");
        assert_eq!(env.target(&A), A.original);
        assert_eq!(env.tries(), 2);
        assert!(env.said.iter().any(|l| l.contains("skipping them")));
    }

    #[test]
    fn boots_that_arent_proven_count_up_to_the_limit() {
        let mut env = Env::new();
        for boot in 1..=LIMIT {
            env.apply().unwrap();
            assert_eq!(env.tries(), boot);
            // The next boot: the binds are gone.
            env.os.reboot();
        }
        assert!(env.apply().is_none());
    }

    #[test]
    fn a_proven_boot_clears_the_count() {
        let mut env = Env::new();
        let applied = env.apply().unwrap();
        assert_eq!(env.tries(), 1);
        env.prove(&applied);
        assert_eq!(env.tries(), 0);
        assert!(!env.paths.run.join("overlay-tries").exists());
        assert_eq!(env.target(&A), A.replacement, "and keeps them");
    }

    #[test]
    fn a_job_upstart_lost_undoes_the_overlays_and_counts() {
        let mut env = Env::new();
        let applied = env.apply().unwrap();
        env.os.unknown_jobs.lock().unwrap().insert("a".into());
        env.os.clear_upstart();
        env.prove(&applied);
        assert_eq!(
            env.os.upstart(),
            "unmount etc/upstart/b.conf,unmount etc/upstart/a.conf,reload-configuration,"
        );
        assert_eq!(
            (env.target(&A), env.target(&B)),
            (A.original.into(), B.original.into())
        );
        assert_eq!(env.tries(), 1);
    }

    #[test]
    fn a_failure_midway_leaves_no_binds() {
        let mut env = Env::new();
        *env.os.fail_bind.lock().unwrap() = Some("b.conf".into());
        assert!(env.apply().is_none());
        assert_eq!(
            env.os.upstart(),
            "bind etc_upstart_a.conf etc/upstart/a.conf,unmount etc/upstart/a.conf,\
             reload-configuration,"
        );
        assert!(env.os.binds.lock().unwrap().is_empty());
        assert_eq!(
            (env.target(&A), env.target(&B)),
            (A.original.into(), B.original.into())
        );
        assert!(env.said.iter().any(|l| l.contains("b.conf: can't overlay")));
    }

    #[test]
    fn applying_again_in_the_same_boot_does_nothing() {
        let mut env = Env::new();
        env.apply().unwrap();
        env.os.clear_upstart();
        assert!(env.apply().is_none());
        assert_eq!((env.os.upstart(), env.tries()), (String::new(), 1));
    }

    #[test]
    fn the_framework_overlay_differs_only_in_the_permissions_loop() {
        let [framework] = OVERLAYS else {
            panic!("one overlay")
        };
        let original = String::from_utf8(framework.original.to_vec()).unwrap();
        let replacement = String::from_utf8(framework.replacement.to_vec()).unwrap();
        let loop_of = |text: &str| -> (String, String, String) {
            let start = text.find("  DIRLIST=").unwrap();
            let end = start + text[start..].find("  done\n").unwrap() + "  done\n".len();
            (
                text[..start].into(),
                text[start..end].into(),
                text[end..].into(),
            )
        };
        let (before, old, after) = loop_of(&original);
        let (before2, new, after2) = loop_of(&replacement);
        assert_eq!((before, after), (before2, after2));
        assert!(old.contains("chgrp -R javausers $dir"));
        assert!(!new.contains("-R javausers $dir"));
    }
}

/// The framework overlay's permissions loop, run by sh over a fake /var with
/// chgrp recorded: everything but /var/local/kindil gets what Amazon's loop
/// gives it.
#[cfg(test)]
mod framework {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn the_loop(text: &[u8]) -> String {
        let text = String::from_utf8(text.to_vec()).unwrap();
        let start = text.find("  DIRLIST=").unwrap();
        let end = start + text[start..].find("  done\n").unwrap() + "  done\n".len();
        text[start..end].to_owned()
    }

    fn mode(path: &Path) -> u32 {
        std::fs::symlink_metadata(path)
            .unwrap()
            .permissions()
            .mode()
            & 0o7777
    }

    /// Runs `script`'s loop over a fresh tree; the modes after and the
    /// paths chgrp got.
    fn run(script: &[u8]) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let t = dir.path();
        let var = t.join("var");
        for d in [
            "local/kindil/sub",
            "local/java",
            "log",
            "lock",
            "tmp",
            "run",
        ] {
            std::fs::create_dir_all(var.join(d)).unwrap();
        }
        for f in [
            "local/kindil/state",
            "local/kindil/sub/f",
            "local/java/f",
            "log/m",
        ] {
            std::fs::write(var.join(f), "").unwrap();
        }
        for d in [
            "local",
            "local/kindil",
            "local/kindil/sub",
            "local/java",
            "log",
            "lock",
            "tmp",
            "run",
        ] {
            std::fs::set_permissions(var.join(d), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        for f in [
            "local/kindil/state",
            "local/kindil/sub/f",
            "local/java/f",
            "log/m",
        ] {
            std::fs::set_permissions(var.join(f), std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        std::os::unix::fs::symlink("state", var.join("local/kindil/link")).unwrap();
        let bin = t.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let chgrp = bin.join("chgrp");
        std::fs::write(
            &chgrp,
            format!("#!/bin/sh\necho \"$*\" >>{}/chgrp\n", t.display()),
        )
        .unwrap();
        std::fs::set_permissions(&chgrp, std::fs::Permissions::from_mode(0o755)).unwrap();
        let body = the_loop(script).replace("/var/", &format!("{}/", var.display()));
        let status = crate::os::retrying(|| {
            std::process::Command::new("/bin/sh")
                .args(["-c", &body])
                .env(
                    "PATH",
                    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
                )
                .status()
        })
        .unwrap();
        assert!(status.success());
        let calls = std::fs::read_to_string(t.join("chgrp"))
            .unwrap()
            .replace(&format!("{}/", var.display()), "");
        (dir, calls)
    }

    #[test]
    fn kindils_directory_keeps_its_group_and_modes() {
        let [framework] = super::OVERLAYS else {
            panic!("one overlay")
        };
        let (dir, calls) = run(framework.replacement);
        let var = dir.path().join("var");
        assert!(!calls.contains("kindil"), "{calls}");
        assert!(calls.contains("-R javausers local/java"), "{calls}");
        assert!(
            calls.lines().any(|l| l.ends_with("javausers local")),
            "{calls}"
        );
        assert_eq!(mode(&var.join("local/kindil")), 0o700);
        assert_eq!(mode(&var.join("local/kindil/sub")), 0o700);
        assert_eq!(mode(&var.join("local/kindil/state")), 0o600);
        // Everything else as Amazon's loop leaves it.
        let (stock, _) = run(framework.original);
        let stock = stock.path().join("var");
        for path in ["local", "local/java", "local/java/f", "log", "log/m", "run"] {
            assert_eq!(mode(&var.join(path)), mode(&stock.join(path)), "{path}");
        }
        assert_eq!(mode(&var.join("local/java")), 0o2770);
        assert_eq!(mode(&stock.join("local/kindil/state")), 0o660, "Amazon's");
    }
}
