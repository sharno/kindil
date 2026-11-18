//! RFC 2131's client, without I/O: messages in and out as bytes, time as a
//! [`Duration`] the caller reads from its clock, and what to do as
//! [`Action`]s. src/dhcpc.rs runs it over real sockets; the tests over a
//! fake network and clock.
//!
//! INIT → SELECTING (DISCOVER, the first OFFER taken) → REQUESTING (REQUEST
//! naming that server) → BOUND; at T1 RENEWING (REQUEST unicast to the
//! server), at T2 REBINDING (broadcast), at expiry the address goes and it
//! starts over. A request for the network it holds a lease on keeps that
//! lease while T1 hasn't passed (no packet at all), confirms it by
//! INIT-REBOOT after T1, and starts afresh on any other network. A NAK
//! starts over; a RELEASE gives the lease back.
//!
//! Retransmissions double from [`FIRST_RETRANSMIT`] to [`MAX_RETRANSMIT`],
//! each ±1 s at random (RFC 2131 4.1). The first wait is 2 s, not the 4 s
//! RFC 2131 gives as an example: udhcpc ran with `-T 2`, and a join has to
//! show its lease within 15 s (src/station.rs). While RENEWING and
//! REBINDING, a REQUEST is repeated after half the time left until T2 (or
//! expiry), but no sooner than [`RENEW_MIN`] (4.4.5).
use dhcproto::v4::{self, DhcpOption, Message, MessageType, Opcode, OptionCode};
use dhcproto::{Decodable, Decoder, Encodable};
use std::collections::VecDeque;
use std::net::Ipv4Addr;
use std::time::Duration;

pub const FIRST_RETRANSMIT: Duration = Duration::from_secs(2);
pub const MAX_RETRANSMIT: Duration = Duration::from_secs(64);
/// REQUESTs for an offer before starting over.
pub const REQUEST_TRIES: u32 = 4;
/// INIT-REBOOT REQUESTs before starting over: a network that doesn't
/// answer them quickly is better asked afresh.
pub const REBOOT_TRIES: u32 = 2;
pub const RENEW_MIN: Duration = Duration::from_secs(60);
/// Without a lease time in the ACK (it is required; this is for a broken
/// server).
pub const DEFAULT_LEASE: Duration = Duration::from_secs(3600);
/// Messages are padded to BOOTP's minimum; some relays drop shorter ones.
const MIN_MESSAGE: usize = 300;
/// What to ask the server for.
const PARAMETERS: [OptionCode; 8] = [
    OptionCode::SubnetMask,
    OptionCode::Router,
    OptionCode::DomainNameServer,
    OptionCode::DomainName,
    OptionCode::BroadcastAddr,
    OptionCode::AddressLeaseTime,
    OptionCode::Renewal,
    OptionCode::Rebinding,
];

/// A network a lease belongs to: the same SSID on another access point
/// counts as another network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Network {
    pub ssid: String,
    pub bssid: [u8; 6],
}

/// A lease, as the ACK gave it; times on the client's clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lease {
    pub ip: Ipv4Addr,
    pub server: Ipv4Addr,
    pub netmask: Option<Ipv4Addr>,
    pub broadcast: Option<Ipv4Addr>,
    pub routers: Vec<Ipv4Addr>,
    pub dns: Vec<Ipv4Addr>,
    pub domain: Option<String>,
    pub duration: Duration,
    pub t1: Duration,
    pub t2: Duration,
    /// When the REQUEST that got it was first sent (RFC 2131 4.4.1).
    pub start: Duration,
}

impl Lease {
    pub fn renew_at(&self) -> Duration {
        self.start.saturating_add(self.t1)
    }

    pub fn rebind_at(&self) -> Duration {
        self.start.saturating_add(self.t2)
    }

    pub fn expires_at(&self) -> Duration {
        self.start.saturating_add(self.duration)
    }
}

/// Where a message goes: broadcast, or to the server's address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dest {
    Broadcast,
    Server(Ipv4Addr),
}

/// How a lease came about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum How {
    /// DISCOVER, OFFER, REQUEST, ACK.
    Fresh,
    /// INIT-REBOOT: the previous address confirmed.
    Rebooted,
    /// At T1, from the server that gave it.
    Renewed,
    /// At T2, from any server.
    Rebound,
    /// Asked for on the network it is for, before T1: kept as it is, no
    /// exchange.
    Kept,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A lease for request `ask`, on the interface once the actions before
    /// this one are done.
    Leased { ask: u32, how: How, ip: Ipv4Addr },
    /// The lease is gone (the address with it): expired, or refused.
    Lost { why: &'static str },
    /// Given back.
    Released,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// `first`: the first message of a transaction (for counting them).
    Send {
        to: Dest,
        kind: MessageType,
        first: bool,
        packet: Vec<u8>,
    },
    /// Puts the lease on the interface: address, route, name servers.
    Configure(Lease),
    /// Takes the address off the interface.
    Deconfigure,
    Event(Event),
}

/// One transaction: its id, when it began, and its retransmissions.
#[derive(Clone, Debug)]
struct Exchange {
    xid: u32,
    started: Duration,
    sent: u32,
    interval: Duration,
    next: Duration,
}

#[derive(Clone, Debug)]
enum State {
    /// Nothing asked for, or given back.
    Idle,
    Selecting {
        exchange: Exchange,
        /// An address to ask for again (the last one held).
        hint: Option<Ipv4Addr>,
    },
    Requesting {
        exchange: Exchange,
        ip: Ipv4Addr,
        server: Ipv4Addr,
    },
    Rebooting {
        exchange: Exchange,
        ip: Ipv4Addr,
    },
    Bound,
    Renewing(Exchange),
    Rebinding(Exchange),
}

/// The state's name, for logs and tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Selecting,
    Requesting,
    Rebooting,
    Bound,
    Renewing,
    Rebinding,
}

