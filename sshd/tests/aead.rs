//! vendor/russh's RustCrypto AEADs (the `rustcrypto` backend kindil-sshd
//! uses) against ring's, which russh uses upstream: the same output for the
//! same keys, nonces and packets, and the same refusals.
#![cfg(not(target_arch = "arm"))]

use ring::aead as theirs;
use ring::aead::chacha20_poly1305_openssh as theirs_chacha;
use russh::cipher::rustcrypto::aead::{self as ours, BoundKey, NonceSequence};
use russh::cipher::rustcrypto::chacha20_poly1305_openssh as ours_chacha;
use russh::cipher::rustcrypto::{error::Unspecified, Budget, PACKETS_PER_KEY};

/// Deterministic bytes (xorshift), so failures repeat.
struct Bytes(u64);

impl Bytes {
    fn fill(&mut self, n: usize) -> Vec<u8> {
        (0..n)
            .map(|_| {
                self.0 ^= self.0 << 13;
                self.0 ^= self.0 >> 7;
                self.0 ^= self.0 << 17;
                self.0 as u8
            })
            .collect()
    }
}

const LENGTHS: [usize; 9] = [0, 1, 4, 15, 16, 17, 255, 4096, 32768 + 5];

/// russh's GCM nonces: the derived IV, then incremented as a 96-bit
/// big-endian number for each packet (cipher/gcm.rs).
struct Counter([u8; 12]);

impl Counter {
    fn next(&mut self) -> [u8; 12] {
        let current = self.0;
        let mut carry = 1u16;
        for byte in self.0.iter_mut().rev() {
            let n = u16::from(*byte) + carry;
            *byte = n as u8;
            carry = n >> 8;
        }
        current
    }
}

impl NonceSequence for Counter {
    fn advance(&mut self) -> Result<ours::Nonce, Unspecified> {
        Ok(ours::Nonce::assume_unique_for_key(self.next()))
    }
}

fn gcm_algorithms() -> [(&'static ours::Algorithm, &'static theirs::Algorithm, usize); 2] {
    [
        (&ours::AES_128_GCM, &theirs::AES_128_GCM, 16),
        (&ours::AES_256_GCM, &theirs::AES_256_GCM, 32),
    ]
}

fn ours_gcm(
    algorithm: &'static ours::Algorithm,
    key: &[u8],
    iv: [u8; 12],
) -> (ours::SealingKey<Counter>, ours::OpeningKey<Counter>) {
    (
        ours::SealingKey::new(ours::UnboundKey::new(algorithm, key).unwrap(), Counter(iv)),
        ours::OpeningKey::new(ours::UnboundKey::new(algorithm, key).unwrap(), Counter(iv)),
    )
}

#[test]
fn aes_gcm_seals_as_ring_does_and_opens_rings() {
    let mut bytes = Bytes(0x5eed);
    for (algorithm, ring_algorithm, key_len) in gcm_algorithms() {
        let key = bytes.fill(key_len);
        // Just before the low 32 bits wrap: the carry is exercised too.
        let mut iv = [0u8; 12];
        iv[..8].copy_from_slice(&bytes.fill(8));
        iv[8..].copy_from_slice(&[0xff, 0xff, 0xff, 0xfe]);
        let (mut sealing, mut opening) = ours_gcm(algorithm, &key, iv);
        let ring_key =
            theirs::LessSafeKey::new(theirs::UnboundKey::new(ring_algorithm, &key).unwrap());
        let mut ring_nonces = Counter(iv);
        for len in LENGTHS {
            let plain = bytes.fill(len);
            let aad = (len as u32).to_be_bytes();
            let mut sealed = plain.clone();
            let tag = sealing
                .seal_in_place_separate_tag(ours::Aad::from(aad), &mut sealed)
                .unwrap();
            let mut ring_sealed = plain.clone();
            let ring_tag = ring_key
                .seal_in_place_separate_tag(
                    theirs::Nonce::assume_unique_for_key(ring_nonces.next()),
                    theirs::Aad::from(aad),
                    &mut ring_sealed,
                )
                .unwrap();
            assert_eq!(sealed, ring_sealed, "{len} bytes");
            assert_eq!(tag.as_ref(), ring_tag.as_ref(), "{len} bytes");
            // What ring sealed, opened here.
            let mut packet = [ring_sealed, ring_tag.as_ref().to_vec()].concat();
            let opened = opening
                .open_in_place(ours::Aad::from(aad), &mut packet)
                .unwrap();
            assert_eq!(opened, &plain[..]);
        }
    }
}

