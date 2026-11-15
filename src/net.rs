//! Wi-Fi, run by kindil itself, always.
//!
//! While kindil runs, none of Amazon's Wi-Fi pieces do (wifid, and the wifim
//! and wifis jobs it drives): kindil loads the ar6003 driver, runs its own
//! wpa_supplicant (src/wifi.rs, built by the flake) and udhcpc with its own
//! hook as its children, and loads its saved networks (src/networks.rs),
//! none on a first run until one is joined. A child that exits without being
//! told is started again, wpa_supplicant with the networks loaded again,
//! with a growing pause if it keeps dying. Off stops them and takes the
//! interface down, and is remembered; the driver is never unloaded. Amazon's wifid only runs outside
//! kindil: the launcher starts it for Amazon's UI.
//!
//! Behind a switch ([`station::SWITCH`] saying `kindil`), each start runs
//! kindil's own Wi-Fi instead of wpa_supplicant: a keeper thread
//! (src/station.rs) joins through the driver's wireless extensions, beside
//! the same udhcpc. If it gives up, it falls back to wpa_supplicant for the
//! rest of the run.
use crate::host::{Power, Signal};
use crate::station::{self, Kick};
use crate::wext::{self, Bss, Driver};
use crate::wifi::{self, Credentials, Link, Network};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const INTERFACE: &str = "wlan0";
/// kindil's own wpa_supplicant (upstream, built by the flake), carried
/// inside kindil and written out here, off /mnt/us, which goes away in USB
/// drive mode under a running program.
const WPA: &str = "/var/local/kindil/wpa_supplicant";
const WPA_BINARY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/wpa_supplicant"));
/// kindil's own wpa_supplicant config: no networks in it, they are loaded
/// over the control socket from kindil's store. Built without config
/// writing, it rejects `update_config`.
const WPA_CONF: &str = "/var/local/kindil/wpa_supplicant.conf";
const WPA_CONF_TEXT: &str = "ctrl_interface=/var/run/wpa_supplicant\nap_scan=1\n";
const WPA_PID: &str = "/var/run/kindil-wpa.pid";
/// udhcpc's hook: kindil itself, through a link named for the hook (see
/// src/dhcp.rs), beside the binary on the system storage: /mnt/us goes away
/// in USB drive mode.
const DHCP_HOOK: &str = "/var/local/kindil/kindil-dhcp";
const DHCP_PID: &str = "/var/run/kindil-udhcpc.pid";
/// Present while Wi-Fi is switched off in kindil.
const OFF_FLAG: &str = "/var/local/kindil/wifi-off";
/// The PW1's (yoshime3) calibration data for the AR6003, as Amazon's
/// wifim job finds it from the board id.
const BOARD_DATA: &str = "/opt/ar6k/target/AR6003/hw2.1.1/bin";
const BOARD_FILE: &str = "wfo/AR6003_wfo_calfile.bin";
/// The driver's MAC before it has loaded its firmware.
const NO_MAC: &str = "00:00:00:00:00:00";
/// Loading the driver and its firmware takes a few seconds.
const STARTUP_WAIT: Duration = Duration::from_secs(30);
/// Joining usually takes under a second; a far access point, a few.
const JOIN_WAIT: Duration = Duration::from_secs(15);
/// A new network gets this long to prove its password.
const TRY_WAIT: Duration = Duration::from_secs(30);
/// A child that lived this long was fine; a restart after it waits only
/// the shortest pause.
const HEALTHY: Duration = Duration::from_secs(60);
const PAUSE_MIN: Duration = Duration::from_secs(1);
const PAUSE_MAX: Duration = Duration::from_secs(60);
/// A scan of kindil's own Wi-Fi this recent picks how to join a network
/// chosen from it in Settings.
const SCAN_FRESH: Duration = Duration::from_secs(120);

/// Which Wi-Fi a start of kindil's stack runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Supplicant,
    /// kindil's own (src/station.rs).
    Kindil,
}

