//! kindil's own Wi-Fi, without wpa_supplicant: behind a switch, off unless
//! [`SWITCH`] says `kindil`.
//!
//! The PW1's AR6003 firmware does the WPA2-PSK 4-way handshake itself when
//! the driver hands it the PMK (src/pmk.rs) before a connect, so joining is a
//! handful of wireless-extension ioctls (src/wext.rs): "essid off" (which
//! also resets the driver's profile), the WPA2 profile through SIOCSIWAUTH,
//! the PMK, and the ESSID last (every profile change clears the stored one).
//! The driver forgets that the PMK is offloaded after every connect, so the
//! PMK is sent again before every connect.
//!
//! A join counts only once udhcpc took a lease after it ([`Dhcp`]): with
//! the handshake in the firmware, a wrong password can still associate for
//! a few seconds, but no DHCP exchange gets through without the keys. The
//! proof is a marker the kindil-dhcp hook leaves for the udhcpc that runs
//! now (a fresh one for a new network; a renew of the same network's lease
//! keeps it), with its address on the interface, while still on the access
//! point joined ([`proves`]); a late hook of an earlier udhcpc proves
//! nothing.
//!
//! This module is the logic, over a [`Driver`]: which saved network to join
//! from a scan, the ioctl sequence, and [`keep`], the loop that joins, proves
//! it with a lease, watches the access point and joins again, with a growing
//! pause between failures. It also does Settings' scans, between joins, so a
//! scan never lands in a connect. src/net.rs runs it beside udhcpc, under its
//! lifecycle lock, and falls back to wpa_supplicant for the rest of the run
//! when [`keep`] gives up: no join with a lease within [`FALLBACK_AFTER`] on a
//! network in range, every saved network failing [`BLIND_TRIES`] times
//! unseen, a saved network kindil can't join this way, or the driver
//! failing.
use crate::dhcp::{Leased, Marker};
use crate::host::Signal;
use crate::wext::{self, Akm, Bss, Cipher, Driver};
use crate::wifi::{Credentials, Network, Security};
use std::collections::BTreeMap;
use std::io;
use std::net::Ipv4Addr;
use std::time::Duration;

/// `kindil` in it: kindil's own Wi-Fi. Absent or anything else: wpa_supplicant.
pub const SWITCH: &str = "/var/local/kindil/wifi";
/// One try at joining.
pub const ASSOC_WAIT: Duration = Duration::from_secs(10);
/// Trying this long on a network in range without a join with a lease
/// gives up on kindil's own Wi-Fi for the run.
pub const FALLBACK_AFTER: Duration = Duration::from_secs(30);
/// How often the access point is checked while associated.
pub const WATCH: Duration = Duration::from_secs(3);
/// After losing the access point, the firmware keeps trying to rejoin it by
/// itself (any disconnect but the host's own); it gets this long.
pub const REJOIN_GRACE: Duration = Duration::from_secs(10);
/// After associating, a lease has to come within this long for the join
/// to count: a wrong password can associate and then fail the handshake.
pub const LEASE_WAIT: Duration = Duration::from_secs(15);
/// The driver's scan of every channel takes about two seconds.
pub const SCAN_WAIT: Duration = Duration::from_secs(3);
/// Scan results the driver says aren't ready are read again for this long.
pub const SCAN_RETRY: Duration = Duration::from_secs(5);
/// Tries of each saved network unseen (out of sight, or hidden) before,
/// with every one of them failed that often, giving up for the run.
pub const BLIND_TRIES: u32 = 2;
/// "essid off" sends a disconnect; give it a moment before the new profile.
pub const CANCEL_SETTLE: Duration = Duration::from_millis(500);
/// Driver errors in a row before giving up on it.
pub const STRIKES: u32 = 3;
/// With no saved networks: wait this long, or for one to be added.
pub const IDLE: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(250);
const PAUSE_MIN: Duration = Duration::from_secs(2);
const PAUSE_MAX: Duration = Duration::from_secs(60);

/// Whether the switch's content picks kindil's own Wi-Fi.
pub fn chosen(switch: Option<&str>) -> bool {
    switch.is_some_and(|text| text.trim() == "kindil")
}

/// How a network is joined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    Open,
    /// WPA2-PSK, CCMP pairwise, with this group cipher (CCMP or TKIP).
    Wpa2 {
        group: Cipher,
    },
}

/// What an access point offers, as far as kindil's own Wi-Fi goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Open,
    Wpa2 { group: Cipher },
    Unsupported(&'static str),
}

pub fn kind(bss: &Bss) -> Kind {
    if let Some(rsn) = &bss.rsn {
        let psk = rsn.akm.iter().any(|a| matches!(a, Akm::Psk));
        if !psk {
            return Kind::Unsupported(if rsn.akm.contains(&Akm::Sae) {
                "it is WPA3-only, which kindil's own Wi-Fi doesn't support"
            } else {
                "it needs a WPA2-Enterprise (802.1X) login, which kindil's own Wi-Fi doesn't support"
            });
        }
        if !rsn.pairwise.contains(&Cipher::Ccmp) {
            return Kind::Unsupported(
                "it is WPA2 with TKIP only, which kindil's own Wi-Fi doesn't support",
            );
        }
        return match rsn.group {
            group @ (Cipher::Ccmp | Cipher::Tkip) => Kind::Wpa2 { group },
            _ => Kind::Unsupported(
                "its group cipher isn't CCMP or TKIP, which kindil's own Wi-Fi doesn't support",
            ),
        };
    }
    if bss.wpa.is_some() {
        return Kind::Unsupported(
            "it is WPA (TKIP) only, which kindil's own Wi-Fi doesn't support",
        );
    }
    if bss.privacy {
        return Kind::Unsupported("it uses WEP, which kindil's own Wi-Fi doesn't support");
    }
    Kind::Open
}

/// A network to join, and how.
#[derive(Clone, PartialEq, Eq)]
pub struct Target {
    pub credentials: Credentials,
    pub plan: Plan,
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?} {:?}", self.ssid(), self.plan)
    }
}

impl Target {
    pub fn ssid(&self) -> &str {
        self.credentials.ssid().as_str()
    }

    /// Joining without having seen it: WPA2 with CCMP pairwise and the
    /// group cipher given (it can't be known unseen).
    pub fn blind(credentials: &Credentials, group: Cipher) -> Self {
        let plan = match credentials {
            Credentials::Open(_) => Plan::Open,
            Credentials::Wpa(..) => Plan::Wpa2 { group },
        };
        Self {
            credentials: credentials.clone(),
            plan,
        }
    }
}

pub enum Choice {
    /// The most preferred saved network in range that kindil can join.
    Join(Target),
    /// The saved networks in range, none joinable this way: the first.
    Unsupported { ssid: String, why: &'static str },
    /// None of the saved networks in sight (out of range, or hidden).
    Unseen,
    /// No saved networks.
    Nothing,
}

/// The first of `saved` (most preferred first) in `seen` that kindil can
/// join, through its strongest access point.
pub fn choose(saved: &[Credentials], seen: &[Bss]) -> Choice {
    let mut refused = None;
    for credentials in saved {
        let ssid = credentials.ssid().as_str().as_bytes();
        let mut here: Vec<&Bss> = seen.iter().filter(|b| b.ssid == ssid).collect();
        if here.is_empty() {
            continue;
        }
        here.sort_by_key(|b| std::cmp::Reverse(b.level.unwrap_or(i32::MIN)));
        let mut why = None;
        for bss in here {
            let plan = match (kind(bss), credentials) {
                (Kind::Open, Credentials::Open(_)) => Plan::Open,
                (Kind::Wpa2 { group }, Credentials::Wpa(..)) => Plan::Wpa2 { group },
                (Kind::Unsupported(reason), _) => {
                    why.get_or_insert(reason);
                    continue;
                }
                (Kind::Open, Credentials::Wpa(..)) => {
                    why.get_or_insert("it is open now, but saved with a password");
                    continue;
                }
                (Kind::Wpa2 { .. }, Credentials::Open(_)) => {
                    why.get_or_insert("it needs a password now; join it again in Settings");
                    continue;
                }
            };
            return Choice::Join(Target {
                credentials: credentials.clone(),
                plan,
            });
        }
        if refused.is_none() {
            refused = why.map(|why| (credentials.ssid().as_str().to_owned(), why));
        }
    }
    match refused {
        Some((ssid, why)) => Choice::Unsupported { ssid, why },
        None if saved.is_empty() => Choice::Nothing,
        None => Choice::Unseen,
    }
}

/// A driver call that failed, and which.
#[derive(Debug)]
pub struct DriverError {
    pub step: &'static str,
    pub error: io::Error,
}

impl std::fmt::Display for DriverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.step, self.error)
    }
}

impl std::error::Error for DriverError {}

fn at(step: &'static str) -> impl FnOnce(io::Error) -> DriverError {
    move |error| DriverError { step, error }
}

/// The ioctl sequence that joins `target`, proven on the PW1: up, essid
/// off, the WPA2 profile in the order the driver needs (WPA_VERSION before
/// KEY_MGMT), the PMK, and the ESSID last. Returns once asked; association
/// follows within a second or so.
pub fn connect(
    driver: &impl Driver,
    target: &Target,
    sleep: &dyn Fn(Duration),
) -> Result<(), DriverError> {
    driver.set_up().map_err(at("SIOCSIFFLAGS up"))?;
    driver.set_essid(None).map_err(at("SIOCSIWESSID off"))?;
    sleep(CANCEL_SETTLE);
    if let Plan::Wpa2 { group } = target.plan {
        let Credentials::Wpa(_, _) = &target.credentials else {
            return Err(DriverError {
                step: "WPA2 profile",
                error: io::Error::new(io::ErrorKind::InvalidInput, "no password"),
            });
        };
        let password = target.credentials.password().unwrap_or_default();
        let pmk =
            crate::pmk::pmk(password, target.ssid().as_bytes()).map_err(|why| DriverError {
                step: "PMK",
                error: io::Error::new(io::ErrorKind::InvalidInput, why),
            })?;
        let group = match group {
            Cipher::Tkip => wext::IW_AUTH_CIPHER_TKIP,
            _ => wext::IW_AUTH_CIPHER_CCMP,
        };
        let profile = [
            ("SIOCSIWAUTH WPA_ENABLED", wext::IW_AUTH_WPA_ENABLED, 1),
            (
                "SIOCSIWAUTH WPA_VERSION",
                wext::IW_AUTH_WPA_VERSION,
                wext::IW_AUTH_WPA_VERSION_WPA2,
            ),
            (
                "SIOCSIWAUTH CIPHER_PAIRWISE",
                wext::IW_AUTH_CIPHER_PAIRWISE,
                wext::IW_AUTH_CIPHER_CCMP,
            ),
            (
                "SIOCSIWAUTH CIPHER_GROUP",
                wext::IW_AUTH_CIPHER_GROUP,
                group,
            ),
            (
                "SIOCSIWAUTH KEY_MGMT",
                wext::IW_AUTH_KEY_MGMT,
                wext::IW_AUTH_KEY_MGMT_PSK,
            ),
            (
                "SIOCSIWAUTH 80211_AUTH_ALG",
                wext::IW_AUTH_80211_AUTH_ALG,
                wext::IW_AUTH_ALG_OPEN_SYSTEM,
            ),
            (
                "SIOCSIWAUTH DROP_UNENCRYPTED",
                wext::IW_AUTH_DROP_UNENCRYPTED,
                1,
            ),
            (
                "SIOCSIWAUTH PRIVACY_INVOKED",
                wext::IW_AUTH_PRIVACY_INVOKED,
                1,
            ),
        ];
        for (step, index, value) in profile {
            driver.set_auth(index, value).map_err(at(step))?;
        }
        driver.set_pmk(&pmk).map_err(at("SIOCSIWENCODEEXT PMK"))?;
    }
    // Open: "essid off" left the driver's open, unencrypted profile.
    driver
        .set_essid(Some(target.ssid().as_bytes()))
        .map_err(at("SIOCSIWESSID"))
}

