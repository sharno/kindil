//! Writing a release onto the Kindle's USB storage.
//!
//! Each file is written beside its place as `<name>.part`, flushed to the
//! disk, moved over the old one in a single rename and read back to check
//! its SHA-256: a cable pulled mid-copy leaves the previous file or a stray
//! `.part`, never half a script for KUAL to run. The launcher files go
//! first and the binary last, as `cargo xtask deploy` does. The settings in
//! `kindil/` are only ever created, never overwritten.
use crate::release::{File, Release};
use anyhow::{ensure, Context};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};

/// What happened to a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Written and checked; `n` of `of`.
    Wrote { path: PathBuf, n: usize, of: usize },
    /// A settings file already there, kept as it is.
    Kept { path: PathBuf },
    /// A stale file of the previous install, removed.
    Removed { path: PathBuf },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub written: Vec<PathBuf>,
    pub kept: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
}

/// Installs `release` on the Kindle storage at `volume`.
pub fn install(
    volume: &Path,
    release: &Release,
    mut progress: impl FnMut(&Event),
) -> anyhow::Result<Report> {
    ensure!(
        volume.is_dir(),
        "{} isn't there any more: was the Kindle unplugged?",
        volume.display()
    );
    let mut report = Report::default();
    // An update kindil staged but never started would run in place of the
    // new binary at the next start.
    let stale = "extensions/kindil/bin/kindil.next";
    if volume.join(stale).is_file() {
        std::fs::remove_file(volume.join(stale)).with_context(|| format!("removing {stale}"))?;
        progress(&Event::Removed { path: stale.into() });
        report.removed.push(stale.into());
    }
    let new_examples: Vec<&File> = release
        .examples
        .iter()
        .filter(|f| {
            let there = volume.join(&f.path).exists();
            if there {
                let event = Event::Kept {
                    path: f.path.clone(),
                };
                progress(&event);
                report.kept.push(f.path.clone());
            }
            !there
        })
        .collect();
    // Settings first: a Kindle unplugged after them still has a working
    // install, if an older one.
    let files: Vec<&File> = new_examples
        .into_iter()
        .chain(release.extension.iter())
        .collect();
    let of = files.len();
    for (i, file) in files.into_iter().enumerate() {
        write(volume, file)?;
        progress(&Event::Wrote {
            path: file.path.clone(),
            n: i + 1,
            of,
        });
        report.written.push(file.path.clone());
    }
    Ok(report)
}

/// Writes one file in place, atomically, and checks what landed.
pub fn write(volume: &Path, file: &File) -> anyhow::Result<()> {
    let path = volume.join(&file.path);
    let shown = file.path.display();
    let folder = path.parent().context("a file in a folder")?;
    std::fs::create_dir_all(folder).with_context(|| format!("creating {}", folder.display()))?;
    let mut part = path.clone().into_os_string();
    part.push(".part");
    let part = PathBuf::from(part);
    let written = (|| {
        let mut out = std::fs::File::create(&part)?;
        out.write_all(&file.bytes)?;
        out.sync_all()
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_file(&part);
        return Err(error).with_context(|| format!("writing {shown} (is the Kindle full?)"));
    }
    std::fs::rename(&part, &path).with_context(|| format!("moving {shown} into place"))?;
    let landed = std::fs::read(&path).with_context(|| format!("reading back {shown}"))?;
    ensure!(
        landed.len() == file.bytes.len() && Sha256::digest(&landed) == Sha256::digest(&file.bytes),
        "{shown} didn't copy intact ({} of {} bytes, or different bytes): \
         try again, or another cable",
        landed.len(),
        file.bytes.len()
    );
    Ok(())
}
