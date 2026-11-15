//! kindil's SSH server, on russh.
//!
//! Small on purpose: public keys only (ed25519 and RSA, from one
//! `authorized_keys`), as root; commands (`exec`, run by `/bin/sh -c` with
//! their stdin, stdout and stderr piped and their exit status sent back) and
//! shells, on a pseudo-terminal when one is asked for. No passwords, port
//! forwarding, agent, X11 or subsystems (so no sftp). Its host key is its own
//! ed25519 key, made on the first start. It runs on one thread.
pub mod log;
pub mod pty;

use bytes::Bytes;
use russh::keys::ssh_key::private::Ed25519Keypair;
use russh::keys::ssh_key::{AuthorizedKeys, LineEnding};
use russh::keys::{Algorithm, HashAlg, PrivateKey, PublicKey};
use russh::server::{Auth, ChannelOpenHandle, Handle, Msg, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet, Preferred, SshId};
use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{self, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, OwnedSemaphorePermit, Semaphore};

/// Sessions' PATH: the Kindle's usual one, sbin (upstart's tools) included.
pub const PATH: &str = "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";
/// The only user: everything runs as root, the server's own user.
pub const USER: &str = "root";
/// Bytes typed or piped into a session's program but not yet read by it.
/// Past this, the connection waits for the program.
const INPUT_BACKLOG: usize = 8 << 20;
/// Output read from a program at a time.
const CHUNK: usize = 32 << 10;
/// How long a shell's last output may take to come through its terminal
/// after it exits, and how long that may go on.
const DRAIN_QUIET: Duration = Duration::from_millis(100);
const DRAIN_MAX: Duration = Duration::from_secs(2);

use log::{line as log, noise};

/// After the login deadline, how long a connection's task gets to wind
/// down before its slot is given back anyway.
const WIND_DOWN: Duration = Duration::from_secs(1);

/// What the server serves, and where its files are.
#[derive(Clone, Debug)]
pub struct Settings {
    /// Its host key and the authorized keys; root's only.
    pub dir: PathBuf,
    /// Sessions start here, and it is their HOME (root's, on the Kindle).
    pub home: PathBuf,
    pub shell: PathBuf,
    /// Sessions' PATH ([`PATH`]).
    pub path: String,
    pub max_connections: usize,
    pub max_auth_attempts: usize,
    /// What a connection gets to log in, from its accept: then it is cut.
    pub login_time: Duration,
    /// New connections let in: this many a second on average, in bursts of
    /// at most `admission_burst`.
    pub admissions_per_second: f64,
    pub admission_burst: f64,
}

impl Settings {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            home: "/tmp/root".into(),
            shell: "/bin/sh".into(),
            path: PATH.into(),
            max_connections: 8,
            max_auth_attempts: 6,
            login_time: Duration::from_secs(30),
            admissions_per_second: 2.0,
            admission_burst: 6.0,
        }
    }

    pub fn host_key(&self) -> PathBuf {
        self.dir.join("host_ed25519")
    }

    pub fn authorized_keys(&self) -> PathBuf {
        self.dir.join("authorized_keys")
    }
}

/// `kindil-sshd [--port N] [--dir DIR] [--listen ADDR] [--log FILE]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Args {
    pub port: u16,
    pub dir: PathBuf,
    pub listen: IpAddr,
    /// The log file (capped, see [`log`]); stderr without one.
    pub log: Option<PathBuf>,
}

impl Args {
    pub fn parse(mut args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut parsed = Args {
            port: 22,
            dir: "/var/local/kindil/ssh".into(),
            listen: Ipv4Addr::UNSPECIFIED.into(),
            log: None,
        };
        while let Some(arg) = args.next() {
            let mut value = || args.next().ok_or(format!("{arg} needs a value"));
            match arg.as_str() {
                "--port" => parsed.port = value()?.parse().map_err(|e| format!("--port: {e}"))?,
                "--dir" => parsed.dir = value()?.into(),
                "--log" => parsed.log = Some(value()?.into()),
                "--listen" => {
                    parsed.listen = value()?.parse().map_err(|e| format!("--listen: {e}"))?
                }
                _ => return Err(format!("unknown argument {arg}")),
            }
        }
        Ok(parsed)
    }
}

