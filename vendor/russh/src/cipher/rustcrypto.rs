// kindil: the subset of ring's AEAD API that gcm.rs and chacha20poly1305.rs
// use, on RustCrypto's pure-Rust aes-gcm and ssh-cipher, so that russh builds
// with no C (no ring, no aws-lc-rs). Selected by the `rustcrypto` feature
// when neither of the others is on. Public (hidden) only so that kindil-sshd
// can test it against ring.
//
// Unlike ring, each key also refuses to open or seal more than
// PACKETS_PER_KEY packets: russh rekeys long before (kindil-sshd asks for it
// every 128 MiB), but a peer that stalls a key exchange keeps the old keys
// in use, and ChaCha20's nonce, the 32-bit sequence number, must never wrap.

use std::sync::atomic::{AtomicU32, Ordering};

pub mod error {
    /// ring's `error::Unspecified`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Unspecified;
}

/// The most packets one key opens, or seals, in one direction.
pub const PACKETS_PER_KEY: u32 = 1 << 28;

/// A key's packets so far.
#[derive(Default)]
pub struct Budget(AtomicU32);

impl Budget {
    /// One more packet, unless the key has had its share.
    pub fn spend(&self) -> Result<(), error::Unspecified> {
        let spent = self.0.fetch_add(1, Ordering::Relaxed);
        if spent >= PACKETS_PER_KEY {
            // Never back past the limit, however often it is asked.
            self.0.store(PACKETS_PER_KEY, Ordering::Relaxed);
            return Err(error::Unspecified);
        }
        Ok(())
    }

    /// For tests: as if `spent` packets had gone already.
    #[doc(hidden)]
    pub fn spent(spent: u32) -> Self {
        Self(AtomicU32::new(spent))
    }
}

/// ring's `aead`, for AES-GCM (`aes128-gcm@openssh.com`,
/// `aes256-gcm@openssh.com`).
pub mod aead {
    use aes_gcm::aead::AeadInOut;
    use aes_gcm::{Aes128Gcm, Aes256Gcm, KeyInit};

    use super::Budget;
    use super::error::Unspecified;

    pub const NONCE_LEN: usize = 12;
    const TAG_LEN: usize = 16;

    pub struct Algorithm {
        key_len: usize,
    }

    impl Algorithm {
        pub fn key_len(&self) -> usize {
            self.key_len
        }

        pub fn nonce_len(&self) -> usize {
            NONCE_LEN
        }

        pub fn tag_len(&self) -> usize {
            TAG_LEN
        }
    }

    pub static AES_128_GCM: Algorithm = Algorithm { key_len: 16 };
    pub static AES_256_GCM: Algorithm = Algorithm { key_len: 32 };

    enum Key {
        Aes128(Box<Aes128Gcm>),
        Aes256(Box<Aes256Gcm>),
    }

    pub struct UnboundKey {
        algorithm: &'static Algorithm,
        key: Key,
    }

    impl UnboundKey {
        pub fn new(algorithm: &'static Algorithm, key: &[u8]) -> Result<Self, Unspecified> {
            let key = match algorithm.key_len {
                16 => Key::Aes128(Box::new(
                    Aes128Gcm::new_from_slice(key).map_err(|_| Unspecified)?,
                )),
                32 => Key::Aes256(Box::new(
                    Aes256Gcm::new_from_slice(key).map_err(|_| Unspecified)?,
                )),
                _ => return Err(Unspecified),
            };
            Ok(Self { algorithm, key })
        }
    }

    pub struct Nonce([u8; NONCE_LEN]);

    impl Nonce {
        pub fn assume_unique_for_key(nonce: [u8; NONCE_LEN]) -> Self {
            Self(nonce)
        }
    }

    pub trait NonceSequence {
        fn advance(&mut self) -> Result<Nonce, Unspecified>;
    }

    pub struct Aad<A>(A);

    impl<A: AsRef<[u8]>> Aad<A> {
        pub fn from(aad: A) -> Self {
            Self(aad)
        }
    }

    pub struct Tag([u8; TAG_LEN]);

    impl AsRef<[u8]> for Tag {
        fn as_ref(&self) -> &[u8] {
            &self.0
        }
    }

