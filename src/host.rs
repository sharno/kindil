//! Everything the UI asks of the device, behind one trait.
//!
//! [`crate::kindle::KindleHost`] drives the device: its own Wi-Fi stack, the
//! battery and front light drivers, USBNetwork's dropbear. [`Simulated`] returns fixtures
//! and never opens a device, so previews and tests cannot touch hardware.
use crate::battery::Battery;
use crate::location::Place;
use crate::update::{Check, Commit, Release};
use crate::wifi::{Credentials, Network, Ssid};
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(from = "u8", into = "u8")]
pub struct Brightness(u8);

impl From<u8> for Brightness {
    fn from(percent: u8) -> Self {
        Self::new(percent)
    }
}

impl From<Brightness> for u8 {
    fn from(level: Brightness) -> u8 {
        level.0
    }
}

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

/// A key kindil's SSH server trusts, or could: what someone checks before
/// trusting it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshKey {
    /// `ED25519`, `RSA`...
    pub kind: String,
    pub bits: u32,
    /// `SHA256:...`, as `ssh-keygen -lf` prints it.
    pub fingerprint: String,
    pub comment: String,
}

impl From<&crate::ssh_keys::Key> for SshKey {
    fn from(key: &crate::ssh_keys::Key) -> Self {
        Self {
            kind: key.kind.clone(),
            bits: key.bits,
            fingerprint: key.fingerprint.clone(),
            comment: key.comment.clone(),
        }
    }
}

/// kindil's own SSH server (src/ssh.rs) and its keys.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OwnSsh {
    pub port: u16,
    pub running: bool,
    pub trusted: Vec<SshKey>,
    /// Keys on the USB storage that are neither trusted nor ignored yet.
    pub new_on_usb: Vec<SshKey>,
}

/// Whether kindil starts at boot instead of Amazon's UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootStart {
    /// The boot job isn't in the system yet: a one-time install over SSH.
    NotInstalled,
    Installed(Boot),
}

/// How a boot reaches kindil, once the boot job is installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Boot {
    /// Amazon's UI, as without kindil.
    Off,
    /// Amazon's UI starts, then kindil takes over from it.
    AfterAmazon,
    /// Amazon's Java framework never starts: kindil comes up sooner and with
    /// more memory. Leaving kindil brings the framework back.
    Standalone,
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
    /// The fuel gauge's remaining charge, where the board reports it.
    pub charge: Option<crate::drain::MilliampHours>,
    pub memory: Option<Memory>,
    pub storage: Option<Storage>,
    pub uptime: Option<Duration>,
    /// The fuel gauge's current in mA, negative on battery: its average
    /// over the last few seconds, so right after a nap mostly the nap's.
    pub current: Option<i32>,
}

/// Delivers device events to the UI from any thread.
pub type Deliver = Box<dyn Fn(crate::system::Event) + Send + Sync>;

