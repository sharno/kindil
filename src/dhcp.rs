//! udhcpc's hook while kindil runs Wi-Fi (src/net.rs): kindil itself, run as
//! `kindil-dhcp` (a link to its binary). It sets the address, the default
//! route and the name servers, which Amazon's wifid and connection manager
//! set otherwise.
//!
//! udhcpc runs it with the event as its argument and the lease in its
//! environment: `interface`, `ip`, `broadcast`, `subnet`, `router` and `dns`
//! (space-separated lists) and `domain`.
use std::collections::BTreeMap;
use std::process::Command;

/// The name kindil answers to as the hook.
pub const NAME: &str = "kindil-dhcp";
/// Where the resolver reads the name servers (`/etc/resolv.conf` links here).
const RESOLV: &str = "/var/run/resolv.conf";
/// Enough to clear any default routes an earlier lease left.
const MAX_OLD_ROUTES: usize = 8;
/// Where the hook leaves a marker of each lease it takes, one file per
/// udhcpc (its parent's pid): kindil's own Wi-Fi (src/station.rs) counts a
/// join only by a marker of the udhcpc running now, with the address it
/// names on the interface. A late hook of an earlier udhcpc writes its own
/// file, which nobody reads.
pub const MARKER_PREFIX: &str = "/var/run/kindil-lease.";

/// CLOCK_MONOTONIC, the same in kindil and in its hook.
pub fn monotonic() -> std::time::Duration {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime fills the timespec it is given.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    std::time::Duration::new(now.tv_sec as u64, now.tv_nsec as u32)
}

/// The kind of lease a marker records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Leased {
    /// A new lease: DISCOVER, OFFER, REQUEST, ACK.
    Bound,
    /// The lease renewed.
    Renew,
}

/// One lease the hook took: what, the address, and when the hook started
/// (after udhcpc had the server's answer), on [`monotonic`]'s clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Marker {
    pub leased: Leased,
    pub ip: std::net::Ipv4Addr,
    pub at: std::time::Duration,
}

impl Marker {
    pub fn text(&self) -> String {
        let event = match self.leased {
            Leased::Bound => "bound",
            Leased::Renew => "renew",
        };
        format!("{event} {} {}\n", self.ip, self.at.as_nanos())
    }

    pub fn parse(text: &str) -> Option<Self> {
        let mut words = text.split_whitespace();
        let leased = match words.next()? {
            "bound" => Leased::Bound,
            "renew" => Leased::Renew,
            _ => return None,
        };
        let ip = words.next()?.parse().ok()?;
        let nanos: u64 = words.next()?.parse().ok()?;
        words.next().is_none().then_some(Self {
            leased,
            ip,
            at: std::time::Duration::from_nanos(nanos),
        })
    }
}

pub fn marker_path(pid: u32) -> String {
    format!("{MARKER_PREFIX}{pid}")
}

/// The marker of udhcpc `pid`, if it took a lease.
pub fn marker(pid: u32) -> Option<Marker> {
    Marker::parse(&std::fs::read_to_string(marker_path(pid)).ok()?)
}

/// Removes the markers of every udhcpc but `keep`.
pub fn clear_markers(keep: Option<u32>) {
    let Some((dir, prefix)) = MARKER_PREFIX.rsplit_once('/') else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|n| n.strip_prefix(prefix))
            .and_then(|p| p.parse::<u32>().ok())
        else {
            continue;
        };
        if Some(pid) != keep {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The marker a hook run leaves, if it took a lease.
pub fn marker_of(
    event: &str,
    env: &BTreeMap<String, String>,
    at: std::time::Duration,
) -> Option<Marker> {
    let leased = match event {
        "bound" => Leased::Bound,
        "renew" => Leased::Renew,
        _ => return None,
    };
    Some(Marker {
        leased,
        ip: env.get("ip")?.parse().ok()?,
        at,
    })
}

/// What a hook run does.
#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    /// Run in order; a failure doesn't stop the rest.
    pub commands: Vec<Vec<String>>,
    /// Routes to try in order until one is added.
    pub gateways: Vec<Vec<String>>,
    /// The resolver file's new content, if it changes.
    pub resolv: Option<String>,
}

