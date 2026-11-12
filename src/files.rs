//! Basic user-file management. No recursive deletion, no system-file browser.
//! Trash keeps the original bytes recoverable; existing destinations are never
//! overwritten (hard_link provides an atomic no-clobber operation on old Linux).
use std::{
    io,
    path::{Path, PathBuf},
};
pub struct Files {
    root: PathBuf,
}
impl Files {
    pub fn new(root: &Path) -> io::Result<Self> {
        Ok(Self {
            root: root.canonicalize()?,
        })
    }
    fn file(&self, name: &str) -> io::Result<PathBuf> {
        if name.is_empty()
            || name.starts_with('.')
            || Path::new(name).components().count() != 1
            || name.contains(['/', '\\', '\0'])
        {
            return Err(io::Error::other("use a plain non-hidden filename"));
        }
        Ok(self.root.join(name))
    }
    pub fn rename(&self, old: &str, new: &str) -> io::Result<()> {
        let old = self.file(old)?;
        let new = self.file(new)?;
        move_file(&old, &new)
    }
    pub fn trash(&self, name: &str) -> io::Result<PathBuf> {
        let old = self.file(name)?;
        let trash = self.root.join(".kindil-trash");
        match std::fs::create_dir(&trash) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        if std::fs::symlink_metadata(&trash)?.file_type().is_symlink() || !trash.is_dir() {
            return Err(io::Error::other("unsafe trash directory"));
        }
        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let destination = trash.join(format!("{id}-{name}"));
        move_file(&old, &destination)?;
        Ok(destination)
    }
    pub fn restore(&self, trashed: &Path, name: &str) -> io::Result<()> {
        let trash = self.root.join(".kindil-trash").canonicalize()?;
        if !trash.starts_with(&self.root) || trashed.parent() != Some(trash.as_path()) {
            return Err(io::Error::other("invalid trash location"));
        }
        move_file(trashed, &self.file(name)?)
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
    #[test]
    fn rename_trash_restore_and_no_clobber() {
        let dir = tempfile::tempdir().unwrap();
        let f = Files::new(dir.path()).unwrap();
        std::fs::write(dir.path().join("a.txt"), "book").unwrap();
        std::fs::write(dir.path().join("b.txt"), "other").unwrap();
        assert!(f.rename("a.txt", "b.txt").is_err());
        assert!(f.rename("a.txt", "../etc").is_err());
        f.rename("a.txt", "c.txt").unwrap();
        let trash = f.trash("c.txt").unwrap();
        assert!(!dir.path().join("c.txt").exists());
        f.restore(&trash, "c.txt").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("c.txt")).unwrap(),
            "book"
        );
    }
    #[test]
    fn symlinks_are_not_managed() {
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/etc/passwd", dir.path().join("link.txt")).unwrap();
        assert!(Files::new(dir.path()).unwrap().trash("link.txt").is_err());
    }
}
