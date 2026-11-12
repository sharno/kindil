//! Talking to `wpa_supplicant` over its control socket.
//!
//! kindil does not do the WPA2 handshake; `wpa_supplicant` does, and we drive it
//! through the same UNIX-datagram text protocol `wpa_cli` uses. That gives the
//! dashboard a real Wi-Fi status without linking any C.
//!
//! Replies are plain text: either `key=value` lines (`STATUS`), or tab-separated
//! tables with a `/`-separated header (`SCAN_RESULTS`).

use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The control socket paths, in the order we try them.
fn socket_paths(interface: &str) -> [PathBuf; 3] {
    [
        PathBuf::from(std::format!("/run/wpa_supplicant/{interface}")),
        PathBuf::from(std::format!("/var/run/wpa_supplicant/{interface}")),
        PathBuf::from(std::format!("/tmp/wpa_supplicant/{interface}")),
    ]
}

/// A connected control socket.
#[derive(Debug)]
pub struct Wpa {
    socket: UnixDatagram,
    local: PathBuf,
    /// The socket path, for error messages.
    path: PathBuf,
}

/// The parts of `STATUS` the dashboard cares about.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub ssid: Option<String>,
    pub state: String,
    pub ip: Option<String>,
    pub signal: Option<i32>,
}

impl Status {
    /// True when the interface is associated.
    pub fn connected(&self) -> bool {
        self.state == "COMPLETED"
    }

    /// A short label for the dashboard.
    pub fn label(&self) -> String {
        if self.connected() {
            self.ssid.clone().unwrap_or_else(|| "wi-fi".to_string())
        } else if self.state.is_empty() {
            "wi-fi off".to_string()
        } else {
            std::format!("wi-fi {}", self.state.to_ascii_lowercase())
        }
    }
}

/// One entry of `SCAN_RESULTS`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Network {
    pub ssid: String,
    pub signal: i32,
    pub secure: bool,
}

/// Open the control socket for an interface.
pub fn open(interface: &str) -> Result<Wpa, Error> {
    let mut last = None;
    for path in socket_paths(interface) {
        if !path.exists() {
            continue;
        }
        match Wpa::open_path(&path) {
            Ok(wpa) => return Ok(wpa),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or(Error::NoSocket {
        interface: interface.to_string(),
    }))
}

impl Wpa {
    /// Connect to a specific control socket.
    pub fn open_path(path: &Path) -> Result<Self, Error> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let local = std::env::temp_dir().join(format!(
            "kindil-wpa-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let socket = UnixDatagram::bind(&local).map_err(|e| Error::Socket {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        match Self::finish(socket, path, local.clone()) {
            Ok(wpa) => Ok(wpa),
            Err(error) => {
                let _ = std::fs::remove_file(local);
                Err(error)
            }
        }
    }

    /// Bind the client end to `client` before connecting.
    ///
    /// `wpa_supplicant` replies to the autobound address of an unbound socket,
    /// which is what the real client uses. Tests need a named endpoint so the
    /// fake server can answer, hence this.
    #[cfg(test)]
    pub fn bind_and_connect(client: &Path, server: &Path) -> Result<Self, Error> {
        let socket = UnixDatagram::bind(client).map_err(|e| Error::Socket {
            path: client.to_path_buf(),
            message: e.to_string(),
        })?;
        Self::finish(socket, server, client.to_path_buf())
    }

    fn finish(socket: UnixDatagram, path: &Path, local: PathBuf) -> Result<Self, Error> {
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .map_err(|e| Error::Socket {
                path: path.to_path_buf(),
                message: e.to_string(),
            })?;
        socket.connect(path).map_err(|e| Error::Socket {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        Ok(Wpa {
            socket,
            local,
            path: path.to_path_buf(),
        })
    }

    /// Send one command and return the reply.
    pub fn command(&self, command: &str) -> Result<String, Error> {
        self.socket
            .send(command.as_bytes())
            .map_err(|e| Error::Io {
                path: self.path.clone(),
                message: e.to_string(),
            })?;
        let mut buffer = [0u8; 8192];
        let n = self.socket.recv(&mut buffer).map_err(|e| Error::Io {
            path: self.path.clone(),
            message: e.to_string(),
        })?;
        let reply = String::from_utf8_lossy(&buffer[..n]).into_owned();
        if reply.trim().starts_with("FAIL") || reply.trim() == "UNKNOWN COMMAND" {
            return Err(Error::Io {
                path: self.path.clone(),
                message: format!(
                    "wpa_supplicant rejected {}",
                    command.split_whitespace().next().unwrap_or("command")
                ),
            });
        }
        Ok(reply)
    }

    /// Read the interface status.
    pub fn status(&self) -> Result<Status, Error> {
        let reply = self.command("STATUS")?;
        Ok(parse_status(&reply))
    }

    /// Read the last scan results, strongest first.
    pub fn scan_results(&self) -> Result<Vec<Network>, Error> {
        let reply = self.command("SCAN_RESULTS")?;
        Ok(parse_scan_results(&reply))
    }

    /// Configure one WPA2 network in memory. Credentials are not logged/saved.
    pub fn connect_network(&self, ssid: &str, passphrase: &str) -> Result<(), Error> {
        if ssid.is_empty()
            || ssid.len() > 32
            || !(8..=63).contains(&passphrase.len())
            || !passphrase.is_ascii()
            || passphrase.bytes().any(|b| b < 32 || b == 127)
        {
            return Err(Error::Io {
                path: self.path.clone(),
                message: "SSID must be 1–32 bytes; WPA2 passphrase 8–63 printable ASCII bytes"
                    .into(),
            });
        }
        let id = self
            .command("ADD_NETWORK")?
            .trim()
            .parse::<u32>()
            .map_err(|_| Error::Io {
                path: self.path.clone(),
                message: "invalid network ID".into(),
            })?;
        let hex = ssid
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let quoted = passphrase.replace('\\', "\\\\").replace('"', "\\\"");
        let result = (|| {
            self.command(&format!("SET_NETWORK {id} ssid {hex}"))?;
            self.command(&format!("SET_NETWORK {id} psk \"{quoted}\""))?;
            self.command(&format!("SELECT_NETWORK {id}"))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = self.command(&format!("REMOVE_NETWORK {id}"));
        }
        result
    }
}

impl Drop for Wpa {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.local);
    }
}

/// Parse the `key=value` body of `STATUS`.
pub fn parse_status(reply: &str) -> Status {
    let mut status = Status::default();
    for line in reply.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "ssid" => status.ssid = Some(value.trim().to_string()),
            "wpa_state" => status.state = value.trim().to_string(),
            "ip_address" => status.ip = Some(value.trim().to_string()),
            "signal_level" => status.signal = value.trim().parse().ok(),
            _ => {}
        }
    }
    status
}

