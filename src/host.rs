//! Everything the UI asks of the device, behind one trait.
//!
//! [`crate::kindle::KindleHost`] drives Amazon's own services (wifid via lipc,
//! USBNetwork's dropbear, the frontlight driver). [`Simulated`] returns fixtures
//! and never opens a device, so previews and tests cannot touch hardware.
use crate::battery::Battery;
use crate::wifi::{Credentials, Network};
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;
use std::sync::Mutex;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Power {
    On,
    #[default]
    Off,
}

impl Power {
    pub fn toggled(self) -> Self {
        match self {
            Power::On => Power::Off,
            Power::Off => Power::On,
        }
    }
}

/// Frontlight level in percent, 0..=100.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Brightness(u8);

impl Brightness {
    pub fn new(percent: u8) -> Self {
        Self(percent.min(100))
    }
    pub fn percent(self) -> u8 {
        self.0
    }
}

/// Received signal strength in dBm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Signal(pub i32);

impl Signal {
    /// 0..=4 bars, the usual phone mapping.
    pub fn bars(self) -> u8 {
        match self.0 {
            i32::MIN..=-90 => 0,
            -89..=-78 => 1,
            -77..=-67 => 2,
            -66..=-56 => 3,
            _ => 4,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wifi {
    Off,
    /// Radio on, not associated with a network.
    Searching,
    Connected {
        ssid: String,
        ip: Option<Ipv4Addr>,
        signal: Option<Signal>,
    },
    Unavailable(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ssh {
    /// USBNetwork is not installed, so there is no SSH server to control.
    NotInstalled,
    Installed {
        server: Power,
        at_boot: Power,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Memory {
    pub total_kib: u64,
    /// Free plus reclaimable page cache. Linux 2.6.31 has no MemAvailable.
    pub available_kib: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Storage {
    pub total_bytes: u64,
    pub free_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub battery: Battery,
    pub memory: Option<Memory>,
    pub storage: Option<Storage>,
    pub uptime: Option<Duration>,
}

/// Delivers device events to the UI from any thread.
pub type Deliver = Box<dyn Fn(crate::system::Event) + Send + Sync>;

pub trait Host: Send + Sync {
    fn brightness(&self) -> anyhow::Result<Brightness>;
    fn set_brightness(&self, level: Brightness) -> anyhow::Result<()>;
    fn wifi(&self) -> Wifi;
    fn set_wifi(&self, power: Power) -> anyhow::Result<()>;
    /// Blocks for a few seconds: call from a worker thread.
    fn scan(&self) -> anyhow::Result<Vec<Network>>;
    /// Joins for this session only; nothing is written to Amazon's Wi-Fi profiles.
    fn join(&self, credentials: &Credentials) -> anyhow::Result<()>;
    fn ssh(&self) -> Ssh;
    fn set_ssh(&self, power: Power) -> anyhow::Result<()>;
    fn set_ssh_at_boot(&self, power: Power) -> anyhow::Result<()>;
    fn info(&self) -> DeviceInfo;
    /// Starts the threads that report the power button and USB drive mode.
    fn listen(&self, deliver: Deliver);
    /// Suspends to RAM; returns once the device is awake again.
    fn suspend(&self) -> anyhow::Result<()>;
    fn restart(&self) -> anyhow::Result<()>;
    fn power_off(&self) -> anyhow::Result<()>;
}

/// Fixture device for previews and tests. Switches remember their state so UI
/// flows can be exercised end to end.
pub struct Simulated {
    state: Mutex<SimulatedState>,
}

struct SimulatedState {
    brightness: Brightness,
    suspends: u32,
    wifi: Power,
    ssh: Power,
    ssh_at_boot: Power,
}

impl Default for Simulated {
    fn default() -> Self {
        Self {
            state: Mutex::new(SimulatedState {
                brightness: Brightness::new(40),
                suspends: 0,
                wifi: Power::On,
                ssh: Power::On,
                ssh_at_boot: Power::On,
            }),
        }
    }
}

impl Simulated {
    /// How many times the device was put to sleep.
    pub fn suspends(&self) -> u32 {
        self.with(|s| s.suspends)
    }

    fn with<T>(&self, f: impl FnOnce(&mut SimulatedState) -> T) -> T {
        f(&mut self.state.lock().expect("simulated host poisoned"))
    }
}

impl Host for Simulated {
    fn brightness(&self) -> anyhow::Result<Brightness> {
        Ok(self.with(|s| s.brightness))
    }
    fn set_brightness(&self, level: Brightness) -> anyhow::Result<()> {
        self.with(|s| s.brightness = level);
        Ok(())
    }
    fn wifi(&self) -> Wifi {
        match self.with(|s| s.wifi) {
            Power::Off => Wifi::Off,
            Power::On => Wifi::Connected {
                ssid: "Home".into(),
                ip: Some(Ipv4Addr::new(192, 168, 1, 120)),
                signal: Some(Signal(-58)),
            },
        }
    }
    fn set_wifi(&self, power: Power) -> anyhow::Result<()> {
        self.with(|s| s.wifi = power);
        Ok(())
    }
    fn scan(&self) -> anyhow::Result<Vec<Network>> {
        use crate::wifi::Security::{Open, Protected};
        Ok(vec![
            Network::new("Home", Signal(-58), Protected),
            Network::new("Neighbour", Signal(-71), Protected),
            Network::new("Café", Signal(-83), Open),
        ])
    }
    fn join(&self, _: &Credentials) -> anyhow::Result<()> {
        self.with(|s| s.wifi = Power::On);
        Ok(())
    }
    fn ssh(&self) -> Ssh {
        self.with(|s| Ssh::Installed {
            server: s.ssh,
            at_boot: s.ssh_at_boot,
        })
    }
    fn set_ssh(&self, power: Power) -> anyhow::Result<()> {
        self.with(|s| s.ssh = power);
        Ok(())
    }
    fn set_ssh_at_boot(&self, power: Power) -> anyhow::Result<()> {
        self.with(|s| s.ssh_at_boot = power);
        Ok(())
    }
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            battery: Battery::Known {
                percent: 82,
                charge: crate::battery::Charge::Charging,
            },
            memory: Some(Memory {
                total_kib: 255_000,
                available_kib: 171_000,
            }),
            storage: Some(Storage {
                total_bytes: 1_420_000_000,
                free_bytes: 1_046_000_000,
            }),
            uptime: Some(Duration::from_secs(3 * 3600 + 25 * 60)),
        }
    }
    /// Tests deliver events themselves.
    fn listen(&self, _: Deliver) {}
    /// Wakes at once, as if the power button were pressed right away.
    fn suspend(&self) -> anyhow::Result<()> {
        self.with(|s| s.suspends += 1);
        Ok(())
    }
    fn restart(&self) -> anyhow::Result<()> {
        anyhow::bail!("a simulated device cannot restart")
    }
    fn power_off(&self) -> anyhow::Result<()> {
        anyhow::bail!("a simulated device cannot power off")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn brightness_stays_in_range() {
        assert_eq!(Brightness::new(250).percent(), 100);
    }
    #[test]
    fn signal_bars() {
        assert_eq!(Signal(-95).bars(), 0);
        assert_eq!(Signal(-70).bars(), 2);
        assert_eq!(Signal(-40).bars(), 4);
    }
}
