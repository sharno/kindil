//! Basic user-file management. No recursive deletion, no system-file browser.
//! Trash keeps the original bytes recoverable; existing destinations are never
//! overwritten (hard_link provides an atomic no-clobber operation on old Linux).
use std::{
    io,
    path::{Path, PathBuf},
};

/// A plain, non-hidden name inside the books directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileName(String);

impl TryFrom<&str> for FileName {
    type Error = io::Error;
    fn try_from(name: &str) -> io::Result<Self> {
        let plain = !name.is_empty()
            && !name.starts_with('.')
            && !name.contains(['/', '\\', '\0'])
            && Path::new(name).components().count() == 1;
        plain
            .then(|| Self(name.to_owned()))
            .ok_or_else(|| io::Error::other("use a plain, non-hidden file name"))
    }
}

/// Proof of a move to Trash; the only way to restore.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trashed {
    path: PathBuf,
    name: FileName,
}

impl Trashed {
    pub fn name(&self) -> &str {
        &self.name.0
    }
}

pub struct Files {
    root: PathBuf,
}

impl Files {
    pub fn new(root: &Path) -> io::Result<Self> {
        Ok(Self {
            root: root.canonicalize()?,
        })
    }

    fn path(&self, name: &FileName) -> PathBuf {
        self.root.join(&name.0)
    }

    pub fn rename(&self, old: &FileName, new: &FileName) -> io::Result<()> {
        move_file(&self.path(old), &self.path(new))
    }

    pub fn trash(&self, name: &FileName) -> io::Result<Trashed> {
        let trash = self.root.join(".kindil-trash");
        match std::fs::create_dir(&trash) {
            Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
            _ => {}
        }
        if !std::fs::symlink_metadata(&trash)?.file_type().is_dir() {
            return Err(io::Error::other("unsafe trash directory"));
        }
        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let path = trash.join(format!("{id}-{}", name.0));
        move_file(&self.path(name), &path)?;
        Ok(Trashed {
            path,
            name: name.clone(),
        })
    }

    pub fn restore(&self, trashed: &Trashed) -> io::Result<()> {
        move_file(&trashed.path, &self.path(&trashed.name))
    }
}

fn move_file(old: &Path, new: &Path) -> io::Result<()> {
    if !std::fs::symlink_metadata(old)?.file_type().is_file() {
        return Err(io::Error::other("only regular user files can be managed"));
    }
    if old == new {
        return Ok(());
    }
    std::fs::hard_link(old, new)?;
    // If unlink fails, both names remain: no data is lost.
    std::fs::remove_file(old)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn name(s: &str) -> FileName {
        FileName::try_from(s).unwrap()
    }
    #[test]
    fn rename_trash_restore_and_no_clobber() {
        let dir = tempfile::tempdir().unwrap();
        let f = Files::new(dir.path()).unwrap();
        std::fs::write(dir.path().join("a.txt"), "book").unwrap();
        std::fs::write(dir.path().join("b.txt"), "other").unwrap();
        assert!(f.rename(&name("a.txt"), &name("b.txt")).is_err());
        assert!(FileName::try_from("../etc").is_err());
        assert!(FileName::try_from(".hidden").is_err());
        f.rename(&name("a.txt"), &name("c.txt")).unwrap();
        let trashed = f.trash(&name("c.txt")).unwrap();
        assert!(!dir.path().join("c.txt").exists());
        f.restore(&trashed).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("c.txt")).unwrap(),
            "book"
        );
    }
    #[test]
    fn symlinks_are_not_managed() {
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/etc/passwd", dir.path().join("link.txt")).unwrap();
        let files = Files::new(dir.path()).unwrap();
        assert!(files.trash(&name("link.txt")).is_err());
    }
}
