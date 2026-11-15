//! Installing a signed GitHub release on the Kindle.
//!
//! The release's binary is checked against the key kindil's own updater
//! trusts (`src/update.rs`) and bundled with the launcher files from the
//! release's tag. On the Kindle the release installs itself: its binary,
//! uploaded beside the bundle, runs `kindil install` (src/install.rs), which
//! rolls back unless kindil comes back up with its Wi-Fi.
use crate::device::Device;
use anyhow::{bail, ensure, Context};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

const REPO: &str = "sharno/kindil";
const DEPLOY: &str = "/mnt/us/kindil-deploy";
const LOG: &str = "/var/local/kindil/deploy.log";
/// How long `kindil install` gets to show it runs: a line in the log, or
/// still running.
const LAUNCH: Duration = Duration::from_secs(30);

/// A release's binary, its signature checked.
pub struct Binary {
    pub version: String,
    pub bytes: Vec<u8>,
}

/// The public key in `src/update.rs`: one key, the one the updater trusts.
pub fn public_key(update_rs: &str) -> anyhow::Result<String> {
    update_rs
        .lines()
        .find_map(|line| {
            line.strip_prefix("const PUBLIC_KEY: &str = \"")?
                .strip_suffix("\";")
        })
        .map(str::to_owned)
        .context("no PUBLIC_KEY in src/update.rs")
}

/// Checks `bytes` against `signature` and that the signature names `version`.
pub fn verify(key: &str, version: &str, bytes: &[u8], signature: &str) -> anyhow::Result<()> {
    let key = minisign_verify::PublicKey::from_base64(key).context("the public key")?;
    let signature = minisign_verify::Signature::decode(signature).context("the signature")?;
    key.verify(bytes, &signature, false)
        .map_err(|e| anyhow::anyhow!("v{version}: bad signature: {e}"))?;
    ensure!(
        signature.trusted_comment() == format!("kindil v{version}"),
        "v{version}: the signature is for {:?}",
        signature.trusted_comment()
    );
    Ok(())
}

fn download(url: &str) -> anyhow::Result<Vec<u8>> {
    let mut response = ureq::get(url)
        .call()
        .with_context(|| format!("GET {url}"))?;
    Ok(response
        .body_mut()
        .with_config()
        .limit(64 << 20)
        .read_to_vec()?)
}

pub fn fetch(root: &Path, version: &str) -> anyhow::Result<Binary> {
    let key = public_key(&std::fs::read_to_string(root.join("src/update.rs"))?)?;
    let base = format!("https://github.com/{REPO}/releases/download/v{version}");
    let bytes = download(&format!("{base}/kindil"))?;
    let signature = String::from_utf8(download(&format!("{base}/kindil.minisig"))?)?;
    verify(&key, version, &bytes, &signature)?;
    Ok(Binary {
        version: version.to_owned(),
        bytes,
    })
}

/// The launcher files at `v<version>`, as `git archive` gives them; fetches
/// the tag first if this clone lacks it (releases are tagged on GitHub).
fn launcher_files(root: &Path, version: &str) -> anyhow::Result<Vec<u8>> {
    let tag = format!("v{version}");
    let git = |args: &[&str]| Command::new("git").arg("-C").arg(root).args(args).output();
    if !git(&["rev-parse", "-q", "--verify", &format!("refs/tags/{tag}")])?
        .status
        .success()
    {
        let refspec = format!("refs/tags/{tag}:refs/tags/{tag}");
        ensure!(
            git(&["fetch", "-q", "origin", &refspec])?.status.success(),
            "can't fetch {tag}"
        );
    }
    let archive = git(&["archive", &tag, "kindle/extensions"])?;
    ensure!(archive.status.success(), "git archive {tag} failed");
    Ok(archive.stdout)
}

/// The bundle `kindil install` unpacks: `extensions/kindil/...` with
/// `binary` as `bin/kindil`, gzipped; with its SHA-256, which it checks.
pub fn bundle(launcher: &[u8], binary: &[u8]) -> anyhow::Result<(Vec<u8>, String)> {
    let mut out = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::default(),
    ));
    out.mode(tar::HeaderMode::Deterministic);
    for entry in tar::Archive::new(launcher).entries()? {
        let mut entry = entry?;
        // Files only: unpacking makes their directories, and git archive's
        // directory entries would include `kindle/` itself, empty once
        // stripped.
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path()?.into_owned();
        let Ok(inside) = path.strip_prefix("kindle") else {
            continue;
        };
        // The release's own binary goes in below, never a stale one.
        if inside == Path::new("extensions/kindil/bin/kindil") {
            continue;
        }
        let mut header = entry.header().clone();
        let mut data = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut data)?;
        out.append_data(&mut header, inside, data.as_slice())?;
    }
    let mut header = tar::Header::new_gnu();
    header.set_size(binary.len() as u64);
    header.set_mode(0o755);
    header.set_entry_type(tar::EntryType::Regular);
    out.append_data(&mut header, "extensions/kindil/bin/kindil", binary)?;
    let bundle = out.into_inner()?.finish()?;
    let sum = Sha256::digest(&bundle)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok((bundle, sum))
}

