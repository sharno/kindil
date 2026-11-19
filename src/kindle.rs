//! The stock Kindle OS as a [`Host`]: kindil runs as an app on top of Amazon's
//! kernel and services, never as init, and changes nothing that survives a reboot
//! except the documented USBNetwork "SSH at boot" flag file. USB drive mode is
//! volumd's, unless switched to kindil's own (src/drive.rs, src/usb.rs).
use crate::battery::{Battery, Charge};
use crate::frontlight::Frontlight;
use crate::host::{
    Boot, BootStart, Brightness, Deliver, DeviceInfo, Host, Memory, OwnSsh, Power, Ssh, SshKey,
    Storage, Wifi,
};
use crate::net::Net;
use crate::networks::{self, Networks};
use crate::system::{self, Event};
use crate::uevent::Uevent;
use crate::update::{Check, Commit, Github, Ota, Release, Slots, Version};
use crate::wifi::{Credentials, Link, Network, Ssid};
use std::io::BufRead;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long kindil keeps retrying its Wi-Fi at start (a slow driver).
const WIFI_START: Duration = Duration::from_secs(180);
/// How long the start's clock sync waits for Wi-Fi to have an address.
const CLOCK_SYNC_WAIT: Duration = Duration::from_secs(120);
/// `1` while the charger may charge; writing `0` pauses it (src/power.rs).
const CHARGER: &str = "/sys/devices/platform/aplite_charger.0/charging";
/// `1` while a charger is plugged in.
const CHARGER_CONNECTED: &str = "/sys/devices/platform/aplite_charger.0/connected";
/// Whether kindil paused charging for the temperature (src/power.rs); only
/// a pause of kindil's own is ever lifted.
static CHARGING_PAUSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// USBNetwork toggles dropbear when configured with USE_WIFI_SSHD_ONLY.
const USBNET_TOGGLE: &str = "/mnt/us/usbnet/bin/usbnetwork";
const USBNET_AT_BOOT: &str = "/mnt/us/usbnet/auto";
const USER_STORAGE: &str = "/mnt/us";
/// The boot job, installed by `boot/install.sh` in the extension.
const BOOT_JOB: &str = "/etc/upstart/kindil.conf";
/// Present: the boot job starts kindil (`bin/boot.sh`).
const BOOT_FLAG: &str = "/mnt/us/kindil/BOOT";
/// The start-at-boot flag's content for a standalone boot (launcher/src/boot.rs).
const BOOT_STANDALONE: &str = "standalone";
/// Amazon's framework job skips its Java framework while this exists.
/// The launcher removes it on every way back to Amazon's UI.
const SKIP_FRAMEWORK: &str = "/mnt/us/DONT_START_FRAMEWORK";
/// Boots in a row where kindil didn't stay up; turning start at boot on
/// gives it a fresh start.
const BOOT_TRIES: &str = "/var/local/kindil/boot-tries";
/// The KUAL extension's binaries: the current one, a staged update, the one
/// on trial and the previous one (see launcher/src/session.rs).
const EXTENSION_BIN: &str = "/mnt/us/extensions/kindil/bin";
/// The launcher's marker while a new version is on trial.
const UPDATE_TRIAL: &str = "/var/local/kindil/trying";

pub struct KindleHost {
    frontlight: Result<Frontlight, String>,
    net: Arc<Net>,
    networks: Arc<Mutex<Networks>>,
    /// The UI let go of the user storage (kindil's own USB drive mode).
    released: Arc<crate::usb::Released>,
}

impl KindleHost {
    /// Reads the front light's range; other services are reached per call.
    pub fn open() -> Self {
        // Amazon's UI, which ran until now, widened them (src/perms.rs):
        // narrowed before anything is read or run.
        for line in crate::perms::narrow_own(Path::new(crate::perms::RUN)) {
            crate::log!("kindil: {line}");
        }
        Self {
            frontlight: Frontlight::open().map_err(|e| e.to_string()),
            net: Arc::default(),
            networks: Arc::new(Mutex::new(Networks::open(Path::new(networks::PATH)))),
            released: Arc::default(),
        }
    }
}

