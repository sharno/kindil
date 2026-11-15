//! SSH, run by kindil itself.
//!
//! kindil carries its own SSH server, kindil-sshd (sshd/: Rust, on russh),
//! writes it out to the system storage and runs it as its child, started
//! again with a growing pause if it keeps dying, like Wi-Fi's children
//! (src/net.rs). Nothing of it lives on /mnt/us, so it never holds the user
//! storage a computer takes in USB drive mode; USBNetwork's dropbear runs
//! from there.
//!
//! Keys only, no passwords. The keys are USBNetwork's: its
//! `authorized_keys` is copied into kindil's own whenever it is there, and
//! kindil's copy stays for when it isn't. The host key is the server's own,
//! made on its first start. The server ends with kindil: the launcher stops
//! it by its pid file on the way out and before each run
//! (launcher/src/main.rs); sessions already open end with it.
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// kindil's SSH port for now, alongside USBNetwork's dropbear on 22. Once
/// kindil's own server has proven itself on the device, this becomes 22
/// and USBNetwork's goes.
pub const PORT: u16 = 2222;
/// Its pid file; the launcher ends it by this.
pub const PID: &str = "/var/run/kindil-sshd.pid";
/// USBNetwork's authorized keys, read by its patched dropbear.
const USBNET_AUTHORIZED_KEYS: &str = "/mnt/us/usbnet/etc/authorized_keys";
const BINARY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/kindil-sshd"));
const INTERFACE: &str = "wlan0";
const IPTABLES: &str = "/usr/sbin/iptables";
/// A server that lived this long was fine; a restart after it waits only
/// the shortest pause.
const HEALTHY: Duration = Duration::from_secs(60);
const PAUSE_MIN: Duration = Duration::from_secs(1);
const PAUSE_MAX: Duration = Duration::from_secs(60);

/// Where kindil's SSH server keeps its files; the tests point it elsewhere.
pub struct Layout {
    /// The written-out kindil-sshd.
    pub binary: PathBuf,
    /// Root's only: the host key (`host_ed25519`) and the authorized keys.
    pub dir: PathBuf,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            binary: "/var/local/kindil/kindil-sshd".into(),
            dir: "/var/local/kindil/ssh".into(),
        }
    }
}

impl Layout {
    /// kindil-sshd reads `authorized_keys` in its directory.
    pub fn authorized_keys(&self) -> PathBuf {
        self.dir.join("authorized_keys")
    }

    /// kindil-sshd's arguments.
    pub fn args(&self, port: u16) -> Vec<String> {
        [
            "--port",
            &port.to_string(),
            "--dir",
            &self.dir.to_string_lossy(),
        ]
        .map(str::to_owned)
        .to_vec()
    }

    /// Everything the server needs: the binary (written only when it
    /// changed), its directory and the keys. It makes its host key itself.
    pub fn prepare(&self, binary: &[u8], usbnet_keys: &Path) -> anyhow::Result<()> {
        anyhow::ensure!(
            !binary.is_empty(),
            "this kindil was built without its SSH server"
        );
        if install(binary, &self.binary)? {
            eprintln!("kindil: SSH server written to {}", self.binary.display());
        }
        std::fs::create_dir_all(&self.dir)?;
        set_mode(&self.dir, 0o700)?;
        // /mnt/us may be gone (USB drive mode): the last copy serves.
        if let Err(error) = copy_keys(usbnet_keys, &self.authorized_keys()) {
            eprintln!("kindil: SSH keys from USBNetwork: {error}");
        }
        Ok(())
    }
}

/// Starts kindil's SSH server in the background and keeps it running for as
/// long as kindil runs.
pub fn start() {
    std::thread::spawn(|| supervise(&Layout::default()));
}

fn supervise(layout: &Layout) {
    // One an earlier kindil left behind holds the port.
    crate::net::terminate(PID);
    let mut pause = PAUSE_MIN;
    loop {
        let since = Instant::now();
        match spawn(layout) {
            Ok(mut child) => {
                eprintln!("kindil: SSH server on port {PORT}");
                let status = child.wait();
                eprintln!("kindil: SSH server exited ({status:?})");
            }
            Err(error) => eprintln!("kindil: SSH server: {error:#}"),
        }
        pause = backoff(pause, since.elapsed());
        std::thread::sleep(pause);
    }
}