/// Leaves the network, stops the firmware chasing it and resets the
/// profile: a disassociation from the current access point, "essid off"
/// and the WPA settings off, each tried whatever the others did; the first
/// error is returned.
pub fn disconnect(driver: &impl Driver) -> Result<(), DriverError> {
    let mut errors: Vec<DriverError> = Vec::new();
    match driver.bssid() {
        Ok(Some(bssid)) => {
            if let Err(error) = driver.disassociate(bssid) {
                errors.push(at("SIOCSIWMLME DISASSOC")(error));
            }
        }
        Ok(None) => {}
        Err(error) => errors.push(at("SIOCGIWAP")(error)),
    }
    if let Err(error) = driver.set_essid(None) {
        errors.push(at("SIOCSIWESSID off")(error));
    }
    let reset = [
        ("SIOCSIWAUTH WPA_ENABLED=0", wext::IW_AUTH_WPA_ENABLED),
        (
            "SIOCSIWAUTH PRIVACY_INVOKED=0",
            wext::IW_AUTH_PRIVACY_INVOKED,
        ),
        (
            "SIOCSIWAUTH DROP_UNENCRYPTED=0",
            wext::IW_AUTH_DROP_UNENCRYPTED,
        ),
    ];
    for (step, index) in reset {
        if let Err(error) = driver.set_auth(index, 0) {
            errors.push(at(step)(error));
        }
    }
    match errors.into_iter().next() {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Starts a scan. Never while a connect is pending: unless associated,
/// "essid off" first stops the firmware chasing a network.
pub fn scan_start(driver: &impl Driver) -> Result<(), DriverError> {
    driver.set_up().map_err(at("SIOCSIFFLAGS up"))?;
    if driver.bssid().map_err(at("SIOCGIWAP"))?.is_none() {
        driver.set_essid(None).map_err(at("SIOCSIWESSID off"))?;
    }
    driver.trigger_scan().map_err(at("SIOCSIWSCAN"))
}

/// The scan's results, `None` while the driver says they aren't ready.
pub fn scan_read(driver: &impl Driver) -> Result<Option<Vec<Bss>>, DriverError> {
    match driver.scan_results() {
        Ok(stream) => Ok(Some(wext::parse_scan(&stream))),
        Err(error) if error.raw_os_error() == Some(libc::EAGAIN) => Ok(None),
        Err(error) => Err(at("SIOCGIWSCAN")(error)),
    }
}

fn scan_timed_out() -> DriverError {
    DriverError {
        step: "SIOCGIWSCAN",
        error: io::Error::new(
            io::ErrorKind::TimedOut,
            format!("no results within {} s", SCAN_RETRY.as_secs()),
        ),
    }
}

/// A whole scan, for a caller that holds the radio throughout: started,
/// [`SCAN_WAIT`], read, results not ready retried for [`SCAN_RETRY`].
pub fn scan(
    driver: &impl Driver,
    now: &dyn Fn() -> Duration,
    sleep: &dyn Fn(Duration),
) -> Result<Vec<Bss>, DriverError> {
    scan_start(driver)?;
    sleep(SCAN_WAIT);
    let deadline = now() + SCAN_RETRY;
    loop {
        if let Some(seen) = scan_read(driver)? {
            return Ok(seen);
        }
        if now() >= deadline {
            return Err(scan_timed_out());
        }
        sleep(POLL);
    }
}

/// A scan as Settings lists it: by name, the strongest access point of
/// each, strongest first, hidden ones left out.
pub fn networks(seen: &[Bss]) -> Vec<Network> {
    let mut strongest: BTreeMap<String, Network> = BTreeMap::new();
    for bss in seen.iter().filter(|b| !b.ssid.is_empty()) {
        let ssid = String::from_utf8_lossy(&bss.ssid).into_owned();
        let signal = Signal(bss.level.unwrap_or(-100));
        let security = match kind(bss) {
            Kind::Open => Security::Open,
            _ => Security::Protected,
        };
        match strongest.get(&ssid) {
            Some(known) if known.signal >= signal => {}
            _ => {
                strongest.insert(ssid.clone(), Network::new(&ssid, signal, security));
            }
        }
    }
    let mut networks: Vec<Network> = strongest.into_values().collect();
    networks.sort_by(|a, b| b.signal.cmp(&a.signal).then_with(|| a.ssid.cmp(&b.ssid)));
    networks
}

/// A request for a lease: the udhcpc that has to answer it (or, with
/// kindil's own DHCP client, the request's number), since when, and
/// whether a renew of a lease it holds may answer too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ticket {
    pub pid: u32,
    pub since: Duration,
    pub renew: bool,
}

/// The address: kindil's udhcpc and its hook (src/dhcp.rs), or kindil's
/// own DHCP client (src/dhcpc.rs). A lease taken
/// after a join proves the join: no DHCP exchange gets through without the
/// keys, while a wrong password can still associate for a few seconds
/// before the access point gives up on the handshake.
///
/// `None` from any of these: the stack that asked is no longer the one
/// running.
pub trait Dhcp {
    /// A lease for `ssid` on `bssid`, now. The udhcpc that holds `ssid`'s
    /// lease (by [`Dhcp::proven`]) is asked to renew it; for any other
    /// network, or without one, udhcpc is started afresh and the ticket
    /// names the new one. kindil's own client numbers each request, and
    /// keeps a lease it holds for this network until T1.
    fn ask(&self, ssid: &str, bssid: [u8; 6]) -> Option<Result<Ticket, String>>;
    /// The marker udhcpc `pid`'s hook left, if any (or the lease kindil's
    /// own client took for request `pid`).
    fn marker(&self, pid: u32) -> Option<Marker>;
    /// The interface's IPv4 address.
    fn address(&self) -> Option<Ipv4Addr>;
    /// `ticket`'s udhcpc holds `ssid`'s lease now.
    fn proven(&self, ticket: &Ticket, ssid: &str) -> Option<()>;
}

/// Whether `marker` answers `ticket`: left by the ticket's udhcpc, a new
/// lease (or, for a renew, a renewed one), taken after the ticket, with
/// its address the one on the interface.
pub fn proves(
    ticket: &Ticket,
    pid: u32,
    marker: Option<Marker>,
    address: Option<Ipv4Addr>,
) -> bool {
    let Some(marker) = marker else {
        return false;
    };
    pid == ticket.pid
        && (marker.leased == Leased::Bound || ticket.renew)
        && marker.at >= ticket.since
        && Some(marker.ip) == address
}

/// Asks for a lease for `ssid` and waits for its proof while the station
/// stays on `bssid` (read through `bssid_now`): Ok(true) once proven within
/// [`LEASE_WAIT`], Ok(false) without (or once the access point is gone),
/// `None` once the stack is gone.
fn prove(
    dhcp: &dyn Dhcp,
    ssid: &str,
    bssid: [u8; 6],
    now: &dyn Fn() -> Duration,
    sleep: &dyn Fn(Duration),
    bssid_now: &mut dyn FnMut() -> Option<Result<Option<[u8; 6]>, DriverError>>,
) -> Option<Result<bool, DriverError>> {
    let ticket = match dhcp.ask(ssid, bssid)? {
        Ok(ticket) => ticket,
        Err(error) => {
            crate::log!("kindil: Wi-Fi: asking for an address: {error}");
            return Some(Ok(false));
        }
    };
    let deadline = ticket.since.max(now()) + LEASE_WAIT;
    loop {
        match bssid_now()? {
            Err(error) => return Some(Err(error)),
            Ok(on) if on != Some(bssid) => return Some(Ok(false)),
            Ok(_) => {}
        }
        if proves(&ticket, ticket.pid, dhcp.marker(ticket.pid), dhcp.address()) {
            dhcp.proven(&ticket, ssid)?;
            return Some(Ok(true));
        }
        if now() >= deadline {
            return Some(Ok(false));
        }
        sleep(POLL);
    }
}

/// How a join for Settings went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Joined {
    Yes,
    /// No association: a wrong password, or out of range.
    NotAssociated,
    /// Associated, but no lease within [`LEASE_WAIT`]: most likely a wrong
    /// password. Left again.
    NoLease,
}

/// Joins `target` now, for Settings: associated within `limit`, then a
/// lease from udhcpc proven within [`LEASE_WAIT`]. The caller holds the
/// radio throughout.
pub fn join(
    driver: &impl Driver,
    dhcp: &dyn Dhcp,
    target: &Target,
    limit: Duration,
    now: &dyn Fn() -> Duration,
    sleep: &dyn Fn(Duration),
) -> Result<Joined, DriverError> {
    let started = now();
    connect(driver, target, sleep)?;
    let bssid = loop {
        if let Some(bssid) = driver.bssid().map_err(at("SIOCGIWAP"))? {
            if driver.essid().map_err(at("SIOCGIWESSID"))? == target.ssid().as_bytes() {
                break bssid;
            }
        }
        if now() - started >= limit {
            disconnect(driver)?;
            return Ok(Joined::NotAssociated);
        }
        sleep(POLL);
    };
    let mut bssid_now = || Some(driver.bssid().map_err(at("SIOCGIWAP")));
    match prove(dhcp, target.ssid(), bssid, now, sleep, &mut bssid_now) {
        Some(Ok(true)) => Ok(Joined::Yes),
        Some(Err(error)) => Err(error),
        Some(Ok(false)) | None => {
            disconnect(driver)?;
            Ok(Joined::NoLease)
        }
    }
}

/// The pause before the next try: doubling from 2 s to a minute.
#[derive(Debug)]
pub struct Backoff {
    next: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self { next: PAUSE_MIN }
    }
}

impl Backoff {
    pub fn pause(&mut self) -> Duration {
        let pause = self.next;
        self.next = (self.next * 2).min(PAUSE_MAX);
        pause
    }

    pub fn reset(&mut self) {
        self.next = PAUSE_MIN;
    }
}

/// Why [`keep`] should act now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kick {
    /// The saved networks changed: still on one of them?
    Saved,
    /// Back from a suspend: join again (the PMK again too).
    Resumed,
    /// Settings wants a scan: done between joins, never during one.
    Scan,
}

/// What [`keep`] runs in: src/net.rs's stack, or the tests' fake. Every
/// effect answers `None` once this keeper's stack is no longer the one
/// running, checked under the lifecycle lock; the keeper then stops.
pub trait Context: Dhcp {
    type D: Driver;
    /// Monotonic time, the clock the DHCP markers are on.
    fn now(&self) -> Duration;
    fn sleep(&self, pause: Duration);
    /// Waits up to `pause`, or until kicked; takes the kick, only while
    /// this keeper's stack is the one running. A scan comes before a
    /// rejoin.
    fn wait(&self, pause: Duration) -> Option<Kick>;
    /// `f` with the driver and the saved networks (borrowed, not copied),
    /// under the lifecycle lock.
    fn with<R>(&self, f: impl FnOnce(&Self::D, &[Credentials]) -> R) -> Option<R>;
    /// Joined, with a lease, after not being.
    fn joined(&self, ssid: &str) -> Option<()>;
    /// The scan Settings asked for.
    fn scanned(&self, result: Result<Vec<Bss>, String>) -> Option<()>;
    /// Gives up on kindil's own Wi-Fi for the run.
    fn fall_back(&self, why: &str);
}

