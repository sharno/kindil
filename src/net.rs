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
//!
//! Behind another switch ([`dhcpc::SWITCH`] saying `kindil`), each start
//! runs kindil's own DHCP client (src/dhcpc.rs), a thread, in place of
//! udhcpc and its hook, with either Wi-Fi. Its leases prove kindil's own
//! Wi-Fi's joins as udhcpc's markers did; a resume on the same network
//! keeps the lease without a DHCP exchange until T1; Off gives the lease
//! back. If it can't start, udhcpc runs for that start.
use crate::dhcpc;
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
/// How long a Settings scan waits for kindil's own Wi-Fi's keeper, which
/// scans between joins (a join and its lease take up to about 30 s).
const SCAN_LIMIT: Duration = Duration::from_secs(60);
/// How often udhcpc's supervisor looks whether it still runs.
const DHCP_POLL: Duration = Duration::from_millis(500);
/// After a resume's rejoin, the most kindil waits for udhcpc's hook to
/// apply the renewed lease (the default route, the name servers) before
/// anything goes online: the resume took the route with the interface.
const LEASE_APPLIED_WAIT: Duration = Duration::from_secs(8);
/// A wait for the lifecycle lock longer than this is logged.
const SLOW_LOCK: Duration = Duration::from_secs(1);

/// A scan's access points, or why it failed.
type Scanned = Result<Vec<Bss>, String>;

