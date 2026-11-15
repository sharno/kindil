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
    fn deconfig_clears_the_address_and_other_events_change_nothing() {
        let plan = plan("deconfig", &lease());
        assert_eq!(plan.commands, [words("/sbin/ifconfig wlan0 0.0.0.0")]);
        assert_eq!(plan.resolv, None);
        for event in ["leasefail", "nak", ""] {
            assert_eq!(super::plan(event, &lease()).commands.len(), 0, "{event}");
        }
    }
}
