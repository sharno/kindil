//! `kindil install`: installs a release on the Kindle and rolls back unless
//! it comes up. `cargo xtask release` uploads the release's own binary with
//! the bundle and runs this, detached, so each release installs itself.
//!
//! It checks the bundle, backs up the whole installed extension, stops
//! kindil, installs every file in the bundle (keeping the replaced binary as
//! the launcher's crash fallback) and starts kindil. Then the new version
//! must be the one running, with its own Wi-Fi joined and addressed (or just
//! running, with Wi-Fi switched off), and still so a minute later. Otherwise
//! the backup goes back, launcher files and all, with the rollback binary
//! sent along if there is one, and the launcher on the system storage as it
//! was (a release that ran two minutes writes its own, src/launcher.rs). Large files stay on the user storage:
//! /var/local is 64 MB, mostly Amazon's.
use anyhow::{bail, Context};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub struct Paths {
    /// Where the bundle was uploaded: /mnt/us/kindil-deploy.
    pub deploy: PathBuf,
    /// The installed extension: /mnt/us/extensions/kindil.
    pub extension: PathBuf,
    /// The running copy's directory: /var/local/kindil.
    pub run: PathBuf,
    /// The launcher the shims run: /var/local/kindil/launcher.
    pub launcher: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        Self {
            deploy: "/mnt/us/kindil-deploy".into(),
            extension: "/mnt/us/extensions/kindil".into(),
            run: "/var/local/kindil".into(),
            launcher: crate::launcher::PATH.into(),
        }
    }
}

/// How long kindil gets to come back up, and to stay so.
pub struct Timing {
    pub up_within: Duration,
    pub hold: Duration,
    pub poll: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            up_within: Duration::from_secs(180),
            hold: Duration::from_secs(60),
            poll: Duration::from_secs(5),
        }
    }
}

/// What the install does to the device beyond its files.
pub trait Kindle {
    /// kindil runs the binary now in `extension`: a running kindil is asked
    /// to restart (SIGUSR1: it exits with the launcher's restart code, and
    /// the launcher's session copies and runs the new binary, Amazon's UI
    /// staying stopped); with none running, `bin/start.sh` starts it.
    ///
    /// Until 0.21 the install killed kindil, which the launcher takes for
    /// Exit to Kindle: it brought Amazon's whole UI (X, its Java
    /// framework) back up, and the new session stopped it again, copying
    /// the binary meanwhile, while it started: 151 s from start to up on
    /// 0.21, growing with each release (36 s on 0.16). A kindil before
    /// 0.22 has no handler: SIGUSR1 ends it, which the launcher takes for a
    /// crash and runs kindil again 2 s later, Amazon's UI staying stopped
    /// too.
    fn restart_kindil(&self, extension: &Path);
    /// The running kindil reports `version` and has its Wi-Fi.
    fn up(&self, version: &str, run: &Path) -> bool;
    fn sleep(&self, time: Duration);
}

/// The PW1 itself.
pub struct Device;

impl Kindle for Device {
    fn restart_kindil(&self, extension: &Path) {
        let running = sessions_kindil(&Paths::default().run.join("kindil"));
        if running.is_empty() {
            let _ = std::process::Command::new(extension.join("bin/start.sh")).status();
        }
        for pid in running {
            // SAFETY: kill(2) on a pid just read from /proc.
            unsafe { libc::kill(pid, libc::SIGUSR1) };
        }
    }

    fn up(&self, version: &str, run: &Path) -> bool {
        let running = std::process::Command::new("pidof")
            .arg("kindil")
            .output()
            .is_ok_and(|out| out.status.success());
        up(version, run, running, || wifi_joined(run))
    }

    fn sleep(&self, time: Duration) {
        std::thread::sleep(time);
    }
}

/// The kindil the launcher's session runs: the copy at `bin`, run as
/// `kindil ... kindle` (not its DHCP hook or lifeline, which are links to
/// it, nor this install).
fn sessions_kindil(bin: &Path) -> Vec<i32> {
    std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let pid: i32 = entry.file_name().to_str()?.parse().ok()?;
            let exe = std::fs::read_link(entry.path().join("exe")).ok()?;
            let cmdline = std::fs::read(entry.path().join("cmdline")).ok()?;
            (exe == bin && is_session(&cmdline)).then_some(pid)
        })
        .collect()
}