/// What kindil's own Wi-Fi's keeper is asked to do.
#[derive(Debug, Default)]
struct Kicks {
    rejoin: Option<Kick>,
    scan: bool,
}

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
    kick: Mutex<Kicks>,
    kicked: Condvar,
    /// Joins the keeper reported, for a reconnect to wait on.
    joins: AtomicU64,
    /// kindil's own Wi-Fi's last scan, when, and how many so far.
    seen: Mutex<Option<(Instant, Scanned)>>,
    scans: AtomicU64,
    /// The udhcpc that holds a network's lease, as proven by its hook.
    lease_for: Mutex<Option<(u32, String)>>,
    /// The running udhcpc, and since when.
    dhcp: Mutex<Option<(Child, Instant)>>,
    /// kindil's own DHCP client, in udhcpc's place, while it runs.
    client: Mutex<Option<dhcpc::Client>>,
    /// Left the network for a doze or a sleep ([`Net::park`]); a reconnect,
    /// a start, or new networks join again.
    parked: AtomicBool,
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
            kick: Mutex::default(),
            kicked: Condvar::new(),
            joins: AtomicU64::new(0),
            seen: Mutex::new(None),
            scans: AtomicU64::new(0),
            lease_for: Mutex::new(None),
            dhcp: Mutex::new(None),
            client: Mutex::new(None),
            parked: AtomicBool::new(false),
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
        let _transition = self.transition("switching Wi-Fi");
        self.switch(power, saved)
    }

    /// Gives the running Wi-Fi `saved`, kindil's networks, when it is on.
    pub fn load(self: &Arc<Self>, saved: &[Credentials]) -> anyhow::Result<()> {
        let _transition = self.transition("loading the networks");
        self.give(saved)
    }

    /// Tries `candidate` as a new first network: Ok once it has joined it.
    /// Any failure puts `saved` back and is returned. Switches Wi-Fi on if off.
    pub fn try_join(
        self: &Arc<Self>,
        candidate: &[Credentials],
        saved: &[Credentials],
    ) -> anyhow::Result<()> {
        let _transition = self.transition("joining");
        let joined = self.join(candidate);
        if joined.is_err() {
            if let Err(error) = self.give(saved) {
                crate::log!("kindil: Wi-Fi: putting the saved networks back: {error}");
            }
        }
        joined
    }

    /// The lifecycle lock, for `why`; a wait of over [`SLOW_LOCK`] is
    /// logged, naming who waited.
    fn transition(&self, why: &str) -> std::sync::MutexGuard<'_, ()> {
        let since = Instant::now();
        let guard = lock(&self.lifecycle);
        let waited = since.elapsed();
        if waited > SLOW_LOCK {
            crate::log!(
                "kindil: Wi-Fi: {why} waited {:.1} s for another Wi-Fi change",
                waited.as_secs_f64()
            );
        }
        guard
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
        // The keeper scans between joins, so a scan never lands in one.
        let scans = self.scans.load(Ordering::SeqCst);
        self.kick(Kick::Scan);
        let since = Instant::now();
        while self.scans.load(Ordering::SeqCst) == scans {
            anyhow::ensure!(
                self.backend() == Backend::Kindil && self.power() == Power::On,
                "Wi-Fi stopped; try again"
            );
            anyhow::ensure!(
                since.elapsed() < SCAN_LIMIT,
                "Wi-Fi is busy joining; try again"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        match lock(&self.seen).as_ref() {
            Some((_, Ok(seen))) => Ok(station::networks(seen)),
            Some((_, Err(error))) => anyhow::bail!("scanning: {error}"),
            None => anyhow::bail!("no scan"),
        }
    }

    /// Before a suspend: wpa_supplicant leaves the network and stays off it.
    /// Each resume powers the chip up and loads its firmware again, and
    /// wpa_supplicant would then scan and join, only to be cut off by the
    /// next nap a second later. [`Net::reconnect`] joins again when a fetch
    /// is due. kindil's own Wi-Fi (behind its switch) isn't parked yet: its
    /// keeper rejoins by itself.
    pub fn park(self: &Arc<Self>) -> anyhow::Result<()> {
        let _transition = self.transition("parking");
        if self.power() == Power::Off
            || self.backend() != Backend::Supplicant
            || self.parked.load(Ordering::SeqCst)
        {
            return Ok(());
        }
        wifi::open(INTERFACE)?.command("DISCONNECT")?;
        self.parked.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// After a suspend: join again, then renew the address (usually kept),
    /// and return once the renewed lease is applied (route and name
    /// servers), or [`LEASE_APPLIED_WAIT`] has passed: whatever goes online
    /// next finds the network. A supplicant that doesn't answer gets the
    /// whole stack started again. The lifecycle lock isn't held while
    /// waiting for the lease.
    pub fn reconnect(self: &Arc<Self>) -> anyhow::Result<()> {
        let joins = {
            let transition = self.transition("reconnecting");
            // REASSOCIATE (or a restart) undoes a park's DISCONNECT.
            self.parked.store(false, Ordering::SeqCst);
            if self.power() == Power::Off {
                return Ok(());
            }
            if self.backend() == Backend::Supplicant {
                let renewal = self.reconnect_supplicant()?;
                drop(transition);
                if let Some(renewal) = renewal {
                    lease_applied(renewal, LEASE_APPLIED_WAIT);
                }
                return Ok(());
            }
            // The keeper joins again, the PMK again too, and proves it
            // with a renewed lease.
            let joins = self.joins.load(Ordering::SeqCst);
            self.kick(Kick::Resumed);
            joins
        };
        let since = Instant::now();
        while self.joins.load(Ordering::SeqCst) == joins {
            anyhow::ensure!(
                since.elapsed() < JOIN_WAIT + station::LEASE_WAIT,
                "no access point joined"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    }

    /// `reconnect` for wpa_supplicant, with `lifecycle` held: the renewal
    /// asked of udhcpc, if one was.
    fn reconnect_supplicant(self: &Arc<Self>) -> anyhow::Result<Option<Renewal>> {
        let rejoined =
            wifi::open(INTERFACE).and_then(|wpa| wpa.command("REASSOCIATE").map(|_| wpa));
        let Ok(wpa) = rejoined else {
            // Gone or hung (no answer within the socket's timeout).
            crate::log!("kindil: wpa_supplicant isn't answering; starting Wi-Fi again");
            let saved = lock(&self.saved).clone();
            return self.start(&saved).map(|()| None);
        };
        let since = Instant::now();
        while !matches!(wpa.link(), Ok(Link::Associated { .. })) {
            anyhow::ensure!(since.elapsed() < JOIN_WAIT, "no access point joined");
            std::thread::sleep(Duration::from_millis(100));
        }
        self.renew()
    }

    /// After a join or a resume with wpa_supplicant: udhcpc renews the
    /// lease now (the renewal asked of it); kindil's own client keeps it
    /// until T1 if it is for this network, and asks afresh if not.
    fn renew(&self) -> anyhow::Result<Option<Renewal>> {
        match lock(&self.client).as_ref() {
            Some(client) => {
                client.request(current_network());
                Ok(None)
            }
            None => renew_dhcp().map(Some),
        }
    }

    /// Before joining `ssid` for Settings: kindil's own client gives back
    /// the lease of the network it is leaving, while still on it.
    fn leaving_for(&self, ssid: &str) {
        let client = lock(&self.client);
        let Some(client) = client.as_ref().filter(|c| c.lease().is_some()) else {
            return;
        };
        if current_network().is_some_and(|on| on.ssid != ssid) {
            client.give_back();
        }
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
                // Off gives the lease back first, while still on the network.
                self.end(true);
                // Off means no Wi-Fi at all, Amazon's included.
                clear_slate();
                Ok(())
            }
        }
    }

    fn give(self: &Arc<Self>, saved: &[Credentials]) -> anyhow::Result<()> {
        *lock(&self.saved) = saved.to_vec();
        // Its REASSOCIATE joins again.
        self.parked.store(false, Ordering::SeqCst);
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
        if self.power() == Power::On {
            self.leaving_for(&ssid);
        }
        match self.power() {
            Power::Off => self.switch(Power::On, candidate)?,
            Power::On => self.give(candidate)?,
        }
        if self.backend() == Backend::Kindil && self.join_own(&candidate[0])? {
            // Joined with a lease: nothing to renew.
            return Ok(());
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
                return self.renew().map(drop);
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
            .and_then(|(_, seen)| seen.as_ref().ok().cloned());
        let seen = match recent {
            Some(seen) => seen,
            None => match station::scan(&socket, &crate::dhcp::monotonic, &std::thread::sleep) {
                Ok(seen) => seen,
                Err(error) => {
                    self.fall_back(&format!("scanning: {error}"));
                    return Ok(false);
                }
            },
        };
        let target = match station::choose(std::slice::from_ref(credentials), &seen) {
            station::Choice::Join(target) => target,
            // Unseen (hidden, maybe): the usual WPA2 with CCMP.
            station::Choice::Unseen => station::Target::blind(credentials, wext::Cipher::Ccmp),
            station::Choice::Unsupported { ssid, why } => anyhow::bail!(
                "can't join {ssid}: {why} (remove {} to use wpa_supplicant)",
                station::SWITCH
            ),
            station::Choice::Nothing => anyhow::bail!("no network to join"),
        };
        let joined = station::join(
            &socket,
            &Locked(self),
            &target,
            TRY_WAIT,
            &crate::dhcp::monotonic,
            &std::thread::sleep,
        );
        match joined {
            Ok(station::Joined::Yes) => Ok(true),
            Ok(station::Joined::NotAssociated) => anyhow::bail!(
                "couldn't join {}: check the password and that it is in range",
                target.ssid()
            ),
            Ok(station::Joined::NoLease) => anyhow::bail!(
                "couldn't join {}: check the password (joined, but got no address)",
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
        crate::log!(
            "kindil: kindil's own Wi-Fi gives up ({why}); wpa_supplicant for the rest of this run"
        );
        self.fell_back.store(true, Ordering::SeqCst);
        let saved = lock(&self.saved).clone();
        if let Err(error) = self.start(&saved) {
            crate::log!("kindil: Wi-Fi with wpa_supplicant: {error}; trying again");
            self.keep_starting();
        }
    }

    /// After a failed start of the fallback: starts again, with growing
    /// pauses, until it comes up, or Wi-Fi is switched off or started anew
    /// meanwhile (a newer generation).
    fn keep_starting(self: &Arc<Self>) {
        let net = Arc::clone(self);
        let mut generation = self.generation.load(Ordering::SeqCst);
        std::thread::spawn(move || {
            retry_start(&std::thread::sleep, &mut || {
                let _transition = net.transition("restarting");
                if net.generation.load(Ordering::SeqCst) != generation
                    || Net::wanted() == Power::Off
                {
                    return None;
                }
                let saved = lock(&net.saved).clone();
                let started = net.start(&saved);
                // A failed start stopped its stack: the next generation.
                generation = net.generation.load(Ordering::SeqCst);
                Some(started)
            });
        });
    }

    /// Wakes the keeper; a resume outranks a change of networks.
    fn kick(&self, kick: Kick) {
        let mut pending = lock(&self.kick);
        match kick {
            Kick::Scan => pending.scan = true,
            _ if pending.rejoin == Some(Kick::Resumed) => {}
            _ => pending.rejoin = Some(kick),
        }
        self.kicked.notify_all();
    }

    /// kindil's whole stack up, with `saved` loaded; On only if it all came up.
    fn start(self: &Arc<Self>, saved: &[Credentials]) -> anyhow::Result<()> {
        self.stop();
        self.parked.store(false, Ordering::SeqCst);
        // From a clean slate, whoever left what behind: a wifid, Amazon's
        // supplicant or a stray one holding the control socket would fight
        // kindil's own.
        clear_slate();
        *lock(&self.saved) = saved.to_vec();
        let switch = std::fs::read_to_string(station::SWITCH).ok();
        let chosen = backend(switch.as_deref(), self.fell_back.load(Ordering::SeqCst));
        if chosen == Backend::Kindil {
            crate::log!(
                "kindil: Wi-Fi: kindil's own, without wpa_supplicant ({})",
                station::SWITCH
            );
        }
        *lock(&self.backend) = chosen;
        *lock(&self.kick) = Kicks::default();
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
                };
                std::thread::spawn(move || station::keep(&keep));
            }
        }
        let switch = std::fs::read_to_string(dhcpc::SWITCH).ok();
        let started = dhcpc::chosen(switch.as_deref()).then(|| {
            dhcpc::Client::start(INTERFACE, std::path::Path::new(dhcpc::RESOLV)).inspect_err(
                |error| crate::log!("kindil: DHCP: kindil's own client: {error}; udhcpc instead"),
            )
        });
        match started {
            Some(Ok(client)) => {
                crate::log!("kindil: DHCP: kindil's own client ({})", dhcpc::SWITCH);
                if self.backend() == Backend::Supplicant {
                    // wpa_supplicant joins by itself: ask at once, as udhcpc
                    // would; a join or a resume asks again.
                    client.request(None);
                }
                *lock(&self.client) = Some(client);
            }
            _ => self.supervise_dhcp(generation)?,
        }
        *lock(&self.power) = Power::On;
        Ok(())
    }

    fn bring_up_supplicant(self: &Arc<Self>, generation: u64) -> anyhow::Result<()> {
        install_supplicant()?;
        crate::ssh::write_new(
            std::path::Path::new(WPA_CONF),
            WPA_CONF_TEXT.as_bytes(),
            0o644,
        )?;
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
                crate::log!("kindil: {name} exited ({status:?}); starting it again in {pause:?}");
                let mut gone = Gone::default();
                loop {
                    std::thread::sleep(pause);
                    let _transition = net.transition("restarting wpa_supplicant");
                    if !wanted(&net) {
                        return;
                    }
                    if gone.waits(name, interface_present()) {
                        continue;
                    }
                    match spawn(&net) {
                        Ok(next) => {
                            child = next;
                            break;
                        }
                        Err(error) => {
                            crate::log!("kindil: {name}: {error}");
                            pause = (pause * 2).min(PAUSE_MAX);
                        }
                    }
                }
            }
        });
        Ok(())
    }

    /// udhcpc, as a child polled every half second rather than waited on,
    /// so kindil's own Wi-Fi can replace it ([`Net::restart_dhcp`]): started
    /// again whenever it exits unasked while this stack (`generation`) is
    /// wanted, as [`Net::supervise`] does.
    fn supervise_dhcp(self: &Arc<Self>, generation: u64) -> anyhow::Result<()> {
        *lock(&self.dhcp) = Some((spawn_dhcp()?, Instant::now()));
        let net = Arc::clone(self);
        std::thread::spawn(move || {
            let wanted = |net: &Net| net.generation.load(Ordering::SeqCst) == generation;
            let mut pause = PAUSE_MIN;
            loop {
                std::thread::sleep(DHCP_POLL);
                if !wanted(&net) {
                    return;
                }
                let exited = match lock(&net.dhcp).as_mut() {
                    Some((child, since)) => child
                        .try_wait()
                        .ok()
                        .flatten()
                        .map(|status| (status, since.elapsed())),
                    None => None,
                };
                let Some((status, lived)) = exited else {
                    continue;
                };
                pause = if lived >= HEALTHY {
                    PAUSE_MIN
                } else {
                    (pause * 2).min(PAUSE_MAX)
                };
                crate::log!("kindil: udhcpc exited ({status}); starting it again in {pause:?}");
                let mut gone = Gone::default();
                loop {
                    std::thread::sleep(pause);
                    let _transition = net.transition("restarting udhcpc");
                    if !wanted(&net) {
                        return;
                    }
                    if gone.waits("udhcpc", interface_present()) {
                        continue;
                    }
                    let mut slot = lock(&net.dhcp);
                    // Replaced meanwhile by kindil's own Wi-Fi.
                    if slot
                        .as_mut()
                        .is_some_and(|(child, _)| matches!(child.try_wait(), Ok(None)))
                    {
                        break;
                    }
                    match spawn_dhcp() {
                        Ok(child) => {
                            *slot = Some((child, Instant::now()));
                            break;
                        }
                        Err(error) => {
                            crate::log!("kindil: udhcpc: {error}");
                            pause = (pause * 2).min(PAUSE_MAX);
                        }
                    }
                }
            }
        });
        Ok(())
    }

    /// The running udhcpc's pid.
    fn dhcp_pid(&self) -> Option<u32> {
        let mut slot = lock(&self.dhcp);
        let (child, _) = slot.as_mut()?;
        matches!(child.try_wait(), Ok(None)).then(|| child.id())
    }

    /// A fresh udhcpc in place of the running one (ended and waited for
    /// first), under the current generation; its pid. `lifecycle` held.
    fn restart_dhcp(&self) -> anyhow::Result<u32> {
        let mut slot = lock(&self.dhcp);
        if let Some((mut child, _)) = slot.take() {
            end(&mut child);
        }
        let _ = std::fs::remove_file(DHCP_PID);
        let child = spawn_dhcp()?;
        let pid = child.id();
        *slot = Some((child, Instant::now()));
        crate::dhcp::clear_markers(Some(pid));
        Ok(pid)
    }

    /// For kindil's own Wi-Fi, `lifecycle` held: a lease for `ssid` on
    /// `bssid`. kindil's own client answers by the request's number; else
    /// the udhcpc that holds it renews, or a fresh udhcpc asks.
    fn ask_lease(&self, ssid: &str, bssid: [u8; 6]) -> anyhow::Result<station::Ticket> {
        let since = crate::dhcp::monotonic();
        if let Some(client) = lock(&self.client).as_ref() {
            let ask = client.request(Some(dhcpc::machine::Network {
                ssid: ssid.to_owned(),
                bssid,
            }));
            return Ok(station::Ticket {
                pid: ask,
                since,
                renew: false,
            });
        }
        let pid = self.dhcp_pid();
        let held = lock(&self.lease_for).clone();
        if let (Some(pid), Some((holder, network))) = (pid, held) {
            // SAFETY: plain kill(2) on kindil's own udhcpc child.
            if holder == pid
                && network == ssid
                && unsafe { libc::kill(pid as i32, libc::SIGUSR1) } == 0
            {
                return Ok(station::Ticket {
                    pid,
                    since,
                    renew: true,
                });
            }
        }
        *lock(&self.lease_for) = None;
        let pid = self.restart_dhcp()?;
        Ok(station::Ticket {
            pid,
            since,
            renew: false,
        })
    }

    /// The lease answering ticket `pid`, from kindil's own client or from
    /// udhcpc `pid`'s hook.
    fn marker(&self, pid: u32) -> Option<crate::dhcp::Marker> {
        match lock(&self.client).as_ref() {
            Some(client) => client.marker(pid),
            None => crate::dhcp::marker(pid),
        }
    }

    /// kindil's stack down; never fails halfway into a state On can't
    /// recover from.
    fn stop(&self) {
        self.end(false);
    }

    /// `stop`, giving the lease back first if `release`.
    fn end(&self, release: bool) {
        // Its children now exit for good, and its keeper stops.
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.kicked.notify_all();
        *lock(&self.power) = Power::Off;
        if let Some(client) = lock(&self.client).take() {
            if release {
                client.release();
            } else {
                client.stop();
            }
        }
        if let Some((mut child, _)) = lock(&self.dhcp).take() {
            end(&mut child);
        }
        *lock(&self.lease_for) = None;
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
                crate::log!("kindil: Wi-Fi: leaving the network: {error}");
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
}

