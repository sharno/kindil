//! Over-the-air updates from the repo's GitHub releases.
//!
//! A release carries the static ARM binary and a minisign signature made by
//! CI. kindil downloads both, refuses anything not signed by [`PUBLIC_KEY`],
//! checks the new binary runs, and stages it next to the current one. The
//! launcher gives a staged binary one trial run; kindil commits it once it has
//! run for [`SETTLE`], and the launcher goes back to the previous binary if
//! the trial ends first. Nothing outside kindil's own folders is touched.
use serde::Deserialize;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

/// Where releases come from.
pub const REPO: &str = "sharno/kindil";

/// The key CI signs releases with (`release.yml`).
const PUBLIC_KEY: &str = "RWToylUetjwYsYn48p66hk/uI0UF3dn9HsRGmsNjF923AT80JDaW+5Q8";

/// Names this build's version inside the binary, for the installer to read
/// off an ARM binary it can't run (installer/src/detect.rs).
#[used]
pub static VERSION_MARKER: &str = concat!("kindil-version=", env!("CARGO_PKG_VERSION"), "\n");

/// How long a new version must run before it replaces the previous one.
pub const SETTLE: Duration = Duration::from_secs(120);

/// The launcher's exit code for "run me again": after staging an update.
pub const RESTART_EXIT_CODE: u8 = 75;

/// The binary's file name in the extension's `bin` folder.
const BINARY: &str = "kindil";
const SIGNATURE: &str = "kindil.minisig";
/// The KUAL extension this binary was built with, relative to its folder,
/// with each file's mode. A settled update installs them, so launcher fixes
/// arrive over the air too; they take effect at the next start from KUAL.
const LAUNCHER: [(&str, &[u8], u32); 6] = [
    (
        "bin/start.sh",
        include_bytes!("../kindle/extensions/kindil/bin/start.sh"),
        0o755,
    ),
    (
        "bin/boot.sh",
        include_bytes!("../kindle/extensions/kindil/bin/boot.sh"),
        0o755,
    ),
    // Only the install script copies these into the system.
    (
        "boot/kindil.conf",
        include_bytes!("../kindle/extensions/kindil/boot/kindil.conf"),
        0o644,
    ),
    (
        "boot/install.sh",
        include_bytes!("../kindle/extensions/kindil/boot/install.sh"),
        0o755,
    ),
    (
        "menu.json",
        include_bytes!("../kindle/extensions/kindil/menu.json"),
        0o644,
    ),
    (
        "config.xml",
        include_bytes!("../kindle/extensions/kindil/config.xml"),
        0o644,
    ),
];
/// A release binary is about 16 MB; anything far larger is not ours.
const MAX_DOWNLOAD: u64 = 64 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    major: u32,
    minor: u32,
    patch: u32,
}

impl Version {
    /// `1.2.3` or `v1.2.3`.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.strip_prefix('v').unwrap_or(text).split('.');
        let mut number = || parts.next()?.parse().ok();
        let version = Self {
            major: number()?,
            minor: number()?,
            patch: number()?,
        };
        parts.next().is_none().then_some(version)
    }

    /// This build's version.
    pub const fn current() -> Self {
        Self {
            major: decimal(env!("CARGO_PKG_VERSION_MAJOR")),
            minor: decimal(env!("CARGO_PKG_VERSION_MINOR")),
            patch: decimal(env!("CARGO_PKG_VERSION_PATCH")),
        }
    }
}

/// Cargo's version parts are plain decimals; evaluated at compile time.
const fn decimal(text: &str) -> u32 {
    let bytes = text.as_bytes();
    let mut value = 0;
    let mut i = 0;
    while i < bytes.len() {
        value = value * 10 + (bytes[i] - b'0') as u32;
        i += 1;
    }
    value
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// A GitHub API URL for a release asset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetUrl(String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    binary: AssetUrl,
    signature: AssetUrl,
}

/// The parts of GitHub's release JSON kindil reads.
#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    url: String,
}

