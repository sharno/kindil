//! kindil's signed GitHub releases.
//!
//! A release carries the ARM binary, `kindil`, and `kindil.minisig`, CI's
//! minisign signature over it, whose trusted comment names the version. The
//! key is the one kindil's own updater trusts ([`public_key`], read from
//! `src/update.rs` at build time, so there is one key in the repo). The
//! KUAL launcher files and the example settings come from the source
//! tarball GitHub makes for the release's tag.
use anyhow::{bail, ensure, Context};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

pub const REPO: &str = "sharno/kindil";
/// The release binary is about 16 MB; the source tarball a few.
const MAX_DOWNLOAD: u64 = 128 << 20;
const UPDATE_RS: &str = include_str!("../../src/update.rs");

/// The public key in `update_rs` (kindil's `src/update.rs`).
pub fn public_key_in(update_rs: &str) -> anyhow::Result<String> {
    update_rs
        .lines()
        .find_map(|line| {
            line.strip_prefix("const PUBLIC_KEY: &str = \"")?
                .strip_suffix("\";")
        })
        .map(str::to_owned)
        .context("no PUBLIC_KEY in src/update.rs")
}

/// The key CI signs releases with: kindil's updater's.
pub fn public_key() -> String {
    public_key_in(UPDATE_RS).expect("src/update.rs has the release key")
}

/// Checks `bytes` against `signature` by `key`, and that the signature names
/// `version` (`kindil v0.13.2`), so an older signed release can't pass for
/// a newer one.
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

/// `0.13.2` from `v0.13.2` or `0.13.2`, if it is a version.
pub fn normalize(version: &str) -> anyhow::Result<String> {
    let version = version.trim().trim_start_matches('v');
    let parts: Vec<&str> = version.split('.').collect();
    ensure!(
        parts.len() == 3
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())),
        "{version:?} is not a version like 0.13.2"
    );
    Ok(version.to_owned())
}

/// Bytes so far and, if the server said, in all.
pub type Progress<'a> = &'a mut dyn FnMut(u64, Option<u64>);

pub struct Github {
    agent: ureq::Agent,
}

impl Default for Github {
    fn default() -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(300)))
            .user_agent(concat!("kindil-installer/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Self { agent }
    }
}

#[derive(serde::Deserialize)]
struct Latest {
    tag_name: String,
}

impl Github {
    /// The latest release's version, `0.13.2`.
    pub fn latest(&self) -> anyhow::Result<String> {
        let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
        let json = self
            .agent
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .call()
            .with_context(|| format!("asking GitHub for the latest release ({url})"))?
            .body_mut()
            .read_to_string()?;
        let latest: Latest = serde_json::from_str(&json)?;
        normalize(&latest.tag_name)
    }

    pub fn get(&self, url: &str, progress: Progress) -> anyhow::Result<Vec<u8>> {
        let mut response = self
            .agent
            .get(url)
            .call()
            .with_context(|| format!("downloading {url}"))?;
        let total = response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok());
        let mut reader = response
            .body_mut()
            .with_config()
            .limit(MAX_DOWNLOAD)
            .reader();
        let mut out = Vec::with_capacity(total.unwrap_or(0) as usize);
        let mut chunk = vec![0; 64 << 10];
        loop {
            let n = reader
                .read(&mut chunk)
                .with_context(|| format!("downloading {url}"))?;
            if n == 0 {
                break;
            }
            out.extend_from_slice(&chunk[..n]);
            progress(out.len() as u64, total);
        }
        Ok(out)
    }

    /// Release `version`'s binary, its signature checked, with the launcher
    /// files and examples from its tag.
    pub fn download(
        &self,
        version: &str,
        mut progress: impl FnMut(&str, u64, Option<u64>),
    ) -> anyhow::Result<Release> {
        let version = normalize(version)?;
        let base = format!("https://github.com/{REPO}/releases/download/v{version}");
        let signature = self.get(&format!("{base}/kindil.minisig"), &mut |_, _| {})?;
        let signature = String::from_utf8(signature).context("the signature isn't text")?;
        let binary = self.get(&format!("{base}/kindil"), &mut |n, t| {
            progress("kindil", n, t)
        })?;
        verify(&public_key(), &version, &binary, &signature)?;
        let source = self.get(
            &format!("https://github.com/{REPO}/archive/refs/tags/v{version}.tar.gz"),
            &mut |n, t| progress("launcher", n, t),
        )?;
        Release::new(version, binary, &source)
    }
}

/// A file to write under the volume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    /// Relative to the volume: `extensions/kindil/menu.json`.
    pub path: PathBuf,
    pub bytes: Vec<u8>,
}

/// A release, ready to install: its signature already checked.
#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    /// The KUAL extension, `extensions/kindil/...`, the binary last.
    pub extension: Vec<File>,
    /// `kindil/miqat.toml` and `kindil/kindil.toml`, for a first install.
    pub examples: Vec<File>,
}