/// The switch's choice, unless kindil's own Wi-Fi gave up this run.
pub fn backend(switch: Option<&str>, fell_back: bool) -> Backend {
    if !fell_back && station::chosen(switch) {
        Backend::Kindil
    } else {
        Backend::Supplicant
    }
}

pub struct Net {
    /// Held through every transition (on, off, load, join, reconnect, a
    /// supervisor's restart): they never interleave, so the last one asked
    /// for is the state Wi-Fi ends in.
    lifecycle: Mutex<()>,
    /// Whether kindil's Wi-Fi is up: On only once a start has succeeded.
    power: Mutex<Power>,
    /// The networks wpa_supplicant was last given: a restarted one gets them.
    saved: Mutex<Vec<Credentials>>,
    /// Bumped whenever kindil stops its stack, under `lifecycle`: children
    /// of an older stack exited because they were told to, and stay down.
    generation: AtomicU64,
    /// The Wi-Fi the last start ran.
    backend: Mutex<Backend>,
    /// kindil's own Wi-Fi gave up: wpa_supplicant for the rest of the run.
    fell_back: AtomicBool,
    /// For kindil's own Wi-Fi's keeper: why to look again, and its wake-up.
    kick: Mutex<Option<Kick>>,
    kicked: Condvar,
    /// Associations the keeper reported, for a reconnect to wait on.
    joins: AtomicU64,
    /// kindil's own Wi-Fi's last scan, and when.
    seen: Mutex<Option<(Instant, Vec<Bss>)>>,
}

impl Default for Net {
    fn default() -> Self {
        Self {
            lifecycle: Mutex::new(()),
            power: Mutex::new(Power::Off),
            saved: Mutex::default(),
            generation: AtomicU64::new(0),
            backend: Mutex::new(Backend::Supplicant),
            fell_back: AtomicBool::new(false),
            kick: Mutex::new(None),
            kicked: Condvar::new(),
            joins: AtomicU64::new(0),
            seen: Mutex::new(None),
        }
    }
}

impl Net {
    pub fn power(&self) -> Power {
        *lock(&self.power)
    }

    /// Whether Wi-Fi was left on: kindil starts it then.
    pub fn wanted() -> Power {
        if std::path::Path::new(OFF_FLAG).exists() {
            Power::Off
        } else {
            Power::On
        }
    }

    /// Switches kindil's Wi-Fi, with `saved` loaded, and remembers it.
    pub fn set(self: &Arc<Self>, power: Power, saved: &[Credentials]) -> anyhow::Result<()> {
        let _transition = lock(&self.lifecycle);
        self.switch(power, saved)
    }

    /// Gives the running Wi-Fi `saved`, kindil's networks, when it is on.
    pub fn load(self: &Arc<Self>, saved: &[Credentials]) -> anyhow::Result<()> {
        let _transition = lock(&self.lifecycle);
        self.give(saved)
    }

    /// Tries `candidate` as a new first network: Ok once it has joined it.
    /// Any failure puts `saved` back and is returned. Switches Wi-Fi on if off.
    pub fn try_join(
        self: &Arc<Self>,
        candidate: &[Credentials],
        saved: &[Credentials],
    ) -> anyhow::Result<()> {
        let _transition = lock(&self.lifecycle);
        let joined = self.join(candidate);
        if joined.is_err() {
            if let Err(error) = self.give(saved) {
                eprintln!("kindil: Wi-Fi: putting the saved networks back: {error}");
            }
        }
        joined
    }

    /// The Wi-Fi the stack runs (or last ran).
    pub fn backend(&self) -> Backend {
        *lock(&self.backend)
    }

    /// The link, and its signal while associated.
    pub fn link(&self) -> anyhow::Result<(Link, Option<Signal>)> {
        match self.backend() {
            Backend::Supplicant => {
                let wpa = wifi::open(INTERFACE)?;
                let link = wpa.link()?;
                let signal = match link {
                    Link::Associated { .. } => wpa.signal(),
                    Link::NotAssociated => None,
                };
                Ok((link, signal))
            }
            Backend::Kindil => own_link(),
        }
    }