/// What the keeper found before choosing.
enum Survey {
    /// On a saved network already.
    Already { ssid: String, bssid: [u8; 6] },
    /// Back from a suspend: connecting to the network it was on.
    Resuming(Target),
    /// No saved networks (any association left).
    Empty,
    /// A scan decides.
    Scan,
}

/// The first look of an attempt: on a saved network already? An
/// association with a network no longer saved is left. After a suspend,
/// straight back to the last network.
fn survey(
    driver: &impl Driver,
    saved: &[Credentials],
    last: Option<&Target>,
    resumed: bool,
    sleep: &dyn Fn(Duration),
) -> Result<(Survey, bool), DriverError> {
    let mut forgot = false;
    if let Some(bssid) = driver.bssid().map_err(at("SIOCGIWAP"))? {
        let essid = driver.essid().map_err(at("SIOCGIWESSID"))?;
        if !saved.iter().any(|c| c.ssid().as_str().as_bytes() == essid) {
            disconnect(driver)?;
            forgot = true;
        } else if !resumed {
            let ssid = String::from_utf8_lossy(&essid).into_owned();
            return Ok((Survey::Already { ssid, bssid }, false));
        }
    }
    if let Some(target) = last.filter(|t| resumed && saved.contains(&t.credentials)) {
        connect(driver, target, sleep)?;
        return Ok((Survey::Resuming(target.clone()), forgot));
    }
    if saved.is_empty() {
        return Ok((Survey::Empty, forgot));
    }
    Ok((Survey::Scan, forgot))
}

/// What a connect was for.
struct Try {
    target: Target,
    /// Seen in the scan: failing counts toward the fallback by time.
    in_range: bool,
    /// Unseen: failing counts toward the fallback by blind tries.
    blind: bool,
}

enum State {
    Attempt(Option<Kick>),
    Watch,
    Lost,
}

#[derive(Default)]
struct Keeper {
    backoff: Backoff,
    /// When the tries on a network in range started failing.
    failing_since: Option<Duration>,
    strikes: u32,
    /// The network last joined, for straight back after a suspend.
    last: Option<Target>,
    /// Joined with a lease, and not lost since.
    associated: bool,
    /// The next saved network to try blind.
    blind_next: usize,
    /// Failed blind tries, by network, since the last join.
    blind_failures: BTreeMap<String, u32>,
    /// How each saved network was last seen, to join it unseen.
    known: BTreeMap<String, Plan>,
}

/// Joins the best saved network and keeps it joined, until the stack stops
/// (an effect answers `None`) or it gives up ([`Context::fall_back`]).
/// Settings' scans are done here too, between joins.
pub fn keep<X: Context>(cx: &X) {
    let mut keeper = Keeper::default();
    let mut state = State::Attempt(None);
    while let Some(next) = keeper.step(cx, state) {
        state = next;
    }
}

impl Keeper {
    fn step<X: Context>(&mut self, cx: &X, state: State) -> Option<State> {
        match state {
            State::Attempt(kick) => self.attempt(cx, kick),
            State::Watch => {
                if let Some(kick) = self.pause(cx, WATCH)? {
                    return Some(State::Attempt(Some(kick)));
                }
                match cx.with(|d, _| d.bssid())? {
                    Err(error) => self.strike(cx, at("SIOCGIWAP")(error), State::Watch),
                    Ok(Some(_)) => {
                        self.strikes = 0;
                        Some(State::Watch)
                    }
                    Ok(None) => {
                        self.strikes = 0;
                        self.associated = false;
                        crate::log!("kindil: Wi-Fi: lost the access point");
                        Some(State::Lost)
                    }
                }
            }
            // The firmware rejoins by itself after any disconnect but the
            // host's; a rejoin is proven like any join.
            State::Lost => match self.wait_associated(cx, REJOIN_GRACE)? {
                Ok(Some(bssid)) => match cx.with(|d, _| d.essid())? {
                    Ok(essid) => {
                        let ssid = String::from_utf8_lossy(&essid).into_owned();
                        self.check(cx, &ssid, bssid, None)
                    }
                    Err(error) => self.strike(cx, at("SIOCGIWESSID")(error), State::Attempt(None)),
                },
                Ok(None) => Some(State::Attempt(None)),
                Err(error) => self.strike(cx, error, State::Attempt(None)),
            },
        }
    }

    fn attempt<X: Context>(&mut self, cx: &X, kick: Option<Kick>) -> Option<State> {
        let resumed = kick == Some(Kick::Resumed);
        let last = self.last.clone();
        let surveyed =
            cx.with(|d, saved| survey(d, saved, last.as_ref(), resumed, &|p| cx.sleep(p)))?;
        let (survey, forgot) = match surveyed {
            Ok(surveyed) => surveyed,
            Err(error) => return self.strike(cx, error, State::Attempt(None)),
        };
        if forgot {
            crate::log!("kindil: Wi-Fi: left a network no longer saved");
            self.associated = false;
            self.last = None;
        }
        match survey {
            Survey::Empty => Some(State::Attempt(self.pause(cx, IDLE)?)),
            Survey::Already { ssid, bssid } => {
                self.strikes = 0;
                if self.associated {
                    // Proven already: still on it.
                    return Some(State::Watch);
                }
                if self.last.as_ref().is_some_and(|t| t.ssid() != ssid) {
                    self.last = None;
                }
                self.check(cx, &ssid, bssid, None)
            }
            Survey::Resuming(target) => {
                self.strikes = 0;
                self.connected(
                    cx,
                    Try {
                        target,
                        in_range: true,
                        blind: false,
                    },
                )
            }
            Survey::Scan => {
                let seen = match self.scan(cx)? {
                    Ok(seen) => seen,
                    Err(error) => return self.strike(cx, error, State::Attempt(None)),
                };
                self.remember(&seen);
                let blind_next = self.blind_next;
                let known = self.known.clone();
                let failures = self.blind_failures.clone();
                let chose = cx.with(
                    |d, saved| -> Result<Option<Result<Try, String>>, DriverError> {
                        let tried = match choose(saved, &seen) {
                            Choice::Nothing => return Ok(None),
                            Choice::Unsupported { ssid, why } => {
                                return Ok(Some(Err(format!("can't join {ssid}: {why}"))))
                            }
                            Choice::Join(target) => Try {
                                target,
                                in_range: true,
                                blind: false,
                            },
                            Choice::Unseen => {
                                let credentials = &saved[blind_next % saved.len()];
                                let ssid = credentials.ssid().as_str();
                                // Seen before: as it was; else CCMP, then TKIP.
                                let group = match known.get(ssid) {
                                    Some(Plan::Wpa2 { group }) => *group,
                                    _ if failures.get(ssid).copied().unwrap_or(0) % 2 == 1 => {
                                        Cipher::Tkip
                                    }
                                    _ => Cipher::Ccmp,
                                };
                                Try {
                                    target: Target::blind(credentials, group),
                                    in_range: false,
                                    blind: true,
                                }
                            }
                        };
                        connect(d, &tried.target, &|p| cx.sleep(p))?;
                        Ok(Some(Ok(tried)))
                    },
                )?;
                match chose {
                    Err(error) => self.strike(cx, error, State::Attempt(None)),
                    Ok(None) => Some(State::Attempt(self.pause(cx, IDLE)?)),
                    Ok(Some(Err(why))) => {
                        cx.fall_back(&why);
                        None
                    }
                    Ok(Some(Ok(tried))) => {
                        self.strikes = 0;
                        if tried.blind {
                            self.blind_next += 1;
                        }
                        self.connected(cx, tried)
                    }
                }
            }
        }
    }

    /// How the saved networks in `seen` were seen, for joining them unseen.
    fn remember(&mut self, seen: &[Bss]) {
        for bss in seen.iter().filter(|b| !b.ssid.is_empty()) {
            if let Kind::Wpa2 { group } = kind(bss) {
                let ssid = String::from_utf8_lossy(&bss.ssid).into_owned();
                self.known.insert(ssid, Plan::Wpa2 { group });
            }
        }
    }

    /// A connect was asked for: association, then its proof.
    fn connected<X: Context>(&mut self, cx: &X, tried: Try) -> Option<State> {
        self.associated = false;
        if tried.in_range {
            // Cleared only by a join with a lease.
            self.failing_since.get_or_insert(cx.now());
            if let Some(why) = self.overdue(cx, tried.target.ssid()) {
                cx.fall_back(&why);
                return None;
            }
        }
        match self.wait_associated(cx, ASSOC_WAIT)? {
            Err(error) => self.strike(cx, error, State::Attempt(None)),
            Ok(None) => {
                let ssid = tried.target.ssid().to_owned();
                self.failed(cx, &ssid, tried.in_range, tried.blind)
            }
            Ok(Some(bssid)) => {
                let ssid = tried.target.ssid().to_owned();
                self.check(cx, &ssid, bssid, Some(tried))
            }
        }
    }

    /// Associated with `ssid` on `bssid`: proves it with a lease, then
    /// watches it; a failed proof goes the way of any failed join.
    fn check<X: Context>(
        &mut self,
        cx: &X,
        ssid: &str,
        bssid: [u8; 6],
        tried: Option<Try>,
    ) -> Option<State> {
        // On it: in range.
        self.failing_since.get_or_insert(cx.now());
        let mut bssid_now = || cx.with(|d, _| d.bssid().map_err(at("SIOCGIWAP")));
        let proved = prove(
            cx,
            ssid,
            bssid,
            &|| cx.now(),
            &|p| cx.sleep(p),
            &mut bssid_now,
        )?;
        match proved {
            Err(error) => self.strike(cx, error, State::Attempt(None)),
            Ok(true) => {
                if let Some(tried) = tried {
                    self.last = Some(tried.target);
                }
                self.backoff.reset();
                self.failing_since = None;
                self.blind_failures.clear();
                if !self.associated {
                    crate::log!("kindil: Wi-Fi: joined {ssid}");
                    cx.joined(ssid)?;
                }
                self.associated = true;
                Some(State::Watch)
            }
            Ok(false) => {
                crate::log!(
                    "kindil: Wi-Fi: on {ssid} but no address within {} s (a wrong password?)",
                    LEASE_WAIT.as_secs()
                );
                let (in_range, blind) = tried.map_or((true, false), |t| (t.in_range, t.blind));
                self.failed(cx, ssid, in_range, blind)
            }
        }
    }

