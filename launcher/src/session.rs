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

/// Room left free on the system storage beyond a new copy: Amazon's
/// settings and logs live there too.
const MARGIN: u64 = 2 * 1024 * 1024;

/// What the copy asks of the system storage; the tests play it.
trait Storage {
    /// Bytes free for new files on the file system holding `dir`, if known.
    fn free(&self, dir: &Path) -> Option<u64>;
    /// Whether a process runs the file `file` describes, under any name:
    /// unlinking it then frees nothing until that process exits.
    fn in_use(&self, file: &std::fs::Metadata) -> bool;
}

/// The PW1's.
struct System;

impl Storage for System {
    fn free(&self, dir: &Path) -> Option<u64> {
        use std::os::unix::ffi::OsStrExt;
        let dir = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
        // SAFETY: statvfs(2) with a valid path and a zeroed out-struct.
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(dir.as_ptr(), &mut stat) } != 0 {
            return None;
        }
        // As df says: without the blocks kept for root.
        #[allow(clippy::useless_conversion)]
        Some(u64::from(stat.f_bavail).saturating_mul(u64::from(stat.f_frsize)))
    }

    /// By /proc/PID/exe, which stat follows to the very file a process
    /// runs, links resolved and even once unlinked: a lifeline run as
    /// `kindil-lifeline`, a crashed kindil not yet gone.
    fn in_use(&self, file: &std::fs::Metadata) -> bool {
        use std::os::unix::fs::MetadataExt;
        std::fs::read_dir("/proc")
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|n| n.parse::<u32>().is_ok())
            })
            .any(|entry| {
                std::fs::metadata(entry.path().join("exe"))
                    .is_ok_and(|exe| exe.dev() == file.dev() && exe.ino() == file.ino())
            })
    }
}

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
    storage: Box<dyn Storage>,
}

impl Files {
    fn new(paths: &Paths) -> Self {
        Self::with(paths, Box::new(System))
    }

    fn with(paths: &Paths, storage: Box<dyn Storage>) -> Self {
        Self {
            bin: paths.run.join("kindil"),
            from: paths.run.join("kindil.from"),
            trying: paths.run.join("trying"),
            next: paths.ext.join("kindil.next"),
            trial: paths.ext.join("kindil.trying"),
            installed: paths.ext.join("kindil"),
            previous: paths.ext.join("kindil.previous"),
            storage,
        }
    }

    /// The binary to run: a staged update gets one trial run; otherwise
    /// `slot`, the current binary or the previous one. Copied to the system
    /// storage; if it can't be, `slot` runs where it is.
    fn pick(&self, slot: &Path) -> PathBuf {
        let picked = self.choose(slot);
        // A copy kept as current may be as Amazon's UI left it.
        narrow(&self.bin);
        picked
    }

    fn choose(&self, slot: &Path) -> PathBuf {
        if let Some(staged) = self.staged() {
            if let Ok(_trying) = staged.begin(self) {
                return self.bin.clone();
            }
        }
        if slot.is_file() && !self.current(slot) {
            if let Err(error) = self.copy(slot) {
                log(&format!("copying {}: {error}", slot.display()));
                // Never no kindil at all: Amazon's UI would stay.
                if executable(slot) {
                    log("running it from the user storage: USB drive mode can't take the storage while it runs");
                    return slot.to_owned();
                }
            }
        }
        self.bin.clone()
    }

    /// A staged update, once nothing else is on trial: a trial or trial
    /// files left over are rolled back first (none are, past the session's
    /// start and the end of each run).
    fn staged(&self) -> Option<Trial<Staged>> {
        for _ in 0..2 {
            match Trial::read(self) {
                Ok(trial::SomeState::Staged(staged)) => return Some(staged),
                Ok(trial::SomeState::Idle(_)) => return None,
                Ok(trial::SomeState::Trying(trying)) => drop(trying.roll_back(self)),
                Err(inconsistent) => inconsistent.roll_back(self),
            }
        }
        None
    }

    /// Comparing the 16 MB binaries takes over 10 s at boot. Instead the copy
    /// records its source and has the source's size and modification time,
    /// set before the record is written: the same source with the same size
    /// and time means the copy is current.
    ///
    /// The time in FAT's 2 s steps: the user storage (vfat under fsp) keeps
    /// a file's time to the second while the kernel holds its inode, but
    /// on disk only in even seconds, so once the inode is dropped (Amazon's
    /// UI starting takes the memory) a source written at an odd second
    /// reads a second earlier than the copy made from it.
    fn current(&self, source: &Path) -> bool {
        std::fs::read_to_string(&self.from).is_ok_and(|from| Path::new(from.trim()) == source)
            && stamp(source).is_some()
            && stamp(source) == stamp(&self.bin)
    }