/// The host key at `path`, made (ed25519, root's only) if there is none. A
/// file beside it is renamed over it, so a half-written key never stays.
pub fn host_key(path: &Path) -> io::Result<PrivateKey> {
    match std::fs::read_to_string(path) {
        Ok(pem) => {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
            return PrivateKey::from_openssh(pem).map_err(io::Error::other);
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| io::Error::other(e.to_string()))?;
    let key = PrivateKey::from(Ed25519Keypair::from_seed(&seed));
    seed.fill(0);
    let pem = key.to_openssh(LineEnding::LF).map_err(io::Error::other)?;
    let part = path.with_extension("part");
    let _ = std::fs::remove_file(&part);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&part)?;
    file.write_all(pem.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&part, path)?;
    log(format_args!(
        "host key made: {}",
        key.public_key().fingerprint(HashAlg::Sha256)
    ));
    Ok(key)
}

/// Whether `key` is in the authorized keys at `file`. Only ed25519 and RSA
/// keys count, and only those without options (`command=`, `from=`...),
/// which this server doesn't enforce. A file others could change is refused.
pub fn authorized(file: &Path, key: &PublicKey) -> io::Result<bool> {
    let meta = match std::fs::metadata(file) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    // SAFETY: geteuid has no preconditions.
    if meta.mode() & 0o022 != 0 || meta.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::other(format!(
            "{} is writable by others",
            file.display()
        )));
    }
    let keys = std::fs::read_to_string(file)?;
    Ok(AuthorizedKeys::new(&keys).flatten().any(|entry| {
        let known = entry.public_key();
        entry.config_opts().is_empty()
            && matches!(
                known.algorithm(),
                Algorithm::Ed25519 | Algorithm::Rsa { .. }
            )
            && known.key_data() == key.key_data()
    }))
}

/// The algorithms offered, and nothing else: elliptic-curve key exchange
/// only (finite-field Diffie-Hellman is slow work an unauthenticated peer
/// could ask for, on the Kindle's one core), AEAD ciphers only (so no MAC is
/// ever negotiated), no compression. The host key is ed25519 (the only one
/// there is); user keys sign with ed25519 or RSA with SHA-2.
pub fn preferred() -> Preferred {
    use russh::keys::HashAlg;
    Preferred {
        kex: Cow::Borrowed(&[
            russh::kex::MLKEM768X25519_SHA256,
            russh::kex::CURVE25519,
            russh::kex::CURVE25519_PRE_RFC_8731,
            // Not algorithms: RFC 8308 extensions and OpenSSH's strict key
            // exchange (Terrapin).
            russh::kex::EXTENSION_SUPPORT_AS_SERVER,
            russh::kex::EXTENSION_OPENSSH_STRICT_KEX_AS_SERVER,
        ]),
        key: Cow::Borrowed(&[
            Algorithm::Ed25519,
            Algorithm::Rsa {
                hash: Some(HashAlg::Sha512),
            },
            Algorithm::Rsa {
                hash: Some(HashAlg::Sha256),
            },
        ]),
        host_key_certificates: Cow::Borrowed(&[]),
        cipher: Cow::Borrowed(&[
            russh::cipher::CHACHA20_POLY1305,
            russh::cipher::AES_256_GCM,
            russh::cipher::AES_128_GCM,
        ]),
        mac: Cow::Borrowed(&[]),
        compression: Cow::Borrowed(&[russh::compression::NONE]),
    }
}

/// Whether a user key may sign with `algorithm`: not RSA with SHA-1.
fn signs_well(algorithm: &Algorithm) -> bool {
    matches!(
        algorithm,
        Algorithm::Ed25519 | Algorithm::Rsa { hash: Some(_) }
    )
}

/// russh's settings: keys only, a few tries, no idle limit but keepalives,
/// fresh keys every 128 MiB or hour each way.
pub fn config(key: PrivateKey, settings: &Settings) -> russh::server::Config {
    let mut methods = MethodSet::empty();
    methods.push(MethodKind::PublicKey);
    russh::server::Config {
        preferred: preferred(),
        limits: russh::Limits::new(128 << 20, 128 << 20, Duration::from_secs(3600)),
        server_id: SshId::Standard("SSH-2.0-kindil".into()),
        methods,
        auth_rejection_time: Duration::from_secs(1),
        auth_rejection_time_initial: Some(Duration::ZERO),
        keys: vec![key],
        max_auth_attempts: settings.max_auth_attempts,
        inactivity_timeout: None,
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 4,
        nodelay: true,
        ..Default::default()
    }
}