    /// A join that didn't come about: leaves the network (so the firmware
    /// isn't chasing it while pausing), counts it, and pauses or gives up.
    fn failed<X: Context>(
        &mut self,
        cx: &X,
        ssid: &str,
        in_range: bool,
        blind: bool,
    ) -> Option<State> {
        self.associated = false;
        if let Err(error) = cx.with(|d, _| disconnect(d))? {
            crate::log!("kindil: Wi-Fi: leaving: {error}");
        }
        if blind {
            *self.blind_failures.entry(ssid.to_owned()).or_default() += 1;
            let failures = &self.blind_failures;
            let exhausted = cx.with(|_, saved| {
                !saved.is_empty()
                    && saved.iter().all(|c| {
                        failures.get(c.ssid().as_str()).copied().unwrap_or(0) >= BLIND_TRIES
                    })
            })?;
            if exhausted {
                cx.fall_back(&format!(
                    "none of the saved networks in sight joined, each tried unseen {BLIND_TRIES} times"
                ));
                return None;
            }
        }
        if in_range {
            if let Some(why) = self.overdue(cx, ssid) {
                cx.fall_back(&why);
                return None;
            }
        }
        let pause = self.backoff.pause();
        crate::log!("kindil: Wi-Fi: {ssid} not joined; trying again in {pause:?}");
        Some(State::Attempt(self.pause(cx, pause)?))
    }

    /// Failing on a network in range for [`FALLBACK_AFTER`]: why, to give up.
    fn overdue<X: Context>(&self, cx: &X, ssid: &str) -> Option<String> {
        let since = self.failing_since?;
        (cx.now() - since >= FALLBACK_AFTER).then(|| {
            format!(
                "no join with an address on {ssid} within {} s",
                FALLBACK_AFTER.as_secs()
            )
        })
    }

    /// Polls until associated (the BSSID) or `limit` passes.
    fn wait_associated<X: Context>(
        &mut self,
        cx: &X,
        limit: Duration,
    ) -> Option<Result<Option<[u8; 6]>, DriverError>> {
        let started = cx.now();
        loop {
            match cx.with(|d, _| d.bssid())? {
                Err(error) => return Some(Err(at("SIOCGIWAP")(error))),
                Ok(Some(bssid)) => return Some(Ok(Some(bssid))),
                Ok(None) => {}
            }
            if cx.now() - started >= limit {
                return Some(Ok(None));
            }
            cx.sleep(POLL);
        }
    }

    /// A scan: started, [`SCAN_WAIT`] and a first read under the lock (no
    /// connect can start meanwhile); results not ready are read again for
    /// [`SCAN_RETRY`], the lock let go between reads.
    fn scan<X: Context>(&mut self, cx: &X) -> Option<Result<Vec<Bss>, DriverError>> {
        let first = cx.with(|d, _| {
            scan_start(d)?;
            cx.sleep(SCAN_WAIT);
            scan_read(d)
        })?;
        let deadline = cx.now() + SCAN_RETRY;
        let mut read = first;
        loop {
            match read {
                Err(error) => return Some(Err(error)),
                Ok(Some(seen)) => return Some(Ok(seen)),
                Ok(None) if cx.now() >= deadline => return Some(Err(scan_timed_out())),
                Ok(None) => {}
            }
            cx.sleep(POLL);
            read = cx.with(|d, _| scan_read(d))?;
        }
    }

    /// Waits up to `pause`, doing the scans Settings asks for meanwhile;
    /// a rejoin kick ends it early. `None`: stopped.
    fn pause<X: Context>(&mut self, cx: &X, pause: Duration) -> Option<Option<Kick>> {
        let end = cx.now() + pause;
        loop {
            match cx.wait(end.saturating_sub(cx.now())) {
                Some(Kick::Scan) => match self.scan(cx)? {
                    Ok(seen) => {
                        self.strikes = 0;
                        self.remember(&seen);
                        cx.scanned(Ok(seen))?;
                    }
                    Err(error) => {
                        cx.scanned(Err(error.to_string()))?;
                        self.strikes += 1;
                        crate::log!("kindil: Wi-Fi: {error}");
                        if self.strikes >= STRIKES {
                            cx.fall_back(&format!("the Wi-Fi driver keeps failing ({error})"));
                            return None;
                        }
                    }
                },
                other => return Some(other),
            }
        }
    }

    /// A driver error: a pause and on, or giving up after [`STRIKES`].
    fn strike<X: Context>(&mut self, cx: &X, error: DriverError, next: State) -> Option<State> {
        self.strikes += 1;
        crate::log!("kindil: Wi-Fi: {error}");
        if self.strikes >= STRIKES {
            cx.fall_back(&format!("the Wi-Fi driver keeps failing ({error})"));
            return None;
        }
        let pause = self.backoff.pause();
        Some(match self.pause(cx, pause)? {
            Some(kick) => State::Attempt(Some(kick)),
            None => next,
        })
    }
}

#[cfg(test)]
pub mod fake {
    //! A driver that answers like the AR6003's and records each call, and a
    //! context with a clock that only moves when slept on, and a model of
    //! udhcpc: a process with a pid, restarted or asked to renew, whose hook
    //! leaves a marker and sets the address some time after a DHCP exchange
    //! that only gets through while associated with a network whose server
    //! answers (the right keys).
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    #[derive(Default)]
    pub struct FakeDriver {
        pub calls: RefCell<Vec<String>>,
        /// What a scan finds.
        pub aps: RefCell<Vec<Bss>>,
        /// Networks it associates with once asked, after this many polls.
        pub joinable: RefCell<Vec<(Vec<u8>, u32)>>,
        /// Networks that associate only with this group cipher (IW_AUTH).
        pub group_needed: RefCell<Vec<(Vec<u8>, i32)>>,
        group: Cell<i32>,
        /// The step (a prefix of a call's name) that fails, with an errno.
        pub failing: RefCell<Option<(&'static str, i32)>>,
        /// Reads of the scan results that answer EAGAIN first.
        pub eagain: Cell<u32>,
        pub associated: RefCell<Option<[u8; 6]>>,
        pub essid: RefCell<Vec<u8>>,
        polls_left: Cell<Option<u32>>,
        pub pmks: Cell<u32>,
    }

    impl FakeDriver {
        pub fn new() -> Self {
            Self::default()
        }

        fn call(&self, name: String) -> io::Result<()> {
            let failing = *self.failing.borrow();
            let fails = failing.filter(|(step, _)| name.starts_with(step));
            self.calls.borrow_mut().push(name);
            match fails {
                Some((_, errno)) => Err(io::Error::from_raw_os_error(errno)),
                None => Ok(()),
            }
        }

        /// The access point drops the station, and the firmware doesn't
        /// rejoin by itself.
        pub fn drop_link(&self) {
            *self.associated.borrow_mut() = None;
            self.polls_left.set(None);
        }

        /// Associated with `ssid` already, as a start finds it.
        pub fn on(&self, ssid: &str, bssid: [u8; 6]) {
            *self.essid.borrow_mut() = ssid.as_bytes().to_vec();
            *self.associated.borrow_mut() = Some(bssid);
        }

        /// Calls other than the polls.
        pub fn actions(&self) -> Vec<String> {
            self.calls
                .borrow()
                .iter()
                .filter(|c| !c.starts_with("bssid") && !c.starts_with("essid?"))
                .cloned()
                .collect()
        }

        pub fn count(&self, name: &str) -> usize {
            self.calls.borrow().iter().filter(|c| *c == name).count()
        }
    }

    pub const BSSID: [u8; 6] = [0x80, 0x69, 1, 2, 3, 4];

    impl Driver for FakeDriver {
        fn set_up(&self) -> io::Result<()> {
            self.call("up".into())
        }
        fn set_auth(&self, index: u16, value: i32) -> io::Result<()> {
            if index == wext::IW_AUTH_CIPHER_GROUP {
                self.group.set(value);
            }
            self.call(format!("auth {index}={value}"))
        }
        fn set_pmk(&self, pmk: &crate::pmk::Pmk) -> io::Result<()> {
            assert_ne!(pmk.bytes(), &[0; 32]);
            self.pmks.set(self.pmks.get() + 1);
            self.call("pmk".into())
        }
        fn set_essid(&self, ssid: Option<&[u8]>) -> io::Result<()> {
            match ssid {
                None => {
                    self.call("essid off".into())?;
                    *self.associated.borrow_mut() = None;
                    self.essid.borrow_mut().clear();
                    self.polls_left.set(None);
                }
                Some(ssid) => {
                    self.call(format!("essid {}", String::from_utf8_lossy(ssid)))?;
                    *self.essid.borrow_mut() = ssid.to_vec();
                    let group_ok = self
                        .group_needed
                        .borrow()
                        .iter()
                        .find(|(name, _)| name == ssid)
                        .is_none_or(|(_, group)| *group == self.group.get());
                    let polls = self
                        .joinable
                        .borrow()
                        .iter()
                        .find(|(name, _)| name == ssid)
                        .map(|(_, polls)| *polls)
                        .filter(|_| group_ok);
                    self.polls_left.set(polls);
                }
            }
            Ok(())
        }
        fn bssid(&self) -> io::Result<Option<[u8; 6]>> {
            self.call("bssid".into())?;
            if self.associated.borrow().is_none() {
                match self.polls_left.get() {
                    Some(0) => *self.associated.borrow_mut() = Some(BSSID),
                    Some(n) => self.polls_left.set(Some(n - 1)),
                    None => {}
                }
            }
            Ok(*self.associated.borrow())
        }
        fn essid(&self) -> io::Result<Vec<u8>> {
            self.call("essid?".into())?;
            Ok(self.essid.borrow().clone())
        }
        fn disassociate(&self, bssid: [u8; 6]) -> io::Result<()> {
            self.call(format!("disassociate {}", wext::mac_string(&bssid)))?;
            *self.associated.borrow_mut() = None;
            Ok(())
        }
        fn trigger_scan(&self) -> io::Result<()> {
            if !self.essid.borrow().is_empty() && self.associated.borrow().is_none() {
                self.calls.borrow_mut().push("SCAN DURING A CONNECT".into());
            }
            self.call("scan".into())
        }
        fn scan_results(&self) -> io::Result<Vec<u8>> {
            self.call("scan results".into())?;
            if self.eagain.get() > 0 {
                self.eagain.set(self.eagain.get().saturating_sub(1));
                return Err(io::Error::from_raw_os_error(libc::EAGAIN));
            }
            let mut stream = wext::stream::Stream::default();
            for ap in self.aps.borrow().iter() {
                let ies: Vec<Vec<u8>> = ap
                    .rsn
                    .iter()
                    .map(|r| {
                        let pairwise: Vec<u8> = r.pairwise.iter().map(cipher_type).collect();
                        let akm: Vec<u8> = r.akm.iter().map(akm_type).collect();
                        wext::stream::rsn(cipher_type(&r.group), &pairwise, &akm)
                    })
                    .chain(ap.wpa.iter().map(|w| {
                        let pairwise: Vec<u8> = w.pairwise.iter().map(cipher_type).collect();
                        wext::stream::wpa(cipher_type(&w.group), &pairwise, &[2])
                    }))
                    .collect();
                let ies: Vec<&[u8]> = ies.iter().map(Vec::as_slice).collect();
                let snr = (ap.level.unwrap_or(-95) + 95).clamp(0, 94) as u8;
                stream.ap(
                    ap.bssid,
                    &ap.ssid,
                    ap.mhz.unwrap_or(2412) as i32,
                    snr,
                    ap.privacy,
                    &ies,
                );
            }
            Ok(stream.0)
        }
    }

    fn cipher_type(c: &Cipher) -> u8 {
        match c {
            Cipher::Wep40 => 1,
            Cipher::Tkip => 2,
            Cipher::Ccmp => 4,
            Cipher::Wep104 => 5,
            Cipher::Other(t) => *t,
        }
    }