    /// Whether the system storage holds a copy of `source` with MARGIN to
    /// spare once the old copy goes: its space counts only if unlinking it
    /// frees it, i.e. nothing else runs it (a lifeline linked to it, say)
    /// and no other link names it.
    fn room(&self, source: &Path) -> std::io::Result<()> {
        use std::os::unix::fs::MetadataExt;
        let need = std::fs::metadata(source)?.len().saturating_add(MARGIN);
        let Some(free) = self.bin.parent().and_then(|dir| self.storage.free(dir)) else {
            // Unknown: try, as before.
            return Ok(());
        };
        let freed = match std::fs::symlink_metadata(&self.bin) {
            Ok(old) if old.is_file() && old.nlink() == 1 && !self.storage.in_use(&old) => old.len(),
            _ => 0,
        };
        if free.saturating_add(freed) >= need {
            return Ok(());
        }
        let mb = |bytes: u64| bytes / (1024 * 1024);
        Err(std::io::Error::other(format!(
            "no room: {} MB free, {} more once the old copy goes, {} MB needed",
            mb(free),
            mb(freed),
            mb(need)
        )))
    }

    /// In place, never beside: /var/local is 64 MB, mostly Amazon's, with
    /// room for one copy of kindil, not two. The old copy is unlinked first
    /// (its space frees once nothing runs it) and the new one written fresh,
    /// unless the room it frees, if any, isn't enough: then nothing is
    /// touched. Unlinking, not truncating, also keeps this clear of ETXTBSY,
    /// which the earlier write-beside-and-rename avoided: kindil has exited
    /// and its Wi-Fi children (whose udhcpc runs the hook from it) are ended
    /// before every pick.
    /// The record of the source comes last, so a copy cut short (a full
    /// disk, a power cut) never counts as current; a failed one is removed.
    fn copy(&self, source: &Path) -> std::io::Result<()> {
        self.room(source)?;
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

    /// Rolls back whatever trial the files show; whether a run was on trial
    /// (the marker was there).
    fn end_trial(&self) -> bool {
        match Trial::read(self) {
            Ok(trial::SomeState::Trying(trying)) => {
                trying.roll_back(self);
                true
            }
            Ok(trial::SomeState::Idle(_) | trial::SomeState::Staged(_)) => false,
            Err(inconsistent) => {
                inconsistent.roll_back(self);
                inconsistent == Inconsistent::MarkerWithoutBinary
            }
        }
    }
}

/// Trial files no step leaves for long: what [`Trial::read`] refuses. Both
/// roll back: a trial is never kept but by kindil, from a run the launcher
/// marked as its trial.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Inconsistent {
    /// The marker without `kindil.trying`: replaced by hand (a deploy), or
    /// kindil's commit stopped between its two steps.
    MarkerWithoutBinary,
    /// `kindil.trying` without the marker: stopped between moving the update
    /// in and marking its trial. It never ran.
    BinaryWithoutMarker,
}

impl Inconsistent {
    fn roll_back(self, files: &Files) {
        log(match self {
            Inconsistent::MarkerWithoutBinary => {
                "the trial marker without kindil.trying: dropping the marker"
            }
            Inconsistent::BinaryWithoutMarker => {
                "kindil.trying without the trial marker: dropping the untried update"
            }
        });
        files.drop_trial();
    }
}

use steps::{trial, Staged, Trial};

/// An update's trial as typestate: a staged binary runs on trial only once
/// it is moved in, copied and marked, and a trial can always be rolled back;
/// only kindil keeps it (src/update.rs). The files are the state between
/// runs: [`Trial::read`] rebuilds it from them. No state can be made outside
/// this module but through [`Trial::read`] (the [`steps::Seal`] can't be).
mod steps {
    use super::{log, Files, Inconsistent};
    use statum::{machine, state, transition, validators, Error};

    /// What only this module can make.
    pub(super) struct Seal(());

    #[state]
    pub(super) enum TrialState {
        /// No update to try.
        Idle,
        /// `kindil.next`, written by kindil after checking its signature.
        Staged,
        /// Moved to `kindil.trying`, copied to run, and marked.
        Trying,
    }

    #[machine]
    pub(super) struct Trial<TrialState> {
        seal: Seal,
    }

    #[transition]
    impl Trial<Staged> {
        /// One trial run: moved to `kindil.trying`, copied to run, and
        /// marked. If it can't be, the update is dropped and nothing is on
        /// trial (or, if it couldn't even be moved, it stays staged).
        pub(super) fn begin(
            self,
            files: &Files,
        ) -> ::core::result::Result<Trial<Trying>, Trial<Idle>> {
            if std::fs::rename(&files.next, &files.trial).is_err() {
                return Err(self.transition());
            }
            match files.copy(&files.trial) {
                Ok(()) if std::fs::write(&files.trying, b"").is_ok() => {
                    log("trying the update");
                    return Ok(self.transition());
                }
                Ok(()) => {}
                Err(error) => log(&format!("copying the update: {error}; dropping it")),
            }
            let _ = std::fs::remove_file(&files.trial);
            Err(self.transition())
        }
    }

    #[transition]
    impl Trial<Trying> {
        /// Back to the current binary: the trial's binary and marker go.
        pub(super) fn roll_back(self, files: &Files) -> Trial<Idle> {
            files.drop_trial();
            self.transition()
        }
    }

