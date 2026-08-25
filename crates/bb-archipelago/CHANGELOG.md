## Unreleased

### Features

* **Native delivery is now the default backend.** With no `--delivery` flag the
  client uses the in-process native path (stage 2 of the CE-free client): it
  attaches to shadPS4, verifies the running image against the vendored
  `bb-native-grant-v5` contract, installs the grant payload with a thread-suspend
  atomicity protocol, and drives the native grant state machine. Defaulting to
  native is bounded by its fail-closed image check: `require_validated_image`
  refuses CUSA00900 and every other serial/build, so only a
  recognised-and-validated image is ever patched.
* **Automatic CE-bridge fallback on the default path.** When native is the
  default (no explicit `--delivery`) and it cannot attach and validate the image,
  the client logs why, loudly, and falls back to the Cheat Engine file bridge
  instead of hard-failing the player. An **explicit** `--delivery=native` keeps
  the strict behaviour and fails closed with a clear error -- it does not fall
  back, because the user asked for native. `--delivery=ce-bridge` still forces
  the bridge directly. The fail-closed image check, no-double-grant, install
  atomicity and image-mismatch guards are unchanged; only which backend is
  default, plus the safe default-path fallback, changed.

  The runtime contract is consumed from a vendored copy of the world repo's
  `bb-native-grant-contract.v5.json` (`src/native/contract.rs`), so no hook-site
  or routine address is hand-copied, and a drift between the copy and the crate
  constants fails a unit test.

  **Untested against a live game.** The pure logic is host-tested; the live
  Windows attach/install/thread seams are CI-compiled only and await owner
  validation. See `README.md` and `src/native/`.
