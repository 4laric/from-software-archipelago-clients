# Bloodborne Archipelago client

This crate is the standalone Bloodborne client path. Unlike the native Dark
Souls III and Sekiro DLLs, it reads Bloodborne location flags directly from
shadPS4 process memory. Item delivery uses the in-process **native** backend
(see below). Unsupported or mismatched game images fail closed before delivery
is armed or any Archipelago item is acknowledged.

Downloaded builds include a SHA-256 companion file and trusted push builds are
signed with GitHub build provenance. See the repository's
[security and verification guide](../../SECURITY.md) before running a client or
reporting a suspected vulnerability.

The native grant contract supports received-item grants:

- The client stages one grant only when no command is pending.
- The runtime validates the expected current quantity before changing anything.
- Existing stacks use a guarded record update; absent items use the native
  game-thread `ItemGrant` path.
- The runtime clears the command only after verifying completion.
- The client advances its durable received-item watermark only after observing
  terminal success.

The vendored native contract retains compatibility identifiers from its
prototype lineage. They validate the compiled payload artifact only; there is
no external command-file transport or table dependency. A terminal harness
failure blocks that AP item with a bounded diagnostic while location polling
and the server connection continue.

Item-ID mappings and location checks are deliberately not hard-coded here. The
local rows are only a migration/test fallback. On connect, the apworld's
`runtime_locations` and `runtime_items` slot-data tables replace them. Every
runtime item carries an explicit raw descriptor, normalized id, item category,
descriptor-evidence class, quantity and receive policy. The client never derives
an equipment descriptor from an ItemLot id. Malformed or unvalidated present
tables fail closed before a bridge command can be published.

The crate also wires pure policy for auto-equip and auto-upgrade into the
ordered receive loop behind mockable backend operations. Equipment
slots are selected from AP receive-stream ordinals rather than the live loadout,
so reconnect replay converges. Right- and left-hand weapons rotate over their
two slots, Caryll Runes rotate over three slots after the Rune Workshop Tool has
appeared in the feed, attire uses its fixed body slot, and Oath Runes use their
dedicated slot. Upgrade targeting is raise-only and clamped to +10. A durable
pending plan records the selected level, target slot, and completed stages
before the receive watermark advances, so a restart between grant and equip
does not re-grant. The native backend's read-only inventory census now selects
the highest recognized held weapon level, including the catalog-backed DLC and
Uncanny families, and grants a received weapon directly from that reinforcement
row. An empty or unrecognized census preserves the received level rather than
claiming a +0 target. The first live canary raised a Saw Cleaver from +0 to +1
against a held Ludwig's Holy Blade +1. It never mutates an existing inventory
record or spends materials. Live auto-equip remains disarmed until its separate
v0.18 memory contract is validated.

## Standalone client

On Windows the normal client invocation also starts a standalone translucent
status window. It is hosted by `bb-ap-client` itself and never injects into or
hooks shadPS4. The game/AP/delivery readiness shown there arrives over a bounded,
coalescing state bridge, so closing or stalling the window cannot reorder or
acknowledge an item. Closing the window requests a controlled client shutdown.
The existing console and session log remain available while the richer activity,
connection controls, tray and persisted-layout slices are built.

`bb-ap-client` connects as game `Bloodborne`, requests a full item sync, polls
configured location flags when its safety context is valid, and grants received
goods strictly by AP index. A seed-and-slot keyed JSON ledger durably binds the
validated save identity before planning any grant and is saved after each
verified stage, so reconnects and process reloads skip acknowledged indices.
Location reads and item mutation share the same gameplay/save gate; losing or
changing that context cannot start or acknowledge a delivery.

The seed also names one `goal_location`. After that location survives the same
debounced flag read as every other check, the client sends Archipelago goal
status before acknowledging the location. On reconnect it re-sends goal status
when the server already records that location, so a dropped connection cannot
strand a completed Father Gascoigne run.

```text
bb-ap-client SERVER SLOT CONFIG LEDGER [PASSWORD] [--mock] [--assume-correct-save]
```