/// What `kindil install` logged for run `id`: done, rolled back or failed,
/// if yet.
pub fn outcome(log: &str, id: &str) -> Option<Result<String, String>> {
    let prefix = format!("install {id}: ");
    let lines: Vec<&str> = log.lines().filter(|line| line.contains(&prefix)).collect();
    let summary = lines.join("\n");
    lines.iter().rev().find_map(|line| {
        let said = line.split_once(&prefix)?.1;
        if said.starts_with("done") {
            Some(Ok(summary.clone()))
        } else if said.starts_with("rolled back") || said.starts_with("failed") {
            Some(Err(summary.clone()))
        } else {
            None
        }
    })
}

pub fn install(
    root: &Path,
    version: &str,
    rollback: Option<&str>,
    hours: u64,
) -> anyhow::Result<()> {
    let device = Device::from_env()?;
    let new = fetch(root, version)?;
    let (bundle, sum) = bundle(&launcher_files(root, version)?, &new.bytes)?;
    let work = tempdir()?;
    let mut files = vec![
        write(&work, "bundle.tgz", &bundle)?,
        write(&work, "bundle.sha256", format!("{sum}\n").as_bytes())?,
        write(&work, "kindil.install", &new.bytes)?,
    ];
    if let Some(rollback) = rollback {
        files.push(write(
            &work,
            "kindil.rollback",
            &fetch(root, rollback)?.bytes,
        )?);
    }
    println!(
        "v{} bundled and verified; waiting for the Kindle",
        new.version
    );
    device.wait(hours)?;
    let id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs()
        .to_string();
    device.run(&format!(
        "mkdir -p {DEPLOY} && rm -f {DEPLOY}/kindil.rollback"
    ))?;
    let paths: Vec<&Path> = files.iter().map(|p| p.as_path()).collect();
    device.put(&paths, DEPLOY)?;
    // The log's directory first: on a Kindle kindil never ran on, the
    // redirect would fail and the install never start.
    let launched = device.run(&format!(
        "mkdir -p /var/local/kindil && chmod 755 {DEPLOY}/kindil.install && \
         {{ setsid {DEPLOY}/kindil.install install {id} </dev/null >>{LOG} 2>&1 & \
         echo \"launched $!\"; }}"
    ))?;
    let pid = launched
        .lines()
        .find_map(|line| line.strip_prefix("launched "))
        .and_then(|pid| pid.trim().parse::<u32>().ok())
        .with_context(|| format!("couldn't launch kindil install on the Kindle: {launched}"))?;
    started(&device, &id, pid)?;
    println!("installing v{} (run {id})", new.version);
    // The Kindle drops off the network while kindil restarts: keep asking.
    let since = Instant::now();
    while since.elapsed() < Duration::from_secs(15 * 60) {
        if let Ok(log) = device.run(&format!("grep 'install {id}: ' {LOG}")) {
            match outcome(&log, &id) {
                Some(Ok(summary)) => {
                    println!("{summary}");
                    return Ok(());
                }
                Some(Err(summary)) => bail!("{summary}"),
                None => {}
            }
        }
        std::thread::sleep(Duration::from_secs(10));
    }
    bail!("no outcome after 15 minutes; see /var/local/kindil/deploy.log on the Kindle")
}

/// Whether install `id` got going, from its lines in the log and, after
/// them, "alive" or "gone" for its process: Ok(true) if it did, Ok(false)
/// if it can't be told yet, Err with what it said if it ended first.
pub fn launch(said: &str, id: &str) -> Result<bool, String> {
    let prefix = format!("install {id}: ");
    if said.lines().any(|line| line.contains(&prefix)) {
        return Ok(true);
    }
    if said.lines().any(|line| line.trim() == "gone") {
        let said: Vec<&str> = said.lines().filter(|l| l.trim() != "gone").collect();
        return Err(said.join("\n"));
    }
    Ok(false)
}

/// Waits up to LAUNCH for install `id` (process `pid`) to log a line, and
/// fails at once if it exits without one: a binary that won't run on the
/// Kindle, a full disk.
fn started(device: &Device, id: &str, pid: u32) -> anyhow::Result<()> {
    let since = Instant::now();
    loop {
        // The log after the process check: a line written as it exits is
        // still seen.
        let said = device.run(&format!(
            "if kill -0 {pid} 2>/dev/null; then s=alive; else s=gone; fi; \
             grep 'install {id}: ' {LOG} || tail -n 5 {LOG} 2>/dev/null; echo $s"
        ))?;
        match launch(&said, id) {
            Ok(true) => return Ok(()),
            Err(said) => bail!("kindil install {id} exited at once; the log ends:\n{said}"),
            Ok(false) if since.elapsed() >= LAUNCH => {
                // Still running: the backup of a large extension can take a
                // while on the PW1.
                println!("kindil install {id} runs but hasn't logged yet");
                return Ok(());
            }
            Ok(false) => std::thread::sleep(Duration::from_secs(2)),
        }
    }
}