/// A packet sealed with `seal`'s first nonce, its AAD and tag.
fn gcm_packet(algorithm: &'static ours::Algorithm, key: &[u8], iv: [u8; 12]) -> (Vec<u8>, [u8; 4]) {
    let (mut sealing, _) = ours_gcm(algorithm, key, iv);
    let mut packet = b"a packet of some length".to_vec();
    let aad = (packet.len() as u32).to_be_bytes();
    let tag = sealing
        .seal_in_place_separate_tag(ours::Aad::from(aad), &mut packet)
        .unwrap();
    packet.extend_from_slice(tag.as_ref());
    (packet, aad)
}

#[test]
fn aes_gcm_refuses_what_was_changed_and_leaves_it_be() {
    let mut bytes = Bytes(7);
    for (algorithm, _, key_len) in gcm_algorithms() {
        let key = bytes.fill(key_len);
        let iv = [3u8; 12];
        let (packet, aad) = gcm_packet(algorithm, &key, iv);
        let refused = |mut packet: Vec<u8>, aad: [u8; 4], iv: [u8; 12]| {
            let (_, mut opening) = ours_gcm(algorithm, &key, iv);
            let before = packet.clone();
            let result = opening
                .open_in_place(ours::Aad::from(aad), &mut packet)
                .map(|_| ());
            if result.is_err() {
                assert_eq!(packet, before, "changed on failure");
            }
            result.is_err()
        };
        assert!(!refused(packet.clone(), aad, iv), "the packet itself opens");
        let last = packet.len() - 1;
        let mut tag = packet.clone();
        tag[last] ^= 1;
        assert!(refused(tag, aad, iv), "tag");
        let mut text = packet.clone();
        text[0] ^= 0x80;
        assert!(refused(text, aad, iv), "ciphertext");
        let mut wrong_aad = aad;
        wrong_aad[3] ^= 1;
        assert!(refused(packet.clone(), wrong_aad, iv), "aad");
        let mut wrong_nonce = iv;
        wrong_nonce[11] ^= 1;
        assert!(refused(packet.clone(), aad, wrong_nonce), "nonce");
        let wrong_key = bytes.fill(key_len);
        let (_, mut opening) = ours_gcm(algorithm, &wrong_key, iv);
        assert!(opening
            .open_in_place(ours::Aad::from(aad), &mut packet.clone())
            .is_err());
        for short in [0, 1, 15] {
            assert!(refused(packet[..short].to_vec(), aad, iv), "{short} bytes");
        }
        assert!(ours::UnboundKey::new(algorithm, &key[1..]).is_err());
    }
}

fn chacha_keys(bytes: &mut Bytes) -> [u8; 64] {
    bytes.fill(64).try_into().unwrap()
}

#[test]
fn chacha20_poly1305_openssh_matches_ring() {
    let mut bytes = Bytes(0xc4ac4a);
    let key = chacha_keys(&mut bytes);
    let (sealing, opening) = (
        ours_chacha::SealingKey::new(&key),
        ours_chacha::OpeningKey::new(&key),
    );
    let (ring_sealing, ring_opening) = (
        theirs_chacha::SealingKey::new(&key),
        theirs_chacha::OpeningKey::new(&key),
    );
    for (i, len) in LENGTHS.into_iter().enumerate() {
        // Sequence numbers near both ends of their range too.
        let sequence = [0, 1, 3, 1000, u32::MAX / 2, u32::MAX - 1, u32::MAX, 7, 42][i];
        let mut plain = (len as u32).to_be_bytes().to_vec();
        plain.extend(bytes.fill(len));
        let (mut sealed, mut tag) = (plain.clone(), [0; 16]);
        sealing.seal_in_place(sequence, &mut sealed, &mut tag);
        let (mut ring_sealed, mut ring_tag) = (plain.clone(), [0; 16]);
        ring_sealing.seal_in_place(sequence, &mut ring_sealed, &mut ring_tag);
        assert_eq!(sealed, ring_sealed, "{len} bytes at {sequence}");
        assert_eq!(tag, ring_tag, "{len} bytes at {sequence}");
        let length: [u8; 4] = sealed[..4].try_into().unwrap();
        assert_eq!(
            opening.decrypt_packet_length(sequence, length),
            ring_opening.decrypt_packet_length(sequence, length)
        );
        assert_eq!(opening.decrypt_packet_length(sequence, length), plain[..4]);
        // Each opens the other's.
        let mut ours_opened = ring_sealed.clone();
        assert_eq!(
            opening
                .open_in_place(sequence, &mut ours_opened, &ring_tag)
                .unwrap(),
            &plain[4..]
        );
        let mut ring_opened = sealed.clone();
        assert_eq!(
            ring_opening
                .open_in_place(sequence, &mut ring_opened, &tag)
                .unwrap(),
            &plain[4..]
        );
    }
}