/// A `/proc/PID/cmdline` whose last argument is `kindle`: the session's
/// kindil (`kindil [--config FILE] kindle`).
fn is_session(cmdline: &[u8]) -> bool {
    cmdline.split(|&b| b == 0).rfind(|arg| !arg.is_empty()) == Some(b"kindle")
}

/// kindil runs, the copy that runs is `version` (not the launcher's
/// fallback to the previous one), and it has its Wi-Fi, or Wi-Fi is off.
fn up(version: &str, run: &Path, running: bool, joined: impl FnOnce() -> bool) -> bool {
    running
        && version_of(&run.join("kindil")).is_ok_and(|v| v == version)
        && (run.join("wifi-off").exists() || joined())
}

/// kindil's own supplicant runs, joined, with an address.
fn wifi_joined(run: &Path) -> bool {
    let supplicant = run.join("wpa_supplicant");
    let ours = std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| {
            std::fs::read(entry.path().join("cmdline"))
                .is_ok_and(|cmdline| cmdline.starts_with(supplicant.as_os_str().as_encoded_bytes()))
        });
    ours && crate::wifi::open("wlan0")
        .and_then(|wpa| wpa.link())
        .is_ok_and(|link| matches!(link, crate::wifi::Link::Associated { ip: Some(_), .. }))
}

fn log(id: &str, message: &str) {
    crate::log!(
        "{} install {id}: {message}",
        chrono::Local::now().format("%F %T")
    );
}

/// Runs install `id`: Ok once the new version has held, Err after a failed
/// check (nothing changed) or a rollback.
pub fn install(
    id: &str,
    paths: &Paths,
    timing: &Timing,
    kindle: &dyn Kindle,
) -> anyhow::Result<()> {
    let result = run(id, paths, timing, kindle);
    if let Err(error) = &result {
        log(id, &format!("{error:#}"));
    }
    result
}

fn run(id: &str, paths: &Paths, timing: &Timing, kindle: &dyn Kindle) -> anyhow::Result<()> {
    let env = steps::Env {
        paths,
        timing,
        kindle,
    };
    // Nothing changes before the backup: a failed check or backup is only
    // an error. From the stop on, every failure rolls back.
    let backed_up = steps::check(id, paths)?.back_up(paths)?;
    let kept = backed_up
        .place(paths)
        .and_then(|placed| placed.start(&env).come_up(&env))
        .and_then(|up| up.hold(&env));
    match kept {
        Ok(_) => Ok(()),
        Err(failed) => Err(failed.roll_back(&env).into_error()),
    }
}

/// The install as typestate: each step is a method of the state before it
/// only, so the new version is kept only once it is in place, started, up
/// and still up a minute later, and a rollback needs a backup to go back to
/// (a failure before it changed nothing). Outside this module no state can
/// be made but through [`check`]: the [`Seal`] can't be.
mod steps {
    use super::*;
    use statum::{machine, state, transition};

    /// What only this module can make.
    pub(super) struct Seal(());