/// The PW1's fuel gauge, read from the chip now (src/gauge.rs), and the
/// charger; the gauge has no `power_supply` class entry. Plugged in counts
/// as charging, even while full or paused for the temperature: not on its
/// own power.
fn gauge_battery(gauge: &crate::gauge::Cached) -> anyhow::Result<Battery> {
    let percent = gauge
        .percent
        .ok_or_else(|| anyhow::anyhow!("no state of charge from the gauge"))?;
    let charge = if plugged()? {
        Charge::Charging
    } else {
        Charge::Discharging
    };
    Ok(Battery::Known { percent, charge })
}

fn plugged() -> anyhow::Result<bool> {
    plugged_from(std::fs::read_to_string(CHARGER_CONNECTED))
}

/// The charger driver makes `connected` only while a charger is plugged
/// in: no file means on battery, not an error.
fn plugged_from(read: std::io::Result<String>) -> anyhow::Result<bool> {
    match read {
        Ok(text) => Ok(text.trim() == "1"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// powerd's charging temperature range, kept by kindil: pauses the charger
/// outside it, and lifts a pause of its own once well inside again.
fn guard_charging(gauge: &crate::gauge::Cached) -> anyhow::Result<()> {
    use std::sync::atomic::Ordering;
    let paused = CHARGING_PAUSED.load(Ordering::Relaxed);
    let temperature = gauge
        .temperature
        .ok_or_else(|| anyhow::anyhow!("no temperature from the gauge"))?;
    let pause = plugged()? && crate::power::pause_charging(temperature, paused);
    if pause != paused {
        std::fs::write(CHARGER, if pause { "0" } else { "1" })?;
        CHARGING_PAUSED.store(pause, Ordering::Relaxed);
        crate::log!(
            "kindil: charging {} at {temperature} °F",
            if pause { "paused" } else { "resumed" }
        );
    }
    Ok(())
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
            Power::Off => Wifi::Off,
            // A supplicant that doesn't answer is being restarted: the switch
            // stays usable, to switch Wi-Fi off or on again.
            Power::On => match self.net.link() {
                Err(_) | Ok((Link::NotAssociated, _)) => Wifi::Searching,
                Ok((Link::Associated { ssid, ip }, signal)) => Wifi::Connected { ssid, ip, signal },
            },
        }
    }

    fn set_wifi(&self, power: Power) -> anyhow::Result<()> {
        let saved = lock(&self.networks).list().to_vec();
        self.net.set(power, &saved)
    }

    fn reconnect_wifi(&self) -> anyhow::Result<()> {
        self.net.reconnect()
    }

    fn park_wifi(&self) {
        let since = std::time::Instant::now();
        let parked = self.net.park();
        let took = since.elapsed();
        if let Err(error) = &parked {
            crate::log!("kindil: parking Wi-Fi: {error}");
        }
        crate::flight::Recorder::device().note(&format!(
            "Wi-Fi {} in {:.2} s",
            if parked.is_ok() {
                "parked"
            } else {
                "not parked"
            },
            took.as_secs_f64()
        ));
    }

    fn ssh_sessions(&self) -> Vec<crate::maintenance::Session> {
        use crate::maintenance::{sessions, SSH_PORTS, TCP_TABLES};
        TCP_TABLES
            .iter()
            .filter_map(|table| std::fs::read_to_string(table).ok())
            .flat_map(|table| sessions(&table, &SSH_PORTS))
            .collect()
    }

    fn scan(&self) -> anyhow::Result<Vec<Network>> {
        self.net.scan()
    }

    fn join(&self, credentials: &Credentials) -> anyhow::Result<()> {
        let saved = lock(&self.networks).list().to_vec();
        // The new network first, preferred over the others; saved only once
        // it has joined, so a wrong password never sticks.
        let candidate: Vec<Credentials> = std::iter::once(credentials.clone())
            .chain(
                saved
                    .iter()
                    .filter(|c| c.ssid() != credentials.ssid())
                    .cloned(),
            )
            .collect();
        self.net.try_join(&candidate, &saved)?;
        lock(&self.networks).add(credentials.clone())
    }

    fn saved_networks(&self) -> Vec<String> {
        lock(&self.networks).ssids()
    }

    fn forget_network(&self, ssid: &Ssid) -> anyhow::Result<()> {
        let saved = {
            let mut networks = lock(&self.networks);
            networks.forget(ssid)?;
            networks.list().to_vec()
        };
        self.net.load(&saved)
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

    fn own_ssh(&self) -> OwnSsh {
        let layout = crate::ssh::Layout::default();
        let usb = Path::new(crate::ssh::USBNET_AUTHORIZED_KEYS);
        let keys = |list: anyhow::Result<Vec<crate::ssh_keys::Key>>| {
            list.unwrap_or_default().iter().map(SshKey::from).collect()
        };
        OwnSsh {
            port: crate::ssh::PORT,
            running: crate::ssh::running(),
            trusted: keys(crate::ssh_keys::trusted(&layout.authorized_keys())),
            new_on_usb: keys(crate::ssh_keys::new_on_usb(
                usb,
                &layout.authorized_keys(),
                &layout.ignored_keys(),
            )),
        }
    }

    fn trust_ssh_keys(&self, fingerprints: &[String]) -> anyhow::Result<()> {
        let layout = crate::ssh::Layout::default();
        crate::ssh_keys::trust(
            Path::new(crate::ssh::USBNET_AUTHORIZED_KEYS),
            &layout.authorized_keys(),
            &layout.ignored_keys(),
            fingerprints,
        )?;
        Ok(())
    }

    fn ignore_ssh_keys(&self, fingerprints: &[String]) -> anyhow::Result<()> {
        crate::ssh_keys::ignore(&crate::ssh::Layout::default().ignored_keys(), fingerprints)
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
        if let Some(dir) = Path::new(BOOT_FLAG).parent() {
            std::fs::create_dir_all(dir)?;
        }
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
        // Read each minute and each doze wake: the charging guard rides
        // along. One reading of the chip for all of it.
        let gauge = crate::gauge::now();
        if let Err(error) = guard_charging(&gauge) {
            crate::log!("kindil: charging guard: {error}");
        }
        DeviceInfo {
            battery: gauge_battery(&gauge)
                .unwrap_or_else(|_| crate::battery::read(Path::new("/sys"))),
            charge: gauge.charge,
            memory: std::fs::read_to_string("/proc/meminfo")
                .ok()
                .and_then(|text| parse_meminfo(&text)),
            storage: storage(Path::new(USER_STORAGE)),
            uptime: std::fs::read_to_string("/proc/uptime")
                .ok()
                .and_then(|text| parse_uptime(&text)),
            current: gauge.current,
        }
    }

    fn listen(&self, deliver: Deliver) {
        let deliver = Arc::new(deliver);
        let power = deliver.clone();
        std::thread::spawn(move || {
            // Without it the power button does nothing: never give up on it.
            loop {
                if let Err(error) = power_button(|| power(Event::PowerButton)) {
                    crate::log!("kindil: power button: {error}; listening again");
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        });
        let released = self.released.clone();
        std::thread::spawn(move || usb_drive(&*deliver, &released));
        let net = self.net.clone();
        let networks = self.networks.clone();
        std::thread::spawn(move || {
            start_wifi(&net, &networks);
            sync_clock_once_online(&net);
        });
        // kindil's own SSH server (src/ssh.rs), beside USBNetwork's for now.
        crate::ssh::start();
    }

    fn storage_released(&self) {
        self.released.notify();
    }

    fn suspend(
        &self,
        why: crate::system::Why,
        ticket: &crate::suspend::Ticket,
    ) -> anyhow::Result<()> {
        let device = Sleeping {
            recorder: crate::flight::Recorder::device(),
            ticket,
        };
        match crate::power::sleep(&device, why)? {
            crate::power::Slept::Woken | crate::power::Slept::Charging => Ok(()),
            crate::power::Slept::Critical => {
                crate::log!("kindil: battery critical; powering off");
                self.power_off()
            }
        }
    }

    fn note(&self, line: &str) {
        crate::flight::Recorder::device().note(line);
    }

    fn nap(
        &self,
        deadline: std::time::Instant,
        ticket: &crate::suspend::Ticket,
    ) -> anyhow::Result<()> {
        let recorder = crate::flight::Recorder::device();
        // Measured now, after parking Wi-Fi and right before arming: the
        // time it took doesn't make the nap overshoot its minute. On the
        // monotonic clock: the wall clock may have been stepped meanwhile.
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left < Duration::from_secs(1) {
            recorder.note(&format!("nap: {left:?} left; not suspending"));
            return Ok(());
        }
        sleep_once(&recorder, "nap", crate::suspend::Wake::Alarm(left), ticket).map(drop)
    }

    /// [`sleep_once`] caught up already, before anything read the clock.
    fn catch_up_clock(&self) -> Option<crate::clock::Correction> {
        lock(&CAUGHT_UP).take()
    }

    fn sync_clock(&self) -> anyhow::Result<Option<crate::clock::Correction>> {
        crate::clock::sync()
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
        Ota::available(release.clone())
            .download(&Github::default())?
            .stage(&Slots::new(EXTENSION_BIN, UPDATE_TRIAL))
            .map(drop)
    }

    fn commit_update(&self) -> anyhow::Result<Commit> {
        let commit = Slots::new(EXTENSION_BIN, UPDATE_TRIAL).commit()?;
        // Up for two minutes: this version's launcher is the one that starts
        // kindil from now on (src/launcher.rs).
        match crate::launcher::install(Path::new(crate::launcher::PATH)) {
            Ok(true) => crate::log!("kindil: launcher updated"),
            Ok(false) => {}
            Err(error) => crate::log!("kindil: writing the launcher: {error:#}"),
        }
        Ok(commit)
    }

    fn search_places(&self, name: &str) -> anyhow::Result<Vec<crate::location::Place>> {
        crate::location::Lookup::default().search(name)
    }

    fn locate_by_ip(&self) -> anyhow::Result<crate::location::Place> {
        crate::location::Lookup::default().locate()
    }
}

/// The power button's presses since kindil started, counted by its listener
/// ([`power_button`]): a wake from the battery's sleep tells the button from
/// a charger by it.
static PRESSES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The device under a sleep (src/power.rs).
struct Sleeping<'a> {
    recorder: crate::flight::Recorder,
    ticket: &'a crate::suspend::Ticket,
}

impl crate::power::Sleeper for Sleeping<'_> {
    fn plugged(&self) -> bool {
        plugged().unwrap_or(false)
    }
    fn presses(&self) -> u64 {
        PRESSES.load(std::sync::atomic::Ordering::SeqCst)
    }
    /// The user asked for the sleep, or the battery did: a sleep the user
    /// asked for still sleeps without its check's alarm, until the button,
    /// rather than stay up; the battery's doesn't (src/power.rs).
    fn suspend(&self, wake: crate::suspend::Wake) -> anyhow::Result<bool> {
        sleep_once(&self.recorder, "sleep", wake, self.ticket)
    }
    fn cancelled(&self) -> bool {
        !self.ticket.valid()
    }
    fn now(&self) -> i64 {
        epoch_seconds()
    }
    /// The chip now: the driver's cache wasn't refreshed in the hours
    /// asleep.
    fn battery(&self) -> anyhow::Result<Battery> {
        gauge_battery(&crate::gauge::now())
    }
    fn wait(&self, time: Duration) {
        std::thread::sleep(time);
    }
    fn note(&self, line: &str) {
        self.recorder.note(line);
    }
}

/// The last suspend's clock correction, for the doze's ledger.
static CAUGHT_UP: Mutex<Option<crate::clock::Correction>> = Mutex::new(None);

/// The PW1 under a suspend (src/suspend.rs).
struct Kernel<'a> {
    recorder: &'a crate::flight::Recorder,
}

impl crate::suspend::Kernel for Kernel<'_> {
    fn note(&self, line: &str) {
        self.recorder.note(line);
    }
    fn mem(&self) -> std::io::Result<()> {
        std::fs::write("/sys/power/state", b"mem")
    }
    fn epoch(&self) -> i64 {
        epoch_seconds()
    }
    fn catch_up(&self) -> Option<crate::clock::Correction> {
        crate::clock::catch_up()
    }
}