    /// Which trial files exist.
    struct Found {
        next: bool,
        trial: bool,
        marker: bool,
    }

    fn when(holds: bool) -> statum::Result<()> {
        if holds {
            Ok(())
        } else {
            Err(Error::InvalidState)
        }
    }

    #[validators(Trial)]
    impl Found {
        fn is_idle(&self) -> statum::Result<()> {
            when(!self.next && !self.trial && !self.marker)
        }
        fn is_staged(&self) -> statum::Result<()> {
            when(self.next && !self.trial && !self.marker)
        }
        /// An update staged during the trial waits for the trial's end.
        fn is_trying(&self) -> statum::Result<()> {
            when(self.trial && self.marker)
        }
    }

    impl Trial<Idle> {
        /// The trial's state from the files on disk, or what is wrong with
        /// them.
        pub(super) fn read(files: &Files) -> Result<trial::SomeState, Inconsistent> {
            let found = Found {
                next: files.next.is_file(),
                trial: files.trial.exists(),
                marker: files.trying.exists(),
            };
            Trial::rebuild(&found).seal(Seal(())).build().map_err(|_| {
                if found.marker {
                    Inconsistent::MarkerWithoutBinary
                } else {
                    Inconsistent::BinaryWithoutMarker
                }
            })
        }
    }
}

/// Size, and modification time in FAT's 2 s steps (see [`Files::current`]).
fn stamp(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let secs = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some((meta.len(), secs / 2))
}

/// Amazon's UI, which ran until now, left what root runs here the
/// javausers group's to change: /etc/upstart/framework.conf runs `chmod -R
/// g=u /var/local` whenever it starts (src/perms.rs in the app). `path` is
/// made 0755 at most again, through the open file, never through a
/// symlink, and only if it is a regular file of the launcher's user.
fn narrow(path: &Path) {
    match narrowed(path) {
        Ok(Some(was)) => log(&format!(
            "{} was {was:o}: narrowed to {:o}",
            path.display(),
            was & 0o755
        )),
        Ok(None) => {}
        Err(error) => log(&format!("{}: {error}", path.display())),
    }
}

/// Its mode before, if it was narrowed.
fn narrowed(path: &Path) -> std::io::Result<Option<u32>> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    let file = match std::fs::File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let meta = file.metadata()?;
    // SAFETY: geteuid has no preconditions.
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } {
        return Err(std::io::Error::other("not a file of the launcher's user"));
    }
    let was = meta.mode() & 0o7777;
    if was & 0o755 == was {
        return Ok(None);
    }
    file.set_permissions(std::fs::Permissions::from_mode(was & 0o755))?;
    Ok(Some(was))
}

fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// USB drive mode run by kindil (src/drive.rs in the app): volumd stops
/// while kindil runs. Stopping it stops USBNetwork's jobs too (`stop on
/// stopping volumd`), so the way back starts those that ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct UsbDrive {
    usbnet: bool,
    autostart: bool,
}

/// The switch, `/var/local/kindil/usb-drive`, holding `kindil`; otherwise
/// volumd keeps USB drive mode, as without kindil.
const USB_SWITCH: &str = "usb-drive";

impl UsbDrive {
    /// Read before anything is stopped.
    fn read(paths: &Paths, os: &dyn Os) -> Option<Self> {
        let switch = std::fs::read_to_string(paths.run.join(USB_SWITCH)).unwrap_or_default();
        (switch.trim() == "kindil").then(|| Self {
            usbnet: os.running("usbnet"),
            autostart: os.running("usbnet-autostart"),
        })
    }

    /// volumd, and USBNetwork's jobs that ran before.
    fn restore(self, os: &dyn Os) {
        if !os.running("volumd") {
            // usbnet's start toggles USBNetwork: over a g_ether still loaded
            // it would switch it off. Without it, it comes up as at boot.
            if self.usbnet && os.module_loaded("g_ether") {
                log("unloading g_ether for USBNetwork to start afresh");
                os.unload_module("g_ether");
            }
            log("starting volumd");
            os.start("volumd");
        }
        for (job, ran) in [
            ("usbnet", self.usbnet),
            ("usbnet-autostart", self.autostart),
        ] {
            if ran && !os.running(job) {
                os.start(job);
            }
        }
    }
}