/// Serves `listener` until it fails: each connection in a task, at most
/// `max_connections` at a time and `admissions_per_second` new ones (more
/// are closed at once), each cut at its login deadline unless logged in.
pub async fn serve(listener: TcpListener, settings: Settings) -> io::Result<()> {
    std::fs::create_dir_all(&settings.dir)?;
    let key = host_key(&settings.host_key())?;
    log(format_args!(
        "listening on {}, host key {}",
        listener.local_addr()?,
        key.public_key().fingerprint(HashAlg::Sha256)
    ));
    let config = Arc::new(config(key, &settings));
    let settings = Arc::new(settings);
    let live = Arc::new(AtomicUsize::new(0));
    let mut bucket = Bucket::new(settings.admissions_per_second, settings.admission_burst);
    loop {
        let (socket, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                // Out of descriptors, say: the server carries on.
                noise(format_args!("accept: {error}"));
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
        };
        if live.load(Ordering::SeqCst) >= settings.max_connections {
            noise(format_args!("{peer}: too many connections"));
            continue;
        }
        if !bucket.admit(std::time::Instant::now()) {
            noise(format_args!("{peer}: too many new connections"));
            continue;
        }
        let guard = Live::new(&live);
        let (config, settings) = (config.clone(), settings.clone());
        tokio::spawn(async move {
            let _guard = guard;
            connection(config, settings, socket, peer).await;
        });
    }
}

/// One connection, from accept to its end. Until it logs in it has one
/// deadline, `login_time` from now: then its socket is shut down under
/// russh (which may be busy, or waiting on a peer that sends nothing), and
/// its slot is given back within [`WIND_DOWN`] whatever russh does.
async fn connection(
    config: Arc<russh::server::Config>,
    settings: Arc<Settings>,
    socket: tokio::net::TcpStream,
    peer: SocketAddr,
) {
    let deadline = tokio::time::Instant::now() + settings.login_time;
    // A second descriptor for the same socket, to shut it down from here.
    let (socket, cutter) = match socket
        .into_std()
        .and_then(|socket| Ok((socket.try_clone()?, socket)))
        .and_then(|(cutter, socket)| Ok((tokio::net::TcpStream::from_std(socket)?, cutter)))
    {
        Ok(pair) => pair,
        Err(error) => return noise(format_args!("{peer}: {error}")),
    };
    let cut = || {
        let _ = cutter.shutdown(std::net::Shutdown::Both);
    };
    let (logged_in_tx, logged_in) = oneshot::channel();
    let handler = Connection {
        settings,
        peer,
        logged_in: Some(logged_in_tx),
        channels: HashMap::new(),
    };
    let started =
        tokio::time::timeout_at(deadline, russh::server::run_stream(config, socket, handler));
    let mut session = match started.await {
        Ok(Ok(session)) => session,
        Ok(Err(error)) => return noise(format_args!("{peer}: {error}")),
        Err(_) => {
            cut();
            return noise(format_args!("{peer}: no handshake in time"));
        }
    };
    let in_time = tokio::select! {
        ended = &mut session => {
            if let Err(error) = ended {
                noise(format_args!("{peer}: {error}"));
            }
            return;
        }
        logged_in = logged_in => logged_in.is_ok(),
        () = tokio::time::sleep_until(deadline) => false,
    };
    if !in_time {
        cut();
        noise(format_args!("{peer}: no login in time"));
        let _ = tokio::time::timeout(WIND_DOWN, session).await;
        return;
    }
    if let Err(error) = session.await {
        noise(format_args!("{peer}: {error}"));
    }
}

/// Admission: a token bucket.
pub struct Bucket {
    rate: f64,
    burst: f64,
    tokens: f64,
    last: Option<std::time::Instant>,
}

impl Bucket {
    pub fn new(rate: f64, burst: f64) -> Self {
        Self {
            rate,
            burst,
            tokens: burst,
            last: None,
        }
    }

