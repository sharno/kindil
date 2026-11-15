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
//! This module is the logic, over a [`Driver`]: which saved network to join
//! from a scan, the ioctl sequence, and [`keep`], the loop that joins, watches
//! the access point and joins again, with a growing pause between failures.
//! src/net.rs runs it beside udhcpc, under its lifecycle lock, and falls back
//! to wpa_supplicant for the rest of the run when [`keep`] gives up: no
//! association within [`FALLBACK_AFTER`] on a network in range, a saved
//! network kindil can't join this way, or the driver failing.
use crate::host::Signal;
use crate::wext::{self, Akm, Bss, Cipher, Driver};
use crate::wifi::{Credentials, Network, Security};
use std::collections::BTreeMap;
use std::io;
use std::time::Duration;

/// `kindil` in it: kindil's own Wi-Fi. Absent or anything else: wpa_supplicant.
pub const SWITCH: &str = "/var/local/kindil/wifi";
/// One try at joining.
pub const ASSOC_WAIT: Duration = Duration::from_secs(10);
/// Trying this long on a network in range without a lasting association
/// gives up on kindil's own Wi-Fi for the run.
pub const FALLBACK_AFTER: Duration = Duration::from_secs(30);
/// How often the access point is checked while associated.
pub const WATCH: Duration = Duration::from_secs(3);
/// After losing the access point, the firmware keeps trying to rejoin it by
/// itself (any disconnect but the host's own); it gets this long.
pub const REJOIN_GRACE: Duration = Duration::from_secs(10);
/// An association has to hold this long to count: a wrong password can
/// associate and then fail the handshake.
pub const STABLE: Duration = Duration::from_secs(3);
/// The driver's scan of every channel takes about two seconds.
pub const SCAN_WAIT: Duration = Duration::from_secs(3);
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

    /// Joining without having seen it: the usual WPA2 with CCMP.
    pub fn blind(credentials: &Credentials) -> Self {
        let plan = match credentials {
            Credentials::Open(_) => Plan::Open,
            Credentials::Wpa(..) => Plan::Wpa2 {
                group: Cipher::Ccmp,
            },
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
    /// None of the saved networks in sight (maybe hidden): the first, blind.
    Blind(Target),
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
    match (refused, saved.first()) {
        (Some((ssid, why)), _) => Choice::Unsupported { ssid, why },
        (None, Some(first)) => Choice::Blind(Target::blind(first)),
        (None, None) => Choice::Nothing,
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

/// Leaves the network and stops the firmware chasing it: a disassociation
/// from the current access point, then "essid off".
pub fn disconnect(driver: &impl Driver) -> Result<(), DriverError> {
    if let Some(bssid) = driver.bssid().map_err(at("SIOCGIWAP"))? {
        driver
            .disassociate(bssid)
            .map_err(at("SIOCSIWMLME DISASSOC"))?;
    }
    driver.set_essid(None).map_err(at("SIOCSIWESSID off"))
}

/// A fresh scan.
pub fn scan(driver: &impl Driver, sleep: &dyn Fn(Duration)) -> Result<Vec<Bss>, DriverError> {
    driver.set_up().map_err(at("SIOCSIFFLAGS up"))?;
    driver.trigger_scan().map_err(at("SIOCSIWSCAN"))?;
    sleep(SCAN_WAIT);
    let stream = driver.scan_results().map_err(at("SIOCGIWSCAN"))?;
    Ok(wext::parse_scan(&stream))
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

/// Joins `target` now, for Settings: Ok(true) once associated with it for
/// [`STABLE`], Ok(false) if not within `limit`.
pub fn join(
    driver: &impl Driver,
    target: &Target,
    limit: Duration,
    now: &dyn Fn() -> Duration,
    sleep: &dyn Fn(Duration),
) -> Result<bool, DriverError> {
    let started = now();
    connect(driver, target, sleep)?;
    let mut since = None;
    while now() - started < limit {
        let on = match driver.bssid().map_err(at("SIOCGIWAP"))? {
            Some(_) => driver.essid().map_err(at("SIOCGIWESSID"))? == target.ssid().as_bytes(),
            None => false,
        };
        if on {
            let since = *since.get_or_insert(now());
            if now() - since >= STABLE {
                return Ok(true);
            }
        } else {
            since = None;
        }
        sleep(POLL);
    }
    Ok(false)
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

/// Why [`keep`] should look again now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kick {
    /// The saved networks changed: still on one of them?
    Saved,
    /// Back from a suspend: join again (the PMK again too).
    Resumed,
}

/// What [`keep`] runs in: src/net.rs's stack, or the tests' fake.
pub trait Context {
    type D: Driver;
    /// Monotonic time.
    fn now(&self) -> Duration;
    fn sleep(&self, pause: Duration);
    /// Waits up to `pause`, or until kicked; takes the kick.
    fn wait(&self, pause: Duration) -> Option<Kick>;
    /// `f` with the driver and the saved networks, under the lifecycle
    /// lock, while this keeper's stack is still the one wanted.
    fn with<R>(&self, f: impl FnOnce(&Self::D, &[Credentials]) -> R) -> Option<R>;
    /// Associated after not being: renew the address.
    fn joined(&self, ssid: &str);
    /// Gives up on kindil's own Wi-Fi for the run.
    fn fall_back(&self, why: &str);
}

enum Attempted {
    /// Already on a saved network.
    Already(Vec<u8>),
    Connecting {
        target: Target,
        in_range: bool,
    },
    Unsupported {
        ssid: String,
        why: &'static str,
    },
    Nothing,
}

fn attempt(
    driver: &impl Driver,
    saved: &[Credentials],
    last: Option<&Target>,
    resumed: bool,
    sleep: &dyn Fn(Duration),
) -> Result<Attempted, DriverError> {
    if !resumed && driver.bssid().map_err(at("SIOCGIWAP"))?.is_some() {
        let essid = driver.essid().map_err(at("SIOCGIWESSID"))?;
        if saved.iter().any(|c| c.ssid().as_str().as_bytes() == essid) {
            return Ok(Attempted::Already(essid));
        }
    }
    // After a suspend: straight back to the network it was on.
    if let Some(target) = last.filter(|t| resumed && saved.contains(&t.credentials)) {
        connect(driver, target, sleep)?;
        return Ok(Attempted::Connecting {
            target: target.clone(),
            in_range: true,
        });
    }
    if saved.is_empty() {
        return Ok(Attempted::Nothing);
    }
    let seen = scan(driver, sleep)?;
    let (target, in_range) = match choose(saved, &seen) {
        Choice::Join(target) => (target, true),
        Choice::Blind(target) => (target, false),
        Choice::Unsupported { ssid, why } => return Ok(Attempted::Unsupported { ssid, why }),
        Choice::Nothing => return Ok(Attempted::Nothing),
    };
    connect(driver, &target, sleep)?;
    Ok(Attempted::Connecting { target, in_range })
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
    associated: bool,
}

/// Joins the best saved network and keeps it joined, until the stack stops
/// ([`Context::with`] gives `None`) or it gives up ([`Context::fall_back`]).
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
                if let Some(kick) = cx.wait(WATCH) {
                    return Some(State::Attempt(Some(kick)));
                }
                match cx.with(|d, _| d.bssid())? {
                    Err(error) => self.strike(cx, at("SIOCGIWAP")(error), State::Watch),
                    Ok(Some(_)) => {
                        // Held: a lasting association.
                        self.strikes = 0;
                        self.backoff.reset();
                        self.failing_since = None;
                        Some(State::Watch)
                    }
                    Ok(None) => {
                        self.strikes = 0;
                        self.associated = false;
                        eprintln!("kindil: Wi-Fi: lost the access point");
                        Some(State::Lost)
                    }
                }
            }
            State::Lost => match self.wait_associated(cx, REJOIN_GRACE)? {
                Ok(true) => {
                    self.on_associated(cx, "the same network");
                    Some(State::Watch)
                }
                Ok(false) => Some(State::Attempt(None)),
                Err(error) => self.strike(cx, error, State::Attempt(None)),
            },
        }
    }

    fn attempt<X: Context>(&mut self, cx: &X, kick: Option<Kick>) -> Option<State> {
        let resumed = kick == Some(Kick::Resumed);
        let last = self.last.clone();
        let tried =
            cx.with(|d, saved| attempt(d, saved, last.as_ref(), resumed, &|p| cx.sleep(p)))?;
        let tried = match tried {
            Ok(tried) => tried,
            Err(error) => return self.strike(cx, error, State::Attempt(None)),
        };
        self.strikes = 0;
        match tried {
            Attempted::Nothing => Some(State::Attempt(cx.wait(IDLE))),
            Attempted::Unsupported { ssid, why } => {
                cx.fall_back(&format!("can't join {ssid}: {why}"));
                None
            }
            Attempted::Already(ssid) => {
                if self
                    .last
                    .as_ref()
                    .is_some_and(|t| t.ssid().as_bytes() != ssid)
                {
                    self.last = None;
                }
                self.on_associated(cx, &String::from_utf8_lossy(&ssid));
                Some(State::Watch)
            }
            Attempted::Connecting { target, in_range } => {
                self.associated = false;
                if in_range {
                    // Cleared only by an association that holds.
                    self.failing_since.get_or_insert(cx.now());
                    if let Some(why) = self.overdue(cx, &target) {
                        cx.fall_back(&why);
                        return None;
                    }
                }
                match self.wait_associated(cx, ASSOC_WAIT)? {
                    Err(error) => self.strike(cx, error, State::Attempt(None)),
                    Ok(true) => {
                        self.on_associated(cx, target.ssid());
                        self.last = Some(target);
                        Some(State::Watch)
                    }
                    Ok(false) => {
                        if let Some(why) = self.overdue(cx, &target).filter(|_| in_range) {
                            cx.fall_back(&why);
                            return None;
                        }
                        let pause = self.backoff.pause();
                        eprintln!(
                            "kindil: Wi-Fi: {} not joined; trying again in {pause:?}",
                            target.ssid()
                        );
                        Some(State::Attempt(cx.wait(pause)))
                    }
                }
            }
        }
    }

    /// Failing on a network in range for [`FALLBACK_AFTER`]: why, to give up.
    fn overdue<X: Context>(&self, cx: &X, target: &Target) -> Option<String> {
        let since = self.failing_since?;
        (cx.now() - since >= FALLBACK_AFTER).then(|| {
            format!(
                "no lasting association with {} within {} s",
                target.ssid(),
                FALLBACK_AFTER.as_secs()
            )
        })
    }

    fn on_associated<X: Context>(&mut self, cx: &X, ssid: &str) {
        if !self.associated {
            eprintln!("kindil: Wi-Fi: joined {ssid}");
            cx.joined(ssid);
        }
        self.associated = true;
    }

    /// Polls until associated or `limit` passes.
    fn wait_associated<X: Context>(
        &mut self,
        cx: &X,
        limit: Duration,
    ) -> Option<Result<bool, DriverError>> {
        let started = cx.now();
        loop {
            match cx.with(|d, _| d.bssid())? {
                Err(error) => return Some(Err(at("SIOCGIWAP")(error))),
                Ok(Some(_)) => return Some(Ok(true)),
                Ok(None) => {}
            }
            if cx.now() - started >= limit {
                return Some(Ok(false));
            }
            cx.sleep(POLL);
        }
    }

    /// A driver error: a pause and on, or giving up after [`STRIKES`].
    fn strike<X: Context>(&mut self, cx: &X, error: DriverError, next: State) -> Option<State> {
        self.strikes += 1;
        eprintln!("kindil: Wi-Fi: {error}");
        if self.strikes >= STRIKES {
            cx.fall_back(&format!("the Wi-Fi driver keeps failing ({error})"));
            return None;
        }
        Some(match cx.wait(self.backoff.pause()) {
            Some(kick) => State::Attempt(Some(kick)),
            None => next,
        })
    }
}

