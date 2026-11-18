//! Putting a lease on the interface, as the udhcpc hook did with ifconfig
//! and route (src/dhcp.rs), but through the ioctls those use: the address,
//! netmask and broadcast address, the default route (any old ones on the
//! interface deleted first), and the name servers in /var/run/resolv.conf.
//! The address of 0.0.0.0 takes it off again.
use super::machine::Lease;
use std::io;
use std::net::Ipv4Addr;

/// Enough to clear any default routes an earlier lease left.
const MAX_OLD_ROUTES: usize = 8;

/// The interface's ioctls; the tests record them.
pub trait Interface {
    fn set_address(&mut self, ip: Ipv4Addr) -> io::Result<()>;
    fn set_netmask(&mut self, mask: Ipv4Addr) -> io::Result<()>;
    fn set_broadcast(&mut self, broadcast: Ipv4Addr) -> io::Result<()>;
    /// One default route on the interface, any gateway; an error once
    /// there is none.
    fn delete_default_route(&mut self) -> io::Result<()>;
    fn add_default_route(&mut self, gateway: Ipv4Addr) -> io::Result<()>;
}

/// The broadcast address: the server's, else the subnet's.
pub fn broadcast(lease: &Lease) -> Option<Ipv4Addr> {
    lease.broadcast.or_else(|| {
        let mask = u32::from(lease.netmask?);
        Some(Ipv4Addr::from(u32::from(lease.ip) | !mask))
    })
}

/// Puts `lease` on the interface; what is the same as in `before` (the
/// lease on it now) is left alone, so a renewal changes nothing. Errors
/// are returned, after trying the rest.
pub fn configure(
    iface: &mut dyn Interface,
    before: Option<&Lease>,
    lease: &Lease,
) -> Vec<io::Error> {
    let mut errors = Vec::new();
    let mut note = |result: io::Result<()>| {
        if let Err(error) = result {
            errors.push(error);
        }
    };
    let addressing = |l: &Lease| (l.ip, l.netmask, broadcast(l));
    let readdressed = before.map(addressing) != Some(addressing(lease));
    if readdressed {
        note(iface.set_address(lease.ip));
        // Without a netmask, the kernel's classful one, and its broadcast.
        if let Some(mask) = lease.netmask {
            note(iface.set_netmask(mask));
        }
        if let Some(broadcast) = broadcast(lease) {
            note(iface.set_broadcast(broadcast));
        }
    }
    if readdressed || before.map(|b| &b.routers) != Some(&lease.routers) {
        for _ in 0..MAX_OLD_ROUTES {
            if iface.delete_default_route().is_err() {
                break;
            }
        }
        // The first router the kernel takes (one outside the subnet won't do).
        let mut last = None;
        for router in &lease.routers {
            match iface.add_default_route(*router) {
                Ok(()) => {
                    last = None;
                    break;
                }
                Err(error) => last = Some(error),
            }
        }
        errors.extend(last);
    }
    errors
}

/// Takes the address off (and the routes through it with it).
pub fn deconfigure(iface: &mut dyn Interface) -> io::Result<()> {
    iface.set_address(Ipv4Addr::UNSPECIFIED)
}

/// resolv.conf for `lease`: its domain, then its name servers.
pub fn resolv(lease: &Lease) -> String {
    let mut text = String::new();
    if let Some(domain) = &lease.domain {
        // One line, whatever the server sent.
        let domain: String = domain.chars().filter(|c| !c.is_control()).collect();
        if !domain.trim().is_empty() {
            text.push_str(&format!("domain {}\n", domain.trim()));
        }
    }
    for server in &lease.dns {
        text.push_str(&format!("nameserver {server}\n"));
    }
    text
}