    pub(super) struct Env<'a> {
        pub paths: &'a Paths,
        pub timing: &'a Timing,
        pub kindle: &'a dyn Kindle,
    }

    #[state]
    pub(super) enum Phase {
        /// The bundle matched its checksum and unpacked here, and its binary
        /// runs.
        Checked(PathBuf),
        /// The extension and the launcher are backed up. kindil still runs
        /// the old binary, from its copy on the system storage.
        BackedUp(PathBuf),
        /// Every file of the bundle in place.
        Placed,
        Started,
        /// The new version runs, with its Wi-Fi.
        Up,
        /// And still did HOLD later: kept.
        Kept,
        /// A step after the backup failed, for this reason.
        Failed(String),
        RolledBack(String),
    }

    #[machine]
    pub(super) struct Installing<Phase> {
        id: String,
        /// The new binary's `--version`.
        version: String,
        seal: Seal,
    }

    /// The bundle: its checksum, unpacked, its binary run, and the rollback
    /// binary sent along, if any, runnable.
    pub(super) fn check(id: &str, paths: &Paths) -> anyhow::Result<Installing<Checked>> {
        let bundle = std::fs::read(paths.deploy.join("bundle.tgz")).context("failed: no bundle")?;
        let expected = std::fs::read_to_string(paths.deploy.join("bundle.sha256"))
            .context("failed: no checksum")?;
        if sha256(&bundle) != expected.trim() {
            bail!("failed: checksum mismatch");
        }
        let staged = paths.deploy.join("x");
        let _ = std::fs::remove_dir_all(&staged);
        tar::Archive::new(flate2::read::GzDecoder::new(bundle.as_slice()))
            .unpack(&staged)
            .context("failed: can't unpack")?;
        let new = staged.join("extensions/kindil");
        let version =
            version_of(&new.join("bin/kindil")).context("failed: the new binary won't run")?;
        let rollback = paths.deploy.join("kindil.rollback");
        if rollback.exists() {
            executable(&rollback).context("failed: can't use the rollback binary")?;
        }
        Ok(Installing::<Checked>::builder()
            .state_data(new)
            .id(id.to_owned())
            .version(version)
            .seal(Seal(()))
            .build())
    }

    #[transition]
    impl Installing<Checked> {
        /// Everything as it is now, to put back if the new version doesn't
        /// come up.
        pub(super) fn back_up(
            self,
            paths: &Paths,
        ) -> ::core::result::Result<Installing<BackedUp>, anyhow::Error> {
            let backup = paths.deploy.join("extension-backup.tar");
            archive(&paths.extension, &backup).context("failed: can't back up the extension")?;
            // Small (half a MB): kept on the user storage beside the bundle.
            let launcher = paths.deploy.join("launcher-backup");
            let _ = std::fs::remove_file(&launcher);
            if paths.launcher.exists() {
                std::fs::copy(&paths.launcher, &launcher)
                    .context("failed: can't back up the launcher")?;
            }
            log(&self.id, &format!("installing {}", self.version));
            Ok(self.transition_map(|new| new))
        }
    }

    #[transition]
    impl Installing<BackedUp> {
        pub(super) fn place(
            self,
            paths: &Paths,
        ) -> ::core::result::Result<Installing<Placed>, Installing<Failed>> {
            match put_in_place(&self.state_data, &paths.extension) {
                Ok(()) => Ok(self.transition()),
                Err(error) => Err(self.transition_with(format!("{error:#}"))),
            }
        }
    }

    #[transition]
    impl Installing<Placed> {
        pub(super) fn start(self, env: &Env) -> Installing<Started> {
            env.kindle.restart_kindil(&env.paths.extension);
            self.transition()
        }
    }

    #[transition]
    impl Installing<Started> {
        /// Up within `up_within`.
        pub(super) fn come_up(
            self,
            env: &Env,
        ) -> ::core::result::Result<Installing<Up>, Installing<Failed>> {
            let since = Instant::now();
            while !env.kindle.up(&self.version, &env.paths.run) {
                if since.elapsed() >= env.timing.up_within {
                    let reason = format!(
                        "{} not up {} s after installing",
                        self.version,
                        env.timing.up_within.as_secs()
                    );
                    return Err(self.transition_with(reason));
                }
                env.kindle.sleep(env.timing.poll);
            }
            log(
                &self.id,
                &format!("up {} s after installing", since.elapsed().as_secs()),
            );
            Ok(self.transition())
        }
    }

    #[transition]
    impl Installing<Up> {
        /// Still up `hold` later: the only way to keep it.
        pub(super) fn hold(
            self,
            env: &Env,
        ) -> ::core::result::Result<Installing<Kept>, Installing<Failed>> {
            env.kindle.sleep(env.timing.hold);
            if !env.kindle.up(&self.version, &env.paths.run) {
                let reason = format!(
                    "{} down again {} s later",
                    self.version,
                    env.timing.hold.as_secs()
                );
                return Err(self.transition_with(reason));
            }
            log(&self.id, &format!("done: {}", self.version));
            Ok(self.transition())
        }
    }

    #[transition]
    impl Installing<Failed> {
        /// The backup back, launcher files and all, with the rollback binary
        /// sent along if there is one, and the launcher on the system storage
        /// as it was.
        pub(super) fn roll_back(self, env: &Env) -> Installing<RolledBack> {
            let (id, paths) = (&self.id, env.paths);
            log(id, &format!("{}; rolling back", self.state_data));
            let restored = (|| -> anyhow::Result<()> {
                let _ = std::fs::remove_dir_all(&paths.extension);
                std::fs::create_dir_all(&paths.extension)?;
                tar::Archive::new(std::fs::File::open(
                    paths.deploy.join("extension-backup.tar"),
                )?)
                .unpack(&paths.extension)?;
                let rollback = paths.deploy.join("kindil.rollback");
                if rollback.exists() {
                    replace(&rollback, &paths.extension.join("bin/kindil"))?;
                    executable(&paths.extension.join("bin/kindil"))?;
                }
                Ok(())
            })();
            if let Err(error) = restored {
                log(id, &format!("restoring the backup failed: {error:#}"));
            }
            if let Err(error) = restore_launcher(paths) {
                log(id, &format!("restoring the launcher failed: {error:#}"));
            }
            env.kindle.restart_kindil(&paths.extension);
            let back =
                version_of(&paths.extension.join("bin/kindil")).unwrap_or_else(|_| "?".into());
            log(id, &format!("rolled back to {back}"));
            self.transition_map(|reason| reason)
        }
    }

    impl Installing<RolledBack> {
        pub(super) fn into_error(self) -> anyhow::Error {
            anyhow::anyhow!("rolled back: {}", self.state_data)
        }
    }
}

