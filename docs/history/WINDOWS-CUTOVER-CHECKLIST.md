# Windows cutover checklist — reconciler strangler

Operator runbook for Alaric. The pure reconciler + its full grant-class coverage are host-tested and
green (`cargo test -p er-logic`: 247 unit + 1 integration). Everything client-side below is written
but was **NOT compile-verified** in the authoring sandbox (Windows-only deps; the cross-compile ran
out of disk before reaching this crate). So: build first, trust the in-game verifies, revert per step.

## 0. Do NOT reconstruct core.rs

`core.rs` is **fine** — a complete, brace-balanced 2124-line file in git. The earlier
"core.rs is truncated / reconstruct it" note was a **mount read-truncation artifact** (the sandbox
mount served a short read). Reconstruct nothing. Read source with `git show HEAD:<path>`, never the
mount. (MIGRATION.md and reconcile_io.rs have been corrected to say this.)

## 1. Build

```
cargo build -p eldenring-archipelago
```

This is the first real compile of the dry-run glue (`build_desired_inputs` / `classify_received` in
core.rs, `acquire_flags` in keyitems.rs, `dry_run_enabled` / `apply_classes` in reconcile_io.rs).
Fix any type errors HERE — likely spots, all flagged in code comments:

- **`inventory_has_goods` goods-id mask** (`reconcile_io.rs`): compares `entry.item_id.param_id()`
  against `goods & 0x0FFF_FFFF`. If the live `ItemId` wants the full category-tagged id, adjust that
  ONE predicate. **Sanity-check this in-game before trusting any `goods` phase.**
- `self.client()` / `ri.item().name()` / `ri.item().id()` shapes in the core.rs dry-run block — they
  mirror the existing section-3 snapshot loop; adjust if the borrow shapes differ.
- `self.item_counts` key domain (ap-id vs full-id) for consumable `qty` — `qty` only affects the
  logged plan, not idempotency, so a wrong key is cosmetic in dry-run.

## 2. Dry run (phase 0 — applies NOTHING)

```
set RECONCILE_DRYRUN=1   &&  <launch>
```

Expected log lines each dirty tick:
```
[reconcile dryrun] stable=<bool> desired(flags=N unique_goods=M ledger=K) would-apply P action(s): [SetFlag(..), GrantUnique(..), GrantLedgered{..}, ...]
```

**Healthy:** on a fresh connect `would-apply` starts non-empty and drains to `0` as the live path
grants; the SetFlag set matches the region/key/rune flags you actually received; `GrantUnique`
appears only for key items/runes; `GrantLedgered` only for consumables you were sent.
**Suspicious (investigate before proceeding):** a `GrantUnique` for a map piece (must never happen —
map pieces are flags-only), a `ClearFlag` for anything (means a seal flag leaked into the plan —
`seal_flags` is intentionally empty, so any ClearFlag is a bug), or `would-apply` never reaching 0
while stable (means the reconciler's desired diverges from what the live path grants — reconcile the
`classify_received` mapping before flipping any class).

Validate against a real seed with region locks, a key item, a great rune, and flasks.

## 3. Phase 1 — flags  (`cutover-phases/phase-1-flags.md`)

`set RECONCILE_APPLY=flags` (unset `RECONCILE_DRYRUN`), apply the two call-deletions, `cargo build`.
**Verify:** region graces + rune-restored persist a save-reload with the old tick handlers gone;
a CE-cleared grace flag self-heals in ~1s. **Revert:** `git checkout core.rs`, unset the env var.

## 4. Phase 2 — unique goods  (`cutover-phases/phase-2-unique-goods.md`)

`set RECONCILE_APPLY=flags,goods`. **No deletion** (additive self-heal; receive-path first-grant
no-ops via `has_good`). **First confirm the goods-id mask from step 1 in-game** (receive a key item;
it lands once; drop it, it re-grants; a great rune received twice → one copy, no "maximum allowed").
**Revert:** `RECONCILE_APPLY=flags`.

## 5. Phase 3 — ledger (atomic flip)  (`cutover-phases/phase-3-ledgered-consumables.md`)

`set RECONCILE_APPLY=flags,goods,ledger`. In the SAME build, retire the receive-path
`grant_full_id` placement (the reconciler becomes the sole received-item grant path). Start items are now folded into
`build_desired_inputs` (Gap 1, ledgered at the negative band), so the old start-item drain may be
retired in this phase too. `cargo build`. **Verify:** tutorial-death reload grants no second flask
(`GrantLedgered` fires once); reconnect re-grants nothing. **Revert:** drop `ledger`,
`git checkout core.rs`, **and delete `reconcile.json`** (only phase with persisted watermark state).

## 6. Phase 4 — burn dead guards  (`cutover-phases/phase-4-burn-guards.md`)

`RECONCILE_APPLY` unset (= `all`). Delete each now-dead idempotency bool ONLY after `git grep`
confirms zero live readers; one commit each so `git revert` is surgical. **Verify:** full regression
(connect / reconnect / seed-change / death-reload / lose+regain key item) — no double-grants, no
lost checks, no panics. No data footprint.

## Known unsure / to double-check on Windows

- **`inventory_has_goods` goods-id mask** (step 1) — the single most important thing to sanity-check
  before any `goods`/`ledger` phase.
- **Slot-data bulk grants — NOW folded (Gap 1):** start graces, `reveal_all_maps` map flags (+ the
  unconditional `82001`), and start items are in `build_desired_inputs` (host-tested in er-logic).
  Start items ledger at the negative `START_ITEM_INDEX_BASE` band and grant once per save. On the
  dry-run, confirm the `would-apply` plan shows those flags/start-item grants matching the old
  startgrants path before retiring `apply_start_flags` / the start-item drain (phase 1 / phase 3).
- **Goal-send (Gap 1) — client seam TODO before the apply cutover.** `build_desired_inputs` sets
  `goal_flag = Some(reconcile_io::GOAL_SENTINEL_FLAG)` + live `goal_met`. Goal-send is a
  `ClientStatus::Goal` network send, NOT an ER flag: in dry-run the `SetFlag(sentinel)` is only
  LOGGED. Before flipping `flags` APPLY, either (a) route that sentinel action to
  `client.set_status(ClientStatus::Goal)` via a client seam, or (b) keep goal-send on the `core.rs`
  §5c handler and set `goal_flag: None`. See the `NOTE(windows-verify)` in `reconcile_io.rs`.
- **Shop native-sold consumables — NOW deduped (Gap 2):** the echo carries `Consumable.echo_skip`
  (same predicate as the live receive loop), which becomes an `Action::SkipLedgered` — the watermark
  advances past that index, no grant. No shop-side watermark poke needed. Still smoke-test: buy a
  shop-sold consumable AP location and confirm exactly one appears in inventory after `ledger` is on.
- **`inventory_has_goods` goods-id mask (Gap 3)** — reviewed but STILL Windows-unverified; the mask
  looks right for the `0x4000_0000` goods-category convention, but `ItemId::param_id()` may return the
  full category-tagged id (then double-mask — the alternative is kept in a comment). This is the
  single most important thing to sanity-check with a set->readback before any `goods`/`ledger` phase.
- All client-side code here is UNVERIFIED by a compiler — the er-logic decision core is the only
  machine-checked part.
