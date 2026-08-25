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
* **An unrecognised build hard-fails with instructions -- no silent fallback.**
  When native is the default (no explicit `--delivery`) and it cannot attach and
  validate the image, the client **stops with a clear, actionable error**: it
  tells the player the build was not recognised and to load the Cheat Engine
  table and re-run with `--delivery=ce-bridge`. It does **not** silently fall
  back to the bridge: with native as the default the CE table will not be
  loaded, so a file-drop grant would sit unconsumed and delivered items would
  vanish. An **explicit** `--delivery=native` also fails closed with a clear
  error, unchanged. `--delivery=ce-bridge` still forces the bridge directly and
  is the remedy the hard-fail points to. The fail-closed image check,
  no-double-grant, install atomicity and image-mismatch guards are unchanged;
  only which backend is default changed. A safe, detected fallback (a liveness
  handshake that confirms a loaded CE table before offering the bridge) is
  tracked as the successor in clients#413.

  The runtime contract is consumed from a vendored copy of the world repo's
  `bb-native-grant-contract.v5.json` (`src/native/contract.rs`), so no hook-site
  or routine address is hand-copied, and a drift between the copy and the crate
  constants fails a unit test.

  **Untested against a live game.** The pure logic is host-tested; the live
  Windows attach/install/thread seams are CI-compiled only and await owner
  validation. See `README.md` and `src/native/`.