impl Keep {
    fn wanted(&self) -> bool {
        self.net.generation.load(Ordering::SeqCst) == self.generation
    }

    /// `f` under the lifecycle lock, while this keeper's stack is the one
    /// running.
    fn guarded<R>(&self, f: impl FnOnce(&Arc<Net>) -> R) -> Option<R> {
        let _transition = self.net.transition("the keeper");
        self.wanted().then(|| f(&self.net))
    }
}

impl station::Context for Keep {
    type D = wext::Socket;

    fn now(&self) -> Duration {
        crate::dhcp::monotonic()
    }

    fn sleep(&self, pause: Duration) {
        std::thread::sleep(pause);
    }

    fn wait(&self, pause: Duration) -> Option<Kick> {
        let pending = lock(&self.net.kick);
        let (mut pending, _) = self
            .net
            .kicked
            .wait_timeout_while(pending, pause, |kicks| {
                kicks.rejoin.is_none() && !kicks.scan && self.wanted()
            })
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // A newer stack's kicks are its keeper's.
        if !self.wanted() {
            return None;
        }
        if std::mem::take(&mut pending.scan) {
            return Some(Kick::Scan);
        }
        pending.rejoin.take()
    }

    fn with<R>(&self, f: impl FnOnce(&wext::Socket, &[Credentials]) -> R) -> Option<R> {
        let _transition = self.net.transition("the keeper");
        if !self.wanted() {
            return None;
        }
        // Borrowed, not copied: no passphrase copies per poll.
        let saved = lock(&self.net.saved);
        Some(f(&self.socket, &saved))
    }

