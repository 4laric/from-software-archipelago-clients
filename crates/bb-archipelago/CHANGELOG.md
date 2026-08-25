## Unreleased

### Features

* Add an experimental in-process **native delivery backend** (stage 2 of the
  CE-free client), selected with `--delivery=native`. It attaches to shadPS4,
  verifies the running image against the vendored `bb-native-grant-v5` contract,
  installs the grant payload with a thread-suspend atomicity protocol, and
  drives the native grant state machine. The Cheat Engine file bridge remains
  the default (`--delivery=ce-bridge`); native selection fails closed on any
  image mismatch and never silently falls back.

  The runtime contract is consumed from a vendored copy of the world repo's
  `bb-native-grant-contract.v5.json` (`src/native/contract.rs`), so no hook-site
  or routine address is hand-copied, and a drift between the copy and the crate
  constants fails a unit test.

  **Untested against a live game.** The pure logic is host-tested; the live
  Windows attach/install/thread seams are CI-compiled only and await owner
  validation. See `README.md` and `src/native/`.
