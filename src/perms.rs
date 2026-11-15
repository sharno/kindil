//! kindil's files on the system storage, kept to root.
//!
//! Each time Amazon's UI starts (at boot, and whenever kindil hands the
//! screen back), /etc/upstart/framework.conf runs `chgrp -R javausers
//! /var/local; chmod -R g=u /var/local` and setgids every directory there:
//! a 0755 binary root runs comes back 0775, a 0600 secret 0660, kindil's
//! directory 02775, all the javausers group's to change. kindil narrows its
//! own again at every start ([`narrow_own`]); the launcher, which shares no
//! code, does the same for itself and the binary it runs
//! (launcher/src/session.rs). While Amazon's UI runs, the group can still
//! change them: README, "Files on the device".
use std::path::Path;

/// kindil's directory on the system storage.
pub const RUN: &str = "/var/local/kindil";

/// kindil's own entries under [`RUN`] and the most each may allow: binaries
/// root runs and the directories 0755 (the ssh directory 0700), secrets
/// 0600, the rest 0644. A narrower mode stays.
pub const OWN: &[(&str, u32)] = &[
    ("", 0o755),
    // Binaries root runs; kindil-dhcp is a link to kindil.
    ("kindil", 0o755),
    ("launcher", 0o755),
    ("kindil-sshd", 0o755),
    ("wpa_supplicant", 0o755),
    // Secrets: Wi-Fi passwords, SSH keys, who logged in from where.
    ("networks.toml", 0o600),
    ("ssh", 0o700),
    ("ssh/authorized_keys", 0o600),
    ("ssh/ignored_keys", 0o600),
    ("ssh/host_ed25519", 0o600),
    ("sshd.log", 0o600),
    ("sshd.log.1", 0o600),
    // What kindil, the launcher and root's daemons read and act on.
    ("wpa_supplicant.conf", 0o644),
    ("wifi", 0o644),
    ("wifi-off", 0o644),
    ("usb-drive", 0o644),
    ("usb-exported", 0o644),
    ("boot-tries", 0o644),
    ("booted", 0o644),
    ("trying", 0o644),
    ("kindil.from", 0o644),
    ("kindil.log", 0o644),
];

/// Narrows `path` to at most `most` (setuid, setgid and sticky always go),
/// if it is a regular file or a directory of kindil's user, never through a
/// symlink, and changed through the open file. Its mode before, if it was
/// wider; nothing if there is no such file.
pub fn narrow(path: &Path, most: u32) -> std::io::Result<Option<u32>> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    // O_NONBLOCK: a FIFO in its place doesn't hang the open.
    let file = match std::fs::OpenOptions::new()
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
    let ours = meta.uid() == unsafe { libc::geteuid() };
    if !(meta.is_file() || meta.is_dir()) || !ours {
        return Err(std::io::Error::other("not a file of kindil's"));
    }
    let was = meta.mode() & 0o7777;
    let narrowed = was & most & 0o777;
    if narrowed == was {
        return Ok(None);
    }
    file.set_permissions(std::fs::Permissions::from_mode(narrowed))?;
    Ok(Some(was))
}

/// Narrows kindil's own entries under `run` ([`OWN`]); a line for each
/// one narrowed or that couldn't be.
pub fn narrow_own(run: &Path) -> Vec<String> {
    let mut lines = Vec::new();
    for (name, most) in OWN {
        let path = run.join(name);
        match narrow(&path, *most) {
            Ok(Some(was)) => lines.push(format!(
                "{} was {was:o}: narrowed to {:o}",
                path.display(),
                was & most & 0o777
            )),
            Ok(None) => {}
            Err(error) => lines.push(format!("{}: {error}", path.display())),
        }
    }
    lines
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(path: &Path) -> u32 {
        std::fs::symlink_metadata(path)
            .unwrap()
            .permissions()
            .mode()
            & 0o7777
    }

    fn set(path: &Path, mode: u32) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// What the Kindle's /etc/upstart/framework.conf does to all of
    /// /var/local each time Amazon's UI starts: `chmod -R g=u`, then
    /// `chmod g+s` on every directory (its `chgrp` aside).
    pub fn framework_starts(dir: &Path) {
        let widen = |path: &Path| {
            let was = mode(path);
            set(path, (was & 0o7707) | ((was & 0o700) >> 3));
        };
        widen(dir);
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                framework_starts(&path);
            } else if meta.is_file() {
                widen(&path);
            }
        }
        // A user outside the directory's group may not set it (macOS); g=u
        // is the harm anyway.
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode(dir) | 0o2000));
    }

    #[test]
    fn narrows_what_the_kindle_ui_widened_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let run = dir.path().join("kindil");
        std::fs::create_dir_all(run.join("ssh")).unwrap();
        set(&run, 0o755);
        set(&run.join("ssh"), 0o700);
        // Each with its mode before Amazon's UI started and after kindil.
        let files = [
            ("kindil", 0o755, 0o755),
            ("launcher", 0o755, 0o755),
            ("networks.toml", 0o600, 0o600),
            ("ssh/authorized_keys", 0o600, 0o600),
            ("wpa_supplicant.conf", 0o644, 0o644),
            // As the launcher's umask leaves it: group write goes.
            ("kindil.log", 0o664, 0o644),
            // At most 0644: the group may read what it already could.
            ("usb-drive", 0o600, 0o640),
            // Not kindil's own: left as Amazon's UI made it.
            ("someones.sh", 0o644, 0o664),
        ];
        for (name, before, _) in files {
            std::fs::write(run.join(name), name).unwrap();
            set(&run.join(name), before);
        }
        std::os::unix::fs::symlink(run.join("kindil"), run.join("kindil-dhcp")).unwrap();
        framework_starts(&run);
        assert_eq!(mode(&run.join("kindil")), 0o775);
        assert_eq!(mode(&run.join("networks.toml")), 0o660);
        assert_eq!(mode(&run.join("ssh")) & 0o777, 0o770);

        let lines = narrow_own(&run);
        assert!(
            lines.iter().any(|l| l.contains("networks.toml was 660")),
            "{lines:?}"
        );
        assert_eq!(mode(&run), 0o755);
        assert_eq!(mode(&run.join("ssh")), 0o700);
        for (name, _, after) in files {
            assert_eq!(mode(&run.join(name)), after, "{name}");
        }
        // Once narrowed, nothing more to say.
        assert!(narrow_own(&run).is_empty());
    }

    #[test]
    fn never_through_a_symlink_or_on_another_kind_of_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(narrow(&dir.path().join("none"), 0o600).unwrap(), None);
        let target = dir.path().join("elsewhere");
        std::fs::write(&target, "").unwrap();
        set(&target, 0o666);
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(narrow(&link, 0o600).is_err());
        assert_eq!(mode(&target), 0o666);
        assert_eq!(narrow(&target, 0o600).unwrap(), Some(0o666));
        assert_eq!(mode(&target), 0o600);
    }
}
