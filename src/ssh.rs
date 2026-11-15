//! SSH, run by kindil itself.
//!
//! kindil carries its own SSH server, kindil-sshd (sshd/: Rust, on russh),
//! writes it out to the system storage and runs it as its child, started
//! again with a growing pause if it keeps dying, like Wi-Fi's children
//! (src/net.rs). Nothing of it lives on /mnt/us, so it never holds the user
//! storage a computer takes in USB drive mode; USBNetwork's dropbear runs
//! from there.
//!
//! Keys only, no passwords. The trusted keys are kindil's own list, which
//! USBNetwork's seeds on the first start only (src/ssh_keys.rs); keys added
//! on the USB storage later wait to be trusted in Settings. The host key is
//! the server's own,
//! made on its first start. The server ends with kindil ([`stop`]), and
//! the launcher stops it by its pid file on the way out and before each run
//! (launcher/src/main.rs), and shuts its port; sessions open end with it.
//! Its log is its own, small file ([`LOG`]).
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// kindil's SSH port for now, alongside USBNetwork's dropbear on 22. Once
/// kindil's own server has proven itself on the device, this becomes 22
/// and USBNetwork's goes.
pub const PORT: u16 = 2222;
/// Its pid file; the launcher ends it by this.
pub const PID: &str = "/var/run/kindil-sshd.pid";
/// Its log, which it keeps under 64 KiB (and one older file beside it).
pub const LOG: &str = "/var/local/kindil/sshd.log";
/// Its process name, as /proc/PID/stat gives it.
const NAME: &str = "kindil-sshd";
/// USBNetwork's authorized keys, read by its patched dropbear.
pub const USBNET_AUTHORIZED_KEYS: &str = "/mnt/us/usbnet/etc/authorized_keys";
const BINARY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/kindil-sshd"));
const INTERFACE: &str = "wlan0";
const IPTABLES: &str = "/usr/sbin/iptables";
/// A server that lived this long was fine; a restart after it waits only
/// the shortest pause.
const HEALTHY: Duration = Duration::from_secs(60);
const PAUSE_MIN: Duration = Duration::from_secs(1);
const PAUSE_MAX: Duration = Duration::from_secs(60);

/// Set once kindil is leaving: no more starts.
static STOPPING: AtomicBool = AtomicBool::new(false);
/// Whether the server runs now.
static RUNNING: AtomicBool = AtomicBool::new(false);

/// A line in kindil's log; never panics (a full /var/local, a closed
/// stderr).
fn say(message: std::fmt::Arguments) {
    let _ = std::io::stderr().write_all(format!("kindil: {message}\n").as_bytes());
}

/// Whether kindil's SSH server is running.
pub fn running() -> bool {
    RUNNING.load(Ordering::SeqCst)
}

/// Where kindil's SSH server keeps its files; the tests point it elsewhere.
pub struct Layout {
    /// The written-out kindil-sshd.
    pub binary: PathBuf,
    /// Root's only: the host key (`host_ed25519`) and the authorized keys.
    pub dir: PathBuf,
    pub log: PathBuf,
    pub pid: PathBuf,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            binary: "/var/local/kindil/kindil-sshd".into(),
            dir: "/var/local/kindil/ssh".into(),
            log: LOG.into(),
            pid: PID.into(),
        }
    }
}

impl Layout {
    /// kindil-sshd reads `authorized_keys` in its directory.
    pub fn authorized_keys(&self) -> PathBuf {
        self.dir.join("authorized_keys")
    }

    /// Fingerprints of USB keys someone chose not to trust.
    pub fn ignored_keys(&self) -> PathBuf {
        self.dir.join("ignored_keys")
    }

    /// kindil-sshd's arguments.
    pub fn args(&self, port: u16) -> Vec<String> {
        [
            "--port",
            &port.to_string(),
            "--dir",
            &self.dir.to_string_lossy(),
            "--log",
            &self.log.to_string_lossy(),
        ]
        .map(str::to_owned)
        .to_vec()
    }