fn tempdir() -> anyhow::Result<std::path::PathBuf> {
    let dir = std::env::temp_dir().join(format!("kindil-xtask-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> anyhow::Result<std::path::PathBuf> {
    let path = dir.join(name);
    std::fs::write(&path, bytes)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_updaters_key() {
        let update_rs = "x\nconst PUBLIC_KEY: &str = \"RWTkey+/=\";\ny\n";
        assert_eq!(public_key(update_rs).unwrap(), "RWTkey+/=");
        assert!(public_key("nothing here").is_err());
        // The real one.
        let real = include_str!("../../src/update.rs");
        assert!(public_key(real).unwrap().starts_with("RW"));
    }

    #[test]
    fn rejects_a_signature_that_doesnt_match() {
        let real = public_key(include_str!("../../src/update.rs")).unwrap();
        let signature = "untrusted comment: x\nRUQ=\ntrusted comment: kindil v1.0.0\nAAAA\n";
        assert!(verify(&real, "1.0.0", b"kindil", signature).is_err());
    }

    fn launcher() -> Vec<u8> {
        let mut out = tar::Builder::new(Vec::new());
        // Directory entries, as git archive writes them.
        for dir in ["kindle/", "kindle/extensions/", "kindle/extensions/kindil/"] {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            header.set_mode(0o755);
            out.append_data(&mut header, dir, std::io::empty()).unwrap();
        }
        for (path, body) in [
            ("kindle/extensions/kindil/menu.json", "{}"),
            ("kindle/extensions/kindil/bin/start.sh", "#!/bin/sh\n"),
            ("kindle/extensions/kindil/bin/kindil", "a stale binary"),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            out.append_data(&mut header, path, body.as_bytes()).unwrap();
        }
        out.into_inner().unwrap()
    }

    #[test]
    fn bundles_the_launcher_with_the_release_binary() {
        let (bundle, sum) = bundle(&launcher(), b"the release").unwrap();
        assert_eq!(sum.len(), 64);
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bundle.as_slice()));
        let mut found: Vec<(String, String, u32)> = archive
            .entries()
            .unwrap()
            .map(|entry| {
                let mut entry = entry.unwrap();
                let path = entry.path().unwrap().display().to_string();
                let mode = entry.header().mode().unwrap();
                let mut body = String::new();
                std::io::Read::read_to_string(&mut entry, &mut body).unwrap();
                (path, body, mode)
            })
            .collect();
        found.sort();
        assert_eq!(
            found,
            [
                (
                    "extensions/kindil/bin/kindil".into(),
                    "the release".into(),
                    0o755
                ),
                (
                    "extensions/kindil/bin/start.sh".into(),
                    "#!/bin/sh\n".into(),
                    0o644
                ),
                ("extensions/kindil/menu.json".into(), "{}".into(), 0o644),
            ]
        );
        // The same inputs, the same bundle.
        assert_eq!(super::bundle(&launcher(), b"the release").unwrap().1, sum);
    }

    #[test]
    fn tells_whether_the_install_got_going() {
        assert_eq!(launch("alive\n", "7"), Ok(false), "too early to tell");
        assert_eq!(
            launch(
                "2026-11-15 00:12:25 install 7: installing kindil 0.9.1\ngone\n",
                "7"
            ),
            Ok(true),
            "it said something: the outcome tells the rest"
        );
        assert_eq!(
            launch("sh: kindil.install: not found\ngone\n", "7"),
            Err("sh: kindil.install: not found".into())
        );
        assert_eq!(
            launch("x install 6: done: kindil 0.9.0\ngone\n", "7"),
            Err("x install 6: done: kindil 0.9.0".into()),
            "another run's lines don't count"
        );
    }

    #[test]
    fn reads_the_installs_outcome() {
        let log = "2026-11-15 00:12:25 install 7: installing kindil 0.9.1\n\
                   2026-11-15 00:13:27 install 6: done: kindil 0.9.0\n\
                   2026-11-15 00:13:27 install 7: up 21 s after installing\n";
        assert_eq!(outcome(log, "7"), None, "still going");
        let done = format!("{log}2026-11-15 00:14:27 install 7: done: kindil 0.9.1\n");
        assert!(matches!(outcome(&done, "7"), Some(Ok(summary)) if summary.lines().count() == 3));
        let back = format!("{log}2026-11-15 00:16:27 install 7: rolled back to kindil 0.9.0\n");
        assert!(matches!(outcome(&back, "7"), Some(Err(_))));
        assert!(matches!(
            outcome("x install 9: failed: checksum mismatch\n", "9"),
            Some(Err(_))
        ));
    }
}