#[test]
fn chacha20_poly1305_openssh_refuses_what_was_changed_and_leaves_it_be() {
    let mut bytes = Bytes(99);
    let key = chacha_keys(&mut bytes);
    let sealing = ours_chacha::SealingKey::new(&key);
    let mut packet = vec![0, 0, 0, 20];
    packet.extend(bytes.fill(20));
    let mut tag = [0; 16];
    sealing.seal_in_place(5, &mut packet, &mut tag);
    let refused = |mut packet: Vec<u8>, tag: [u8; 16], sequence: u32| {
        let opening = ours_chacha::OpeningKey::new(&key);
        let before = packet.clone();
        let result = opening
            .open_in_place(sequence, &mut packet, &tag)
            .map(|_| ());
        if result.is_err() {
            assert_eq!(packet, before, "changed on failure");
        }
        // ring agrees.
        let ring_opening = theirs_chacha::OpeningKey::new(&key);
        let ring = ring_opening
            .open_in_place(sequence, &mut before.clone(), &tag)
            .map(|_| ());
        assert_eq!(result.is_err(), ring.is_err());
        result.is_err()
    };
    assert!(!refused(packet.clone(), tag, 5), "the packet itself opens");
    let mut bad_tag = tag;
    bad_tag[0] ^= 1;
    assert!(refused(packet.clone(), bad_tag, 5), "tag");
    let mut length = packet.clone();
    length[2] ^= 1;
    assert!(refused(length, tag, 5), "the encrypted length (its AAD)");
    let mut text = packet.clone();
    text[10] ^= 1;
    assert!(refused(text, tag, 5), "ciphertext");
    assert!(refused(packet.clone(), tag, 6), "sequence number");
    assert!(
        refused(packet[..packet.len() - 1].to_vec(), tag, 5),
        "truncated"
    );
    let opening = ours_chacha::OpeningKey::new(&key);
    for short in [0, 3] {
        assert!(opening
            .open_in_place(5, &mut packet[..short].to_vec(), &tag)
            .is_err());
    }
    let mut other_key = key;
    other_key[0] ^= 1;
    let other = ours_chacha::OpeningKey::new(&other_key);
    assert!(other.open_in_place(5, &mut packet.clone(), &tag).is_err());
}

#[test]
fn a_key_opens_no_more_than_its_share() {
    let mut budget = Budget::spent(PACKETS_PER_KEY - 2);
    assert!(budget.spend().is_ok());
    assert!(budget.spend().is_ok());
    for _ in 0..3 {
        assert!(budget.spend().is_err());
    }
    budget = Budget::spent(PACKETS_PER_KEY);
    assert!(budget.spend().is_err());

    // AES-GCM: one more packet, then none.
    let key = [9u8; 32];
    let (packet, aad) = gcm_packet(&ours::AES_256_GCM, &key, [1; 12]);
    let (_, opening) = ours_gcm(&ours::AES_256_GCM, &key, [1; 12]);
    let mut opening = opening.with_budget(Budget::spent(PACKETS_PER_KEY - 1));
    assert!(opening
        .open_in_place(ours::Aad::from(aad), &mut packet.clone())
        .is_ok());
    assert!(opening
        .open_in_place(ours::Aad::from(aad), &mut packet.clone())
        .is_err());
    let (sealing, _) = ours_gcm(&ours::AES_256_GCM, &key, [1; 12]);
    let mut sealing = sealing.with_budget(Budget::spent(PACKETS_PER_KEY));
    assert!(sealing
        .seal_in_place_separate_tag(ours::Aad::from(aad), &mut [0u8; 8])
        .is_err());

    // ChaCha20-Poly1305: the same.
    let key = [4u8; 64];
    let mut packet = vec![0, 0, 0, 8, 1, 2, 3, 4, 5, 6, 7, 8];
    let mut tag = [0; 16];
    ours_chacha::SealingKey::new(&key).seal_in_place(0, &mut packet, &mut tag);
    let opening =
        ours_chacha::OpeningKey::new(&key).with_budget(Budget::spent(PACKETS_PER_KEY - 1));
    assert!(opening.open_in_place(0, &mut packet.clone(), &tag).is_ok());
    assert!(opening.open_in_place(0, &mut packet.clone(), &tag).is_err());
}

#[test]
#[should_panic(expected = "sealed its share")]
fn a_chacha_key_seals_no_more_than_its_share() {
    let sealing =
        ours_chacha::SealingKey::new(&[4u8; 64]).with_budget(Budget::spent(PACKETS_PER_KEY));
    sealing.seal_in_place(0, &mut [0, 0, 0, 4, 1, 2, 3, 4], &mut [0; 16]);
}
