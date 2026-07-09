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

`core.rs` is **truncated in HEAD** and does not compile. Reconstruct it first (bak_rlwarn + reflog +
PR #9 drafts). Nothing below can be built or wired until `cargo build -p eldenring-archipelago`
compiles again. `reconcile_io.rs` is already added to `lib.rs` (`mod reconcile_io;`) but is not yet
called from `core.rs`.

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