impl Release {
    /// From a checked `binary` and the tag's source tarball (`.tar.gz`, one
    /// top folder such as `kindil-0.13.2/`).
    pub fn new(version: String, binary: Vec<u8>, source: &[u8]) -> anyhow::Result<Self> {
        let mut extension = Vec::new();
        let mut examples = Vec::new();
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(source));
        for entry in archive.entries().context("reading the source tarball")? {
            let mut entry = entry?;
            if !entry.header().entry_type().is_file() {
                continue;
            }
            let path = entry.path()?.into_owned();
            // The top folder's name follows the tag; drop it.
            let inside: PathBuf = path.components().skip(1).collect();
            if inside
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
            {
                bail!("unsafe path in the source tarball: {}", path.display());
            }
            let mut bytes = Vec::new();
            if let Ok(rest) = inside.strip_prefix("kindle/extensions/kindil") {
                // The release's own binary goes in below, never another.
                if rest == Path::new("bin/kindil") {
                    continue;
                }
                entry.read_to_end(&mut bytes)?;
                extension.push(File {
                    path: Path::new("extensions/kindil").join(rest),
                    bytes,
                });
            } else if inside == Path::new("examples/miqat.toml") {
                entry.read_to_end(&mut bytes)?;
                examples.push(File {
                    path: "kindil/miqat.toml".into(),
                    bytes,
                });
            } else if inside == Path::new("examples/kindil.toml") {
                entry.read_to_end(&mut bytes)?;
                examples.push(File {
                    path: "kindil/kindil.toml".into(),
                    bytes: for_the_kindle(&String::from_utf8_lossy(&bytes)).into_bytes(),
                });
            }
        }
        ensure!(
            extension
                .iter()
                .any(|f| f.path == Path::new("extensions/kindil/menu.json")),
            "the source tarball has no kindle/extensions/kindil/menu.json"
        );
        extension.sort_by(|a, b| a.path.cmp(&b.path));
        examples.sort_by(|a, b| a.path.cmp(&b.path));
        extension.push(File {
            path: "extensions/kindil/bin/kindil".into(),
            bytes: binary,
        });
        Ok(Self {
            version,
            extension,
            examples,
        })
    }
}

/// The example `kindil.toml` points at the repo's `examples/` for a desktop
/// run; on the Kindle every key's default is right, so those lines are
/// commented out.
pub fn for_the_kindle(example: &str) -> String {
    example
        .lines()
        .map(|line| {
            let key = line.split('=').next().unwrap_or("").trim();
            if !line.starts_with('#') && line.contains("\"examples/") && !key.is_empty() {
                format!("# {line}")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_updaters_key() {
        let update_rs = "x\nconst PUBLIC_KEY: &str = \"RWTkey+/=\";\ny\n";
        assert_eq!(public_key_in(update_rs).unwrap(), "RWTkey+/=");
        assert!(public_key_in("nothing here").is_err());
        assert!(public_key().starts_with("RW"));
    }

    /// src/update.rs's fixture: a throwaway key and its signature over
    /// `kindil test fixture`, trusted comment `fixture`.
    const TEST_KEY: &str = "RWTyZYsg/MjSsBaHXWuR+TBprmyzIALfj/Jo8mQpkcfLPUGWjscxzlDo";
    const TEST_SIGNATURE: &str = "untrusted comment: signature from minisign secret key
RUTyZYsg/MjSsK0vy1RouLf0XdVUA7ZPpwDpRgPJdurL1mtO4sNF0bQJ+0ZJYJFJ/wXxSX2za3lcgh59KADFtps5BqsasgGeCwk=
trusted comment: fixture
SW/I/JX/SBpxgS4DThvhL91wP3EJoH7taYJoyf43DW8zZZ3wB65CwRxKLqoEs5+fTbZ4yzLmtSU3WMSUk80iBQ==
";

    #[test]
    fn verifies_the_signature_and_the_version_it_names() {
        let fixture = b"kindil test fixture";
        // Signed by the key, but for "fixture", not a kindil version.
        let error = verify(TEST_KEY, "1.0.0", fixture, TEST_SIGNATURE).unwrap_err();
        assert!(error.to_string().contains("is for \"fixture\""), "{error}");
        let error = verify(TEST_KEY, "1.0.0", b"kindil test fixturE", TEST_SIGNATURE).unwrap_err();
        assert!(error.to_string().contains("bad signature"), "{error}");
        assert!(verify(TEST_KEY, "1.0.0", fixture, "not a signature").is_err());
        // The release key did not sign it.
        assert!(verify(&public_key(), "1.0.0", fixture, TEST_SIGNATURE).is_err());
    }

    #[test]
    fn normalizes_versions() {
        assert_eq!(normalize("v0.13.2").unwrap(), "0.13.2");
        assert_eq!(normalize(" 1.2.3 ").unwrap(), "1.2.3");
        assert!(normalize("1.2").is_err());
        assert!(normalize("1.2.x").is_err());
        assert!(normalize("../1.2.3").is_err());
    }

    #[test]
    fn makes_the_example_settings_fit_the_kindle() {
        let example = "dashboard = \"miqat\"\nmiqat_config = \"examples/miqat.toml\" # x\n\
                       books = \"examples/books\"\n# books = \"examples/books\"\n";
        assert_eq!(
            for_the_kindle(example),
            "dashboard = \"miqat\"\n# miqat_config = \"examples/miqat.toml\" # x\n\
             # books = \"examples/books\"\n# books = \"examples/books\"\n"
        );
    }
}