    /// Whether one more may come in now.
    pub fn admit(&mut self, now: std::time::Instant) -> bool {
        if let Some(last) = self.last {
            let refill = now.saturating_duration_since(last).as_secs_f64() * self.rate;
            self.tokens = (self.tokens + refill).min(self.burst);
        }
        self.last = Some(now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// One live connection, counted while it lasts.
struct Live(Arc<AtomicUsize>);

impl Live {
    fn new(count: &Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self(count.clone())
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A client's connection.
struct Connection {
    settings: Arc<Settings>,
    peer: SocketAddr,
    /// Fired once it has logged in (dropped unfired if it ends before).
    logged_in: Option<oneshot::Sender<()>>,
    channels: HashMap<ChannelId, Chan>,
}

/// One session channel.
#[derive(Default)]
struct Chan {
    pty: Option<(String, pty::Size)>,
    master: Option<Arc<pty::Master>>,
    input: Option<Input>,
    started: bool,
    /// Dropped with the channel (closed, or the connection gone): the tasks
    /// serving it stop.
    _open: Option<oneshot::Sender<()>>,
}

/// The way into a program's stdin (or its terminal), with room for at most
/// [`INPUT_BACKLOG`] bytes it hasn't read yet.
#[derive(Clone)]
struct Input {
    queue: mpsc::UnboundedSender<(Bytes, OwnedSemaphorePermit)>,
    room: Arc<Semaphore>,
}

impl Input {
    fn new() -> (Self, mpsc::UnboundedReceiver<(Bytes, OwnedSemaphorePermit)>) {
        let (queue, receiver) = mpsc::unbounded_channel();
        let room = Arc::new(Semaphore::new(INPUT_BACKLOG));
        (Self { queue, room }, receiver)
    }

    async fn send(&self, data: &[u8]) {
        let n = data.len().min(INPUT_BACKLOG) as u32;
        if let Ok(permit) = self.room.clone().acquire_many_owned(n).await {
            let _ = self.queue.send((Bytes::copy_from_slice(data), permit));
        }
    }
}

impl Connection {
    fn allowed(&self, user: &str, key: &PublicKey) -> bool {
        if user != USER {
            return false;
        }
        authorized(&self.settings.authorized_keys(), key).unwrap_or_else(|error| {
            noise(format_args!("{}: {error}", self.peer));
            false
        })
    }

    /// Starts the channel's program: `command` by the shell, or the shell
    /// itself (a login shell), on its terminal if it asked for one.
    fn start(&mut self, id: ChannelId, command: Option<&[u8]>, handle: Handle) -> io::Result<()> {
        let settings = self.settings.clone();
        let chan = self.channels.entry(id).or_default();
        if chan.started {
            return Err(io::Error::other("already started"));
        }
        let mut cmd = std::process::Command::new(&settings.shell);
        match command {
            Some(command) => {
                use std::os::unix::ffi::OsStrExt;
                cmd.arg("-c").arg(std::ffi::OsStr::from_bytes(command));
            }
            None => {
                cmd.arg0("-sh");
            }
        }
        let home = if settings.home.is_dir() {
            settings.home.as_path()
        } else {
            Path::new("/")
        };
        cmd.env_clear()
            .env("PATH", &settings.path)
            .env("HOME", &settings.home)
            .env("USER", USER)
            .env("LOGNAME", USER)
            .env("SHELL", &settings.shell)
            .current_dir(home);
        let (open, closed) = oneshot::channel();
        let (input, received) = Input::new();
        if let Some((term, size)) = chan.pty.clone() {
            let (master, slave) = pty::open(size)?;
            cmd.env("TERM", term);
            pty::attach(&mut cmd, slave)?;
            let child = tokio::process::Command::from(cmd).spawn()?;
            let master = Arc::new(master);
            tokio::spawn(type_into(master.clone(), received));
            tokio::spawn(run_on_terminal(handle, id, child, master.clone(), closed));
            chan.master = Some(master);
        } else {
            cmd.stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = tokio::process::Command::from(cmd).spawn()?;
            if let Some(stdin) = child.stdin.take() {
                tokio::spawn(pipe_into(stdin, received));
            }
            tokio::spawn(run_on_pipes(handle, id, child, closed));
        }
        chan.input = Some(input);
        chan._open = Some(open);
        chan.started = true;
        Ok(())
    }
}

/// Feeds what the client sends into a program's stdin, closed (EOF) once
/// the client says it is done.
async fn pipe_into(
    mut stdin: tokio::process::ChildStdin,
    mut received: mpsc::UnboundedReceiver<(Bytes, OwnedSemaphorePermit)>,
) {
    while let Some((data, _room)) = received.recv().await {
        if stdin.write_all(&data).await.is_err() {
            return;
        }
    }
}

/// Types what the client sends into a terminal.
async fn type_into(
    master: Arc<pty::Master>,
    mut received: mpsc::UnboundedReceiver<(Bytes, OwnedSemaphorePermit)>,
) {
    while let Some((data, _room)) = received.recv().await {
        if master.write_all(&data).await.is_err() {
            return;
        }
    }
}

/// Sends what `reader` gives as the channel's data (`ext` 1: its stderr)
/// until its end, or until the client is gone.
async fn pump(
    reader: Option<impl AsyncRead + Unpin>,
    handle: &Handle,
    id: ChannelId,
    ext: Option<u32>,
) {
    let Some(mut reader) = reader else { return };
    let mut buf = vec![0; CHUNK];
    loop {
        let n = match reader.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        let data = Bytes::copy_from_slice(&buf[..n]);
        let sent = match ext {
            None => handle.data(id, data).await,
            Some(ext) => handle.extended_data(id, ext, data).await,
        };
        if sent.is_err() {
            return;
        }
    }
}

async fn run_on_pipes(
    handle: Handle,
    id: ChannelId,
    mut child: tokio::process::Child,
    closed: oneshot::Receiver<()>,
) {
    let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
    let work = async {
        let (_, _, status) = tokio::join!(
            pump(stdout, &handle, id, None),
            pump(stderr, &handle, id, Some(1)),
            child.wait()
        );
        status
    };
    let status = tokio::select! {
        status = work => status,
        // The channel or the connection is gone: its pipes close.
        _ = closed => return,
    };
    finish(&handle, id, status).await;
}

async fn run_on_terminal(
    handle: Handle,
    id: ChannelId,
    mut child: tokio::process::Child,
    master: Arc<pty::Master>,
    mut closed: oneshot::Receiver<()>,
) {
    let mut buf = vec![0; CHUNK];
    let status = loop {
        tokio::select! {
            read = master.read(&mut buf) => match read {
                Ok(n) if n > 0 => {
                    if handle.data(id, Bytes::copy_from_slice(&buf[..n])).await.is_err() {
                        return;
                    }
                }
                // Every slave side closed: the shell is ending.
                _ => break child.wait().await,
            },
            status = child.wait() => {
                // What it wrote last may still be on its way through the
                // terminal; what its leftover children write isn't waited for.
                let since = tokio::time::Instant::now();
                while since.elapsed() < DRAIN_MAX {
                    match tokio::time::timeout(DRAIN_QUIET, master.read(&mut buf)).await {
                        Ok(Ok(n)) if n > 0 => {
                            let _ = handle.data(id, Bytes::copy_from_slice(&buf[..n])).await;
                        }
                        _ => break,
                    }
                }
                break status;
            }
            // Gone: the terminal closes (the last master with the channel's),
            // which hangs up on the shell.
            _ = &mut closed => return,
        }
    };
    finish(&handle, id, status).await;
}

/// The program's exit status (128 + the signal that killed it), then EOF
/// and close.
async fn finish(handle: &Handle, id: ChannelId, status: io::Result<ExitStatus>) {
    let code = match status {
        Ok(status) => status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)),
        Err(_) => 255,
    };
    let _ = handle.exit_status_request(id, code as u32).await;
    let _ = handle.eof(id).await;
    let _ = handle.close(id).await;
}

impl russh::server::Handler for Connection {
    type Error = russh::Error;

    async fn auth_publickey_offered(
        &mut self,
        user: &str,
        offer: russh::server::PublicKeyOffer<'_>,
    ) -> Result<Auth, Self::Error> {
        Ok(
            if signs_well(offer.algorithm) && self.allowed(user, offer.public_key) {
                Auth::Accept
            } else {
                Auth::reject()
            },
        )
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        key: russh::server::VerifiedPublicKey<'_>,
    ) -> Result<Auth, Self::Error> {
        if !signs_well(key.algorithm) || !self.allowed(user, key.public_key) {
            noise(format_args!("{}: refused {user}", self.peer));
            return Ok(Auth::reject());
        }
        log(format_args!(
            "{}: {user} in with {} {}",
            self.peer,
            key.public_key.algorithm(),
            key.public_key.fingerprint(HashAlg::Sha256)
        ));
        if let Some(logged_in) = self.logged_in.take() {
            let _ = logged_in.send(());
        }
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        // Its messages come through this handler; the channel object, unread,
        // goes.
        self.channels.insert(channel.id(), Chan::default());
        reply.accept().await;
        Ok(())
    }

    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(chan) = self.channels.get_mut(&channel) {
            chan.input = None;
        }
        Ok(())
    }

    async fn channel_close(
        &mut self,
        channel: ChannelId,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.remove(&channel);
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        let input = self.channels.get(&channel).and_then(|c| c.input.clone());
        if let Some(input) = input {
            input.send(data).await;
        }
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        term: &str,
        cols: u32,
        rows: u32,
        width: u32,
        height: u32,
        _: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        match self.channels.get_mut(&channel) {
            Some(chan) if !chan.started => {
                let size = pty::Size {
                    cols,
                    rows,
                    width,
                    height,
                };
                chan.pty = Some((term.to_owned(), size));
                session.channel_success(channel)
            }
            _ => session.channel_failure(channel),
        }
    }

    async fn window_change_request(
        &mut self,
        channel: ChannelId,
        cols: u32,
        rows: u32,
        width: u32,
        height: u32,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        let size = pty::Size {
            cols,
            rows,
            width,
            height,
        };
        if let Some(chan) = self.channels.get_mut(&channel) {
            if let Some((_, pending)) = &mut chan.pty {
                *pending = size;
            }
            if let Some(master) = &chan.master {
                let _ = master.resize(size);
            }
        }
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.reply(channel, None, session)
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        command: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.reply(channel, Some(command), session)
    }

    async fn env_request(
        &mut self,
        channel: ChannelId,
        _: &str,
        _: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel)
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        _: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel)
    }