/// Installs every file of `new` into `extension`, each written aside and
/// moved into place, keeping the replaced binary as `kindil.previous`.
fn put_in_place(new: &Path, extension: &Path) -> anyhow::Result<()> {
    let bin = extension.join("bin");
    std::fs::create_dir_all(&bin).context("can't write the extension")?;
    let current = bin.join("kindil");
    if current.exists() {
        replace(&current, &bin.join("kindil.previous"))
            .context("can't keep the previous version")?;
    }
    for file in files(new)? {
        let inside = file.strip_prefix(new)?;
        let target = extension.join(inside);
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir)?;
        }
        replace(&file, &target).with_context(|| format!("can't install {}", inside.display()))?;
        let name = inside.to_string_lossy();
        if name == "bin/kindil" || name.ends_with(".sh") {
            executable(&target)?;
        }
    }
    Ok(())
}

/// The launcher as it was before the install: the backup through a file
/// beside it and a rename, or none if there was none (the shims then have
/// the rolled-back kindil write its own).
fn restore_launcher(paths: &Paths) -> anyhow::Result<()> {
    let backup = paths.deploy.join("launcher-backup");
    if backup.exists() {
        replace(&backup, &paths.launcher)?;
        executable(&paths.launcher)?;
    } else {
        match std::fs::remove_file(&paths.launcher) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
            _ => {}
        }
    }
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn version_of(binary: &Path) -> anyhow::Result<String> {
    executable(binary)?;
    // A file just written can stay open a moment in a child another thread
    // forked and hasn't yet exec'd: running it fails with ETXTBSY meanwhile.
    let mut tries = 0;
    let out = loop {
        match std::process::Command::new(binary).arg("--version").output() {
            Err(error) if error.raw_os_error() == Some(libc::ETXTBSY) && tries < 50 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            result => break result?,
        }
    };
    anyhow::ensure!(
        out.status.success(),
        "{} --version failed",
        binary.display()
    );
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

fn executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

/// Copies `from` over `to` through a file beside it, so `to` is never half
/// written.
fn replace(from: &Path, to: &Path) -> std::io::Result<()> {
    let part = to.with_extension("new");
    std::fs::copy(from, &part)?;
    std::fs::rename(&part, to)
}

fn files(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            found.extend(files(&path)?);
        } else {
            found.push(path);
        }
    }
    found.sort();
    Ok(found)
}

