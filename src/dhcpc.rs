//! kindil's own DHCP client, a thread of kindil's in place of udhcpc:
//! behind a switch, off unless [`SWITCH`] says `kindil`.
//!
//! [`machine`] is RFC 2131's client without I/O; [`wire`] reads replies
//! off a packet socket; [`apply`] puts a lease on the interface; the Linux
//! sockets and ioctls are in `sys`. This module runs them: [`Client`]
//! starts the thread, takes requests for a lease (one per join, each
//! numbered), and records each lease once it is on the interface, under the
//! number of the request it answers. kindil's Wi-Fi (src/station.rs) counts
//! a join only by a lease for its own request, with that address on the
//! interface, as it did with udhcpc's markers.
//!
//! Lease times run on [`LeaseClock`]: CLOCK_MONOTONIC stops while the
//! Kindle sleeps (2.6.31 has no CLOCK_BOOTTIME), so the time asleep is
//! added from the RTC. A request for the network it holds a lease on keeps
//! the lease until T1 (no packet: the doze's fetches every 30 minutes cost
//! no DHCP), confirms it by INIT-REBOOT after T1, and asks afresh on
//! another network. Switching Wi-Fi off gives the lease back (RELEASE).
pub mod apply;
pub mod machine;
#[cfg(target_os = "linux")]
mod sys;
pub mod wire;

use crate::dhcp::{Leased, Marker};
#[cfg(target_os = "linux")]
use machine::Machine;
use machine::{Event, How, Lease, Network};
use std::collections::VecDeque;
use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// `kindil` in it: kindil's own DHCP client. Absent or anything else: udhcpc.
pub const SWITCH: &str = "/var/local/kindil/dhcp";
/// Where the resolver reads the name servers (`/etc/resolv.conf` links here).
pub const RESOLV: &str = "/var/run/resolv.conf";
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
/// The thread looks at the clock at least this often, to notice a sleep
/// that passed a deadline: poll's timeout doesn't run while asleep.
const LOOK_EVERY: Duration = Duration::from_secs(60);

/// Whether the switch's content picks kindil's own DHCP client.
pub fn chosen(switch: Option<&str>) -> bool {
    switch.is_some_and(|text| text.trim() == "kindil")
}

/// Transactions begun and messages sent by any client of this run, for
/// the doze ledger.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub exchanges: u64,
    pub packets: u64,
}

static EXCHANGES: AtomicU64 = AtomicU64::new(0);
static PACKETS: AtomicU64 = AtomicU64::new(0);
/// Request numbers, unique across clients of a run.
static ASKS: AtomicU32 = AtomicU32::new(0);

pub fn counts() -> Counts {
    Counts {
        exchanges: EXCHANGES.load(Ordering::Relaxed),
        packets: PACKETS.load(Ordering::Relaxed),
    }
}

/// A clock that keeps running while the Kindle sleeps: CLOCK_MONOTONIC,
/// plus the time asleep as the RTC tells it (whole seconds). It never goes
/// back, and the RTC only ever adds to it.
#[derive(Debug)]
pub struct LeaseClock {
    first: Option<(Duration, i64)>,
    asleep: Duration,
}

impl LeaseClock {
    /// A sleep shorter than this isn't told apart from the RTC's rounding.
    const SLACK: Duration = Duration::from_secs(2);

    pub fn new() -> Self {
        Self {
            first: None,
            asleep: Duration::ZERO,
        }
    }

    /// Now, from the monotonic clock and the RTC (if it could be read).
    pub fn now(&mut self, monotonic: Duration, rtc: Option<i64>) -> Duration {
        if let Some(rtc) = rtc {
            let (mono0, rtc0) = *self.first.get_or_insert((monotonic, rtc));
            let wall = Duration::from_secs(rtc.saturating_sub(rtc0).max(0) as u64);
            let awake = monotonic.saturating_sub(mono0);
            if wall > awake + self.asleep + Self::SLACK {
                self.asleep = wall - awake;
            }
        }
        monotonic + self.asleep
    }
}

