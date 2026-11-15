//! Linux wireless extensions (v22, `<linux/wireless.h>` of 2.6.31) as the
//! PW1's AR6003 driver (ath6kl staging, `wireless_ext.c`) answers them: the
//! ioctls kindil's own Wi-Fi (src/station.rs) joins with, and the decoding of
//! what they return: the scan's event stream, the beacons' RSN and WPA
//! elements, and `/proc/net/wireless`.
//!
//! The structs are the C ones; the tests and a compile-time check pin their
//! sizes for 32-bit ARM, where `iwreq` is 32 bytes. The scan stream is decoded
//! from bytes, as the 32-bit little-endian kernel lays it out, so the same
//! decoding is tested on any host.
use crate::pmk::Pmk;
use std::io;
use std::net::Ipv4Addr;
use std::os::raw::c_void;

const SIOCGIFFLAGS: u32 = 0x8913;
const SIOCSIFFLAGS: u32 = 0x8914;
const SIOCGIFADDR: u32 = 0x8915;
const IFF_UP: i16 = 0x1;

pub const SIOCGIWNAME: u16 = 0x8B01;
pub const SIOCGIWFREQ: u16 = 0x8B05;
pub const SIOCGIWMODE: u16 = 0x8B07;
pub const SIOCGIWAP: u16 = 0x8B15;
const SIOCSIWMLME: u16 = 0x8B16;
const SIOCSIWSCAN: u16 = 0x8B18;
pub const SIOCGIWSCAN: u16 = 0x8B19;
const SIOCSIWESSID: u16 = 0x8B1A;
pub const SIOCGIWESSID: u16 = 0x8B1B;
pub const SIOCGIWRATE: u16 = 0x8B21;
pub const SIOCGIWENCODE: u16 = 0x8B2B;
const SIOCSIWAUTH: u16 = 0x8B32;
const SIOCSIWENCODEEXT: u16 = 0x8B34;
pub const IWEVQUAL: u16 = 0x8C01;
pub const IWEVCUSTOM: u16 = 0x8C02;
pub const IWEVGENIE: u16 = 0x8C05;

pub const IW_AUTH_WPA_VERSION: u16 = 0;
pub const IW_AUTH_CIPHER_PAIRWISE: u16 = 1;
pub const IW_AUTH_CIPHER_GROUP: u16 = 2;
pub const IW_AUTH_KEY_MGMT: u16 = 3;
pub const IW_AUTH_DROP_UNENCRYPTED: u16 = 5;
pub const IW_AUTH_80211_AUTH_ALG: u16 = 6;
pub const IW_AUTH_WPA_ENABLED: u16 = 7;
pub const IW_AUTH_PRIVACY_INVOKED: u16 = 10;

pub const IW_AUTH_WPA_VERSION_WPA2: i32 = 0x4;
pub const IW_AUTH_CIPHER_TKIP: i32 = 0x4;
pub const IW_AUTH_CIPHER_CCMP: i32 = 0x8;
pub const IW_AUTH_KEY_MGMT_PSK: i32 = 2;
pub const IW_AUTH_ALG_OPEN_SYSTEM: i32 = 1;

const IW_ENCODE_ALG_PMK: u16 = 4;
const IW_ENCODE_DISABLED: u16 = 0x8000;
const IW_MLME_DISASSOC: u16 = 1;
/// WLAN_REASON_DEAUTH_LEAVING
const REASON_LEAVING: u16 = 3;
const ARPHRD_ETHER: u16 = 1;
/// The most an `iw_point` can describe.
const SCAN_BUFFER_MAX: usize = 0xFFFF;

