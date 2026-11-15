//! The keys kindil's SSH server trusts, and how new ones get there.
//!
//! `/var/local/kindil/ssh/authorized_keys` is the only list the server
//! reads, on storage only root can write. USBNetwork's list on the user
//! storage (`/mnt/us/usbnet/etc/authorized_keys`, FAT: anyone with the
//! Kindle on a USB cable can change it) seeds it once, when there is no list
//! yet ([`bootstrap`]); afterwards a key there that isn't trusted yet waits
//! for someone at the Kindle to look at its fingerprint and tap Trust
//! ([`trust`]) or Ignore ([`ignore`], remembered in `ignored_keys`).
use base64::Engine;
use sha2::{Digest, Sha256};
use std::path::Path;

/// One key of an `authorized_keys` file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    /// As ssh-keygen names it: `ED25519`, `RSA`, `ECDSA`...
    pub kind: String,
    pub bits: u32,
    /// `SHA256:` and the unpadded base64 of the blob's SHA-256.
    pub fingerprint: String,
    pub comment: String,
    /// `command=` and the like, which kindil's server doesn't enforce: it
    /// ignores keys with any.
    pub options: bool,
    blob: Vec<u8>,
    line: String,
}

impl Key {
    /// The line `ssh-keygen -lf` prints for it.
    pub fn keygen_line(&self) -> String {
        let comment = if self.comment.is_empty() {
            "no comment"
        } else {
            &self.comment
        };
        format!(
            "{} {} {comment} ({})",
            self.bits, self.fingerprint, self.kind
        )
    }
}

/// The keys in an `authorized_keys` text; other lines are skipped.
pub fn parse(text: &str) -> Vec<Key> {
    text.lines().filter_map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<Key> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    // The key type is the first word that is one, followed by its blob:
    // options, if any, come before it.
    let words: Vec<(usize, &str)> = line
        .split_whitespace()
        .scan(0, |at, word| {
            let start = line[*at..].find(word)? + *at;
            *at = start + word.len();
            Some((start, word))
        })
        .collect();
    words.windows(2).enumerate().find_map(|(i, pair)| {
        let [(type_at, kind), (blob_at, encoded)] = [pair[0], pair[1]];
        let blob = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()?;
        let named = ssh_string(&blob, 0)?.0;
        if named != kind.as_bytes() {
            return None;
        }
        let (name, bits) = describe(kind, &blob)?;
        let comment = line[blob_at + encoded.len()..].trim().to_owned();
        let digest = Sha256::digest(&blob);
        Some(Key {
            kind: name,
            bits,
            fingerprint: format!(
                "SHA256:{}",
                base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest)
            ),
            comment,
            options: i > 0 || type_at > 0,
            line: line.to_owned(),
            blob,
        })
    })
}

/// An SSH wire string at `at`: its bytes, and where the next one starts.
fn ssh_string(blob: &[u8], at: usize) -> Option<(&[u8], usize)> {
    let len = u32::from_be_bytes(blob.get(at..at + 4)?.try_into().ok()?) as usize;
    let start = at + 4;
    Some((blob.get(start..start.checked_add(len)?)?, start + len))
}

/// ssh-keygen's name for a key type, and the key's size in bits.
fn describe(kind: &str, blob: &[u8]) -> Option<(String, u32)> {
    Some(match kind {
        "ssh-ed25519" => ("ED25519".into(), 256),
        "sk-ssh-ed25519@openssh.com" => ("ED25519-SK".into(), 256),
        "ssh-rsa" => {
            // string "ssh-rsa", mpint e, mpint n.
            let (_, after_type) = ssh_string(blob, 0)?;
            let (_, after_e) = ssh_string(blob, after_type)?;
            let (n, _) = ssh_string(blob, after_e)?;
            ("RSA".into(), bit_length(n))
        }
        "ecdsa-sha2-nistp256" => ("ECDSA".into(), 256),
        "ecdsa-sha2-nistp384" => ("ECDSA".into(), 384),
        "ecdsa-sha2-nistp521" => ("ECDSA".into(), 521),
        "sk-ecdsa-sha2-nistp256@openssh.com" => ("ECDSA-SK".into(), 256),
        "ssh-dss" => ("DSA".into(), 1024),
        _ => return None,
    })
}

/// The bits of a big-endian magnitude, leading zeros aside.
fn bit_length(n: &[u8]) -> u32 {
    let n: Vec<u8> = n.iter().copied().skip_while(|&b| b == 0).collect();
    match n.first() {
        None => 0,
        Some(first) => (n.len() as u32 - 1) * 8 + (8 - first.leading_zeros()),
    }
}

fn read(path: &Path) -> std::io::Result<String> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let mut text = String::new();
    std::io::Read::read_to_string(&mut file, &mut text)?;
    Ok(text)
}

/// The text at `path`, or nothing if there's no file.
fn read_or_empty(path: &Path) -> std::io::Result<String> {
    match read(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        other => other,
    }
}

