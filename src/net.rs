//! Who runs Wi-Fi: Amazon's wifid, or kindil itself.
//!
//! wifid loads the driver, starts wpa_supplicant with the saved networks
//! (kept encrypted, so only wifid can read them) and runs DHCP. After a
//! suspend it hears nothing of, it needs a full off and on, about 10 s.
//! Once wpa_supplicant holds the networks, kindil stops wifid and runs DHCP
//! itself: after a nap, `REASSOCIATE` has it back online in about a second.
//! Leaving kindil hands Wi-Fi back (bin/session.sh starts wifid again).
use crate::host::Power;
use crate::lipc;
use crate::wifi::{self, Link};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const INTERFACE: &str = "wlan0";
/// udhcpc's hook, on the system storage: /mnt/us goes away in USB drive
/// mode, and a renewal then would find no script.
const DHCP_SCRIPT: &str = "/var/local/kindil/dhcp.sh";
const DHCP_PID: &str = "/var/run/kindil-udhcpc.pid";
const SCRIPT: &str = include_str!("../kindle/extensions/kindil/bin/dhcp.sh");
/// wifid's switch settles asynchronously.
const WIFID_SETTLE: Duration = Duration::from_secs(2);
/// Joining usually takes under a second; a far access point, a few.
const JOIN_WAIT: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    Amazon,
    Kindil(Power),
}

pub struct Net {
    owner: Mutex<Owner>,
}

impl Default for Net {
    fn default() -> Self {
        Self {
            owner: Mutex::new(Owner::Amazon),
        }
    }
}

impl Net {
    fn owner(&self) -> Owner {
        *self.owner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_owner(&self, owner: Owner) {
        *self.owner.lock().unwrap_or_else(|e| e.into_inner()) = owner;
    }

    /// Takes Wi-Fi over from wifid once wpa_supplicant holds a network.
    /// Ok(false): not yet (wifid is still starting it); try again later.
    pub fn take_over(&self) -> anyhow::Result<bool> {
        if self.owner() != Owner::Amazon {
            return Ok(true);
        }
        let wpa = wifi::open(INTERFACE)?;
        if count_networks(&wpa.command("LIST_NETWORKS")?) == 0 {
            return Ok(false);
        }
        let power = wifid_power()?;
        std::fs::write(DHCP_SCRIPT, SCRIPT)?;
        set_executable(DHCP_SCRIPT)?;
        // Stopping wifid also stops its udhcpc and clears the address;
        // wpa_supplicant and the driver stay, with the networks.
        run(Command::new("/sbin/stop").arg("wifid"))?;
        self.set_owner(Owner::Kindil(Power::Off));
        if power == Power::On {
            self.set(Power::On)?;
        }
        Ok(true)
    }

    /// Gives Wi-Fi back to wifid, which starts it afresh.
    pub fn hand_back(&self) -> anyhow::Result<()> {
        if self.owner() == Owner::Amazon {
            return Ok(());
        }
        stop_dhcp();
        self.set_owner(Owner::Amazon);
        run(Command::new("/sbin/start").arg("wifid"))
    }

    pub fn power(&self) -> anyhow::Result<Power> {
        match self.owner() {
            Owner::Amazon => wifid_power(),
            Owner::Kindil(power) => Ok(power),
        }
    }

    pub fn set(&self, power: Power) -> anyhow::Result<()> {
        match self.owner() {
            Owner::Amazon => lipc::set("com.lab126.wifid", "enable", wifid_value(power)),
            Owner::Kindil(_) => {
                let wpa = wifi::open(INTERFACE)?;
                match power {
                    Power::On => {
                        wpa.command("RECONNECT")?;
                        start_dhcp()?;
                    }
                    Power::Off => {
                        wpa.command("DISCONNECT")?;
                        stop_dhcp();
                        run(Command::new("/sbin/ifconfig").args([INTERFACE, "0.0.0.0"]))?;
                    }
                }
                self.set_owner(Owner::Kindil(power));
                Ok(())
            }
        }
    }

    /// After a suspend: join again, then renew the address (usually kept).
    pub fn reconnect(&self) -> anyhow::Result<()> {
        match self.owner() {
            Owner::Amazon => match wifid_power()? {
                Power::Off => Ok(()),
                Power::On => {
                    lipc::set("com.lab126.wifid", "enable", "0")?;
                    // An immediate switch back can land before it went down.
                    std::thread::sleep(WIFID_SETTLE);
                    lipc::set("com.lab126.wifid", "enable", "1")
                }
            },
            Owner::Kindil(Power::Off) => Ok(()),
            Owner::Kindil(Power::On) => {
                let wpa = wifi::open(INTERFACE)?;
                wpa.command("REASSOCIATE")?;
                let since = Instant::now();
                while !matches!(wpa.link(), Ok(Link::Associated { .. })) {
                    anyhow::ensure!(since.elapsed() < JOIN_WAIT, "no access point joined");
                    std::thread::sleep(Duration::from_millis(100));
                }
                renew_dhcp()
            }
        }
    }
}

fn wifid_power() -> anyhow::Result<Power> {
    match lipc::get("com.lab126.wifid", "enable")?.as_str() {
        "1" => Ok(Power::On),
        "0" => Ok(Power::Off),
        other => anyhow::bail!("unexpected wifid enable value {other:?}"),
    }
}

fn wifid_value(power: Power) -> &'static str {
    match power {
        Power::On => "1",
        Power::Off => "0",
    }
}

/// `LIST_NETWORKS` replies with a header line, then one line per network.
pub fn count_networks(reply: &str) -> usize {
    reply
        .lines()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .count()
}

fn dhcp_pid() -> Option<i32> {
    std::fs::read_to_string(DHCP_PID).ok()?.trim().parse().ok()
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn start_dhcp() -> anyhow::Result<()> {
    if dhcp_pid().is_some_and(alive) {
        return renew_dhcp();
    }
    // -b: carry on in the background until it gets a lease.
    run(Command::new("/sbin/udhcpc").args([
        "-i",
        INTERFACE,
        "-s",
        DHCP_SCRIPT,
        "-p",
        DHCP_PID,
        "-b",
        "-t",
        "5",
        "-T",
        "2",
    ]))
}

fn renew_dhcp() -> anyhow::Result<()> {
    match dhcp_pid().filter(|&pid| alive(pid)) {
        // SAFETY: plain kill(2) on udhcpc's own pid.
        Some(pid) => {
            unsafe { libc::kill(pid, libc::SIGUSR1) };
            Ok(())
        }
        None => start_dhcp(),
    }
}

fn stop_dhcp() {
    if let Some(pid) = dhcp_pid().filter(|&pid| alive(pid)) {
        // SAFETY: plain kill(2) on udhcpc's own pid.
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
    let _ = std::fs::remove_file(DHCP_PID);
}

fn set_executable(path: &str) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

fn run(command: &mut Command) -> anyhow::Result<()> {
    let status = command.status()?;
    anyhow::ensure!(status.success(), "{command:?} exited with {status}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_the_networks_wpa_supplicant_holds() {
        assert_eq!(count_networks("network id / ssid / bssid / flags\n"), 0);
        assert_eq!(
            count_networks(
                "network id / ssid / bssid / flags\n0\tHome\tany\t[CURRENT]\n1\tCafe\tany\t\n"
            ),
            2
        );
    }

    #[test]
    fn starts_out_with_amazon() {
        assert_eq!(Net::default().owner(), Owner::Amazon);
    }
}