    async fn x11_request(
        &mut self,
        channel: ChannelId,
        _: bool,
        _: &str,
        _: &str,
        _: u32,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel)
    }
}

impl Connection {
    /// Starts the program, telling the client whether it did.
    fn reply(
        &mut self,
        channel: ChannelId,
        command: Option<&[u8]>,
        session: &mut Session,
    ) -> Result<(), russh::Error> {
        if !self.channels.contains_key(&channel) {
            return session.channel_failure(channel);
        }
        match self.start(channel, command, session.handle()) {
            Ok(()) => session.channel_success(channel),
            Err(error) => {
                log(format_args!("{}: starting: {error}", self.peer));
                session.channel_failure(channel)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Result<Args, String> {
        Args::parse(list.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses_its_arguments() {
        let parsed = args(&["--port", "2222", "--dir", "/var/local/kindil/ssh"]).unwrap();
        assert_eq!(parsed.port, 2222);
        assert_eq!(parsed.dir, Path::new("/var/local/kindil/ssh"));
        assert_eq!(parsed.listen, IpAddr::from(Ipv4Addr::UNSPECIFIED));
        assert_eq!(parsed.log, None);
        let parsed = args(&["--log", "/var/local/kindil/sshd.log"]).unwrap();
        assert_eq!(
            parsed.log.as_deref(),
            Some(Path::new("/var/local/kindil/sshd.log"))
        );
        let parsed = args(&["--listen", "127.0.0.1"]).unwrap();
        assert_eq!(
            (parsed.port, parsed.listen),
            (22, Ipv4Addr::LOCALHOST.into())
        );
        assert!(args(&["--port"]).is_err());
        assert!(args(&["--port", "x"]).is_err());
        assert!(args(&["--password"]).is_err());
    }

    #[test]
    fn admits_bursts_then_a_steady_rate() {
        let mut bucket = Bucket::new(2.0, 3.0);
        let start = std::time::Instant::now();
        assert!((0..3).all(|_| bucket.admit(start)));
        assert!(!bucket.admit(start));
        assert!(!bucket.admit(start + Duration::from_millis(400)));
        assert!(bucket.admit(start + Duration::from_millis(600)));
        assert!(!bucket.admit(start + Duration::from_millis(700)));
        // A long quiet refills to the burst, no more.
        let later = start + Duration::from_secs(60);
        assert!((0..3).all(|_| bucket.admit(later)));
        assert!(!bucket.admit(later));
    }

    #[test]
    fn keeps_its_files_in_its_directory() {
        let settings = Settings::new("/var/local/kindil/ssh");
        assert_eq!(
            settings.host_key(),
            Path::new("/var/local/kindil/ssh/host_ed25519")
        );
        assert_eq!(
            settings.authorized_keys(),
            Path::new("/var/local/kindil/ssh/authorized_keys")
        );
        assert_eq!(settings.home, Path::new("/tmp/root"));
        assert_eq!(settings.path, PATH);
    }

    #[test]
    fn no_authorized_keys_lets_nobody_in() {
        let dir = tempfile::tempdir().unwrap();
        let key = PrivateKey::from(Ed25519Keypair::from_seed(&[1; 32]));
        let file = dir.path().join("authorized_keys");
        assert!(!authorized(&file, key.public_key()).unwrap());
        std::fs::write(&file, "not a key\n\n").unwrap();
        assert!(!authorized(&file, key.public_key()).unwrap());
        let line = key.public_key().to_openssh().unwrap();
        std::fs::write(&file, format!("garbage\n{line} me@there\n")).unwrap();
        assert!(authorized(&file, key.public_key()).unwrap());
    }
}