pub trait Host: Send + Sync {
    fn brightness(&self) -> anyhow::Result<Brightness>;
    fn set_brightness(&self, level: Brightness) -> anyhow::Result<()>;
    fn wifi(&self) -> Wifi;
    fn set_wifi(&self, power: Power) -> anyhow::Result<()>;
    /// After kindil suspends the device: joins again and renews the address.
    /// Blocks for a few seconds: call from a worker thread.
    fn reconnect_wifi(&self) -> anyhow::Result<()>;
    /// Before a suspend: leaves the network, so nothing joins it again on
    /// each resume; [`Host::reconnect_wifi`] brings it back. Quick.
    fn park_wifi(&self) {}
    /// The SSH sessions open on the device, to either server. Quick: reads
    /// the kernel's TCP tables.
    fn ssh_sessions(&self) -> Vec<crate::maintenance::Session> {
        Vec::new()
    }
    /// Blocks for a few seconds: call from a worker thread.
    fn scan(&self) -> anyhow::Result<Vec<Network>>;
    /// Joins for this session only; nothing is written to Amazon's Wi-Fi profiles.
    /// Joins the network and saves it, first among the saved ones.
    fn join(&self, credentials: &Credentials) -> anyhow::Result<()>;
    /// The saved networks' names, most recently joined first.
    fn saved_networks(&self) -> Vec<String>;
    fn forget_network(&self, ssid: &Ssid) -> anyhow::Result<()>;
    fn ssh(&self) -> Ssh;
    fn set_ssh(&self, power: Power) -> anyhow::Result<()>;
    fn set_ssh_at_boot(&self, power: Power) -> anyhow::Result<()>;
    /// kindil's own SSH server and its keys.
    fn own_ssh(&self) -> OwnSsh;
    /// Trusts the new USB keys with these fingerprints (those shown).
    fn trust_ssh_keys(&self, fingerprints: &[String]) -> anyhow::Result<()>;
    /// Stops offering the new USB keys with these fingerprints.
    fn ignore_ssh_keys(&self, fingerprints: &[String]) -> anyhow::Result<()>;
    fn boot_start(&self) -> BootStart;
    /// Fails while the boot job isn't installed.
    fn set_boot_start(&self, boot: Boot) -> anyhow::Result<()>;
    fn info(&self) -> DeviceInfo;
    /// Starts the threads that report the power button and USB drive mode.
    fn listen(&self, deliver: Deliver);
    /// The UI has let go of the user storage after [`crate::system::Event::StorageLeaving`]:
    /// kindil's own USB drive mode may unmount it.
    fn storage_released(&self) {}
    /// A line for the device's flight recorder (src/flight.rs): how each
    /// doze wake ended. Nothing elsewhere.
    fn note(&self, _line: &str) {}
    /// Suspends to RAM for this reason; returns once the app should wake
    /// (see [`crate::power::sleep`]). For an empty battery it returns at
    /// once, without suspending, if a charger is in; and at once, without
    /// suspending, if the app voided `ticket` meanwhile (src/suspend.rs).
    fn suspend(
        &self,
        why: crate::system::Why,
        ticket: &crate::suspend::Ticket,
    ) -> anyhow::Result<()>;
    /// Suspends to RAM with a timed wake at `until` (on the wall clock, taken
    /// right before arming); returns once awake, by the timer or the power
    /// button. An error means it didn't suspend; nor does it once the app
    /// voided `ticket`.
    fn nap(
        &self,
        until: chrono::DateTime<chrono::Utc>,
        ticket: &crate::suspend::Ticket,
    ) -> anyhow::Result<()>;
    /// Back from a suspend or a nap: catches the clock up with the RTC if
    /// the resume lost a second or more (src/clock.rs). Quick.
    fn catch_up_clock(&self) -> Option<crate::clock::Correction> {
        None
    }
    /// Online: sets the clock (and the RTC) from network time if it is off.
    /// Blocks for a few seconds: call from a worker thread.
    fn sync_clock(&self) -> anyhow::Result<Option<crate::clock::Correction>> {
        Ok(None)
    }
    fn restart(&self) -> anyhow::Result<()>;
    fn power_off(&self) -> anyhow::Result<()>;
    /// Asks GitHub for the latest release. Blocks: call from a worker thread.
    fn check_update(&self) -> anyhow::Result<Check>;
    /// Downloads, verifies and stages a release for the launcher's next start.
    fn stage_update(&self, release: &Release) -> anyhow::Result<()>;
    /// Keeps a version on trial once it has run long enough.
    fn commit_update(&self) -> anyhow::Result<Commit>;
    /// Places whose name starts with `name`. Blocks: call from a worker thread.
    fn search_places(&self, name: &str) -> anyhow::Result<Vec<Place>>;
    /// Where the internet address seems to be; only ever on the owner's tap.
    /// Blocks: call from a worker thread.
    fn locate_by_ip(&self) -> anyhow::Result<Place>;
}

/// Fixture device for previews and tests. Switches remember their state so UI
/// flows can be exercised end to end.
pub struct Simulated {
    state: Mutex<SimulatedState>,
}

