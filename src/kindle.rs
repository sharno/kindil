//! The stock Kindle OS as a [`Host`]: kindil runs as an app on top of Amazon's
//! kernel and services, never as init, and changes nothing that survives a reboot
//! except the documented USBNetwork "SSH at boot" flag file.
use crate::battery::{Battery, Charge};
use crate::frontlight::Frontlight;
use crate::host::{Brightness, DeviceInfo, Host, Memory, Power, Ssh, Storage, Wifi};
use crate::wifi::{self, Credentials, Link, Network};
use anyhow::Context;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

const INTERFACE: &str = "wlan0";
/// USBNetwork toggles dropbear when configured with USE_WIFI_SSHD_ONLY.
const USBNET_TOGGLE: &str = "/mnt/us/usbnet/bin/usbnetwork";
const USBNET_AT_BOOT: &str = "/mnt/us/usbnet/auto";
const USER_STORAGE: &str = "/mnt/us";

pub struct KindleHost {
    frontlight: Result<Frontlight, String>,
}

impl KindleHost {
    /// Opens the frontlight device; other services are reached per call.
    pub fn open() -> Self {
        Self {
            frontlight: Frontlight::open().map_err(|e| e.to_string()),
        }
    }
}

/// Amazon's IPC: `lipc-get-prop com.lab126.wifid enable` and friends.
fn lipc_get(service: &str, property: &str) -> anyhow::Result<String> {
    let output = Command::new("lipc-get-prop")
        .args([service, property])
        .output()
        .context("lipc-get-prop")?;
    anyhow::ensure!(output.status.success(), "lipc-get-prop {property} failed");
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn lipc_set(service: &str, property: &str, value: &str) -> anyhow::Result<()> {
    let status = Command::new("lipc-set-prop")
        .args([service, property, value])
        .status()
        .context("lipc-set-prop")?;
    anyhow::ensure!(status.success(), "lipc-set-prop {property} failed");
    Ok(())
}

fn wifi_power() -> anyhow::Result<Power> {
    match lipc_get("com.lab126.wifid", "enable")?.as_str() {
        "1" => Ok(Power::On),
        "0" => Ok(Power::Off),
        other => anyhow::bail!("unexpected wifid enable value {other:?}"),
    }
}

/// powerd knows the PW1's fuel gauge, which has no `power_supply` class entry.
fn powerd_battery() -> anyhow::Result<Battery> {
    let percent: u8 = lipc_get("com.lab126.powerd", "battLevel")?.parse()?;
    let charge = match lipc_get("com.lab126.powerd", "isCharging")?.as_str() {
        "1" => Charge::Charging,
        _ => Charge::Discharging,
    };
    Ok(Battery::Known {
        percent: percent.min(100),
        charge,
    })
}

impl Host for KindleHost {
    fn brightness(&self) -> anyhow::Result<Brightness> {
        let light = self
            .frontlight
            .as_ref()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(Brightness::new(light.percent()?))
    }

    fn set_brightness(&self, level: Brightness) -> anyhow::Result<()> {
        let light = self
            .frontlight
            .as_ref()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(light.set_percent(level.percent())?)
    }

    fn wifi(&self) -> Wifi {
        match wifi_power() {
            Err(e) => Wifi::Unavailable(e.to_string()),
            Ok(Power::Off) => Wifi::Off,
            Ok(Power::On) => match wifi::open(INTERFACE).and_then(|wpa| Ok((wpa.link()?, wpa))) {
                Err(e) => Wifi::Unavailable(e.to_string()),
                Ok((Link::NotAssociated, _)) => Wifi::Searching,
                Ok((Link::Associated { ssid, ip }, wpa)) => Wifi::Connected {
                    ssid,
                    ip,
                    signal: wpa.signal(),
                },
            },
        }
    }

    fn set_wifi(&self, power: Power) -> anyhow::Result<()> {
        let value = match power {
            Power::On => "1",
            Power::Off => "0",
        };
        lipc_set("com.lab126.wifid", "enable", value)
    }

    fn scan(&self) -> anyhow::Result<Vec<Network>> {
        wifi::open(INTERFACE)?.scan()
    }

    fn join(&self, credentials: &Credentials) -> anyhow::Result<()> {
        wifi::open(INTERFACE)?.join(credentials)
    }

    fn ssh(&self) -> Ssh {
        if !Path::new(USBNET_TOGGLE).is_file() {
            return Ssh::NotInstalled;
        }
        let listening = ["/proc/net/tcp", "/proc/net/tcp6"]
            .iter()
            .filter_map(|path| std::fs::read_to_string(path).ok())
            .any(|table| listens_on(&table, 22));
        Ssh::Installed {
            server: if listening { Power::On } else { Power::Off },
            at_boot: if Path::new(USBNET_AT_BOOT).is_file() {
                Power::On
            } else {
                Power::Off
            },
        }
    }

    fn set_ssh(&self, power: Power) -> anyhow::Result<()> {
        match self.ssh() {
            Ssh::NotInstalled => anyhow::bail!("USBNetwork is not installed"),
            // The script only toggles, so call it only when the state must change.
            Ssh::Installed { server, .. } if server == power => Ok(()),
            Ssh::Installed { .. } => {
                let status = Command::new(USBNET_TOGGLE).status()?;
                anyhow::ensure!(status.success(), "usbnetwork exited with {status}");
                Ok(())
            }
        }
    }

    fn set_ssh_at_boot(&self, power: Power) -> anyhow::Result<()> {
        match power {
            Power::On => std::fs::write(USBNET_AT_BOOT, b"")?,
            Power::Off => match std::fs::remove_file(USBNET_AT_BOOT) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            },
        }
        Ok(())
    }

    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            battery: powerd_battery().unwrap_or_else(|_| crate::battery::read(Path::new("/sys"))),
            memory: std::fs::read_to_string("/proc/meminfo")
                .ok()
                .and_then(|text| parse_meminfo(&text)),
            storage: storage(Path::new(USER_STORAGE)),
            uptime: std::fs::read_to_string("/proc/uptime")
                .ok()
                .and_then(|text| parse_uptime(&text)),
        }
    }
}