/// Parse the table body of `SCAN_RESULTS`.
pub fn parse_scan_results(reply: &str) -> Vec<Network> {
    let mut networks: Vec<Network> = Vec::new();
    for line in reply.lines().skip(1) {
        let fields: Vec<&str> = line.splitn(5, '\t').collect();
        if fields.len() < 5 {
            continue;
        }
        let ssid = fields[4].trim();
        if ssid.is_empty() {
            continue;
        }
        let signal = fields[2].trim().parse().unwrap_or(-100);
        let flags = fields[3];
        let secure = flags.contains("WPA") || flags.contains("WEP") || flags.contains("RSN");
        match networks.iter_mut().find(|n| n.ssid == ssid) {
            Some(existing) => {
                if signal > existing.signal {
                    existing.signal = signal;
                    existing.secure = secure;
                }
            }
            None => networks.push(Network {
                ssid: ssid.to_string(),
                signal,
                secure,
            }),
        }
    }
    networks.sort_by(|a, b| b.signal.cmp(&a.signal).then_with(|| a.ssid.cmp(&b.ssid)));
    networks
}

/// Errors from the Wi-Fi control socket.
#[derive(Debug)]
pub enum Error {
    /// No control socket exists for the interface.
    NoSocket { interface: String },
    /// The socket could not be connected.
    Socket { path: PathBuf, message: String },
    /// Sending or receiving failed.
    Io { path: PathBuf, message: String },
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::NoSocket { interface } => {
                write!(f, "no wpa_supplicant control socket for {interface}")
            }
            Error::Socket { path, message } | Error::Io { path, message } => {
                write!(f, "{}: {message}", path.display())
            }
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixDatagram;

    const STATUS: &str = "bssid=00:11:22:33:44:55\nssid=home\nid=0\nwpa_state=COMPLETED\nip_address=192.168.1.42\nsignal_level=-47\n";

    const SCAN: &str = "bssid / frequency / signal level / flags / ssid\n\
          aa:bb:cc:dd:ee:ff\t2412\t-60\t[WPA2-PSK-CCMP][ESS]\tneighbour\n\
          aa:bb:cc:dd:ee:00\t5180\t-42\t[WPA2-PSK-CCMP][ESS]\thome\n\
          11:22:33:44:55:66\t2437\t-80\t[ESS]\topen network\n\
          aa:bb:cc:dd:ee:01\t2412\t-30\t[WPA2-PSK-CCMP][ESS]\thome\n";

    #[test]
    fn status_parses() {
        let status = parse_status(STATUS);
        assert_eq!(status.ssid.as_deref(), Some("home"));
        assert_eq!(status.state, "COMPLETED");
        assert_eq!(status.ip.as_deref(), Some("192.168.1.42"));
        assert_eq!(status.signal, Some(-47));
        assert!(status.connected());
        assert_eq!(status.label(), "home");
    }

    #[test]
    fn status_without_a_connection() {
        let status = parse_status("wpa_state=DISCONNECTED\n");
        assert!(!status.connected());
        assert_eq!(status.label(), "wi-fi disconnected");
        assert_eq!(Status::default().label(), "wi-fi off");
    }

    #[test]
    fn scan_results_are_deduplicated_and_sorted() {
        let networks = parse_scan_results(SCAN);
        assert_eq!(networks.len(), 3);
        assert_eq!(networks[0].ssid, "home");
        assert_eq!(networks[0].signal, -30);
        assert!(networks[0].secure);
        assert_eq!(networks[1].ssid, "neighbour");
        assert_eq!(networks[2].ssid, "open network");
        assert!(!networks[2].secure);
    }

    #[test]
    fn a_command_round_trips_over_a_socket() {
        let dir = std::env::temp_dir().join(std::format!("kindil-wpa-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let server_path = dir.join("wlan0");
        let client_path = dir.join("client");

        let server = UnixDatagram::bind(&server_path).unwrap();
        let reply_to = client_path.clone();
        let handle = std::thread::spawn(move || {
            let mut buffer = [0u8; 256];
            let (n, _) = server.recv_from(&mut buffer).unwrap();
            assert_eq!(&buffer[..n], b"PING");
            server.send_to(b"PONG\n", &reply_to).unwrap();
        });

        let wpa = Wpa::bind_and_connect(&client_path, &server_path).unwrap();
        assert_eq!(wpa.command("PING").unwrap(), "PONG\n");
        handle.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_socket_is_an_error() {
        let err = open("definitely-not-an-interface").unwrap_err();
        assert!(matches!(err, Error::NoSocket { .. }));
    }
}