struct SimulatedState {
    brightness: Brightness,
    suspends: u32,
    naps: u32,
    /// Naps and suspends that found Wi-Fi parked first.
    parked_naps: u32,
    parked: bool,
    battery: Battery,
    charge: Option<crate::drain::MilliampHours>,
    wifi: Power,
    saved: Vec<String>,
    ssh: Power,
    ssh_at_boot: Power,
    own_ssh: OwnSsh,
    boot_start: BootStart,
    /// Off: place lookups fail as without a network.
    online: bool,
    lookups: u32,
    ssh_sessions: Vec<crate::maintenance::Session>,
    /// Times the clock was synced, and the step the next sync reports.
    clock_syncs: u32,
    clock_off: Option<f64>,
    /// Naps fail as when the wake alarm can't be armed; how many did.
    refuse_naps: bool,
    refused_naps: u32,
    /// A charger is plugged in during the next suspend.
    plug_in_asleep: bool,
}

/// The Simulated device's place search: well-known public places.
fn simulated_places() -> Vec<Place> {
    let place = |name: &str, latitude, longitude, timezone| Place {
        name: Some(name.into()),
        latitude,
        longitude,
        timezone,
    };
    vec![
        place(
            "Amsterdam, North Holland, Netherlands",
            52.37403,
            4.88969,
            chrono_tz::Europe::Amsterdam,
        ),
        place(
            "Amsterdam, New York, United States",
            42.93869,
            -74.18819,
            chrono_tz::America::New_York,
        ),
        place(
            "Mecca, Saudi Arabia",
            21.42664,
            39.82563,
            chrono_tz::Asia::Riyadh,
        ),
        place(
            "New York, United States",
            40.71427,
            -74.00597,
            chrono_tz::America::New_York,
        ),
    ]
}

/// The Simulated device's keys: one trusted, one new on the USB storage.
fn simulated_ssh() -> OwnSsh {
    let key = |kind: &str, bits, fingerprint: &str, comment: &str| SshKey {
        kind: kind.into(),
        bits,
        fingerprint: fingerprint.into(),
        comment: comment.into(),
    };
    OwnSsh {
        port: 2222,
        running: true,
        trusted: vec![key(
            "ED25519",
            256,
            "SHA256:Tmj8P5/f8150RFsVfCD8guzdZW/Exkcf9MVSDzekGsg",
            "me@laptop",
        )],
        new_on_usb: vec![key(
            "RSA",
            3072,
            "SHA256:kASt+czojb6SDSr4yElIlLfOJ/VLtLLNJiTGIu8Qql4",
            "me@desktop",
        )],
    }
}

impl Default for Simulated {
    fn default() -> Self {
        Self {
            state: Mutex::new(SimulatedState {
                brightness: Brightness::new(40),
                suspends: 0,
                naps: 0,
                parked_naps: 0,
                parked: false,
                battery: Battery::Known {
                    percent: 82,
                    charge: crate::battery::Charge::Charging,
                },
                charge: None,
                wifi: Power::On,
                saved: Vec::new(),
                ssh: Power::On,
                ssh_at_boot: Power::On,
                own_ssh: simulated_ssh(),
                boot_start: BootStart::Installed(Boot::Off),
                online: true,
                lookups: 0,
                ssh_sessions: Vec::new(),
                clock_syncs: 0,
                clock_off: None,
                refuse_naps: false,
                refused_naps: 0,
                plug_in_asleep: false,
            }),
        }
    }
}

impl Simulated {
    /// How many times the clock was synced from network time.
    pub fn clock_syncs(&self) -> u32 {
        self.with(|s| s.clock_syncs)
    }

    /// The next sync finds the clock off by this many seconds (slow when
    /// positive).
    pub fn set_clock_off(&self, seconds: f64) {
        self.with(|s| s.clock_off = Some(seconds));
    }

    /// How many times the device was put to sleep.
    pub fn suspends(&self) -> u32 {
        self.with(|s| s.suspends)
    }

    pub fn set_battery(&self, battery: Battery) {
        self.with(|s| s.battery = battery);
    }

    /// From now on a charger is plugged in while the device sleeps: the
    /// suspend ends charging.
    pub fn plug_in_asleep(&self) {
        self.with(|s| s.plug_in_asleep = true);
    }