    /// Everything the server needs: the binary (written only when it
    /// changed), its directory and, the first time, its trusted keys. It
    /// makes its host key itself.
    ///
    /// Every start also closes again what Amazon's framework opened: each
    /// time the Kindle UI starts (whenever kindil hands the screen back),
    /// /etc/upstart/framework.conf runs `chgrp -R javausers /var/local;
    /// chmod -R g=u /var/local` and setgids every directory there, so a
    /// 0600 file comes back 0660 and the 0700 directory 02770. kindil-sshd
    /// rightly refuses a group-writable `authorized_keys`, and only fixes
    /// its host key itself.
    pub fn prepare(&self, binary: &[u8], usbnet_keys: &Path) -> anyhow::Result<()> {
        anyhow::ensure!(
            !binary.is_empty(),
            "this kindil was built without its SSH server"
        );
        if install(binary, &self.binary)? {
            say(format_args!(
                "SSH server written to {}",
                self.binary.display()
            ));
        }
        if let Some(was) = own_dir(&self.dir)? {
            say(format_args!(
                "{} was {was:o}: made 0700 again",
                self.dir.display()
            ));
        }
        for file in [&self.authorized_keys(), &self.ignored_keys(), &self.log] {
            match crate::perms::narrow(file, 0o600) {
                Ok(Some(was)) => say(format_args!(
                    "{} was {was:o}: made 0600 again",
                    file.display()
                )),
                Ok(None) => {}
                Err(error) => say(format_args!("{}: {error}", file.display())),
            }
        }
        // The first start only; /mnt/us may be gone (USB drive mode).
        match crate::ssh_keys::bootstrap(usbnet_keys, &self.authorized_keys()) {
            Ok(true) => say(format_args!("SSH keys taken from USBNetwork's, once")),
            Ok(false) => {}
            Err(error) => say(format_args!("SSH keys from USBNetwork: {error}")),
        }
        Ok(())
    }
}

/// Starts kindil's SSH server in the background and keeps it running for as
/// long as kindil runs.
pub fn start() {
    std::thread::spawn(|| supervise(&Layout::default()));
}

/// kindil is leaving: its server goes, and its port is shut. (The launcher
/// does both too, for a kindil that crashed.)
pub fn stop() {
    STOPPING.store(true, Ordering::SeqCst);
    end_stale(Path::new(PID));
    close_port(PORT);
}

fn supervise(layout: &Layout) {
    // One an earlier kindil left behind holds the port.
    end_stale(&layout.pid);
    let mut pause = PAUSE_MIN;
    while !STOPPING.load(Ordering::SeqCst) {
        let since = Instant::now();
        match spawn(layout) {
            Ok(mut child) => {
                RUNNING.store(true, Ordering::SeqCst);
                say(format_args!("SSH server on port {PORT}"));
                let status = child.wait();
                RUNNING.store(false, Ordering::SeqCst);
                forget_pid(&layout.pid, child.id());
                say(format_args!("SSH server exited ({status:?})"));
            }
            Err(error) => say(format_args!("SSH server: {error:#}")),
        }
        pause = backoff(pause, since.elapsed());
        std::thread::sleep(pause);
    }
}

fn spawn(layout: &Layout) -> anyhow::Result<Child> {
    layout.prepare(BINARY, Path::new(USBNET_AUTHORIZED_KEYS))?;
    if let Err(error) = open_port(PORT) {
        say(format_args!("opening port {PORT}: {error:#}"));
    }
    let mut command = Command::new(&layout.binary);
    // Its log is its own file; stderr (kindil's log) gets only a panic.
    command
        .args(layout.args(PORT))
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    let child = launch(&mut command, |pid| {
        write_new(&layout.pid, format!("{pid}\n").as_bytes(), 0o644)?;
        if STOPPING.load(Ordering::SeqCst) {
            return Err(std::io::Error::other("kindil is leaving"));
        }
        Ok(())
    })?;
    Ok(child)
}

/// Starts `command` and has `publish` record its pid; if that fails, the
/// child is killed and reaped rather than left running unrecorded.
pub fn launch(
    command: &mut Command,
    publish: impl FnOnce(u32) -> std::io::Result<()>,
) -> std::io::Result<Child> {
    let mut child = command.spawn()?;
    if let Err(error) = publish(child.id()) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    Ok(child)
}

/// Removes the pid file if it still names `pid`.
pub fn forget_pid(file: &Path, pid: u32) {
    let named = std::fs::read_to_string(file)
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok());
    if named == Some(pid) {
        let _ = std::fs::remove_file(file);
    }
}

/// Ends the server a pid file names, only if that process is kindil-sshd
/// (a pid outlives its process); the file goes either way.
pub fn end_stale(file: &Path) {
    let pid = std::fs::read_to_string(file)
        .ok()
        .and_then(|text| text.trim().parse::<i32>().ok());
    match (pid, file.to_str()) {
        (Some(pid), Some(path)) if process_name(pid).as_deref() == Some(NAME) => {
            crate::net::terminate(path)
        }
        _ => {
            let _ = std::fs::remove_file(file);
        }
    }
}