    pub trait BoundKey<N: NonceSequence> {
        fn new(key: UnboundKey, nonce_sequence: N) -> Self;
        fn algorithm(&self) -> &'static Algorithm;
    }

    pub struct OpeningKey<N> {
        key: UnboundKey,
        nonces: N,
        budget: Budget,
    }

    pub struct SealingKey<N> {
        key: UnboundKey,
        nonces: N,
        budget: Budget,
    }

    impl<N> OpeningKey<N> {
        #[doc(hidden)]
        pub fn with_budget(mut self, budget: Budget) -> Self {
            self.budget = budget;
            self
        }
    }

    impl<N> SealingKey<N> {
        #[doc(hidden)]
        pub fn with_budget(mut self, budget: Budget) -> Self {
            self.budget = budget;
            self
        }
    }

    impl<N: NonceSequence> BoundKey<N> for OpeningKey<N> {
        fn new(key: UnboundKey, nonces: N) -> Self {
            Self {
                key,
                nonces,
                budget: Budget::default(),
            }
        }

        fn algorithm(&self) -> &'static Algorithm {
            self.key.algorithm
        }
    }

    impl<N: NonceSequence> BoundKey<N> for SealingKey<N> {
        fn new(key: UnboundKey, nonces: N) -> Self {
            Self {
                key,
                nonces,
                budget: Budget::default(),
            }
        }

        fn algorithm(&self) -> &'static Algorithm {
            self.key.algorithm
        }
    }

    impl<N: NonceSequence> OpeningKey<N> {
        /// `in_out` is the ciphertext followed by its tag; the plaintext, in
        /// place, without the tag.
        pub fn open_in_place<'a, A: AsRef<[u8]>>(
            &mut self,
            aad: Aad<A>,
            in_out: &'a mut [u8],
        ) -> Result<&'a mut [u8], Unspecified> {
            self.budget.spend()?;
            let nonce = self.nonces.advance()?;
            let len = in_out.len().checked_sub(TAG_LEN).ok_or(Unspecified)?;
            let (data, tag) = in_out.split_at_mut(len);
            let tag: [u8; TAG_LEN] = (&*tag).try_into().map_err(|_| Unspecified)?;
            let (nonce, tag) = (&nonce.0.into(), &tag.into());
            match &self.key.key {
                Key::Aes128(key) => {
                    key.decrypt_inout_detached(nonce, aad.0.as_ref(), data.into(), tag)
                }
                Key::Aes256(key) => {
                    key.decrypt_inout_detached(nonce, aad.0.as_ref(), data.into(), tag)
                }
            }
            .map_err(|_| Unspecified)?;
            Ok(data)
        }
    }

    impl<N: NonceSequence> SealingKey<N> {
        pub fn seal_in_place_separate_tag<A: AsRef<[u8]>>(
            &mut self,
            aad: Aad<A>,
            in_out: &mut [u8],
        ) -> Result<Tag, Unspecified> {
            self.budget.spend()?;
            let nonce = self.nonces.advance()?;
            let nonce = &nonce.0.into();
            let tag = match &self.key.key {
                Key::Aes128(key) => {
                    key.encrypt_inout_detached(nonce, aad.0.as_ref(), in_out.into())
                }
                Key::Aes256(key) => {
                    key.encrypt_inout_detached(nonce, aad.0.as_ref(), in_out.into())
                }
            }
            .map_err(|_| Unspecified)?;
            Ok(Tag(tag.into()))
        }
    }
}

/// ring's `aead::chacha20_poly1305_openssh`: `chacha20-poly1305@openssh.com`
/// as OpenSSH's PROTOCOL.chacha20poly1305 describes it. Of the 512-bit key,
/// the first half is K_2 (the payload and the Poly1305 key) and the second
/// K_1 (the packet length); the nonce is the sequence number.
pub mod chacha20_poly1305_openssh {
    use ssh_cipher::aead::{AeadInOut, KeyInit};
    use ssh_cipher::cipher::{KeyIvInit, StreamCipher};
    use ssh_cipher::{ChaCha20, ChaCha20Poly1305, ChaChaKey, ChaChaNonce};

    use super::Budget;
    use super::error::Unspecified;

    pub const KEY_LEN: usize = 64;
    pub const TAG_LEN: usize = 16;
    const PACKET_LENGTH_LEN: usize = 4;