/// True if `/proc/net/tcp{,6}` lists a socket in LISTEN state (0A) on `port`.
pub fn listens_on(table: &str, port: u16) -> bool {
    let wanted = format!("{port:04X}");
    table.lines().skip(1).any(|line| {
        let columns: Vec<&str> = line.split_whitespace().collect();
        let local_port = columns.get(1).and_then(|addr| addr.rsplit(':').next());
        local_port == Some(wanted.as_str()) && columns.get(3) == Some(&"0A")
    })
}

pub fn parse_meminfo(text: &str) -> Option<Memory> {
    let kib = |key: &str| -> Option<u64> {
        text.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    };
    Some(Memory {
        total_kib: kib("MemTotal")?,
        available_kib: kib("MemFree")? + kib("Buffers").unwrap_or(0) + kib("Cached").unwrap_or(0),
    })
}

pub fn parse_uptime(text: &str) -> Option<Duration> {
    let seconds: f64 = text.split_whitespace().next()?.parse().ok()?;
    Some(Duration::from_secs(seconds as u64))
}

fn storage(path: &Path) -> Option<Storage> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: valid NUL-terminated path and a properly sized out-parameter.
    if unsafe { libc::statvfs(path.as_ptr(), &mut stats) } != 0 {
        return None;
    }
    let block = stats.f_frsize as u64;
    Some(Storage {
        total_bytes: stats.f_blocks as u64 * block,
        free_bytes: stats.f_bavail as u64 * block,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_a_listening_ssh_server() {
        let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
           0: 00000000:0016 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 1234\n\
           1: 7801A8C0:A2F4 3201A8C0:1F9C 01 00000000:00000000 00:00000000 00000000     0        0 5678\n";
        assert!(listens_on(table, 22));
        assert!(!listens_on(table, 8092), "established, not listening");
        assert!(!listens_on("header\n", 22));
    }

    #[test]
    fn meminfo_without_memavailable() {
        let text = "MemTotal:         255000 kB\nMemFree:           50000 kB\nBuffers:           10000 kB\nCached:            90000 kB\n";
        assert_eq!(
            parse_meminfo(text),
            Some(Memory {
                total_kib: 255000,
                available_kib: 150000
            })
        );
        assert_eq!(
            parse_uptime("12345.67 9999.00\n"),
            Some(Duration::from_secs(12345))
        );
    }
}