impl Release {
    /// GitHub's release JSON.
    pub fn decode(json: &str) -> anyhow::Result<Self> {
        let release: GithubRelease = serde_json::from_str(json)?;
        let version = Version::parse(&release.tag_name).ok_or_else(|| {
            anyhow::anyhow!("release tag {:?} is not a version", release.tag_name)
        })?;
        let asset = |name: &str| {
            release
                .assets
                .iter()
                .find(|a| a.name == name)
                .map(|a| AssetUrl(a.url.clone()))
                .ok_or_else(|| anyhow::anyhow!("release {version} has no {name} yet"))
        };
        Ok(Self {
            version,
            binary: asset(BINARY)?,
            signature: asset(SIGNATURE)?,
        })
    }
}

/// The Settings row's state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Updates {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    /// Downloading, verifying and staging; kindil restarts when done.
    Installing(Version),
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Check {
    UpToDate,
    Available(Release),
}

impl Check {
    pub fn of(current: Version, latest: Release) -> Self {
        if latest.version > current {
            Check::Available(latest)
        } else {
            Check::UpToDate
        }
    }
}

/// Refuses a binary not signed by CI's key.
pub fn verify(binary: &[u8], signature: &str) -> anyhow::Result<()> {
    verify_with(PUBLIC_KEY, binary, signature)
}

fn verify_with(public_key: &str, binary: &[u8], signature: &str) -> anyhow::Result<()> {
    let key = minisign_verify::PublicKey::from_base64(public_key)?;
    let signature = minisign_verify::Signature::decode(signature)?;
    key.verify(binary, &signature, false)?;
    Ok(())
}

/// The public repo's releases; no token needed.
pub struct Github {
    agent: ureq::Agent,
}

impl Default for Github {
    fn default() -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(120)))
            .user_agent(concat!("kindil/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Self { agent }
    }
}

impl Github {
    pub fn latest(&self) -> anyhow::Result<Release> {
        let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
        let json = self
            .agent
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .call()?
            .body_mut()
            .read_to_string()?;
        Release::decode(&json)
    }

    fn asset(&self, url: &AssetUrl) -> anyhow::Result<Vec<u8>> {
        Ok(self
            .agent
            .get(&url.0)
            .header("Accept", "application/octet-stream")
            .call()?
            .body_mut()
            .with_config()
            .limit(MAX_DOWNLOAD)
            .read_to_vec()?)
    }

    /// The release's binary and its signature, not yet trusted.
    fn download(&self, release: &Release) -> anyhow::Result<(Vec<u8>, String)> {
        let signature = String::from_utf8(self.asset(&release.signature)?)?;
        let binary = self.asset(&release.binary)?;
        Ok((binary, signature))
    }
}

/// The extension's `bin` folder, where the launcher looks for binaries, and
/// the marker the launcher leaves while a new version is on trial.
pub struct Slots {
    bin: PathBuf,
    trial: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Commit {
    /// This run is not a trial.
    Nothing,
    /// The new version replaced the previous one.
    Committed,
}

impl Slots {
    pub fn new(bin: impl Into<PathBuf>, trial: impl Into<PathBuf>) -> Self {
        Self {
            bin: bin.into(),
            trial: trial.into(),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.bin.join(name)
    }

    /// After a trial run settles: the trial binary becomes the current one
    /// and the current one becomes the previous one. Only a run the trial
    /// files say is a trial is kept ([`Ota::read`]).
    pub fn commit(&self) -> anyhow::Result<Commit> {
        match Ota::read(self) {
            Ok(OtaFound::Trying(trial)) => {
                trial.commit(self)?;
                Ok(Commit::Committed)
            }
            Ok(_) => Ok(Commit::Nothing),
            Err(Inconsistent::MarkerWithoutBinary) => {
                // Replaced by hand mid-trial (a deploy): nothing left to keep.
                std::fs::remove_file(&self.trial)?;
                Ok(Commit::Nothing)
            }
            Err(error @ Inconsistent::BinaryWithoutMarker) => {
                // Never tried: not kept. The launcher drops it.
                crate::log!("kindil: update: {error}");
                Ok(Commit::Nothing)
            }
        }
    }

    /// Writes the launcher files that differ, each replaced in one rename so
    /// a cut-off write never leaves half a script for KUAL to run.
    fn install_launcher(&self) -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let root = self
            .bin
            .parent()
            .ok_or_else(|| anyhow::anyhow!("{} has no parent", self.bin.display()))?;
        LAUNCHER
            .iter()
            .filter(|(name, bytes, _)| {
                std::fs::read(root.join(name)).ok().as_deref() != Some(*bytes)
            })
            .try_for_each(|(name, bytes, mode)| {
                let path = root.join(name);
                if let Some(folder) = path.parent() {
                    std::fs::create_dir_all(folder)?;
                }
                let part = path.with_extension("part");
                std::fs::write(&part, bytes)?;
                std::fs::set_permissions(&part, std::fs::Permissions::from_mode(*mode))?;
                std::fs::rename(&part, &path)?;
                Ok(())
            })
    }
}

/// Trial files no step leaves for long: what [`Ota::read`] refuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inconsistent {
    /// The marker without `kindil.trying`: the trial binary was replaced by
    /// hand (a deploy), or a commit stopped between its two steps.
    MarkerWithoutBinary,
    /// `kindil.trying` without the marker: the launcher stopped between
    /// moving the update in and marking its trial. It never ran.
    BinaryWithoutMarker,
}