impl Default for LeaseClock {
    fn default() -> Self {
        Self::new()
    }
}

/// What the thread is asked to do.
#[derive(Debug)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
enum Command {
    Request(Option<Network>, u32),
    /// Give the lease back, and stop if told to.
    Release {
        stop: bool,
    },
    Stop,
}

/// What the thread shares with its [`Client`].
#[derive(Default)]
struct Shared {
    commands: Mutex<VecDeque<Command>>,
    /// Commands done, for [`Client::give_back`] to wait on.
    done: AtomicU64,
    /// The last lease put on the interface, and the request it answered.
    leased: Mutex<Option<(u32, Marker)>>,
    /// The lease held, for status.
    lease: Mutex<Option<Lease>>,
}

/// The running client thread.
pub struct Client {
    shared: Arc<Shared>,
    /// Commands given so far.
    given: AtomicU64,
    wake: std::os::unix::net::UnixDatagram,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Client {
    /// Starts the client on `interface`, writing the name servers to
    /// `resolv`; it waits for a [`Client::request`]. An address already on
    /// the interface (an earlier client's, or udhcpc's) is the one it asks
    /// for first.
    #[cfg(target_os = "linux")]
    pub fn start(interface: &str, resolv: &Path) -> std::io::Result<Self> {
        let sockets = sys::Sockets::open(interface)?;
        let ioctls = sys::Ioctls::open(interface)?;
        let mac = ioctls.mac()?;
        let (wake, woken) = std::os::unix::net::UnixDatagram::pair()?;
        woken.set_nonblocking(true)?;
        wake.set_nonblocking(true)?;
        let shared = Arc::new(Shared::default());
        let machine = Machine::new(mac, Box::new(rand::random::<u32>)).with_hint(ioctls.address());
        let resolv = resolv.to_owned();
        let thread = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("dhcp".into())
                .spawn(move || run(machine, sockets, ioctls, &resolv, &shared, &woken))?
        };
        Ok(Self {
            shared,
            given: AtomicU64::new(0),
            wake,
            thread: Some(thread),
        })
    }