    struct Key {
        k_1: ChaChaKey,
        k_2: ChaCha20Poly1305,
    }

    impl Key {
        fn new(key_material: &[u8; KEY_LEN]) -> Self {
            let (k_2, k_1) = key_material.split_at(KEY_LEN / 2);
            #[allow(clippy::unwrap_used)] // both halves are 32 bytes
            Self {
                k_1: ChaChaKey::try_from(k_1).unwrap(),
                k_2: ChaCha20Poly1305::new(&ChaChaKey::try_from(k_2).unwrap()),
            }
        }

        fn crypt_length(&self, sequence_number: u32, length: &mut [u8; PACKET_LENGTH_LEN]) {
            ChaCha20::new(&self.k_1, &nonce(sequence_number)).apply_keystream(length);
        }
    }

    fn nonce(sequence_number: u32) -> ChaChaNonce {
        let mut nonce = ChaChaNonce::default();
        #[allow(clippy::indexing_slicing)] // 8 bytes
        nonce[4..].copy_from_slice(&sequence_number.to_be_bytes());
        nonce
    }

    pub struct SealingKey(Key, Budget);

    impl SealingKey {
        #[doc(hidden)]
        pub fn with_budget(mut self, budget: Budget) -> Self {
            self.1 = budget;
            self
        }
    }

    impl SealingKey {
        pub fn new(key_material: &[u8; KEY_LEN]) -> Self {
            Self(Key::new(key_material), Budget::default())
        }

        /// `plaintext_in_ciphertext_out` is `packet_length || plaintext`.
        pub fn seal_in_place(
            &self,
            sequence_number: u32,
            plaintext_in_ciphertext_out: &mut [u8],
            tag_out: &mut [u8; TAG_LEN],
        ) {
            // russh's sealing can't fail: past the budget, stop rather than
            // reuse a nonce. Unreachable while russh rekeys on schedule.
            assert!(
                self.1.spend().is_ok(),
                "chacha20-poly1305: this key has sealed its share of packets"
            );
            #[allow(clippy::unwrap_used)] // russh always passes the length
            let (length, data) = plaintext_in_ciphertext_out
                .split_first_chunk_mut::<PACKET_LENGTH_LEN>()
                .unwrap();
            self.0.crypt_length(sequence_number, length);
            #[allow(clippy::unwrap_used)] // only fails past ChaCha20's limits
            let tag = self
                .0
                .k_2
                .encrypt_inout_detached(&nonce(sequence_number), length, data.into())
                .unwrap();
            *tag_out = tag.into();
        }
    }

    pub struct OpeningKey(Key, Budget);

    impl OpeningKey {
        #[doc(hidden)]
        pub fn with_budget(mut self, budget: Budget) -> Self {
            self.1 = budget;
            self
        }
    }

    impl OpeningKey {
        pub fn new(key_material: &[u8; KEY_LEN]) -> Self {
            Self(Key::new(key_material), Budget::default())
        }

        /// The packet length, decrypted but not yet authenticated.
        pub fn decrypt_packet_length(
            &self,
            sequence_number: u32,
            encrypted_packet_length: [u8; PACKET_LENGTH_LEN],
        ) -> [u8; PACKET_LENGTH_LEN] {
            let mut length = encrypted_packet_length;
            self.0.crypt_length(sequence_number, &mut length);
            length
        }

        /// `ciphertext_in_plaintext_out` is `encrypted_packet_length ||
        /// ciphertext`; the plaintext after the length, once authentic.
        pub fn open_in_place<'a>(
            &self,
            sequence_number: u32,
            ciphertext_in_plaintext_out: &'a mut [u8],
            tag: &[u8; TAG_LEN],
        ) -> Result<&'a [u8], Unspecified> {
            self.1.spend()?;
            let (length, data) = ciphertext_in_plaintext_out
                .split_first_chunk_mut::<PACKET_LENGTH_LEN>()
                .ok_or(Unspecified)?;
            self.0
                .k_2
                .decrypt_inout_detached(
                    &nonce(sequence_number),
                    length,
                    data.into(),
                    &(*tag).into(),
                )
                .map_err(|_| Unspecified)?;
            Ok(data)
        }
    }
}