#[cfg(test)]
pub mod fake {
    //! A driver that answers like the AR6003's and records each call, and a
    //! context with a clock that only moves when slept on.
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
        /// The step (a prefix of a call's name) that fails, with an errno.
        pub failing: RefCell<Option<(&'static str, i32)>>,
        pub associated: RefCell<Option<[u8; 6]>>,
        essid: RefCell<Vec<u8>>,
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

        /// The access point drops the station.
        pub fn drop_link(&self) {
            *self.associated.borrow_mut() = None;
            self.polls_left.set(None);
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
    }

    impl Driver for FakeDriver {
        fn set_up(&self) -> io::Result<()> {
            self.call("up".into())
        }
        fn set_auth(&self, index: u16, value: i32) -> io::Result<()> {
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
                    let polls = self
                        .joinable
                        .borrow()
                        .iter()
                        .find(|(name, _)| name == ssid)
                        .map(|(_, polls)| *polls);
                    self.polls_left.set(polls);
                }
            }
            Ok(())
        }
        fn bssid(&self) -> io::Result<Option<[u8; 6]>> {
            self.call("bssid".into())?;
            if self.associated.borrow().is_none() {
                match self.polls_left.get() {
                    Some(0) => *self.associated.borrow_mut() = Some([0x80, 0x69, 1, 2, 3, 4]),
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
            self.call("scan".into())
        }
        fn scan_results(&self) -> io::Result<Vec<u8>> {
            self.call("scan results".into())?;
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

    /// A change to the driver or the saved networks.
    pub type Change = Box<dyn Fn(&FakeDriver, &mut Vec<Credentials>)>;

    /// Something to happen at a time: a kick, or a change.
    pub enum Event {
        Kick(Kick),
        Do(Change),
    }

    pub struct FakeContext {
        pub driver: FakeDriver,
        pub saved: RefCell<Vec<Credentials>>,
        pub clock: Cell<Duration>,
        /// The stack stops at this time.
        pub until: Duration,
        pub events: RefCell<VecDeque<(Duration, Event)>>,
        pending: Cell<Option<Kick>>,
        pub joins: RefCell<Vec<(Duration, String)>>,
        pub fell_back: RefCell<Option<(Duration, String)>>,
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
                joins: RefCell::default(),
                fell_back: RefCell::default(),
            }
        }

        pub fn at(&self, time: Duration, event: Event) {
            let mut events = self.events.borrow_mut();
            events.push_back((time, event));
            events.make_contiguous().sort_by_key(|(t, _)| *t);
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
                    Some((_, Event::Kick(kick))) => self.pending.set(Some(kick)),
                    Some((_, Event::Do(change))) => {
                        change(&self.driver, &mut self.saved.borrow_mut())
                    }
                    None => return,
                }
            }
        }