pub struct Machine {
    chaddr: [u8; 6],
    random: Box<dyn FnMut() -> u32 + Send>,
    state: State,
    /// The lease held, and the network it is for.
    lease: Option<(Lease, Option<Network>)>,
    /// The address is on the interface (as far as the client knows).
    configured: bool,
    /// The network asked for last, and that request's number.
    wanted: Option<Network>,
    ask: u32,
    /// An address to ask for when there is no lease to go by: the one the
    /// interface had when the client started.
    hint: Option<Ipv4Addr>,
    out: VecDeque<Action>,
}

impl Machine {
    /// `random`: transaction ids and jitter.
    pub fn new(chaddr: [u8; 6], random: Box<dyn FnMut() -> u32 + Send>) -> Self {
        Self {
            chaddr,
            random,
            state: State::Idle,
            lease: None,
            configured: false,
            wanted: None,
            ask: 0,
            hint: None,
            out: VecDeque::new(),
        }
    }

    /// An address to ask for in a DISCOVER without a lease to go by.
    pub fn with_hint(mut self, hint: Option<Ipv4Addr>) -> Self {
        self.hint = hint.filter(|ip| !ip.is_unspecified());
        self
    }

    pub fn phase(&self) -> Phase {
        match self.state {
            State::Idle => Phase::Idle,
            State::Selecting { .. } => Phase::Selecting,
            State::Requesting { .. } => Phase::Requesting,
            State::Rebooting { .. } => Phase::Rebooting,
            State::Bound => Phase::Bound,
            State::Renewing(_) => Phase::Renewing,
            State::Rebinding(_) => Phase::Rebinding,
        }
    }

    pub fn lease(&self) -> Option<&Lease> {
        self.lease.as_ref().map(|(lease, _)| lease)
    }

    /// Whether the lease's address is on the interface: replies then come
    /// to it through the kernel; before, only a packet socket sees them.
    pub fn configured(&self) -> bool {
        self.configured
    }

    /// The next thing to do.
    pub fn poll_action(&mut self) -> Option<Action> {
        self.out.pop_front()
    }

    /// When [`Machine::timeout`] has something to do.
    pub fn deadline(&self) -> Option<Duration> {
        let lease = self.lease();
        match &self.state {
            State::Idle => None,
            State::Selecting { exchange, .. }
            | State::Requesting { exchange, .. }
            | State::Rebooting { exchange, .. } => Some(exchange.next),
            State::Bound => lease.map(Lease::renew_at),
            State::Renewing(exchange) => lease.map(|l| exchange.next.min(l.rebind_at())),
            State::Rebinding(exchange) => lease.map(|l| exchange.next.min(l.expires_at())),
        }
    }

    /// A lease for `network` (`None`: not known), as request `ask`: kept
    /// if it is held for that network and T1 hasn't passed, confirmed by
    /// INIT-REBOOT if it hasn't expired (or if it was taken on a network
    /// not known, which this may be), else asked for afresh.
    pub fn request(&mut self, now: Duration, network: Option<Network>, ask: u32) {
        self.ask = ask;
        self.wanted = network.clone();
        let (held, same) = match &self.lease {
            Some((lease, on)) if network.is_some() && *on == network => (Some(lease.clone()), true),
            Some((lease, None)) if network.is_some() => (Some(lease.clone()), false),
            _ => (None, false),
        };
        match held {
            Some(lease) if same && self.configured && now < lease.renew_at() => {
                self.state = State::Bound;
                self.emit(Event::Leased {
                    ask,
                    how: How::Kept,
                    ip: lease.ip,
                });
            }
            Some(lease) if now < lease.expires_at() => {
                let exchange = self.exchange(now);
                self.state = State::Rebooting {
                    exchange,
                    ip: lease.ip,
                };
                self.send_current(now);
            }
            _ => {
                let hint = self.lease().map(|lease| lease.ip);
                self.drop_lease();
                self.select(now, hint);
            }
        }
    }

    /// Gives the lease back (while still on its network): a RELEASE to its
    /// server, the address off the interface. Idle after.
    pub fn release(&mut self, now: Duration) {
        let bound = matches!(
            self.state,
            State::Bound | State::Renewing(_) | State::Rebinding(_)
        );
        if let (true, Some(lease)) = (bound, self.lease().cloned()) {
            let xid = (self.random)();
            let packet = self.message(
                MessageType::Release,
                xid,
                now,
                now,
                lease.ip,
                false,
                &[DhcpOption::ServerIdentifier(lease.server)],
            );
            self.out.push_back(Action::Send {
                to: Dest::Server(lease.server),
                kind: MessageType::Release,
                first: true,
                packet,
            });
            self.drop_lease();
            self.emit(Event::Released);
        }
        self.state = State::Idle;
    }

    /// Whatever is due by `now`: retransmissions, T1, T2, expiry.
    pub fn timeout(&mut self, now: Duration) {
        // A long sleep can pass several deadlines at once.
        for _ in 0..8 {
            match self.deadline() {
                Some(deadline) if deadline <= now => self.due(now),
                _ => return,
            }
        }
    }

    fn due(&mut self, now: Duration) {
        let Some(lease) = self.lease().cloned() else {
            return self.due_unbound(now);
        };
        let phase = self.phase();
        if !matches!(phase, Phase::Bound | Phase::Renewing | Phase::Rebinding) {
            return self.due_unbound(now);
        }
        // Straight to where `now` is: a long sleep may have passed T1, T2
        // and expiry all at once.
        if now >= lease.expires_at() {
            self.lose("expired");
            self.select(now, None);
        } else if now >= lease.rebind_at() {
            if phase != Phase::Rebinding {
                let exchange = self.exchange(now);
                self.state = State::Rebinding(exchange);
            }
            self.send_current(now);
            self.renew_wait(now, lease.expires_at());
        } else if now >= lease.renew_at() {
            if phase == Phase::Bound {
                let exchange = self.exchange(now);
                self.state = State::Renewing(exchange);
            }
            self.send_current(now);
            self.renew_wait(now, lease.rebind_at());
        }
    }

