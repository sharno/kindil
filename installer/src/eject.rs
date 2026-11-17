//! Ejecting the Kindle's storage, so the Kindle gets it back with every
//! file on the disk and leaves the USB screen.
use anyhow::{bail, Context};
use std::path::Path;
use std::process::Command;

fn run(command: &mut Command) -> anyhow::Result<()> {
    let out = command
        .output()
        .with_context(|| format!("running {command:?}"))?;
    if !out.status.success() {
        bail!(
            "{} failed: {}",
            command.get_program().to_string_lossy(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// What to do by hand, where ejecting from here didn't work.
pub fn by_hand() -> &'static str {
    if cfg!(target_os = "macos") {
        "Eject the Kindle in Finder (the ⏏ beside it), then unplug it."
    } else if cfg!(windows) {
        "Use \"Safely Remove Hardware and Eject Media\" in the taskbar, pick the Kindle, \
         then unplug it."
    } else {
        "Eject the Kindle in your file manager (or run: udisksctl unmount -b <device>), \
         then unplug it."
    }
}

/// Ejects the volume at `volume`.
pub fn eject(volume: &Path) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        run(Command::new("diskutil").arg("eject").arg(volume))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mounts = std::fs::read_to_string("/proc/mounts").context("reading /proc/mounts")?;
        let device = device_of(&mounts, volume)
            .with_context(|| format!("{} isn't mounted", volume.display()))?;
        run(Command::new("udisksctl").args(["unmount", "-b", &device]))
            .or_else(|_| run(Command::new("umount").arg(volume)))?;
        // Tells the Kindle the computer is done; it returns to its own
        // screen. Not every setup allows it, and unmounted is enough.
        let _ = run(Command::new("udisksctl").args(["power-off", "-b", &device]));
        Ok(())
    }
    #[cfg(windows)]
    {
        // Explorer's own Eject, on the drive letter.
        let drive = volume.to_string_lossy();
        let drive = drive.trim_end_matches('\\');
        anyhow::ensure!(
            drive.len() == 2 && drive.ends_with(':'),
            "{drive} isn't a drive letter"
        );
        run(Command::new("powershell").args([
            "-NoProfile",
            "-Command",
            &format!(
                "(New-Object -ComObject Shell.Application).Namespace(17).ParseName('{drive}').InvokeVerb('Eject')"
            ),
        ]))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = volume;
        bail!("can't eject on this system")
    }
}

/// The device mounted at `volume`, from `/proc/mounts`.
#[allow(dead_code)]
fn device_of(mounts: &str, volume: &Path) -> Option<String> {
    mounts.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let (device, point) = (fields.next()?, fields.next()?);
        (Path::new(&point.replace("\\040", " ")) == volume).then(|| device.to_owned())
    })
}

/// Opens `url` in the browser.
pub fn open_url(url: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(windows)]
    let mut command = {
        let mut c = Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = Command::new("xdg-open");
    command
        .arg(url)
        .spawn()
        .with_context(|| format!("opening {url}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_device_behind_a_mount() {
        let mounts = "/dev/sda2 / ext4 rw 0 0\n/dev/sdc1 /media/me/Kindle vfat rw 0 0\n";
        assert_eq!(
            device_of(mounts, Path::new("/media/me/Kindle")).as_deref(),
            Some("/dev/sdc1")
        );
        assert_eq!(device_of(mounts, Path::new("/media/me/Other")), None);
    }
}
