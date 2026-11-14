//! The stock Kindle OS as a [`Host`]: kindil runs as an app on top of Amazon's
//! kernel and services, never as init, and changes nothing that survives a reboot
//! except the documented USBNetwork "SSH at boot" flag file.
use crate::battery::{Battery, Charge};
use crate::frontlight::Frontlight;
use crate::host::{
    Boot, BootStart, Brightness, Deliver, DeviceInfo, Host, Memory, Power, Ssh, Storage, Wifi,
};
use crate::net::Net;
use crate::system::{self, Event};
use crate::uevent::Uevent;
use crate::update::{Check, Commit, Github, Release, Slots, Version};
use crate::wifi::{self, Credentials, Link, Network};
use std::io::BufRead;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

const INTERFACE: &str = "wlan0";
/// How long wifid may take at boot to hand wpa_supplicant the networks.
const WIFI_TAKEOVER: Duration = Duration::from_secs(180);
/// udhcpc's lease for [`INTERFACE`] (`/usr/share/udhcpc/default.script`).
const DHCP_LEASE: &str = "/tmp/wlan0";
/// The name servers udhcpc got, and where the resolver reads them
/// (`/etc/resolv.conf` links there).
const DHCP_RESOLV: &str = "/tmp/resolv.conf";
const RESOLV: &str = "/var/run/resolv.conf";
const ROUTES: &str = "/proc/net/route";
/// The PW1's fuel gauge; it stalls while suspended and catches up on resume.
const GAUGE_CHARGE: &str = "/sys/devices/system/yoshi_battery/yoshi_battery0/battery_mAH";
/// The gauge's state of charge, as `71%`; powerd's `battLevel` is this.
const GAUGE_CAPACITY: &str = "/sys/devices/system/yoshi_battery/yoshi_battery0/battery_capacity";
/// `1` while the charger charges; powerd's `isCharging` is this.
const CHARGER: &str = "/sys/devices/platform/aplite_charger.0/charging";
/// USBNetwork toggles dropbear when configured with USE_WIFI_SSHD_ONLY.
const USBNET_TOGGLE: &str = "/mnt/us/usbnet/bin/usbnetwork";
const USBNET_AT_BOOT: &str = "/mnt/us/usbnet/auto";
const USER_STORAGE: &str = "/mnt/us";
/// The boot job, installed by `boot/install.sh` in the extension.
const BOOT_JOB: &str = "/etc/upstart/kindil.conf";
/// Present: the boot job starts kindil (`bin/boot.sh`).
const BOOT_FLAG: &str = "/mnt/us/kindil/BOOT";
/// The start-at-boot flag's content for a standalone boot (bin/boot.sh).
const BOOT_STANDALONE: &str = "standalone";
/// Amazon's framework job skips its Java framework while this exists.
/// session.sh and boot.sh remove it on every way back to Amazon's UI.
const SKIP_FRAMEWORK: &str = "/mnt/us/DONT_START_FRAMEWORK";
/// Boots in a row where kindil didn't stay up; turning start at boot on
/// gives it a fresh start.
const BOOT_TRIES: &str = "/var/local/kindil/boot-tries";
/// The KUAL extension's binaries: the current one, a staged update, the one
/// on trial and the previous one (see `bin/session.sh`).
const EXTENSION_BIN: &str = "/mnt/us/extensions/kindil/bin";
/// The launcher's marker while a new version is on trial.
const UPDATE_TRIAL: &str = "/var/local/kindil/trying";

pub struct KindleHost {
    frontlight: Result<Frontlight, String>,
    net: Arc<Net>,
}

impl KindleHost {
    /// Reads the front light's range; other services are reached per call.
    pub fn open() -> Self {
        Self {
            frontlight: Frontlight::open().map_err(|e| e.to_string()),
            net: Arc::default(),
        }
    }
}

/// The PW1's fuel gauge and charger, read where powerd reads them; the
/// gauge has no `power_supply` class entry.
fn gauge_battery() -> anyhow::Result<Battery> {
    let text = std::fs::read_to_string(GAUGE_CAPACITY)?;
    let percent =
        parse_capacity(&text).ok_or_else(|| anyhow::anyhow!("unexpected capacity {text:?}"))?;
    let charge = match std::fs::read_to_string(CHARGER)?.trim() {
        "1" => Charge::Charging,
        _ => Charge::Discharging,
    };
    Ok(Battery::Known { percent, charge })
}

/// The gauge reports `71%`.
pub fn parse_capacity(text: &str) -> Option<u8> {
    let percent: u8 = text.trim().trim_end_matches('%').parse().ok()?;
    Some(percent.min(100))
}