    fn due_unbound(&mut self, now: Duration) {
        match self.state.clone() {
            State::Selecting { .. } => self.retransmit(now),
            State::Requesting { exchange, .. } if exchange.sent >= REQUEST_TRIES => {
                self.select(now, None)
            }
            State::Rebooting { exchange, ip } if exchange.sent >= REBOOT_TRIES => {
                self.drop_lease();
                self.select(now, Some(ip));
            }
            State::Requesting { .. } | State::Rebooting { .. } => self.retransmit(now),
            _ => {}
        }
    }

    /// A datagram from port 67: an answer to the transaction under way, or
    /// ignored.
    pub fn packet(&mut self, now: Duration, bytes: &[u8]) {
        let Ok(message) = Message::decode(&mut Decoder::new(bytes)) else {
            return;
        };
        let Some(kind) = message.opts().msg_type() else {
            return;
        };
        let xid = match &self.state {
            State::Selecting { exchange, .. }
            | State::Requesting { exchange, .. }
            | State::Rebooting { exchange, .. }
            | State::Renewing(exchange)
            | State::Rebinding(exchange) => exchange.xid,
            State::Idle | State::Bound => return,
        };
        if message.opcode() != Opcode::BootReply
            || message.xid() != xid
            || message.chaddr().get(..6) != Some(&self.chaddr[..])
        {
            return;
        }
        let server = match message.opts().get(OptionCode::ServerIdentifier) {
            Some(DhcpOption::ServerIdentifier(server)) => Some(*server),
            _ => None,
        };
        match (self.state.clone(), kind) {
            (State::Selecting { exchange, .. }, MessageType::Offer) => {
                let ip = message.yiaddr();
                let Some(server) = server.filter(|_| !ip.is_unspecified()) else {
                    return;
                };
                // The first offer is taken; the REQUEST keeps its xid.
                self.state = State::Requesting {
                    exchange: Exchange {
                        started: now,
                        sent: 0,
                        interval: FIRST_RETRANSMIT,
                        next: now,
                        ..exchange
                    },
                    ip,
                    server,
                };
                self.send_current(now);
            }
            (
                State::Requesting {
                    exchange,
                    server: chosen,
                    ..
                },
                MessageType::Ack,
            ) if server.is_none_or(|s| s == chosen) => {
                self.bind(&message, chosen, exchange.started, How::Fresh);
            }
            (State::Rebooting { exchange, .. }, MessageType::Ack) => {
                let Some(server) = server else { return };
                self.bind(&message, server, exchange.started, How::Rebooted);
            }
            (State::Renewing(exchange), MessageType::Ack) => {
                let held = self.lease().map(|l| l.server);
                let Some(server) = server.or(held) else {
                    return;
                };
                self.bind(&message, server, exchange.started, How::Renewed);
            }
            (State::Rebinding(exchange), MessageType::Ack) => {
                let Some(server) = server else { return };
                self.bind(&message, server, exchange.started, How::Rebound);
            }
            (State::Requesting { server: chosen, .. }, MessageType::Nak)
                if server.is_none_or(|s| s == chosen) =>
            {
                self.nak(now);
            }
            (
                State::Rebooting { .. } | State::Renewing(_) | State::Rebinding(_),
                MessageType::Nak,
            ) => self.nak(now),
            _ => {}
        }
    }

    fn nak(&mut self, now: Duration) {
        self.hint = None;
        if self.lease.is_some() || self.configured {
            self.lose("refused (NAK)");
        }
        self.select(now, None);
    }

    fn bind(&mut self, ack: &Message, server: Ipv4Addr, start: Duration, how: How) {
        let ip = ack.yiaddr();
        if ip.is_unspecified() {
            return;
        }
        let opts = ack.opts();
        let addr = |code| match opts.get(code) {
            Some(DhcpOption::SubnetMask(a) | DhcpOption::BroadcastAddr(a)) => Some(*a),
            _ => None,
        };
        let list = |code| match opts.get(code) {
            Some(DhcpOption::Router(list) | DhcpOption::DomainNameServer(list)) => list.clone(),
            _ => vec![],
        };
        let secs = |code| match opts.get(code) {
            Some(
                DhcpOption::AddressLeaseTime(s) | DhcpOption::Renewal(s) | DhcpOption::Rebinding(s),
            ) => Some(Duration::from_secs(u64::from(*s))),
            _ => None,
        };
        let duration = secs(OptionCode::AddressLeaseTime).unwrap_or(DEFAULT_LEASE);
        // T1 ≤ T2 ≤ the lease, whatever the server says.
        let t2 = secs(OptionCode::Rebinding)
            .filter(|t2| *t2 <= duration)
            .unwrap_or(duration * 7 / 8);
        let t1 = secs(OptionCode::Renewal)
            .filter(|t1| *t1 <= t2)
            .unwrap_or((duration / 2).min(t2));
        let domain = match opts.get(OptionCode::DomainName) {
            Some(DhcpOption::DomainName(name)) => Some(name.clone()).filter(|n| !n.is_empty()),
            _ => None,
        };
        let lease = Lease {
            ip,
            server,
            netmask: addr(OptionCode::SubnetMask),
            broadcast: addr(OptionCode::BroadcastAddr),
            routers: list(OptionCode::Router),
            dns: list(OptionCode::DomainNameServer),
            domain,
            duration,
            t1,
            t2,
            start,
        };
        self.out.push_back(Action::Configure(lease.clone()));
        self.hint = None;
        self.configured = true;
        self.lease = Some((lease, self.wanted.clone()));
        self.state = State::Bound;
        self.emit(Event::Leased {
            ask: self.ask,
            how,
            ip,
        });
    }

    /// INIT: the address off, a new DISCOVER.
    fn select(&mut self, now: Duration, hint: Option<Ipv4Addr>) {
        if self.configured {
            self.out.push_back(Action::Deconfigure);
            self.configured = false;
        }
        let exchange = self.exchange(now);
        let hint = hint.or(self.hint);
        self.state = State::Selecting { exchange, hint };
        self.send_current(now);
    }