/// The session: Amazon's UI and services step aside, kindil runs, and
/// they come back. Its result, [`Ended`], comes only from refusing before
/// anything was stopped or from restoring.
pub fn session(paths: &Paths, os: &dyn Os) -> Ended {
    crate::os::ignore(libc::SIGHUP);
    // Before anything runs: the launcher itself (the boot job and KUAL run
    // it next time), and below, in pick, the binary it runs.
    narrow(&paths.run.join("launcher"));
    let files = Files::new(paths);
    // A trial cut short by a reboot or a kill never settled: drop it, so the
    // current binary runs and nothing keeps the untried one.
    if files.end_trial() {
        log("dropped an update whose trial was cut short");
    }
    // Event watchers an earlier kindil left behind (orphans, now init's);
    // they ignore TERM.
    os.kill_orphaned_watchers();
    end_children(paths, os);
    let ready = match Session::start().pick(&files) {
        Ok(ready) => ready,
        Err(ended) => return ended,
    };
    let aside = ready.step_aside(paths, os);
    std::thread::scope(|scope| {
        // The rest in the background, so kindil starts at once. Ended before
        // anything is brought back, so the two never race. Through Builder:
        // scope.spawn panics if no thread can be made; then one round now.
        let (done, rounds) = mpsc::channel::<()>();
        let usb_drive = aside.state_data.usb.is_some();
        let stopper = std::thread::Builder::new()
            .spawn_scoped(scope, move || {
                stop_services(os, &rounds, STOP_ROUNDS, usb_drive);
            })
            .map_err(|error| {
                log(&format!(
                    "no thread for the stopper ({error}); stopping once"
                ));
                stop_services(os, &mpsc::channel().1, 1, usb_drive);
            });
        // Amazon's screen modules would draw over kindil in USB drive mode
        // and after it; kindil draws its own screens. Only Amazon's UI loads
        // them.
        if aside.state_data.gui {
            os.lipc_set("com.lab126.blanket", "unload", "usb");
            os.lipc_set("com.lab126.blanket", "unload", "splash");
        }
        os.clear_screen();
        aside.run(paths, os, &files);
        drop(done);
        if let Ok(stopper) = stopper {
            let _ = stopper.join();
        }
        aside.restore(paths, os)
    })
    .end()
}

