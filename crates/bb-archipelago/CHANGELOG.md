## Unreleased

### Fixed

* **A terminal startup error now prints through the tee (clients#437).**
  `fn main() -> Result<()>` handed any bubbled `Err` to Rust's default
  termination handler, which prints `Error: {err:?}` onto the process's real
  stderr. Since clients#426 the client owns its own tee and the launcher no
  longer pipes the child's stderr, so that line reached neither `client.log`
  nor the launcher's early-exit dialog: the log stopped mid-startup and the
  dialog showed exit code 1 over a healthy-looking tail. Badgerous hit this
  live -- `verify_suppression_install` refused an installed binder and the
  reason evaporated.

  The run moved into `fn run() -> Result<()>`; `fn main()` prints the full
  anyhow chain via `client_eprintln!` in the same text the default handler
  produced (`Error: ` plus the `Caused by:` chain) and then exits 1. Nothing
  double-prints: no path both prints an error and returns it. Unchanged
  residual: a failure inside `arguments()` happens before the log path is
  known and can still only reach the console.

### Fixed

* **Existing-stack grants go through the cave, not an external write
  (clients#433).** oz's fresh seed parked consumables intermittently as
  `write_error: tag=ap_N quantity write failed` -- the same item type
  delivering at one AP index and failing at the next. `GrantSession` took a
  `direct_write` branch whenever the stack already existed, and that branch
  issued an external `WriteProcessMemory` into the guest *inventory heap*.
  bb-archipelago#144 established with two live repros that shadPS4
  protection-tracks those pages: the write fails intermittently
  (read-ok/write-ok/read-FAIL microseconds apart) and can wound the emulator.
  The CLI tool was moved off it in bb-archipelago#145; the client's port kept
  the branch. It is deleted. An existing stack now takes the contract's
  existing-stack delta lane -- `state_cells.request = 2`, `quantity` read as a
  DELTA by `native_routines.quantity_delta` (`edx = delta`), addressed by
  `slot_index` and `item_quantity_pointer` -- which runs on the game thread.
  No contract change: value `2` and the routine are already validated in v5.
  Eboot-image writes (the caves and the state cells) are unaffected; only
  inventory-heap writes were ever the hazard.

  Two consequences that are tested, not assumed. The verify no longer accepts
  the reported slot as a witness on the delta lane -- a stack of 5 owed 2 more
  already satisfies "the slot holds at least `quantity`" *before* the delta
  lands -- so only the read-back total completes it. And an existing stack with
  no record pointer now parks as `write_error (... quantity pointer missing)`
  rather than falling back to a write, because the fallback is what was
  removed.

* **Parks caused by that refused write redeliver on startup (clients#433).**
  The startup unpark (clients#427) generalises from `quantity_mismatch` to
  "every park whose cause is known to be fixed", which is now also a
  `write_error` whose detail ends `quantity write failed`. Every other park
  stays put for `bb-blocked` -- including `write_error (... quantity pointer
  missing)`, a different cause. `SlotLedger::requeue_quantity_mismatch_parks`
  is renamed `requeue_fixed_cause_parks` to say what it does.

* **The shipped binary actually calls the live-stack observation
  (clients#427).** `observe_stack_quantity` (clients#428) and
  `grant_may_have_applied` (clients#429) were implemented on `NativeBackend`
  but never forwarded by the `Backend` enum in `main.rs`, which is what every
  dispatch in the real client goes through. Both fell to their silent trait
  defaults -- `Unsupported` and `true` -- so the native implementations were
  dead code in the exe and every fresh grant kept using the ledger-sum
  baseline. That is why oz's clients#428 build still parked with a climbing
  `expected_before` (10/12/14/16) against an actual stack of 2. The tests were
  green because they exercise `MockBackend` directly and never the enum. Both
  methods are now forwarded, both are *required* trait methods (implemented
  explicitly on `FileBackend` with the CE bridge's documented behaviour), and a
  regression test dispatches through `Backend::Mock` so the wrapper itself is
  witnessed.

* **A retained grant command re-observes its baseline instead of comparing a
  stale one (clients#427, follow-up to clients#428).** oz ran the clients#428
  build and the requeued backlog re-parked as `quantity_mismatch`: pebbles
  `expected_before=5 actual=20`, bullets `expected_before=18 actual=17`.
  The premise "the baseline is sampled at enqueue time" is refuted: the client
  observes at the *head of the queue*, in the same call that publishes the
  command, with one command in flight at a time
  (`client_loop.rs`, the observation immediately precedes `grant_item`). The
  stale number comes from afterwards. A published command is not always an
  executed one: the native machine can *retain* it in `awaiting_inventory` --
  the state whose own operator message asks the player to go acquire a stack of
  the item -- for an unbounded number of polls. The baseline sampled before that
  wait was frozen, so everything the player did during it (spending a bullet,
  buying twenty pebbles) read back as a mismatch and parked the item.
  A backend now reports whether the command for a tag *may already have
  applied*. The recorded baseline is binding only while that is true; while the
  command is merely retained the next publication re-observes the live stack and
  records the fresh number durably before publishing. The replay contract is
  unchanged: a command that may have applied -- anything past `queued` /
  `awaiting_inventory` / `busy`, including a durable prior restored after a
  restart -- keeps its recorded number, so `recovered_complete` still decides a
  restart mid-grant instead of double-granting. Backends that cannot tell (the
  CE file bridge) keep the previous freeze-on-first-observe behaviour.
  Residual and stated rather than papered over: the observation and the
  execution are one poll tick apart at most (the same `poll_items` call
  publishes the command the machine then evaluates), so a player who spends in
  that window can still park an item. What is closed is the *unbounded* window
  -- a baseline no longer survives a wait of arbitrary length.
  The startup requeue from clients#428 covers today's re-parks unchanged: they
  carry the same `quantity_mismatch` park status, so they re-enter the delivery
  queue on the next launch and deliver against a freshly observed stack.

### Added

* **The client tees its own output: console AND `--log-file` (clients#425).**
  New optional argument `--log-file <path>` (the joined `--log-file=<path>` form
  works too). Given one, the client opens it for append as the first thing after
  argument parsing -- before any other output -- stamps
  `\n=== SESSION START YYYY-MM-DD HH:MM:SS UTC ===\n`, and from then on every
  line it prints goes to *both* the real console and that file, flushed per line
  so a crash cannot lose the tail. Without the flag nothing changes: no file is
  opened, no path is touched, and the console output is byte-identical to
  before.
  This puts the split where it belongs. bb-archipelago#171/#172 captured the
  client's output by redirecting its handles into `<session>/client.log`, which
  blanked the console window players watch to see what arrived;
  bb-archipelago#179 bought the console back by teeing in the *launcher*, with a
  pipe and a pump thread. Teeing here means the client keeps a real inherited
  console, no cross-process relay sits in its output path, and a client started
  by hand outside the launcher still writes a log. The header shape is pinned to
  what bb-archipelago's `read_session_log_tail` slices on, so the launcher's
  early-exit dialog keeps showing exactly this session.
  Mechanically: a `logging` module owning a process-wide tee sink, and a
  `client_eprintln!` macro that replaces `eprintln!` at all 40 of this client's
  printing sites. No unsafe, no file-descriptor games.
  Residual, accepted and deliberate: a failure *during* argument parsing happens
  before the log path is known, so it can only reach the console.
  Pairs with bb-archipelago#181, which makes the generated plan pass
  `--log-file {client_log}` and stops the launcher redirecting that entry. The
  two merge independently: the plan pins the client by SHA-256, so plan and exe
  always travel together.

* **The client reconnects to Archipelago by itself (clients#423).** An
  archipelago.gg room closes its port while it sits idle, so a session that
  paused for lunch came back to a client that had silently stopped talking to
  the server: `Connection`'s `Disconnected` state is terminal by contract, and
  nothing here ever left it. The loop now watches for it and rebuilds the
  connection -- a fresh `Connection::new`, exactly the way shared
  `Core::reconnect()` does for Elden Ring/DS3/Sekiro; no archipelago-rs change
  was needed or made. Backoff is 5s, doubling to a 60s cap. Messaging follows
  the clients#404/#415 once-then-quiet shape: one line when retrying begins,
  naming the address and the sleeping-room remedy ("open the room's page in
  your browser to wake it"), a one-line reminder about once a minute while it
  lasts, and `Connected to Archipelago.` on recovery -- printed by the existing
  `Event::Connected` arm, so it is never doubled. Nothing is lost while
  offline: the flag poll re-detects checks every tick and derives what to send
  from the *server*-checked set that a reconnect's `sync()` refreshes, so a
  check found offline is re-sent, and owed items deliver from the ledger cursor
  as usual.
  Two things are deliberately NOT retried:
  * **A rejected login is terminal and loud.** Every
    `Error::ConnectionRefused` reason -- wrong slot, wrong game, wrong
    password, version mismatch, bad ItemsHandling, or an unknown reason the
    server named -- stops the client with one actionable line. Retrying a
    rejected login forever would only hide the setting the player has to fix.
    Transport failures (`WebSocket`, `Async`/IO, a dropped socket, an
    interrupted connect) are the retryable set.
  * **A reconnect that lands on a different seed is refused.** If the host
    regenerated the multiworld while the client was offline, the reconnect's
    slot data carries a new seed name while the runtime and the receive ledger
    are still bound to the old one. Continuing would deliver the new seed's
    items against the old seed's delivery cursor, so the client stops and names
    both seeds. A same-seed reconnect changes nothing: the runtime is not
    re-created and no delivery state is reset, so nothing double-delivers.

* **A one-line startup banner so a working console is legibly alive
  (clients#404 companion).** On a normal launch the client streams its
  diagnostics to the console but, between the build line and the long silent
  attach/connect waits, a healthy run looked identical to a frozen or dead one
  -- a playtester (oz, 2026-08-24) saw the black console window and assumed his
  run was broken. The client now prints exactly one at-a-glance line at startup,
  on every launch and never gated behind an error path:
  `bb-ap-client running - delivery: <native|ce-bridge> - server: <host:port> - slot: <slot> - diagnostics stream to this console`.
  The delivery label is derived from the *resolved* `DeliveryMode`, so it stays
  correct whichever default is in effect (the clients#412 native-default flip).
  This is the silent-success companion to clients#404's noisy-failure line: it
  answers "is it working / where do I look" up front.

### Fixed

* **A consumable you have spent no longer parks every later copy of it
  (clients#427).** In oz's live session the first deliveries landed and then 21
  items parked, all `quantity_mismatch: expected_before=18 actual=0` --
  quicksilver bullets, blood stone shards, molotovs. The fresh-grant
  precondition was the receive ledger's *lifetime delivered sum* for that item,
  which the delivery machine required the live stack to equal. For anything the
  player spends, actual is below that sum the moment one is used, so every
  further grant of that item parked, permanently, while unspent item types kept
  arriving.

  The precondition for a **new** command is now the quantity observed in the
  game at submit time: the client reads the stack (`observe_stack_quantity`),
  records it durably on the pending plan as `observed_before`, and delivers the
  delta against that. Double-delivery protection is untouched -- it has always
  lived in the ledger's index cursor, which delivers each AP index at most once,
  never in this arithmetic.

  **Replay recovery is unchanged and still durable.** The baseline is written to
  the ledger *before* the grant can execute, so a restart mid-grant replays the
  one uncommitted in-flight command against the very same number and an
  already-applied stack is still recognised as `recovered_complete` instead of
  being granted twice. What changed is where that number comes from, not what it
  protects. A command the client proves was withdrawn unexecuted (a load
  transition, a save switch) releases its recorded baseline, so a player who
  spends during that window is not parked on a stale number when it re-publishes.

  Fail-closed details: no grant is published off an inventory that has not
  hydrated, and an absent stack must survive the contract's `min_absent_polls`
  grace before zero is accepted as a baseline -- a hydration lie must never be
  recorded as one. A backend that cannot read inventory (the Cheat Engine file
  bridge, whose harness ignores the field anyway) keeps the ledger-derived
  baseline it always used.

* **Items already parked as `quantity_mismatch` re-deliver by themselves
  (clients#427).** On startup, every ledger entry parked with that status
  re-enters the delivery queue, so oz's 21 parked items deliver instead of
  needing 21 manual `bb-blocked` invocations. Only that reason auto-unparks:
  it is the one whose cause this release removed. `failed`, `write_error` and
  `command_rejected` parks stay parked for `bb-blocked`, because nothing here
  fixed them. A requeued index is delivered before anything new, retiring it
  never moves the delivery cursor backwards, and nothing already delivered is
  delivered again.

  Ledger format: `PendingItem` gains `observed_before` and `SlotLedger` gains
  `redeliver`, both `#[serde(default)]`. An older ledger loads unchanged -- an
  absent `observed_before` simply means "sample on the next poll" -- and a
  ledger with an empty requeue set serializes exactly as before.

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
