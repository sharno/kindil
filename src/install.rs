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
//! sent along if there is one. Large files stay on the user storage:
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
}

impl Default for Paths {
    fn default() -> Self {
        Self {
            deploy: "/mnt/us/kindil-deploy".into(),
            extension: "/mnt/us/extensions/kindil".into(),
            run: "/var/local/kindil".into(),
        }
    }
}

/// How long kindil gets to come back up, and to stay so.
pub struct Timing {
    /// kindil stopping fully: its session restores Amazon's UI on the way out.
    pub settle: Duration,
    pub up_within: Duration,
    pub hold: Duration,
    pub poll: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            settle: Duration::from_secs(20),
            up_within: Duration::from_secs(180),
            hold: Duration::from_secs(60),
            poll: Duration::from_secs(5),
        }
    }
}

/// What the install does to the device beyond its files.
pub trait Kindle {
    fn stop_kindil(&self);
    fn start_kindil(&self, extension: &Path);
    /// The running kindil reports `version` and has its Wi-Fi.
    fn up(&self, version: &str, run: &Path) -> bool;
    fn sleep(&self, time: Duration);
}

/// The PW1 itself.
pub struct Device;

impl Kindle for Device {
    fn stop_kindil(&self) {
        let _ = std::process::Command::new("killall").arg("kindil").status();
    }

    fn start_kindil(&self, extension: &Path) {
        let _ = std::process::Command::new(extension.join("bin/start.sh")).status();
    }

    fn up(&self, version: &str, run: &Path) -> bool {
        let running = std::process::Command::new("pidof")
            .arg("kindil")
            .output()
            .is_ok_and(|out| out.status.success());
        let reports = std::process::Command::new(run.join("kindil"))
            .arg("--version")
            .output()
            .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == version);
        running && reports && (run.join("wifi-off").exists() || wifi_joined(run))
    }

    fn sleep(&self, time: Duration) {
        std::thread::sleep(time);
    }
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
    eprintln!(
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
    // Everything as it is now, to put back if the new version doesn't come up.
    let backup = paths.deploy.join("extension-backup.tar");
    archive(&paths.extension, &backup).context("failed: can't back up the extension")?;
    log(id, &format!("installing {version}"));

    kindle.stop_kindil();
    kindle.sleep(timing.settle);
    if let Err(error) = put_in_place(&new, &paths.extension) {
        return rollback_to(id, paths, timing, kindle, &format!("{error:#}"));
    }
    kindle.start_kindil(&paths.extension);

    let since = Instant::now();
    while !kindle.up(&version, &paths.run) {
        if since.elapsed() >= timing.up_within {
            let reason = format!(
                "{version} not up {} s after installing",
                timing.up_within.as_secs()
            );
            return rollback_to(id, paths, timing, kindle, &reason);
        }
        kindle.sleep(timing.poll);
    }
    log(
        id,
        &format!("up {} s after installing", since.elapsed().as_secs()),
    );
    kindle.sleep(timing.hold);
    if !kindle.up(&version, &paths.run) {
        let reason = format!("{version} down again {} s later", timing.hold.as_secs());
        return rollback_to(id, paths, timing, kindle, &reason);
    }
    log(id, &format!("done: {version}"));
    Ok(())
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

fn rollback_to(
    id: &str,
    paths: &Paths,
    timing: &Timing,
    kindle: &dyn Kindle,
    reason: &str,
) -> anyhow::Result<()> {
    log(id, &format!("{reason}; rolling back"));
    kindle.stop_kindil();
    kindle.sleep(timing.settle);
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
    kindle.start_kindil(&paths.extension);
    let back = version_of(&paths.extension.join("bin/kindil")).unwrap_or_else(|_| "?".into());
    log(id, &format!("rolled back to {back}"));
    bail!("rolled back: {reason}")
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
    /// stays up, and what was done to it.
    struct Fake {
        comes_up: bool,
        stays_up: bool,
        checks: RefCell<u32>,
        calls: RefCell<Vec<String>>,
    }

    impl Fake {
        fn new(comes_up: bool, stays_up: bool) -> Self {
            Self {
                comes_up,
                stays_up,
                checks: RefCell::new(0),
                calls: RefCell::default(),
            }
        }
    }

    impl Kindle for Fake {
        fn stop_kindil(&self) {
            self.calls.borrow_mut().push("stop".into());
        }
        fn start_kindil(&self, extension: &Path) {
            let version = version_of(&extension.join("bin/kindil")).unwrap();
            self.calls.borrow_mut().push(format!("start {version}"));
        }
        fn up(&self, version: &str, _: &Path) -> bool {
            *self.checks.borrow_mut() += 1;
            version == "kindil 2.0.0"
                && self.comes_up
                && (self.stays_up || *self.checks.borrow() == 1)
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
            settle: Duration::ZERO,
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
    fn a_bundle_that_fails_its_checksum_changes_nothing() {
        let s = setup();
        std::fs::write(s.paths.deploy.join("bundle.sha256"), "junk").unwrap();
        let fake = Fake::new(true, true);
        let error = install("t", &s.paths, &timing(), &fake).unwrap_err();
        assert!(error.to_string().contains("checksum mismatch"), "{error}");
        assert_eq!(installed(&s.paths, "kindil"), "kindil 1.0.0");
        assert!(fake.calls.borrow().is_empty(), "kindil never stopped");
    }

    #[test]
    fn a_version_that_comes_up_and_holds_is_kept() {
        let s = setup();
        let fake = Fake::new(true, true);
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
        assert_eq!(*fake.calls.borrow(), ["stop", "start kindil 2.0.0"]);
        let mode = std::fs::metadata(s.paths.extension.join("bin/start.sh")).unwrap();
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&mode.permissions()) & 0o111,
            0o111
        );
    }

    #[test]
    fn a_version_that_never_comes_up_is_rolled_back_whole() {
        let s = setup();
        let fake = Fake::new(false, false);
        let error = install("t", &s.paths, &timing(), &fake).unwrap_err();
        assert!(error.to_string().contains("rolled back"), "{error}");
        assert_eq!(installed(&s.paths, "kindil"), "kindil 1.0.0");
        assert_eq!(launcher(&s.paths), "old launcher", "the launcher files too");
        assert_eq!(fake.calls.borrow().last().unwrap(), "start kindil 1.0.0");
    }

    #[test]
    fn a_version_that_goes_down_again_is_rolled_back() {
        let s = setup();
        let fake = Fake::new(true, false);
        assert!(install("t", &s.paths, &timing(), &fake).is_err());
        assert_eq!(installed(&s.paths, "kindil"), "kindil 1.0.0");
    }

    #[test]
    fn a_rollback_binary_sent_along_is_the_one_rolled_back_to() {
        let s = setup();
        std::fs::write(s.paths.deploy.join("kindil.rollback"), binary("0.9.0")).unwrap();
        let fake = Fake::new(false, false);
        assert!(install("t", &s.paths, &timing(), &fake).is_err());
        assert_eq!(installed(&s.paths, "kindil"), "kindil 0.9.0");
        assert_eq!(launcher(&s.paths), "old launcher");
    }
}