/// The driver as kindil's own Wi-Fi uses it; [`Socket`] on the Kindle, a
/// fake in the tests. Errors are the driver's, unchanged, except the ones
/// that only say "not connected".
pub trait Driver {
    /// `ifconfig wlan0 up`.
    fn set_up(&self) -> io::Result<()>;
    fn set_auth(&self, index: u16, value: i32) -> io::Result<()>;
    /// The PMK for the firmware's own 4-way handshake (IW_ENCODE_ALG_PMK).
    fn set_pmk(&self, pmk: &Pmk) -> io::Result<()>;
    /// `None` is "essid off": disconnects and resets the driver's profile.
    fn set_essid(&self, ssid: Option<&[u8]>) -> io::Result<()>;
    /// The access point, `None` while not associated.
    fn bssid(&self) -> io::Result<Option<[u8; 6]>>;
    fn essid(&self) -> io::Result<Vec<u8>>;
    /// The driver disassociates only from the current BSSID.
    fn disassociate(&self, bssid: [u8; 6]) -> io::Result<()>;
    /// Starts a scan; the driver ignores one asked for during another.
    fn trigger_scan(&self) -> io::Result<()>;
    /// The driver's table of access points, as a SIOCGIWSCAN event stream.
    /// EAGAIN (a scan not done yet) comes back as the error, unretried:
    /// the caller retries within its own bound, without holding locks.
    fn scan_results(&self) -> io::Result<Vec<u8>>;
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Sockaddr {
    family: u16,
    data: [u8; 14],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct IwPoint {
    pointer: *mut c_void,
    length: u16,
    flags: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct IwParam {
    value: i32,
    fixed: u8,
    disabled: u8,
    flags: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
union IwreqData {
    name: [u8; 16],
    point: IwPoint,
    param: IwParam,
    addr: Sockaddr,
}

#[repr(C)]
struct Iwreq {
    ifr_name: [u8; 16],
    u: IwreqData,
}

#[repr(C)]
struct IwEncodeExt {
    ext_flags: u32,
    tx_seq: [u8; 8],
    rx_seq: [u8; 8],
    addr: Sockaddr,
    alg: u16,
    key_len: u16,
    // u8 key[0] follows
}

/// iw_encode_ext with its key right behind it, as the driver reads it:
/// key length = iw_point.length - sizeof(struct iw_encode_ext).
#[repr(C)]
struct EncodeExtPmk {
    ext: IwEncodeExt,
    key: [u8; 32],
}

#[repr(C)]
struct IwMlme {
    cmd: u16,
    reason_code: u16,
    addr: Sockaddr,
}

#[repr(C)]
struct IfreqFlags {
    name: [u8; 16],
    flags: i16,
    pad: [u8; 22],
}

#[repr(C)]
struct IfreqAddr {
    name: [u8; 16],
    family: u16,
    port: u16,
    addr: [u8; 4],
    pad: [u8; 16],
}

/// An all-zero value of a plain-old-data struct above.
fn zeroed<T>() -> T {
    // SAFETY: only used for the repr(C) integer, array and raw-pointer
    // structs of this module, for which all-zero bytes are valid.
    unsafe { std::mem::zeroed() }
}

/// A datagram socket to ask the driver of one interface.
pub struct Socket {
    fd: libc::c_int,
    ifname: [u8; 16],
}

impl Socket {
    pub fn open(ifname: &str) -> io::Result<Self> {
        let mut name = [0u8; 16];
        if ifname.len() >= name.len() {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        name[..ifname.len()].copy_from_slice(ifname.as_bytes());
        // SAFETY: plain syscall.
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd, ifname: name })
    }

    fn request(&self) -> Iwreq {
        let mut iwr: Iwreq = zeroed();
        iwr.ifr_name = self.ifname;
        iwr
    }

    /// Any ioctl on a struct that starts with the interface name.
    fn ioctl<T>(&self, request: u32, arg: &mut T) -> io::Result<()> {
        // SAFETY: `arg` is one of this module's C structs, as large as the
        // kernel copies for `request`; pointers inside it outlive the call.
        let r = unsafe { libc::ioctl(self.fd, request as _, arg as *mut T as *mut c_void) };
        if r < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn point(
        &self,
        request: u16,
        pointer: *mut c_void,
        length: u16,
        flags: u16,
    ) -> io::Result<u16> {
        let mut iwr = self.request();
        iwr.u.point = IwPoint {
            pointer,
            length,
            flags,
        };
        self.ioctl(u32::from(request), &mut iwr)?;
        // SAFETY: a point request comes back as one.
        Ok(unsafe { iwr.u.point.length })
    }

    /// The interface's IPv4 address, if it has one.
    pub fn ipv4(&self) -> Option<Ipv4Addr> {
        let mut ifr: IfreqAddr = zeroed();
        ifr.name = self.ifname;
        ifr.family = libc::AF_INET as u16;
        self.ioctl(SIOCGIFADDR, &mut ifr).ok()?;
        let ip = Ipv4Addr::from(ifr.addr);
        (!ip.is_unspecified()).then_some(ip)
    }
}

impl Driver for Socket {
    fn set_up(&self) -> io::Result<()> {
        let mut ifr: IfreqFlags = zeroed();
        ifr.name = self.ifname;
        self.ioctl(SIOCGIFFLAGS, &mut ifr)?;
        if ifr.flags & IFF_UP != 0 {
            return Ok(());
        }
        ifr.flags |= IFF_UP;
        self.ioctl(SIOCSIFFLAGS, &mut ifr)
    }

    fn set_auth(&self, index: u16, value: i32) -> io::Result<()> {
        let mut iwr = self.request();
        iwr.u.param = IwParam {
            value,
            fixed: 0,
            disabled: 0,
            flags: index,
        };
        self.ioctl(u32::from(SIOCSIWAUTH), &mut iwr)
    }

    fn set_pmk(&self, pmk: &Pmk) -> io::Result<()> {
        let mut ext: EncodeExtPmk = zeroed();
        ext.ext.alg = IW_ENCODE_ALG_PMK;
        ext.ext.key_len = 32;
        ext.key = *pmk.bytes();
        // Key index 0 (current), encoding enabled.
        let result = self
            .point(
                SIOCSIWENCODEEXT,
                &mut ext as *mut EncodeExtPmk as *mut c_void,
                std::mem::size_of::<EncodeExtPmk>() as u16,
                0,
            )
            .map(drop);
        crate::pmk::wipe(&mut ext.key);
        result
    }

    fn set_essid(&self, ssid: Option<&[u8]>) -> io::Result<()> {
        let mut buf = [0u8; 33];
        let (length, flags) = match ssid {
            Some(ssid) if (1..=32).contains(&ssid.len()) => {
                buf[..ssid.len()].copy_from_slice(ssid);
                // WE >= 21: the length excludes the NUL; the driver adds one.
                (ssid.len() as u16, 1)
            }
            Some(_) => return Err(io::Error::from(io::ErrorKind::InvalidInput)),
            None => (0, 0),
        };
        self.point(SIOCSIWESSID, buf.as_mut_ptr() as *mut c_void, length, flags)
            .map(drop)
    }

    fn bssid(&self) -> io::Result<Option<[u8; 6]>> {
        let mut iwr = self.request();
        match self.ioctl(u32::from(SIOCGIWAP), &mut iwr) {
            // ar6000 answers EINVAL while not connected.
            Err(e) if e.raw_os_error() == Some(libc::EINVAL) => Ok(None),
            Err(e) => Err(e),
            Ok(()) => {
                // SAFETY: SIOCGIWAP fills ap_addr.
                let addr = unsafe { iwr.u.addr };
                let mut mac = [0u8; 6];
                mac.copy_from_slice(&addr.data[..6]);
                Ok(is_bssid(&mac).then_some(mac))
            }
        }
    }

    fn essid(&self) -> io::Result<Vec<u8>> {
        let mut buf = [0u8; 33];
        let length = self.point(SIOCGIWESSID, buf.as_mut_ptr() as *mut c_void, 33, 0)?;
        let mut ssid = buf[..usize::from(length).min(32)].to_vec();
        // Older drivers count a NUL.
        while ssid.last() == Some(&0) {
            ssid.pop();
        }
        Ok(ssid)
    }

    fn disassociate(&self, bssid: [u8; 6]) -> io::Result<()> {
        let mut mlme: IwMlme = zeroed();
        mlme.cmd = IW_MLME_DISASSOC;
        mlme.reason_code = REASON_LEAVING;
        mlme.addr.family = ARPHRD_ETHER;
        mlme.addr.data[..6].copy_from_slice(&bssid);
        self.point(
            SIOCSIWMLME,
            &mut mlme as *mut IwMlme as *mut c_void,
            std::mem::size_of::<IwMlme>() as u16,
            0,
        )
        .map(drop)
    }

    fn trigger_scan(&self) -> io::Result<()> {
        match self.point(SIOCSIWSCAN, std::ptr::null_mut(), 0, 0) {
            // Busy: the table it has is good enough.
            Err(e) if matches!(e.raw_os_error(), Some(libc::EBUSY | libc::EAGAIN)) => Ok(()),
            other => other.map(drop),
        }
    }

    fn scan_results(&self) -> io::Result<Vec<u8>> {
        read_growing(|buf| {
            self.point(
                SIOCGIWSCAN,
                buf.as_mut_ptr() as *mut c_void,
                buf.len() as u16,
                0,
            )
        })
    }
}

/// Reads into a buffer that doubles from 4 KiB while `read` answers E2BIG,
/// up to what an `iw_point` can describe; any other error (EAGAIN
/// included) is returned as is. `read` fills the buffer and returns the
/// length it wrote.
pub fn read_growing(mut read: impl FnMut(&mut [u8]) -> io::Result<u16>) -> io::Result<Vec<u8>> {
    let mut size = 4096;
    loop {
        let mut buf = vec![0u8; size];
        match read(&mut buf) {
            Ok(length) => {
                buf.truncate(usize::from(length).min(size));
                return Ok(buf);
            }
            Err(e) if e.raw_os_error() == Some(libc::E2BIG) && size < SCAN_BUFFER_MAX => {
                size = (size * 2).min(SCAN_BUFFER_MAX);
            }
            Err(e) => return Err(e),
        }
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        // SAFETY: our own descriptor.
        unsafe { libc::close(self.fd) };
    }
}

/// A real BSSID: not zero, broadcast, or the 44:44:44:44:44:44 some
/// drivers report while unassociated.
pub fn is_bssid(mac: &[u8; 6]) -> bool {
    !(mac.iter().all(|&b| b == 0)
        || mac.iter().all(|&b| b == 0xff)
        || mac.iter().all(|&b| b == 0x44))
}

pub fn mac_string(mac: &[u8; 6]) -> String {
    mac.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// A cipher suite of an RSN or WPA element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cipher {
    Wep40,
    Tkip,
    Ccmp,
    Wep104,
    Other(u8),
}

impl Cipher {
    fn from_type(t: u8) -> Self {
        match t {
            1 => Self::Wep40,
            2 => Self::Tkip,
            4 => Self::Ccmp,
            5 => Self::Wep104,
            other => Self::Other(other),
        }
    }
}

/// Authentication and key management suites that matter here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Akm {
    Ieee8021x,
    Psk,
    PskSha256,
    Sae,
    Other(u8),
}

impl Akm {
    fn from_type(t: u8) -> Self {
        match t {
            1 => Self::Ieee8021x,
            2 => Self::Psk,
            6 => Self::PskSha256,
            8 => Self::Sae,
            other => Self::Other(other),
        }
    }
}

/// An RSN (WPA2) or WPA (version 1) element, decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suites {
    pub group: Cipher,
    pub pairwise: Vec<Cipher>,
    pub akm: Vec<Akm>,
}

const RSN_OUI: [u8; 3] = [0x00, 0x0F, 0xAC];
const WPA_OUI: [u8; 3] = [0x00, 0x50, 0xF2];
const ELEMENT_RSN: u8 = 48;
const ELEMENT_VENDOR: u8 = 221;

/// The suites of one element body (after the id and length): version,
/// group, pairwise list, AKM list; the ones left out take their defaults.
fn suites(body: &[u8], oui: [u8; 3], default: Cipher) -> Option<Suites> {
    let mut s = Suites {
        group: default,
        pairwise: vec![default],
        akm: vec![Akm::Ieee8021x],
    };
    let version = u16::from_le_bytes([*body.first()?, *body.get(1)?]);
    if version != 1 {
        return None;
    }
    let mut rest = &body[2..];
    let suite = |bytes: &[u8]| (bytes[..3] == oui).then_some(bytes[3]);
    if rest.len() < 4 {
        return Some(s);
    }
    s.group = suite(rest).map_or(Cipher::Other(0), Cipher::from_type);
    rest = &rest[4..];
    let list = |rest: &mut &[u8]| -> Option<Option<Vec<u8>>> {
        if rest.len() < 2 {
            return Some(None);
        }
        let count = usize::from(u16::from_le_bytes([rest[0], rest[1]]));
        let bytes = rest.get(2..2 + 4 * count)?;
        *rest = &rest[2 + 4 * count..];
        Some(Some(
            bytes
                .chunks(4)
                .map(|c| suite(c).unwrap_or(0))
                .collect::<Vec<_>>(),
        ))
    };
    if let Some(types) = list(&mut rest)? {
        s.pairwise = types.into_iter().map(Cipher::from_type).collect();
    }
    if let Some(types) = list(&mut rest)? {
        s.akm = types.into_iter().map(Akm::from_type).collect();
    }
    Some(s)
}

/// The RSN and WPA elements among `ies` (a run of id, length, body).
pub fn security_elements(ies: &[u8]) -> (Option<Suites>, Option<Suites>) {
    let (mut rsn, mut wpa) = (None, None);
    let mut rest = ies;
    while rest.len() >= 2 {
        let (id, len) = (rest[0], usize::from(rest[1]));
        let Some(body) = rest.get(2..2 + len) else {
            break;
        };
        match id {
            ELEMENT_RSN if rsn.is_none() => rsn = suites(body, RSN_OUI, Cipher::Ccmp),
            ELEMENT_VENDOR
                if wpa.is_none() && body.len() >= 4 && body[..4] == [0, 0x50, 0xF2, 1] =>
            {
                wpa = suites(&body[4..], WPA_OUI, Cipher::Tkip)
            }
            _ => {}
        }
        rest = &rest[2 + len..];
    }
    (rsn, wpa)
}

/// One access point of a scan.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bss {
    pub bssid: [u8; 6],
    pub ssid: Vec<u8>,
    pub mhz: Option<u32>,
    /// dBm.
    pub level: Option<i32>,
    /// The privacy bit: encryption of some kind.
    pub privacy: bool,
    pub rsn: Option<Suites>,
    pub wpa: Option<Suites>,
}

impl Bss {
    /// The channel of the frequency, 2.4 or 5 GHz.
    pub fn channel(&self) -> Option<u32> {
        match self.mhz? {
            2484 => Some(14),
            mhz @ 2412..=2472 => Some((mhz - 2407) / 5),
            mhz @ 5000..=5900 => Some((mhz - 5000) / 5),
            _ => None,
        }
    }
}

/// A channel number or a frequency, as `m * 10^e`, in MHz.
fn mhz(m: i32, e: i16) -> Option<u32> {
    let mut value = f64::from(m) * 10f64.powi(i32::from(e));
    if value <= 0.0 {
        return None;
    }
    if value < 1000.0 {
        // A channel.
        let channel = value as u32;
        return match channel {
            14 => Some(2484),
            1..=13 => Some(2407 + 5 * channel),
            32..=196 => Some(5000 + 5 * channel),
            _ => None,
        };
    }
    value /= 1e6;
    Some(value.round() as u32)
}

/// An 8-bit level as the driver gives it: ar6000 gives 161 + SNR, which
/// read as a signed byte is dBm (noise at -95 dBm). Zero is no reading.
pub fn level_dbm(level: i32) -> Option<i32> {
    match level {
        0 => None,
        l if l < 0 => Some(l),
        l if l > 63 => Some(l - 256),
        // A plain 0..63 quality scale: not dBm.
        _ => None,
    }
}

fn hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

/// The access points of a SIOCGIWSCAN event stream, in its order: each
/// starts with a SIOCGIWAP event. 32-bit kernel layout: every event is
/// `u16 len, u16 cmd` and its payload; a pointer event's payload is
/// `u16 length, u16 flags` and the data, unpadded.
pub fn parse_scan(stream: &[u8]) -> Vec<Bss> {
    let u16_at = |b: &[u8], at: usize| u16::from_le_bytes([b[at], b[at + 1]]);
    let mut found: Vec<Bss> = Vec::new();
    let mut pos = 0;
    while pos + 4 <= stream.len() {
        let len = usize::from(u16_at(stream, pos));
        let cmd = u16_at(stream, pos + 2);
        if len < 4 || pos + len > stream.len() {
            break;
        }
        let payload = &stream[pos + 4..pos + len];
        pos += len;
        // A pointer event's data, cut to the length it claims.
        let data = || -> Option<(&[u8], u16)> {
            if payload.len() < 4 {
                return None;
            }
            let length = usize::from(u16_at(payload, 0));
            Some((payload.get(4..4 + length)?, u16_at(payload, 2)))
        };
        if cmd == SIOCGIWAP {
            if payload.len() >= 8 {
                let mut bssid = [0u8; 6];
                bssid.copy_from_slice(&payload[2..8]);
                found.push(Bss {
                    bssid,
                    ..Bss::default()
                });
            }
            continue;
        }
        let Some(bss) = found.last_mut() else {
            continue;
        };
        match cmd {
            SIOCGIWESSID => {
                if let Some((ssid, _)) = data() {
                    let mut ssid = ssid.to_vec();
                    while ssid.last() == Some(&0) {
                        ssid.pop();
                    }
                    bss.ssid = ssid;
                }
            }
            SIOCGIWFREQ if payload.len() >= 6 => {
                let m = i32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
                let e = i16::from_le_bytes([payload[4], payload[5]]);
                bss.mhz = mhz(m, e).or(bss.mhz);
            }
            IWEVQUAL if payload.len() >= 4 => {
                bss.level = level_dbm(i32::from(payload[1]));
            }
            SIOCGIWENCODE => {
                if let Some((_, flags)) = data() {
                    bss.privacy = flags & IW_ENCODE_DISABLED == 0;
                }
            }
            IWEVGENIE => {
                if let Some((ies, _)) = data() {
                    let (rsn, wpa) = security_elements(ies);
                    bss.rsn = bss.rsn.take().or(rsn);
                    bss.wpa = bss.wpa.take().or(wpa);
                }
            }
            // Drivers before WE 18 hand the elements over as hex text.
            IWEVCUSTOM => {
                if let Some((text, _)) = data() {
                    let text = String::from_utf8_lossy(text);
                    for prefix in ["wpa_ie=", "rsn_ie="] {
                        if let Some(ies) = text.strip_prefix(prefix).and_then(hex) {
                            let (rsn, wpa) = security_elements(&ies);
                            bss.rsn = bss.rsn.take().or(rsn);
                            bss.wpa = bss.wpa.take().or(wpa);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    found
}

/// The link of `interface` in `/proc/net/wireless`: its level in dBm.
pub fn parse_proc_level(text: &str, interface: &str) -> Option<i32> {
    let line = text
        .lines()
        .find(|l| l.trim_start().starts_with(&format!("{interface}:")))?;
    let field = line.split_whitespace().nth(3)?;
    level_dbm(field.trim_end_matches('.').parse().ok()?)
}

#[cfg(test)]
pub mod stream {
    //! Builds SIOCGIWSCAN event streams as the 32-bit ARM kernel writes
    //! them (iwe_stream_add_event/_point/_value of 2.6.31).
    use super::*;

    #[derive(Default)]
    pub struct Stream(pub Vec<u8>);

    impl Stream {
        fn event(&mut self, cmd: u16, payload: &[u8]) -> &mut Self {
            let len = 4 + payload.len() as u16;
            self.0.extend_from_slice(&len.to_le_bytes());
            self.0.extend_from_slice(&cmd.to_le_bytes());
            self.0.extend_from_slice(payload);
            self
        }

        fn point(&mut self, cmd: u16, flags: u16, data: &[u8]) -> &mut Self {
            let mut payload = (data.len() as u16).to_le_bytes().to_vec();
            payload.extend_from_slice(&flags.to_le_bytes());
            payload.extend_from_slice(data);
            self.event(cmd, &payload)
        }

        /// One access point as ar6000_scan_node writes it.
        pub fn ap(
            &mut self,
            bssid: [u8; 6],
            ssid: &[u8],
            mhz: i32,
            snr: u8,
            privacy: bool,
            ies: &[&[u8]],
        ) -> &mut Self {
            let mut addr = ARPHRD_ETHER.to_le_bytes().to_vec();
            addr.extend_from_slice(&bssid);
            addr.resize(16, 0);
            self.event(SIOCGIWAP, &addr);
            self.point(SIOCGIWESSID, 1, ssid);
            // IW_MODE_MASTER
            self.event(SIOCGIWMODE, &3u32.to_le_bytes());
            let mut freq = (mhz * 100_000).to_le_bytes().to_vec();
            freq.extend_from_slice(&1i16.to_le_bytes());
            freq.extend_from_slice(&[0, 0]);
            self.event(SIOCGIWFREQ, &freq);
            let qual = snr.min(94);
            self.event(IWEVQUAL, &[qual, 161 + qual, 161, 7]);
            let flags = if privacy {
                0x0800 // IW_ENCODE_ENABLED (0) | IW_ENCODE_NOKEY
            } else {
                IW_ENCODE_DISABLED
            };
            self.point(SIOCGIWENCODE, flags, &[]);
            // Rates: one header, then a value per rate, iw_param each.
            let mut rates = Vec::new();
            for rate in [1_000_000i32, 54_000_000] {
                rates.extend_from_slice(&rate.to_le_bytes());
                rates.extend_from_slice(&[0, 0, 0, 0]);
            }
            self.event(SIOCGIWRATE, &rates);
            for ie in ies {
                self.point(IWEVGENIE, 0, ie);
            }
            let mut name = b"IEEE 802.11ng".to_vec();
            name.resize(16, 0);
            self.event(SIOCGIWNAME, &name);
            self.point(IWEVCUSTOM, 0, b"bcn_int=100")
        }
    }

    pub fn rsn(group: u8, pairwise: &[u8], akm: &[u8]) -> Vec<u8> {
        let mut body = vec![1, 0, 0x00, 0x0F, 0xAC, group];
        body.extend_from_slice(&(pairwise.len() as u16).to_le_bytes());
        for p in pairwise {
            body.extend_from_slice(&[0x00, 0x0F, 0xAC, *p]);
        }
        body.extend_from_slice(&(akm.len() as u16).to_le_bytes());
        for a in akm {
            body.extend_from_slice(&[0x00, 0x0F, 0xAC, *a]);
        }
        body.extend_from_slice(&[0x0c, 0x00]); // RSN capabilities
        let mut ie = vec![ELEMENT_RSN, body.len() as u8];
        ie.extend(body);
        ie
    }

    pub fn wpa(group: u8, pairwise: &[u8], akm: &[u8]) -> Vec<u8> {
        let mut body = vec![0x00, 0x50, 0xF2, 1, 1, 0, 0x00, 0x50, 0xF2, group];
        body.extend_from_slice(&(pairwise.len() as u16).to_le_bytes());
        for p in pairwise {
            body.extend_from_slice(&[0x00, 0x50, 0xF2, *p]);
        }
        body.extend_from_slice(&(akm.len() as u16).to_le_bytes());
        for a in akm {
            body.extend_from_slice(&[0x00, 0x50, 0xF2, *a]);
        }
        let mut ie = vec![ELEMENT_VENDOR, body.len() as u8];
        ie.extend(body);
        ie
    }
}

#[cfg(test)]
mod tests {
    use super::stream::*;
    use super::*;
    use std::mem::{offset_of, size_of};

    #[test]
    fn layouts_without_pointers_match_wireless_h() {
        // Identical on 32- and 64-bit: nothing wider than 4 bytes.
        assert_eq!(size_of::<Sockaddr>(), 16);
        assert_eq!(size_of::<IwParam>(), 8);
        assert_eq!(size_of::<IwEncodeExt>(), 40);
        assert_eq!(offset_of!(IwEncodeExt, addr), 20);
        assert_eq!(offset_of!(IwEncodeExt, alg), 36);
        assert_eq!(size_of::<EncodeExtPmk>(), 72);
        assert_eq!(size_of::<IwMlme>(), 20);
        assert_eq!(size_of::<IfreqAddr>(), 40);
        assert_eq!(offset_of!(IfreqAddr, addr), 20);
    }

    #[test]
    fn iwreq_is_32_bytes() {
        assert!(size_of::<Iwreq>() >= 32);
        assert_eq!(offset_of!(Iwreq, u), 16);
        assert!(size_of::<IfreqFlags>() >= 32);
    }

    #[test]
    #[cfg(target_pointer_width = "32")]
    fn iw_point_on_32_bit_arm() {
        assert_eq!(size_of::<Iwreq>(), 32);
        assert_eq!(size_of::<IwPoint>(), 8);
        assert_eq!(offset_of!(IwPoint, length), 4);
        assert_eq!(offset_of!(IwPoint, flags), 6);
        assert_eq!(size_of::<IwreqData>(), 16);
    }

    #[test]
    fn bssids() {
        assert!(!is_bssid(&[0; 6]));
        assert!(!is_bssid(&[0xff; 6]));
        assert!(!is_bssid(&[0x44; 6]));
        assert!(is_bssid(&[0x80, 0x69, 0x1a, 0x21, 0xab, 0xc2]));
        assert_eq!(
            mac_string(&[0x80, 0x69, 0x1a, 0, 0xab, 0xc2]),
            "80:69:1a:00:ab:c2"
        );
    }

    #[test]
    fn decodes_rsn_and_wpa_elements() {
        let (found_rsn, found_wpa) = security_elements(&rsn(4, &[4], &[2]));
        assert_eq!(
            found_rsn,
            Some(Suites {
                group: Cipher::Ccmp,
                pairwise: vec![Cipher::Ccmp],
                akm: vec![Akm::Psk]
            })
        );
        assert_eq!(found_wpa, None);
        // Mixed mode: both elements, TKIP group.
        let mut ies = wpa(2, &[2], &[2]);
        ies.extend(rsn(2, &[4, 2], &[2, 8]));
        let (mixed_rsn, mixed_wpa) = security_elements(&ies);
        let mixed_rsn = mixed_rsn.unwrap();
        assert_eq!(mixed_rsn.group, Cipher::Tkip);
        assert_eq!(mixed_rsn.pairwise, [Cipher::Ccmp, Cipher::Tkip]);
        assert_eq!(mixed_rsn.akm, [Akm::Psk, Akm::Sae]);
        assert_eq!(mixed_wpa.unwrap().pairwise, [Cipher::Tkip]);
        // The bare minimum, version only: every default.
        let (minimal, _) = security_elements(&[48, 2, 1, 0]);
        assert_eq!(minimal.unwrap().akm, [Akm::Ieee8021x]);
        // Truncated lists and other vendors' elements are ignored.
        assert_eq!(
            security_elements(&[48, 8, 1, 0, 0, 0x0F, 0xAC, 4, 5, 0]),
            (None, None)
        );
        assert_eq!(security_elements(&[221, 4, 0, 0x50, 0xF2, 2]), (None, None));
        assert_eq!(security_elements(&[48, 40, 1]), (None, None));
    }

    #[test]
    fn parses_an_ar6000_scan_stream() {
        let home = [0x80, 0x69, 0x1a, 0x21, 0xab, 0xc2];
        let mut s = Stream::default();
        s.ap(home, b"Home", 2437, 60, true, &[&rsn(4, &[4], &[2])])
            .ap([2; 6], b"", 5180, 20, true, &[&rsn(4, &[4], &[2])])
            .ap([3; 6], b"Cafe", 2412, 10, false, &[])
            .ap([4; 6], b"Old", 2462, 30, true, &[&wpa(2, &[2], &[2])])
            .ap([5; 6], b"Wep", 2484, 30, true, &[]);
        let found = parse_scan(&s.0);
        assert_eq!(found.len(), 5);
        assert_eq!(found[0].bssid, home);
        assert_eq!(found[0].ssid, b"Home");
        assert_eq!(found[0].mhz, Some(2437));
        assert_eq!(found[0].channel(), Some(6));
        // 161 + 60 = 221 as a signed byte: -35 dBm.
        assert_eq!(found[0].level, Some(-35));
        assert!(found[0].privacy);
        assert_eq!(found[0].rsn.as_ref().unwrap().akm, [Akm::Psk]);
        assert!(found[1].ssid.is_empty());
        assert_eq!(found[1].channel(), Some(36));
        assert!(!found[2].privacy);
        assert_eq!(found[2].rsn, None);
        assert_eq!(found[3].wpa.as_ref().unwrap().group, Cipher::Tkip);
        assert_eq!(found[4].channel(), Some(14));
        assert!(found[4].privacy && found[4].rsn.is_none() && found[4].wpa.is_none());
    }

    #[test]
    fn a_truncated_stream_keeps_what_was_whole() {
        let mut s = Stream::default();
        s.ap([1; 6], b"One", 2412, 30, false, &[])
            .ap([2; 6], b"Two", 2412, 30, false, &[]);
        let cut = s.0.len() - 3;
        let found = parse_scan(&s.0[..cut]);
        assert_eq!(found.len(), 2);
        assert_eq!(found[1].ssid, b"Two");
        assert_eq!(parse_scan(&[]), []);
        // A zero-length event stops the walk instead of looping.
        assert_eq!(parse_scan(&[0, 0, 0x15, 0x8b, 1, 2, 3, 4]), []);
    }

    #[test]
    fn old_drivers_hex_elements() {
        let mut s = Stream::default();
        s.ap([1; 6], b"One", 2412, 30, true, &[]);
        let text: String = rsn(4, &[4], &[2])
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let custom = format!("rsn_ie={text}");
        let mut payload = (custom.len() as u16).to_le_bytes().to_vec();
        payload.extend_from_slice(&[0, 0]);
        payload.extend_from_slice(custom.as_bytes());
        s.0.extend_from_slice(&(4 + payload.len() as u16).to_le_bytes());
        s.0.extend_from_slice(&IWEVCUSTOM.to_le_bytes());
        s.0.extend_from_slice(&payload);
        let found = parse_scan(&s.0);
        assert_eq!(found[0].rsn.as_ref().unwrap().group, Cipher::Ccmp);
    }

    #[test]
    fn the_scan_buffer_grows_on_e2big_up_to_the_most_an_iw_point_holds() {
        let mut sizes = Vec::new();
        let got = read_growing(|buf| {
            sizes.push(buf.len());
            if buf.len() < 20_000 {
                Err(io::Error::from_raw_os_error(libc::E2BIG))
            } else {
                buf[..3].copy_from_slice(b"abc");
                Ok(3)
            }
        })
        .unwrap();
        assert_eq!(got, b"abc");
        assert_eq!(sizes, [4096, 8192, 16384, 32768]);
        // Never more than 65535: then E2BIG is the answer.
        let mut last = 0;
        let error = read_growing(|buf| {
            last = buf.len();
            Err(io::Error::from_raw_os_error(libc::E2BIG))
        })
        .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::E2BIG));
        assert_eq!(last, 65535);
        // EAGAIN isn't retried here.
        let mut calls = 0;
        let error = read_growing(|_| {
            calls += 1;
            Err(io::Error::from_raw_os_error(libc::EAGAIN))
        })
        .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EAGAIN));
        assert_eq!(calls, 1);
        // A driver claiming more than the buffer: cut to the buffer.
        assert_eq!(read_growing(|_| Ok(60_000)).unwrap().len(), 4096);
    }

    #[test]
    fn hand_written_malformed_and_unaligned_streams() {
        // An odd-length ESSID puts the next event at an odd offset.
        let mut s: Vec<u8> = Vec::new();
        s.extend_from_slice(&[20, 0, 0x15, 0x8b, 1, 0, 1, 2, 3, 4, 5, 6]);
        s.extend_from_slice(&[0; 8]);
        s.extend_from_slice(&[11, 0, 0x1b, 0x8b, 3, 0, 1, 0, b'a', b'b', b'c']);
        // IWEVQUAL at offset 31.
        s.extend_from_slice(&[8, 0, 0x01, 0x8c, 40, 201, 161, 7]);
        // A point event claiming more data than it carries: ignored.
        s.extend_from_slice(&[10, 0, 0x05, 0x8c, 200, 0, 0, 0, 48, 2]);
        // A GENIE whose element is cut short: no suites.
        s.extend_from_slice(&[12, 0, 0x05, 0x8c, 4, 0, 0, 0, 48, 20, 1, 0]);
        let found = parse_scan(&s);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].bssid, [1, 2, 3, 4, 5, 6]);
        assert_eq!(found[0].ssid, b"abc");
        assert_eq!(found[0].level, Some(-55));
        assert_eq!(found[0].rsn, None);
        // Events before any access point, an AP address too short, an event
        // shorter than its header, and one running past the end.
        let junk = [
            8, 0, 0x01, 0x8c, 1, 2, 3, 4, // QUAL before any AP: skipped
            6, 0, 0x15, 0x8b, 1, 0, // SIOCGIWAP without an address
            3, 0, 0x1b, 0x8b, // shorter than a header: stop
            0, 0,
        ];
        assert_eq!(parse_scan(&junk), []);
        assert_eq!(parse_scan(&[200, 0, 0x15, 0x8b, 1, 0]), []);
        assert_eq!(parse_scan(&[1, 0, 0x15]), []);
    }

    #[test]
    fn frequencies_and_levels() {
        assert_eq!(mhz(2412 * 100_000, 1), Some(2412));
        assert_eq!(mhz(6, 0), Some(2437));
        assert_eq!(mhz(36, 0), Some(5180));
        assert_eq!(mhz(0, 0), None);
        assert_eq!(level_dbm(220), Some(-36));
        assert_eq!(level_dbm(-60), Some(-60));
        assert_eq!(level_dbm(0), None);
        assert_eq!(level_dbm(40), None);
    }

    #[test]
    fn reads_the_level_of_proc_net_wireless() {
        // As the PW1 shows it (WE 22, ar6000: no dBm flag, no dots).
        let proc = "Inter-| sta-|   Quality        |   Discarded packets               | Missed | WE\n \
                    face | tus | link level noise |  nwid  crypt   frag  retry   misc | beacon | 22\n \
                    wlan0: 0001   59   220   160        0      0      0      0      0      179\n";
        assert_eq!(parse_proc_level(proc, "wlan0"), Some(-36));
        let dotted =
            " wlan0: 0000   45.  -50.  -95.       0      0      0      0      0        0\n";
        assert_eq!(parse_proc_level(dotted, "wlan0"), Some(-50));
        assert_eq!(parse_proc_level(proc, "eth0"), None);
    }
}

// The ARM layout, checked at compile time when building for the device.
#[cfg(target_pointer_width = "32")]
const _: () = {
    assert!(std::mem::size_of::<Iwreq>() == 32);
    assert!(std::mem::size_of::<IwPoint>() == 8);
    assert!(std::mem::size_of::<IwEncodeExt>() == 40);
    assert!(std::mem::size_of::<EncodeExtPmk>() == 72);
};
