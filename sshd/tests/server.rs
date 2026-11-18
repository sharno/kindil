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
    serve_with(authorized, |settings| {
        settings.max_connections = max_connections
    })
    .await
}

async fn serve_with(authorized: &str, adjust: impl FnOnce(&mut sshd::Settings)) -> Server {
    let (dir, home) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
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
    // Tests connect often and quickly.
    settings.admissions_per_second = 1000.0;
    settings.admission_burst = 1000.0;
    adjust(&mut settings);
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
    // The prompt, set apart from the line's own echo by the quotes.
    channel
        .data(&b"PS1='ki''ndil$ '; echo \"[$TERM]\"; stty size; tty\n"[..])
        .await
        .unwrap();
    read_until(&mut channel, &mut seen, "[xterm-test]").await;
    read_until(&mut channel, &mut seen, "24 80").await;
    read_until(&mut channel, &mut seen, "/dev/").await;
    // Resized only once the shell waits at its prompt: readline (bash's,
    // macOS's /bin/sh) reads the size and writes it back as it sets up
    // the terminal for each line, so a resize landing in between is
    // undone, whatever the server. Once it has prompted, nothing does.
    read_until(&mut channel, &mut seen, "kindil$ ").await;
    channel.window_change(100, 30, 0, 0).await.unwrap();
    // Handled before the line typed after it: the next command sees it.
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

/// Connects offering only `kex` and `cipher`; whether it logs in.
async fn login_with(
    addr: SocketAddr,
    key: PrivateKey,
    kex: russh::kex::Name,
    cipher: russh::cipher::Name,
) -> bool {
    let mut config = client::Config::default();
    config.preferred.kex = vec![kex, russh::kex::EXTENSION_SUPPORT_AS_CLIENT].into();
    config.preferred.cipher = vec![cipher].into();
    let Ok(mut handle) = client::connect(Arc::new(config), addr, Client).await else {
        return false;
    };
    let key = PrivateKeyWithHashAlg::new(Arc::new(key), None);
    matches!(handle.authenticate_publickey("root", key).await, Ok(r) if r.success())
}

#[tokio::test]
async fn offers_only_elliptic_curve_kex_and_aead_ciphers() {
    use russh::{cipher, kex};
    let key = ed25519(1);
    let server = serve(&line(key.public_key()), 8).await;
    for kex in [
        kex::MLKEM768X25519_SHA256,
        kex::CURVE25519,
        kex::CURVE25519_PRE_RFC_8731,
    ] {
        for cipher in [
            cipher::CHACHA20_POLY1305,
            cipher::AES_256_GCM,
            cipher::AES_128_GCM,
        ] {
            assert!(
                login_with(server.addr, key.clone(), kex, cipher).await,
                "{kex:?} {cipher:?}"
            );
        }
    }
    for kex in [
        kex::DH_G14_SHA256,
        kex::DH_G16_SHA512,
        kex::DH_G18_SHA512,
        kex::DH_GEX_SHA256,
    ] {
        assert!(
            !login_with(server.addr, key.clone(), kex, cipher::CHACHA20_POLY1305).await,
            "{kex:?}"
        );
    }
    for cipher in [
        cipher::AES_128_CTR,
        cipher::AES_256_CTR,
        cipher::AES_256_CBC,
    ] {
        assert!(
            !login_with(server.addr, key.clone(), kex::CURVE25519, cipher).await,
            "{cipher:?}"
        );
    }
}

#[tokio::test]
async fn refuses_rsa_signatures_with_sha1() {
    let key = rsa();
    let server = serve(&line(key.public_key()), 8).await;
    let config = Arc::new(client::Config::default());
    let mut handle = client::connect(config, server.addr, Client).await.unwrap();
    let sha1 = PrivateKeyWithHashAlg::new(Arc::new(key.clone()), None);
    assert!(!handle
        .authenticate_publickey("root", sha1)
        .await
        .unwrap()
        .success());
    let sha512 = PrivateKeyWithHashAlg::new(Arc::new(key), Some(russh::keys::HashAlg::Sha512));
    assert!(handle
        .authenticate_publickey("root", sha512)
        .await
        .unwrap()
        .success());
}

/// Seconds until the server closes `stream`, sending SSH_MSG_IGNORE (in
/// the clear, as before any key exchange) every 200 ms if `chatty`.
async fn seconds_until_cut(mut stream: tokio::net::TcpStream, chatty: bool) -> f64 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let since = std::time::Instant::now();
    // uint32 length 12, padding length 6, SSH_MSG_IGNORE (2), empty string,
    // 6 bytes of padding: 16 bytes in all.
    let ignore = [0, 0, 0, 12, 6, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let mut buf = [0u8; 4096];
    loop {
        let tick = tokio::time::sleep(Duration::from_millis(200));
        tokio::select! {
            read = stream.read(&mut buf) => match read {
                Ok(0) | Err(_) => return since.elapsed().as_secs_f64(),
                Ok(_) => {}
            },
            () = tick => {
                if chatty && stream.write_all(&ignore).await.is_err() {
                    return since.elapsed().as_secs_f64();
                }
                assert!(since.elapsed() < Duration::from_secs(20), "never cut");
            }
        }
    }
}

#[tokio::test]
async fn cuts_a_peer_that_never_logs_in_and_frees_its_slot() {
    use tokio::io::AsyncWriteExt;
    let key = ed25519(1);
    for chatty in [true, false] {
        // One slot, so the cut peer's slot is the one a real client needs.
        let server = serve_with(&line(key.public_key()), |settings| {
            settings.max_connections = 1;
            settings.login_time = Duration::from_secs(2);
        })
        .await;
        let mut stream = tokio::net::TcpStream::connect(server.addr).await.unwrap();
        if chatty {
            stream.write_all(b"SSH-2.0-stall\r\n").await.unwrap();
        }
        let cut = seconds_until_cut(stream, chatty).await;
        assert!((1.5..5.0).contains(&cut), "cut after {cut} s");
        // Its slot is free again: a real client gets in.
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert!(login(server.addr, "root", key.clone()).await.is_some());
    }
}

#[tokio::test]
async fn a_peer_that_logs_in_outlives_the_deadline() {
    let key = ed25519(1);
    // Long enough to log in under an emulator, too.
    let server = serve_with(&line(key.public_key()), |settings| {
        settings.login_time = Duration::from_secs(5);
    })
    .await;
    let handle = login(server.addr, "root", key).await.unwrap();
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert_eq!(exec(&handle, "exit 4", None).await.status, Some(4));
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// The pids a command wrote to `file`, once it has.
async fn pids(file: &Path) -> Vec<i32> {
    for _ in 0..100 {
        if let Ok(text) = std::fs::read_to_string(file) {
            if text.ends_with('\n') {
                return text
                    .split_whitespace()
                    .map(|p| p.parse().unwrap())
                    .collect();
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no pids in {}", file.display());
}

/// Whether all of `pids` are gone within `within`.
async fn gone(pids: &[i32], within: Duration) -> bool {
    let since = std::time::Instant::now();
    while since.elapsed() < within {
        if !pids.iter().any(|&pid| alive(pid)) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test]
async fn closing_a_channel_ends_its_program_and_process_group() {
    let key = ed25519(1);
    let server = serve(&line(key.public_key()), 8).await;
    let handle = login(server.addr, "root", key).await.unwrap();
    let file = server.home.path().join("pids");
    // A background job, and a job that ignores SIGHUP and SIGTERM.
    let command = format!(
        "sleep 300 & trap '' HUP TERM; echo $$ $! > {}; while :; do sleep 1; done",
        file.display()
    );
    let channel = handle.channel_open_session().await.unwrap();
    channel.exec(true, command).await.unwrap();
    let pids = pids(&file).await;
    assert!(pids.iter().all(|&pid| alive(pid)));
    channel.close().await.unwrap();
    // SIGKILL comes 2 s after SIGTERM.
    assert!(
        gone(&pids, Duration::from_secs(6)).await,
        "{pids:?} still there"
    );
}

#[tokio::test]
async fn a_dropped_connection_ends_its_programs() {
    let key = ed25519(1);
    let server = serve(&line(key.public_key()), 8).await;
    let handle = login(server.addr, "root", key).await.unwrap();
    let (piped, typed) = (
        server.home.path().join("piped"),
        server.home.path().join("typed"),
    );
    let channel = handle.channel_open_session().await.unwrap();
    let command = format!("echo $$ > {}; sleep 300", piped.display());
    channel.exec(true, command).await.unwrap();
    let mut pids_seen = pids(&piped).await;
    if Path::new("/dev/ptmx").exists() {
        let shell = handle.channel_open_session().await.unwrap();
        shell
            .request_pty(true, "xterm", 80, 24, 0, 0, &[])
            .await
            .unwrap();
        shell.request_shell(true).await.unwrap();
        let typing = format!("echo $$ > {}; sleep 300\n", typed.display());
        shell.data(typing.as_bytes()).await.unwrap();
        pids_seen.extend(pids(&typed).await);
    }
    handle
        .disconnect(russh::Disconnect::ByApplication, "", "")
        .await
        .unwrap();
    drop(handle);
    assert!(
        gone(&pids_seen, Duration::from_secs(6)).await,
        "{pids_seen:?} still there"
    );
}

#[tokio::test]
async fn a_finished_commands_background_jobs_stay() {
    let key = ed25519(1);
    let server = serve(&line(key.public_key()), 8).await;
    let handle = login(server.addr, "root", key).await.unwrap();
    let out = exec(
        &handle,
        "sleep 30 </dev/null >/dev/null 2>&1 & echo $!",
        None,
    )
    .await;
    assert_eq!(out.status, Some(0));
    let pid: i32 = out.stdout().trim().parse().unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(alive(pid), "the background job went with its channel");
    // SAFETY: the test's own sleep.
    unsafe { libc::kill(pid, libc::SIGKILL) };
}

#[tokio::test]
async fn a_program_that_stops_reading_keeps_its_output_and_status() {
    let key = ed25519(1);
    let server = serve(&line(key.public_key()), 8).await;
    let handle = login(server.addr, "root", key).await.unwrap();
    let channel = handle.channel_open_session().await.unwrap();
    channel.exec(true, "head -c 3; exit 5").await.unwrap();
    let (mut read, write) = channel.split();
    // Sent alongside: the server closes the channel before it is all sent.
    let sending = tokio::spawn(async move {
        let data = vec![b'x'; 3 << 20];
        let _ = write.data(&data[..]).await;
    });
    let mut out = Output::default();
    while let Some(msg) = tokio::time::timeout(Duration::from_secs(20), read.wait())
        .await
        .expect("the server went quiet")
    {
        match msg {
            ChannelMsg::Data { data } => out.stdout.extend_from_slice(&data),
            ChannelMsg::ExitStatus { exit_status } => out.status = Some(exit_status),
            _ => {}
        }
    }
    sending.abort();
    assert_eq!(out.stdout(), "xxx");
    assert_eq!(out.status, Some(5));
}

#[tokio::test]
async fn opens_at_most_four_channels_per_connection() {
    let key = ed25519(1);
    let server = serve(&line(key.public_key()), 8).await;
    let handle = login(server.addr, "root", key).await.unwrap();
    let mut open = Vec::new();
    for _ in 0..4 {
        open.push(handle.channel_open_session().await.unwrap());
    }
    assert!(handle.channel_open_session().await.is_err());
    // A closed one makes room.
    open.pop().unwrap().close().await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(handle.channel_open_session().await.is_ok());
}

#[tokio::test]
async fn a_program_that_never_reads_is_ended_rather_than_stall_the_connection() {
    let key = ed25519(1);
    let server = serve_with(&line(key.public_key()), |settings| {
        settings.input_wait = Duration::from_secs(1);
    })
    .await;
    let handle = login(server.addr, "root", key).await.unwrap();
    let file = server.home.path().join("pid");
    let channel = handle.channel_open_session().await.unwrap();
    channel
        .exec(
            true,
            format!("echo $$ > {}; exec sleep 300", file.display()),
        )
        .await
        .unwrap();
    let pid = pids(&file).await;
    let (mut read, write) = channel.split();
    let sending = tokio::spawn(async move {
        let data = vec![b'x'; 8 << 20];
        let _ = write.data(&data[..]).await;
    });
    // Ended: no exit status, the channel just closes.
    while tokio::time::timeout(Duration::from_secs(20), read.wait())
        .await
        .expect("the server went quiet")
        .is_some()
    {}
    sending.abort();
    assert!(gone(&pid, Duration::from_secs(6)).await);
    // The connection is still good.
    assert_eq!(exec(&handle, "exit 2", None).await.status, Some(2));
}
