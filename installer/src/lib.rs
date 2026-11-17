//! Installs kindil on a jailbroken first-generation Kindle Paperwhite
//! plugged into this computer over USB, from macOS, Linux or Windows.
//!
//! The steps, which the `kindil-install` CLI and the `kindil-installer-gui`
//! window both walk through:
//! 1. [`detect`]: find the Kindle's USB storage and tell its model
//!    ([`model`], [`usb`]) and firmware.
//! 2. [`check`]: a PW1, jailbroken, with KUAL; or why not and where to read on.
//! 3. [`release`] and [`install`]: download the latest signed release and
//!    write it onto the storage, keeping any settings already there.
//! 4. [`eject`]: hand the storage back to the Kindle.
pub mod check;
pub mod detect;
pub mod eject;
pub mod install;
pub mod model;
pub mod release;
pub mod usb;

use std::path::Path;

/// What to do once kindil is on the Kindle.
pub const NEXT_STEPS: &str = "Eject the Kindle and unplug it. On the Kindle, open KUAL and \
tap kindil → Start kindil. Set your location in kindil's Settings → Location (or edit \
kindil/miqat.toml on the Kindle's storage). To have kindil start whenever the Kindle boots, \
choose a mode in kindil's Settings → Power → Start at boot.";

/// The Kindle to work on: the one at `volume`, or the only one plugged in.
pub fn choose(volume: Option<&Path>) -> anyhow::Result<detect::Kindle> {
    if let Some(volume) = volume {
        anyhow::ensure!(
            detect::looks_like_kindle(volume),
            "{} doesn't look like a Kindle's storage (no documents and system folders)",
            volume.display()
        );
        let serials = usb::kindle_serials();
        let serial = match serials.as_slice() {
            [one] => Some(one.as_str()),
            _ => None,
        };
        return Ok(detect::inspect(volume, serial));
    }
    let mut found = detect::scan();
    match found.len() {
        0 => anyhow::bail!(
            "No Kindle found. Plug it in with a data cable and unlock its screen: it should \
             show \"USB drive mode\" and appear as a drive named Kindle."
        ),
        1 => Ok(found.remove(0)),
        _ => anyhow::bail!(
            "More than one Kindle is plugged in ({}): choose one with --volume.",
            found
                .iter()
                .map(|k| k.volume.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}