impl fmt::Display for Inconsistent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Inconsistent::MarkerWithoutBinary => {
                "the trial marker is there but kindil.trying isn't: nothing on trial to keep"
            }
            Inconsistent::BinaryWithoutMarker => {
                "kindil.trying is there but the trial marker isn't: it never had its trial"
            }
        })
    }
}

impl std::error::Error for Inconsistent {}

pub use steps::ota::SomeState as OtaFound;
pub use steps::{Available, Idle, Kept, Ota, RolledBack, Staged, Trying, Verified};

/// An over-the-air update as typestate: a binary is staged only once its
/// signature checked out, and kept only from a run the launcher marked as
/// its trial; any trial state can be rolled back. A run between the launcher
/// and kindil spans processes: [`Ota::read`] rebuilds the state from the
/// files each leaves. No state can be made outside this module but through
/// [`Ota::available`] and [`Ota::read`] (the [`steps::Seal`] can't be).
mod steps {
    use super::*;
    use statum::{machine, state, transition, validators, Error};

    /// What only this module can make.
    #[derive(Debug)]
    pub struct Seal(());

    #[state]
    #[derive(Debug)]
    pub enum OtaState {
        /// Nothing staged or on trial.
        Idle,
        /// A newer release than this build.
        Available(Release),
        /// Its binary, signed by CI's key.
        Verified(Signed),
        /// `kindil.next`, reporting its version, for the launcher's next
        /// start.
        Staged,
        /// This run is the trial: `kindil.trying` and the launcher's marker.
        Trying,
        /// The trial binary is now `kindil`.
        Kept,
        /// The staged or trial binary is gone, the current one stays.
        RolledBack,
    }

    /// A binary that matched its signature, and the version it should report.
    #[derive(Debug)]
    pub struct Signed {
        binary: Vec<u8>,
        version: Version,
    }

    #[machine]
    #[derive(Debug)]
    pub struct Ota<OtaState> {
        seal: Seal,
    }

    impl Ota<Available> {
        /// Where every update starts.
        pub fn available(release: Release) -> Self {
            Self::builder().state_data(release).seal(Seal(())).build()
        }
    }

    #[transition]
    impl Ota<Available> {
        /// Downloads the release, refusing anything not signed by CI's key.
        pub fn download(
            self,
            github: &Github,
        ) -> ::core::result::Result<Ota<Verified>, anyhow::Error> {
            let (binary, signature) = github.download(&self.state_data)?;
            self.verify(binary, &signature, PUBLIC_KEY)
        }

        pub(super) fn verify(
            self,
            binary: Vec<u8>,
            signature: &str,
            public_key: &str,
        ) -> ::core::result::Result<Ota<Verified>, anyhow::Error> {
            verify_with(public_key, &binary, signature)?;
            let version = self.state_data.version;
            Ok(self.transition_with(Signed { binary, version }))
        }
    }

