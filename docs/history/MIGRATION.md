# Reconciler migration (strangler) — `eldenring-archipelago`

The client's grant/snapshot bugs share one root cause: grants and snapshots fire on **discrete
events** (connect, load, `ItemReceived`) and every feature hand-rolls its own idempotency. That
produces double-grants, clobbers, and lost state (flask double-grant, Torch clobber, great-rune
double-grant, map-pieces-on-connect, flag-poll re-snapshot, bundle-lock grace loss,
reconnect-new-seed panic).

The fix is a single **reconciler**: compute DESIRED state from server-authoritative inputs, read
OBSERVED live state, apply the DIFF idempotently, all gated on "world loaded and stable." Events stop
mutating — they only mark the reconciler dirty. The pure loop + its proof live in
`crates/er-logic/src/reconcile.rs` and `crates/er-logic/src/reconciler_replay.rs` (host-tested,
`cargo test -p er-logic`). The Windows binding is `src/reconcile_io.rs`.

## Prerequisite (Windows)

**CORRECTION (2026-07-08): `core.rs` is NOT truncated.** An earlier note here claimed `core.rs` was
truncated in HEAD and had to be reconstructed. That was a *mount read-truncation artifact* — the
sandbox mount served a short read of the file. In git, `core.rs` is a complete, brace-balanced
2124-line file that compiles. **Reconstruct nothing.** Read every source file with
`git show HEAD:<path>`, never through the mount.

`reconcile_io.rs` is added to `lib.rs` (`mod reconcile_io;`). The dry-run wiring
(`reconcile_io::{init, tick, set_inputs, mark_dirty}` + `build_desired_inputs`) is now present in
`core.rs`, guarded by `RECONCILE_DRYRUN` so it computes and LOGS only — the old handlers stay live
and unchanged. The build prerequisite is simply `cargo build -p eldenring-archipelago` (Windows).

## Five-phase strangler

Each phase is independently shippable and reversible; the old path stays until its class is proven.

1. **Phase 0 — dry run.** Wire `reconcile_io::{init, tick, set_inputs, mark_dirty}` into the
   reconstructed `update_live` / net loop (call sites are marked `INTEGRATION:` in
   `reconcile_io.rs`). Run with `RECONCILE_DRYRUN=1`: the reconciler computes and LOGS the desired
   state / diff every tick but applies **nothing**. Validate the logged diff against live behavior on
   a real seed. No behavior change.

2. **Phase 1 — flags only.** Turn off dry-run for the FLAG classes (region open/seal, grace bundles,
   map-reveal incl. 62060-64 + 82001, great-rune `restored`, key-item obtained `4000xx`, goal). Let
   the reconciler own every event-flag write. Delete the corresponding inline flag sets
   (`flush_grace_flags` session set, `region.rs` bloom latch, map-reveal grant path). Keep the
   unique-good and ledger paths on the OLD code.

3. **Phase 2 — unique goods.** Hand key items + great runes to the reconciler (`GrantUnique` — grant
   the good iff absent from inventory; set companion obtained/restored flags atomically). Delete the
   great-rune `restored_great_rune_goods` re-grant and the key-item once-per-save bools. This is
   where the great-rune double-grant and map-pieces-on-connect classes are structurally retired (a
   map piece is a `MapReveal` — it can never become a granted good).

4. **Phase 3 — ledgered consumables.** Move flasks/runes/stones onto the per-`SaveIdentity` ledger
   watermark (`reconcile.json` next to the client). Delete `start_items_granted` and the
   receive-loop's ad-hoc start-item drain. This retires flask-double-grant-on-reload and the
   Torch clobber (the whole loop is stability-gated).

5. **Phase 4 — burn the idempotency bools.** With every class reconciled, delete the remaining
   per-feature guards (`notify_granted`, session grace `HashSet`, region bloom latch, flag-poll
   re-snapshot proxy, the seed-change scramble handling now covered by `Reconciler::set_inputs`).
   `core.rs` `update_live` shrinks to: nudge on events, `reconcile_io::tick()` per frame.

