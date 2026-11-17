//! Doze's maintenance window: a dozing kindil is online only for a few
//! seconds every half hour, too short to reach over SSH. Every
//! [`MAINTENANCE_EVERY`] it stays awake, Wi-Fi up, for [`MAINTENANCE_WINDOW`]
//! after a weather fetch, and it doesn't nap while an SSH session is open
//! (each held for at most [`SSH_HOLD_MAX`]).
use chrono::{DateTime, DurationRound, TimeDelta, Utc};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

/// How often a dozing kindil opens a maintenance window. The window opens
/// after the first weather fetch on or after this long since the last
/// window (or the doze's start: the minutes awake before dozing were one).
pub const MAINTENANCE_EVERY: TimeDelta = TimeDelta::hours(2);
/// How long a maintenance window stays awake and online once the fetched
/// frame is on the panel.
pub const MAINTENANCE_WINDOW: TimeDelta = TimeDelta::minutes(2);
/// While held awake, how often kindil looks for SSH sessions.
pub const HOLD_CHECK: Duration = Duration::from_secs(5);
/// The most one SSH session keeps a dozing kindil awake: a connection left
/// open, or stuck, can't drain the battery.
pub const SSH_HOLD_MAX: TimeDelta = TimeDelta::minutes(30);
/// USBNetwork's dropbear, and kindil's own server (src/ssh.rs).
pub const SSH_PORTS: [u16; 2] = [22, 2222];
/// The kernel's TCP tables.
pub const TCP_TABLES: [&str; 2] = ["/proc/net/tcp", "/proc/net/tcp6"];

/// An established connection to an SSH server on the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Session {
    pub local_port: u16,
    pub remote: SocketAddr,
}

/// The established connections to `ports` in a `/proc/net/tcp` or
/// `/proc/net/tcp6` table.
pub fn sessions(table: &str, ports: &[u16]) -> Vec<Session> {
    table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut fields = line.split_whitespace().skip(1);
            let (local, remote, state) = (fields.next()?, fields.next()?, fields.next()?);
            // 01: TCP_ESTABLISHED.
            if state != "01" {
                return None;
            }
            let (_, local_port) = endpoint(local)?;
            let (ip, port) = endpoint(remote)?;
            ports.contains(&local_port).then_some(Session {
                local_port,
                remote: SocketAddr::new(ip, port),
            })
        })
        .collect()
}

/// `0100007F:0016`: the address as the kernel's 32-bit words in host order
/// (the Kindle's ARM is little-endian), then the port in hex.
fn endpoint(field: &str) -> Option<(IpAddr, u16)> {
    let (address, port) = field.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let words = (0..address.len() / 8)
        .map(|i| {
            let word = address.get(i * 8..i * 8 + 8)?;
            u32::from_str_radix(word, 16).ok().map(u32::to_le_bytes)
        })
        .collect::<Option<Vec<[u8; 4]>>>()?;
    let bytes: Vec<u8> = words.concat();
    let ip = match address.len() {
        8 => IpAddr::V4(Ipv4Addr::from(<[u8; 4]>::try_from(bytes).ok()?)),
        32 => IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(bytes).ok()?)),
        _ => return None,
    };
    Some((ip, port))
}

/// Why a dozing kindil stays awake instead of napping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hold {
    /// A maintenance window is open.
    Window,
    /// An SSH session is open.
    Ssh,
}

/// A doze's maintenance schedule and the SSH sessions it has seen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Maintenance {
    /// When the last window was due (or the doze began).
    last: DateTime<Utc>,
    /// This wake's fetch opens a window once its frame is on the panel.
    opening: bool,
    open_until: Option<DateTime<Utc>>,
    /// The open sessions, and when each was first seen.
    seen: Vec<(Session, DateTime<Utc>)>,
}

impl Maintenance {
    pub fn new(doze_began: DateTime<Utc>) -> Self {
        Self {
            last: doze_began,
            opening: false,
            open_until: None,
            seen: Vec::new(),
        }
    }

    /// At an online (fetching) wake: whether it opens a window. Times are
    /// compared in whole minutes, as wakes land a second or so past the
    /// minute and not always the same one.
    pub fn online_wake(&mut self, now: DateTime<Utc>) -> bool {
        let minute = |t: DateTime<Utc>| t.duration_trunc(TimeDelta::minutes(1)).unwrap_or(t);
        let since = minute(now) - minute(self.last);
        // A last window after now: the clock went back past it.
        let due = since >= MAINTENANCE_EVERY || since < TimeDelta::zero();
        if due {
            self.last = now;
            self.opening = true;
        }
        due
    }

    /// The clock was stepped by `by` from network time: the times noted so
    /// far were on the old clock.
    pub fn clock_stepped(&mut self, by: TimeDelta) {
        self.last += by;
        if let Some(until) = &mut self.open_until {
            *until += by;
        }
        for (_, since) in &mut self.seen {
            *since += by;
        }
    }