Use `runtime-config.example.json` as the configuration shape. Its zero AP IDs
are placeholders to replace with IDs emitted by the APWorld. The test location
binds the statically confirmed Pebble acquisition flag `52100000`. On Windows,
live mode discovers shadPS4, reads the launch-specific eboot base from
`shad_log`, verifies the Bloodborne 01.09 setter signature, and resolves flags
through the manager's group tree. The diagnostic reader is live, but automatic
`LocationChecks` are fail-closed because the live backend cannot yet prove a
loaded-save identity or a stable gameplay state. Mock mode supplies both,
requires the configured identity, and debounces true reads before reporting.
For the controlled vertical-slice MVP, `--assume-correct-save` explicitly makes
the player responsible for loading the character belonging to the connected AP
slot. That mode uses a synthetic identity, requires three consecutive healthy
event-flag-manager probes, and then enables automatic location checks and item
delivery. Any failed manager probe immediately disarms it and resets all
location debounce streaks. Do not switch characters while it is connected;
checks sent from the wrong character cannot be undone, and received items may be
recorded against the wrong save.
Run the client as administrator when shadPS4
is elevated. `--mock` applies `mock_set_flags` and exercises the complete
network, check, ordered-grant, acknowledgement, and persistence loop without
game-memory access.

Live startup retries for up to ten minutes while shadPS4, the eboot mapping,
and the event-flag manager initialize. The BBLauncher companion may therefore
start the client as soon as the emulator process appears without turning that
normal initialization window into a terminal attach failure.

The read-only diagnostic below prints whether a decimal event flag is set:

```text
bb-flag-probe SHAD_LOG EVENT_FLAG [OUTPUT]
```

If a grant terminally fails in the harness, the client parks it (acknowledges
it as blocked) and keeps delivering later items. `bb-blocked LEDGER SEED_NAME
SLOT_NAME` lists parked entries with manual re-grant hints;
`bb-blocked LEDGER SEED_NAME SLOT_NAME INDEX --confirm` clears one after you
verify the item physically arrived. It never re-grants automatically —
re-issuing an already-delivered item would duplicate it.

## BBLauncher companion startup

`tools/start-bloodborne-ap.ps1` requests elevation once, starts BBLauncher,
waits while the player selects the build/patches and presses Play, then starts
`bb-ap-client` at the same privilege level. This is a companion rather than a
BBLauncher binary patch; the current launcher settings format has no supported
external-command field.

The live backend automatically reattaches after a stale shad process handle. If
shad is temporarily unavailable, location polling stays connected to AP, reports
the failure at a bounded cadence, and announces recovery after the new process is
readable.

Seeds that claim any vanilla award is suppressed also carry the exact
suppression-plan SHA-256 and manifest format. Configure `suppression_manifest`
to the build manifest and `installed_gameparam` to the binder actually loaded by
the game. The client requires the installation-relative `param/gameparam` path,
independently hashes that installed file, and refuses the seed if it does not
equal the manifest output; the separate build artifact is explicitly rejected
as installation evidence.

Runtime event flags, normalized item IDs, and executable
signatures stay in this client-side configuration/backend layer. They are never
read from Bloodborne world-design data.

## Native delivery

The client reads and writes shadPS4 memory directly, installs the native grant
payload, and drives its grant state machine in-process. Native delivery is the
only supported live backend. The old spelling `--delivery=native` remains
accepted for launch-plan compatibility, but selecting a backend is unnecessary:

```
bb-ap-client SERVER SLOT CONFIG LEDGER [PASSWORD]                 # native
bb-ap-client SERVER SLOT CONFIG LEDGER [PASSWORD] --delivery=native
```

Defaulting to native is safe because native **fails closed** on any image it
cannot validate: `require_validated_image` refuses CUSA00900 and every other
serial/build, so a recognised-and-validated image gets native and nothing else
is ever patched. An image native cannot validate makes the client **stop with a
clear, actionable error**:

> This game build was not recognized, so native item delivery cannot run
> safely. Delivery was not armed and no Archipelago item was acknowledged. Use
> the launcher's Open Logs & Diagnostics action and send the session bundle so
> native support can be added for this build.

`--delivery=ce-bridge` is rejected with a migration error. The fail-closed image
check, no-double-grant, install atomicity and image-mismatch guards are
unchanged.

The native code lives in `src/native/`:

- `contract.rs` consumes `contract/bb-native-grant-contract.v5.json`, a verbatim
  vendored copy of the world repo's single source of truth. Every hook-site RVA,
  native-routine RVA, state-cell offset, descriptor prefix, image-assert byte
  string and relocatable payload blob is read from that file — no address is a
  hand-copied number, and a unit test refuses to arm if the vendored copy drifts
  from the crate's `RUNTIME_BUILD`/`HARNESS_VERSION`/`BRIDGE_PROTOCOL`.
- `mem.rs` puts `ReadProcessMemory`/`WriteProcessMemory`/`VirtualProtectEx`
  behind a `ProcessMemory` trait with a host `FakeMemory`, and implements
  `require_validated_image` — every image assert must match before anything is
  written; CUSA00900 and every other build are refused, not guessed.
