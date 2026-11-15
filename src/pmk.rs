//! The WPA pre-shared key: PBKDF2-HMAC-SHA1(passphrase, ssid, 4096, 32),
//! IEEE 802.11-2016 J.4, which kindil's own Wi-Fi (src/station.rs) hands the
//! AR6003 firmware for its 4-way handshake. SHA-1 and HMAC are written out
//! here (as in the wifiprobe experiment) rather than pulled in as crates;
//! neither is used for anything else.
//!
//! Every copy of key material here is wiped when dropped, best effort: the
//! passphrase itself lives in kindil's saved networks (src/networks.rs).

pub const PMK_LEN: usize = 32;

/// A pairwise master key, wiped when dropped. Never printed.
pub struct Pmk([u8; PMK_LEN]);

impl Pmk {
    pub fn bytes(&self) -> &[u8; PMK_LEN] {
        &self.0
    }
}

impl Drop for Pmk {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

impl std::fmt::Debug for Pmk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pmk(..)")
    }
}

/// Zeroes a string's bytes, its spare capacity too, in place.
pub fn wipe_string(text: &mut String) {
    // SAFETY: zero bytes keep the string valid UTF-8; the spare capacity
    // is initialised (zeroed) before it is counted in.
    let bytes = unsafe { text.as_mut_vec() };
    let len = bytes.len();
    bytes.resize(bytes.capacity(), 0);
    wipe(bytes);
    bytes.truncate(len);
}

