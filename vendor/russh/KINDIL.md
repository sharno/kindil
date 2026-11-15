# russh 0.65.1, vendored by kindil

[russh](https://github.com/warp-tech/russh) 0.65.1 as published on crates.io
(Apache-2.0), used by kindil-sshd (`sshd/`) through `[patch.crates-io]` in the
workspace's Cargo.toml. Kept: `Cargo.toml` (crates.io's normalized one),
`README.md` and `src/`; dropped: examples, tests, benches and their
dev-dependencies.

## Why

russh needs a C crypto library (`ring` or `aws-lc-rs`), but only for its
AEAD ciphers; everything else it does on RustCrypto. kindil builds without C,
so this copy adds a third backend, the `rustcrypto` feature: the AEADs on
RustCrypto's `aes-gcm` and `ssh-cipher` (already in russh's tree, through
`ssh-key`).

## The changes

- `src/cipher/rustcrypto.rs` (new): the part of ring's AEAD API that
  `gcm.rs` and `chacha20poly1305.rs` use, for `aes128-gcm@openssh.com`,
  `aes256-gcm@openssh.com` and `chacha20-poly1305@openssh.com`.
- `src/cipher/{mod,gcm,chacha20poly1305}.rs`: with neither `ring` nor
  `aws-lc-rs`, import from it instead.
- `src/lib.rs`, `src/helpers.rs`: `rustcrypto` counts as a backend.
- `src/cipher/mod.rs`: no dead-code warning without `flate2`.
- `Cargo.toml`: the `rustcrypto` feature and its two optional dependencies;
  examples, tests, benches and dev-dependencies removed.

`diff -r` against `~/.cargo/registry/src/*/russh-0.65.1/src` shows exactly
these. To update: copy a newer release's `src/` over, reapply the import
switches (each marked by `rustcrypto`) and re-run kindil-sshd's tests and its
OpenSSH check (each cipher with `ssh -c`).