/// A process's name, from /proc/PID/stat (2.6.31 has no /proc/PID/comm).
pub fn process_name(pid: i32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    name_in_stat(&stat).map(str::to_owned)
}

/// "PID (NAME) STATE ...": the name may hold spaces and parentheses.
pub fn name_in_stat(stat: &str) -> Option<&str> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    stat.get(open + 1..close)
}

/// The pause before the next start: the shortest after a server that ran
/// well, otherwise twice the last, up to a minute.
pub fn backoff(last: Duration, lived: Duration) -> Duration {
    if lived >= HEALTHY {
        PAUSE_MIN
    } else {
        (last * 2).min(PAUSE_MAX)
    }
}

/// Deletes every copy of the rule that lets `port` in.
fn close_port(port: u16) {
    // Its iptables (1.3.8) has no -C: delete until there is none.
    for _ in 0..8 {
        let removed = Command::new(IPTABLES)
            .args(firewall_rule("-D", port))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if !removed.is_ok_and(|status| status.success()) {
            break;
        }
    }
}

/// Amazon's firewall drops everything arriving over Wi-Fi but its own
/// ports (and 22, which USBNetwork opens): `port` is let in, once.
fn open_port(port: u16) -> anyhow::Result<()> {
    close_port(port);
    let status = Command::new(IPTABLES)
        .args(firewall_rule("-A", port))
        .status()?;
    anyhow::ensure!(status.success(), "iptables exited with {status}");
    Ok(())
}

/// iptables' arguments to add (`-A`) or delete (`-D`) the rule that lets
/// `port` in over Wi-Fi.
pub fn firewall_rule(action: &str, port: u16) -> Vec<String> {
    [
        action,
        "INPUT",
        "-i",
        INTERFACE,
        "-p",
        "tcp",
        "--dport",
        &port.to_string(),
        "-j",
        "ACCEPT",
    ]
    .map(str::to_owned)
    .to_vec()
}

/// The server's directory: made 0700 if missing; a real directory of
/// kindil's user (root's), put back to 0700 (setgid off too) if it was
/// opened up. Its mode before, if it was.
pub fn own_dir(dir: &Path) -> anyhow::Result<Option<u32>> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let meta = std::fs::symlink_metadata(dir)?;
    // SAFETY: geteuid has no preconditions.
    let ours = meta.uid() == unsafe { libc::geteuid() };
    anyhow::ensure!(
        meta.is_dir() && ours,
        "{} is not a directory of kindil's",
        dir.display()
    );
    let was = meta.mode() & 0o7777;
    if was == 0o700 {
        return Ok(None);
    }
    set_mode(dir, 0o700)?;
    Ok(Some(was))
}

/// The file at `path`, not through a symlink, open, and what it holds.
fn open_no_symlink(path: &Path) -> std::io::Result<(std::fs::File, Vec<u8>)> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes)?;
    Ok((file, bytes))
}

/// Writes `path` whole or not at all: a new `.part` beside it (O_EXCL,
/// after a stale one goes), synced, renamed over it.
pub fn write_new(path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let part = path.with_extension("part");
    let _ = std::fs::remove_file(&part);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&part)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    // The mode as asked, whatever the umask.
    set_mode(&part, mode)?;
    std::fs::rename(&part, path)
}

/// Writes `binary` to `path` unless it is already there (then only made
/// 0755 again if others could change it); whether it wrote it.
pub fn install(binary: &[u8], path: &Path) -> anyhow::Result<bool> {
    if let Ok((file, bytes)) = open_no_symlink(path) {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let meta = file.metadata()?;
        // SAFETY: geteuid has no preconditions.
        let ours = meta.is_file() && meta.uid() == unsafe { libc::geteuid() };
        if ours && bytes == binary {
            // Through the open file: the directory may be the javausers
            // group's to change for now (see Layout::prepare).
            if meta.mode() & 0o7022 != 0 {
                file.set_permissions(std::fs::Permissions::from_mode(0o755))?;
            }
            return Ok(false);
        }
    }
    write_new(path, binary, 0o755)?;
    Ok(true)
}

fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn read_no_symlink(path: &Path) -> std::io::Result<Vec<u8>> {
        open_no_symlink(path).map(|(_, bytes)| bytes)
    }

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn layout(dir: &Path) -> Layout {
        Layout {
            binary: dir.join("kindil-sshd"),
            dir: dir.join("ssh"),
            log: dir.join("sshd.log"),
            pid: dir.join("kindil-sshd.pid"),
        }
    }

    #[test]
    fn lives_on_the_system_storage() {
        let layout = Layout::default();
        assert_eq!(layout.binary, Path::new("/var/local/kindil/kindil-sshd"));
        assert_eq!(
            layout.authorized_keys(),
            Path::new("/var/local/kindil/ssh/authorized_keys")
        );
        assert_eq!(layout.log, Path::new("/var/local/kindil/sshd.log"));
        for path in [&layout.binary, &layout.dir, &layout.log] {
            assert!(!path.starts_with("/mnt/us"), "{}", path.display());
        }
    }

    #[test]
    fn runs_on_its_port_with_its_directory() {
        assert_eq!(
            Layout::default().args(PORT).join(" "),
            "--port 2222 --dir /var/local/kindil/ssh --log /var/local/kindil/sshd.log"
        );
    }

    #[test]
    fn the_launcher_ends_it_by_its_pid_file_and_shuts_its_port() {
        let launcher = include_str!("../launcher/src/main.rs");
        assert!(launcher.contains(&format!("\"{PID}\"")));
        assert!(launcher.contains(&format!("pub const SSH_PORT: u16 = {PORT};")));
    }

    #[test]
    fn reads_a_process_name_from_its_stat() {
        assert_eq!(
            name_in_stat("812 (kindil-sshd) S 1 812 812 0 -1"),
            Some("kindil-sshd")
        );
        // A name may hold spaces and parentheses itself.
        assert_eq!(name_in_stat("9 (a) b (c) R 1"), Some("a) b (c"));
        assert_eq!(name_in_stat("garbage"), None);
    }

    fn alive(pid: u32) -> bool {
        // SAFETY: signal 0 only checks that the process exists.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }

    #[test]
    fn a_child_whose_pid_cant_be_recorded_is_ended() {
        let mut pid = None;
        let result = launch(Command::new("sleep").arg("30"), |child| {
            pid = Some(child);
            Err(std::io::Error::other("no room"))
        });
        assert!(result.is_err());
        let pid = pid.unwrap();
        assert!(!alive(pid), "{pid} still running");
    }

    #[test]
    fn its_pid_file_goes_when_it_exits_unless_another_took_it() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pid");
        write_new(&file, b"41\n", 0o644).unwrap();
        forget_pid(&file, 40);
        assert!(file.exists());
        forget_pid(&file, 41);
        assert!(!file.exists());
    }

    #[test]
    fn a_stale_pid_naming_another_program_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pid");
        let mut other = Command::new("sleep").arg("30").spawn().unwrap();
        write_new(&file, other.id().to_string().as_bytes(), 0o644).unwrap();
        end_stale(&file);
        assert!(!file.exists());
        assert!(other.try_wait().unwrap().is_none(), "sleep was signalled");
        other.kill().unwrap();
        other.wait().unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_stale_kindil_sshd_is_ended() {
        let dir = tempfile::tempdir().unwrap();
        let sleep = dir.path().join(NAME);
        // From PATH: the Nix build sandbox has no /bin/sleep.
        let path = std::env::var_os("PATH").unwrap_or_default();
        let found = std::env::split_paths(&path)
            .map(|dir| dir.join("sleep"))
            .find(|candidate| candidate.is_file())
            .expect("sleep on PATH");
        std::fs::copy(found, &sleep).unwrap();
        let mut stale = Command::new(&sleep).arg("30").spawn().unwrap();
        let file = dir.path().join("pid");
        write_new(&file, stale.id().to_string().as_bytes(), 0o644).unwrap();
        assert_eq!(process_name(stale.id() as i32).as_deref(), Some(NAME));
        end_stale(&file);
        assert!(stale.try_wait().unwrap().is_some(), "still running");
        assert!(!file.exists());
    }

    #[test]
    fn writes_whole_files_and_never_through_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("authorized_keys");
        // A stale .part, even a symlink, is replaced, not written through.
        let elsewhere = dir.path().join("elsewhere");
        std::os::unix::fs::symlink(&elsewhere, path.with_extension("part")).unwrap();
        write_new(&path, b"keys", 0o600).unwrap();
        assert!(!elsewhere.exists());
        assert_eq!(std::fs::read(&path).unwrap(), b"keys");
        assert_eq!(mode(&path), 0o600);
        // A symlink in a file's place is never read as it.
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(read_no_symlink(&link).is_err());
        assert!(install(b"keys", &link).unwrap(), "written over the link");
        assert!(!std::fs::symlink_metadata(&link).unwrap().is_symlink());
    }

    #[test]
    fn keeps_its_directory_to_itself() {
        let dir = tempfile::tempdir().unwrap();
        let own = dir.path().join("ssh");
        own_dir(&own).unwrap();
        assert_eq!(mode(&own), 0o700);
        set_mode(&own, 0o775).unwrap();
        own_dir(&own).unwrap();
        assert_eq!(mode(&own), 0o700);
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&own, &link).unwrap();
        assert!(own_dir(&link).is_err());
    }

    #[test]
    fn lets_its_port_in_over_wifi() {
        assert_eq!(
            firewall_rule("-A", 2222).join(" "),
            "-A INPUT -i wlan0 -p tcp --dport 2222 -j ACCEPT"
        );
        assert_eq!(firewall_rule("-D", 22)[0], "-D");
    }

    #[test]
    fn backs_off_while_it_keeps_dying() {
        let mut pause = PAUSE_MIN;
        let quick = Duration::from_millis(10);
        pause = backoff(pause, quick);
        assert_eq!(pause, Duration::from_secs(2));
        for _ in 0..10 {
            pause = backoff(pause, quick);
        }
        assert_eq!(pause, PAUSE_MAX);
        assert_eq!(backoff(pause, HEALTHY), PAUSE_MIN);
    }

    #[test]
    fn writes_the_binary_only_when_it_changed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kindil-sshd");
        assert!(install(b"one", &path).unwrap());
        assert_eq!(mode(&path), 0o755);
        assert!(!install(b"one", &path).unwrap());
        assert!(install(b"two", &path).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        assert!(!path.with_extension("part").exists());
    }

    #[test]
    fn prepares_binary_directory_and_keys() {
        let dir = tempfile::tempdir().unwrap();
        let layout = layout(dir.path());
        let usbnet = dir.path().join("authorized_keys");
        std::fs::write(&usbnet, "ssh-ed25519 AAAA k\n").unwrap();
        layout.prepare(b"sshd", &usbnet).unwrap();
        assert_eq!(std::fs::read(&layout.binary).unwrap(), b"sshd");
        assert_eq!(mode(&layout.dir), 0o700);
        assert_eq!(mode(&layout.authorized_keys()), 0o600);
        // Again, with the directory opened up: closed again. A key added on
        // the USB storage since isn't taken.
        set_mode(&layout.dir, 0o775).unwrap();
        std::fs::write(&usbnet, "ssh-ed25519 AAAA k\nssh-ed25519 BBBB new\n").unwrap();
        layout.prepare(b"sshd", &usbnet).unwrap();
        assert_eq!(mode(&layout.dir), 0o700);
        assert_eq!(
            std::fs::read_to_string(layout.authorized_keys()).unwrap(),
            "ssh-ed25519 AAAA k\n"
        );
    }

    #[test]
    fn closes_again_what_the_kindle_ui_opened_up() {
        let dir = tempfile::tempdir().unwrap();
        let layout = layout(dir.path());
        let usbnet = dir.path().join("authorized_keys");
        std::fs::write(&usbnet, "ssh-ed25519 AAAA k\n").unwrap();
        layout.prepare(b"sshd", &usbnet).unwrap();
        crate::ssh_keys::ignore(&layout.ignored_keys(), &["SHA256:x".into()]).unwrap();
        std::fs::write(&layout.log, "a line\n").unwrap();
        set_mode(&layout.log, 0o600).unwrap();
        crate::perms::tests::framework_starts(dir.path());
        assert_eq!(mode(&layout.authorized_keys()), 0o660);
        assert_eq!(mode(&layout.dir), 0o770);
        assert_eq!(mode(&layout.binary), 0o775);
        layout.prepare(b"sshd", &usbnet).unwrap();
        let full = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(full(&layout.dir), 0o700);
        assert_eq!(full(&layout.binary), 0o755);
        for file in [layout.authorized_keys(), layout.ignored_keys(), layout.log] {
            assert_eq!(full(&file), 0o600, "{}", file.display());
        }
    }

    #[test]
    fn refuses_a_build_without_its_server() {
        let dir = tempfile::tempdir().unwrap();
        assert!(layout(dir.path())
            .prepare(b"", &dir.path().join("none"))
            .is_err());
    }
}
