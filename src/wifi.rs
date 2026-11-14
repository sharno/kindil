//! Talking to `wpa_supplicant` over its control socket.
//!
//! Amazon's `wifid` starts `wpa_supplicant` and feeds it the networks it keeps;
//! once kindil runs Wi-Fi itself (src/net.rs), it feeds it its own
//! (src/networks.rs). It never saves the supplicant config, so Amazon's stored
//! Wi-Fi profiles are left untouched.
//!
//! Replies are plain text: either `key=value` lines (`STATUS`), or tab-separated
//! tables with a `/`-separated header (`SCAN_RESULTS`).

use crate::host::Signal;
use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Network name: 1..=32 bytes, no control characters.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Ssid(String);

impl TryFrom<String> for Ssid {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.len() {
            1..=32 if !value.chars().any(char::is_control) => Ok(Self(value)),
            _ => Err("network name must be 1–32 bytes"),
        }
    }
}

impl Ssid {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// WPA2 passphrase: 8..=63 printable ASCII characters. Never logged.
#[derive(Clone, PartialEq, Eq)]
pub struct Passphrase(String);

impl TryFrom<String> for Passphrase {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let printable = value.bytes().all(|b| (32..127).contains(&b));
        match value.len() {
            8..=63 if printable => Ok(Self(value)),
            _ => Err("password must be 8–63 printable ASCII characters"),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum Credentials {
    Open(Ssid),
    Wpa(Ssid, Passphrase),
}

impl Credentials {
    pub fn ssid(&self) -> &Ssid {
        match self {
            Self::Open(ssid) | Self::Wpa(ssid, _) => ssid,
        }
    }

    /// For saving it (src/networks.rs); never for logs.
    pub fn password(&self) -> Option<&str> {
        match self {
            Self::Open(_) => None,
            Self::Wpa(_, passphrase) => Some(&passphrase.0),
        }
    }

    /// An empty password means an open network.
    pub fn parse(ssid: String, password: String) -> Result<Self, &'static str> {
        let ssid = Ssid::try_from(ssid)?;
        if password.is_empty() {
            Ok(Self::Open(ssid))
        } else {
            Ok(Self::Wpa(ssid, Passphrase::try_from(password)?))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Security {
    Open,
    Protected,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Network {
    pub ssid: String,
    pub signal: Signal,
    pub security: Security,
}

impl Network {
    pub fn new(ssid: &str, signal: Signal, security: Security) -> Self {
        Self {
            ssid: ssid.into(),
            signal,
            security,
        }
    }
}

/// `STATUS`, decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    Associated { ssid: String, ip: Option<Ipv4Addr> },
    NotAssociated,
}

/// The control socket paths, in the order we try them.
fn socket_paths(interface: &str) -> [PathBuf; 3] {
    [
        PathBuf::from(format!("/var/run/wpa_supplicant/{interface}")),
        PathBuf::from(format!("/run/wpa_supplicant/{interface}")),
        PathBuf::from(format!("/tmp/wpa_supplicant/{interface}")),
    ]
}

/// A connected control socket.
#[derive(Debug)]
pub struct Wpa {
    socket: UnixDatagram,
    local: PathBuf,
}

pub fn open(interface: &str) -> anyhow::Result<Wpa> {
    socket_paths(interface)
        .iter()
        .filter(|path| path.exists())
        .map(|path| Wpa::open_path(path))
        .find_map(Result::ok)
        .ok_or_else(|| anyhow::anyhow!("no wpa_supplicant control socket for {interface}"))
}

impl Wpa {
    pub fn open_path(path: &Path) -> anyhow::Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        // wpa_supplicant replies to the sender's address, so bind a unique name.
        let local = std::env::temp_dir().join(format!(
            "kindil-wpa-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let socket = UnixDatagram::bind(&local)?;
        let wpa = Self { socket, local };
        wpa.socket.set_read_timeout(Some(Duration::from_secs(3)))?;
        wpa.socket.connect(path)?;
        Ok(wpa)
    }

    /// Send one command and return the reply.
    pub fn command(&self, command: &str) -> anyhow::Result<String> {
        self.socket.send(command.as_bytes())?;
        let mut buffer = [0u8; 8192];
        let n = self.socket.recv(&mut buffer)?;
        let reply = String::from_utf8_lossy(&buffer[..n]).into_owned();
        let verb = command.split_whitespace().next().unwrap_or(command);
        match reply.trim() {
            "UNKNOWN COMMAND" => anyhow::bail!("wpa_supplicant does not support {verb}"),
            r if r.starts_with("FAIL") => anyhow::bail!("wpa_supplicant rejected {verb}"),
            _ => Ok(reply),
        }
    }

    pub fn link(&self) -> anyhow::Result<Link> {
        Ok(parse_status(&self.command("STATUS")?))
    }

    /// RSSI of the current association. Old supplicants lack SIGNAL_POLL.
    pub fn signal(&self) -> Option<Signal> {
        parse_signal(&self.command("SIGNAL_POLL").ok()?)
    }

    /// Scan and wait for results, strongest first.
    pub fn scan(&self) -> anyhow::Result<Vec<Network>> {
        self.command("SCAN")?;
        std::thread::sleep(Duration::from_secs(3));
        Ok(parse_scan_results(&self.command("SCAN_RESULTS")?))
    }

    /// Add and select a network in memory only (no SAVE_CONFIG).
    pub fn join(&self, credentials: &Credentials) -> anyhow::Result<()> {
        let id = self.add(credentials, 0)?;
        self.command(&format!("SELECT_NETWORK {id}")).map(drop)
    }

    /// Replaces every network wpa_supplicant holds with `networks`, the
    /// first preferred most, and lets it pick among them from its own scans
    /// (Amazon's wifid runs it with ap_scan 2 and picks itself).
    pub fn load(&self, networks: &[Credentials]) -> anyhow::Result<()> {
        self.command("AP_SCAN 1")?;
        self.command("REMOVE_NETWORK all")?;
        let count = networks.len() as u32;
        networks
            .iter()
            .zip((1..=count).rev())
            .try_for_each(|(credentials, priority)| {
                let id = self.add(credentials, priority)?;
                self.command(&format!("ENABLE_NETWORK {id}")).map(drop)
            })
    }

    /// Adds a network, disabled, with settings that suit any WPA or WPA2
    /// access point; returns its id.
    fn add(&self, credentials: &Credentials, priority: u32) -> anyhow::Result<u32> {
        let id: u32 = self.command("ADD_NETWORK")?.trim().parse()?;
        let hex = |ssid: &Ssid| {
            ssid.0
                .bytes()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        };
        let mut settings = vec![
            format!("SET_NETWORK {id} ssid {}", hex(credentials.ssid())),
            // Also finds a network that hides its name.
            format!("SET_NETWORK {id} scan_ssid 1"),
            format!("SET_NETWORK {id} priority {priority}"),
        ];
        match credentials {
            Credentials::Open(_) => settings.push(format!("SET_NETWORK {id} key_mgmt NONE")),
            Credentials::Wpa(_, passphrase) => settings.extend([
                format!("SET_NETWORK {id} key_mgmt WPA-PSK"),
                format!("SET_NETWORK {id} proto RSN WPA"),
                format!("SET_NETWORK {id} pairwise CCMP TKIP"),
                format!("SET_NETWORK {id} group CCMP TKIP"),
                // Printable ASCII without quotes or backslashes needs no escaping;
                // the quoted form is wpa_supplicant's passphrase syntax.
                format!(
                    "SET_NETWORK {id} psk \"{}\"",
                    passphrase.0.replace('\\', "\\\\").replace('"', "\\\"")
                ),
            ]),
        }
        let result = settings
            .iter()
            .try_for_each(|command| self.command(command).map(drop));
        if let Err(error) = result {
            let _ = self.command(&format!("REMOVE_NETWORK {id}"));
            return Err(error);
        }
        Ok(id)
    }
}

impl Drop for Wpa {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.local);
    }
}

fn fields(reply: &str) -> BTreeMap<&str, &str> {
    reply
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.trim(), v.trim()))
        .collect()
}

pub fn parse_status(reply: &str) -> Link {
    let fields = fields(reply);
    match (fields.get("wpa_state"), fields.get("ssid")) {
        (Some(&"COMPLETED"), Some(ssid)) => Link::Associated {
            ssid: (*ssid).to_owned(),
            ip: fields.get("ip_address").and_then(|ip| ip.parse().ok()),
        },
        _ => Link::NotAssociated,
    }
}

pub fn parse_signal(reply: &str) -> Option<Signal> {
    fields(reply).get("RSSI")?.parse().ok().map(Signal)
}

/// Deduplicated by name (strongest access point wins), strongest first.
pub fn parse_scan_results(reply: &str) -> Vec<Network> {
    let strongest = reply
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut columns = line.splitn(5, '\t');
            let (_bssid, _freq) = (columns.next()?, columns.next()?);
            let signal = Signal(columns.next()?.trim().parse().ok()?);
            let flags = columns.next()?;
            let ssid = columns.next()?.trim();
            let security = if ["WPA", "WEP", "RSN"].iter().any(|f| flags.contains(f)) {
                Security::Protected
            } else {
                Security::Open
            };
            (!ssid.is_empty()).then(|| Network::new(ssid, signal, security))
        })
        .fold(BTreeMap::<String, Network>::new(), |mut best, network| {
            match best.get(&network.ssid) {
                Some(known) if known.signal >= network.signal => {}
                _ => {
                    best.insert(network.ssid.clone(), network);
                }
            }
            best
        });
    let mut networks: Vec<Network> = strongest.into_values().collect();
    networks.sort_by(|a, b| b.signal.cmp(&a.signal).then_with(|| a.ssid.cmp(&b.ssid)));
    networks
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCAN: &str = "bssid / frequency / signal level / flags / ssid\n\
          aa:bb:cc:dd:ee:ff\t2412\t-60\t[WPA2-PSK-CCMP][ESS]\tneighbour\n\
          aa:bb:cc:dd:ee:00\t5180\t-42\t[WPA2-PSK-CCMP][ESS]\thome\n\
          11:22:33:44:55:66\t2437\t-80\t[ESS]\topen network\n\
          aa:bb:cc:dd:ee:01\t2412\t-30\t[WPA2-PSK-CCMP][ESS]\thome\n";