/// `dir`'s contents as a plain tar at `to`: gzip takes minutes on the PW1.
fn archive(dir: &Path, to: &Path) -> anyhow::Result<()> {
    let _ = std::fs::remove_file(to);
    let result = (|| -> anyhow::Result<()> {
        let mut out = tar::Builder::new(std::fs::File::create(to)?);
        out.append_dir_all(".", dir)?;
        out.into_inner()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(to);
    }
    result
}

/// The install's graph, for docs/state-machines.md.
#[cfg(test)]
pub(crate) fn graph() -> statum::StableGraphMetadata {
    use statum::MachineIntrospection;
    statum::StableGraphMetadata::from_graph(steps::Installing::<steps::Checked>::GRAPH)
}

/// For tests: a bundle of `files` under `extensions/kindil/`.
#[cfg(test)]
fn bundle(files: &[(&str, &str, u32)]) -> (Vec<u8>, String) {
    let mut out = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    for (path, body, mode) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(*mode);
        out.append_data(
            &mut header,
            format!("extensions/kindil/{path}"),
            body.as_bytes(),
        )
        .unwrap();
    }
    let bytes = out.into_inner().unwrap().finish().unwrap();
    let sum = sha256(&bytes);
    (bytes, sum)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A fake binary that reports `version`.
    fn binary(version: &str) -> String {
        format!("#!/bin/sh\necho 'kindil {version}'\n")
    }

    /// The Kindle as the tests see it: whether the new version comes up and
    /// stays up, and what was done to it. Starting kindil copies the binary
    /// to run, as the launcher does.
    struct Fake {
        comes_up: bool,
        stays_up: bool,
        /// The launcher runs the previous version instead.
        falls_back: bool,
        /// kindil joins Wi-Fi.
        wifi: bool,
        /// Where 2.0.0, once up, writes its launcher, as after two minutes.
        launcher: Option<PathBuf>,
        run: PathBuf,
        checks: RefCell<u32>,
        calls: RefCell<Vec<String>>,
    }

    impl Fake {
        fn new(s: &Setup, comes_up: bool, stays_up: bool) -> Self {
            Self {
                comes_up,
                stays_up,
                falls_back: false,
                wifi: true,
                launcher: None,
                run: s.paths.run.clone(),
                checks: RefCell::new(0),
                calls: RefCell::default(),
            }
        }
    }

    impl Kindle for Fake {
        fn restart_kindil(&self, extension: &Path) {
            let version = version_of(&extension.join("bin/kindil")).unwrap();
            self.calls
                .borrow_mut()
                .push(format!("restart into {version}"));
            let bin = extension.join("bin");
            let runs = if self.falls_back && bin.join("kindil.previous").exists() {
                bin.join("kindil.previous")
            } else {
                bin.join("kindil")
            };
            replace(&runs, &self.run.join("kindil")).unwrap();
            if let (Some(launcher), "kindil 2.0.0", true) =
                (&self.launcher, version.as_str(), self.comes_up)
            {
                std::fs::write(launcher, "launcher 2.0.0").unwrap();
            }
        }
        fn up(&self, version: &str, run: &Path) -> bool {
            *self.checks.borrow_mut() += 1;
            let running = self.comes_up && (self.stays_up || *self.checks.borrow() == 1);
            up(version, run, running, || self.wifi)
        }
        fn sleep(&self, _: Duration) {}
    }

    struct Setup {
        _dir: tempfile::TempDir,
        paths: Paths,
    }

    /// 1.0.0 installed, with its launcher; a bundle of 2.0.0 uploaded.
    fn setup() -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            deploy: dir.path().join("deploy"),
            extension: dir.path().join("ext"),
            run: dir.path().join("run"),
            launcher: dir.path().join("run/launcher"),
        };
        for d in [&paths.deploy, &paths.run, &paths.extension.join("bin")] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(paths.extension.join("bin/kindil"), binary("1.0.0")).unwrap();
        executable(&paths.extension.join("bin/kindil")).unwrap();
        std::fs::write(paths.extension.join("menu.json"), "old launcher").unwrap();
        let (bytes, sum) = bundle(&[
            ("bin/kindil", &binary("2.0.0"), 0o755),
            ("bin/start.sh", "#!/bin/sh\n", 0o644),
            ("menu.json", "new launcher", 0o644),
        ]);
        std::fs::write(paths.deploy.join("bundle.tgz"), bytes).unwrap();
        std::fs::write(paths.deploy.join("bundle.sha256"), format!("{sum}\n")).unwrap();
        Setup { _dir: dir, paths }
    }

    fn timing() -> Timing {
        Timing {
            up_within: Duration::from_millis(50),
            hold: Duration::ZERO,
            poll: Duration::ZERO,
        }
    }

    fn installed(paths: &Paths, name: &str) -> String {
        version_of(&paths.extension.join("bin").join(name)).unwrap()
    }

    fn launcher(paths: &Paths) -> String {
        std::fs::read_to_string(paths.extension.join("menu.json")).unwrap()
    }

    #[test]
    fn only_the_sessions_kindil_is_asked_to_restart() {
        let args = |args: &[&str]| args.join("\0").into_bytes();
        assert!(is_session(&args(&["/var/local/kindil/kindil", "kindle"])));
        assert!(is_session(&args(&[
            "/var/local/kindil/kindil",
            "--config",
            "/mnt/us/kindil/kindil.toml",
            "kindle",
            ""
        ])));
        // udhcpc's hook (a link to the same file), and this install.
        assert!(!is_session(&args(&[
            "/var/local/kindil/kindil-dhcp",
            "bound"
        ])));
        assert!(!is_session(&args(&["kindil", "install", "1795329732"])));
        assert!(!is_session(b""));
    }

    #[test]
    fn a_bundle_that_fails_its_checksum_changes_nothing() {
        let s = setup();
        std::fs::write(s.paths.deploy.join("bundle.sha256"), "junk").unwrap();
        let fake = Fake::new(&s, true, true);
        let error = install("t", &s.paths, &timing(), &fake).unwrap_err();
        assert!(error.to_string().contains("checksum mismatch"), "{error}");
        assert_eq!(installed(&s.paths, "kindil"), "kindil 1.0.0");
        assert!(fake.calls.borrow().is_empty(), "kindil never stopped");
    }

    #[test]
    fn a_version_that_comes_up_and_holds_is_kept() {
        let s = setup();
        let fake = Fake::new(&s, true, true);
        install("t", &s.paths, &timing(), &fake).unwrap();
        assert_eq!(installed(&s.paths, "kindil"), "kindil 2.0.0");
        assert_eq!(
            installed(&s.paths, "kindil.previous"),
            "kindil 1.0.0",
            "the crash fallback"
        );
        assert_eq!(
            launcher(&s.paths),
            "new launcher",
            "every file in the bundle"
        );
        assert_eq!(*fake.calls.borrow(), ["restart into kindil 2.0.0"]);
        let mode = std::fs::metadata(s.paths.extension.join("bin/start.sh")).unwrap();
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&mode.permissions()) & 0o111,
            0o111
        );
    }

    #[test]
    fn a_version_that_never_comes_up_is_rolled_back_whole() {
        let s = setup();
        let fake = Fake::new(&s, false, false);
        let error = install("t", &s.paths, &timing(), &fake).unwrap_err();
        assert!(error.to_string().contains("rolled back"), "{error}");
        assert_eq!(installed(&s.paths, "kindil"), "kindil 1.0.0");
        assert_eq!(launcher(&s.paths), "old launcher", "the launcher files too");
        assert_eq!(
            fake.calls.borrow().last().unwrap(),
            "restart into kindil 1.0.0"
        );
    }

    #[test]
    fn files_that_wont_go_in_place_roll_back_before_anything_starts() {
        let s = setup();
        // A directory where the bundle's script goes: it can't be replaced.
        std::fs::create_dir_all(s.paths.extension.join("bin/start.sh/x")).unwrap();
        let fake = Fake::new(&s, true, true);
        let error = install("t", &s.paths, &timing(), &fake).unwrap_err();
        assert!(
            error.to_string().contains("can't install bin/start.sh"),
            "{error}"
        );
        assert_eq!(installed(&s.paths, "kindil"), "kindil 1.0.0");
        assert_eq!(launcher(&s.paths), "old launcher");
        assert_eq!(
            *fake.calls.borrow(),
            ["restart into kindil 1.0.0"],
            "the new version never started"
        );
    }

    #[test]
    fn a_version_that_goes_down_again_is_rolled_back() {
        let s = setup();
        let fake = Fake::new(&s, true, false);
        assert!(install("t", &s.paths, &timing(), &fake).is_err());
        assert_eq!(installed(&s.paths, "kindil"), "kindil 1.0.0");
    }

    #[test]
    fn a_rollback_binary_sent_along_is_the_one_rolled_back_to() {
        let s = setup();
        std::fs::write(s.paths.deploy.join("kindil.rollback"), binary("0.9.0")).unwrap();
        let fake = Fake::new(&s, false, false);
        assert!(install("t", &s.paths, &timing(), &fake).is_err());
        assert_eq!(installed(&s.paths, "kindil"), "kindil 0.9.0");
        assert_eq!(launcher(&s.paths), "old launcher");
    }

    fn launcher_file(paths: &Paths) -> Option<String> {
        std::fs::read_to_string(&paths.launcher).ok()
    }

    #[test]
    fn the_launcher_falling_back_to_the_previous_version_isnt_taken_for_the_new_one() {
        let s = setup();
        let fake = Fake {
            falls_back: true,
            ..Fake::new(&s, true, true)
        };
        let error = install("t", &s.paths, &timing(), &fake).unwrap_err();
        assert!(error.to_string().contains("rolled back"), "{error}");
        assert_eq!(installed(&s.paths, "kindil"), "kindil 1.0.0");
    }

    #[test]
    fn with_wifi_switched_off_kindil_running_is_enough() {
        let s = setup();
        std::fs::write(s.paths.run.join("wifi-off"), "").unwrap();
        let fake = Fake {
            wifi: false,
            ..Fake::new(&s, true, true)
        };
        install("t", &s.paths, &timing(), &fake).unwrap();
        assert_eq!(installed(&s.paths, "kindil"), "kindil 2.0.0");
    }

    #[test]
    fn with_wifi_on_but_not_joined_it_isnt_up() {
        let s = setup();
        let fake = Fake {
            wifi: false,
            ..Fake::new(&s, true, true)
        };
        assert!(install("t", &s.paths, &timing(), &fake).is_err());
        assert_eq!(installed(&s.paths, "kindil"), "kindil 1.0.0");
    }

    #[test]
    fn a_rollback_brings_back_the_launcher_the_new_version_wrote() {
        let s = setup();
        std::fs::write(&s.paths.launcher, "launcher 1.0.0").unwrap();
        let fake = Fake {
            launcher: Some(s.paths.launcher.clone()),
            ..Fake::new(&s, true, false)
        };
        assert!(install("t", &s.paths, &timing(), &fake).is_err());
        assert_eq!(launcher_file(&s.paths).as_deref(), Some("launcher 1.0.0"));
        assert!(!s.paths.run.join("launcher.new").exists());
        let mode = std::fs::metadata(&s.paths.launcher).unwrap().permissions();
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&mode) & 0o111,
            0o111
        );
    }

    #[test]
    fn a_rollback_with_no_launcher_before_leaves_none() {
        let s = setup();
        let fake = Fake {
            launcher: Some(s.paths.launcher.clone()),
            ..Fake::new(&s, true, false)
        };
        assert!(install("t", &s.paths, &timing(), &fake).is_err());
        assert_eq!(
            launcher_file(&s.paths),
            None,
            "the shims write kindil's own"
        );
    }

    #[test]
    fn a_kept_version_keeps_its_launcher() {
        let s = setup();
        std::fs::write(&s.paths.launcher, "launcher 1.0.0").unwrap();
        let fake = Fake {
            launcher: Some(s.paths.launcher.clone()),
            ..Fake::new(&s, true, true)
        };
        install("t", &s.paths, &timing(), &fake).unwrap();
        assert_eq!(launcher_file(&s.paths).as_deref(), Some("launcher 2.0.0"));
    }
}