    fn drop_lease(&mut self) {
        self.lease = None;
        if self.configured {
            self.out.push_back(Action::Deconfigure);
            self.configured = false;
        }
    }

    fn lose(&mut self, why: &'static str) {
        self.drop_lease();
        self.emit(Event::Lost { why });
    }

    fn emit(&mut self, event: Event) {
        self.out.push_back(Action::Event(event));
    }

    fn exchange(&mut self, now: Duration) -> Exchange {
        Exchange {
            xid: (self.random)(),
            started: now,
            sent: 0,
            interval: FIRST_RETRANSMIT,
            next: now,
        }
    }

    /// Sends again, and schedules the next one.
    fn retransmit(&mut self, now: Duration) {
        self.send_current(now);
        self.schedule(now);
    }

    /// The next retransmission: after the doubling interval, ±1 s.
    fn schedule(&mut self, now: Duration) {
        let jitter = i64::from((self.random)() % 2001) - 1000;
        if let Some(exchange) = self.exchange_mut() {
            let wait = exchange.interval.as_millis() as i64 + jitter;
            exchange.next = now + Duration::from_millis(wait.max(500) as u64);
            exchange.interval = (exchange.interval * 2).min(MAX_RETRANSMIT);
        }
    }

    /// RENEWING and REBINDING: again after half the time left until
    /// `until`, at least [`RENEW_MIN`].
    fn renew_wait(&mut self, now: Duration, until: Duration) {
        if let Some(exchange) = self.exchange_mut() {
            exchange.next = now + (until.saturating_sub(now) / 2).max(RENEW_MIN);
        }
    }

    fn exchange_mut(&mut self) -> Option<&mut Exchange> {
        match &mut self.state {
            State::Selecting { exchange, .. }
            | State::Requesting { exchange, .. }
            | State::Rebooting { exchange, .. }
            | State::Renewing(exchange)
            | State::Rebinding(exchange) => Some(exchange),
            State::Idle | State::Bound => None,
        }
    }

