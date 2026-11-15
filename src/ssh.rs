//! SSH, run by kindil itself.
//!
//! kindil carries its own Dropbear (upstream, built by the flake as one
//! static dropbearmulti with dropbear, dropbearkey and scp), writes it out to
//! the system storage and runs it as its child, started again with a growing
//! pause if it keeps dying, like Wi-Fi's children (src/net.rs). Nothing of it
//! lives on /mnt/us, so it never holds the user storage a computer takes in
//! USB drive mode; USBNetwork's dropbear runs from there.
//!
//! Keys only, no passwords. The keys are USBNetwork's: its
//! `authorized_keys` is copied into kindil's own whenever it is there, and
//! kindil's copy stays for when it isn't. The host key is kindil's own,
//! made once. The server ends with kindil: the launcher stops it by its pid
//! file on the way out and before each run (launcher/src/main.rs); sessions
//! already open carry on.
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// kindil's SSH port for now, alongside USBNetwork's dropbear on 22. Once
/// kindil's own server has proven itself on the device, this becomes 22
/// and USBNetwork's goes.
pub const PORT: u16 = 2222;
/// Its pid file; the launcher ends it by this.
pub const PID: &str = "/var/run/kindil-dropbear.pid";
/// Where sessions find `scp` first. Dropbear sets a session's PATH itself,
/// to the one built in: the flake's dropbear-device holds this same string
/// (checked by a test below).
pub const SESSION_PATH: &str =
    "/var/local/kindil/ssh/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";
/// USBNetwork's authorized keys, read by its patched dropbear.
const USBNET_AUTHORIZED_KEYS: &str = "/mnt/us/usbnet/etc/authorized_keys";
const BINARY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/dropbear"));
/// The programs in dropbearmulti, each a link named for it in `bin`.
const PROGRAMS: [&str; 3] = ["dropbear", "dropbearkey", "scp"];
const INTERFACE: &str = "wlan0";
const IPTABLES: &str = "/usr/sbin/iptables";
/// A server that lived this long was fine; a restart after it waits only
/// the shortest pause.
const HEALTHY: Duration = Duration::from_secs(60);
const PAUSE_MIN: Duration = Duration::from_secs(1);
const PAUSE_MAX: Duration = Duration::from_secs(60);

/// Where kindil's SSH server keeps its files; the tests point it elsewhere.
pub struct Layout {
    /// The written-out dropbearmulti.
    pub multi: PathBuf,
    /// Root's only: the host key and the authorized keys, with `bin` inside.
    pub dir: PathBuf,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            multi: "/var/local/kindil/dropbearmulti".into(),
            dir: "/var/local/kindil/ssh".into(),
        }
    }
}

impl Layout {
    /// Links named for each program in dropbearmulti; first on sessions' PATH.
    pub fn bin(&self) -> PathBuf {
        self.dir.join("bin")
    }

    pub fn host_key(&self) -> PathBuf {
        self.dir.join("dropbear_ed25519_host_key")
    }

    /// Dropbear reads `authorized_keys` in the directory given by `-D`.
    pub fn authorized_keys(&self) -> PathBuf {
        self.dir.join("authorized_keys")
    }

    /// dropbear's arguments: in the foreground, logging to kindil's log, keys
    /// only.
    pub fn args(&self, port: u16, pid: &str) -> Vec<String> {
        [
            "dropbear",
            "-F",
            "-E",
            "-s",
            "-p",
            &port.to_string(),
            "-r",
            &self.host_key().to_string_lossy(),
            "-D",
            &self.dir.to_string_lossy(),
            "-P",
            pid,
        ]
        .map(str::to_owned)
        .to_vec()
    }

    /// Everything dropbear needs, short of the host key: the binary (written
    /// only when it changed), its links, kindil's directory and the keys.
    pub fn prepare(&self, binary: &[u8], usbnet_keys: &Path) -> anyhow::Result<()> {
        anyhow::ensure!(
            !binary.is_empty(),
            "this kindil was built without its SSH server"
        );
        if install(binary, &self.multi)? {
            eprintln!("kindil: SSH server written to {}", self.multi.display());
        }
        std::fs::create_dir_all(&self.dir)?;
        set_mode(&self.dir, 0o700)?;
        self.link()?;
        // /mnt/us may be gone (USB drive mode): the last copy serves.
        if let Err(error) = copy_keys(usbnet_keys, &self.authorized_keys()) {
            eprintln!("kindil: SSH keys from USBNetwork: {error}");
        }
        Ok(())
    }