    #[cfg(not(target_os = "linux"))]
    pub fn start(_interface: &str, _resolv: &Path) -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "kindil's DHCP client runs on Linux",
        ))
    }

    /// A lease for `network` (`None`: not known); the request's number,
    /// which [`Client::marker`] answers for.
    pub fn request(&self, network: Option<Network>) -> u32 {
        let ask = ASKS.fetch_add(1, Ordering::SeqCst) + 1;
        self.command(Command::Request(network, ask));
        ask
    }

    /// The lease that answered request `ask`, once it is on the interface.
    pub fn marker(&self, ask: u32) -> Option<Marker> {
        lock(&self.shared.leased)
            .filter(|(answered, _)| *answered == ask)
            .map(|(_, marker)| marker)
    }

    pub fn lease(&self) -> Option<Lease> {
        lock(&self.shared.lease).clone()
    }

    /// Gives the lease back (while still on its network) and stops.
    pub fn release(mut self) {
        self.command(Command::Release { stop: true });
        self.join();
    }

    /// Gives the lease back (while still on its network) and waits, a
    /// second at most, for the RELEASE to be sent; the client stays, for
    /// the next request.
    pub fn give_back(&self) {
        let given = self.command(Command::Release { stop: false });
        let since = std::time::Instant::now();
        while self.shared.done.load(Ordering::SeqCst) < given
            && since.elapsed() < Duration::from_secs(1)
        {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Stops, leaving the address as it is (as udhcpc did when killed).
    pub fn stop(mut self) {
        self.command(Command::Stop);
        self.join();
    }

    /// Hands the thread `command`; how many have been given, with it.
    fn command(&self, command: Command) -> u64 {
        lock(&self.shared.commands).push_back(command);
        let _ = self.wake.send(&[1]);
        self.given.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn join(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if self.thread.is_some() {
            self.command(Command::Stop);
            self.join();
        }
    }
}

/// The thread: commands, deadlines and replies into the machine, its
/// actions out to the sockets and the interface.
#[cfg(target_os = "linux")]
fn run(
    mut machine: Machine,
    mut sockets: sys::Sockets,
    mut ioctls: sys::Ioctls,
    resolv: &Path,
    shared: &Shared,
    woken: &std::os::unix::net::UnixDatagram,
) {
    let mut clock = LeaseClock::new();
    let mut now = || clock.now(crate::dhcp::monotonic(), crate::clock::read_rtc().ok());
    let mut applied: Option<Lease> = None;
    let mut buf = vec![0u8; 2048];
    let mut stopping = false;
    loop {
        let mut drained = [0u8; 16];
        while woken.recv(&mut drained).is_ok() {}
        let commands: Vec<Command> = lock(&shared.commands).drain(..).collect();
        let took = commands.len() as u64;
        for command in commands {
            match command {
                Command::Request(network, ask) => machine.request(now(), network, ask),
                Command::Release { stop } => {
                    machine.release(now());
                    stopping |= stop;
                }
                Command::Stop => stopping = true,
            }
        }
        machine.timeout(now());
        // Listening before sending: an answer can come at once.
        if let Err(error) = sockets.listen_raw(!stopping && !machine.configured()) {
            log(&format!("the packet socket: {error}"));
        }
        perform(
            &mut machine,
            &sockets,
            &mut ioctls,
            resolv,
            shared,
            &mut applied,
        );
        shared.done.fetch_add(took, Ordering::SeqCst);
        if stopping {
            // A RELEASE went out in `perform`: sent, not answered.
            return;
        }
        let wait = machine
            .deadline()
            .map_or(LOOK_EVERY, |at| at.saturating_sub(now()))
            .min(LOOK_EVERY);
        let ready = match sockets.wait(std::os::fd::AsRawFd::as_raw_fd(woken), wait) {
            Ok(ready) => ready,
            Err(error) => {
                log(&format!("waiting: {error}"));
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
        };
        if ready.udp {
            while let Some(n) = sockets.recv_udp(&mut buf) {
                machine.packet(now(), &buf[..n]);
            }
        }
        if ready.raw {
            while let Some((n, summed)) = sockets.recv_raw(&mut buf) {
                if let Some(message) = wire::dhcp_payload(&buf[..n], summed) {
                    let message = message.to_vec();
                    machine.packet(now(), &message);
                }
            }
        }
    }
}

/// Does the machine's actions.
#[cfg(target_os = "linux")]
fn perform(
    machine: &mut Machine,
    sockets: &sys::Sockets,
    ioctls: &mut sys::Ioctls,
    resolv: &Path,
    shared: &Shared,
    applied: &mut Option<Lease>,
) {
    use machine::{Action, Dest};
    while let Some(action) = machine.poll_action() {
        match action {
            Action::Send {
                to,
                kind,
                first,
                packet,
            } => {
                if first {
                    EXCHANGES.fetch_add(1, Ordering::Relaxed);
                }
                PACKETS.fetch_add(1, Ordering::Relaxed);
                let to = match to {
                    Dest::Broadcast => Ipv4Addr::BROADCAST,
                    Dest::Server(server) => server,
                };
                if let Err(error) = sockets.send(to, &packet) {
                    log(&format!("sending {kind:?} to {to}: {error}"));
                }
            }
            Action::Configure(lease) => {
                for error in apply::configure(ioctls, applied.as_ref(), &lease) {
                    log(&format!("setting the lease of {}: {error}", lease.ip));
                }
                if let Err(error) = apply::write_resolv(resolv, &apply::resolv(&lease)) {
                    log(&format!("writing {}: {error}", resolv.display()));
                }
                *lock(&shared.lease) = Some(lease.clone());
                *applied = Some(lease);
            }
            Action::Deconfigure => {
                if let Err(error) = apply::deconfigure(ioctls) {
                    log(&format!("taking the address off: {error}"));
                }
                *lock(&shared.lease) = None;
                *applied = None;
            }
            Action::Event(event) => record(&event, ioctls.address(), shared),
        }
    }
}

/// Logs an event; a lease whose address is on the interface is recorded
/// for the request it answers.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn record(event: &Event, on_interface: Option<Ipv4Addr>, shared: &Shared) {
    match event {
        Event::Leased { ask, how, ip } => {
            let what = match how {
                How::Fresh => "leased",
                How::Rebooted => "confirmed",
                How::Renewed => "renewed",
                How::Rebound => "rebound",
                How::Kept => "kept",
            };
            if *how != How::Kept {
                let held = lock(&shared.lease).as_ref().map(|lease| {
                    format!(
                        " from {} for {} s (renewing after {} s)",
                        lease.server,
                        lease.duration.as_secs(),
                        lease.t1.as_secs()
                    )
                });
                log(&format!("{what} {ip}{}", held.unwrap_or_default()));
            }
            if on_interface != Some(*ip) {
                log(&format!("{ip} isn't on the interface"));
                return;
            }
            let leased = match how {
                How::Renewed | How::Rebound => Leased::Renew,
                How::Fresh | How::Rebooted | How::Kept => Leased::Bound,
            };
            let marker = Marker {
                leased,
                ip: *ip,
                at: crate::dhcp::monotonic(),
            };
            *lock(&shared.leased) = Some((*ask, marker));
        }
        Event::Lost { why } => log(&format!("lease lost: {why}")),
        Event::Released => log("lease given back"),
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn log(line: &str) {
    // Never panics, unlike eprintln!: a full /var/local.
    let line = format!("kindil: DHCP: {line}\n");
    let _ = std::io::Write::write_all(&mut std::io::stderr(), line.as_bytes());
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn the_switch_picks_the_client() {
        assert!(!chosen(None));
        assert!(chosen(Some("kindil\n")));
        assert!(!chosen(Some("udhcpc")));
    }

    #[test]
    fn the_lease_clock_counts_time_asleep() {
        let mut clock = LeaseClock::new();
        assert_eq!(clock.now(secs(100), Some(1_000)), secs(100));
        // Awake: the RTC's rounding changes nothing.
        assert_eq!(clock.now(secs(150), Some(1_051)), secs(150));
        // Asleep 30 min: the monotonic clock moved 1 s, the RTC 1801 s.
        assert_eq!(clock.now(secs(151), Some(2_851)), secs(1_951));
        // And on from there.
        assert_eq!(clock.now(secs(161), Some(2_861)), secs(1_961));
        // Without the RTC, or with it gone back: never backwards.
        assert_eq!(clock.now(secs(162), None), secs(1_962));
        assert_eq!(clock.now(secs(163), Some(500)), secs(1_963));
    }

    #[test]
    fn a_lease_is_recorded_for_its_request_only_once_on_the_interface() {
        let shared = Shared::default();
        let ip = Ipv4Addr::new(192, 0, 2, 30);
        let leased = |how| Event::Leased { ask: 4, how, ip };
        record(&leased(How::Fresh), None, &shared);
        assert_eq!(*lock(&shared.leased), None);
        record(
            &leased(How::Fresh),
            Some(Ipv4Addr::new(192, 0, 2, 31)),
            &shared,
        );
        assert_eq!(*lock(&shared.leased), None);
        record(&leased(How::Kept), Some(ip), &shared);
        let (ask, marker) = lock(&shared.leased).unwrap();
        assert_eq!((ask, marker.leased, marker.ip), (4, Leased::Bound, ip));
        record(&leased(How::Renewed), Some(ip), &shared);
        assert_eq!(lock(&shared.leased).unwrap().1.leased, Leased::Renew);
    }

    /// The default gateway on `interface`, from /proc/net/route.
    #[cfg(target_os = "linux")]
    fn default_gateway(interface: &str) -> Option<Ipv4Addr> {
        let routes = std::fs::read_to_string("/proc/net/route").ok()?;
        routes.lines().skip(1).find_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            (fields.first() == Some(&interface) && fields.get(1) == Some(&"00000000"))
                .then(|| u32::from_str_radix(fields[2], 16).ok())
                .flatten()
                .map(|gw| Ipv4Addr::from(gw.to_le_bytes()))
        })
    }

    /// Against a real server: dnsmasq, in throwaway network namespaces
    /// (tests/dhcp-netns.sh sets them up and runs this). About 70 s: it
    /// waits for T1 of dnsmasq's shortest lease (2 minutes).
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore]
    fn against_a_real_server() {
        let interface = std::env::var("KINDIL_DHCP_TEST_INTERFACE")
            .expect("run by tests/dhcp-netns.sh: KINDIL_DHCP_TEST_INTERFACE");
        let leases = std::env::var("KINDIL_DHCP_TEST_LEASES").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let resolv = dir.path().join("resolv.conf");
        let client = Client::start(&interface, &resolv).unwrap();
        let ioctls = sys::Ioctls::open(&interface).unwrap();
        let wait = |ask: u32, kind: Leased, limit: Duration| {
            let since = std::time::Instant::now();
            loop {
                if let Some(marker) = client.marker(ask).filter(|m| m.leased == kind) {
                    return marker;
                }
                assert!(since.elapsed() < limit, "no {kind:?} lease for {ask}");
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        let home = Network {
            ssid: "Home".into(),
            bssid: [2, 0, 0, 0, 0, 1],
        };

        // DISCOVER, OFFER, REQUEST, ACK; the lease on the interface.
        let ask = client.request(Some(home.clone()));
        let marker = wait(ask, Leased::Bound, Duration::from_secs(15));
        let server = Ipv4Addr::new(192, 0, 2, 1);
        assert_eq!(ioctls.address(), Some(marker.ip));
        assert_eq!(default_gateway(&interface), Some(server));
        assert_eq!(
            std::fs::read_to_string(&resolv).unwrap(),
            "domain example.test\nnameserver 192.0.2.1\n"
        );
        let lease = client.lease().unwrap();
        assert_eq!(lease.server, server);
        assert_eq!(lease.duration, Duration::from_secs(120));
        assert!(std::fs::read_to_string(&leases)
            .unwrap()
            .contains(&marker.ip.to_string()));

        // The same network again: kept, nothing sent.
        let before = counts();
        let ask = client.request(Some(home.clone()));
        wait(ask, Leased::Bound, Duration::from_secs(1));
        assert_eq!(counts(), before);

        // T1: renewed by a REQUEST unicast to the server, the reply coming
        // through the kernel to the address.
        wait(ask, Leased::Renew, Duration::from_secs(75));
        assert_eq!(counts().exchanges, before.exchanges + 1);
        assert_eq!(ioctls.address(), Some(marker.ip));

        // Another access point: afresh, asking for the same address.
        let ask = client.request(Some(Network {
            bssid: [2, 0, 0, 0, 0, 2],
            ..home
        }));
        let again = wait(ask, Leased::Bound, Duration::from_secs(15));
        assert_eq!(again.ip, marker.ip);
        assert_eq!(default_gateway(&interface), Some(server));

        // RELEASE: the address goes, and the server frees the lease.
        client.release();
        assert_eq!(ioctls.address(), None);
        let since = std::time::Instant::now();
        while std::fs::read_to_string(&leases)
            .unwrap()
            .contains(&marker.ip.to_string())
        {
            assert!(since.elapsed() < Duration::from_secs(5), "lease not freed");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