    pub fn set_charge(&self, charge: crate::drain::MilliampHours) {
        self.with(|s| s.charge = Some(charge));
    }

    /// From now on naps fail, as when the wake alarm can't be armed.
    pub fn set_refuse_naps(&self, refuse: bool) {
        self.with(|s| s.refuse_naps = refuse);
    }

    /// How many naps failed.
    pub fn refused_naps(&self) -> u32 {
        self.with(|s| s.refused_naps)
    }

    /// How many times the device napped while dozing.
    pub fn naps(&self) -> u32 {
        self.with(|s| s.naps)
    }

    /// How many naps and suspends found Wi-Fi parked.
    pub fn parked_naps(&self) -> u32 {
        self.with(|s| s.parked_naps)
    }

    /// Whether Wi-Fi is parked now.
    pub fn parked(&self) -> bool {
        self.with(|s| s.parked)
    }

    /// Offline, place lookups fail as without a network.
    pub fn set_online(&self, online: bool) {
        self.with(|s| s.online = online);
    }

    /// The SSH sessions open from now on.
    pub fn set_ssh_sessions(&self, sessions: Vec<crate::maintenance::Session>) {
        self.with(|s| s.ssh_sessions = sessions);
    }

    /// How many times a place service was asked.
    pub fn lookups(&self) -> u32 {
        self.with(|s| s.lookups)
    }