/// Zeroes `bytes` in a way the optimizer keeps.
pub fn wipe(bytes: &mut [u8]) {
    for b in bytes.iter_mut() {
        // SAFETY: a valid, exclusively borrowed byte.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

fn wipe_words(words: &mut [u32]) {
    for w in words.iter_mut() {
        // SAFETY: a valid, exclusively borrowed word.
        unsafe { std::ptr::write_volatile(w, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

/// A 64-hex-digit passphrase is the PSK itself; 8 to 63 printable ASCII
/// characters are hashed with the SSID.
pub fn pmk(passphrase: &str, ssid: &[u8]) -> Result<Pmk, &'static str> {
    let mut out = Pmk([0u8; PMK_LEN]);
    if passphrase.len() == 64 && passphrase.bytes().all(|b| b.is_ascii_hexdigit()) {
        for (i, pair) in passphrase.as_bytes().chunks(2).enumerate() {
            let hex = |b: u8| (b as char).to_digit(16).unwrap_or(0) as u8;
            out.0[i] = hex(pair[0]) << 4 | hex(pair[1]);
        }
        return Ok(out);
    }
    if !(8..=63).contains(&passphrase.len()) || !passphrase.bytes().all(|b| (32..127).contains(&b))
    {
        return Err("the password is not 8 to 63 printable ASCII characters");
    }
    if ssid.is_empty() || ssid.len() > 32 {
        return Err("the network name is not 1 to 32 bytes");
    }
    pbkdf2_hmac_sha1(passphrase.as_bytes(), ssid, 4096, &mut out.0);
    Ok(out)
}

fn pbkdf2_hmac_sha1(password: &[u8], salt: &[u8], rounds: u32, out: &mut [u8]) {
    let mac = Hmac::new(password);
    for (block, chunk) in out.chunks_mut(20).enumerate() {
        let mut first = salt.to_vec();
        first.extend_from_slice(&(block as u32 + 1).to_be_bytes());
        let mut u = [0u8; 20];
        mac.sign_into(&first, &mut u);
        let mut t = u;
        let mut next = [0u8; 20];
        for _ in 1..rounds {
            mac.sign_into(&u, &mut next);
            u.copy_from_slice(&next);
            for (t, u) in t.iter_mut().zip(u.iter()) {
                *t ^= u;
            }
        }
        chunk.copy_from_slice(&t[..chunk.len()]);
        wipe(&mut u);
        wipe(&mut t);
        wipe(&mut next);
    }
}

/// HMAC-SHA1 with the keyed inner and outer states computed once.
struct Hmac {
    inner: Sha1,
    outer: Sha1,
}

impl Hmac {
    fn new(key: &[u8]) -> Self {
        let mut block = [0u8; 64];
        if key.len() > 64 {
            let mut h = Sha1::new();
            h.update(key);
            h.finish_into(&mut block[..20]);
        } else {
            block[..key.len()].copy_from_slice(key);
        }
        let mut inner = Sha1::new();
        let mut outer = Sha1::new();
        let mut pad = block.map(|b| b ^ 0x36);
        inner.update(&pad);
        pad = block.map(|b| b ^ 0x5c);
        outer.update(&pad);
        wipe(&mut pad);
        wipe(&mut block);
        Self { inner, outer }
    }

    /// The MAC of `message` into `out`; the inner digest is wiped.
    fn sign_into(&self, message: &[u8], out: &mut [u8; 20]) {
        let mut inner = self.inner.clone();
        inner.update(message);
        let mut digest = [0u8; 20];
        inner.finish_into(&mut digest);
        let mut outer = self.outer.clone();
        outer.update(&digest);
        wipe(&mut digest);
        outer.finish_into(out);
    }

    #[cfg(test)]
    fn sign(&self, message: &[u8]) -> [u8; 20] {
        let mut out = [0u8; 20];
        self.sign_into(message, &mut out);
        out
    }
}

#[derive(Clone)]
struct Sha1 {
    state: [u32; 5],
    buffer: [u8; 64],
    buffered: usize,
    length: u64,
}

impl Drop for Sha1 {
    fn drop(&mut self) {
        // The keyed states hold the key, xored with a known pad.
        wipe(&mut self.buffer);
        wipe_words(&mut self.state);
    }
}

impl Sha1 {
    fn new() -> Self {
        Self {
            state: [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0],
            buffer: [0; 64],
            buffered: 0,
            length: 0,
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.length += data.len() as u64;
        while !data.is_empty() {
            let take = (64 - self.buffered).min(data.len());
            self.buffer[self.buffered..self.buffered + take].copy_from_slice(&data[..take]);
            self.buffered += take;
            data = &data[take..];
            if self.buffered == 64 {
                self.compress();
                self.buffered = 0;
            }
        }
    }

    /// The digest into `out` (20 bytes); the state is wiped as it drops.
    fn finish_into(mut self, out: &mut [u8]) {
        let bits = self.length * 8;
        self.update(&[0x80]);
        while self.buffered != 56 {
            self.update(&[0]);
        }
        self.update(&bits.to_be_bytes());
        for (chunk, word) in out.chunks_mut(4).zip(self.state.iter()) {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
    }

    #[cfg(test)]
    fn finish(self) -> [u8; 20] {
        let mut out = [0u8; 20];
        self.finish_into(&mut out);
        out
    }

    fn compress(&mut self) {
        let mut w = [0u32; 80];
        for (i, chunk) in self.buffer.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = self.state;
        for (i, &word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        for (s, v) in self.state.iter_mut().zip([a, b, c, d, e]) {
            *s = s.wrapping_add(v);
        }
        wipe_words(&mut w);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn sha1_fips_vectors() {
        let digest = |m: &[u8]| {
            let mut h = Sha1::new();
            h.update(m);
            hex(&h.finish())
        };
        assert_eq!(digest(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(digest(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(
            digest(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    #[test]
    fn hmac_rfc2202() {
        let mac = Hmac::new(b"Jefe");
        assert_eq!(
            hex(&mac.sign(b"what do ya want for nothing?")),
            "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79"
        );
        // A key longer than the block is hashed first.
        let mac = Hmac::new(&[0xaa; 80]);
        assert_eq!(
            hex(&mac.sign(b"Test Using Larger Than Block-Size Key - Hash Key First")),
            "aa4ae5e15272d00e95705637ce8a3b55ed402112"
        );
    }

    #[test]
    fn ieee_802_11_annex_j_psk() {
        assert_eq!(
            hex(pmk("password", b"IEEE").unwrap().bytes()),
            "f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e"
        );
        assert_eq!(
            hex(pmk("ThisIsAPassword", b"ThisIsASSID").unwrap().bytes()),
            "0dc0d6eb90555ed6419756b9a15ec3e3209b63df707dd508d14581f8982721af"
        );
    }

    #[test]
    fn hex_psk_is_taken_as_is() {
        let key = "f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e";
        assert_eq!(hex(pmk(key, b"anything").unwrap().bytes()), key);
    }

    #[test]
    fn rejects_bad_passphrases() {
        assert!(pmk("short", b"IEEE").is_err());
        assert!(pmk(&"x".repeat(64), b"IEEE").is_err());
        assert!(pmk("password", b"").is_err());
        assert_eq!(
            format!("{:?}", pmk("password", b"IEEE").unwrap()),
            "Pmk(..)"
        );
    }

    #[test]
    fn wipes() {
        let mut key = [7u8; 4];
        wipe(&mut key);
        assert_eq!(key, [0; 4]);
        let mut text = String::with_capacity(32);
        text.push_str("correct horse");
        let capacity = text.capacity();
        wipe_string(&mut text);
        assert_eq!(text.len(), 13);
        assert!(text.bytes().all(|b| b == 0));
        assert_eq!(text.capacity(), capacity);
    }
}