    fn akm_type(a: &Akm) -> u8 {
        match a {
            Akm::Ieee8021x => 1,
            Akm::Psk => 2,
            Akm::PskSha256 => 6,
            Akm::Sae => 8,
            Akm::Other(t) => *t,
        }
    }

    pub fn wpa2(bssid: u8, ssid: &str, level: i32, group: Cipher) -> Bss {
        Bss {
            bssid: [bssid; 6],
            ssid: ssid.as_bytes().to_vec(),
            mhz: Some(2437),
            level: Some(level),
            privacy: true,
            rsn: Some(wext::Suites {
                group,
                pairwise: vec![Cipher::Ccmp],
                akm: vec![Akm::Psk],
            }),
            wpa: None,
        }
    }

    pub fn open(bssid: u8, ssid: &str, level: i32) -> Bss {
        Bss {
            bssid: [bssid; 6],
            ssid: ssid.as_bytes().to_vec(),
            mhz: Some(2412),
            level: Some(level),
            ..Bss::default()
        }
    }

    pub fn wpa1(bssid: u8, ssid: &str, level: i32) -> Bss {
        Bss {
            bssid: [bssid; 6],
            ssid: ssid.as_bytes().to_vec(),
            mhz: Some(2412),
            level: Some(level),
            privacy: true,
            rsn: None,
            wpa: Some(wext::Suites {
                group: Cipher::Tkip,
                pairwise: vec![Cipher::Tkip],
                akm: vec![Akm::Psk],
            }),
        }
    }

    /// A change to anything in the fake.
    pub type Change = Box<dyn Fn(&FakeContext)>;

    /// Something to happen at a time: a kick, or a change.
    pub enum Event {
        Kick(Kick),
        Do(Change),
    }

    pub struct FakeContext {
        pub driver: FakeDriver,
        pub saved: RefCell<Vec<Credentials>>,
        pub clock: Cell<Duration>,
        /// The stack stops (a newer generation) at this time.
        pub until: Duration,
        pub events: RefCell<VecDeque<(Duration, Event)>>,
        pending: Cell<Option<Kick>>,
        pending_scan: Cell<bool>,
        pub joins: RefCell<Vec<(Duration, String)>>,
        pub fell_back: RefCell<Option<(Duration, String)>>,
        pub scans: RefCell<Vec<(Duration, Result<usize, String>)>>,
        /// Effects asked for after the stack stopped: must stay empty.
        pub late: RefCell<Vec<String>>,
        /// Networks whose DHCP server answers, how fast, and the address.
        pub dhcp: RefCell<Vec<(Vec<u8>, Duration, Ipv4Addr)>>,
        /// The udhcpc running now, and how many were started.
        pub pid: Cell<u32>,
        pub restarts: Cell<u32>,
        pub renews: Cell<u32>,
        pub markers: RefCell<BTreeMap<u32, Marker>>,
        pub address: Cell<Option<Ipv4Addr>>,
        /// The udhcpc that holds a network's lease, as told by `proven`.
        pub lease_for: RefCell<Option<(u32, String)>>,
        pub asked: RefCell<Vec<String>>,
    }

    impl FakeContext {
        pub fn new(saved: Vec<Credentials>, until: Duration) -> Self {
            Self {
                driver: FakeDriver::new(),
                saved: RefCell::new(saved),
                clock: Cell::new(Duration::ZERO),
                until,
                events: RefCell::default(),
                pending: Cell::new(None),
                pending_scan: Cell::new(false),
                joins: RefCell::default(),
                fell_back: RefCell::default(),
                scans: RefCell::default(),
                late: RefCell::default(),
                dhcp: RefCell::default(),
                pid: Cell::new(100),
                restarts: Cell::new(0),
                renews: Cell::new(0),
                markers: RefCell::default(),
                address: Cell::new(None),
                lease_for: RefCell::default(),
                asked: RefCell::default(),
            }
        }

        pub fn at(&self, time: Duration, event: Event) {
            let mut events = self.events.borrow_mut();
            events.push_back((time, event));
            events.make_contiguous().sort_by_key(|(t, _)| *t);
        }

        /// A hook of udhcpc `pid` at `time`: its address set, then its
        /// marker written.
        pub fn hook(&self, time: Duration, pid: u32, leased: Leased, ip: Ipv4Addr) {
            self.at(
                time,
                Event::Do(Box::new(move |cx| {
                    cx.address.set(Some(ip));
                    let at = cx.clock.get();
                    cx.markers
                        .borrow_mut()
                        .insert(pid, Marker { leased, ip, at });
                })),
            );
        }

        fn stopped(&self) -> bool {
            self.clock.get() >= self.until || self.fell_back.borrow().is_some()
        }

        fn effect(&self, what: &str) -> Option<()> {
            if self.stopped() {
                self.late.borrow_mut().push(what.to_owned());
                return None;
            }
            Some(())
        }

        fn take_kick(&self) -> Option<Kick> {
            if self.pending_scan.replace(false) {
                return Some(Kick::Scan);
            }
            self.pending.take()
        }

        /// Runs the events due by now.
        fn fire(&self) {
            loop {
                let due = {
                    let mut events = self.events.borrow_mut();
                    match events.front() {
                        Some((t, _)) if *t <= self.clock.get() => events.pop_front(),
                        _ => None,
                    }
                };
                match due {
                    Some((_, Event::Kick(Kick::Scan))) => self.pending_scan.set(true),
                    Some((_, Event::Kick(kick))) => self.pending.set(Some(kick)),
                    Some((_, Event::Do(change))) => change(self),
                    None => return,
                }
            }
        }

        fn advance(&self, pause: Duration) {
            self.clock.set(self.clock.get() + pause);
            self.fire();
        }

        /// The DHCP exchange udhcpc `pid` starts now: it gets through if
        /// associated with a network whose server answers, and its hook
        /// runs after the server's delay, if that udhcpc still runs and the
        /// station is still on that network.
        fn exchange(&self, pid: u32, leased: Leased) {
            let essid = self.driver.essid.borrow().clone();
            if self.driver.associated.borrow().is_none() {
                return;
            }
            let server = self
                .dhcp
                .borrow()
                .iter()
                .find(|(name, _, _)| *name == essid)
                .map(|(_, delay, ip)| (*delay, *ip));
            if let Some((delay, ip)) = server {
                self.at(
                    self.clock.get() + delay,
                    Event::Do(Box::new(move |cx| {
                        let on = cx.driver.associated.borrow().is_some()
                            && *cx.driver.essid.borrow() == essid;
                        if cx.pid.get() == pid && on {
                            cx.address.set(Some(ip));
                            let at = cx.clock.get();
                            cx.markers
                                .borrow_mut()
                                .insert(pid, Marker { leased, ip, at });
                        }
                    })),
                );
            }
        }
    }

    impl Context for FakeContext {
        type D = FakeDriver;
        fn now(&self) -> Duration {
            self.clock.get()
        }
        fn sleep(&self, pause: Duration) {
            self.advance(pause);
        }
        fn wait(&self, pause: Duration) -> Option<Kick> {
            let end = self.clock.get() + pause;
            while self.clock.get() < end {
                if self.stopped() {
                    return None;
                }
                if let Some(kick) = self.take_kick() {
                    return Some(kick);
                }
                let next_event = self.events.borrow().front().map(|(t, _)| *t);
                let step = match next_event {
                    Some(t) if t > self.clock.get() && t < end => t - self.clock.get(),
                    _ => end - self.clock.get(),
                };
                self.advance(step);
            }
            if self.stopped() {
                return None;
            }
            self.take_kick()
        }
        fn with<R>(&self, f: impl FnOnce(&FakeDriver, &[Credentials]) -> R) -> Option<R> {
            self.fire();
            if self.stopped() {
                return None;
            }
            let saved = self.saved.borrow();
            Some(f(&self.driver, &saved))
        }
        fn joined(&self, ssid: &str) -> Option<()> {
            self.effect("joined")?;
            self.joins
                .borrow_mut()
                .push((self.clock.get(), ssid.to_owned()));
            Some(())
        }
        fn scanned(&self, result: Result<Vec<Bss>, String>) -> Option<()> {
            self.effect("scanned")?;
            self.scans
                .borrow_mut()
                .push((self.clock.get(), result.map(|seen| seen.len())));
            Some(())
        }
        fn fall_back(&self, why: &str) {
            if self.effect("fall back").is_some() {
                *self.fell_back.borrow_mut() = Some((self.clock.get(), why.to_owned()));
            }
        }
    }

    impl Dhcp for FakeContext {
        fn ask(&self, ssid: &str, _bssid: [u8; 6]) -> Option<Result<Ticket, String>> {
            self.effect("ask")?;
            self.asked.borrow_mut().push(ssid.to_owned());
            let pid = self.pid.get();
            let renew = self.lease_for.borrow().as_ref() == Some(&(pid, ssid.to_owned()));
            let ticket = if renew {
                self.renews.set(self.renews.get() + 1);
                Ticket {
                    pid,
                    since: self.clock.get(),
                    renew: true,
                }
            } else {
                // The old one ended; a new one with a new pid.
                self.pid.set(pid + 1);
                self.restarts.set(self.restarts.get() + 1);
                Ticket {
                    pid: pid + 1,
                    since: self.clock.get(),
                    renew: false,
                }
            };
            let leased = if renew { Leased::Renew } else { Leased::Bound };
            self.exchange(ticket.pid, leased);
            Some(Ok(ticket))
        }
        fn marker(&self, pid: u32) -> Option<Marker> {
            self.markers.borrow().get(&pid).copied()
        }
        fn address(&self) -> Option<Ipv4Addr> {
            self.address.get()
        }
        fn proven(&self, ticket: &Ticket, ssid: &str) -> Option<()> {
            self.effect("proven")?;
            *self.lease_for.borrow_mut() = Some((ticket.pid, ssid.to_owned()));
            Some(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::*;
    use super::*;

    fn wpa(ssid: &str) -> Credentials {
        Credentials::parse(ssid.into(), "correct horse".into()).unwrap()
    }

    fn open_net(ssid: &str) -> Credentials {
        Credentials::parse(ssid.into(), String::new()).unwrap()
    }

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    const IP: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 42);
    const OLD_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 7);

    #[test]
    fn the_switch() {
        assert!(chosen(Some("kindil\n")));
        assert!(chosen(Some(" kindil ")));
        assert!(!chosen(Some("wpa_supplicant")));
        assert!(!chosen(Some("")));
        assert!(!chosen(None));
    }