pub fn plan(event: &str, env: &BTreeMap<String, String>) -> Plan {
    let get = |key: &str| env.get(key).map(String::as_str).unwrap_or_default();
    let interface = get("interface");
    let words = |key: &str| {
        get(key)
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let command = |args: &[&str]| args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>();
    match event {
        "deconfig" => Plan {
            commands: vec![command(&["/sbin/ifconfig", interface, "0.0.0.0"])],
            gateways: vec![],
            resolv: None,
        },
        "bound" | "renew" => {
            let mut address = command(&["/sbin/ifconfig", interface, get("ip")]);
            if !get("broadcast").is_empty() {
                address.extend(["broadcast".into(), get("broadcast").into()]);
            }
            if !get("subnet").is_empty() {
                address.extend(["netmask".into(), get("subnet").into()]);
            }
            let mut commands = vec![address];
            commands.extend(std::iter::repeat_n(
                command(&["/sbin/route", "del", "default", "dev", interface]),
                MAX_OLD_ROUTES,
            ));
            let gateways = words("router")
                .iter()
                .map(|gw| command(&["/sbin/route", "add", "default", "gw", gw, "dev", interface]))
                .collect();
            let mut resolv = String::new();
            if !get("domain").is_empty() {
                resolv.push_str(&format!("domain {}\n", get("domain")));
            }
            for server in words("dns") {
                resolv.push_str(&format!("nameserver {server}\n"));
            }
            Plan {
                commands,
                gateways,
                resolv: Some(resolv),
            }
        }
        // leasefail, nak: nothing to change.
        _ => Plan {
            commands: vec![],
            gateways: vec![],
            resolv: None,
        },
    }
}

/// Runs the hook for udhcpc; always succeeds, as udhcpc ignores failures.
pub fn hook(event: &str) {
    // Who and when, before anything: udhcpc has the server's answer now.
    let at = monotonic();
    // SAFETY: getppid has no failure mode.
    let udhcpc = unsafe { libc::getppid() } as u32;
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let plan = plan(event, &env);
    let quiet = |args: &[String]| {
        Command::new(&args[0])
            .args(&args[1..])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    };
    for command in &plan.commands {
        quiet(command);
    }
    let _ = plan.gateways.iter().find(|gateway| quiet(gateway));
    if let Some(resolv) = plan.resolv {
        let part = format!("{RESOLV}.new");
        if std::fs::write(&part, resolv).is_ok() {
            let _ = std::fs::rename(&part, RESOLV);
        }
    }
    // Last: the address is on the interface by now.
    if let Some(marker) = marker_of(event, &env, at) {
        let path = marker_path(udhcpc);
        let part = format!("{path}.new");
        if std::fs::write(&part, marker.text()).is_ok() {
            let _ = std::fs::rename(&part, path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lease() -> BTreeMap<String, String> {
        [
            ("interface", "wlan0"),
            ("ip", "192.168.1.130"),
            ("broadcast", "192.168.1.255"),
            ("subnet", "255.255.255.0"),
            ("router", "192.168.1.1 192.168.1.2"),
            ("dns", "192.168.1.1 9.9.9.9"),
            ("domain", "home.arpa"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect()
    }

    fn words(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_owned).collect()
    }

    #[test]
    fn a_lease_sets_the_address_route_and_name_servers() {
        let plan = plan("bound", &lease());
        assert_eq!(
            plan.commands[0],
            words(
                "/sbin/ifconfig wlan0 192.168.1.130 broadcast 192.168.1.255 netmask 255.255.255.0"
            )
        );
        assert!(plan.commands[1..]
            .iter()
            .all(|c| *c == words("/sbin/route del default dev wlan0")));
        assert_eq!(
            plan.gateways,
            [
                words("/sbin/route add default gw 192.168.1.1 dev wlan0"),
                words("/sbin/route add default gw 192.168.1.2 dev wlan0"),
            ]
        );
        assert_eq!(
            plan.resolv.as_deref(),
            Some("domain home.arpa\nnameserver 192.168.1.1\nnameserver 9.9.9.9\n")
        );
        assert_eq!(super::plan("renew", &lease()), plan);
    }

    #[test]
    fn a_bare_lease_sets_only_what_it_has() {
        let mut bare = lease();
        for key in ["broadcast", "subnet", "domain", "router"] {
            bare.remove(key);
        }
        let plan = plan("bound", &bare);
        assert_eq!(
            plan.commands[0],
            words("/sbin/ifconfig wlan0 192.168.1.130")
        );
        assert!(plan.gateways.is_empty());
        assert_eq!(
            plan.resolv.as_deref(),
            Some("nameserver 192.168.1.1\nnameserver 9.9.9.9\n")
        );
    }

    #[test]
    fn markers_round_trip_and_name_the_lease() {
        let at = std::time::Duration::new(12_345, 678_901_234);
        let marker = marker_of("bound", &lease(), at).unwrap();
        assert_eq!(marker.leased, Leased::Bound);
        assert_eq!(marker.ip, std::net::Ipv4Addr::new(192, 168, 1, 130));
        assert_eq!(Marker::parse(&marker.text()), Some(marker));
        assert_eq!(
            marker_of("renew", &lease(), at).unwrap().leased,
            Leased::Renew
        );
        for event in ["deconfig", "leasefail", "nak"] {
            assert_eq!(marker_of(event, &lease(), at), None);
        }
        let mut no_ip = lease();
        no_ip.remove("ip");
        assert_eq!(marker_of("bound", &no_ip, at), None);
        assert_eq!(Marker::parse("bound 1.2.3.4"), None);
        assert_eq!(Marker::parse("bound 1.2.3.4 5 extra"), None);
        assert_eq!(Marker::parse("released 1.2.3.4 5"), None);
        assert_eq!(marker_path(42), "/var/run/kindil-lease.42");
        assert!(monotonic() <= monotonic());
    }

    #[test]
    fn deconfig_clears_the_address_and_other_events_change_nothing() {
        let plan = plan("deconfig", &lease());
        assert_eq!(plan.commands, [words("/sbin/ifconfig wlan0 0.0.0.0")]);
        assert_eq!(plan.resolv, None);
        for event in ["leasefail", "nak", ""] {
            assert_eq!(super::plan(event, &lease()).commands.len(), 0, "{event}");
        }
    }
}