fn spawn(layout: &Layout) -> anyhow::Result<Child> {
    layout.prepare(BINARY, Path::new(USBNET_AUTHORIZED_KEYS))?;
    if let Err(error) = open_port(PORT) {
        eprintln!("kindil: opening port {PORT}: {error:#}");
    }
    // Its log lines (stderr) go to kindil's log.
    let child = Command::new(&layout.binary)
        .args(layout.args(PORT))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()?;
    std::fs::write(PID, child.id().to_string())?;
    Ok(child)
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

/// Amazon's firewall drops everything arriving over Wi-Fi but its own
/// ports (and 22, which USBNetwork opens): `port` is let in, once.
fn open_port(port: u16) -> anyhow::Result<()> {
    // Its iptables (1.3.8) has no -C: drop any copies, then add one.
    for _ in 0..8 {
        let removed = Command::new(IPTABLES)
            .args(firewall_rule("-D", port))
            .stderr(Stdio::null())
            .status()?;
        if !removed.success() {
            break;
        }
    }
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

/// Writes `binary` to `path` unless it is already there, through a file
/// beside it and a rename; whether it wrote it.
pub fn install(binary: &[u8], path: &Path) -> anyhow::Result<bool> {
    if std::fs::read(path).ok().as_deref() == Some(binary) {
        return Ok(false);
    }
    let part = path.with_extension("part");
    std::fs::write(&part, binary)?;
    set_mode(&part, 0o755)?;
    std::fs::rename(&part, path)?;
    Ok(true)
}

/// Copies the authorized keys at `from` to `to`, root's only, when `from`
/// is there and differs; whether it copied. With no `from`, `to` stays.
pub fn copy_keys(from: &Path, to: &Path) -> anyhow::Result<bool> {
    let keys = match std::fs::read(from) {
        Ok(keys) => keys,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if std::fs::read(to).ok().as_deref() == Some(keys.as_slice()) {
        set_mode(to, 0o600)?;
        return Ok(false);
    }
    let part = to.with_extension("part");
    std::fs::write(&part, &keys)?;
    set_mode(&part, 0o600)?;
    std::fs::rename(&part, to)?;
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

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn layout(dir: &Path) -> Layout {
        Layout {
            binary: dir.join("kindil-sshd"),
            dir: dir.join("ssh"),
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
        for path in [&layout.binary, &layout.dir] {
            assert!(!path.starts_with("/mnt/us"), "{}", path.display());
        }
    }

    #[test]
    fn runs_on_its_port_with_its_directory() {
        assert_eq!(
            Layout::default().args(PORT).join(" "),
            "--port 2222 --dir /var/local/kindil/ssh"
        );
    }

    #[test]
    fn the_launcher_ends_it_by_its_pid_file() {
        let launcher = include_str!("../launcher/src/main.rs");
        assert!(launcher.contains(&format!("\"{PID}\"")));
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
    fn copies_usbnetworks_keys_root_only() {
        let dir = tempfile::tempdir().unwrap();
        let (from, to) = (dir.path().join("usbnet"), dir.path().join("keys"));
        // No USBNetwork keys: nothing to copy, nothing made.
        assert!(!copy_keys(&from, &to).unwrap());
        assert!(!to.exists());
        std::fs::write(&from, "ssh-ed25519 AAAA one\n").unwrap();
        set_mode(&from, 0o777).unwrap();
        assert!(copy_keys(&from, &to).unwrap());
        assert_eq!(
            std::fs::read_to_string(&to).unwrap(),
            "ssh-ed25519 AAAA one\n"
        );
        assert_eq!(mode(&to), 0o600);
        assert!(!copy_keys(&from, &to).unwrap());
        std::fs::write(&from, "ssh-ed25519 AAAA two\n").unwrap();
        assert!(copy_keys(&from, &to).unwrap());
        // USBNetwork's gone (USB drive mode): the last copy stays.
        std::fs::remove_file(&from).unwrap();
        assert!(!copy_keys(&from, &to).unwrap());
        assert_eq!(
            std::fs::read_to_string(&to).unwrap(),
            "ssh-ed25519 AAAA two\n"
        );
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
        // Again, with the directory opened up: closed again.
        set_mode(&layout.dir, 0o775).unwrap();
        layout.prepare(b"sshd", &usbnet).unwrap();
        assert_eq!(mode(&layout.dir), 0o700);
    }

    #[test]
    fn refuses_a_build_without_its_server() {
        let dir = tempfile::tempdir().unwrap();
        assert!(layout(dir.path())
            .prepare(b"", &dir.path().join("none"))
            .is_err());
    }
}