    #[test]
    fn kinds_of_access_point() {
        assert_eq!(
            kind(&wpa2(1, "a", -50, Cipher::Ccmp)),
            Kind::Wpa2 {
                group: Cipher::Ccmp
            }
        );
        assert_eq!(
            kind(&wpa2(1, "a", -50, Cipher::Tkip)),
            Kind::Wpa2 {
                group: Cipher::Tkip
            }
        );
        assert_eq!(kind(&open(1, "a", -50)), Kind::Open);
        assert!(
            matches!(kind(&wpa1(1, "a", -50)), Kind::Unsupported(w) if w.contains("WPA (TKIP)"))
        );
        let wep = Bss {
            privacy: true,
            ..open(1, "a", -50)
        };
        assert!(matches!(kind(&wep), Kind::Unsupported(w) if w.contains("WEP")));
        let mut tkip_only = wpa2(1, "a", -50, Cipher::Tkip);
        tkip_only.rsn.as_mut().unwrap().pairwise = vec![Cipher::Tkip];
        assert!(matches!(kind(&tkip_only), Kind::Unsupported(w) if w.contains("TKIP only")));
        let mut enterprise = wpa2(1, "a", -50, Cipher::Ccmp);
        enterprise.rsn.as_mut().unwrap().akm = vec![Akm::Ieee8021x];
        assert!(matches!(kind(&enterprise), Kind::Unsupported(w) if w.contains("802.1X")));
        let mut sae = wpa2(1, "a", -50, Cipher::Ccmp);
        sae.rsn.as_mut().unwrap().akm = vec![Akm::Sae];
        assert!(matches!(kind(&sae), Kind::Unsupported(w) if w.contains("WPA3")));
        sae.rsn.as_mut().unwrap().akm = vec![Akm::Sae, Akm::Psk];
        assert!(matches!(kind(&sae), Kind::Wpa2 { .. }));
        let mut mixed = wpa2(1, "a", -50, Cipher::Tkip);
        mixed.wpa = wpa1(1, "a", -50).wpa;
        assert!(matches!(kind(&mixed), Kind::Wpa2 { .. }));
    }

    #[test]
    fn chooses_the_most_preferred_saved_network_in_range() {
        let saved = [wpa("Home"), open_net("Library"), wpa("Phone")];
        let seen = [
            wpa2(1, "Neighbour", -40, Cipher::Ccmp),
            wpa2(2, "Phone", -45, Cipher::Ccmp),
            open(3, "Library", -60),
        ];
        let Choice::Join(t) = choose(&saved, &seen) else {
            panic!()
        };
        assert_eq!(t.ssid(), "Library");
        assert_eq!(t.plan, Plan::Open);
        let mut seen = seen.to_vec();
        seen.push(wpa2(4, "Home", -80, Cipher::Ccmp));
        seen.push(wpa2(5, "Home", -50, Cipher::Tkip));
        let Choice::Join(t) = choose(&saved, &seen) else {
            panic!()
        };
        assert_eq!(t.ssid(), "Home");
        assert_eq!(
            t.plan,
            Plan::Wpa2 {
                group: Cipher::Tkip
            }
        );
    }

    #[test]
    fn skips_what_it_cant_join_and_says_why() {
        let saved = [wpa("Old"), wpa("Home")];
        let seen = [wpa1(1, "Old", -40), wpa2(2, "Home", -70, Cipher::Ccmp)];
        assert!(matches!(choose(&saved, &seen), Choice::Join(t) if t.ssid() == "Home"));
        let Choice::Unsupported { ssid, why } = choose(&saved[..1], &seen) else {
            panic!()
        };
        assert_eq!(ssid, "Old");
        assert!(why.contains("WPA (TKIP)"));
        let Choice::Unsupported { why, .. } =
            choose(&[open_net("Home")], &[wpa2(2, "Home", -70, Cipher::Ccmp)])
        else {
            panic!()
        };
        assert!(why.contains("password"));
    }

    #[test]
    fn nothing_in_sight_is_unseen_and_nothing_saved_is_nothing() {
        let saved = [wpa("Hidden"), open_net("Cafe")];
        assert!(matches!(
            choose(&saved, &[open(1, "Other", -50)]),
            Choice::Unseen
        ));
        assert!(matches!(
            choose(&[], &[open(1, "Other", -50)]),
            Choice::Nothing
        ));
        assert_eq!(
            Target::blind(&saved[0], Cipher::Tkip).plan,
            Plan::Wpa2 {
                group: Cipher::Tkip
            }
        );
        assert_eq!(Target::blind(&saved[1], Cipher::Tkip).plan, Plan::Open);
    }

    #[test]
    fn the_proven_ioctl_sequence() {
        let driver = FakeDriver::new();
        let target = Target::blind(&wpa("Home"), Cipher::Ccmp);
        let slept = std::cell::Cell::new(Duration::ZERO);
        connect(&driver, &target, &|p| slept.set(slept.get() + p)).unwrap();
        assert_eq!(
            driver.actions(),
            [
                "up",
                "essid off",
                "auth 7=1",
                "auth 0=4",
                "auth 1=8",
                "auth 2=8",
                "auth 3=2",
                "auth 6=1",
                "auth 5=1",
                "auth 10=1",
                "pmk",
                "essid Home",
            ]
        );
        assert_eq!(slept.get(), CANCEL_SETTLE);
        let driver = FakeDriver::new();
        connect(&driver, &Target::blind(&wpa("Home"), Cipher::Tkip), &|_| {}).unwrap();
        assert!(driver.actions().contains(&"auth 2=4".to_string()));
        let driver = FakeDriver::new();
        connect(
            &driver,
            &Target::blind(&open_net("Cafe"), Cipher::Ccmp),
            &|_| {},
        )
        .unwrap();
        assert_eq!(driver.actions(), ["up", "essid off", "essid Cafe"]);
    }

    #[test]
    fn a_failing_ioctl_stops_the_sequence_and_names_itself() {
        let driver = FakeDriver::new();
        *driver.failing.borrow_mut() = Some(("pmk", libc::EINVAL));
        let error =
            connect(&driver, &Target::blind(&wpa("Home"), Cipher::Ccmp), &|_| {}).unwrap_err();
        assert_eq!(error.step, "SIOCSIWENCODEEXT PMK");
        assert!(!driver.actions().iter().any(|c| c.starts_with("essid Home")));
    }

    const RESET: [&str; 4] = ["essid off", "auth 7=0", "auth 10=0", "auth 5=0"];

    #[test]
    fn disconnecting_disassociates_turns_the_essid_off_and_resets_the_profile() {
        let driver = FakeDriver::new();
        driver.on("Home", BSSID);
        disconnect(&driver).unwrap();
        let mut expected = vec!["disassociate 80:69:01:02:03:04"];
        expected.extend(RESET);
        assert_eq!(driver.actions(), expected);
        let driver = FakeDriver::new();
        disconnect(&driver).unwrap();
        assert_eq!(driver.actions(), RESET);
    }

    #[test]
    fn the_profile_is_reset_whatever_the_disassociation_did() {
        for failing in ["disassociate", "bssid"] {
            let driver = FakeDriver::new();
            driver.on("Home", BSSID);
            *driver.failing.borrow_mut() = Some((failing, libc::EIO));
            let error = disconnect(&driver).unwrap_err();
            assert!(
                error.step.contains("DISASSOC") || error.step == "SIOCGIWAP",
                "{error}"
            );
            for step in RESET {
                assert!(
                    driver.actions().contains(&step.to_string()),
                    "{failing}: {step}"
                );
            }
        }
        // Essid off failing too: the auth resets still go.
        let driver = FakeDriver::new();
        *driver.failing.borrow_mut() = Some(("essid off", libc::EIO));
        assert_eq!(disconnect(&driver).unwrap_err().step, "SIOCSIWESSID off");
        assert!(driver.actions().contains(&"auth 7=0".to_string()));
    }

    #[test]
    fn scans_into_the_settings_list() {
        let driver = FakeDriver::new();
        *driver.aps.borrow_mut() = vec![
            wpa2(1, "Home", -60, Cipher::Ccmp),
            wpa2(2, "Home", -40, Cipher::Ccmp),
            open(3, "Cafe", -70),
            wpa1(4, "Old", -50),
            open(5, "", -30),
        ];
        let seen = scan(&driver, &|| Duration::ZERO, &|_| {}).unwrap();
        assert_eq!(seen.len(), 5);
        let list = networks(&seen);
        let names: Vec<&str> = list.iter().map(|n| n.ssid.as_str()).collect();
        assert_eq!(names, ["Home", "Old", "Cafe"]);
        assert_eq!(list[0].signal, Signal(-40));
        assert_eq!(list[1].security, Security::Protected);
        assert_eq!(list[2].security, Security::Open);
    }

    #[test]
    fn a_scan_retries_results_not_ready_within_a_bound() {
        let clock = std::cell::Cell::new(Duration::ZERO);
        let now = || clock.get();
        let sleep = |p| clock.set(clock.get() + p);
        let driver = FakeDriver::new();
        driver.aps.borrow_mut().push(open(3, "Cafe", -70));
        driver.eagain.set(3);
        assert_eq!(scan(&driver, &now, &sleep).unwrap().len(), 1);
        clock.set(Duration::ZERO);
        driver.eagain.set(u32::MAX);
        let error = scan(&driver, &now, &sleep).unwrap_err();
        assert_eq!(error.error.kind(), io::ErrorKind::TimedOut);
        assert!(clock.get() >= SCAN_WAIT + SCAN_RETRY);
        assert!(clock.get() < SCAN_WAIT + SCAN_RETRY + secs(1));
    }

    #[test]
    fn backs_off_doubling_to_a_minute() {
        let mut backoff = Backoff::default();
        let pauses: Vec<u64> = (0..7).map(|_| backoff.pause().as_secs()).collect();
        assert_eq!(pauses, [2, 4, 8, 16, 32, 60, 60]);
        backoff.reset();
        assert_eq!(backoff.pause(), secs(2));
    }

    fn marker(leased: Leased, ip: Ipv4Addr, at: u64) -> Option<Marker> {
        Some(Marker {
            leased,
            ip,
            at: secs(at),
        })
    }

    #[test]
    fn what_proves_a_lease() {
        let fresh = Ticket {
            pid: 7,
            since: secs(10),
            renew: false,
        };
        let renew = Ticket {
            renew: true,
            ..fresh
        };
        let ip = Some(IP);
        assert!(proves(&fresh, 7, marker(Leased::Bound, IP, 11), ip));
        // Another udhcpc's marker: a late hook of the one before.
        assert!(!proves(&fresh, 6, marker(Leased::Bound, IP, 11), ip));
        // A renew answers only a renew.
        assert!(!proves(&fresh, 7, marker(Leased::Renew, IP, 11), ip));
        assert!(proves(&renew, 7, marker(Leased::Renew, IP, 11), ip));
        // From before the ask.
        assert!(!proves(&renew, 7, marker(Leased::Renew, IP, 9), ip));
        // The address isn't (yet) the marker's.
        assert!(!proves(
            &fresh,
            7,
            marker(Leased::Bound, IP, 11),
            Some(OLD_IP)
        ));
        assert!(!proves(&fresh, 7, marker(Leased::Bound, IP, 11), None));
        assert!(!proves(&fresh, 7, None, ip));
    }

    /// `ssid` associates after `polls` and its DHCP server answers in a
    /// second with [`IP`].
    fn serves(cx: &FakeContext, ssid: &str, polls: u32) {
        cx.driver
            .joinable
            .borrow_mut()
            .push((ssid.as_bytes().to_vec(), polls));
        cx.dhcp
            .borrow_mut()
            .push((ssid.as_bytes().to_vec(), secs(1), IP));
    }

    /// `ssid` associates after `polls`, but no DHCP gets through: the
    /// wrong keys.
    fn associates_only(cx: &FakeContext, ssid: &str, polls: u32) {
        cx.driver
            .joinable
            .borrow_mut()
            .push((ssid.as_bytes().to_vec(), polls));
    }

    fn in_sight(cx: &FakeContext, bss: Bss) {
        cx.driver.aps.borrow_mut().push(bss);
    }

