# Phase 1 — flags only

- **Env:** `RECONCILE_APPLY=flags` (and drop `RECONCILE_DRYRUN`).
- **Reconciler owns:** every event-flag write the received-item stream implies — region-lock OPEN
  flags + their revealed grace bundles (`ItemSemantics::RegionFlags`), and key-item / great-rune
  vanilla obtained/restored companion flags `4000xx` / `191-196` (`ItemSemantics::KeyItem`). These
  self-heal every stable tick (the bundle-lock grace-loss + great-rune-restored classes).
- **NOT yet owned (leave their handlers):** goal-send flag, `reveal_all_maps` map flags, start-grace
  flags — these are slot-data BULK grants not yet folded into `build_desired_inputs`.

## Retire (core.rs `update_live`)

Both of these become redundant once `RECONCILE_APPLY=flags` is live — the reconciler sets the same
flags idempotently and self-heals losses, which the tick handlers were bolted on to do:

1. `git grep -n 'crate::keyitems::tick_keyitem_flags(&received_all)'` — delete this call. The
   reconciler now owns the obtained/restored flags via `KeyItem.obtained_flags`.
2. `git grep -n 'crate::region::tick_reconcile_received_locks(cfg, &received_all)'` — delete this
   call. The reconciler now owns region open + grace-bundle flags via `RegionFlags`.

Keep `region::open_on_received_name` in the receive NAME-dispatch for now (it also drives console
"Region unlocked" messaging); it is idempotent and harmless alongside the reconciler. It is removed
in phase 4.

## In-game verify

Receive a region Lock and a great rune, then save-and-quit and reload. The region's graces stay
revealed and the rune stays usable **without** the old tick handlers running (grep the log for
`[reconcile] applied` flag actions; the `key item ... applied` / `tick_reconcile` lines are gone).
Deliberately clear a grace bundle flag (CE) — it re-sets within ~1s (self-heal).

## Revert

`git checkout crates/eldenring-archipelago/src/core.rs` (or re-add the two deleted calls) and unset
`RECONCILE_APPLY`. No data migration; flags are idempotent.