/// The first start: with no trusted list yet, USBNetwork's becomes it, as
/// it is (0600). Whether it did. Never once there is a list, even an empty
/// one.
pub fn bootstrap(usb: &Path, trusted: &Path) -> anyhow::Result<bool> {
    if std::fs::symlink_metadata(trusted).is_ok() {
        return Ok(false);
    }
    let keys = match std::fs::read(usb) {
        Ok(keys) => keys,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    crate::ssh::write_new(trusted, &keys, 0o600)?;
    Ok(true)
}

/// The trusted keys.
pub fn trusted(trusted: &Path) -> anyhow::Result<Vec<Key>> {
    Ok(parse(&read_or_empty(trusted)?))
}

/// Keys on the USB storage that are neither trusted nor ignored, each once.
pub fn new_on_usb(usb: &Path, trusted: &Path, ignored: &Path) -> anyhow::Result<Vec<Key>> {
    let known = parse(&read_or_empty(trusted)?);
    let ignored = read_or_empty(ignored)?;
    let ignored: Vec<&str> = ignored.split_whitespace().collect();
    let mut new: Vec<Key> = Vec::new();
    for key in parse(&read_or_empty(usb)?) {
        if !known.iter().any(|k| k.blob == key.blob)
            && !ignored.contains(&key.fingerprint.as_str())
            && !new.iter().any(|k| k.blob == key.blob)
        {
            new.push(key);
        }
    }
    Ok(new)
}

/// Appends to the trusted list the keys on the USB storage that are new
/// and have one of `fingerprints` (those shown); how many.
pub fn trust(
    usb: &Path,
    trusted: &Path,
    ignored: &Path,
    fingerprints: &[String],
) -> anyhow::Result<usize> {
    let chosen: Vec<Key> = new_on_usb(usb, trusted, ignored)?
        .into_iter()
        .filter(|key| fingerprints.contains(&key.fingerprint))
        .collect();
    if chosen.is_empty() {
        return Ok(0);
    }
    let mut text = read_or_empty(trusted)?;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    for key in &chosen {
        text.push_str(&key.line);
        text.push('\n');
    }
    crate::ssh::write_new(trusted, text.as_bytes(), 0o600)?;
    Ok(chosen.len())
}

/// Remembers `fingerprints` as not to be trusted, so they aren't offered
/// again.
pub fn ignore(ignored: &Path, fingerprints: &[String]) -> anyhow::Result<()> {
    let mut text = read_or_empty(ignored)?;
    for fingerprint in fingerprints {
        if !text.split_whitespace().any(|f| f == fingerprint) {
            text.push_str(fingerprint);
            text.push('\n');
        }
    }
    crate::ssh::write_new(ignored, text.as_bytes(), 0o600)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Throwaway keys made by ssh-keygen; each with what `ssh-keygen -lf`
    // printed for it (OpenSSH on macOS).
    const ED25519: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMF765g2MiJUVDULy+qyyRmfG2xJ3H89OXyvOvexC0WS kindil test ed25519";
    const ED25519_LF: &str =
        "256 SHA256:Tmj8P5/f8150RFsVfCD8guzdZW/Exkcf9MVSDzekGsg kindil test ed25519 (ED25519)";
    const RSA: &str = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABgQDJZI39InfRacKgPnr8s6hVcwEdsKwcYMuxWR6w/3BVr9krGhs34R/Cu5fzO/dH+swzTbzvmUR3fU4rKuG45UzgniS9Z0bchXxEMiMxziwAvr4J0mcwbdiR6Pi6qqVEqlGuJ64E5ZUpQx4ZWUGzpD7uhp6rXIPIARlUEmJjVC27+kVTkWXOJgZNHq9XS4ObHr29JRdc888GPMT0DJ5Df3Qz393CJJt8+NqDzfI2j0BrQ1AlkDsmqBZzOvhXg4QAsJEtPDeJEFnLOHUz/l3b2y6uddQ73YjgMMtnYcxFhwLZHnLZ/6s5bCYbGzAv76+9PGnHNyr2lDPafOCpDISNQHNz/VNXOdINrIr0+Qox4FnVHGGMtcaNTto4ocEvUiSoyMcdNMQfGEF3qS/bpse28PYNHqXGUa1/n/PpLuWmpODz9xRJsADwDXYYwrev8ju5u8sOmRcrAanw3iV9WSTAc5B8WS1EfLVbNb4pLYLdoUJDjGe7LbT/AWFzkQvuSdvJh+0= kindil test rsa";
    const RSA_LF: &str =
        "3072 SHA256:kASt+czojb6SDSr4yElIlLfOJ/VLtLLNJiTGIu8Qql4 kindil test rsa (RSA)";
    const BARE: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJok5NGyJ7Bdtqh00uQYOBnbVKRW3Xb2VFwHqZGQ3GSq";
    const BARE_LF: &str =
        "256 SHA256:gSVZ8vdTWwtjK5i3jP532dBwjRNILqzeGzlthEoBG1A no comment (ED25519)";
    const ECDSA: &str = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBAuNhF8aeZIETX8LC/O073qbHfwZHrrhOHmonc/ThdIUgaIKZrH5QAJmJ48YHYZ5A6CJN3UHzp7iiQxBPBix5QI= ec key";
    const ECDSA_LF: &str = "256 SHA256:2xWNKSrR5FzpWaljiABPfstJBtI3jra8BSbVE9lDGn8 ec key (ECDSA)";

    fn one(line: &str) -> Key {
        let keys = parse(line);
        assert_eq!(keys.len(), 1, "{line}");
        keys.into_iter().next().unwrap()
    }

    #[test]
    fn fingerprints_as_ssh_keygen_does() {
        for (line, keygen) in [
            (ED25519, ED25519_LF),
            (RSA, RSA_LF),
            (BARE, BARE_LF),
            (ECDSA, ECDSA_LF),
        ] {
            assert_eq!(one(line).keygen_line(), keygen);
        }
    }

    #[test]
    fn reads_options_comments_and_skips_the_rest() {
        let key = one(&format!("command=\"echo hi there\",no-pty {ED25519}"));
        assert!(key.options);
        assert_eq!(key.comment, "kindil test ed25519");
        assert!(!one(ED25519).options);
        let text = format!("# mine\n\nnot a key\nssh-ed25519 !!!notbase64\n{RSA}\n  {BARE}  \n");
        let keys = parse(&text);
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].kind, "RSA");
        // A blob that names another type isn't that type.
        let lying = ED25519.replacen("ssh-ed25519", "ssh-rsa", 1);
        assert!(parse(&lying).is_empty());
    }

    struct Files {
        _dir: tempfile::TempDir,
        usb: std::path::PathBuf,
        trusted: std::path::PathBuf,
        ignored: std::path::PathBuf,
    }

    fn files() -> Files {
        let dir = tempfile::tempdir().unwrap();
        let path = |name: &str| dir.path().join(name);
        Files {
            usb: path("usb"),
            trusted: path("authorized_keys"),
            ignored: path("ignored_keys"),
            _dir: dir,
        }
    }

    #[test]
    fn usb_keys_seed_the_list_only_while_there_is_none() {
        let f = files();
        // No USB keys either: nothing made.
        assert!(!bootstrap(&f.usb, &f.trusted).unwrap());
        assert!(!f.trusted.exists());
        std::fs::write(&f.usb, format!("{ED25519}\n")).unwrap();
        assert!(bootstrap(&f.usb, &f.trusted).unwrap());
        assert_eq!(trusted(&f.trusted).unwrap(), parse(ED25519));
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&f.trusted).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        // Afterwards a key added on the USB storage is only offered.
        std::fs::write(&f.usb, format!("{ED25519}\n{RSA}\n")).unwrap();
        assert!(!bootstrap(&f.usb, &f.trusted).unwrap());
        assert_eq!(trusted(&f.trusted).unwrap().len(), 1);
        let new = new_on_usb(&f.usb, &f.trusted, &f.ignored).unwrap();
        assert_eq!(new, parse(RSA));
        // An emptied list is still a list: no seeding again.
        std::fs::write(&f.trusted, "").unwrap();
        assert!(!bootstrap(&f.usb, &f.trusted).unwrap());
        assert!(trusted(&f.trusted).unwrap().is_empty());
    }

    #[test]
    fn trusts_only_the_keys_shown() {
        let f = files();
        std::fs::write(&f.trusted, format!("{ED25519}\n")).unwrap();
        std::fs::write(&f.usb, format!("{ED25519}\n{RSA}\n{BARE}\n{RSA}\n")).unwrap();
        let new = new_on_usb(&f.usb, &f.trusted, &f.ignored).unwrap();
        assert_eq!(new.len(), 2, "each new key once");
        let rsa = one(RSA).fingerprint;
        // A fingerprint not among the new keys does nothing.
        let nothing = trust(&f.usb, &f.trusted, &f.ignored, &[one(ECDSA).fingerprint]);
        assert_eq!(nothing.unwrap(), 0);
        assert_eq!(trust(&f.usb, &f.trusted, &f.ignored, &[rsa]).unwrap(), 1);
        let now = trusted(&f.trusted).unwrap();
        assert_eq!(now, parse(&format!("{ED25519}\n{RSA}")));
        assert_eq!(
            new_on_usb(&f.usb, &f.trusted, &f.ignored).unwrap(),
            parse(BARE)
        );
        // Ignored, it isn't offered again.
        ignore(&f.ignored, &[one(BARE).fingerprint]).unwrap();
        ignore(&f.ignored, &[one(BARE).fingerprint]).unwrap();
        assert!(new_on_usb(&f.usb, &f.trusted, &f.ignored)
            .unwrap()
            .is_empty());
        assert_eq!(
            std::fs::read_to_string(&f.ignored).unwrap().lines().count(),
            1
        );
    }

    #[test]
    fn a_symlinked_list_is_not_read() {
        let f = files();
        std::fs::write(&f.usb, format!("{ED25519}\n")).unwrap();
        std::os::unix::fs::symlink(&f.usb, &f.trusted).unwrap();
        assert!(trusted(&f.trusted).is_err());
        assert!(!bootstrap(&f.usb, &f.trusted).unwrap());
    }
}