    fn joined(&self, _ssid: &str) -> Option<()> {
        self.guarded(|net| {
            net.joins.fetch_add(1, Ordering::SeqCst);
        })
    }

    fn scanned(&self, result: Result<Vec<Bss>, String>) -> Option<()> {
        self.guarded(|net| {
            *lock(&net.seen) = Some((Instant::now(), result));
            net.scans.fetch_add(1, Ordering::SeqCst);
        })
    }

    fn fall_back(&self, why: &str) {
        self.guarded(|net| net.fall_back(why));
    }
}

impl station::Dhcp for Keep {
    fn ask(&self, ssid: &str, bssid: [u8; 6]) -> Option<Result<station::Ticket, String>> {
        self.guarded(|net| net.ask_lease(ssid, bssid).map_err(|e| format!("{e:#}")))
    }

    fn marker(&self, pid: u32) -> Option<crate::dhcp::Marker> {
        self.net.marker(pid)
    }

    fn address(&self) -> Option<std::net::Ipv4Addr> {
        self.socket.ipv4()
    }

    fn proven(&self, ticket: &station::Ticket, ssid: &str) -> Option<()> {
        self.guarded(|net| *lock(&net.lease_for) = Some((ticket.pid, ssid.to_owned())))
    }
}

/// udhcpc for a Settings join, which holds `lifecycle` throughout.
struct Locked<'a>(&'a Net);