/// Writes `text` to `path` through a rename, so readers see the old file or
/// the new one; unchanged files are left alone.
pub fn write_resolv(path: &std::path::Path, text: &str) -> io::Result<()> {
    if std::fs::read_to_string(path).ok().as_deref() == Some(text) {
        return Ok(());
    }
    let mut part = path.as_os_str().to_owned();
    part.push(".new");
    std::fs::write(&part, text)?;
    std::fs::rename(&part, path)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::time::Duration;

    /// Records the ioctls; refuses routes through `unreachable` and stops
    /// deleting once `routes` default routes are gone.
    #[derive(Debug, Default)]
    pub struct Recorder {
        pub calls: Vec<String>,
        pub routes: usize,
        pub unreachable: Vec<Ipv4Addr>,
    }

    impl Interface for Recorder {
        fn set_address(&mut self, ip: Ipv4Addr) -> io::Result<()> {
            self.calls.push(format!("SIOCSIFADDR {ip}"));
            Ok(())
        }
        fn set_netmask(&mut self, mask: Ipv4Addr) -> io::Result<()> {
            self.calls.push(format!("SIOCSIFNETMASK {mask}"));
            Ok(())
        }
        fn set_broadcast(&mut self, broadcast: Ipv4Addr) -> io::Result<()> {
            self.calls.push(format!("SIOCSIFBRDADDR {broadcast}"));
            Ok(())
        }
        fn delete_default_route(&mut self) -> io::Result<()> {
            if self.routes == 0 {
                return Err(io::Error::from_raw_os_error(libc::ESRCH));
            }
            self.routes -= 1;
            self.calls.push("SIOCDELRT default".into());
            Ok(())
        }
        fn add_default_route(&mut self, gateway: Ipv4Addr) -> io::Result<()> {
            if self.unreachable.contains(&gateway) {
                return Err(io::Error::from_raw_os_error(libc::ENETUNREACH));
            }
            self.routes += 1;
            self.calls.push(format!("SIOCADDRT default gw {gateway}"));
            Ok(())
        }
    }

    pub fn lease() -> Lease {
        Lease {
            ip: Ipv4Addr::new(192, 0, 2, 30),
            server: Ipv4Addr::new(192, 0, 2, 1),
            netmask: Some(Ipv4Addr::new(255, 255, 255, 0)),
            broadcast: None,
            routers: vec![Ipv4Addr::new(192, 0, 2, 1), Ipv4Addr::new(192, 0, 2, 2)],
            dns: vec![Ipv4Addr::new(192, 0, 2, 1), Ipv4Addr::new(198, 51, 100, 53)],
            domain: Some("example.test".into()),
            duration: Duration::from_secs(3600),
            t1: Duration::from_secs(1800),
            t2: Duration::from_secs(3150),
            start: Duration::ZERO,
        }
    }

    #[test]
    fn a_lease_sets_the_address_then_replaces_the_default_routes() {
        let mut iface = Recorder {
            routes: 2,
            ..Recorder::default()
        };
        assert!(configure(&mut iface, None, &lease()).is_empty());
        assert_eq!(
            iface.calls,
            [
                "SIOCSIFADDR 192.0.2.30",
                "SIOCSIFNETMASK 255.255.255.0",
                "SIOCSIFBRDADDR 192.0.2.255",
                "SIOCDELRT default",
                "SIOCDELRT default",
                "SIOCADDRT default gw 192.0.2.1",
            ]
        );
        assert_eq!(iface.routes, 1);
    }

    #[test]
    fn a_router_the_kernel_refuses_gives_way_to_the_next() {
        let mut iface = Recorder {
            unreachable: vec![Ipv4Addr::new(192, 0, 2, 1)],
            ..Recorder::default()
        };
        assert!(configure(&mut iface, None, &lease()).is_empty());
        assert_eq!(
            iface.calls.last().map(String::as_str),
            Some("SIOCADDRT default gw 192.0.2.2")
        );
        // None taken: the error is reported.
        let mut iface = Recorder {
            unreachable: lease().routers,
            ..Recorder::default()
        };
        assert_eq!(configure(&mut iface, None, &lease()).len(), 1);
    }

    #[test]
    fn a_renewal_with_nothing_new_changes_nothing() {
        let mut iface = Recorder::default();
        let mut renewed = lease();
        renewed.start = Duration::from_secs(1800);
        renewed.dns.reverse();
        assert!(configure(&mut iface, Some(&lease()), &renewed).is_empty());
        assert!(iface.calls.is_empty(), "{:?}", iface.calls);
        // A new router: only the route.
        renewed.routers = vec![Ipv4Addr::new(192, 0, 2, 9)];
        configure(&mut iface, Some(&lease()), &renewed);
        assert_eq!(iface.calls, ["SIOCADDRT default gw 192.0.2.9"]);
    }

    #[test]
    fn a_bare_lease_sets_only_what_it_has() {
        let mut iface = Recorder::default();
        let bare = Lease {
            netmask: None,
            broadcast: None,
            routers: vec![],
            ..lease()
        };
        configure(&mut iface, None, &bare);
        assert_eq!(iface.calls, ["SIOCSIFADDR 192.0.2.30"]);
        // The server's broadcast address wins over the subnet's.
        let with = Lease {
            broadcast: Some(Ipv4Addr::new(192, 0, 2, 127)),
            ..lease()
        };
        assert_eq!(broadcast(&with), Some(Ipv4Addr::new(192, 0, 2, 127)));
    }

    #[test]
    fn deconfiguring_sets_the_unspecified_address() {
        let mut iface = Recorder::default();
        deconfigure(&mut iface).unwrap();
        assert_eq!(iface.calls, ["SIOCSIFADDR 0.0.0.0"]);
    }

    #[test]
    fn resolv_conf_has_the_domain_and_the_name_servers() {
        assert_eq!(
            resolv(&lease()),
            "domain example.test\nnameserver 192.0.2.1\nnameserver 198.51.100.53\n"
        );
        let odd = Lease {
            domain: Some("evil\nnameserver 203.0.113.66".into()),
            dns: vec![],
            ..lease()
        };
        assert_eq!(resolv(&odd), "domain evilnameserver 203.0.113.66\n");
        let none = Lease {
            domain: None,
            dns: vec![],
            ..lease()
        };
        assert_eq!(resolv(&none), "");
    }

    #[test]
    fn resolv_conf_is_replaced_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("resolv.conf");
        std::fs::write(&path, "nameserver 203.0.113.1\n").unwrap();
        write_resolv(&path, "nameserver 192.0.2.1\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "nameserver 192.0.2.1\n"
        );
        assert!(!dir.path().join("resolv.conf.new").exists());
        write_resolv(&path, "nameserver 192.0.2.1\n").unwrap();
    }
}
