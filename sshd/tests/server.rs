//! kindil-sshd on localhost, driven by russh's client.
use russh::client::{self, Handle};
use russh::keys::ssh_key::private::Ed25519Keypair;
use russh::keys::ssh_key::LineEnding;
use russh::keys::{PrivateKey, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate};
use russh::{Channel, ChannelMsg};
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

/// A test-only RSA key (tests/keys/rsa, made with ssh-keygen).
const RSA_KEY: &str = include_str!("keys/rsa");

fn ed25519(seed: u8) -> PrivateKey {
    PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32]))
}

fn rsa() -> PrivateKey {
    PrivateKey::from_openssh(RSA_KEY).unwrap()
}

fn line(key: &PublicKey) -> String {
    format!("{}\n", key.to_openssh().unwrap())
}

struct Server {
    addr: SocketAddr,
    dir: TempDir,
    home: TempDir,
}

impl Server {
    fn keys(&self) -> std::path::PathBuf {
        self.dir.path().join("authorized_keys")
    }
}

async fn serve(authorized: &str, max_connections: usize) -> Server {
    let (dir, home) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let keys = dir.path().join("authorized_keys");
    std::fs::write(&keys, authorized).unwrap();
    std::fs::set_permissions(&keys, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut settings = sshd::Settings::new(dir.path());
    settings.home = home.path().to_owned();
    // The Kindle's PATH, then this machine's: NixOS has no /bin/cat.
    settings.path = format!(
        "{}:{}",
        sshd::PATH,
        std::env::var("PATH").unwrap_or_default()
    );
    settings.max_connections = max_connections;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(sshd::serve(listener, settings));
    Server { addr, dir, home }
}

struct Client;

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(&mut self, _: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// A connection logged in as `user` with `key`, or None if refused.
async fn login(addr: SocketAddr, user: &str, key: PrivateKey) -> Option<Handle<Client>> {
    let config = Arc::new(client::Config::default());
    let mut handle = client::connect(config, addr, Client).await.unwrap();
    let hash = handle.best_supported_rsa_hash().await.unwrap().flatten();
    let key = PrivateKeyWithHashAlg::new(Arc::new(key), hash);
    let result = handle.authenticate_publickey(user, key).await.unwrap();
    result.success().then_some(handle)
}

#[derive(Default, Debug)]
struct Output {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    status: Option<u32>,
}

impl Output {
    fn stdout(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

/// Everything until the channel closes.
async fn collect(channel: &mut Channel<client::Msg>) -> Output {
    let mut out = Output::default();
    while let Some(msg) = tokio::time::timeout(Duration::from_secs(20), channel.wait())
        .await
        .expect("the server went quiet")
    {
        match msg {
            ChannelMsg::Data { data } => out.stdout.extend_from_slice(&data),
            ChannelMsg::ExtendedData { data, ext: 1 } => out.stderr.extend_from_slice(&data),
            ChannelMsg::ExitStatus { exit_status } => out.status = Some(exit_status),
            _ => {}
        }
    }
    out
}

/// Runs `command`, with `stdin` (then EOF) if given.
async fn exec(handle: &Handle<Client>, command: &str, stdin: Option<&[u8]>) -> Output {
    let mut channel = handle.channel_open_session().await.unwrap();
    channel.exec(true, command).await.unwrap();
    if let Some(stdin) = stdin {
        // A command that ends without reading it all leaves the rest unsent.
        tokio::time::timeout(Duration::from_secs(60), channel.data(stdin))
            .await
            .expect("stdin not taken")
            .unwrap();
        channel.eof().await.unwrap();
    }
    collect(&mut channel).await
}

#[tokio::test]
async fn runs_a_command_with_its_output_errors_and_status() {
    let key = ed25519(1);
    let server = serve(&line(key.public_key()), 8).await;
    let handle = login(server.addr, "root", key).await.expect("logged in");
    let out = exec(&handle, "echo out; echo err >&2; exit 3", None).await;
    assert_eq!(out.stdout(), "out\n");
    assert_eq!(out.stderr, b"err\n");
    assert_eq!(out.status, Some(3));
}

#[tokio::test]
async fn commands_get_the_kindles_path_and_roots_home() {
    let key = ed25519(1);
    let server = serve(&line(key.public_key()), 8).await;
    let handle = login(server.addr, "root", key).await.unwrap();
    let out = exec(
        &handle,
        "echo \"$PATH\"; echo \"$HOME\"; pwd -P; echo $USER",
        None,
    )
    .await;
    let home = std::fs::canonicalize(server.home.path()).unwrap();
    // The Kindle's PATH first (the tests add this machine's after it).
    let expected = format!(
        "\n{}\n{}\nroot\n",
        server.home.path().display(),
        home.display()
    );
    assert!(
        out.stdout().starts_with(&format!("{}:", sshd::PATH)),
        "{}",
        out.stdout()
    );
    assert!(out.stdout().ends_with(&expected), "{}", out.stdout());
}

#[tokio::test]
async fn pipes_stdin_until_the_clients_eof() {
    let key = ed25519(1);
    let server = serve(&line(key.public_key()), 8).await;
    let handle = login(server.addr, "root", key).await.unwrap();
    let data: Vec<u8> = (0..(3 << 20)).map(|i: u32| (i * 7 % 251) as u8).collect();
    // The way cargo xtask copies files to the Kindle.
    let target = server.home.path().join("file");
    let command = format!(
        "cat > {t}.part && mv {t}.part {t} && wc -c < {t}",
        t = target.display()
    );
    let out = exec(&handle, &command, Some(&data)).await;
    assert_eq!(out.stdout().trim(), (3 << 20).to_string());
    assert_eq!(out.status, Some(0));
    assert_eq!(std::fs::read(&target).unwrap(), data);
    // Nothing piped in: the program reads an empty stdin.
    let out = exec(&handle, "wc -c", Some(b"")).await;
    assert_eq!(out.stdout().trim(), "0");
}

#[tokio::test]
async fn accepts_rsa_keys_too() {
    let key = rsa();
    let server = serve(&line(key.public_key()), 8).await;
    let handle = login(server.addr, "root", key)
        .await
        .expect("logged in with RSA");
    assert_eq!(exec(&handle, "exit 0", None).await.status, Some(0));
}

#[tokio::test]
async fn refuses_other_keys_users_and_keys_with_options() {
    let (known, other) = (ed25519(1), ed25519(2));
    let restricted = ed25519(3);
    let keys = format!(
        "# comment\n{}command=\"true\" {}",
        line(known.public_key()),
        line(restricted.public_key())
    );
    let server = serve(&keys, 8).await;
    assert!(login(server.addr, "root", other).await.is_none());
    assert!(login(server.addr, "admin", known.clone()).await.is_none());
    assert!(login(server.addr, "root", restricted).await.is_none());
    assert!(login(server.addr, "root", known).await.is_some());
}

#[tokio::test]
async fn refuses_authorized_keys_others_could_change() {
    let key = ed25519(1);
    let server = serve(&line(key.public_key()), 8).await;
    std::fs::set_permissions(server.keys(), std::fs::Permissions::from_mode(0o664)).unwrap();
    assert!(login(server.addr, "root", key).await.is_none());
}

#[tokio::test]
async fn holds_no_more_connections_than_allowed() {
    let key = ed25519(1);
    let server = serve(&line(key.public_key()), 1).await;
    let first = login(server.addr, "root", key.clone()).await.unwrap();
    let config = Arc::new(client::Config::default());
    assert!(client::connect(config, server.addr, Client).await.is_err());
    drop(first);
    // Its place frees up once it has gone.
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let config = Arc::new(client::Config::default());
        if client::connect(config, server.addr, Client).await.is_ok() {
            return;
        }
    }
    panic!("the connection's place never freed up");
}

/// Reads the terminal into `seen` until `needle` shows.
async fn read_until(channel: &mut Channel<client::Msg>, seen: &mut String, needle: &str) {
    while !seen.contains(needle) {
        let msg = tokio::time::timeout(Duration::from_secs(20), channel.wait())
            .await
            .unwrap_or_else(|_| panic!("no {needle:?} in {seen:?}"));
        match msg {
            Some(ChannelMsg::Data { data }) => seen.push_str(&String::from_utf8_lossy(&data)),
            Some(_) => {}
            None => panic!("closed before {needle:?} in {seen:?}"),
        }
    }
}

#[tokio::test]
async fn gives_a_shell_its_terminal_and_its_size() {
    if !Path::new("/dev/ptmx").exists() {
        eprintln!("no pseudo-terminals here: skipped");
        return;
    }
    let key = ed25519(1);
    let server = serve(&line(key.public_key()), 8).await;
    let handle = login(server.addr, "root", key).await.unwrap();
    let mut channel = handle.channel_open_session().await.unwrap();
    channel
        .request_pty(true, "xterm-test", 80, 24, 0, 0, &[])
        .await
        .unwrap();
    channel.request_shell(true).await.unwrap();
    let mut seen = String::new();
    channel
        .data(&b"echo \"[$TERM]\"; stty size; tty\n"[..])
        .await
        .unwrap();
    read_until(&mut channel, &mut seen, "[xterm-test]").await;
    read_until(&mut channel, &mut seen, "24 80").await;
    read_until(&mut channel, &mut seen, "/dev/").await;
    channel.window_change(100, 30, 0, 0).await.unwrap();
    channel.data(&b"stty size; exit 7\n"[..]).await.unwrap();
    let rest = collect(&mut channel).await;
    assert!(rest.stdout().contains("30 100"), "{seen}{}", rest.stdout());
    assert_eq!(rest.status, Some(7), "{seen}{}", rest.stdout());
}

#[test]
fn makes_its_host_key_once_root_only() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("host_ed25519");
    let made = sshd::host_key(&path).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!path.with_extension("part").exists());
    let again = sshd::host_key(&path).unwrap();
    assert_eq!(made.public_key(), again.public_key());
    assert!(matches!(made.algorithm(), russh::keys::Algorithm::Ed25519));
    // It is OpenSSH's own format.
    let pem = std::fs::read_to_string(&path).unwrap();
    assert!(pem.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----"));
    let _ = made.to_openssh(LineEnding::LF).unwrap();
}