impl station::Dhcp for Locked<'_> {
    fn ask(&self, ssid: &str, bssid: [u8; 6]) -> Option<Result<station::Ticket, String>> {
        Some(self.0.ask_lease(ssid, bssid).map_err(|e| format!("{e:#}")))
    }

    fn marker(&self, pid: u32) -> Option<crate::dhcp::Marker> {
        self.0.marker(pid)
    }

    fn address(&self) -> Option<std::net::Ipv4Addr> {
        wext::Socket::open(INTERFACE).ok()?.ipv4()
    }

    fn proven(&self, ticket: &station::Ticket, ssid: &str) -> Option<()> {
        *lock(&self.0.lease_for) = Some((ticket.pid, ssid.to_owned()));
        Some(())
    }
}

/// Tries `attempt` after each pause (2 s, doubling to a minute) until it
/// succeeds (true) or answers `None`: cancelled (false).
fn retry_start(
    sleep: &dyn Fn(Duration),
    attempt: &mut dyn FnMut() -> Option<anyhow::Result<()>>,
) -> bool {
    let mut backoff = station::Backoff::default();
    loop {
        sleep(backoff.pause());
        match attempt() {
            None => return false,
            Some(Ok(())) => return true,
            Some(Err(error)) => {
                crate::log!("kindil: Wi-Fi with wpa_supplicant: {error}; trying again")
            }
        }
    }
}

