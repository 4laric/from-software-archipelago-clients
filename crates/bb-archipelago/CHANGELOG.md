## Unreleased

### Fixed

* **A game that has not loaded a character no longer stops the client, and
  never gets told its build is unrecognised (clients#420).** Immediately after
  clients#418 landed, attach got past the base wait and image validation and
  then died on `Bloodborne event-flag manager is not initialized` -- wrapped in
  the unrecognised-build guidance, which points at a Cheat Engine lane that
  provides no flag reads either. The manager is a guest global that stays null
  until the game is further into boot (plausibly only once a save is loaded),
  so this was a third startup-ordering race, not a bad build. The flag half now
  arms **lazily**: attach succeeds with native item delivery armed and the flag
  gate pending, and the client loop -- which already polls every tick -- arms it
  the moment the manager appears. While pending, `read_event_flag` answers
  "no accessor" (never "the flag is false") and the context reports
  not-gameplay-ready, which is the *existing* send-gate shape
  (`require_runtime_context` -> no checks, no sends); no parallel gate was
  invented, and no check can be missed by waiting, because checks cannot fire
  before gameplay anyway. Exactly two lines are printed, ever: one when the wait
  begins and one when checks arm. A signature mismatch or a vanished process is
  still terminal. And the routing now fails safe from both ends: the
  not-initialized state is a distinct error type, exempted from the
  unrecognised-build/ce-bridge guidance by the same mechanism as the
  clients#418 stale-log case (clients#416).

* **Native attach now waits for the game instead of racing it (clients#418).**
  The launcher starts shadPS4 and the client at the same moment, and the shad
  log is appended across runs, so reading it once at startup handed attach the
  **previous** run's eboot base: verification read an unmapped page and the
  client exited immediately -- blaming the player's game build for what was
  purely an ordering problem. Attach now records the log length at start, tries
  the base already in the log (live verification, not the file, decides whether
  it is current), and if that base cannot be confirmed it waits on a 1s poll
  with a 90s budget, accepting only a base line written **past** the recorded
  offset -- a stale line can never satisfy the wait. One `Waiting for shadPS4 to
  load the game...` line is printed when the wait begins, not per poll. The
  terminal messages are now distinct: no fresh base line ever appeared names the
  configured `shad_log` path and says what to compare it against (portable
  `user\log\shad_log.txt` beside the exe vs the `%APPDATA%` one) **without**
  mentioning the Cheat Engine bridge, while a confirmed base whose image fails
  validation keeps the existing unrecognised-build guidance. The live event-flag
  attach is handed the confirmed base instead of re-reading the log, so it
  cannot re-run the same race. Fail-closed behaviour is unchanged: nothing is
  written until a base is both confirmed and validated.

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