    #[test]
    fn status_parses() {
        let status =
            "bssid=00:11:22:33:44:55\nssid=home\nwpa_state=COMPLETED\nip_address=192.168.1.42\n";
        assert_eq!(
            parse_status(status),
            Link::Associated {
                ssid: "home".into(),
                ip: Some(Ipv4Addr::new(192, 168, 1, 42))
            }
        );
        assert_eq!(parse_status("wpa_state=SCANNING\n"), Link::NotAssociated);
        assert_eq!(parse_signal("RSSI=-47\nLINKSPEED=65\n"), Some(Signal(-47)));
    }

    #[test]
    fn scan_results_are_deduplicated_and_sorted() {
        let networks = parse_scan_results(SCAN);
        let names: Vec<_> = networks.iter().map(|n| n.ssid.as_str()).collect();
        assert_eq!(names, ["home", "neighbour", "open network"]);
        assert_eq!(networks[0].signal, Signal(-30));
        assert_eq!(networks[2].security, Security::Open);
    }

    #[test]
    fn credentials_are_parsed() {
        assert!(Credentials::parse("".into(), "".into()).is_err());
        assert!(Credentials::parse("home".into(), "short".into()).is_err());
        assert!(matches!(
            Credentials::parse("cafe".into(), "".into()),
            Ok(Credentials::Open(_))
        ));
        assert!(matches!(
            Credentials::parse("home".into(), "correct horse".into()),
            Ok(Credentials::Wpa(..))
        ));
    }

    #[test]
    fn a_command_round_trips_over_a_socket() {
        let dir = tempfile::tempdir().unwrap();
        let server_path = dir.path().join("wlan0");
        let server = UnixDatagram::bind(&server_path).unwrap();
        let handle = std::thread::spawn(move || {
            let mut buffer = [0u8; 256];
            let (n, from) = server.recv_from(&mut buffer).unwrap();
            assert_eq!(&buffer[..n], b"PING");
            server
                .send_to(b"PONG\n", from.as_pathname().unwrap())
                .unwrap();
        });
        let wpa = Wpa::open_path(&server_path).unwrap();
        assert_eq!(wpa.command("PING").unwrap(), "PONG\n");
        handle.join().unwrap();
    }

    #[test]
    fn missing_socket_is_an_error() {
        assert!(open("definitely-not-an-interface").is_err());
    }
}