    /// Instead of a nap at `now`, with these SSH sessions open: whether to
    /// stay awake, and why. The window opened by an online wake starts at
    /// its first nap, once the fetched frame is on the panel.
    pub fn hold(&mut self, now: DateTime<Utc>, sessions: &[Session]) -> Option<Hold> {
        if std::mem::take(&mut self.opening) {
            self.open_until = Some(now + MAINTENANCE_WINDOW);
        }
        self.seen.retain(|(session, _)| sessions.contains(session));
        for session in sessions {
            if !self.seen.iter().any(|(seen, _)| seen == session) {
                self.seen.push((*session, now));
            }
        }
        // Whatever the clock does, a window holds for at most its length
        // from now, and a session for at most its own from now.
        for (_, since) in &mut self.seen {
            *since = (*since).min(now);
        }
        if let Some(until) = &mut self.open_until {
            *until = (*until).min(now + MAINTENANCE_WINDOW);
        }
        if self.open_until.is_some_and(|until| now < until) {
            Some(Hold::Window)
        } else if self
            .seen
            .iter()
            .any(|(_, since)| now - *since < SSH_HOLD_MAX)
        {
            Some(Hold::Ssh)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn wall(hour: u32, minute: u32, second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 16, hour, minute, second)
            .unwrap()
    }

    const TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000:08AE 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 1234 1 c0000000 100 0 0 10 0
   1: 7801A8C0:08AE 0501A8C0:D431 01 00000000:00000000 02:000A7B2D 00000000     0        0 1235 2 c0000000 20 4 30 10 -1
   2: 7801A8C0:0016 0501A8C0:D432 01 00000000:00000000 02:000A7B2D 00000000     0        0 1236 2 c0000000 20 4 30 10 -1
   3: 7801A8C0:0016 0501A8C0:D433 06 00000000:00000000 03:00000F9D 00000000     0        0 0 3 c0000000
   4: 7801A8C0:A1B2 0A0B0C0D:01BB 01 00000000:00000000 00:00000000 00000000     0        0 1237 1 c0000000 20 4 30 10 -1
";

    const TCP6: &str = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000000000000000000000000000:08AE 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 2234 1 c0000000 100 0 0 10 0
   1: 0000000000000000FFFF00007801A8C0:08AE 0000000000000000FFFF00000501A8C0:D434 01 00000000:00000000 02:000A7B2D 00000000     0        0 2235 2 c0000000 20 4 30 10 -1
   2: 000080FE00000000FF020002FEEEEEEE:0016 000080FE00000000FF0200020D0C0B0A:E001 01 00000000:00000000 02:000A7B2D 00000000     0        0 2236 2 c0000000 20 4 30 10 -1
";

    #[test]
    fn established_ssh_connections_in_the_tcp_tables() {
        let ip4 = |a, b, c, d| IpAddr::V4(Ipv4Addr::new(a, b, c, d));
        // Listening (0A), closing (06) and other ports' connections don't count.
        assert_eq!(
            sessions(TCP, &SSH_PORTS),
            [
                Session {
                    local_port: 2222,
                    remote: SocketAddr::new(ip4(192, 168, 1, 5), 0xD431),
                },
                Session {
                    local_port: 22,
                    remote: SocketAddr::new(ip4(192, 168, 1, 5), 0xD432),
                },
            ]
        );
        assert_eq!(
            sessions(TCP6, &SSH_PORTS),
            [
                Session {
                    local_port: 2222,
                    remote: SocketAddr::new(
                        "::ffff:192.168.1.5".parse::<Ipv6Addr>().unwrap().into(),
                        0xD434
                    ),
                },
                Session {
                    local_port: 22,
                    remote: SocketAddr::new(
                        "fe80::200:2ff:a0b:c0d".parse::<Ipv6Addr>().unwrap().into(),
                        0xE001
                    ),
                },
            ]
        );
        assert_eq!(sessions(TCP, &[2222]).len(), 1);
        assert_eq!(sessions("", &SSH_PORTS), []);
        assert_eq!(sessions("header\n   0: garbage\n", &SSH_PORTS), []);
    }

    #[test]
    fn a_window_every_two_hours_on_a_fetch() {
        let mut m = Maintenance::new(wall(9, 0, 30));
        // The doze's start counts as one: fetches every half hour don't open one.
        for minute in [0, 30] {
            assert!(!m.online_wake(wall(10, minute, 1)));
        }
        // The first window: the first fetch two hours after the doze began,
        // a second earlier in the minute or not.
        assert!(m.online_wake(wall(11, 0, 1)));
        assert!(!m.online_wake(wall(11, 30, 1)));
        assert!(!m.online_wake(wall(12, 30, 1)));
        assert!(m.online_wake(wall(13, 0, 2)));
        // The 15:00 fetch missed (the device was in use): the next one opens it.
        assert!(!m.online_wake(wall(14, 30, 1)));
        assert!(m.online_wake(wall(15, 30, 1)));
        assert!(!m.online_wake(wall(17, 0, 1)));
        assert!(m.online_wake(wall(17, 30, 1)));
    }

    #[test]
    fn the_schedule_moves_with_a_step_of_network_time() {
        let step = TimeDelta::seconds(724);
        let mut m = Maintenance::new(wall(9, 0, 30));
        // The clock was 724 s slow: the doze began at 9:12:34.
        m.clock_stepped(step);
        assert!(!m.online_wake(wall(11, 0, 1)));
        assert!(m.online_wake(wall(11, 12, 1)));
        // Set back past the last window: one opens rather than none for hours.
        let mut m = Maintenance::new(wall(11, 0, 0));
        assert!(m.online_wake(wall(9, 30, 1)));
        // A window open when the clock goes forward with network time ends
        // as it would have; set back, it still holds no longer than its length.
        let mut m = Maintenance::new(wall(9, 0, 0));
        assert!(m.online_wake(wall(11, 0, 1)));
        assert_eq!(m.hold(wall(11, 0, 20), &[]), Some(Hold::Window));
        m.clock_stepped(step);
        assert_eq!(m.hold(wall(11, 14, 23), &[]), Some(Hold::Window));
        assert_eq!(m.hold(wall(11, 14, 24), &[]), None);
        let mut m = Maintenance::new(wall(9, 0, 0));
        assert!(m.online_wake(wall(11, 0, 1)));
        assert_eq!(m.hold(wall(11, 0, 20), &[]), Some(Hold::Window));
        assert_eq!(m.hold(wall(10, 0, 0), &[]), Some(Hold::Window));
        assert_eq!(m.hold(wall(10, 2, 0), &[]), None);
    }

    #[test]
    fn a_session_holds_no_longer_for_a_clock_set_back() {
        let one = Session {
            local_port: 22,
            remote: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 5)), 50_001),
        };
        let mut m = Maintenance::new(wall(9, 0, 0));
        assert_eq!(m.hold(wall(10, 0, 0), &[one]), Some(Hold::Ssh));
        // An hour back: half an hour from then, not an hour and a half.
        assert_eq!(m.hold(wall(9, 0, 0), &[one]), Some(Hold::Ssh));
        assert_eq!(m.hold(wall(9, 30, 0), &[one]), None);
    }

    #[test]
    fn a_window_holds_the_naps_for_two_minutes_after_its_fetch() {
        let mut m = Maintenance::new(wall(9, 0, 0));
        assert_eq!(m.hold(wall(9, 30, 3), &[]), None);
        assert!(m.online_wake(wall(11, 0, 1)));
        // Opens when the fetched frame is on the panel, at the first nap.
        assert_eq!(m.hold(wall(11, 0, 20), &[]), Some(Hold::Window));
        assert_eq!(m.hold(wall(11, 1, 1), &[]), Some(Hold::Window));
        assert_eq!(m.hold(wall(11, 2, 19), &[]), Some(Hold::Window));
        assert_eq!(m.hold(wall(11, 2, 20), &[]), None);
        assert_eq!(m.hold(wall(11, 3, 1), &[]), None);
    }

    #[test]
    fn an_ssh_session_holds_the_naps_until_it_closes_for_at_most_half_an_hour() {
        let session = |port| Session {
            local_port: 2222,
            remote: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 5)), port),
        };
        let (one, two) = (session(50_001), session(50_002));
        let mut m = Maintenance::new(wall(9, 0, 0));
        assert_eq!(m.hold(wall(9, 1, 0), &[one]), Some(Hold::Ssh));
        assert_eq!(m.hold(wall(9, 1, 5), &[one]), Some(Hold::Ssh));
        // Closed: nap.
        assert_eq!(m.hold(wall(9, 1, 10), &[]), None);
        // Open since 9:02; stuck past half an hour, it no longer holds.
        assert_eq!(m.hold(wall(9, 2, 0), &[one]), Some(Hold::Ssh));
        assert_eq!(m.hold(wall(9, 31, 59), &[one]), Some(Hold::Ssh));
        assert_eq!(m.hold(wall(9, 32, 0), &[one]), None);
        assert_eq!(m.hold(wall(9, 40, 0), &[one]), None);
        // A new session holds again, for its own half hour.
        assert_eq!(m.hold(wall(9, 41, 0), &[one, two]), Some(Hold::Ssh));
        assert_eq!(m.hold(wall(10, 11, 0), &[one, two]), None);
        // An open window is the reason while it lasts.
        let mut m = Maintenance::new(wall(9, 0, 0));
        assert!(m.online_wake(wall(11, 0, 1)));
        assert_eq!(m.hold(wall(11, 0, 20), &[one]), Some(Hold::Window));
        assert_eq!(m.hold(wall(11, 2, 20), &[one]), Some(Hold::Ssh));
    }
}