/// Up to `rounds` rounds STOP_EVERY apart, until `done` ends.
fn stop_services(os: &dyn Os, done: &mpsc::Receiver<()>, rounds: u32, usb_drive: bool) {
    for round in 1..=rounds {
        // kindil runs USB drive mode, and waits for volumd to go. It started
        // at boot, as kindil's boot job needs; USBNetwork's jobs go with it,
        // their post-stop taking a few seconds.
        if usb_drive && os.running("volumd") {
            log("stopping volumd: kindil runs USB drive mode");
            os.stop("volumd");
        }
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

/// Runs kindil, `bin` first, until it exits for good.
fn run(paths: &Paths, os: &dyn Os, files: &Files, mut slot: PathBuf, mut bin: PathBuf) {
    let config = paths.config.to_string_lossy().into_owned();
    let args: Vec<&str> = if paths.config.is_file() {
        vec!["--config", &config, "kindle"]
    } else {
        vec!["kindle"]
    };
    let mut crashes: Vec<u64> = Vec::new();
    loop {
        log(&os.version(&bin).unwrap_or_else(|| "kindil ?".into()));
        let status = os.run(&bin, &args);
        log(&format!("kindil exited ({status})"));
        if files.end_trial() {
            // The trial ended before kindil kept it: back to the previous
            // binary.
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
        bin = files.pick(&slot);
    }
}

/// kindil's own children (Wi-Fi's, its SSH server) from a run that crashed
/// or was killed: stopped before it runs again, which starts them afresh,
/// and before its binary is replaced, which udhcpc runs as its hook.
fn end_children(paths: &Paths, os: &dyn Os) {
    for pid_file in paths.child_pids() {
        os.end_pid_file(pid_file);
    }
}

/// kindil ran Wi-Fi itself (src/net.rs): its DHCP client and wpa_supplicant
/// go, and so does its SSH server (src/ssh.rs) with the sessions open over
/// it, and wifid starts Amazon's supplicant afresh. The driver stays loaded:
/// unloading it can hang, and wifid's driver job is fine with it loaded.
/// Only a Wi-Fi child's pid file says kindil ran Wi-Fi: the SSH server's
/// doesn't, and Amazon's supplicant then stays.
fn wifi_back(paths: &Paths, os: &dyn Os) {
    let mut ran = false;
    for pid_file in &paths.wifi_pids {
        if pid_file.exists() {
            os.end_pid_file(pid_file);
            ran = true;
        }
    }
    os.end_pid_file(&paths.sshd_pid);
    if ran && !os.running("wifid") {
        os.stop("wifis");
    }
    if !os.running("wifid") {
        os.start("wifid");
    }
}

pub use lifecycle::Ended;
use lifecycle::Session;

/// The session as typestate: once Amazon's UI, X or powerd have stopped
/// (`Aside`), the only way to the session's result, [`Ended`], is
/// `Session::restore`. What that covers, and what it can't:
///
/// - Every return: [`session`] must return an [`Ended`], which only
///   `pick`'s refusal (before anything stopped) and a `Restored`
///   session make; once `step_aside` took the `Ready` session, a refusal is
///   no longer to be had. The transitions are `#[must_use]`.
/// - Not a `Session<Aside>` dropped on purpose: Rust has no linear types,
///   so code could drop one and then loop forever or exit the process.
///   None does.
/// - Not a panic: the release build aborts, so no `Drop` runs on one, and
///   a drop guard would cover nothing the types don't. The launcher keeps
///   to code that doesn't panic instead (no indexing, no unwraps), and a
///   reboot is always a way back (see the module's head).
/// - Not a kill: SIGKILL ends the launcher wherever it is; HUP and TERM
///   are ignored.
mod lifecycle {
    use super::{executable, log, run, wifi_back, Files, UsbDrive};
    use crate::os::Os;
    use crate::{Paths, SERVICES};
    use statum::{machine, state, transition};
    use std::path::PathBuf;
    use std::process::ExitCode;

    /// What only this module can make.
    pub(super) struct Seal(());

    /// How a session ended: `main`'s exit code.
    #[must_use = "the session's result is its exit code"]
    pub struct Ended(ExitCode);

    impl Ended {
        pub fn code(self) -> ExitCode {
            self.0
        }
    }

    /// The binary to run, and the slot it came from.
    pub(super) struct Picked {
        slot: PathBuf,
        bin: PathBuf,
    }

    /// What stepped aside, to bring back.
    pub(super) struct Away {
        pub(super) gui: bool,
        pub(super) usb: Option<UsbDrive>,
        picked: Picked,
    }

    #[state]
    pub(super) enum SessionState {
        /// Nothing picked or stopped yet.
        Starting,
        /// A runnable binary picked; nothing stopped yet.
        Ready(Picked),
        /// Amazon's UI, X and powerd stopped (the rest stop in the
        /// background): only `restore` leaves.
        Aside(Away),
        /// Amazon's UI and everything it needs started again.
        Restored,
    }

    #[machine]
    pub(super) struct Session<SessionState> {
        seal: Seal,
    }

    impl Session<Starting> {
        /// Through a state without data: statum's builder for one with data
        /// keeps a panic (an `expect` it never takes) in the release build.
        pub(super) fn start() -> Self {
            Self::builder().seal(Seal(())).build()
        }
    }

    #[transition]
    impl Session<Starting> {
        /// The binary to run, or the session refused with nothing stopped.
        pub(super) fn pick(self, files: &Files) -> ::core::result::Result<Session<Ready>, Ended> {
            let slot = files.installed.clone();
            let bin = files.pick(&slot);
            if !executable(&bin) {
                log(&format!("missing {}", bin.display()));
                return Err(Ended(ExitCode::FAILURE));
            }
            Ok(self.transition_with(Picked { slot, bin }))
        }
    }

    #[transition]
    impl Session<Ready> {
        /// Amazon's UI, X and powerd stop; what USB drive mode needs back is
        /// read first.
        #[must_use = "once Amazon's UI has stopped, only restore() brings it back"]
        pub(super) fn step_aside(self, paths: &Paths, os: &dyn Os) -> Session<Aside> {
            let usb = UsbDrive::read(paths, os);
            // Stopping the GUI job signals its whole tree, which can include
            // us.
            crate::os::ignore(libc::SIGTERM);
            // A standalone boot never started it.
            let gui = os.running("lab126_gui");
            if gui {
                // Let KUAL close its menu before the UI goes away.
                os.sleep(std::time::Duration::from_secs(2));
                log("stopping the Kindle UI");
                os.stop("lab126_gui");
                os.sleep(std::time::Duration::from_secs(2));
            }
            // kindil draws to the framebuffer itself: X has nothing to do
            // while it runs. The way back starts it again, and Amazon's UI
            // with it.
            if os.running("x") {
                os.stop("x");
            }
            // kindil does powerd's work itself: battery, front light,
            // charging limits, sleep and the power button. Stopped first,
            // and before kindil starts: stopping powerd switches the front
            // light off, which kindil then sets.
            if os.running("powerd") {
                log("stopping powerd");
                os.stop("powerd");
            }
            self.transition_map(|picked| Away { gui, usb, picked })
        }
    }

    impl Session<Aside> {
        /// Runs kindil until it exits for good.
        pub(super) fn run(&self, paths: &Paths, os: &dyn Os, files: &Files) {
            let picked = &self.state_data.picked;
            run(paths, os, files, picked.slot.clone(), picked.bin.clone());
        }
    }

    #[transition]
    impl Session<Aside> {
        /// Amazon's UI back, with everything it needs.
        #[must_use = "the session ends with end()"]
        pub(super) fn restore(self, paths: &Paths, os: &dyn Os) -> Session<Restored> {
            let (gui, usb) = (self.state_data.gui, self.state_data.usb);
            // A standalone boot skipped Amazon's framework: without it,
            // Amazon's UI is an empty screen. `launcher boot` sets the flag
            // again after a boot kindil stays up.
            let _ = std::fs::remove_file(&paths.skip);
            // kindil may have paused charging for the temperature
            // (src/power.rs); powerd, back now, keeps its own range.
            let _ = std::fs::write(&paths.charger, "1");
            if !os.running("powerd") {
                os.start("powerd");
            }
            // Before Amazon's UI, which expects the storage volumd's. Its
            // start runs kindil's boot job again, which does nothing twice in
            // a boot.
            if let Some(usb) = usb {
                usb.restore(os);
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
            self.transition()
        }
    }

    impl Session<Restored> {
        pub(super) fn end(self) -> Ended {
            Ended(ExitCode::SUCCESS)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::os::fake::Fake;
    use std::collections::BTreeSet;
    use std::process::ExitCode;

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
                wifi_pids: vec![t.join("udhcpc.pid"), t.join("wpa.pid")],
                sshd_pid: t.join("sshd.pid"),
                pid_file: t.join("kindil.pid"),
                boot_id: t.join("boot_id"),
                overlays_on: vec![t.join("user/OVERLAYS")],
                overlay_dir: t.join("overlay-copies"),
                root: t.join("root"),
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
            session(&self.paths, os).code()
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

    #[test]
    fn inconsistent_trial_files_roll_back_and_the_current_binary_runs() {
        for leftover in ["kindil.trying", "marker"] {
            let env = Env::new();
            env.fake(&env.ext("kindil"), "1.0.0", "exit 0");
            if leftover == "marker" {
                std::fs::write(env.paths.run.join("trying"), "").unwrap();
            } else {
                env.fake(&env.ext("kindil.trying"), "2.0.0", "exit 0");
            }
            env.session(&Fake::default());
            assert_eq!(env.files(), "kindil", "{leftover}");
            assert!(!env.paths.run.join("trying").exists(), "{leftover}");
            assert_eq!(env.running(), "kindil 1.0.0", "{leftover}");
        }
    }

    #[test]
    fn the_trial_files_rebuild_one_state_or_say_whats_wrong() {
        let env = Env::new();
        let files = Files::new(&env.paths);
        let read = || match Trial::read(&files) {
            Ok(trial::SomeState::Idle(_)) => "idle".to_owned(),
            Ok(trial::SomeState::Staged(_)) => "staged".to_owned(),
            Ok(trial::SomeState::Trying(_)) => "trying".to_owned(),
            Err(error) => format!("{error:?}"),
        };
        for (next, trial, marker, want) in [
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
                (next, &files.next),
                (trial, &files.trial),
                (marker, &files.trying),
            ] {
                let _ = std::fs::remove_file(path);
                if on {
                    std::fs::write(path, "").unwrap();
                }
            }
            assert_eq!(read(), want, "next {next}, trial {trial}, marker {marker}");
        }
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
        // Same size and time, different content: only a copy would bring
        // the content back.
        let bin = env.paths.run.join("kindil");
        let modified = std::fs::metadata(&bin).unwrap().modified().unwrap();
        let other = std::fs::read_to_string(&bin)
            .unwrap()
            .replace("1.0.0", "9.9.9");
        std::fs::write(&bin, other).unwrap();
        set_time(&bin, modified);
        env.session(&Fake::default());
        assert_eq!(env.running(), "kindil 9.9.9");
        env.fake(&env.ext("kindil"), "1.1.0", "exit 0");
        let later = std::time::SystemTime::now() + Duration::from_secs(3600);
        set_time(&env.ext("kindil"), later);
        env.session(&Fake::default());
        assert_eq!(env.running(), "kindil 1.1.0");
        // Same time, another size: copied too.
        env.fake(&env.ext("kindil"), "1.1.10", "exit 0");
        set_time(&env.ext("kindil"), later);
        env.session(&Fake::default());
        assert_eq!(env.running(), "kindil 1.1.10");
    }

    fn set_time(path: &Path, time: std::time::SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(time)
            .unwrap();
    }

    fn inode(path: &Path) -> u64 {
        std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(path).unwrap())
    }

    /// 2026-11-15 15:40: the copy made at 15:34 from a source written at an
    /// odd second (as the kernel's inode had it) wasn't current once the
    /// source read as FAT keeps it on disk, a second earlier, and was
    /// copied again.
    #[test]
    fn a_source_whose_time_fat_rounds_to_even_seconds_isnt_copied_again() {
        let env = Env::new();
        let source = env.ext("kindil");
        env.fake(&source, "1.0.0", "exit 0");
        let odd = UNIX_EPOCH + Duration::from_secs(1_794_771_277);
        set_time(&source, odd);
        let files = Files::new(&env.paths);
        assert_eq!(files.pick(&source), files.bin);
        let copied = inode(&files.bin);
        // The kernel drops the user storage's inode and reads it again.
        set_time(&source, odd - Duration::from_secs(1));
        assert!(files.current(&source));
        assert_eq!(files.pick(&source), files.bin);
        assert_eq!(inode(&files.bin), copied, "not written again");
        // A source two seconds on is another one.
        set_time(&source, odd + Duration::from_secs(1));
        assert!(!files.current(&source));
    }

    /// The system storage as tight as on 2026-11-15: `free` bytes, and the
    /// old copy run by a lifeline or not.
    struct Tight {
        free: u64,
        in_use: bool,
    }

    impl Storage for Tight {
        fn free(&self, _: &Path) -> Option<u64> {
            Some(self.free)
        }
        fn in_use(&self, _: &std::fs::Metadata) -> bool {
            self.in_use
        }
    }

    #[test]
    fn with_no_room_for_a_copy_the_old_one_stays_and_the_installed_one_runs_where_it_is() {
        let env = Env::new();
        let source = env.ext("kindil");
        env.fake(&source, "1.0.0", "exit 0");
        Files::new(&env.paths).pick(&source);
        env.fake(&source, "1.1.0", "exit 0");
        set_time(
            &source,
            std::time::SystemTime::now() + Duration::from_secs(3600),
        );
        let size = std::fs::metadata(&source).unwrap().len();
        // Only the old copy's space would make room, and a lifeline runs it.
        let tight = |in_use| {
            Files::with(
                &env.paths,
                Box::new(Tight {
                    free: MARGIN + size - 1,
                    in_use,
                }),
            )
        };
        let files = tight(true);
        let old = inode(&files.bin);
        assert_eq!(files.pick(&source), source);
        assert_eq!(inode(&files.bin), old, "the old copy untouched");
        assert!(files.from.exists());
        assert_eq!(env.running(), "kindil 1.0.0");
        // Once nothing runs it, unlinking it makes the room.
        let files = tight(false);
        assert_eq!(files.pick(&source), files.bin);
        assert_eq!(env.running(), "kindil 1.1.0");
    }

    #[test]
    fn a_failed_copy_runs_the_installed_binary_where_it_is() {
        let env = Env::new();
        // Neither unlinked nor written over: the copy fails.
        std::fs::create_dir(env.paths.run.join("kindil")).unwrap();
        let ran = env.t().join("ran");
        env.fake(
            &env.ext("kindil"),
            "1.0.0",
            &format!("echo \"$0\" >{}; exit 0", ran.display()),
        );
        let os = kual();
        assert_eq!(env.session(&os), ExitCode::SUCCESS);
        assert_eq!(
            std::fs::read_to_string(&ran).unwrap().trim(),
            env.ext("kindil").display().to_string()
        );
        assert!(os.running("lab126_gui"), "and Amazon's UI back after");
    }

    #[test]
    fn with_neither_a_copy_nor_a_runnable_installed_binary_nothing_is_stopped() {
        let env = Env::new();
        std::fs::create_dir(env.paths.run.join("kindil")).unwrap();
        std::fs::write(env.ext("kindil"), "not runnable").unwrap();
        let os = kual();
        assert_eq!(env.session(&os), ExitCode::FAILURE);
        assert_eq!(os.upstart(), "");
    }

    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::symlink_metadata(path)
            .unwrap()
            .permissions()
            .mode()
            & 0o7777
    }

    /// What /etc/upstart/framework.conf does to /var/local whenever Amazon's
    /// UI starts: `chmod -R g=u`, and setgid on directories.
    fn framework_starts(dir: &Path) {
        use std::os::unix::fs::PermissionsExt;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if meta.is_symlink() {
                continue;
            }
            let was = mode(&path);
            let widened = (was & 0o7707) | ((was & 0o700) >> 3);
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(widened)).unwrap();
            if meta.is_dir() {
                framework_starts(&path);
                let setgid = std::fs::Permissions::from_mode(widened | 0o2000);
                let _ = std::fs::set_permissions(&path, setgid);
            }
        }
    }

    #[test]
    fn what_root_runs_is_narrowed_again_before_it_runs() {
        let env = Env::new();
        let bin = env.paths.run.join("kindil");
        // Each run notes the mode it runs with, and its inode: the same
        // inode means the copy wasn't written again.
        let note = format!(
            "ls -li {} | awk '{{print $1, substr($2, 1, 10)}}' >>{}; exit 0",
            bin.display(),
            env.t().join("runs").display()
        );
        env.fake(&env.ext("kindil"), "1.0.0", &note);
        let launcher = env.paths.run.join("launcher");
        env.fake(&launcher, "launcher", "exit 0");
        std::os::unix::fs::symlink(&bin, env.paths.run.join("kindil-dhcp")).unwrap();
        env.session(&Fake::default());
        // Amazon's UI after it: both group-writable.
        framework_starts(env.t());
        assert_eq!((mode(&bin), mode(&launcher)), (0o775, 0o775));
        env.session(&Fake::default());
        assert_eq!((mode(&bin), mode(&launcher)), (0o755, 0o755));
        let runs = env.runs();
        let runs: Vec<&str> = runs.split(' ').collect();
        assert_eq!(runs.len(), 4, "{runs:?}");
        assert_eq!(runs[0], runs[2], "the copy was kept, not written again");
        assert_eq!((runs[1], runs[3]), ("-rwxr-xr-x", "-rwxr-xr-x"));
    }

    #[test]
    fn narrows_only_a_file_of_its_own_never_through_a_symlink() {
        use std::os::unix::fs::PermissionsExt;
        let env = Env::new();
        let elsewhere = env.t().join("elsewhere");
        std::fs::write(&elsewhere, "").unwrap();
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o777)).unwrap();
        let link = env.t().join("link");
        std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
        assert!(narrowed(&link).is_err());
        assert!(narrowed(env.t()).is_err(), "a directory isn't narrowed");
        assert_eq!(narrowed(&env.t().join("none")).unwrap(), None);
        assert_eq!(mode(&elsewhere), 0o777);
        assert_eq!(narrowed(&elsewhere).unwrap(), Some(0o777));
        assert_eq!(mode(&elsewhere), 0o755);
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
        let wpa = &env.paths.wifi_pids[1];
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
        let [dhcp, wpa] = [&env.paths.wifi_pids[0], &env.paths.wifi_pids[1]];
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
    fn leaving_with_only_the_ssh_server_left_keeps_amazons_supplicant() {
        let env = Env::new();
        let os = Fake::default();
        let ssh = &env.paths.sshd_pid;
        env.fake(
            &env.ext("kindil"),
            "1.0.0",
            &format!("echo 7 >{}; exit 0", ssh.display()),
        );
        env.session(&os);
        assert!(!os.upstart().contains("stop wifis"));
        assert!(os.running("wifid"));
        assert!(!ssh.exists());
        assert_eq!(*os.ended.lock().unwrap(), std::slice::from_ref(ssh));
    }

    #[test]
    fn a_run_after_a_crash_doesnt_find_the_crashed_runs_wifi_children() {
        let env = Env::new();
        let (runs, wpa) = (env.t().join("runs"), &env.paths.wifi_pids[1]);
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

    /// From KUAL, with volumd and USBNetwork's jobs up as on the PW1.
    fn kual_with_volumd() -> Fake {
        let os = kual();
        for job in ["volumd", "usbnet", "usbnet-autostart"] {
            os.jobs.lock().unwrap().insert(job.into());
        }
        os
    }

    #[test]
    fn volumd_stays_unless_kindil_runs_usb_drive_mode() {
        let env = Env::new();
        let os = kual_with_volumd();
        env.fake(&env.ext("kindil"), "1.0.0", "exit 0");
        std::fs::write(env.paths.run.join(USB_SWITCH), "amazon\n").unwrap();
        env.session(&os);
        let during = os.during.lock().unwrap().clone().unwrap();
        assert!(during.contains("volumd") && during.contains("usbnet"));
        assert!(!os.upstart().contains("volumd"), "{}", os.upstart());
    }

    #[test]
    fn kindils_usb_drive_mode_stops_volumd_and_brings_it_back_with_usbnetwork() {
        let env = Env::new();
        let os = kual_with_volumd();
        let before = os.jobs();
        env.fake(&env.ext("kindil"), "1.0.0", "exit 0");
        std::fs::write(env.paths.run.join(USB_SWITCH), "kindil\n").unwrap();
        env.session(&os);
        let during = os.during.lock().unwrap().clone().unwrap();
        for job in ["volumd", "usbnet", "usbnet-autostart"] {
            assert!(!during.contains(job), "{job} stopped while kindil runs");
        }
        assert_eq!(os.jobs(), before, "every one of them back");
        let upstart = os.upstart.lock().unwrap().clone();
        let at = |line: &str| upstart.iter().position(|l| l == line).unwrap();
        assert!(at("start volumd") < at("start usbnet-autostart"));
        assert!(at("start volumd") < at("start x"), "before Amazon's UI");
        assert!(!upstart.contains(&"rmmod g_ether".to_owned()));
    }

    #[test]
    fn usbnetwork_starts_afresh_without_a_leftover_g_ether() {
        let env = Env::new();
        let os = kual_with_volumd();
        os.modules.lock().unwrap().insert("g_ether".into());
        env.fake(&env.ext("kindil"), "1.0.0", "exit 0");
        std::fs::write(env.paths.run.join(USB_SWITCH), "kindil").unwrap();
        env.session(&os);
        let upstart = os.upstart.lock().unwrap().clone();
        let at = |line: &str| upstart.iter().position(|l| l == line).unwrap();
        assert!(at("rmmod g_ether") < at("start volumd"));
    }

    #[test]
    fn usbnetwork_that_didnt_run_isnt_started() {
        let env = Env::new();
        let os = kual();
        os.jobs.lock().unwrap().insert("volumd".into());
        os.modules.lock().unwrap().insert("g_ether".into());
        env.fake(&env.ext("kindil"), "1.0.0", "exit 0");
        std::fs::write(env.paths.run.join(USB_SWITCH), "kindil").unwrap();
        env.session(&os);
        assert!(os.running("volumd"));
        // upstart itself starts usbnet with volumd; the launcher adds nothing.
        assert!(!os.upstart().contains("start usbnet-autostart"));
        assert!(!os.upstart().contains("rmmod"));
    }

    #[test]
    fn kindils_ssh_server_ends_before_each_run_and_on_the_way_out() {
        let env = Env::new();
        let os = Fake::default();
        let (runs, ssh) = (env.t().join("runs"), &env.paths.sshd_pid);
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