- `install.rs` writes the payload with a thread-suspend atomicity protocol:
  the caves and state region first, then the two seven-byte detours together,
  under a single suspend of every guest thread, only once no thread's RIP lies
  inside either detour window; a persistently obstructed window aborts with **no
  detour written** and always resumes the threads.
- `delivery.rs` is the grant state machine (hydration grace, bounded verify,
  verify-against-the-reported-slot, replay recovery, fail-closed absent Blood
  Vial), `guest.rs` is the inventory-geometry `Runtime` over `ProcessMemory`,
  and `engine.rs`/`backend.rs` drive one grant at a time and adapt it to the
  `BloodborneBackend` trait.
- `diagnostics.rs` is the passive per-grant delivery record (clients#445); see
  below. It is write-only — nothing in the state machine ever branches on it.

### Delivery diagnostics (clients#445)

On the native path the client appends **one JSON line per terminal grant** to
`delivery-diagnostics.jsonl`, in the same session folder as `ledger.json` and
`client.log`. There is no flag and nothing to run: play normally, and when a
delivery looks wrong, send that file back the way you send `client.log`.

The line carries only what the delivery machine already computed for its own
decisions — tag and AP index, raw/normalized item id, lane (`insert`/`delta`)
and descriptor source, quantity, the baseline it observed, the total it
expected, every held-stack read-back the verify loop saw, the cave's result
cell, the terminal status and detail, and the client's own gameplay-ready state
at submit and at the terminal step. It also records a session sequence number,
the millisecond gap from the preceding terminal grant, and that grant's inferred
destination so release-flood and sticky-overflow hypotheses can be tested from
an ordinary playthrough. **No extra read of the game is performed for
it**, and a failure to write the file warns once and never touches a delivery.

`inferred_destination` is `held` when the read-back arithmetic accounts for the
grant in the held stack. It is `storage` for the player-validated insert shape:
ItemGrant completed but the inserted goods never appeared in held inventory,
and the player confirmed the item in the Hunter's Dream storage box. A delta
that executes while the held total remains short is still
`storage_suspected`: the client cannot distinguish capped overflow from a
concurrent spend. `unknown` covers every other shape. Thus only `storage` is a
confirmed destination; `storage_suspected` remains a hypothesis, never a
measurement.

`tools/summarize_delivery_diagnostics.py <file>` groups the records by item,
status and inferred destination, then prints a short player-verification list
for suspected storage deliveries and the grants immediately after them.

This complements, and does not replace, the manual probe in bb-archipelago#203:
controlled-condition questions — a unique-item insert, a deliberately at-cap
arming — still need the probe, because those conditions do not arise on their
own during play. The passive file answers the question the probe cannot: what
the distribution looks like across a real session.

### Blood-gem ItemGrant diagnostic

The earlier inventory-manager snapshot was retired after two clean natural-gem
acquisitions changed only ordinary consumable stacks. Native sessions now hook
the already validated `ItemGrant(inventory, descriptor, quantity)` boundary and
append one compact, read-only record to `blood-gem-capture.jsonl` for each call.
The guest cave copies the transient descriptor fields, quantity and caller into
dedicated diagnostic cells before replaying the exact displaced prologue. It
does not alter the call or any pointed-to object.

The probe is fail-soft: an unexpected prologue or occupied scratch region leaves
delivery available and prints that blood-gem diagnostics are inactive. Each
record includes a sequence gap so a burst faster than the client poll is visible
rather than silently mistaken for a complete capture.

For a focused capture, relaunch through a bundle that prints `Blood-gem
ItemGrant probe armed`, acquire one ordinary pickup, then two natural blood gems.
Send `blood-gem-capture.jsonl` with `client.log`, naming the two gems and their
pickup order. A gem-shaped descriptor proves this boundary owns category-8
insertion; no call across both clean acquisitions rules it out directly.

### Pickup-notification diagnostic (clients#510)

Set `"pickup_notification_probe": true` in the local `runtime-config.json` to
create `pickup-notification-capture.jsonl` beside the receive ledger. The probe
is disabled by default and cannot be enabled by seed slot data. It is strictly
observation-only: it correlates newly sent AP location IDs and delivery states
with the already validated native `ItemGrant` boundary, including its stable
caller RVA. It neither calls a message function nor changes suppression,
acknowledgement, delivery, or inventory.

For the focused playtest, number the visible actions in a note and perform: one
ordinary vanilla pickup; one AP-owned pickup whose result is local; one whose
result is remote; one direct consumable delivery; one weapon delivery; and, if
convenient, one at-cap delivery routed to storage. Note the exact banner shown
for each action and send `pickup-notification-capture.jsonl` with `client.log`.
The capture is bounded to 4096 records and repeated pending states/native-call
sequences are deduplicated. A write failure is non-fatal.

The v2 probe also wraps only the two vanilla pickup-side direct calls proven by
playtest.32 (`0x17D93F9 -> ItemGrant`, returning at `0x17D93FE`, and
`0x14DA9FA -> ItemGrant`, returning at `0x14DA9FF`). Before either is touched,
both exact five-byte calls and two empty cave/state regions must match the
supported CUSA03173 01.09 image. Each wrapper makes the original call unchanged
and records entry registers, an opaque guest-stack thread token, the native
return value and a best-effort 24-byte descriptor. Ambient `rcx`, `r8`, and `r9`
are labelled only as candidate message/icon/auxiliary context; the probe does
not claim their meaning, call a message routine, synthesize a banner, or mutate
an argument/result. Any mismatch or install/capture failure is non-fatal.

For the next focused capture, leave the probe enabled and collect one ordinary
lower-corner pickup banner and one centered modal pickup if convenient. Send
`pickup-notification-capture.jsonl` with `client.log`, noting which visible
action produced each presentation. `vanilla_pickup_call` records now identify
the call edge, entry/return values, stack correlation token, and candidate
presentation contexts without sampling the AP grant caller `0x50DBB44`.

### Zero-Vial diagnostic (bb-archipelago#70)

Native sessions also append read-only samples to `blood-vial-capture.jsonl`
beside the ledger. Every five seconds it records whether a canonical
`0x400003E8` Vial stack exists and includes any inventory row whose low item id
is 1000. This deliberately includes the known shop-only/HUD collision without
mistaking it for a stack. It records the canonical row itself; ordinary goods
must not be passed through the generated-instance resolver, whose output is
meaningful only for weapons and blood gems. The diagnostic never stages or
grants a Vial.

When a canonical Vial row first appears, the capture emits a
`canonical_vial_created` record. It names the selected slot, records the last
occupied slot before and after creation, and preserves a five-slot window
around the target from both snapshots. Missing pre-creation rows are explicit
`present: false` entries rather than invented zero bytes. This distinguishes
append, reuse, and in-place rewrite behavior and supplies the exact neighboring
bytes needed to design a guarded zero-to-one bootstrap.

For the dedicated capture: begin at zero Vials, leave the client running for
one heartbeat, buy the first shop Vial, wait for another heartbeat, then obtain
one Vial from an enemy or world pickup and wait five seconds. Send
`blood-vial-capture.jsonl` with `client.log`. Discarding the throwaway save is
still recommended; absent-stack AP Vial insertion remains refused.

### Shop-enablement diagnostic

Native sessions also append a read-only `shop-capture.jsonl` beside the ledger.
It records inventory transitions in full and keeps a five-second focused
heartbeat for the Hunter Chief Emblem, workshop tools, and hunter badges. This
is the live witness needed before general shop randomization: it distinguishes
the inventory good that unlocks a shelf from the exact descriptor and quantity
record created by a natural purchase. It never edits shop rows, inventory, or
event flags.

For the dedicated capture, wait for one heartbeat before acquiring a badge,
acquire it naturally or through AP, wait five seconds, open the Bath Messenger
shop and note which new wares appear, then buy one newly unlocked item and wait
for one final heartbeat. Send `shop-capture.jsonl` with `client.log` and report
the badge and purchased item names. A throwaway save is recommended so the
before/after shelf comparison is unambiguous.

Replay recovery is **coordinated with the receive ledger**, not a parallel
store: `grant_item` feeds the ledger-derived `expected_before`
(`SlotLedger::delivered_quantity`) straight into the delivery machine, so a
restart mid-grant recognises an already-applied stack (`recovered_complete`)
instead of granting twice.

> ⚠️ **Untested against a live game.** The pure logic (contract consumption,
> descriptor encoding, image verification, install atomicity, the delivery
> machine, the inventory walk) is host-tested against fakes. The live Windows
> attach/install/thread seams (`#[cfg(windows)]`) are compiled and linted by CI
> only and **must be owner-validated against a running process** before the
> native path is fully trusted. Defaulting to native is bounded by its
> fail-closed image check: an unrecognised build hard-fails with instructions
> rather than being delivered, so nothing is patched on an image native cannot
> validate. Owner-checklist item 3 (the CUSA00900 wrong-image refusal) is still
> untested against a dump.
