How to contribute to tawqit
===========================

Thank you for considering contributing!

Reporting issues
----------------

- Describe what you expected to happen, with the place, date and method.
- Describe what actually happened.
- A prayer time that differs from an authority's table isn't always a bug:
  say which table, and check it against Adhan's times for the same method.

Submitting patches
------------------

- A bug fix comes with a test that fails without it
  (`tests/regressions.rs`, or next to the code).
- Keep the API total: a new input gets a checked type with one fallible
  constructor; a new outcome gets a variant, not a silent fallback.
- Integer logic after the astronomy goes in `src/verified.rs`, with its
  Verus spec and proof; a new float-to-integer step gets a Kani harness
  showing it meets the core's preconditions (VERIFICATION.md).

Checks
------

    cargo fmt --all --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo test --release --all-features
    verus src/verified.rs --crate-type=lib      # Verus 0.2026.11.02.8bf68ee
    cargo kani -Z stubbing                       # Kani 0.68.0

CI runs all of them (`.github/workflows/ci.yml`).