    #[transition]
    impl Ota<Verified> {
        /// Writes the binary where the launcher picks it up on the next
        /// start, after checking it runs and reports the expected version.
        pub fn stage(self, slots: &Slots) -> ::core::result::Result<Ota<Staged>, anyhow::Error> {
            use std::os::unix::fs::PermissionsExt;
            let Signed { binary, version } = &self.state_data;
            let part = slots.path("kindil.next.part");
            std::fs::write(&part, binary)?;
            std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o755))?;
            let reported = std::process::Command::new(&part)
                .arg("--version")
                .output()?;
            let expected = format!("kindil {version}");
            let reported = String::from_utf8_lossy(&reported.stdout);
            if reported.trim() != expected {
                let _ = std::fs::remove_file(&part);
                anyhow::bail!(
                    "the download reports {:?}, not {expected:?}",
                    reported.trim()
                );
            }
            std::fs::rename(&part, slots.path("kindil.next"))?;
            Ok(self.transition())
        }
    }

    #[transition]
    impl Ota<Staged> {
        /// The staged binary dropped before its trial.
        pub fn roll_back(self, slots: &Slots) -> Ota<RolledBack> {
            let _ = std::fs::remove_file(slots.path("kindil.next"));
            self.transition()
        }
    }

    #[transition]
    impl Ota<Trying> {
        /// After a trial run settles: the trial binary becomes the current
        /// one and the current one the previous one.
        pub fn commit(self, slots: &Slots) -> ::core::result::Result<Ota<Kept>, anyhow::Error> {
            let current = slots.path(BINARY);
            if current.exists() {
                std::fs::rename(&current, slots.path("kindil.previous"))?;
            }
            std::fs::rename(slots.path("kindil.trying"), &current)?;
            std::fs::remove_file(&slots.trial)?;
            // The new binary is kept either way; an old launcher still runs it.
            if let Err(error) = slots.install_launcher() {
                crate::log!("kindil: launcher: {error}");
            }
            Ok(self.transition())
        }

        /// What the launcher does when the trial ends first; here for
        /// completeness: any trial state can go back.
        pub fn roll_back(self, slots: &Slots) -> Ota<RolledBack> {
            let _ = std::fs::remove_file(&slots.trial);
            let _ = std::fs::remove_file(slots.path("kindil.trying"));
            self.transition()
        }
    }

    /// Which trial files exist.
    pub(super) struct Files {
        next: bool,
        trying: bool,
        marker: bool,
    }

    impl Files {
        pub(super) fn of(slots: &Slots) -> Self {
            Self {
                next: slots.path("kindil.next").is_file(),
                trying: slots.path("kindil.trying").exists(),
                marker: slots.trial.exists(),
            }
        }

        fn inconsistent(&self) -> Inconsistent {
            if self.marker {
                Inconsistent::MarkerWithoutBinary
            } else {
                Inconsistent::BinaryWithoutMarker
            }
        }
    }

    fn when(holds: bool) -> statum::Result<()> {
        if holds {
            Ok(())
        } else {
            Err(Error::InvalidState)
        }
    }

    /// Only the launcher's and kindil's own files on disk: an update
    /// available or verified lives in one run's memory, and kept or rolled
    /// back look like nothing on trial.
    #[validators(Ota)]
    impl Files {
        fn is_idle(&self) -> statum::Result<()> {
            when(!self.next && !self.trying && !self.marker)
        }
        fn is_available(&self) -> statum::Result<Release> {
            Err(Error::InvalidState)
        }
        fn is_verified(&self) -> statum::Result<Signed> {
            Err(Error::InvalidState)
        }
        fn is_staged(&self) -> statum::Result<()> {
            when(self.next && !self.trying && !self.marker)
        }
        /// An update staged during the trial waits for the next start.
        fn is_trying(&self) -> statum::Result<()> {
            when(self.trying && self.marker)
        }
        fn is_kept(&self) -> statum::Result<()> {
            Err(Error::InvalidState)
        }
        fn is_rolled_back(&self) -> statum::Result<()> {
            Err(Error::InvalidState)
        }
    }

    /// For tests: a binary as if signed, since the tests can't sign one.
    #[cfg(test)]
    pub(super) fn signed(binary: &[u8], version: Version) -> Ota<Verified> {
        Ota::<Verified>::builder()
            .state_data(Signed {
                binary: binary.to_vec(),
                version,
            })
            .seal(Seal(()))
            .build()
    }

    impl Ota<Idle> {
        /// The update's state from the files on disk, or what is wrong with
        /// them.
        pub fn read(slots: &Slots) -> ::core::result::Result<ota::SomeState, Inconsistent> {
            let files = Files::of(slots);
            Ota::rebuild(&files)
                .seal(Seal(()))
                .build()
                .map_err(|_| files.inconsistent())
        }
    }
}