    /// Scans and returns the networks around, strongest first.
    pub fn scan(self: &Arc<Self>) -> anyhow::Result<Vec<Network>> {
        if self.backend() == Backend::Supplicant {
            return wifi::open(INTERFACE)?.scan();
        }
        anyhow::ensure!(self.power() == Power::On, "Wi-Fi is off");
        let socket = wext::Socket::open(INTERFACE)?;
        match station::scan(&socket, &std::thread::sleep) {
            Ok(seen) => {
                let networks = station::networks(&seen);
                *lock(&self.seen) = Some((Instant::now(), seen));
                Ok(networks)
            }
            Err(error) => {
                let _transition = lock(&self.lifecycle);
                if self.backend() == Backend::Kindil && self.power() == Power::On {
                    self.fall_back(&format!("scanning: {error}"));
                }
                anyhow::bail!("scanning: {error}; try again")
            }
        }
    }

    /// After a suspend: join again, then renew the address (usually kept).
    /// A supplicant that doesn't answer gets the whole stack started again.
    pub fn reconnect(self: &Arc<Self>) -> anyhow::Result<()> {
        let joins = {
            let _transition = lock(&self.lifecycle);
            if self.power() == Power::Off {
                return Ok(());
            }
            if self.backend() == Backend::Supplicant {
                return self.reconnect_supplicant();
            }
            // The keeper joins again, the PMK again too, and renews.
            let joins = self.joins.load(Ordering::SeqCst);
            self.kick(Kick::Resumed);
            joins
        };
        let since = Instant::now();
        while self.joins.load(Ordering::SeqCst) == joins {
            anyhow::ensure!(since.elapsed() < JOIN_WAIT, "no access point joined");
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    }

    /// `reconnect` for wpa_supplicant, with `lifecycle` held.
    fn reconnect_supplicant(self: &Arc<Self>) -> anyhow::Result<()> {
        let rejoined =
            wifi::open(INTERFACE).and_then(|wpa| wpa.command("REASSOCIATE").map(|_| wpa));
        let Ok(wpa) = rejoined else {
            // Gone or hung (no answer within the socket's timeout).
            eprintln!("kindil: wpa_supplicant isn't answering; starting Wi-Fi again");
            let saved = lock(&self.saved).clone();
            return self.start(&saved);
        };
        let since = Instant::now();
        while !matches!(wpa.link(), Ok(Link::Associated { .. })) {
            anyhow::ensure!(since.elapsed() < JOIN_WAIT, "no access point joined");
            std::thread::sleep(Duration::from_millis(100));
        }
        renew_dhcp()
    }

    // Below, `lifecycle` is held by the caller.

    fn switch(self: &Arc<Self>, power: Power, saved: &[Credentials]) -> anyhow::Result<()> {
        match power {
            Power::On => {
                let _ = std::fs::remove_file(OFF_FLAG);
                self.start(saved)
            }
            Power::Off => {
                std::fs::write(OFF_FLAG, b"")?;
                self.stop();
                // Off means no Wi-Fi at all, Amazon's included.
                clear_slate();
                Ok(())
            }
        }
    }

    fn give(self: &Arc<Self>, saved: &[Credentials]) -> anyhow::Result<()> {
        *lock(&self.saved) = saved.to_vec();
        if self.power() == Power::Off {
            return Ok(());
        }
        if self.backend() == Backend::Kindil {
            // Still on one of them? The keeper looks, and moves if not.
            self.kick(Kick::Saved);
            return Ok(());
        }
        let wpa = wifi::open(INTERFACE)?;
        wpa.load(saved)?;
        wpa.command("REASSOCIATE").map(drop)
    }

    fn join(self: &Arc<Self>, candidate: &[Credentials]) -> anyhow::Result<()> {
        let ssid = candidate
            .first()
            .map(|c| c.ssid().as_str().to_owned())
            .ok_or_else(|| anyhow::anyhow!("no network to join"))?;
        match self.power() {
            Power::Off => self.switch(Power::On, candidate)?,
            Power::On => self.give(candidate)?,
        }
        if self.backend() == Backend::Kindil && self.join_own(&candidate[0])? {
            return renew_dhcp();
        }
        // wpa_supplicant, or kindil's own Wi-Fi fell back to it (with
        // `candidate` loaded).
        let since = Instant::now();
        let wpa = wifi::open(INTERFACE)?;
        if let Some(id) = wpa
            .command("LIST_NETWORKS")
            .ok()
            .and_then(|list| network_id(&list, &ssid))
        {
            // Join this one now, not just whenever it wins.
            wpa.command(&format!("SELECT_NETWORK {id}"))?;
            wpa.command("ENABLE_NETWORK all")?;
        }
        while since.elapsed() < TRY_WAIT {
            if matches!(wpa.link(), Ok(Link::Associated { ssid: ref joined, .. }) if *joined == ssid)
            {
                return renew_dhcp();
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        anyhow::bail!("couldn't join {ssid}: check the password and that it is in range")
    }

    /// Joins `credentials` with kindil's own Wi-Fi: Ok(true) once joined,
    /// Ok(false) if the driver failed and it fell back to wpa_supplicant.
    fn join_own(self: &Arc<Self>, credentials: &Credentials) -> anyhow::Result<bool> {
        let socket = wext::Socket::open(INTERFACE)?;
        let recent = lock(&self.seen)
            .as_ref()
            .filter(|(when, _)| when.elapsed() < SCAN_FRESH)
            .map(|(_, seen)| seen.clone());
        let seen = match recent {
            Some(seen) => seen,
            None => match station::scan(&socket, &std::thread::sleep) {
                Ok(seen) => seen,
                Err(error) => {
                    self.fall_back(&format!("scanning: {error}"));
                    return Ok(false);
                }
            },
        };
        let target = match station::choose(std::slice::from_ref(credentials), &seen) {
            station::Choice::Join(target) | station::Choice::Blind(target) => target,
            station::Choice::Unsupported { ssid, why } => anyhow::bail!(
                "can't join {ssid}: {why} (remove {} to use wpa_supplicant)",
                station::SWITCH
            ),
            station::Choice::Nothing => anyhow::bail!("no network to join"),
        };
        let epoch = Instant::now();
        let joined = station::join(
            &socket,
            &target,
            TRY_WAIT,
            &|| epoch.elapsed(),
            &std::thread::sleep,
        );
        match joined {
            Ok(true) => Ok(true),
            Ok(false) => anyhow::bail!(
                "couldn't join {}: check the password and that it is in range",
                target.ssid()
            ),
            Err(error) => {
                self.fall_back(&format!("joining {}: {error}", target.ssid()));
                Ok(false)
            }
        }
    }

    /// Gives up on kindil's own Wi-Fi for the run: its driver state
    /// cleared (by the stop in `start`), wpa_supplicant started with the
    /// saved networks.
    fn fall_back(self: &Arc<Self>, why: &str) {
        eprintln!(
            "kindil: kindil's own Wi-Fi gives up ({why}); wpa_supplicant for the rest of this run"
        );
        self.fell_back.store(true, Ordering::SeqCst);
        let saved = lock(&self.saved).clone();
        if let Err(error) = self.start(&saved) {
            eprintln!("kindil: Wi-Fi with wpa_supplicant: {error}");
        }
    }

    /// Wakes the keeper; a resume outranks a change of networks.
    fn kick(&self, kick: Kick) {
        let mut pending = lock(&self.kick);
        if *pending != Some(Kick::Resumed) {
            *pending = Some(kick);
        }
        self.kicked.notify_all();
    }

    /// kindil's whole stack up, with `saved` loaded; On only if it all came up.
    fn start(self: &Arc<Self>, saved: &[Credentials]) -> anyhow::Result<()> {
        self.stop();
        // From a clean slate, whoever left what behind: a wifid, Amazon's
        // supplicant or a stray one holding the control socket would fight
        // kindil's own.
        clear_slate();
        *lock(&self.saved) = saved.to_vec();
        let switch = std::fs::read_to_string(station::SWITCH).ok();
        let chosen = backend(switch.as_deref(), self.fell_back.load(Ordering::SeqCst));
        if chosen == Backend::Kindil {
            eprintln!(
                "kindil: Wi-Fi: kindil's own, without wpa_supplicant ({})",
                station::SWITCH
            );
        }
        *lock(&self.backend) = chosen;
        *lock(&self.kick) = None;
        let result = self.bring_up();
        if result.is_err() {
            self.stop();
        }
        result
    }

    fn bring_up(self: &Arc<Self>) -> anyhow::Result<()> {
        load_driver()?;
        link_hook()?;
        let generation = self.generation.load(Ordering::SeqCst);
        match self.backend() {
            Backend::Supplicant => self.bring_up_supplicant(generation)?,
            Backend::Kindil => {
                let socket = wext::Socket::open(INTERFACE)?;
                socket.set_up()?;
                let keep = Keep {
                    net: Arc::clone(self),
                    generation,
                    socket,
                    epoch: Instant::now(),
                };
                std::thread::spawn(move || station::keep(&keep));
            }
        }
        self.supervise("udhcpc", generation, |_| spawn_dhcp())?;
        *lock(&self.power) = Power::On;
        Ok(())
    }

    fn bring_up_supplicant(self: &Arc<Self>, generation: u64) -> anyhow::Result<()> {
        install_supplicant()?;
        std::fs::write(WPA_CONF, WPA_CONF_TEXT)?;
        self.supervise("wpa_supplicant", generation, |net| {
            let mut child = spawn_supplicant()?;
            let loaded = wifi::open(INTERFACE).and_then(|wpa| {
                wpa.load(&lock(&net.saved))?;
                wpa.command("REASSOCIATE").map(drop)
            });
            if let Err(error) = loaded {
                // Without its networks it is no use; never leave it behind.
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
            Ok(child)
        })
    }

    /// Starts a child with `spawn`, and again whenever it exits while this
    /// stack (`generation`) is still wanted. A restart takes `lifecycle`,
    /// so it never lands after a stop.
    fn supervise(
        self: &Arc<Self>,
        name: &'static str,
        generation: u64,
        spawn: fn(&Net) -> anyhow::Result<Child>,
    ) -> anyhow::Result<()> {
        let mut child = spawn(self)?;
        let net = Arc::clone(self);
        std::thread::spawn(move || {
            let wanted = |net: &Net| net.generation.load(Ordering::SeqCst) == generation;
            let mut pause = PAUSE_MIN;
            loop {
                let since = Instant::now();
                let status = child.wait();
                if !wanted(&net) {
                    return;
                }
                pause = if since.elapsed() >= HEALTHY {
                    PAUSE_MIN
                } else {
                    (pause * 2).min(PAUSE_MAX)
                };
                eprintln!("kindil: {name} exited ({status:?}); starting it again in {pause:?}");
                loop {
                    std::thread::sleep(pause);
                    let _transition = lock(&net.lifecycle);
                    if !wanted(&net) {
                        return;
                    }
                    match spawn(&net) {
                        Ok(next) => {
                            child = next;
                            break;
                        }
                        Err(error) => {
                            eprintln!("kindil: {name}: {error}");
                            pause = (pause * 2).min(PAUSE_MAX);
                        }
                    }
                }
            }
        });
        Ok(())
    }

    /// kindil's stack down; never fails halfway into a state On can't
    /// recover from.
    fn stop(&self) {
        // Its children now exit for good, and its keeper stops.
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.kicked.notify_all();
        *lock(&self.power) = Power::Off;
        terminate(DHCP_PID);
        terminate(WPA_PID);
        if self.backend() == Backend::Kindil {
            // Nothing does it for kindil's own Wi-Fi: leave the network and
            // stop the firmware chasing it.
            if let Err(error) = wext::Socket::open(INTERFACE)
                .map_err(|error| station::DriverError {
                    step: "socket",
                    error,
                })
                .and_then(|socket| station::disconnect(&socket))
            {
                eprintln!("kindil: Wi-Fi: leaving the network: {error}");
            }
        }
        // Nothing to take down the first time (no driver yet): quietly.
        let quiet = |command: &mut Command| {
            let _ = command
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        };
        // The driver stays: unloading ar6003 can hang for good (Amazon wraps
        // it in a reboot-on-hang guard), which hung kindil's Wi-Fi with it.
        // Down, the interface is idle, and a suspend powers the chip off.
        quiet(Command::new("/sbin/ifconfig").args([INTERFACE, "down"]));
    }
}

/// What kindil's own Wi-Fi's keeper (src/station.rs) runs in: this stack,
/// while it is the one wanted.
struct Keep {
    net: Arc<Net>,
    generation: u64,
    socket: wext::Socket,
    epoch: Instant,
}

impl Keep {
    fn wanted(&self) -> bool {
        self.net.generation.load(Ordering::SeqCst) == self.generation
    }
}

impl station::Context for Keep {
    type D = wext::Socket;

    fn now(&self) -> Duration {
        self.epoch.elapsed()
    }

    fn sleep(&self, pause: Duration) {
        std::thread::sleep(pause);
    }

    fn wait(&self, pause: Duration) -> Option<Kick> {
        let pending = lock(&self.net.kick);
        let (mut pending, _) = self
            .net
            .kicked
            .wait_timeout_while(pending, pause, |kick| kick.is_none() && self.wanted())
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        pending.take()
    }

    fn with<R>(&self, f: impl FnOnce(&wext::Socket, &[Credentials]) -> R) -> Option<R> {
        let _transition = lock(&self.net.lifecycle);
        if !self.wanted() {
            return None;
        }
        let saved = lock(&self.net.saved).clone();
        Some(f(&self.socket, &saved))
    }

    fn joined(&self, _ssid: &str) {
        if let Err(error) = renew_dhcp() {
            eprintln!("kindil: Wi-Fi: renewing the address: {error}");
        }
        self.net.joins.fetch_add(1, Ordering::SeqCst);
    }

    fn fall_back(&self, why: &str) {
        let _transition = lock(&self.net.lifecycle);
        if self.wanted() {
            self.net.fall_back(why);
        }
    }
}

/// kindil's own Wi-Fi's link: the driver's access point and ESSID, the
/// interface's address and the level in `/proc/net/wireless`.
fn own_link() -> anyhow::Result<(Link, Option<Signal>)> {
    let socket = wext::Socket::open(INTERFACE)?;
    if socket.bssid()?.is_none() {
        return Ok((Link::NotAssociated, None));
    }
    let ssid = String::from_utf8_lossy(&socket.essid()?).into_owned();
    let signal = std::fs::read_to_string("/proc/net/wireless")
        .ok()
        .and_then(|text| wext::parse_proc_level(&text, INTERFACE))
        .map(Signal);
    Ok((
        Link::Associated {
            ssid,
            ip: socket.ipv4(),
        },
        signal,
    ))
}

/// Amazon's Wi-Fi jobs, if anything started them, and any wpa_supplicant or
/// udhcpc that isn't kindil's (kindil's own are stopped before this runs).
fn clear_slate() {
    for job in ["wifid", "wifis"] {
        if running(job) {
            let _ = run(Command::new("/sbin/stop").arg(job));
        }
    }
    for stray in ["wpa_supplicant", "udhcpc"] {
        let _ = Command::new("killall")
            .arg(stray)
            .stderr(std::process::Stdio::null())
            .status();
    }
    // A dead supplicant's socket: the new one would think it in use.
    let _ = std::fs::remove_file(format!("/var/run/wpa_supplicant/{INTERFACE}"));
}

/// Starting or running, by upstart's goal.
fn running(job: &str) -> bool {
    Command::new("/sbin/status")
        .arg(job)
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains("start/"))
}

fn mac() -> Option<String> {
    let address = std::fs::read_to_string(format!("/sys/class/net/{INTERFACE}/address")).ok()?;
    Some(address.trim().to_owned())
}

/// The ar6003 driver as Amazon's wifim job loads it on a PW1, waited on
/// until its firmware is up and the interface has its MAC.
fn load_driver() -> anyhow::Result<()> {
    if mac().is_some_and(|mac| mac != NO_MAC) {
        return Ok(());
    }
    let board = board_file()?;
    run(Command::new("/sbin/modprobe").args([
        "ar6003",
        &format!("board_data_file={board}"),
        "bmienable=1",
        "wlaninitmode=3",
        // yoshime3 boards have two antennas.
        "enable_diversity=1",
    ]))?;
    let since = Instant::now();
    while !mac().is_some_and(|mac| mac != NO_MAC) {
        anyhow::ensure!(
            since.elapsed() < STARTUP_WAIT,
            "the Wi-Fi driver didn't come up"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(())
}

/// `<BOARD_DATA>/<platform>/<board>/wfo/...`: the one the PW1 ships.
fn board_file() -> anyhow::Result<String> {
    let platforms = std::fs::read_dir(BOARD_DATA)?.flatten();
    platforms
        .flat_map(|platform| {
            std::fs::read_dir(platform.path())
                .into_iter()
                .flatten()
                .flatten()
        })
        .map(|board| board.path().join(BOARD_FILE))
        .find(|path| path.is_file())
        .map(|path| path.display().to_string())
        .ok_or_else(|| anyhow::anyhow!("no Wi-Fi calibration data under {BOARD_DATA}"))
}

fn pid(file: &str) -> Option<i32> {
    std::fs::read_to_string(file).ok()?.trim().parse().ok()
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Stops a process kindil started and confirms it is gone: TERM, then KILL
/// after a few seconds, so nothing of an old stack overlaps a new one.
pub(crate) fn terminate(file: &str) {
    if let Some(pid) = pid(file).filter(|&pid| alive(pid)) {
        let gone_within = |signal, wait: Duration| {
            // SAFETY: plain kill(2) on a process kindil started.
            unsafe { libc::kill(pid, signal) };
            let since = Instant::now();
            while alive(pid) && since.elapsed() < wait {
                std::thread::sleep(Duration::from_millis(50));
            }
            !alive(pid)
        };
        if !gone_within(libc::SIGTERM, Duration::from_secs(3))
            && !gone_within(libc::SIGKILL, Duration::from_secs(2))
        {
            eprintln!("kindil: process {pid} ({file}) won't exit");
        }
    }
    let _ = std::fs::remove_file(file);
}

/// Points the hook's link at the running binary.
fn link_hook() -> anyhow::Result<()> {
    let binary = std::env::current_exe()?;
    if std::fs::read_link(DHCP_HOOK).ok().as_deref() == Some(binary.as_path()) {
        return Ok(());
    }
    let _ = std::fs::remove_file(DHCP_HOOK);
    std::os::unix::fs::symlink(&binary, DHCP_HOOK)?;
    Ok(())
}

/// Writes kindil's wpa_supplicant out when it differs from the one there.
fn install_supplicant() -> anyhow::Result<()> {
    anyhow::ensure!(
        !WPA_BINARY.is_empty(),
        "this kindil was built without its wpa_supplicant"
    );
    if std::fs::read(WPA).ok().as_deref() == Some(WPA_BINARY) {
        return Ok(());
    }
    let part = format!("{WPA}.part");
    std::fs::write(&part, WPA_BINARY)?;
    set_executable(&part)?;
    std::fs::rename(&part, WPA)?;
    Ok(())
}

/// wpa_supplicant in the foreground, as kindil's child; returns once it
/// answers on its control socket.
fn spawn_supplicant() -> anyhow::Result<Child> {
    let mut child = Command::new(WPA)
        .args(["-i", INTERFACE, "-D", "wext", "-c", WPA_CONF])
        .stdout(std::process::Stdio::null())
        .spawn()?;
    // wpa_supplicant writes a pid file only when it daemonizes; kindil's
    // own stop and the launcher find it by this one.
    std::fs::write(WPA_PID, child.id().to_string())?;
    let since = Instant::now();
    while wifi::open(INTERFACE).is_err() {
        if let Some(status) = child.try_wait()? {
            anyhow::bail!("wpa_supplicant exited at start ({status})");
        }
        if since.elapsed() >= STARTUP_WAIT {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("wpa_supplicant didn't start");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(child)
}

/// udhcpc in the foreground, as kindil's child: it keeps trying until it
/// gets a lease, then keeps the lease renewed.
fn spawn_dhcp() -> anyhow::Result<Child> {
    Ok(Command::new("/sbin/udhcpc")
        .args([
            "-f", "-i", INTERFACE, "-s", DHCP_HOOK, "-p", DHCP_PID, "-t", "5", "-T", "2",
        ])
        .stdout(std::process::Stdio::null())
        .spawn()?)
}

/// Asks udhcpc to renew the lease now; it was kept through a nap.
fn renew_dhcp() -> anyhow::Result<()> {
    let pid = pid(DHCP_PID)
        .filter(|&pid| alive(pid))
        .ok_or_else(|| anyhow::anyhow!("udhcpc isn't running"))?;
    // SAFETY: plain kill(2) on udhcpc's own pid.
    unsafe { libc::kill(pid, libc::SIGUSR1) };
    Ok(())
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The id of the network named `ssid` in a `LIST_NETWORKS` reply.
pub fn network_id(reply: &str, ssid: &str) -> Option<u32> {
    reply.lines().skip(1).find_map(|line| {
        let mut columns = line.split('\t');
        let id = columns.next()?.trim().parse().ok()?;
        // The PW1's wpa_supplicant shows names in brackets.
        let name = columns.next()?;
        let name = name
            .strip_prefix('[')
            .and_then(|n| n.strip_suffix(']'))
            .unwrap_or(name);
        (name == ssid).then_some(id)
    })
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
    fn finds_a_network_by_name() {
        let reply =
            "network id / ssid / bssid / flags\n0\tHome\tany\t[CURRENT]\n1\tCafe Bar\tany\t\n";
        assert_eq!(network_id(reply, "Home"), Some(0));
        assert_eq!(network_id(reply, "Cafe Bar"), Some(1));
        assert_eq!(network_id(reply, "Cafe"), None);
        assert_eq!(
            network_id("network id / ssid / bssid / flags\n", "Home"),
            None
        );
        let bracketed = "network id / ssid / bssid / flags\n13\t[Tomato]\tany\t[CURRENT]\n";
        assert_eq!(network_id(bracketed, "Tomato"), Some(13));
    }

    #[test]
    fn starts_off_until_its_stack_is_up() {
        assert_eq!(Net::default().power(), Power::Off);
        assert_eq!(Net::default().backend(), Backend::Supplicant);
    }

    #[test]
    fn the_switch_picks_the_backend_until_a_fall_back() {
        assert_eq!(backend(None, false), Backend::Supplicant);
        assert_eq!(backend(Some("kindil\n"), false), Backend::Kindil);
        assert_eq!(backend(Some("kindil\n"), true), Backend::Supplicant);
        assert_eq!(backend(Some("wpa\n"), false), Backend::Supplicant);
    }
}