## Invariants the reconciler guarantees (proven in `er-logic`)

- **Permutation / duplication / load-screen invariance** of the final state
  (`reconciler_replay.rs`) — the theorem that makes every event-ordering bug impossible.
- **Map pieces are flags only** — `ItemSemantics::MapReveal` carries no good, so
  map-pieces-on-connect is unrepresentable.
- **Consumables use a per-save watermark**, not a count-diff (the player spends them).
- **`ClearFlag` is restricted to an owned-flags allowlist** — a vanilla-owned flag is never cleared.
- **Every mutation and snapshot is stability-gated** (`in_game && player_valid && (real_pickup_seen
  || dwell >= 8s)`), generalizing the Torch-clobber fix to the whole client.
- **Seed change → atomic input swap + watermark reset**, with non-panicking `Option` lookups (the
  reconnect-new-seed panic fix).

## Full grant-class coverage (host-tested in `er-logic`)

Every grant/flag class the live client emits is now represented in `ItemSemantics`, so the reconciler
can own the whole grant surface. Each class picks the idempotency rule that matches its
observability:

| Client behaviour (old path) | `ItemSemantics` variant | Idempotency rule |
|---|---|---|
| Received consumables / Torch, flasks, golden/lord's runes, smithing stones | `Consumable { full_id, qty, echo_skip }` | per-`SaveIdentity` ledger watermark; `echo_skip` dedups a native-sold shop echo (advance past, no grant) |
| Key items + vanilla obtained flags `4000xx` | `KeyItem { goods, obtained_flags }` | unique good: grant iff absent; obtained flags set-only (self-heal, never cleared) |
| Great runes + `restored` flag | `GreatRune { goods, restored_flag }` | unique good iff absent; restored flag is an independent observable flag |
| Map pieces `62060-64` + underground view `82001` | `MapReveal(flags)` | flags only — **a map piece can never become a granted good** |
| Region open / seal flags + grace bundles | `RegionFlags(flags)` + `SlotData.seal_flags` | observable flags; seals are OWNED (clearable), a received Lock overrides the seal to open |
| Goal send (received-item) | `GoalFlag(flag)` | observable flag |
| Slot-data start graces + `reveal_all_maps` map flags + underground `82001` | `SlotData.start_graces / map_reveal_flags / always_map_flags` | observable flags, SET-only (self-heal, never owned/cleared) |
| Slot-data start items (Torrent, flasks) | `SlotData.start_items` → ledgered at the `START_ITEM_INDEX_BASE` negative band | per-save watermark: granted once, a reload leaves the negative indices behind the frontier |
| Slot-data goal-send (condition-based) | `SlotData.goal_flag` + `goal_met` | goal flag desired-SET once every goal location is done (client seam routes the sentinel to `ClientStatus::Goal`) |
| Progressive items (Nth copy → tier N; overflow → Lord's Rune) | `Progressive { tiers, overflow_full_id }` | COUNT-based: tier goods/flags folded from the copy count (order-independent); overflow ledgered per stream index |
| Unmapped AP ids | `Inert` | no effect |

Proven in `reconcile.rs` unit tests and `reconciler_replay.rs` (the permutation / duplication /
load-injection theorem runs over a 7-item corpus covering region+seal, map, key, rune, goal, and two
consumables, WITH the slot-data bulk grants — start graces, both map-flag classes, a start item, and a
met goal — folded into the corpus `SlotData` so invariance is proven with every class present;
progressive has its own dedicated count-based invariance test).

### Gap 1 — slot-data BULK grants are now first-class desired state

Start graces, `reveal_all_maps` world-map flags (+ the unconditional `82001` underground view-unlock),
start items, and goal-send are folded into `DesiredState::build` from `SlotData`, so they are no longer
riding the scattered `startgrants` / goal handlers only:

- **Start graces / map-reveal flags** → observable flags, SET-only (self-heal after a load clobber,
  never owned so never cleared). `reveal_all_maps=false` desires ONLY the unconditional `82001`.
- **Start items** → ledgered at a reserved NEGATIVE synthetic-index band (`START_ITEM_INDEX_BASE`)
  BELOW the `>= 0` received stream, so the SINGLE per-save watermark grants them exactly once and a
  reload/seed-change is handled by the same `ledger_floor()` frontier logic (a fresh save / new seed
  starts the watermark at the band floor so they are owed; a reload leaves them behind it).
- **Goal-send** → `SlotData.goal_flag` becomes desired-SET when `goal_met`. See the client-seam
  `NOTE(windows-verify)` in `reconcile_io.rs` (dry-run only logs it today; the apply cutover routes the
  sentinel to `ClientStatus::Goal` or keeps goal-send on the `core.rs` §5c handler).

Host-tested: `reveal_all_maps` on ⇒ every map flag desired / off ⇒ only `82001`; start graces set &
never owned; goal flag desired only when met; start items grant once across a reload; a seed change
re-owes the new seed's start items.

### Gap 2 — shop native-sold consumable echo-dedup

A consumable whose reward the rewritten shop row already delivered at purchase carries
`Consumable.echo_skip = true` (mirrors `receive::RecvItem::echo_skip`). In the ledger it becomes a
`LedgeredGrant { apply: false }` → an `Action::SkipLedgered` that advances the per-save watermark PAST
that stream index WITHOUT granting. Host-tested: a real buy + its echo → one grant, watermark past
both; a pure echo → zero grants, watermark still advances (frontier stays contiguous, reload-safe).

## Classes deliberately NOT owned by the reconciler (and why)

These are **not** `flags ∪ goods ∪ ledger` desired-state, so they stay on their own code paths. The
reconciler is a state-convergence engine for *server-delivered grants*; the following are either
read-side predicates or global param patches, and forcing them into the desired-state model would be
wrong:

- **`auto_upgrade`** — a per-grant *transform* of the granted weapon's FullID
  (`upgrades::apply_auto_upgrade(full_id) -> full_id`). It is not a separate grant; the client mapper
  applies it when it computes the `goods`/`full_id` for a `KeyItem`/`Consumable`, so the reconciler
  simply grants the already-upgraded id. No new variant needed.
- **`flatten_regular_upgrades`** (`upgrade_cost::set_flatten`) — a one-shot **regulation param patch**
  applied at slot_data parse. It mutates upgrade *cost* tables globally, is inherently idempotent, and
  is not a per-item grant.
- **`global_scadutree_blessing`** (`upgrades::set_global_scadu_blessing`) — a continuous, periodically
  re-applied *stat effect* (`SCADU_LAST_TICK`), not a discrete grant. It has no observable
  flag/good/ledger representation.
- **Vanilla-drop suppression** (`er_logic::vanilla_suppress::should_suppress`, driven from the
  `detour.rs` pickup hook against the server COLLECTED set) — a **read-side predicate** deciding
  whether to null a native pickup. It is already a pure function in `er-logic`; it is not a mutation
  the reconciler applies.
- **Shop weapon-slot guard** — **retired** (`shop_sell.rs`: `should_suppress_sold` is a no-op; the
  weapon-slot echo-dedup now lives in the rewritten shop rows). Nothing for the reconciler to own.
- **Shop native-sold echo-dedup** — **now owned by the reconciler (Gap 2)**. A consumable the
  rewritten shop row delivered at purchase carries `Consumable.echo_skip`, which the ledger turns into
  an `Action::SkipLedgered` (advance the watermark past that index, grant nothing). Unique-good shop
  rewards still self-heal via `has_good`. No separate shop-side watermark poke is needed anymore.
