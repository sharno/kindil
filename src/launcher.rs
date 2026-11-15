//! kindil carries its launcher (the launcher/ crate: the session around
//! kindil and the start-at-boot guard) and writes it out to the system
//! storage, where KUAL's start.sh and the boot job's boot.sh run it.
//!
//! Only a kindil that has run for two minutes writes it (at the same point an
//! update on trial is kept), so a release that doesn't stay up never
//! replaces the Kindle's way back to Amazon's UI. The one exception is a
//! Kindle with no launcher that runs: the shims then have the installed
//! kindil write it (`kindil launcher`).
use std::path::Path;

pub const PATH: &str = "/var/local/kindil/launcher";
const BINARY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/launcher"));

/// Writes the launcher to `path` unless it is already there; whether it
/// wrote it. Through a file beside it and a rename: a running session keeps
/// its own copy.
pub fn install(path: &Path) -> anyhow::Result<bool> {
    anyhow::ensure!(
        !BINARY.is_empty(),
        "this kindil was built without its launcher"
    );
    if std::fs::read(path).ok().as_deref() == Some(BINARY) {
        return Ok(false);
    }
    let part = path.with_extension("part");
    std::fs::write(&part, BINARY)?;
    std::fs::set_permissions(&part, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
    std::fs::rename(&part, path)?;
    Ok(true)
}