        fn advance(&self, pause: Duration) {
            self.clock.set(self.clock.get() + pause);
            self.fire();
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
                if let Some(kick) = self.pending.take() {
                    return Some(kick);
                }
                let next_event = self.events.borrow().front().map(|(t, _)| *t);
                let step = match next_event {
                    Some(t) if t > self.clock.get() && t < end => t - self.clock.get(),
                    _ => end - self.clock.get(),
                };
                self.advance(step);
            }
            self.pending.take()
        }
        fn with<R>(&self, f: impl FnOnce(&FakeDriver, &[Credentials]) -> R) -> Option<R> {
            self.fire();
            if self.clock.get() >= self.until || self.fell_back.borrow().is_some() {
                return None;
            }
            let saved = self.saved.borrow().clone();
            Some(f(&self.driver, &saved))
        }
        fn joined(&self, ssid: &str) {
            self.joins
                .borrow_mut()
                .push((self.clock.get(), ssid.to_owned()));
        }
        fn fall_back(&self, why: &str) {
            *self.fell_back.borrow_mut() = Some((self.clock.get(), why.to_owned()));
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
        // WPA3 transition mode offers PSK too.
        sae.rsn.as_mut().unwrap().akm = vec![Akm::Sae, Akm::Psk];
        assert!(matches!(kind(&sae), Kind::Wpa2 { .. }));
        // Mixed WPA/WPA2: the RSN element decides.
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
        // Home in range wins, through its strongest access point's ciphers.
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
        // Saved open, now protected.
        let Choice::Unsupported { why, .. } =
            choose(&[open_net("Home")], &[wpa2(2, "Home", -70, Cipher::Ccmp)])
        else {
            panic!()
        };
        assert!(why.contains("password"));
    }

    #[test]
    fn nothing_in_sight_is_a_blind_try_on_the_first() {
        let saved = [wpa("Hidden"), open_net("Cafe")];
        let Choice::Blind(t) = choose(&saved, &[open(1, "Other", -50)]) else {
            panic!()
        };
        assert_eq!(t.ssid(), "Hidden");
        assert_eq!(
            t.plan,
            Plan::Wpa2 {
                group: Cipher::Ccmp
            }
        );
        assert!(matches!(
            choose(&[], &[open(1, "Other", -50)]),
            Choice::Nothing
        ));
    }

    #[test]
    fn the_proven_ioctl_sequence() {
        let driver = FakeDriver::new();
        let target = Target {
            credentials: wpa("Home"),
            plan: Plan::Wpa2 {
                group: Cipher::Ccmp,
            },
        };
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
        // TKIP group.
        let driver = FakeDriver::new();
        let target = Target {
            plan: Plan::Wpa2 {
                group: Cipher::Tkip,
            },
            ..target
        };
        connect(&driver, &target, &|_| {}).unwrap();
        assert!(driver.actions().contains(&"auth 2=4".to_string()));
        // Open: no profile, no key.
        let driver = FakeDriver::new();
        connect(&driver, &Target::blind(&open_net("Cafe")), &|_| {}).unwrap();
        assert_eq!(driver.actions(), ["up", "essid off", "essid Cafe"]);
    }

    #[test]
    fn a_failing_ioctl_stops_the_sequence_and_names_itself() {
        let driver = FakeDriver::new();
        *driver.failing.borrow_mut() = Some(("pmk", libc::EINVAL));
        let error = connect(&driver, &Target::blind(&wpa("Home")), &|_| {}).unwrap_err();
        assert_eq!(error.step, "SIOCSIWENCODEEXT PMK");
        assert!(!driver.actions().iter().any(|c| c.starts_with("essid Home")));
    }

    #[test]
    fn disconnecting_disassociates_then_turns_the_essid_off() {
        let driver = FakeDriver::new();
        *driver.associated.borrow_mut() = Some([0x80, 0x69, 1, 2, 3, 4]);
        disconnect(&driver).unwrap();
        assert_eq!(
            driver.actions(),
            ["disassociate 80:69:01:02:03:04", "essid off"]
        );
        let driver = FakeDriver::new();
        disconnect(&driver).unwrap();
        assert_eq!(driver.actions(), ["essid off"]);
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
        let seen = scan(&driver, &|_| {}).unwrap();
        assert_eq!(seen.len(), 5);
        let list = networks(&seen);
        let names: Vec<&str> = list.iter().map(|n| n.ssid.as_str()).collect();
        assert_eq!(names, ["Home", "Old", "Cafe"]);
        assert_eq!(list[0].signal, Signal(-40));
        assert_eq!(list[1].security, Security::Protected);
        assert_eq!(list[2].security, Security::Open);
    }

    #[test]
    fn joining_for_settings_waits_for_a_lasting_association() {
        let clock = std::cell::Cell::new(Duration::ZERO);
        let now = || clock.get();
        let sleep = |p| clock.set(clock.get() + p);
        let driver = FakeDriver::new();
        driver.joinable.borrow_mut().push((b"Home".to_vec(), 2));
        let target = Target::blind(&wpa("Home"));
        assert!(join(&driver, &target, secs(30), &now, &sleep).unwrap());
        assert!(clock.get() >= STABLE);
        // A wrong password: never associates.
        clock.set(Duration::ZERO);
        let driver = FakeDriver::new();
        assert!(!join(&driver, &target, secs(30), &now, &sleep).unwrap());
        assert!(clock.get() >= secs(30));
    }

    #[test]
    fn backs_off_doubling_to_a_minute() {
        let mut backoff = Backoff::default();
        let pauses: Vec<u64> = (0..7).map(|_| backoff.pause().as_secs()).collect();
        assert_eq!(pauses, [2, 4, 8, 16, 32, 60, 60]);
        backoff.reset();
        assert_eq!(backoff.pause(), secs(2));
    }

    fn home_in_range(cx: &FakeContext, polls: u32) {
        cx.driver
            .aps
            .borrow_mut()
            .push(wpa2(1, "Home", -50, Cipher::Ccmp));
        cx.driver
            .joinable
            .borrow_mut()
            .push((b"Home".to_vec(), polls));
    }

    #[test]
    fn keeps_joins_and_watches() {
        let cx = FakeContext::new(vec![wpa("Home")], secs(60));
        home_in_range(&cx, 1);
        keep(&cx);
        assert_eq!(cx.joins.borrow().len(), 1);
        assert_eq!(cx.joins.borrow()[0].1, "Home");
        assert!(cx.fell_back.borrow().is_none());
        // Scanned once, joined once, then only watched.
        let actions = cx.driver.actions();
        assert_eq!(actions.iter().filter(|c| *c == "scan").count(), 1);
        assert_eq!(cx.driver.pmks.get(), 1);
    }

    #[test]
    fn rejoins_on_its_own_when_the_firmware_does() {
        let cx = FakeContext::new(vec![wpa("Home")], secs(120));
        home_in_range(&cx, 1);
        // The AP drops it; the firmware rejoins 2 s later.
        cx.at(secs(30), Event::Do(Box::new(|d, _| d.drop_link())));
        cx.at(
            secs(32),
            Event::Do(Box::new(|d, _| {
                *d.associated.borrow_mut() = Some([9; 6]);
            })),
        );
        keep(&cx);
        assert_eq!(cx.joins.borrow().len(), 2);
        assert_eq!(cx.driver.pmks.get(), 1, "no new connect was needed");
    }

    #[test]
    fn reconnects_with_the_pmk_again_when_the_firmware_doesnt() {
        let cx = FakeContext::new(vec![wpa("Home")], secs(120));
        home_in_range(&cx, 1);
        cx.at(secs(30), Event::Do(Box::new(|d, _| d.drop_link())));
        keep(&cx);
        assert_eq!(cx.joins.borrow().len(), 2);
        assert_eq!(cx.driver.pmks.get(), 2);
        let (rejoined, _) = cx.joins.borrow()[1].clone();
        assert!(rejoined > secs(30) + REJOIN_GRACE);
        assert!(rejoined < secs(30) + REJOIN_GRACE + secs(10));
    }

    #[test]
    fn after_a_suspend_goes_straight_back_with_the_pmk() {
        let cx = FakeContext::new(vec![wpa("Home")], secs(60));
        home_in_range(&cx, 1);
        cx.at(secs(20), Event::Kick(Kick::Resumed));
        keep(&cx);
        assert_eq!(cx.driver.pmks.get(), 2);
        // No second scan: the network it was on.
        let scans = cx.driver.actions().iter().filter(|c| *c == "scan").count();
        assert_eq!(scans, 1);
        assert!(cx.fell_back.borrow().is_none());
    }

    #[test]
    fn a_forgotten_network_is_left_for_another() {
        let cx = FakeContext::new(vec![wpa("Home"), wpa("Phone")], secs(60));
        home_in_range(&cx, 1);
        cx.driver
            .aps
            .borrow_mut()
            .push(wpa2(2, "Phone", -70, Cipher::Ccmp));
        cx.driver.joinable.borrow_mut().push((b"Phone".to_vec(), 1));
        cx.at(
            secs(20),
            Event::Do(Box::new(|_, saved| {
                saved.remove(0);
            })),
        );
        cx.at(secs(20), Event::Kick(Kick::Saved));
        keep(&cx);
        let joins: Vec<String> = cx.joins.borrow().iter().map(|(_, s)| s.clone()).collect();
        assert_eq!(joins, ["Home", "Phone"]);
    }

    #[test]
    fn a_kick_while_still_on_a_saved_network_changes_nothing() {
        let cx = FakeContext::new(vec![wpa("Home")], secs(60));
        home_in_range(&cx, 1);
        cx.at(secs(20), Event::Kick(Kick::Saved));
        keep(&cx);
        assert_eq!(cx.joins.borrow().len(), 1);
        assert_eq!(cx.driver.pmks.get(), 1);
    }

    #[test]
    fn falls_back_after_thirty_seconds_in_range_without_joining() {
        let cx = FakeContext::new(vec![wpa("Home")], secs(600));
        cx.driver
            .aps
            .borrow_mut()
            .push(wpa2(1, "Home", -50, Cipher::Ccmp));
        keep(&cx);
        let (when, why) = cx.fell_back.borrow().clone().unwrap();
        assert!(why.contains("Home"), "{why}");
        assert!(when >= FALLBACK_AFTER && when < secs(60), "{when:?}");
        assert!(cx.joins.borrow().is_empty());
    }

    #[test]
    fn an_association_that_never_holds_falls_back_too() {
        let cx = FakeContext::new(vec![wpa("Home")], secs(600));
        home_in_range(&cx, 0);
        // A wrong password: associates, then the handshake fails.
        for t in 0..300 {
            cx.at(
                Duration::from_millis(t * 1000 + 500),
                Event::Do(Box::new(|d, _| d.drop_link())),
            );
        }
        keep(&cx);
        let (when, _) = cx.fell_back.borrow().clone().unwrap();
        assert!(when < secs(120), "{when:?}");
    }

    #[test]
    fn out_of_range_keeps_trying_blind_without_falling_back() {
        let cx = FakeContext::new(vec![wpa("Home")], secs(600));
        keep(&cx);
        assert!(cx.fell_back.borrow().is_none());
        let tries = cx
            .driver
            .actions()
            .iter()
            .filter(|c| *c == "essid Home")
            .count();
        // Backing off to a minute: about a dozen tries in ten minutes.
        assert!((5..=20).contains(&tries), "{tries}");
    }

    #[test]
    fn a_hidden_network_joins_blind() {
        let cx = FakeContext::new(vec![wpa("Hidden")], secs(60));
        cx.driver
            .joinable
            .borrow_mut()
            .push((b"Hidden".to_vec(), 1));
        keep(&cx);
        assert_eq!(cx.joins.borrow().len(), 1);
    }

    #[test]
    fn an_unsupported_saved_network_falls_back_with_the_reason() {
        let cx = FakeContext::new(vec![wpa("Old")], secs(60));
        cx.driver.aps.borrow_mut().push(wpa1(1, "Old", -40));
        keep(&cx);
        let (when, why) = cx.fell_back.borrow().clone().unwrap();
        assert!(why.contains("Old") && why.contains("WPA (TKIP)"), "{why}");
        assert!(when < secs(5));
    }

    #[test]
    fn a_driver_that_keeps_failing_falls_back() {
        let cx = FakeContext::new(vec![wpa("Home")], secs(600));
        home_in_range(&cx, 1);
        *cx.driver.failing.borrow_mut() = Some(("auth", libc::EIO));
        keep(&cx);
        let (_, why) = cx.fell_back.borrow().clone().unwrap();
        assert!(why.contains("SIOCSIWAUTH"), "{why}");
        let auths = cx
            .driver
            .actions()
            .iter()
            .filter(|c| c.starts_with("auth"))
            .count();
        assert_eq!(auths as u32, STRIKES);
    }

    #[test]
    fn one_driver_error_is_forgiven() {
        let cx = FakeContext::new(vec![wpa("Home")], secs(60));
        home_in_range(&cx, 1);
        *cx.driver.failing.borrow_mut() = Some(("scan", libc::EIO));
        cx.at(
            secs(1),
            Event::Do(Box::new(|d, _| *d.failing.borrow_mut() = None)),
        );
        keep(&cx);
        assert!(cx.fell_back.borrow().is_none());
        assert_eq!(cx.joins.borrow().len(), 1);
    }

    #[test]
    fn no_saved_networks_idles_until_one_is_added() {
        let cx = FakeContext::new(vec![], secs(200));
        home_in_range(&cx, 1);
        cx.at(
            secs(100),
            Event::Do(Box::new(|_, saved| saved.push(wpa("Home")))),
        );
        cx.at(secs(100), Event::Kick(Kick::Saved));
        keep(&cx);
        let joins = cx.joins.borrow();
        assert_eq!(joins.len(), 1);
        assert!(joins[0].0 >= secs(100) && joins[0].0 < secs(110));
        assert!(cx.fell_back.borrow().is_none());
    }

    #[test]
    fn already_associated_at_start_is_kept() {
        let cx = FakeContext::new(vec![wpa("Home")], secs(30));
        home_in_range(&cx, 0);
        cx.driver.set_essid(Some(b"Home")).unwrap();
        cx.driver.bssid().unwrap();
        cx.driver.calls.borrow_mut().clear();
        keep(&cx);
        assert_eq!(cx.joins.borrow().len(), 1);
        assert_eq!(cx.driver.pmks.get(), 0);
    }
}