/// Every suspend goes through here (src/suspend.rs): the wake alarm armed
/// and read back as the very last thing before it, or, for a sleep the user
/// asked for, moved out of the way; no suspend at all if an alarm it needs
/// fails. The clock's correction is kept for the doze's ledger. Whether a
/// wake alarm was armed for it.
fn sleep_once(
    recorder: &crate::flight::Recorder,
    what: &'static str,
    wake: crate::suspend::Wake,
    ticket: &crate::suspend::Ticket,
) -> anyhow::Result<bool> {
    let rtc = crate::wakealarm::WakeRtc::find();
    let env = crate::suspend::Env {
        rtc: rtc.as_ref().map(|rtc| rtc as &dyn crate::suspend::Rtc),
        kernel: &Kernel { recorder },
        ticket,
    };
    let back = crate::suspend::once(&env, what, wake)?;
    if let Some(correction) = back.correction {
        *lock(&CAUGHT_UP) = Some(correction);
    }
    match back.refused {
        Some(error) => Err(error.into()),
        None => Ok(back.armed),
    }
}

fn epoch_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
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
            PRESSES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            pressed();
        }
    }
}

/// USB drive mode: volumd's, followed, unless the switch gives it to
/// kindil (src/drive.rs) and the launcher stopped volumd.
fn usb_drive(deliver: &(dyn Fn(Event) + Send + Sync), released: &crate::usb::Released) {
    let follow_volumd = || {
        if let Err(error) = volumd(deliver) {
            crate::log!("kindil: USB drive mode: {error}");
        }
    };
    match crate::drive::Owner::read(Path::new(crate::drive::SWITCH)) {
        crate::drive::Owner::Amazon => follow_volumd(),
        crate::drive::Owner::Kindil => {
            crate::log!("kindil: USB drive mode is kindil's; waiting for volumd to stop");
            if !crate::usb::volumd_gone() {
                crate::log!("kindil: volumd still runs; USB drive mode stays volumd's");
                return follow_volumd();
            }
            loop {
                if let Err(error) = crate::usb::run_drive(deliver, released) {
                    crate::log!("kindil: USB drive: {error}; starting again");
                }
                std::thread::sleep(Duration::from_secs(1));
            }
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

/// Starts kindil's Wi-Fi if it was left on, with the saved networks (none
/// on a first run: then it scans, and a network joined in Settings is
/// saved). A failed start is retried for a while; after that the switch
/// in Settings tries again.
fn start_wifi(net: &Arc<Net>, networks: &Mutex<Networks>) {
    let since = std::time::Instant::now();
    loop {
        let saved = lock(networks).list().to_vec();
        // Asked each time: switched off meanwhile, it stays off. Off still
        // takes Wi-Fi from Amazon: no Wi-Fi at all while kindil runs.
        if Net::wanted() == Power::Off {
            crate::log!("kindil: Wi-Fi is switched off");
            if let Err(error) = net.set(Power::Off, &saved) {
                crate::log!("kindil: Wi-Fi off: {error}");
            }
            return;
        }
        match net.set(Power::On, &saved) {
            Ok(()) => {
                crate::log!("kindil: Wi-Fi up with {} saved networks", saved.len());
                return;
            }
            Err(error) if since.elapsed() < WIFI_START => {
                crate::log!("kindil: Wi-Fi start: {error}; trying again");
                std::thread::sleep(Duration::from_secs(5));
            }
            Err(error) => {
                crate::log!("kindil: Wi-Fi start: {error}; switch Wi-Fi on to try again");
                return;
            }
        }
    }
}

/// At start: once Wi-Fi has an address, sets the clock from network time
/// (Amazon's services, which did, are stopped). Later, each online doze
/// wake and each wake from sleep does it again (src/ui.rs).
fn sync_clock_once_online(net: &Net) {
    let since = std::time::Instant::now();
    while since.elapsed() < CLOCK_SYNC_WAIT && net.power() == Power::On {
        if let Ok((Link::Associated { ip: Some(_), .. }, _)) = net.link() {
            match crate::clock::sync() {
                Ok(Some(correction)) => crate::log!("kindil: {correction}"),
                Ok(None) => crate::log!("kindil: clock: right by network time"),
                Err(error) => crate::log!("kindil: clock: {error:#}"),
            }
            return;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
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
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
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

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn remove_if_present(path: &str) -> anyhow::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn no_connected_file_means_on_battery() {
        use std::io::{Error, ErrorKind};
        assert!(!plugged_from(Err(Error::from(ErrorKind::NotFound))).unwrap());
        assert!(plugged_from(Ok("1\n".into())).unwrap());
        assert!(!plugged_from(Ok("0\n".into())).unwrap());
        assert!(plugged_from(Err(Error::from(ErrorKind::PermissionDenied))).is_err());
    }
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
