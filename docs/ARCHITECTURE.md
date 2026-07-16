# Elden Ring client architecture

How the `eldenring-archipelago` client works today. (The migration that got it
here is archived under [docs/history/](history/).)

## The split: pure core, thin binding

* **`crates/er-logic`** holds the decision logic — pure Rust, no game, Windows,
  or socket dependencies, host-tested by `cargo test -p er-logic`. The state
  engine lives in `er-logic/src/reconcile.rs`; its ordering-invariance proof in
  `er-logic/src/reconciler_replay.rs`.
* **`crates/eldenring-archipelago`** is the `cdylib` loaded into the live game
  by me3. Its binding to the reconciler is `src/reconcile_io.rs`; the rest of
  the crate is hooks, params, and I/O glue.

## Pure-runtime model

Nothing is baked into game files. At connect the client reads the seed's slot
data from the Archipelago server, verifies the contract hash and version band
(`contract_gen.rs`, generated from the apworld's `contract.py`; `er-semver`),
and then operates entirely on the live game: it detects checks as they happen,
grants received items, lights graces, and enforces region locks (regions are
sealed until their Region Lock item arrives; runs start at Roundtable Hold and
entering a locked region warps you back there).

## The reconciler

Grants and world flags are not applied by event handlers. Instead a
**reconciler** runs the classic converge loop:

1. compute **DESIRED** state from server-authoritative inputs (slot data + the
   received-item stream),
2. read **OBSERVED** live game state,
3. apply the **DIFF** idempotently,

all gated on "world loaded and stable." Events (connect, load, `ItemReceived`)
never mutate anything — they only mark the reconciler dirty.

**The reconciler owns all three apply classes by default** —
`DEFAULT_APPLY = ApplyClasses::ALL` in `reconcile_io.rs`: event **flags**,
unique **goods**, and the **ledger**ed consumable stream. The pre-reconciler
grant handlers still exist but are demoted to fallbacks gated on
`reconcile_io::owns_flags() / owns_goods() / owns_ledger()`: they only run for
a class the reconciler has been told not to own. Runtime narrowing needs no
rebuild:

* `RECONCILE_APPLY=flags` (or `flags,goods`, or `none`) — hand the disabled
  classes back to the old handlers;
* `RECONCILE_DRYRUN=1` — compute and log the plan every dirty tick, apply
  nothing (old handlers fully authoritative).

The class gating itself is host-proven
(`tick_with_classes_owns_only_enabled_classes` in `er-logic`).

## Invariants (proven in `er-logic`)

* **Permutation / duplication / load-screen invariance** of the final state
  (`reconciler_replay.rs`) — the theorem that makes event-ordering bugs
  impossible.
* **Map pieces are flags only** — `ItemSemantics::MapReveal` carries no good,
  so "map piece granted as an item" is unrepresentable.
* **Consumables use a per-save watermark**, not a count-diff (the player
  spends them).
* **`ClearFlag` is restricted to an owned-flags allowlist** — a vanilla-owned
  flag is never cleared.
* **Every mutation and snapshot is stability-gated**
  (`in_game && player_valid && (real_pickup_seen || dwell >= 8s)`).
* **Seed change → atomic input swap + watermark reset**, with non-panicking
  `Option` lookups.

## Grant-class coverage (`ItemSemantics`)

Every grant/flag class the client emits is represented in `ItemSemantics`, so
the reconciler owns the whole grant surface. Each class uses the idempotency
rule that matches its observability:

| Client behaviour | `ItemSemantics` variant | Idempotency rule |
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

The permutation / duplication / load-injection theorem runs over a corpus
covering region+seal, map, key, rune, goal, and consumables, with the slot-data
bulk grants (start graces, both map-flag classes, a start item, a met goal)
folded in, so invariance is proven with every class present; progressive items
have their own count-based invariance test.

Slot-data bulk grants are first-class desired state (`DesiredState::build`
folds them from `SlotData`): start graces and map-reveal flags are SET-only
observable flags (`reveal_all_maps=false` desires only the unconditional
`82001`); start items ledger at a reserved negative synthetic-index band below
the `>= 0` received stream, so the single per-save watermark grants them
exactly once and a seed change re-owes the new seed's start items.

## Classes deliberately NOT owned by the reconciler (and why)

The reconciler is a state-convergence engine for *server-delivered grants*
(`flags ∪ goods ∪ ledger`). The following are read-side predicates or global
param patches, and forcing them into the desired-state model would be wrong:

* **`auto_upgrade`** — a per-grant *transform* of the granted weapon's FullID
  (`upgrades::apply_auto_upgrade`). Not a separate grant; the mapper applies it
  when computing the `goods`/`full_id`, so the reconciler grants the
  already-upgraded id.
* **`flatten_regular_upgrades`** (`upgrade_cost::set_flatten`) — a one-shot
  **regulation param patch** applied at slot-data parse. Globally mutates
  upgrade cost tables, inherently idempotent, not a per-item grant.
* **`global_scadutree_blessing`** (`upgrades::set_global_scadu_blessing`) — a
  continuous, periodically re-applied *stat effect*, not a discrete grant; it
  has no observable flag/good/ledger representation.
* **Vanilla-drop suppression** (`er_logic::vanilla_suppress::should_suppress`,
  driven from the `detour.rs` pickup hook against the server COLLECTED set) —
  a **read-side predicate** deciding whether to null a native pickup, already a
  pure function in `er-logic`; not a mutation the reconciler applies.
* **Shop weapon-slot guard** — retired (`shop_sell.rs::should_suppress_sold`
  is a no-op; the weapon-slot echo-dedup lives in the rewritten shop rows).
* **Shop native-sold echo-dedup** — this one IS owned by the reconciler: a
  consumable the rewritten shop row already delivered at purchase carries
  `Consumable.echo_skip`, which the ledger turns into `Action::SkipLedgered`
  (advance the watermark past that index, grant nothing). Unique-good shop
  rewards self-heal via `has_good`.