    /// `bin/<program>` → dropbearmulti, for each of its programs.
    fn link(&self) -> anyhow::Result<()> {
        let bin = self.bin();
        std::fs::create_dir_all(&bin)?;
        for program in PROGRAMS {
            let link = bin.join(program);
            if std::fs::read_link(&link).ok().as_deref() == Some(self.multi.as_path()) {
                continue;
            }
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(&self.multi, &link)?;
        }
        Ok(())
    }

    /// Makes the host key once, through a file beside it: a half-written key
    /// would stop dropbear for good.
    fn ensure_host_key(&self) -> anyhow::Result<()> {
        let key = self.host_key();
        if key.is_file() {
            return Ok(());
        }
        let part = key.with_extension("part");
        // dropbearkey refuses to overwrite a file.
        let _ = std::fs::remove_file(&part);
        let out = Command::new(&self.multi)
            .args(["dropbearkey", "-t", "ed25519", "-f"])
            .arg(&part)
            .output()?;
        anyhow::ensure!(
            out.status.success(),
            "dropbearkey exited with {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
        set_mode(&part, 0o600)?;
        std::fs::rename(&part, &key)?;
        eprintln!("kindil: SSH host key made");
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
    layout.ensure_host_key()?;
    if let Err(error) = open_port(PORT) {
        eprintln!("kindil: opening port {PORT}: {error:#}");
    }
    let args = layout.args(PORT, PID);
    let mut command = Command::new(&layout.multi);
    command
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null());
    let child = command.spawn()?;
    // Dropbear writes it too, once listening; the launcher may need it sooner.
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
            .stderr(std::process::Stdio::null())
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
            multi: dir.join("dropbearmulti"),
            dir: dir.join("ssh"),
        }
    }

    #[test]
    fn lives_on_the_system_storage() {
        let layout = Layout::default();
        assert_eq!(layout.bin(), Path::new("/var/local/kindil/ssh/bin"));
        assert_eq!(
            layout.host_key(),
            Path::new("/var/local/kindil/ssh/dropbear_ed25519_host_key")
        );
        assert_eq!(
            layout.authorized_keys(),
            Path::new("/var/local/kindil/ssh/authorized_keys")
        );
        for path in [&layout.multi, &layout.dir] {
            assert!(!path.starts_with("/mnt/us"), "{}", path.display());
        }
    }

    #[test]
    fn runs_in_the_foreground_with_keys_only() {
        let args = Layout::default().args(PORT, PID).join(" ");
        assert_eq!(
            args,
            "dropbear -F -E -s -p 2222 \
             -r /var/local/kindil/ssh/dropbear_ed25519_host_key \
             -D /var/local/kindil/ssh -P /var/run/kindil-dropbear.pid"
        );
    }

    #[test]
    fn sessions_find_kindils_scp_first() {
        let bin = Layout::default().bin();
        let first = SESSION_PATH.split(':').next().unwrap();
        assert_eq!(Path::new(first), bin);
        for dir in ["/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"] {
            assert!(SESSION_PATH.split(':').any(|d| d == dir), "{dir}");
        }
        // Built into the flake's dropbear, which sets it for each session.
        let flake = include_str!("../flake.nix");
        assert!(
            flake.contains(&format!("#define DEFAULT_PATH \"{SESSION_PATH}\"")),
            "flake.nix's dropbear-device must build in SESSION_PATH"
        );
        assert!(flake.contains(&format!("#define DROPBEAR_PIDFILE \"{PID}\"")));
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
        let path = dir.path().join("dropbearmulti");
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
    fn prepares_binary_links_and_keys() {
        let dir = tempfile::tempdir().unwrap();
        let layout = layout(dir.path());
        let usbnet = dir.path().join("authorized_keys");
        std::fs::write(&usbnet, "ssh-ed25519 AAAA k\n").unwrap();
        layout.prepare(b"multi", &usbnet).unwrap();
        assert_eq!(std::fs::read(&layout.multi).unwrap(), b"multi");
        assert_eq!(mode(&layout.dir), 0o700);
        for program in PROGRAMS {
            assert_eq!(
                std::fs::read_link(layout.bin().join(program)).unwrap(),
                layout.multi
            );
        }
        assert_eq!(mode(&layout.authorized_keys()), 0o600);
        // Again, with a link pointing elsewhere: put right.
        let scp = layout.bin().join("scp");
        std::fs::remove_file(&scp).unwrap();
        std::os::unix::fs::symlink("/mnt/us/usbnet/bin/scp", &scp).unwrap();
        layout.prepare(b"multi", &usbnet).unwrap();
        assert_eq!(std::fs::read_link(&scp).unwrap(), layout.multi);
    }

    #[test]
    fn refuses_a_build_without_its_server() {
        let dir = tempfile::tempdir().unwrap();
        assert!(layout(dir.path())
            .prepare(b"", &dir.path().join("none"))
            .is_err());
    }
}