/// The update's graph, for docs/state-machines.md.
#[cfg(test)]
pub(crate) fn graph() -> statum::StableGraphMetadata {
    use statum::MachineIntrospection;
    statum::StableGraphMetadata::from_graph(Ota::<Idle>::GRAPH)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn versions_parse_and_order() {
        assert_eq!(v("v0.4.10").to_string(), "0.4.10");
        assert!(v("0.4.10") > v("0.4.9"));
        assert!(v("1.0.0") > v("0.99.99"));
        assert_eq!(Version::parse("0.4"), None);
        assert_eq!(Version::parse("0.4.0.1"), None);
        assert_eq!(Version::parse("v0.4.x"), None);
        assert_eq!(Version::current().to_string(), env!("CARGO_PKG_VERSION"));
    }

    const RELEASE: &str = r#"{
        "tag_name": "v0.5.1",
        "assets": [
            {"name": "kindil", "url": "https://api.github.com/repos/sharno/kindil/releases/assets/1"},
            {"name": "kindil.minisig", "url": "https://api.github.com/repos/sharno/kindil/releases/assets/2"}
        ]
    }"#;

    #[test]
    fn decodes_a_release_and_compares_it() {
        let release = Release::decode(RELEASE).unwrap();
        assert_eq!(release.version, v("0.5.1"));
        assert_eq!(
            Check::of(v("0.5.0"), release.clone()),
            Check::Available(release.clone())
        );
        assert_eq!(Check::of(v("0.5.1"), release.clone()), Check::UpToDate);
        assert_eq!(Check::of(v("0.6.0"), release), Check::UpToDate);
    }

    #[test]
    fn a_release_still_building_has_no_binary() {
        let error = Release::decode(r#"{"tag_name": "v0.5.1", "assets": []}"#).unwrap_err();
        assert!(error.to_string().contains("no kindil yet"), "{error}");
    }

    /// A throwaway key and its signature over `kindil test fixture`.
    const TEST_KEY: &str = "RWTyZYsg/MjSsBaHXWuR+TBprmyzIALfj/Jo8mQpkcfLPUGWjscxzlDo";
    const TEST_SIGNATURE: &str = "untrusted comment: signature from minisign secret key
RUTyZYsg/MjSsK0vy1RouLf0XdVUA7ZPpwDpRgPJdurL1mtO4sNF0bQJ+0ZJYJFJ/wXxSX2za3lcgh59KADFtps5BqsasgGeCwk=
trusted comment: fixture
SW/I/JX/SBpxgS4DThvhL91wP3EJoH7taYJoyf43DW8zZZ3wB65CwRxKLqoEs5+fTbZ4yzLmtSU3WMSUk80iBQ==
";

    #[test]
    fn accepts_only_what_the_key_signed() {
        let fixture = b"kindil test fixture";
        verify_with(TEST_KEY, fixture, TEST_SIGNATURE).unwrap();
        assert!(verify_with(TEST_KEY, b"kindil test fixturE", TEST_SIGNATURE).is_err());
        assert!(verify_with(TEST_KEY, fixture, "not a signature").is_err());
        // The release key did not sign it.
        assert!(verify(fixture, TEST_SIGNATURE).is_err());
    }

    #[test]
    fn commit_swaps_the_trial_in_and_keeps_the_previous() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let trial = dir.path().join("trying");
        let slots = Slots::new(&bin, &trial);
        std::fs::write(bin.join("kindil"), "old").unwrap();
        std::fs::write(bin.join("kindil.trying"), "new").unwrap();
        assert_eq!(slots.commit().unwrap(), Commit::Nothing);
        std::fs::write(&trial, "").unwrap();
        assert_eq!(slots.commit().unwrap(), Commit::Committed);
        let read = |name: &str| std::fs::read_to_string(bin.join(name)).unwrap();
        assert_eq!(read("kindil"), "new");
        assert_eq!(read("kindil.previous"), "old");
        assert!(!trial.exists() && !bin.join("kindil.trying").exists());
        LAUNCHER.iter().for_each(|(name, bytes, _)| {
            assert_eq!(
                std::fs::read(dir.path().join(name)).unwrap(),
                *bytes,
                "{name}"
            );
        });
        assert_eq!(slots.commit().unwrap(), Commit::Nothing);
        std::fs::write(&trial, "").unwrap();
        assert_eq!(slots.commit().unwrap(), Commit::Nothing);
        assert!(
            !trial.exists(),
            "a marker without a trial binary is dropped"
        );
        assert_eq!(read("kindil"), "new");
    }

    #[test]
    fn staging_refuses_a_binary_that_reports_another_version() {
        let dir = tempfile::tempdir().unwrap();
        let slots = Slots::new(dir.path(), dir.path().join("trying"));
        let script = b"#!/bin/sh\necho kindil 9.9.9\n";
        assert!(steps::signed(script, v("0.5.1")).stage(&slots).is_err());
        assert!(!dir.path().join("kindil.next").exists());
        steps::signed(script, v("9.9.9")).stage(&slots).unwrap();
        assert!(dir.path().join("kindil.next").exists());
        assert!(matches!(Ota::read(&slots), Ok(OtaFound::Staged(_))));
    }

    #[test]
    fn only_a_signed_binary_is_verified() {
        let release = || Ota::available(Release::decode(RELEASE).unwrap());
        let fixture = b"kindil test fixture".to_vec();
        assert!(release()
            .verify(fixture.clone(), TEST_SIGNATURE, TEST_KEY)
            .is_ok());
        let error = release()
            .verify(fixture, TEST_SIGNATURE, PUBLIC_KEY)
            .unwrap_err();
        assert!(!error.to_string().is_empty());
    }

    /// Every combination of `kindil.next`, `kindil.trying` and the marker.
    #[test]
    fn the_trial_files_rebuild_one_state_or_say_whats_wrong() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let slots = Slots::new(&bin, dir.path().join("trying"));
        let state = |found: Result<OtaFound, Inconsistent>| match found {
            Ok(OtaFound::Idle(_)) => "idle".to_owned(),
            Ok(OtaFound::Staged(_)) => "staged".to_owned(),
            Ok(OtaFound::Trying(_)) => "trying".to_owned(),
            Ok(_) => "in memory only".to_owned(),
            Err(error) => format!("{error:?}"),
        };
        for (next, trying, marker, want) in [
            (false, false, false, "idle"),
            (true, false, false, "staged"),
            (false, true, true, "trying"),
            (true, true, true, "trying"),
            (false, false, true, "MarkerWithoutBinary"),
            (true, false, true, "MarkerWithoutBinary"),
            (false, true, false, "BinaryWithoutMarker"),
            (true, true, false, "BinaryWithoutMarker"),
        ] {
            for (on, path) in [
                (next, bin.join("kindil.next")),
                (trying, bin.join("kindil.trying")),
                (marker, dir.path().join("trying")),
            ] {
                let _ = std::fs::remove_file(&path);
                if on {
                    std::fs::write(&path, "").unwrap();
                }
            }
            assert_eq!(
                state(Ota::read(&slots)),
                want,
                "next {next}, trying {trying}, marker {marker}"
            );
        }
        assert!(Inconsistent::BinaryWithoutMarker
            .to_string()
            .contains("never had its trial"));
    }

    #[test]
    fn a_binary_without_its_marker_is_never_kept() {
        let dir = tempfile::tempdir().unwrap();
        let slots = Slots::new(dir.path(), dir.path().join("trying"));
        std::fs::write(dir.path().join("kindil"), "old").unwrap();
        std::fs::write(dir.path().join("kindil.trying"), "new").unwrap();
        assert_eq!(slots.commit().unwrap(), Commit::Nothing);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("kindil")).unwrap(),
            "old"
        );
    }

    #[test]
    fn any_trial_state_rolls_back() {
        let dir = tempfile::tempdir().unwrap();
        let slots = Slots::new(dir.path(), dir.path().join("trying"));
        std::fs::write(dir.path().join("kindil.next"), "").unwrap();
        let Ok(OtaFound::Staged(staged)) = Ota::read(&slots) else {
            panic!("staged")
        };
        staged.roll_back(&slots);
        assert!(matches!(Ota::read(&slots), Ok(OtaFound::Idle(_))));
        std::fs::write(dir.path().join("kindil.trying"), "").unwrap();
        std::fs::write(dir.path().join("trying"), "").unwrap();
        let Ok(OtaFound::Trying(trying)) = Ota::read(&slots) else {
            panic!("trying")
        };
        trying.roll_back(&slots);
        assert!(matches!(Ota::read(&slots), Ok(OtaFound::Idle(_))));
    }
}