    /// Sends the current state's message; for SELECTING, REQUESTING and
    /// INIT-REBOOT, schedules its retransmission too.
    fn send_current(&mut self, now: Duration) {
        let lease_ip = self.lease().map(|l| l.ip);
        let held_server = self.lease().map(|l| l.server);
        let (kind, to, ciaddr, broadcast, options, exchange) = match &self.state {
            State::Selecting { exchange, hint } => (
                MessageType::Discover,
                Dest::Broadcast,
                Ipv4Addr::UNSPECIFIED,
                true,
                hint.map(DhcpOption::RequestedIpAddress)
                    .into_iter()
                    .collect::<Vec<_>>(),
                exchange.clone(),
            ),
            State::Requesting {
                exchange,
                ip,
                server,
            } => (
                MessageType::Request,
                Dest::Broadcast,
                Ipv4Addr::UNSPECIFIED,
                true,
                vec![
                    DhcpOption::RequestedIpAddress(*ip),
                    DhcpOption::ServerIdentifier(*server),
                ],
                exchange.clone(),
            ),
            State::Rebooting { exchange, ip } => (
                MessageType::Request,
                Dest::Broadcast,
                Ipv4Addr::UNSPECIFIED,
                !self.configured,
                vec![DhcpOption::RequestedIpAddress(*ip)],
                exchange.clone(),
            ),
            State::Renewing(exchange) => {
                let (Some(ip), Some(server)) = (lease_ip, held_server) else {
                    return;
                };
                (
                    MessageType::Request,
                    Dest::Server(server),
                    ip,
                    false,
                    vec![],
                    exchange.clone(),
                )
            }
            State::Rebinding(exchange) => {
                let Some(ip) = lease_ip else { return };
                (
                    MessageType::Request,
                    Dest::Broadcast,
                    ip,
                    false,
                    vec![],
                    exchange.clone(),
                )
            }
            State::Idle | State::Bound => return,
        };
        let packet = self.message(
            kind,
            exchange.xid,
            exchange.started,
            now,
            ciaddr,
            broadcast,
            &options,
        );
        self.out.push_back(Action::Send {
            to,
            kind,
            first: exchange.sent == 0,
            packet,
        });
        let unbound = matches!(
            self.state,
            State::Selecting { .. } | State::Requesting { .. } | State::Rebooting { .. }
        );
        if let Some(exchange) = self.exchange_mut() {
            exchange.sent += 1;
        }
        if unbound && exchange.sent == 0 {
            // The first send; `retransmit` schedules the later ones.
            self.schedule(now);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn message(
        &self,
        kind: MessageType,
        xid: u32,
        started: Duration,
        now: Duration,
        ciaddr: Ipv4Addr,
        broadcast: bool,
        extra: &[DhcpOption],
    ) -> Vec<u8> {
        let unspecified = Ipv4Addr::UNSPECIFIED;
        let mut message = Message::new_with_id(
            xid,
            ciaddr,
            unspecified,
            unspecified,
            unspecified,
            &self.chaddr,
        );
        let secs = now
            .saturating_sub(started)
            .as_secs()
            .min(u64::from(u16::MAX));
        message.set_secs(secs as u16);
        if broadcast {
            message.set_flags(v4::Flags::default().set_broadcast());
        }
        let opts = message.opts_mut();
        opts.insert(DhcpOption::MessageType(kind));
        // As udhcpc identifies itself: the server keeps the same lease.
        let mut id = vec![1];
        id.extend_from_slice(&self.chaddr);
        opts.insert(DhcpOption::ClientIdentifier(id));
        if kind != MessageType::Release {
            opts.insert(DhcpOption::MaxMessageSize(576));
            opts.insert(DhcpOption::ParameterRequestList(PARAMETERS.to_vec()));
        }
        for option in extra {
            opts.insert(option.clone());
        }
        let mut bytes = message.to_vec().unwrap_or_default();
        if bytes.len() < MIN_MESSAGE {
            bytes.resize(MIN_MESSAGE, 0);
        }
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dhcproto::v4::Flags;

    const MAC: [u8; 6] = [0x02, 0, 0, 0, 0, 0x30];
    const SERVER: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);
    const OTHER_SERVER: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 2);
    const IP: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 30);
    const IP2: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 31);
    const LEASE: u32 = 3600;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    fn home() -> Option<Network> {
        Some(Network {
            ssid: "Home".into(),
            bssid: [2, 0, 0, 0, 0, 1],
        })
    }

    /// Ids 1, 2, 3, …; jitter from the same counter (so ±1 s, but
    /// predictable).
    fn machine() -> Machine {
        let mut n = 0u32;
        Machine::new(
            MAC,
            Box::new(move || {
                n += 1;
                n
            }),
        )
    }

    /// A sent message, decoded.
    #[derive(Debug)]
    struct Sent {
        to: Dest,
        first: bool,
        message: Message,
    }

    impl Sent {
        fn kind(&self) -> MessageType {
            self.message.opts().msg_type().unwrap()
        }

        fn opt(&self, code: OptionCode) -> Option<&DhcpOption> {
            self.message.opts().get(code)
        }
    }

    /// Drains the actions: what was sent, and the rest.
    fn drain(m: &mut Machine) -> (Vec<Sent>, Vec<Action>) {
        let (mut sent, mut rest) = (vec![], vec![]);
        while let Some(action) = m.poll_action() {
            match action {
                Action::Send {
                    to, first, packet, ..
                } => {
                    assert!(packet.len() >= MIN_MESSAGE);
                    sent.push(Sent {
                        to,
                        first,
                        message: Message::decode(&mut Decoder::new(&packet)).unwrap(),
                    })
                }
                other => rest.push(other),
            }
        }
        (sent, rest)
    }

    fn reply(to: &Sent, kind: MessageType, server: Ipv4Addr, ip: Ipv4Addr) -> Vec<u8> {
        let mut m = Message::new_with_id(
            to.message.xid(),
            Ipv4Addr::UNSPECIFIED,
            ip,
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::UNSPECIFIED,
            &MAC,
        );
        m.set_opcode(Opcode::BootReply);
        let opts = m.opts_mut();
        opts.insert(DhcpOption::MessageType(kind));
        opts.insert(DhcpOption::ServerIdentifier(server));
        if kind != MessageType::Nak {
            opts.insert(DhcpOption::AddressLeaseTime(LEASE));
            opts.insert(DhcpOption::SubnetMask(Ipv4Addr::new(255, 255, 255, 0)));
            opts.insert(DhcpOption::Router(vec![SERVER]));
            opts.insert(DhcpOption::DomainNameServer(vec![SERVER, OTHER_SERVER]));
            opts.insert(DhcpOption::DomainName("example.test".into()));
        }
        m.to_vec().unwrap()
    }

    fn leased(rest: &[Action]) -> Vec<(u32, How, Ipv4Addr)> {
        rest.iter()
            .filter_map(|a| match a {
                Action::Event(Event::Leased { ask, how, ip }) => Some((*ask, *how, *ip)),
                _ => None,
            })
            .collect()
    }

    /// DISCOVER, OFFER, REQUEST, ACK at t = 0..: bound to `IP`.
    fn bound(m: &mut Machine, ask: u32) {
        m.request(secs(0), home(), ask);
        let (discover, _) = drain(m);
        m.packet(
            secs(0),
            &reply(&discover[0], MessageType::Offer, SERVER, IP),
        );
        let (request, _) = drain(m);
        m.packet(secs(0), &reply(&request[0], MessageType::Ack, SERVER, IP));
        let (_, rest) = drain(m);
        assert_eq!(leased(&rest), [(ask, How::Fresh, IP)]);
        assert_eq!(m.phase(), Phase::Bound);
    }

    #[test]
    fn messages_round_trip_through_dhcproto() {
        let mut m = machine();
        m.request(secs(5), None, 1);
        let Some(Action::Send { packet, .. }) = m.poll_action() else {
            panic!("no DISCOVER");
        };
        let decoded = Message::decode(&mut Decoder::new(&packet)).unwrap();
        assert_eq!(decoded.opcode(), Opcode::BootRequest);
        assert_eq!(&decoded.chaddr()[..6], &MAC);
        assert_eq!(decoded.flags(), Flags::default().set_broadcast());
        assert_eq!(
            decoded.opts().get(OptionCode::ClientIdentifier),
            Some(&DhcpOption::ClientIdentifier(vec![1, 2, 0, 0, 0, 0, 0x30]))
        );
        // Encoding the decoded message gives the same bytes, padding aside.
        let again = decoded.to_vec().unwrap();
        assert_eq!(&packet[..again.len()], &again[..]);
        assert!(packet[again.len()..].iter().all(|b| *b == 0));
    }

    #[test]
    fn discover_offer_request_ack() {
        let mut m = machine();
        m.request(secs(0), home(), 7);
        let (discover, rest) = drain(&mut m);
        assert!(rest.is_empty());
        assert_eq!(discover.len(), 1);
        assert_eq!(discover[0].kind(), MessageType::Discover);
        assert_eq!(discover[0].to, Dest::Broadcast);
        assert!(discover[0].first);
        assert!(discover[0].message.flags().broadcast());
        assert_eq!(m.phase(), Phase::Selecting);
        assert!(!m.configured());

        m.packet(
            secs(1),
            &reply(&discover[0], MessageType::Offer, SERVER, IP),
        );
        let (request, _) = drain(&mut m);
        assert_eq!(request[0].kind(), MessageType::Request);
        assert_eq!(request[0].message.xid(), discover[0].message.xid());
        assert_eq!(request[0].message.ciaddr(), Ipv4Addr::UNSPECIFIED);
        assert_eq!(
            request[0].opt(OptionCode::RequestedIpAddress),
            Some(&DhcpOption::RequestedIpAddress(IP))
        );
        assert_eq!(
            request[0].opt(OptionCode::ServerIdentifier),
            Some(&DhcpOption::ServerIdentifier(SERVER))
        );
        assert_eq!(m.phase(), Phase::Requesting);

        m.packet(secs(2), &reply(&request[0], MessageType::Ack, SERVER, IP));
        let (sent, rest) = drain(&mut m);
        assert!(sent.is_empty());
        let Action::Configure(lease) = &rest[0] else {
            panic!("{rest:?}");
        };
        assert_eq!(lease.ip, IP);
        assert_eq!(lease.server, SERVER);
        assert_eq!(lease.netmask, Some(Ipv4Addr::new(255, 255, 255, 0)));
        assert_eq!(lease.routers, [SERVER]);
        assert_eq!(lease.dns, [SERVER, OTHER_SERVER]);
        assert_eq!(lease.domain.as_deref(), Some("example.test"));
        // Times from the REQUEST's first send; T1 and T2 by default.
        assert_eq!(lease.start, secs(1));
        assert_eq!(lease.t1, secs(1800));
        assert_eq!(lease.t2, secs(3150));
        assert_eq!(leased(&rest), [(7, How::Fresh, IP)]);
        assert!(m.configured());
        assert_eq!(m.deadline(), Some(secs(1801)));
    }

    #[test]
    fn replies_to_another_transaction_or_client_are_ignored() {
        let mut m = machine();
        m.request(secs(0), home(), 1);
        let (discover, _) = drain(&mut m);
        let offer = reply(&discover[0], MessageType::Offer, SERVER, IP);
        // Another xid.
        let mut other = Message::decode(&mut Decoder::new(&offer)).unwrap();
        other.set_xid(other.xid() ^ 1);
        m.packet(secs(1), &other.to_vec().unwrap());
        // Another client's hardware address.
        let mut other = Message::decode(&mut Decoder::new(&offer)).unwrap();
        other.set_chaddr(&[2, 0, 0, 0, 0, 0x99]);
        m.packet(secs(1), &other.to_vec().unwrap());
        // A request, not a reply (another client's, say).
        let mut other = Message::decode(&mut Decoder::new(&offer)).unwrap();
        other.set_opcode(Opcode::BootRequest);
        m.packet(secs(1), &other.to_vec().unwrap());
        // Garbage, and an ACK where an OFFER is due.
        m.packet(secs(1), &[1, 2, 3]);
        m.packet(secs(1), &reply(&discover[0], MessageType::Ack, SERVER, IP));
        assert_eq!(m.phase(), Phase::Selecting);
        assert!(drain(&mut m).0.is_empty());
        m.packet(secs(1), &offer);
        assert_eq!(m.phase(), Phase::Requesting);
    }

    #[test]
    fn the_first_of_several_offers_is_taken_and_only_its_server_answers() {
        let mut m = machine();
        m.request(secs(0), home(), 1);
        let (discover, _) = drain(&mut m);
        m.packet(
            secs(1),
            &reply(&discover[0], MessageType::Offer, SERVER, IP),
        );
        m.packet(
            secs(1),
            &reply(&discover[0], MessageType::Offer, OTHER_SERVER, IP2),
        );
        let (request, _) = drain(&mut m);
        assert_eq!(request.len(), 1, "one REQUEST, for the first offer");
        // The other server's ACK or NAK isn't for this REQUEST.
        m.packet(
            secs(2),
            &reply(&request[0], MessageType::Ack, OTHER_SERVER, IP2),
        );
        m.packet(
            secs(2),
            &reply(&request[0], MessageType::Nak, OTHER_SERVER, IP2),
        );
        assert_eq!(m.phase(), Phase::Requesting);
        m.packet(secs(2), &reply(&request[0], MessageType::Ack, SERVER, IP));
        assert_eq!(m.lease().unwrap().ip, IP);
    }

    #[test]
    fn lost_packets_are_sent_again_with_doubling_waits() {
        let mut m = machine();
        m.request(secs(0), home(), 1);
        let (first, _) = drain(&mut m);
        let mut at = Vec::new();
        let mut now = secs(0);
        for _ in 0..8 {
            now = m.deadline().unwrap();
            m.timeout(now);
            let (again, _) = drain(&mut m);
            assert_eq!(again.len(), 1);
            assert_eq!(again[0].kind(), MessageType::Discover);
            assert!(!again[0].first);
            assert_eq!(again[0].message.xid(), first[0].message.xid());
            at.push(now.as_millis());
        }
        let waits: Vec<u128> = std::iter::once(at[0])
            .chain(at.windows(2).map(|w| w[1] - w[0]))
            .collect();
        for (wait, nominal) in waits.iter().zip([2, 4, 8, 16, 32, 64, 64, 64]) {
            let nominal = nominal * 1000;
            assert!(
                (nominal - 1000..=nominal + 1000).contains(wait),
                "{waits:?}"
            );
        }
        // An OFFER after all that still counts, and `secs` says how long.
        let (_, _) = drain(&mut m);
        m.packet(now, &reply(&first[0], MessageType::Offer, SERVER, IP));
        let (request, _) = drain(&mut m);
        assert_eq!(request[0].kind(), MessageType::Request);
    }

    #[test]
    fn an_unanswered_request_starts_over_with_a_new_discover() {
        let mut m = machine();
        m.request(secs(0), home(), 1);
        let (discover, _) = drain(&mut m);
        m.packet(
            secs(0),
            &reply(&discover[0], MessageType::Offer, SERVER, IP),
        );
        let mut requests = drain(&mut m).0.len();
        while m.phase() == Phase::Requesting {
            m.timeout(m.deadline().unwrap());
            let (sent, _) = drain(&mut m);
            match sent[0].kind() {
                MessageType::Request => requests += 1,
                MessageType::Discover => {
                    assert_ne!(sent[0].message.xid(), discover[0].message.xid());
                    assert!(sent[0].first);
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(requests, REQUEST_TRIES as usize);
        assert_eq!(m.phase(), Phase::Selecting);
    }

    #[test]
    fn a_nak_starts_over() {
        let mut m = machine();
        m.request(secs(0), home(), 1);
        let (discover, _) = drain(&mut m);
        m.packet(
            secs(0),
            &reply(&discover[0], MessageType::Offer, SERVER, IP),
        );
        let (request, _) = drain(&mut m);
        m.packet(secs(1), &reply(&request[0], MessageType::Nak, SERVER, IP));
        let (sent, rest) = drain(&mut m);
        assert_eq!(sent[0].kind(), MessageType::Discover);
        assert!(rest.is_empty(), "nothing held, nothing lost: {rest:?}");
        assert_eq!(m.phase(), Phase::Selecting);
    }

    #[test]
    fn renews_at_t1_unicast_then_rebinds_at_t2_then_expires() {
        let mut m = machine();
        bound(&mut m, 1);
        assert_eq!(m.deadline(), Some(secs(1800)));
        m.timeout(secs(1799));
        assert!(m.poll_action().is_none());
        m.timeout(secs(1800));
        let (renew, _) = drain(&mut m);
        assert_eq!(m.phase(), Phase::Renewing);
        assert_eq!(renew[0].to, Dest::Server(SERVER));
        assert_eq!(renew[0].message.ciaddr(), IP);
        assert!(!renew[0].message.flags().broadcast());
        assert_eq!(renew[0].opt(OptionCode::ServerIdentifier), None);
        assert_eq!(renew[0].opt(OptionCode::RequestedIpAddress), None);
        // Again after half the time to T2 (3150): 675 s.
        assert_eq!(m.deadline(), Some(secs(2475)));
        m.timeout(secs(2475));
        let (again, _) = drain(&mut m);
        assert_eq!(again[0].message.xid(), renew[0].message.xid());
        // Then 337 s, then the floor of 60 s… until T2.
        let mut now = secs(2475);
        while m.phase() == Phase::Renewing {
            now = m.deadline().unwrap();
            m.timeout(now);
        }
        assert_eq!(now, secs(3150));
        let (sent, _) = drain(&mut m);
        let rebind = sent.last().unwrap();
        assert_eq!(m.phase(), Phase::Rebinding);
        assert_eq!(rebind.to, Dest::Broadcast);
        assert_eq!(rebind.message.ciaddr(), IP);
        assert!(rebind.first);
        while m.phase() == Phase::Rebinding {
            now = m.deadline().unwrap();
            m.timeout(now);
        }
        assert_eq!(now, secs(3600));
        let (sent, rest) = drain(&mut m);
        assert_eq!(
            rest,
            [
                Action::Deconfigure,
                Action::Event(Event::Lost { why: "expired" })
            ]
        );
        assert_eq!(sent.last().unwrap().kind(), MessageType::Discover);
        assert!(m.lease().is_none());
        assert!(!m.configured());
    }

    #[test]
    fn a_renewal_ack_extends_the_lease_and_a_nak_ends_it() {
        let mut m = machine();
        bound(&mut m, 1);
        m.timeout(secs(1800));
        let (renew, _) = drain(&mut m);
        m.packet(secs(1801), &reply(&renew[0], MessageType::Ack, SERVER, IP));
        let (_, rest) = drain(&mut m);
        assert_eq!(leased(&rest), [(1, How::Renewed, IP)]);
        assert_eq!(m.lease().unwrap().start, secs(1800));
        assert_eq!(m.deadline(), Some(secs(3600)));
        m.timeout(secs(3600));
        let (renew, _) = drain(&mut m);
        m.packet(secs(3601), &reply(&renew[0], MessageType::Nak, SERVER, IP));
        let (sent, rest) = drain(&mut m);
        assert_eq!(
            rest,
            [
                Action::Deconfigure,
                Action::Event(Event::Lost {
                    why: "refused (NAK)"
                })
            ]
        );
        assert_eq!(sent[0].kind(), MessageType::Discover);
    }

    #[test]
    fn a_rebinding_ack_from_any_server_counts() {
        let mut m = machine();
        bound(&mut m, 1);
        m.timeout(secs(1800));
        m.timeout(secs(3150));
        let (sent, _) = drain(&mut m);
        let rebind = sent.last().unwrap();
        m.packet(
            secs(3151),
            &reply(rebind, MessageType::Ack, OTHER_SERVER, IP),
        );
        let (_, rest) = drain(&mut m);
        assert_eq!(leased(&rest), [(1, How::Rebound, IP)]);
        assert_eq!(m.lease().unwrap().server, OTHER_SERVER);
    }

    #[test]
    fn the_same_network_before_t1_keeps_the_lease_without_a_packet() {
        let mut m = machine();
        bound(&mut m, 1);
        m.request(secs(1799), home(), 2);
        let (sent, rest) = drain(&mut m);
        assert!(sent.is_empty());
        assert_eq!(leased(&rest), [(2, How::Kept, IP)]);
        assert_eq!(m.phase(), Phase::Bound);
    }

    #[test]
    fn the_same_network_after_t1_confirms_it_by_init_reboot() {
        let mut m = machine();
        bound(&mut m, 1);
        m.request(secs(2000), home(), 2);
        let (sent, rest) = drain(&mut m);
        assert!(rest.is_empty());
        assert_eq!(m.phase(), Phase::Rebooting);
        let reboot = &sent[0];
        assert_eq!(reboot.kind(), MessageType::Request);
        assert_eq!(reboot.to, Dest::Broadcast);
        assert_eq!(reboot.message.ciaddr(), Ipv4Addr::UNSPECIFIED);
        assert_eq!(
            reboot.opt(OptionCode::RequestedIpAddress),
            Some(&DhcpOption::RequestedIpAddress(IP))
        );
        assert_eq!(reboot.opt(OptionCode::ServerIdentifier), None);
        m.packet(secs(2001), &reply(reboot, MessageType::Ack, SERVER, IP));
        let (_, rest) = drain(&mut m);
        assert_eq!(leased(&rest), [(2, How::Rebooted, IP)]);
        assert_eq!(m.lease().unwrap().start, secs(2000));
    }

    #[test]
    fn an_unanswered_or_refused_init_reboot_starts_over() {
        let mut m = machine();
        bound(&mut m, 1);
        m.request(secs(2000), home(), 2);
        drain(&mut m);
        while m.phase() == Phase::Rebooting {
            m.timeout(m.deadline().unwrap());
        }
        let (sent, rest) = drain(&mut m);
        let discover = sent.last().unwrap();
        assert_eq!(discover.kind(), MessageType::Discover);
        // It asks for the address it had.
        assert_eq!(
            discover.opt(OptionCode::RequestedIpAddress),
            Some(&DhcpOption::RequestedIpAddress(IP))
        );
        assert_eq!(rest, [Action::Deconfigure]);

        let mut m = machine();
        bound(&mut m, 1);
        m.request(secs(2000), home(), 2);
        let (sent, _) = drain(&mut m);
        m.packet(secs(2001), &reply(&sent[0], MessageType::Nak, SERVER, IP));
        let (sent, rest) = drain(&mut m);
        assert_eq!(sent[0].kind(), MessageType::Discover);
        assert_eq!(sent[0].opt(OptionCode::RequestedIpAddress), None);
        assert_eq!(
            rest,
            [
                Action::Deconfigure,
                Action::Event(Event::Lost {
                    why: "refused (NAK)"
                })
            ]
        );
    }

    #[test]
    fn another_network_or_access_point_starts_afresh() {
        for other in [
            Network {
                ssid: "Cafe".into(),
                bssid: [2, 0, 0, 0, 0, 1],
            },
            Network {
                ssid: "Home".into(),
                bssid: [2, 0, 0, 0, 0, 2],
            },
        ] {
            let mut m = machine();
            bound(&mut m, 1);
            m.request(secs(10), Some(other), 2);
            let (sent, rest) = drain(&mut m);
            assert_eq!(rest, [Action::Deconfigure]);
            assert_eq!(sent[0].kind(), MessageType::Discover);
            assert!(m.lease().is_none());
        }
        // Nor does an unknown network keep anything.
        let mut m = machine();
        bound(&mut m, 1);
        m.request(secs(10), None, 2);
        assert_eq!(m.phase(), Phase::Selecting);
    }

    #[test]
    fn a_lease_taken_on_a_network_not_known_is_confirmed_then_kept() {
        // wpa_supplicant joins by itself: the first request names no network.
        let mut m = machine();
        m.request(secs(0), None, 1);
        let (discover, _) = drain(&mut m);
        m.packet(
            secs(0),
            &reply(&discover[0], MessageType::Offer, SERVER, IP),
        );
        let (request, _) = drain(&mut m);
        m.packet(secs(0), &reply(&request[0], MessageType::Ack, SERVER, IP));
        drain(&mut m);
        // A resume on a network now known: INIT-REBOOT, not a DISCOVER.
        m.request(secs(60), home(), 2);
        let (sent, _) = drain(&mut m);
        assert_eq!(m.phase(), Phase::Rebooting);
        m.packet(secs(61), &reply(&sent[0], MessageType::Ack, SERVER, IP));
        drain(&mut m);
        // From then on, it is that network's lease.
        m.request(secs(120), home(), 3);
        let (sent, rest) = drain(&mut m);
        assert!(sent.is_empty());
        assert_eq!(leased(&rest), [(3, How::Kept, IP)]);
    }

    #[test]
    fn a_lease_expired_over_a_nap_is_asked_for_afresh() {
        let mut m = machine();
        bound(&mut m, 1);
        m.request(secs(4000), home(), 2);
        assert_eq!(m.phase(), Phase::Selecting);
        // A wake long after: every deadline passed at once.
        let mut m = machine();
        bound(&mut m, 1);
        m.timeout(secs(9000));
        let (_, rest) = drain(&mut m);
        assert!(rest.contains(&Action::Event(Event::Lost { why: "expired" })));
        assert_eq!(m.phase(), Phase::Selecting);
    }

    #[test]
    fn a_started_client_asks_for_the_address_the_interface_had() {
        let mut m = machine().with_hint(Some(IP2));
        m.request(secs(0), home(), 1);
        let (sent, _) = drain(&mut m);
        assert_eq!(
            sent[0].opt(OptionCode::RequestedIpAddress),
            Some(&DhcpOption::RequestedIpAddress(IP2))
        );
        assert!(!m.configured(), "it isn't the client's to take off");
    }

    #[test]
    fn a_release_goes_to_the_server_and_takes_the_address_off() {
        let mut m = machine();
        bound(&mut m, 1);
        m.release(secs(100));
        let (sent, rest) = drain(&mut m);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].kind(), MessageType::Release);
        assert_eq!(sent[0].to, Dest::Server(SERVER));
        assert_eq!(sent[0].message.ciaddr(), IP);
        assert_eq!(
            sent[0].opt(OptionCode::ServerIdentifier),
            Some(&DhcpOption::ServerIdentifier(SERVER))
        );
        assert_eq!(rest, [Action::Deconfigure, Action::Event(Event::Released)]);
        assert_eq!(m.phase(), Phase::Idle);
        assert_eq!(m.deadline(), None);
        // Nothing held: nothing to give back.
        m.release(secs(101));
        assert!(m.poll_action().is_none());
    }

    #[test]
    fn server_times_are_kept_in_order() {
        let mut m = machine();
        m.request(secs(0), home(), 1);
        let (discover, _) = drain(&mut m);
        m.packet(
            secs(0),
            &reply(&discover[0], MessageType::Offer, SERVER, IP),
        );
        let (request, _) = drain(&mut m);
        let mut ack = Message::decode(&mut Decoder::new(&reply(
            &request[0],
            MessageType::Ack,
            SERVER,
            IP,
        )))
        .unwrap();
        // T1 past T2 and T2 past the lease: ignored.
        ack.opts_mut().insert(DhcpOption::Renewal(5000));
        ack.opts_mut().insert(DhcpOption::Rebinding(4000));
        m.packet(secs(0), &ack.to_vec().unwrap());
        let lease = m.lease().unwrap();
        assert_eq!((lease.t1, lease.t2), (secs(1800), secs(3150)));
    }
}
