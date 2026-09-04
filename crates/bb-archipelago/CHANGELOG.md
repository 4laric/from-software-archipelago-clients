## Unreleased

### Changed

* **A refused insert re-plans as a delta instead of parking (clients#613).**
  A playtester's Antidotes parked as `failed (tag=ap_43 expected_after=2
  actual=Some(10) native_result=4294967295 retry_budget=20)`: the dequeue-time
  scan saw no stack, the insert lane was taken with a zero baseline, and by
  verify time a stack of ten (the pouch cap) was there and the game refused
  to create a second one. The result cell never left the sentinel, so the
  grant provably did not apply. The machine now re-plans that shape once, as
  a delta against the stack that exists, and lets clients#443's overflow rule
  complete it if the pouch is capped. A second refusal parks as before. The
  delivery diagnostics record two new fields, `stack_present_at_dequeue` and
  `replanned_to_delta`, so a scan false-negative is distinguishable from a
  storage withdrawal next time. On startup, a goods-lane `failed` park whose
  `native_result` is the sentinel re-enters the queue; every other `failed`
  park still stays put.

* **Receive-ledger rotation now survives brief Windows file locks
  (clients#560).** Backup removal, rotation, publication, and rollback retry
  only sharing/lock violations for a bounded 250 ms window and report every
  retry. Other I/O failures still stop immediately, and an exhausted lock can
  no longer be mistaken for a successful save.

* **Goal completion is now witnessed, durable, and unmistakable.** Only a new
  configured goal check from the validated positioned-save poll sends
  `ClientStatus::Goal`. The seed/slot ledger prevents duplicate sends or stale
  restart celebrations, while the GUI and `victory-summary.txt` retain a
  restrained victory summary and label unavailable counters as unknown.

* **Compact mode now shows real Archipelago pickup toasts.** Sent checks name
  the scouted item and recipient; completed deliveries name the item and
  sender. Up to three cards appear newest-first for four seconds and fade
  without touching check, grant, or acknowledgement semantics. Full mode keeps
  the same information in its activity feed instead of drawing it twice.

* **An opt-in passive research probe pack can capture the remaining beta
  unknowns without changing game state.** It records reviewed boss-flag flips,
  changed inventory descriptors for rune research, pickup-presentation markers,
  and time spent in each client-readiness state. Diagnostics export embeds the
  bounded capture files. Insight correlation remains disabled until a reviewed
  offset manifest exists; the client refuses to guess addresses.

* **Bloodborne auto-upgrade now recognizes randomized Uncanny weapons as the
  player's upgrade target.** The native census still uses an exact weapon
  allowlist, but includes the catalog-backed Uncanny families and reports an
  unrecognized/empty census as unknown rather than a misleading +0. A
  production-loop regression carries a held Whirligig Saw +7 across families
  to a received Ludwig's Holy Blade.

* **The standalone status window now shows named run progress.** The goal label
  comes from Archipelago's typed game data rather than exposing a numeric
  location ID, and the window shows exact checked/total locations from the
  seed-owned runtime contract. Go Mode remains explicitly unknown until the
  client has a trustworthy readiness signal.

* **Native-only delivery (clients#526).** Removed the live command-file backend,
  bridge-root configuration, startup probes, and backend selector. Legacy
  `--delivery=ce-bridge` launch plans now stop with an explicit migration error.
  Unsupported images continue to fail closed before delivery is armed and now
  direct players to the session diagnostics bundle instead of external tooling.

* **The standalone command window is keyboard accessible.** Enter submits the
  command field through the same bounded worker channel as the Send button;
  Tab and Shift+Tab traverse every interactive control, and Alt-key mnemonics
  activate Send, Status, diagnostics export, and the session folder.

* **The zero-Vial diagnostic now captures the actual record-creation
  transition.** On the first canonical Vial row it records the chosen slot,
  neighboring before/after bytes, and inventory tail movement. Delivery remains
  fail-closed; this is the read-only evidence needed before implementing an
  absent-stack bootstrap.

* **Windows launches now include a native standalone Bloodborne status
  window.** Distinct shadPS4, Archipelago, and delivery readiness is fed by the
  nonblocking UI bridge. The translucent, always-on-top shell never touches the
  emulator process or delivery acknowledgement path.

* **The opt-in pickup-notification diagnostic now probes the two vanilla
  pickup-side call edges found in playtest.32.** Exact-byte guarded,
  observation-only wrappers at callers `0x17D93FE` and `0x14DA9FF` record
  entry/return and candidate presentation context without touching the client
  grant caller, changing item delivery, or synthesizing a message.

* **An opt-in pickup-notification correlation probe is ready for live mapping.**
  Setting `pickup_notification_probe` in the local runtime config writes a
  bounded `pickup-notification-capture.jsonl` that joins AP checks and delivery
  states to native ItemGrant caller RVAs. It is observation-only and leaves
  banners, item transactions, suppression, and acknowledgement untouched.

* **Randomized fixed pickups now sustain exploration with one Quicksilver
  Bullet.** Their suppressed ItemLot already supplies one Blood Vial; after a
  new check is sent, the client queues the matching Bullet through the native
  grant machine. Completion is save-bound and keyed by AP location, so polling,
  reconnects, and interrupted commands cannot create historical or duplicate
  awards, and sustain failure never blocks the randomized item.
* **Bloodborne now understands the seed-owned DeathLink-amnesty contract.**
  Its local-death cycle is stored in the per-seed ledger so reconnecting or
  relaunching cannot reset it. The policy remains dormant while Bloodborne is
  receive-only, pending validation of the local-death signal.

* **Blood-gem diagnostics no longer mislabel armor as gems.** ItemLot category
  8 is a generation recipe, not a prefix encoded into a runtime inventory id.
  The replacement diagnostic takes bounded, read-only snapshots of the live
  inventory manager and its immediate pointer blocks, allowing natural gem
  pickups to reveal their separate container without guessing or writing to
  guest memory.

* **Routine item acknowledgements no longer flood the player console.** The
  detailed per-index acknowledgement lines remain in `client.log` for support
  and replay diagnosis; blocked deliveries, failures, and recovery notices
  remain visible in the console.
* **Bloodborne can receive DeathLinks when the seed opts in.** Runtime r9
  advertises the `DeathLink` tag only for enabled slots, queues incoming links
  across loads, and kills through a validated current-HP write once the native
  HP hook has captured a gameplay-ready player. The hook, cave, and mutation
  all fail closed on an unrecognized image or stale context. Sending remains
  disabled until the separate live death-signal hunt proves a safe trigger.

* **A read-only shop-enablement diagnostic is ready for the next playtest.**
  Native sessions write `shop-capture.jsonl` beside the ledger, recording exact
  inventory-row transitions and five-second snapshots of the Hunter Chief
  Emblem, workshop tools, and hunter badges. A badge acquisition followed by a
  natural shop purchase now gives us the live evidence needed to map stock
  unlocks and purchase descriptors without modifying the save.

* **A read-only zero-Blood-Vial diagnostic is ready for bb-archipelago#70.**
  Native sessions sample Vial-shaped inventory rows every five seconds into
  `blood-vial-capture.jsonl`, distinguish the known shop/HUD low-ID collision
  from a canonical stack, and capture the backing object when a natural world
  pickup creates that stack. The unsafe absent-stack insertion remains refused.

* **The Bloodborne client window is visibly translucent by default.** Its
  console now starts at 70% opacity and prints the applied value at startup.
  If Windows Terminal owns the visible window instead, the client says so
  rather than silently claiming success. `--window-opacity 35-100` tunes it;
  100 keeps the traditional fully opaque window.

* **Normal play now captures storage-routing correlations (clients#445).** Each
  delivery diagnostic records its terminal sequence number, the millisecond gap
  from the preceding grant, and the preceding destination inference. The
  summarizer prints an Oz verification shortlist containing suspected storage
  deliveries, the grants immediately following them, and insert-lane result 2
  cases. This is passive instrumentation only; delivery timing and routing are
  unchanged while the hypothesis is still being measured.

### Fixed

* **Confirmed insert-lane storage deliveries are named consistently.** When
  ItemGrant completes but the new goods record does not enter held inventory,
  the console directs the player to the Hunter's Dream storage box and the
  diagnostic record now says `storage`, matching the player-validated outcome.
  Delta-lane deficits remain `storage_suspected` because a concurrent spend is
  arithmetically indistinguishable from overflow.

* **Goal-release item floods are paced instead of hammering Bloodborne's
  inventory routine.** A live 71-item capture showed stable held delivery at
  ordinary cadence, then storage routing after more than twenty grants arrived
  roughly 130–170 ms apart. Successful grants now wait one second before the
  next item is submitted, trading a short release-drain time for predictable
  inventory placement.
* **Zero-Vial diagnostics no longer report executable code as a backing
  object.** Blood Vials are ordinary stackable goods, not generated instances;
  their canonical inventory-row transitions remain captured, while the
  generated-object resolver is now reserved for blood gems and weapons.

* **Location checks retry until the server confirms them (clients#455).** A
  socket killed without a detected transport error could accept a local write,
  after which archipelago-rs's optimistic checked-location cache made the
  Bloodborne loop believe the server knew about the check. The loop now reads
  a distinct server-confirmed view and harmlessly resends `LocationChecks`
  until a `RoomUpdate` acknowledges them. Retries use a per-location
  1/2/4/8/16/30-second capped backoff, reset on reconnect, so a zombie socket
  cannot produce a 20-message-per-second retry loop. Relaunching is no longer
  required to recover checks sent into a zombie connection.

* **Equipment never takes the delta lane (clients#451).** The first
  `delivery-diagnostics.jsonl` from the field showed `ap_7` Hunter Pistol
  delivered as `delta persistent ... storage_suspected`: the player already
  owned one, the inventory scan matched the owned weapon's record, and the
  native call added the grant quantity into a field that is not a quantity for
  an equipment instance record -- a possible corruption of the weapon the
  player already had. The delivery machine now chooses the lane by item
  CATEGORY first and inventory contents second. Category 4 (goods) stacks and
  may delta when a matching stack exists; category 0 (equipment, and armour
  when it arrives) ALWAYS inserts, however many matching records the scan
  finds, and the owned record's slot and pointer are no longer handed to the
  cave. A duplicate weapon is a second INSTANCE -- the point of the Uncanny
  design, and equally true of a plain duplicate. The category is derived from
  the raw/normalized descriptor pair (the same two pairings the request
  validator checks against the declared category); an unrecognised pair fails
  closed to *not stackable*, because guessing "stack" is the failure this
  entry describes.

  Verification for an instance insert is stated honestly: **instance-count
  read-back is not available** to this client (`find_stack` returns the first
  record matching the id, and its quantity position is not an instance count),
  so the count checks are SKIPPED for that lane rather than parking a
  delivered weapon on arithmetic that means nothing. The witness is the cave's
  state cell (the routine provably ran) plus a read-back of the slot the
  routine reports, which must hold our normalized id. A reported slot holding
  a different id is a contradiction and still fails. Consequently an equipment
  grant publishes no `observed_before`/`expected_after` in the diagnostics
  record, and its `inferred_destination` is `unknown` -- no count arithmetic
  is published that could be mistaken for a measurement. Quantity-based replay
  recovery (`recovered_complete`) does not apply to an instance insert either;
  a replayed equipment grant is guarded by the ledger's index cursor, as
  before.

  The separate half of clients#451 -- the unpopulated `r8`/metadata argument
  that makes inserted weapons arrive at 0 durability and crashes the fortify
  station -- is NOT addressed here and needs a live capture at a vanilla
  weapon pickup.

### Fixed

* **`param_id_inferred` descriptor evidence, and a forward-compatible parse.**
  A seed generated by a bb-archipelago #208-era world bound weapons with a
  third evidence value, `param_id_inferred` -- param ids documented by two
  independent sources but not yet witnessed live. The Rust enum knew two
  values, so the client exited at startup with `parsing
  slot_data.runtime_items -- unknown variant`, and the seed was unplayable.
  `param_id_inferred` is now accepted and **delivers exactly like the other
  two**: provenance strength is bookkeeping, not behavior. When a seed carries
  any such binding the client prints one line at slot-data parse -- `N item
  binding(s) carry inferred param ids; first live delivery of each is its
  validation.` -- so the operator can see the promotion surface.

### Changed

* **An unknown descriptor evidence no longer kills the session.** A slot-data
  enum is a two-repo contract, and the world can grow a variant before any
  client build knows it. Previously that skew was fatal at parse: one unknown
  string on one binding and no part of the seed was playable. The evidence
  value is now carried verbatim as an unknown, the session arms, and the
  *individual* binding is refused at delivery -- parked, naming the evidence
  string it did not understand and telling the operator to update the client.
  Fail-closed per item instead of per seed. Nothing is granted for a refused
  binding, and the stream continues with the next index.

### Added

* **Passive per-grant delivery diagnostics (clients#445).** On the native path
  every grant that reaches a terminal outcome -- completed, completed with the
  clients#443 concurrent activity, parked, or recovered -- appends one JSON line
  to `delivery-diagnostics.jsonl` in the session folder, beside `ledger.json`
  and `client.log`. The operator flow is the whole point: play normally, then
  send that file the way you send `client.log`. There is no probe step, no flag,
  and no new argument -- the path is derived from the ledger path the client is
  already given, because the launcher already puts all three files in one
  folder. Each line carries only values the delivery machine already computed
  for a decision it already makes: tag and AP index, raw and normalized item id,
  lane (`insert`/`delta`) and descriptor source, quantity, the observed
  baseline, the expected total, every held-stack read-back the verify loop saw
  (first and most recent sixteen, with the true count), the cave's result cell
  and whether it constitutes clients#443 execution evidence, the verify poll
  count, the terminal status and detail, and the client's own gameplay-ready and
  flag-gate state at submit and at the terminal step. **No new read of the game
  is performed for any of it**, and nothing in the state machine branches on the
  record. A failure to write warns exactly once and is then silent: a diagnostic
  that can park a delivery is worse than no diagnostic, and there is a test that
  refuses every write and asserts the item is delivered anyway.

  One field is an inference and says so in its name. `inferred_destination` is
  `held` when the read-back arithmetic accounts for the delta in the held stack
  (a clients#443 surplus included -- the stack still absorbed the grant),
  `storage_suspected` when the cave provably executed and the held stack came in
  *under* the expected total, and `unknown` otherwise. That deficit shape is
  what a capped Bloodborne pouch overflowing into storage produces -- and what a
  concurrent spend in the same unobservable window produces too. The client has
  no read of the storage box and cannot separate them, which is why the value
  says *suspected*; these counts must never be reported as measured storage
  routing.

  This complements the manual probe in bb-archipelago#203 rather than replacing
  it. Controlled-condition questions -- a unique-item insert, a deliberately
  at-cap arming -- still belong to the probe, because those conditions do not
  arise on their own during play. This answers the one the probe cannot: the
  distribution across a real session. `tools/summarize_delivery_diagnostics.py`
  groups the records by item, terminal status and inferred destination and
  prints a table to paste into clients#445.

### Fixed

* **Concurrent inventory activity (either direction) during a delta grant no
  longer parks a delivered item (clients#443).** A playtester's `ap_0` parked as
  `failed (tag=ap_0 expected_after=7 actual=Some(8) native_result=8 retry_budget=20)`.
  The delta lane's read-back verification demanded exact equality with
  `expected_after`, and the player -- actively looting -- picked up one more of
  the same item in the window between the dequeue-time observation and the
  cave's execution on the game thread. `native_result=8` is the game's own
  `quantity_delta` return, so the grant had EXECUTED and the item was in the
  inventory; the equality was broken by the pickup, and no retry could ever
  bring 8 back down to 7. This is the residual observe-to-execute race
  clients#429 predicted; the race was real, the punishment was wrong.

  The delta lane now orders its evidence: with execution confirmed (the state
  cell reports done and the result cell -- written only by the routine's own
  return -- is no longer the pre-arm sentinel), `quantity_delta` ran, and
  because it is an unconditional ADD, the delta APPLIED. What the read-back
  TOTAL then says is a statement about the player, not about the grant, so ANY
  disagreement with `expected_after` completes and names its direction in the
  detail: `completed with concurrent pickup: expected_after=7 actual=8` above
  it, `completed with concurrent spend or storage overflow: expected_after=7
  actual=5` below it. A deficit is a concurrent SPEND in the same unobservable
  window, or a capped stack overflowing into storage -- Bloodborne consumables
  overflow, and the pouch count can sit still while the items land in storage.
  With execution evidence those two cannot be told apart and need not be: both
  mean delivered.

  Without execution evidence nothing changes: the equality and both of its
  failure directions keep their full meaning there, so an unexecuted delta is
  never read as delivered. Such a completion acknowledges exactly like any
  other, recording the AP item's own quantity and nothing of the player's
  activity, and the next grant of the same item re-observes the live stack.
  Startup unpark is untouched and cannot match a park of this shape: an
  execution-evidenced park means the item LANDED, so requeueing it would
  double-grant. Operators holding one should resolve it with
  `bb-blocked INDEX --confirm`, never redeliver.

  The insert lane needs no equivalent: its witness is the reported slot record
  (`id` plus `quantity >= delta`), which concurrent activity does not weaken.

* **A truncated shad log resets the attach-wait freshness floor
  (clients#440).** shadPS4 appends to `shad_log.txt` within a run but
  *truncates* it at each launch -- a playtester's file shrank 644KB to 577KB
  across a relaunch, with the eboot `base_virtual_addr` line near the top.
  clients#419's wait recorded the log length at attach start as a monotonic
  freshness floor and accepted a base line only at or past that offset. Since
  the launcher spawns shadPS4 and the client as simultaneous siblings, the
  client sometimes read the log *before* the truncation: the floor became the
  previous run's large size, this run's real base landed near offset 0, and the
  gate rejected it for the whole 90-second budget. The resulting `NoFreshBase`
  error blamed the configured `shad_log` path, which was wrong, and cost a
  playtester an hour. Every past success was the other side of that race --
  truncate-then-write finished first and the pre-existing-base fast path
  attached instantly.

  The wait now follows the file the way `tail -F` does: a poll whose text is
  shorter than the floor means rotation, so the floor drops to 0 and that same
  poll reconsiders the whole file -- the truncated log's last base line is by
  definition this run's, so it is the fast-path check re-run. The stale-line
  guarantee is unchanged for a log that never shrinks. `NoFreshBase` keeps its
  wrong-path guidance, which is now only reachable when nothing base-shaped
  ever appears in the file at all.

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