    fn lookup(&self) -> anyhow::Result<()> {
        self.with(|s| {
            s.lookups += 1;
            anyhow::ensure!(s.online, "can't reach the service: network unreachable");
            Ok(())
        })
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
    fn reconnect_wifi(&self) -> anyhow::Result<()> {
        self.with(|s| s.parked = false);
        Ok(())
    }
    fn park_wifi(&self) {
        self.with(|s| s.parked = true);
    }
    fn ssh_sessions(&self) -> Vec<crate::maintenance::Session> {
        self.with(|s| s.ssh_sessions.clone())
    }
    fn scan(&self) -> anyhow::Result<Vec<Network>> {
        use crate::wifi::Security::{Open, Protected};
        Ok(vec![
            Network::new("Home", Signal(-58), Protected),
            Network::new("Neighbour", Signal(-71), Protected),
            Network::new("Café", Signal(-83), Open),
        ])
    }
    fn join(&self, credentials: &Credentials) -> anyhow::Result<()> {
        let ssid = credentials.ssid().as_str().to_owned();
        self.with(|s| {
            s.wifi = Power::On;
            s.saved.retain(|saved| *saved != ssid);
            s.saved.insert(0, ssid);
        });
        Ok(())
    }
    fn saved_networks(&self) -> Vec<String> {
        self.with(|s| s.saved.clone())
    }
    fn forget_network(&self, ssid: &Ssid) -> anyhow::Result<()> {
        self.with(|s| s.saved.retain(|saved| saved != ssid.as_str()));
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
    fn own_ssh(&self) -> OwnSsh {
        self.with(|s| s.own_ssh.clone())
    }
    fn trust_ssh_keys(&self, fingerprints: &[String]) -> anyhow::Result<()> {
        self.with(|s| {
            let (chosen, rest) = std::mem::take(&mut s.own_ssh.new_on_usb)
                .into_iter()
                .partition(|key| fingerprints.contains(&key.fingerprint));
            s.own_ssh.new_on_usb = rest;
            s.own_ssh.trusted.extend::<Vec<SshKey>>(chosen);
        });
        Ok(())
    }
    fn ignore_ssh_keys(&self, fingerprints: &[String]) -> anyhow::Result<()> {
        self.with(|s| {
            s.own_ssh
                .new_on_usb
                .retain(|key| !fingerprints.contains(&key.fingerprint))
        });
        Ok(())
    }
    fn boot_start(&self) -> BootStart {
        self.with(|s| s.boot_start)
    }
    fn set_boot_start(&self, boot: Boot) -> anyhow::Result<()> {
        self.with(|s| match s.boot_start {
            BootStart::NotInstalled => anyhow::bail!("the boot job is not installed"),
            BootStart::Installed(_) => {
                s.boot_start = BootStart::Installed(boot);
                Ok(())
            }
        })
    }
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            battery: self.with(|s| s.battery),
            charge: self.with(|s| s.charge),
            memory: Some(Memory {
                total_kib: 255_000,
                available_kib: 171_000,
            }),
            storage: Some(Storage {
                total_bytes: 1_420_000_000,
                free_bytes: 1_046_000_000,
            }),
            uptime: Some(Duration::from_secs(3 * 3600 + 25 * 60)),
            current: None,
        }
    }
    /// Tests deliver events themselves.
    fn listen(&self, _: Deliver) {}
    /// Wakes at once, as if the power button were pressed right away, or
    /// by a charger plugged in while asleep ([`Simulated::plug_in_asleep`]).
    /// Like the device, it doesn't sleep for an empty battery while charging.
    fn suspend(
        &self,
        why: crate::system::Why,
        ticket: &crate::suspend::Ticket,
    ) -> anyhow::Result<()> {
        if !ticket.valid() {
            return Ok(());
        }
        self.with(|s| {
            let charging = matches!(
                s.battery,
                Battery::Known {
                    charge: crate::battery::Charge::Charging,
                    ..
                }
            );
            if why == crate::system::Why::BatteryEmpty && charging {
                return;
            }
            s.suspends += 1;
            s.parked_naps += u32::from(s.parked);
            if s.plug_in_asleep {
                if let Battery::Known { percent, .. } = s.battery {
                    s.battery = Battery::Known {
                        percent,
                        charge: crate::battery::Charge::Charging,
                    };
                }
            }
        });
        Ok(())
    }
    /// Wakes at once, as if the minute were up.
    fn nap(
        &self,
        _: chrono::DateTime<chrono::Utc>,
        ticket: &crate::suspend::Ticket,
    ) -> anyhow::Result<()> {
        if !ticket.valid() {
            return Ok(());
        }
        if self.with(|s| {
            s.refused_naps += u32::from(s.refuse_naps);
            s.refuse_naps
        }) {
            anyhow::bail!("the wake alarm reads back 0");
        }
        self.with(|s| {
            s.naps += 1;
            s.parked_naps += u32::from(s.parked);
        });
        Ok(())
    }
    /// Reports the step set with [`Simulated::set_clock_off`], once; the
    /// clock itself is the UI's to fake.
    fn sync_clock(&self) -> anyhow::Result<Option<crate::clock::Correction>> {
        self.with(|s| {
            s.clock_syncs += 1;
            Ok(s.clock_off.take().map(|seconds| crate::clock::Correction {
                seconds,
                source: crate::clock::Source::Ntp("time.example.org"),
            }))
        })
    }
    fn restart(&self) -> anyhow::Result<()> {
        anyhow::bail!("a simulated device cannot restart")
    }
    fn power_off(&self) -> anyhow::Result<()> {
        anyhow::bail!("a simulated device cannot power off")
    }
    fn check_update(&self) -> anyhow::Result<Check> {
        Ok(Check::UpToDate)
    }
    fn stage_update(&self, _: &Release) -> anyhow::Result<()> {
        anyhow::bail!("a simulated device cannot update")
    }
    fn commit_update(&self) -> anyhow::Result<Commit> {
        Ok(Commit::Nothing)
    }
    fn search_places(&self, name: &str) -> anyhow::Result<Vec<Place>> {
        self.lookup()?;
        let name = name.trim().to_lowercase();
        Ok(simulated_places()
            .into_iter()
            .filter(|p| p.title().to_lowercase().starts_with(&name))
            .collect())
    }
    fn locate_by_ip(&self) -> anyhow::Result<Place> {
        self.lookup()?;
        Ok(Place {
            name: Some("Amsterdam, North Holland, NL".into()),
            latitude: 52.374,
            longitude: 4.8897,
            timezone: chrono_tz::Europe::Amsterdam,
        })
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