impl Host for KindleHost {
    fn brightness(&self) -> anyhow::Result<Brightness> {
        let light = self
            .frontlight
            .as_ref()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        light.brightness()
    }

    fn set_brightness(&self, level: Brightness) -> anyhow::Result<()> {
        let light = self
            .frontlight
            .as_ref()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        light.set(level)
    }

    fn wifi(&self) -> Wifi {
        match self.net.power() {
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
        self.net.set(power)
    }

    fn reconnect_wifi(&self) -> anyhow::Result<()> {
        self.net.reconnect()
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
            Power::On => Ok(std::fs::write(USBNET_AT_BOOT, b"")?),
            Power::Off => remove_if_present(USBNET_AT_BOOT),
        }
    }

    fn boot_start(&self) -> BootStart {
        if !Path::new(BOOT_JOB).is_file() {
            return BootStart::NotInstalled;
        }
        BootStart::Installed(match std::fs::read_to_string(BOOT_FLAG) {
            Err(_) => Boot::Off,
            Ok(mode) if mode.trim() == BOOT_STANDALONE => Boot::Standalone,
            Ok(_) => Boot::AfterAmazon,
        })
    }

    fn set_boot_start(&self, boot: Boot) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.boot_start() != BootStart::NotInstalled,
            "the boot job is not installed"
        );
        match boot {
            Boot::Off => {
                remove_if_present(BOOT_FLAG)?;
                remove_if_present(SKIP_FRAMEWORK)
            }
            Boot::AfterAmazon => {
                std::fs::write(BOOT_FLAG, b"")?;
                remove_if_present(SKIP_FRAMEWORK)?;
                remove_if_present(BOOT_TRIES)
            }
            Boot::Standalone => {
                std::fs::write(BOOT_FLAG, format!("{BOOT_STANDALONE}\n"))?;
                std::fs::write(SKIP_FRAMEWORK, b"")?;
                remove_if_present(BOOT_TRIES)
            }
        }
    }

    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            battery: gauge_battery().unwrap_or_else(|_| crate::battery::read(Path::new("/sys"))),
            charge: std::fs::read_to_string(GAUGE_CHARGE)
                .ok()
                .and_then(|text| text.trim().parse().ok())
                .map(crate::drain::MilliampHours),
            memory: std::fs::read_to_string("/proc/meminfo")
                .ok()
                .and_then(|text| parse_meminfo(&text)),
            storage: storage(Path::new(USER_STORAGE)),
            uptime: std::fs::read_to_string("/proc/uptime")
                .ok()
                .and_then(|text| parse_uptime(&text)),
        }
    }

    fn listen(&self, deliver: Deliver) {
        let deliver = Arc::new(deliver);
        let power = deliver.clone();
        std::thread::spawn(move || {
            // Without it the power button does nothing: never give up on it.
            loop {
                if let Err(error) = power_button(|| power(Event::PowerButton)) {
                    eprintln!("kindil: power button: {error}; listening again");
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        });
        std::thread::spawn(move || {
            if let Err(error) = volumd(|event| deliver(event)) {
                eprintln!("kindil: USB drive mode: {error}");
            }
        });
        std::thread::spawn(|| {
            if let Err(error) = wifid() {
                eprintln!("kindil: Wi-Fi: {error}");
            }
        });
        let net = self.net.clone();
        std::thread::spawn(move || {
            take_over_wifi(&net);
            watch_wifi(&net);
        });
    }

    fn suspend(&self) -> anyhow::Result<()> {
        // A nap's alarm must not wake a sleep the user asked for.
        if let Ok(alarm) = wakealarm() {
            let _ = std::fs::write(alarm, "0");
        }
        // powerd suspends the same way; it ignores the button while kindil
        // runs, so nothing else will.
        std::fs::write("/sys/power/state", b"mem")?;
        Ok(())
    }

    fn nap(&self, wake_after: Duration) -> anyhow::Result<()> {
        let alarm = wakealarm()?;
        // Writing a new alarm over a pending one fails: clear it first.
        std::fs::write(&alarm, "0")?;
        std::fs::write(&alarm, format!("+{}", wake_after.as_secs().max(1)))?;
        let slept = std::fs::write("/sys/power/state", b"mem");
        // The power button may have woken it first: never leave the alarm set.
        let _ = std::fs::write(&alarm, "0");
        Ok(slept?)
    }

    fn restart(&self) -> anyhow::Result<()> {
        run(&mut Command::new("/sbin/reboot"))
    }

    fn power_off(&self) -> anyhow::Result<()> {
        // Let the panel finish drawing the "Off" screen, which it then keeps.
        std::thread::sleep(crate::system::PANEL_SETTLE);
        run(Command::new("/sbin/shutdown").args(["-h", "now"]))
    }

    fn check_update(&self) -> anyhow::Result<Check> {
        Ok(Check::of(Version::current(), Github::default().latest()?))
    }

    fn stage_update(&self, release: &Release) -> anyhow::Result<()> {
        let binary = Github::default().download(release)?;
        Slots::new(EXTENSION_BIN, UPDATE_TRIAL).stage(&binary, release.version)
    }

    fn commit_update(&self) -> anyhow::Result<Commit> {
        Slots::new(EXTENSION_BIN, UPDATE_TRIAL).commit()
    }
}

/// The RTC that can wake the device: on the PW1 the PMIC's (`rtc1`); the
/// SoC's own RTC has no wake alarm.
fn wakealarm() -> anyhow::Result<std::path::PathBuf> {
    (0..4)
        .map(|n| std::path::PathBuf::from(format!("/sys/class/rtc/rtc{n}/wakealarm")))
        .find(|path| path.exists())
        .ok_or_else(|| anyhow::anyhow!("no RTC wake alarm"))
}

fn run(command: &mut Command) -> anyhow::Result<()> {
    let status = command.status()?;
    anyhow::ensure!(status.success(), "{command:?} exited with {status}");
    Ok(())
}

/// The PW1 reports the power button as this misc device coming online.
fn is_power_button(event: &Uevent) -> bool {
    event.action == crate::uevent::Action::Online && event.devpath.ends_with("/yoshibutton")
}

fn power_button(pressed: impl Fn()) -> std::io::Result<()> {
    let listener = crate::uevent::Listener::open()?;
    loop {
        if is_power_button(&listener.next()?) {
            pressed();
        }
    }
}

/// Follows volumd's drive mode.
fn volumd(deliver: impl Fn(Event)) -> anyhow::Result<()> {
    lipc_events(
        "com.lab126.volumd",
        "driveModeStateChanged,userstoreIsLikelyToUnMount",
        |line| {
            if let Some(event) = system::volumd_event(line) {
                deliver(event);
            }
        },
    )
}

/// Each time Wi-Fi connects, does what Amazon's connection manager (`cmd`)
/// would: a standalone boot stops it (bin/boot.sh), and udhcpc leaves the
/// default route and the name servers to it. While `cmd` runs, both are
/// already in place and this changes nothing.
fn wifid() -> anyhow::Result<()> {
    let route = || {
        if let Err(error) = route_wifi() {
            eprintln!("kindil: Wi-Fi route: {error}");
        }
    };
    route();
    lipc_events("com.lab126.wifid", "cmConnected", |_| route())
}

/// Whether Wi-Fi, switched on, has a connection with an address.
fn wifi_seen(net: &Net) -> system::WifiSeen {
    match net.power() {
        Ok(Power::Off) => system::WifiSeen::Off,
        Err(_) => system::WifiSeen::Down,
        Ok(Power::On) => match wifi::open(INTERFACE).and_then(|wpa| wpa.link()) {
            Ok(Link::Associated { ip: Some(_), .. }) => system::WifiSeen::Up,
            _ => system::WifiSeen::Down,
        },
    }
}

/// Waits for wifid to give wpa_supplicant the saved networks, then runs
/// Wi-Fi without it; until then, and if this fails, wifid keeps it.
fn take_over_wifi(net: &Net) {
    let since = std::time::Instant::now();
    while since.elapsed() < WIFI_TAKEOVER {
        match net.take_over() {
            Ok(true) => {
                eprintln!("kindil: running Wi-Fi without wifid");
                return;
            }
            Ok(false) => {}
            Err(error) => eprintln!("kindil: Wi-Fi takeover: {error}"),
        }
        std::thread::sleep(Duration::from_secs(5));
    }
    eprintln!("kindil: no saved networks reached wpa_supplicant; wifid keeps Wi-Fi");
}

/// Whatever left Wi-Fi on but stuck (a suspend nobody heard of, a script,
/// the router), joining again brings it back: checked each minute kindil is
/// awake. Time suspended doesn't count, so a dozing Kindle, which
/// reconnects in its online windows, is rarely touched.
fn watch_wifi(net: &Net) {
    let mut watchdog = system::WifiWatchdog::default();
    // Firings with no connection seen in between.
    let mut stuck_in_a_row = 0u8;
    loop {
        std::thread::sleep(system::WifiWatchdog::EVERY);
        let (next, stuck) = watchdog.saw(wifi_seen(net));
        watchdog = next;
        if !stuck {
            if next == system::WifiWatchdog::default() {
                stuck_in_a_row = 0;
            }
            continue;
        }
        stuck_in_a_row += 1;
        // kindil's own way failed twice: wifid's full restart is the fallback.
        let result = if stuck_in_a_row >= 2 {
            stuck_in_a_row = 0;
            eprintln!("kindil: Wi-Fi still stuck; handing it back to wifid");
            net.hand_back()
        } else {
            eprintln!("kindil: Wi-Fi stuck; joining again");
            net.reconnect()
        };
        if let Err(error) = result {
            eprintln!("kindil: Wi-Fi watchdog: {error}");
        }
    }
}

fn route_wifi() -> anyhow::Result<()> {
    let lease = match std::fs::read_to_string(DHCP_LEASE) {
        // Not connected since boot: cmConnected comes once it is.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        lease => lease?,
    };
    let Some(gateway) = lease_gateway(&lease) else {
        return Ok(());
    };
    let routes = std::fs::read_to_string(ROUTES)?;
    // No address on Wi-Fi (an old lease, Wi-Fi down): the router is out of reach.
    if !routes_through(&routes, INTERFACE) {
        return Ok(());
    }
    if !has_default_route(&routes) {
        let gateway = gateway.to_string();
        run(Command::new("/sbin/route").args(["add", "default", "gw", &gateway, "dev", INTERFACE]))?;
    }
    let servers = std::fs::read_to_string(DHCP_RESOLV)?;
    if std::fs::read_to_string(RESOLV).ok().as_ref() != Some(&servers) {
        std::fs::write(RESOLV, servers)?;
    }
    Ok(())
}

/// The router in udhcpc's lease (`gw=192.168.1.1`), if it named one.
pub fn lease_gateway(lease: &str) -> Option<std::net::Ipv4Addr> {
    lease
        .lines()
        .find_map(|line| line.strip_prefix("gw="))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// True if `/proc/net/route` has any route through `interface`: it has an address.
pub fn routes_through(table: &str, interface: &str) -> bool {
    table
        .lines()
        .skip(1)
        .any(|line| line.split_whitespace().next() == Some(interface))
}

/// True if `/proc/net/route` has a default route, on any interface.
pub fn has_default_route(table: &str) -> bool {
    table.lines().skip(1).any(|line| {
        let columns: Vec<&str> = line.split_whitespace().collect();
        columns.get(1) == Some(&"00000000") && columns.get(7) == Some(&"00000000")
    })
}

/// Hands each of `events` from lipc service `publisher` to `each`, through
/// `lipc-wait-event`, which dies with kindil. Returns once it exits.
fn lipc_events(publisher: &str, events: &str, mut each: impl FnMut(&str)) -> anyhow::Result<()> {
    let mut command = Command::new("lipc-wait-event");
    command
        .args(["-m", publisher, events])
        .stdout(std::process::Stdio::piped());
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: prctl is async-signal-safe and touches only this child.
        unsafe {
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
    }
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("no stdout"))?;
    std::io::BufReader::new(stdout)
        .lines()
        .map_while(Result::ok)
        .for_each(|line| each(&line));
    anyhow::bail!("lipc-wait-event exited with {}", child.wait()?)
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

fn remove_if_present(path: &str) -> anyhow::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_the_power_button() {
        let press = Uevent::parse(
            b"online@/devices/virtual/misc/yoshibutton\0ACTION=online\0SUBSYSTEM=misc\0",
        )
        .unwrap();
        let console = Uevent::parse(b"add@/devices/virtual/vc/vcs63\0ACTION=add\0").unwrap();
        assert!(is_power_button(&press));
        assert!(!is_power_button(&console));
    }

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
    fn reads_the_router_from_the_dhcp_lease() {
        let lease =
            "ip=192.168.1.130\nbroadcast=192.168.1.255\nsubnet=255.255.255.0\ngw=192.168.1.1\n";
        assert_eq!(
            lease_gateway(lease),
            Some(std::net::Ipv4Addr::new(192, 168, 1, 1))
        );
        assert_eq!(lease_gateway("ip=192.168.1.130\ngw=\n"), None);
        assert_eq!(lease_gateway(""), None);
    }

    #[test]
    fn finds_a_default_route() {
        let header =
            "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n";
        let local = "wlan0\t0001A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0\n";
        let default = "wlan0\t00000000\t0101A8C0\t0003\t0\t0\t0\t00000000\t0\t0\t0\n";
        assert!(!has_default_route(&format!("{header}{local}")));
        assert!(routes_through(&format!("{header}{local}"), "wlan0"));
        assert!(!routes_through(header, "wlan0"), "no address, no route");
        assert!(has_default_route(&format!("{header}{local}{default}")));
    }

    #[test]
    fn reads_the_gauges_capacity() {
        assert_eq!(parse_capacity("71%\n"), Some(71));
        assert_eq!(parse_capacity("100%"), Some(100));
        assert_eq!(parse_capacity("%"), None);
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