    fn home_in_range(cx: &FakeContext, polls: u32) {
        in_sight(cx, wpa2(1, "Home", -50, Cipher::Ccmp));
        serves(cx, "Home", polls);
    }

    fn overlaps(cx: &FakeContext) -> bool {
        cx.driver.count("SCAN DURING A CONNECT") > 0
    }

    fn joined_names(cx: &FakeContext) -> Vec<String> {
        cx.joins.borrow().iter().map(|(_, s)| s.clone()).collect()
    }

    fn fell_back(cx: &FakeContext) -> (Duration, String) {
        cx.fell_back.borrow().clone().expect("fell back")
    }

    fn settings_join(cx: &FakeContext) -> (Joined, Duration) {
        let target = Target::blind(&wpa("Home"), Cipher::Ccmp);
        let joined = join(
            &cx.driver,
            cx,
            &target,
            secs(30),
            &|| cx.clock.get(),
            &|p| cx.sleep(p),
        )
        .unwrap();
        (joined, cx.clock.get())
    }

    #[test]
    fn a_settings_join_counts_with_a_bound_lease_of_a_fresh_udhcpc() {
        let cx = FakeContext::new(vec![], secs(600));
        serves(&cx, "Home", 2);
        let (joined, took) = settings_join(&cx);
        assert_eq!(joined, Joined::Yes);
        assert_eq!(cx.restarts.get(), 1);
        assert_eq!(*cx.lease_for.borrow(), Some((101, "Home".to_string())));
        assert!(took < secs(3), "{took:?}");
        assert_eq!(cx.driver.actions().last().unwrap(), "essid Home");
    }

    #[test]
    fn associated_without_a_lease_is_a_wrong_password_and_left() {
        let cx = FakeContext::new(vec![], secs(600));
        associates_only(&cx, "Home", 2);
        let (joined, took) = settings_join(&cx);
        assert_eq!(joined, Joined::NoLease);
        assert!(cx
            .driver
            .actions()
            .iter()
            .any(|c| c.starts_with("disassociate")));
        assert_eq!(cx.driver.actions().last().unwrap(), "auth 5=0");
        assert!(
            took >= LEASE_WAIT && took < LEASE_WAIT + secs(3),
            "{took:?}"
        );
        assert_eq!(*cx.lease_for.borrow(), None);
    }

    #[test]
    fn a_late_hook_of_the_previous_udhcpc_proves_nothing() {
        // The previous udhcpc (100) held a lease; its hook lands after the
        // new association, with its address on the interface.
        let cx = FakeContext::new(vec![], secs(600));
        associates_only(&cx, "Home", 2);
        cx.address.set(Some(IP));
        cx.hook(secs(1), 100, Leased::Bound, IP);
        cx.hook(secs(2), 100, Leased::Renew, IP);
        let (joined, _) = settings_join(&cx);
        assert_eq!(joined, Joined::NoLease);
    }

    #[test]
    fn a_marker_before_its_address_waits_for_the_address() {
        // The new udhcpc's marker shows up while the old address is still on
        // the interface (events out of order); the join counts only once
        // the marker's address is there.
        let cx = FakeContext::new(vec![], secs(600));
        associates_only(&cx, "Home", 2);
        cx.address.set(Some(OLD_IP));
        cx.at(
            secs(2),
            Event::Do(Box::new(|cx| {
                let at = cx.clock.get();
                cx.markers.borrow_mut().insert(
                    101,
                    Marker {
                        leased: Leased::Bound,
                        ip: IP,
                        at,
                    },
                );
            })),
        );
        cx.at(secs(6), Event::Do(Box::new(|cx| cx.address.set(Some(IP)))));
        let (joined, took) = settings_join(&cx);
        assert_eq!(joined, Joined::Yes);
        assert!(took >= secs(6) && took < secs(7), "{took:?}");
    }

    #[test]
    fn a_proof_needs_the_same_access_point_throughout() {
        let cx = FakeContext::new(vec![], secs(600));
        cx.driver.joinable.borrow_mut().push((b"Home".to_vec(), 0));
        // Its DHCP is slow; meanwhile it roams to another access point.
        cx.dhcp.borrow_mut().push((b"Home".to_vec(), secs(5), IP));
        cx.at(
            secs(2),
            Event::Do(Box::new(|cx| {
                *cx.driver.associated.borrow_mut() = Some([9; 6]);
            })),
        );
        let (joined, took) = settings_join(&cx);
        assert_eq!(joined, Joined::NoLease);
        assert!(took < secs(3), "{took:?}");
    }

    #[test]
    fn never_associating_is_reported_as_such() {
        let cx = FakeContext::new(vec![], secs(600));
        let (joined, took) = settings_join(&cx);
        assert_eq!(joined, Joined::NotAssociated);
        assert!(took >= secs(30));
        assert!(cx.asked.borrow().is_empty());
    }

    #[test]
    fn keeps_joins_and_watches() {
        let cx = FakeContext::new(vec![wpa("Home")], secs(60));
        home_in_range(&cx, 1);
        keep(&cx);
        assert_eq!(joined_names(&cx), ["Home"]);
        let joined = cx.joins.borrow()[0].0;
        // Scan 3 s, settle 0.5 s, a poll, then the lease a second after.
        assert!(joined >= secs(4) && joined < secs(6), "{joined:?}");
        assert!(cx.fell_back.borrow().is_none());
        assert_eq!(cx.driver.count("scan"), 1);
        assert_eq!(cx.driver.pmks.get(), 1);
        assert_eq!(cx.restarts.get(), 1);
        assert!(cx.late.borrow().is_empty());
    }

    #[test]
    fn rejoins_on_its_own_when_the_firmware_does_and_renews() {
        let cx = FakeContext::new(vec![wpa("Home")], secs(120));
        home_in_range(&cx, 1);
        cx.at(secs(30), Event::Do(Box::new(|cx| cx.driver.drop_link())));
        cx.at(
            secs(32),
            Event::Do(Box::new(|cx| cx.driver.on("Home", [9; 6]))),
        );
        keep(&cx);
        assert_eq!(cx.joins.borrow().len(), 2);
        assert_eq!(cx.driver.pmks.get(), 1, "no new connect was needed");
        // The same network: the same udhcpc renews.
        assert_eq!(cx.restarts.get(), 1);
        assert_eq!(cx.renews.get(), 1);
    }

    #[test]
    fn a_firmware_rejoin_without_a_lease_is_left_and_counted() {
        let cx = FakeContext::new(vec![wpa("Home")], secs(600));
        home_in_range(&cx, 1);
        // Dropped at 30 s, the firmware rejoins, but no DHCP gets through
        // any more (the password changed on the router).
        cx.at(
            secs(30),
            Event::Do(Box::new(|cx| {
                cx.driver.drop_link();
                cx.dhcp.borrow_mut().clear();
            })),
        );
        cx.at(
            secs(32),
            Event::Do(Box::new(|cx| cx.driver.on("Home", [9; 6]))),
        );
        keep(&cx);
        assert_eq!(cx.joins.borrow().len(), 1);
        assert!(cx
            .driver
            .actions()
            .iter()
            .any(|c| c == "disassociate 09:09:09:09:09:09"));
        let (when, why) = fell_back(&cx);
        assert!(why.contains("Home"), "{why}");
        assert!(when >= secs(62) && when < secs(140), "{when:?}");
    }

    #[test]
    fn reconnects_with_the_pmk_again_when_the_firmware_doesnt() {
        let cx = FakeContext::new(vec![wpa("Home")], secs(120));
        home_in_range(&cx, 1);
        cx.at(secs(30), Event::Do(Box::new(|cx| cx.driver.drop_link())));
        keep(&cx);
        assert_eq!(cx.joins.borrow().len(), 2);
        assert_eq!(cx.driver.pmks.get(), 2);
        let (rejoined, _) = cx.joins.borrow()[1].clone();
        assert!(rejoined > secs(30) + REJOIN_GRACE);
        assert!(rejoined < secs(30) + REJOIN_GRACE + secs(10));
    }

    #[test]
    fn after_a_suspend_goes_straight_back_with_the_pmk_and_renews() {
        let cx = FakeContext::new(vec![wpa("Home")], secs(60));
        home_in_range(&cx, 1);
        cx.at(secs(20), Event::Kick(Kick::Resumed));
        keep(&cx);
        assert_eq!(cx.driver.pmks.get(), 2);
        assert_eq!(cx.driver.count("scan"), 1);
        assert_eq!(cx.joins.borrow().len(), 2);
        assert_eq!((cx.restarts.get(), cx.renews.get()), (1, 1));
        assert!(cx.fell_back.borrow().is_none());
    }

    #[test]
    fn a_forgotten_network_is_left_for_another_with_a_fresh_udhcpc() {
        let cx = FakeContext::new(vec![wpa("Home"), wpa("Phone")], secs(60));
        home_in_range(&cx, 1);
        in_sight(&cx, wpa2(2, "Phone", -70, Cipher::Ccmp));
        serves(&cx, "Phone", 1);
        cx.at(
            secs(20),
            Event::Do(Box::new(|cx| {
                cx.saved.borrow_mut().remove(0);
            })),
        );
        cx.at(secs(20), Event::Kick(Kick::Saved));
        keep(&cx);
        assert_eq!(joined_names(&cx), ["Home", "Phone"]);
        assert_eq!(cx.restarts.get(), 2);
        assert_eq!(*cx.lease_for.borrow(), Some((102, "Phone".to_string())));
    }

    #[test]
    fn forgetting_the_last_network_leaves_it() {
        let cx = FakeContext::new(vec![wpa("Home")], secs(120));
        home_in_range(&cx, 1);
        cx.at(
            secs(20),
            Event::Do(Box::new(|cx| cx.saved.borrow_mut().clear())),
        );
        cx.at(secs(20), Event::Kick(Kick::Saved));
        keep(&cx);
        assert_eq!(joined_names(&cx), ["Home"]);
        assert!(cx.driver.associated.borrow().is_none());
        let after: Vec<String> = cx.driver.actions();
        let left = after
            .iter()
            .position(|c| c == "disassociate 80:69:01:02:03:04")
            .expect("left");
        // Nothing joined after that.
        assert!(!after[left..].iter().any(|c| c.starts_with("essid H")));
        assert!(cx.fell_back.borrow().is_none());
    }

    #[test]
    fn a_kick_while_still_on_a_saved_network_changes_nothing() {
        let cx = FakeContext::new(vec![wpa("Home")], secs(60));
        home_in_range(&cx, 1);
        cx.at(secs(20), Event::Kick(Kick::Saved));
        keep(&cx);
        assert_eq!(cx.joins.borrow().len(), 1);
        assert_eq!(cx.driver.pmks.get(), 1);
        assert_eq!(cx.asked.borrow().len(), 1);
    }

    #[test]
    fn falls_back_after_thirty_seconds_in_range_without_joining() {
        let cx = FakeContext::new(vec![wpa("Home")], secs(600));
        in_sight(&cx, wpa2(1, "Home", -50, Cipher::Ccmp));
        keep(&cx);
        let (when, why) = fell_back(&cx);
        assert!(why.contains("Home"), "{why}");
        assert!(when >= FALLBACK_AFTER && when < secs(60), "{when:?}");
        assert!(cx.joins.borrow().is_empty());
    }

