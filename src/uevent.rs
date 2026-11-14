//! Kernel uevents: how the PW1 reports its power button and USB cable.
//!
//! powerd reads the button from the same netlink broadcast, but while Amazon's
//! UI is stopped it never acts on a press. Any process may listen, so kindil
//! hears the press too and runs sleep itself.
use std::collections::BTreeMap;

/// What happened to the device. The kernel's set, plus whatever a future
/// kernel adds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Add,
    Remove,
    Change,
    Move,
    Online,
    Offline,
    Other(String),
}

impl Action {
    fn parse(text: &str) -> Self {
        match text {
            "add" => Action::Add,
            "remove" => Action::Remove,
            "change" => Action::Change,
            "move" => Action::Move,
            "online" => Action::Online,
            "offline" => Action::Offline,
            other => Action::Other(other.to_owned()),
        }
    }

    fn as_str(&self) -> &str {
        match self {
            Action::Add => "add",
            Action::Remove => "remove",
            Action::Change => "change",
            Action::Move => "move",
            Action::Online => "online",
            Action::Offline => "offline",
            Action::Other(other) => other,
        }
    }
}

/// One uevent: `ACTION@DEVPATH` followed by `KEY=VALUE` lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Uevent {
    pub action: Action,
    pub devpath: String,
    pub vars: BTreeMap<String, String>,
}

impl Uevent {
    /// A netlink datagram: NUL-separated fields, the header first.
    pub fn parse(datagram: &[u8]) -> Option<Self> {
        let mut fields = datagram
            .split(|b| *b == 0)
            .filter(|f| !f.is_empty())
            .map(String::from_utf8_lossy);
        let header = fields.next()?;
        let (action, devpath) = header.split_once('@')?;
        let vars = fields
            .filter_map(|f| f.split_once('=').map(|(k, v)| (k.to_owned(), v.to_owned())))
            .collect();
        Some(Self {
            action: Action::parse(action),
            devpath: devpath.to_owned(),
            vars,
        })
    }
}

impl std::fmt::Display for Uevent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}", self.action.as_str(), self.devpath)?;
        self.vars
            .iter()
            .try_for_each(|(k, v)| write!(f, " {k}={v}"))
    }
}

#[cfg(target_os = "linux")]
pub use socket::Listener;

/// Netlink is Linux-only; elsewhere there is nothing to listen to.
#[cfg(not(target_os = "linux"))]
pub enum Listener {}

#[cfg(not(target_os = "linux"))]
impl Listener {
    pub fn open() -> std::io::Result<Self> {
        Err(std::io::ErrorKind::Unsupported.into())
    }
    pub fn next(&self) -> std::io::Result<Uevent> {
        match *self {}
    }
}

#[cfg(target_os = "linux")]
mod socket {
    use super::Uevent;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    /// The kernel's uevent broadcast group.
    const KERNEL_GROUP: u32 = 1;
    const RECEIVE_BUFFER: libc::c_int = 256 * 1024;

    pub struct Listener(OwnedFd);

    impl Listener {
        pub fn open() -> io::Result<Self> {
            // SAFETY: plain socket(2); the result is checked before wrapping.
            let fd = unsafe {
                libc::socket(
                    libc::AF_NETLINK,
                    libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
                    libc::NETLINK_KOBJECT_UEVENT,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: fd is a fresh descriptor we own.
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };
            // SAFETY: zeroed sockaddr_nl is valid; nl_pid 0 lets the kernel assign one.
            let mut addr: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
            addr.nl_family = libc::AF_NETLINK as libc::sa_family_t;
            addr.nl_groups = KERNEL_GROUP;
            // SAFETY: addr is a valid sockaddr_nl for the call's duration.
            let rc = unsafe {
                libc::bind(
                    fd.as_raw_fd(),
                    (&raw const addr).cast(),
                    std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
                )
            };
            if rc < 0 {
                return Err(io::Error::last_os_error());
            }
            // Room for a burst (a resume brings dozens); best effort, as the
            // kernel caps it at rmem_max.
            let size: libc::c_int = RECEIVE_BUFFER;
            // SAFETY: size outlives the call and its length is passed.
            unsafe {
                libc::setsockopt(
                    fd.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_RCVBUF,
                    (&raw const size).cast(),
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );
            }
            Ok(Self(fd))
        }

        /// Blocks until the next uevent the kernel broadcasts.
        pub fn next(&self) -> io::Result<Uevent> {
            let mut buf = [0u8; 4096];
            loop {
                // SAFETY: buf is valid for its length; fd is open.
                let n = unsafe {
                    libc::recv(self.0.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0)
                };
                match n {
                    n if n > 0 => {
                        if let Some(event) = Uevent::parse(&buf[..n as usize]) {
                            return Ok(event);
                        }
                    }
                    0 => return Err(io::ErrorKind::UnexpectedEof.into()),
                    _ => {
                        let error = io::Error::last_os_error();
                        match error.raw_os_error() {
                            Some(libc::EINTR) => {}
                            // The buffer overflowed and some events were
                            // dropped; the socket itself still works.
                            Some(libc::ENOBUFS) => {
                                eprintln!("kindil: uevents overflowed; some were missed")
                            }
                            _ => return Err(error),
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_kernel_datagram() {
        let event =
            Uevent::parse(b"change@/devices/platform/whitney-button\0ACTION=change\0KEY=pressed\0")
                .unwrap();
        assert_eq!(event.action, Action::Change);
        assert_eq!(event.devpath, "/devices/platform/whitney-button");
        assert_eq!(event.vars.get("KEY").map(String::as_str), Some("pressed"));
        assert_eq!(
            event.to_string(),
            "change@/devices/platform/whitney-button ACTION=change KEY=pressed"
        );
    }

    #[test]
    fn rejects_datagrams_without_a_header() {
        assert_eq!(Uevent::parse(b"libudev\0junk"), None);
        assert_eq!(Uevent::parse(b""), None);
    }
}