/// Ends a child: TERM, then KILL after a few seconds; reaped either way.
fn end(child: &mut Child) {
    // SAFETY: plain kill(2) on kindil's own child.
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    let since = Instant::now();
    while since.elapsed() < Duration::from_secs(3) {
        if !matches!(child.try_wait(), Ok(None)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
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

/// The network the driver is on now, whichever Wi-Fi joined it.
fn current_network() -> Option<dhcpc::machine::Network> {
    let socket = wext::Socket::open(INTERFACE).ok()?;
    let bssid = socket.bssid().ok()??;
    let ssid = String::from_utf8_lossy(&socket.essid().ok()?).into_owned();
    Some(dhcpc::machine::Network { ssid, bssid })
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
            crate::log!("kindil: process {pid} ({file}) won't exit");
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
    // Not through a .part left in its place (src/perms.rs).
    crate::ssh::write_new(std::path::Path::new(WPA), WPA_BINARY, 0o755)?;
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
fn renew_dhcp() -> anyhow::Result<Renewal> {
    let pid = pid(DHCP_PID)
        .filter(|&pid| alive(pid))
        .ok_or_else(|| anyhow::anyhow!("udhcpc isn't running"))?;
    let asked = crate::dhcp::monotonic();
    // SAFETY: plain kill(2) on udhcpc's own pid.
    unsafe { libc::kill(pid, libc::SIGUSR1) };
    Ok(Renewal {
        pid: pid as u32,
        asked,
    })
}

/// A renewal asked of udhcpc `pid` at `asked` ([`crate::dhcp::monotonic`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Renewal {
    pub pid: u32,
    pub asked: Duration,
}

impl Renewal {
    /// Whether `marker`, udhcpc's hook's latest, applied a lease asked for
    /// by this renewal (or after it).
    pub fn applied_by(self, marker: Option<crate::dhcp::Marker>) -> bool {
        marker.is_some_and(|marker| marker.at >= self.asked)
    }
}

/// Waits up to `wait` for udhcpc's hook to apply `renewal`'s lease: it
/// leaves its marker last, after the route and the name servers.
fn lease_applied(renewal: Renewal, wait: Duration) -> bool {
    let since = Instant::now();
    loop {
        if renewal.applied_by(crate::dhcp::marker(renewal.pid)) {
            return true;
        }
        if since.elapsed() >= wait {
            crate::log!(
                "kindil: Wi-Fi: the renewed lease wasn't applied within {wait:?}; going on"
            );
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
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

/// Whether the Wi-Fi interface exists: a child started without it exits at
/// once ("SIOCGIFINDEX: No such device"), over and over.
fn interface_present() -> bool {
    std::path::Path::new("/sys/class/net")
        .join(INTERFACE)
        .exists()
}

/// A supervisor's view of a missing interface: it waits for it to come back
/// instead of restarting a child that can only fail, and says so once.
#[derive(Debug, Default)]
struct Gone {
    said: bool,
}

impl Gone {
    /// True while the interface is missing: skip this restart.
    fn waits(&mut self, child: &str, present: bool) -> bool {
        match (present, self.said) {
            (true, said) => {
                if said {
                    crate::log!("kindil: {INTERFACE} is back; starting {child}");
                }
                self.said = false;
                false
            }
            (false, false) => {
                crate::log!("kindil: {INTERFACE} is gone; {child} waits for it");
                self.said = true;
                true
            }
            (false, true) => true,
        }
    }
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
    fn a_child_waits_for_a_missing_interface_and_says_so_once() {
        let mut gone = Gone::default();
        assert!(!gone.waits("udhcpc", true));
        assert!(gone.waits("udhcpc", false));
        assert!(gone.said);
        assert!(gone.waits("udhcpc", false));
        assert!(!gone.waits("udhcpc", true), "back: started again");
        assert!(!gone.said);
    }

    #[test]
    fn a_stopped_or_own_wifi_is_never_parked() {
        let net = Arc::new(Net::default());
        // Off: nothing to leave, and no supplicant to ask.
        net.park().unwrap();
        assert!(!net.parked.load(Ordering::SeqCst));
        *lock(&net.power) = Power::On;
        *lock(&net.backend) = Backend::Kindil;
        net.park().unwrap();
        assert!(!net.parked.load(Ordering::SeqCst));
    }

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

    fn keeper(net: &Arc<Net>) -> Keep {
        Keep {
            net: Arc::clone(net),
            generation: net.generation.load(Ordering::SeqCst),
            socket: wext::Socket::open("lo").unwrap(),
        }
    }

    #[test]
    fn a_keeper_of_an_older_stack_affects_nothing() {
        use station::{Context, Dhcp};
        let net = Arc::new(Net::default());
        let keep = keeper(&net);
        let ticket = station::Ticket {
            pid: 7,
            since: Duration::ZERO,
            renew: false,
        };
        // Its own stack: effects go through.
        assert_eq!(keep.joined("Home"), Some(()));
        assert_eq!(keep.scanned(Ok(vec![])), Some(()));
        assert_eq!(keep.proven(&ticket, "Home"), Some(()));
        assert_eq!(net.joins.load(Ordering::SeqCst), 1);
        assert_eq!(net.scans.load(Ordering::SeqCst), 1);
        assert!(keep.with(|_, _| ()).is_some());
        // A newer stack (an Off, or a start): nothing more.
        *lock(&net.lease_for) = None;
        net.generation.fetch_add(1, Ordering::SeqCst);
        assert_eq!(keep.joined("Home"), None);
        assert_eq!(keep.scanned(Ok(vec![])), None);
        assert_eq!(keep.proven(&ticket, "Home"), None);
        assert_eq!(keep.ask("Home", [2, 0, 0, 0, 0, 1]), None);
        assert!(keep.with(|_, _| ()).is_none());
        assert_eq!(net.joins.load(Ordering::SeqCst), 1);
        assert_eq!(net.scans.load(Ordering::SeqCst), 1);
        assert_eq!(*lock(&net.lease_for), None);
        assert!(lock(&net.dhcp).is_none(), "no udhcpc started");
        // Nor does it fall back on the newer stack's behalf.
        keep.fall_back("old news");
        assert!(!net.fell_back.load(Ordering::SeqCst));
    }

    #[test]
    fn a_keeper_of_an_older_stack_leaves_the_newer_ones_kicks() {
        use station::Context;
        let net = Arc::new(Net::default());
        let old = keeper(&net);
        net.generation.fetch_add(1, Ordering::SeqCst);
        net.kick(Kick::Saved);
        net.kick(Kick::Scan);
        assert_eq!(old.wait(Duration::ZERO), None);
        assert_eq!(old.wait(Duration::from_millis(20)), None);
        let new = keeper(&net);
        assert_eq!(new.wait(Duration::ZERO), Some(Kick::Scan));
        assert_eq!(new.wait(Duration::ZERO), Some(Kick::Saved));
        assert_eq!(new.wait(Duration::ZERO), None);
        // A resume outranks a change of networks.
        net.kick(Kick::Resumed);
        net.kick(Kick::Saved);
        assert_eq!(new.wait(Duration::ZERO), Some(Kick::Resumed));
    }

    #[test]
    fn a_renewal_is_applied_by_the_hooks_marker_from_after_it() {
        use crate::dhcp::{Leased, Marker};
        let renewal = Renewal {
            pid: 812,
            asked: Duration::from_secs(5_000),
        };
        let marker = |secs| {
            Some(Marker {
                leased: Leased::Renew,
                ip: "192.168.1.130".parse().unwrap(),
                at: Duration::from_secs(secs),
            })
        };
        assert!(!renewal.applied_by(None), "no lease yet");
        assert!(
            !renewal.applied_by(marker(4_000)),
            "the lease before the nap"
        );
        assert!(renewal.applied_by(marker(5_000)));
        assert!(renewal.applied_by(marker(5_001)));
    }

    #[test]
    fn a_stop_wakes_a_waiting_keeper() {
        use station::Context;
        let net = Arc::new(Net::default());
        let keep = keeper(&net);
        let waiter = std::thread::spawn(move || {
            let since = Instant::now();
            let kick = keep.wait(Duration::from_secs(30));
            (kick, since.elapsed())
        });
        std::thread::sleep(Duration::from_millis(50));
        net.generation.fetch_add(1, Ordering::SeqCst);
        net.kicked.notify_all();
        let (kick, waited) = waiter.join().unwrap();
        assert_eq!(kick, None);
        assert!(waited < Duration::from_secs(5), "{waited:?}");
    }

    #[test]
    fn a_failed_fallback_start_is_retried_until_it_comes_up_or_is_cancelled() {
        let slept = std::cell::RefCell::new(Vec::new());
        let sleep = |pause: Duration| slept.borrow_mut().push(pause.as_secs());
        let mut tries = 0;
        let up = retry_start(&sleep, &mut || {
            tries += 1;
            Some(if tries < 3 {
                Err(anyhow::anyhow!("the Wi-Fi driver didn't come up"))
            } else {
                Ok(())
            })
        });
        assert!(up);
        assert_eq!(tries, 3);
        assert_eq!(*slept.borrow(), [2, 4, 8]);
        // Switched off (or started anew) meanwhile: no more tries.
        let mut tries = 0;
        let up = retry_start(&|_| {}, &mut || {
            tries += 1;
            (tries < 2).then(|| Err(anyhow::anyhow!("no")))
        });
        assert!(!up);
        assert_eq!(tries, 2);
    }
}