    #[test]
    fn associated_without_a_lease_never_counts_and_falls_back() {
        let cx = FakeContext::new(vec![wpa("Home")], secs(600));
        in_sight(&cx, wpa2(1, "Home", -50, Cipher::Ccmp));
        associates_only(&cx, "Home", 1);
        keep(&cx);
        assert!(cx.joins.borrow().is_empty());
        assert!(!cx.asked.borrow().is_empty());
        let (when, why) = fell_back(&cx);
        assert!(why.contains("Home"), "{why}");
        assert!(when >= FALLBACK_AFTER && when < secs(90), "{when:?}");
        assert!(cx
            .driver
            .actions()
            .iter()
            .any(|c| c.starts_with("disassociate")));
    }

    #[test]
    fn associated_at_start_without_a_lease_is_left_and_falls_back() {
        // Finding 2: the firmware is on Home when kindil starts, but no DHCP
        // gets through. It must not prove the same association forever.
        let cx = FakeContext::new(vec![wpa("Home")], secs(600));
        in_sight(&cx, wpa2(1, "Home", -50, Cipher::Ccmp));
        associates_only(&cx, "Home", 1);
        cx.driver.on("Home", BSSID);
        keep(&cx);
        assert!(cx.joins.borrow().is_empty());
        let (when, _) = fell_back(&cx);
        assert!(when >= FALLBACK_AFTER && when < secs(90), "{when:?}");
        assert!(cx
            .driver
            .actions()
            .iter()
            .any(|c| c.starts_with("disassociate")));
        // With pauses between tries.
        assert!(cx.asked.borrow().len() <= 4, "{:?}", cx.asked.borrow());
    }

    #[test]
    fn associated_at_start_with_a_lease_is_kept() {
        let cx = FakeContext::new(vec![wpa("Home")], secs(30));
        home_in_range(&cx, 0);
        cx.driver.on("Home", BSSID);
        keep(&cx);
        assert_eq!(cx.joins.borrow().len(), 1);
        assert_eq!(cx.driver.pmks.get(), 0);
        assert_eq!(cx.restarts.get(), 1);
    }

    #[test]
    fn hidden_profiles_are_tried_in_turn() {
        // Finding 3: the first saved network is nowhere; the second is
        // hidden and in range.
        let cx = FakeContext::new(vec![wpa("Away"), wpa("Hidden")], secs(120));
        serves(&cx, "Hidden", 1);
        keep(&cx);
        assert_eq!(joined_names(&cx), ["Hidden"]);
        assert!(cx.fell_back.borrow().is_none());
        let essids: Vec<String> = cx
            .driver
            .actions()
            .into_iter()
            .filter(|c| c.starts_with("essid ") && c != "essid off")
            .collect();
        assert_eq!(essids, ["essid Away", "essid Hidden"]);
    }

    #[test]
    fn a_hidden_network_with_a_tkip_group_joins_on_its_second_try() {
        let cx = FakeContext::new(vec![wpa("Hidden")], secs(120));
        serves(&cx, "Hidden", 1);
        cx.driver
            .group_needed
            .borrow_mut()
            .push((b"Hidden".to_vec(), wext::IW_AUTH_CIPHER_TKIP));
        keep(&cx);
        assert_eq!(joined_names(&cx), ["Hidden"]);
        assert_eq!(cx.driver.pmks.get(), 2);
    }

    #[test]
    fn a_network_seen_before_is_joined_unseen_as_it_was_seen() {
        let cx = FakeContext::new(vec![wpa("Home")], secs(200));
        in_sight(&cx, wpa2(1, "Home", -50, Cipher::Tkip));
        serves(&cx, "Home", 1);
        cx.driver
            .group_needed
            .borrow_mut()
            .push((b"Home".to_vec(), wext::IW_AUTH_CIPHER_TKIP));
        // Seen and joined, then gone from scans (hidden from now on), then
        // dropped.
        cx.at(
            secs(20),
            Event::Do(Box::new(|cx| {
                cx.driver.aps.borrow_mut().clear();
                cx.driver.drop_link();
            })),
        );
        keep(&cx);
        assert_eq!(joined_names(&cx), ["Home", "Home"]);
        // Its first unseen try already had the TKIP group.
        assert_eq!(cx.driver.pmks.get(), 2);
    }

    #[test]
    fn every_saved_network_failing_unseen_twice_falls_back() {
        let cx = FakeContext::new(vec![wpa("Away"), wpa("Gone")], secs(3600));
        keep(&cx);
        let (when, why) = fell_back(&cx);
        assert!(why.contains("unseen"), "{why}");
        let tries = cx.driver.count("essid Away") + cx.driver.count("essid Gone");
        assert_eq!(tries as u32, 2 * BLIND_TRIES);
        assert!(when < secs(200), "{when:?}");
    }

    #[test]
    fn an_unsupported_saved_network_falls_back_with_the_reason() {
        let cx = FakeContext::new(vec![wpa("Old")], secs(60));
        in_sight(&cx, wpa1(1, "Old", -40));
        keep(&cx);
        let (when, why) = fell_back(&cx);
        assert!(why.contains("Old") && why.contains("WPA (TKIP)"), "{why}");
        assert!(when < secs(5));
    }

    #[test]
    fn a_driver_that_keeps_failing_falls_back() {
        let cx = FakeContext::new(vec![wpa("Home")], secs(600));
        home_in_range(&cx, 1);
        *cx.driver.failing.borrow_mut() = Some(("auth 7=1", libc::EIO));
        keep(&cx);
        let (_, why) = fell_back(&cx);
        assert!(why.contains("SIOCSIWAUTH"), "{why}");
        assert_eq!(cx.driver.count("auth 7=1") as u32, STRIKES);
    }

    #[test]
    fn one_driver_error_is_forgiven() {
        let cx = FakeContext::new(vec![wpa("Home")], secs(60));
        home_in_range(&cx, 1);
        *cx.driver.failing.borrow_mut() = Some(("scan", libc::EIO));
        cx.at(
            secs(1),
            Event::Do(Box::new(|cx| *cx.driver.failing.borrow_mut() = None)),
        );
        keep(&cx);
        assert!(cx.fell_back.borrow().is_none());
        assert_eq!(cx.joins.borrow().len(), 1);
    }

    #[test]
    fn scan_results_not_ready_are_read_again_and_then_fail() {
        let cx = FakeContext::new(vec![wpa("Home")], secs(60));
        home_in_range(&cx, 1);
        cx.driver.eagain.set(4);
        keep(&cx);
        assert_eq!(cx.joins.borrow().len(), 1);
        // Not ready, ever: a strike per scan, then the fallback.
        let cx = FakeContext::new(vec![wpa("Home")], secs(600));
        home_in_range(&cx, 1);
        cx.driver.eagain.set(u32::MAX);
        keep(&cx);
        let (when, why) = fell_back(&cx);
        assert!(why.contains("SIOCGIWSCAN"), "{why}");
        assert!(when < secs(60), "{when:?}");
    }

    #[test]
    fn a_stop_during_scan_retries_is_noticed_at_once() {
        // The stack stops 4.5 s in, during the retries (SCAN_WAIT is 3 s):
        // the keeper lets go of the lock between reads, so it stops then.
        let cx = FakeContext::new(vec![wpa("Home")], ms(4500));
        home_in_range(&cx, 1);
        cx.driver.eagain.set(u32::MAX);
        keep(&cx);
        assert!(cx.clock.get() < ms(5000), "{:?}", cx.clock.get());
        assert!(cx.fell_back.borrow().is_none());
    }

    #[test]
    fn a_stop_mid_proof_publishes_nothing() {
        // A newer generation 4.7 s in: after the association, before the
        // lease (at about 5 s).
        let cx = FakeContext::new(vec![wpa("Home")], ms(4700));
        home_in_range(&cx, 1);
        cx.at(secs(4), Event::Kick(Kick::Scan));
        keep(&cx);
        assert!(cx.joins.borrow().is_empty());
        assert!(cx.scans.borrow().is_empty());
        assert_eq!(*cx.lease_for.borrow(), None);
        assert!(cx.fell_back.borrow().is_none());
        assert!(cx.clock.get() < secs(6));
    }

    #[test]
    fn no_saved_networks_idles_until_one_is_added() {
        let cx = FakeContext::new(vec![], secs(200));
        home_in_range(&cx, 1);
        cx.at(
            secs(100),
            Event::Do(Box::new(|cx| cx.saved.borrow_mut().push(wpa("Home")))),
        );
        cx.at(secs(100), Event::Kick(Kick::Saved));
        keep(&cx);
        let joins = cx.joins.borrow();
        assert_eq!(joins.len(), 1);
        assert!(joins[0].0 >= secs(100) && joins[0].0 < secs(110));
        assert!(cx.fell_back.borrow().is_none());
    }

    #[test]
    fn a_scan_asked_for_during_a_connect_waits_for_it() {
        let cx = FakeContext::new(vec![wpa("Home")], secs(60));
        home_in_range(&cx, 20);
        cx.at(secs(5), Event::Kick(Kick::Scan));
        keep(&cx);
        assert!(!overlaps(&cx), "{:?}", cx.driver.calls.borrow());
        let joined = cx.joins.borrow()[0].0;
        let scans = cx.scans.borrow();
        assert_eq!(scans.len(), 1);
        let (when, found) = scans[0].clone();
        assert!(
            when > joined && when <= joined + SCAN_WAIT + secs(1),
            "{when:?} {joined:?}"
        );
        assert_eq!(found, Ok(1));
    }

    #[test]
    fn a_scan_while_watching_is_done_at_once_and_keeps_the_link() {
        let cx = FakeContext::new(vec![wpa("Home")], secs(60));
        home_in_range(&cx, 1);
        cx.at(secs(20), Event::Kick(Kick::Scan));
        keep(&cx);
        let (when, _) = cx.scans.borrow()[0].clone();
        assert!(when >= secs(20) && when <= secs(20) + SCAN_WAIT, "{when:?}");
        assert_eq!(cx.joins.borrow().len(), 1);
        assert_eq!(cx.driver.pmks.get(), 1);
        assert!(!overlaps(&cx));
    }

    #[test]
    fn a_scan_between_failed_tries_finds_no_connect_pending() {
        let cx = FakeContext::new(vec![wpa("Home"), wpa("Work")], secs(120));
        for t in [12, 30, 45, 70] {
            cx.at(secs(t), Event::Kick(Kick::Scan));
        }
        keep(&cx);
        assert!(!cx.scans.borrow().is_empty());
        assert!(!overlaps(&cx), "{:?}", cx.driver.calls.borrow());
    }

    #[test]
    fn a_failing_scan_answers_with_the_error() {
        let cx = FakeContext::new(vec![wpa("Home")], secs(60));
        home_in_range(&cx, 1);
        cx.at(
            secs(20),
            Event::Do(Box::new(|cx| {
                *cx.driver.failing.borrow_mut() = Some(("scan results", libc::EIO))
            })),
        );
        cx.at(secs(20), Event::Kick(Kick::Scan));
        cx.at(
            secs(25),
            Event::Do(Box::new(|cx| *cx.driver.failing.borrow_mut() = None)),
        );
        keep(&cx);
        let (_, result) = cx.scans.borrow()[0].clone();
        assert!(result.unwrap_err().contains("SIOCGIWSCAN"));
        assert!(cx.fell_back.borrow().is_none());
    }
}
