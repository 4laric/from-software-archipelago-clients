# Phase 3 — ledgered consumables (THE ATOMIC FLIP)

- **Env:** `RECONCILE_APPLY=flags,goods,ledger` (i.e. `all`).
- **Reconciler owns:** received consumables (flasks, golden/lord's runes, smithing stones, and the
  progressive overflow), applied EXACTLY ONCE per `SaveIdentity` via the ledger watermark persisted
  to `reconcile.json` next to the client. This retires flask-double-grant-on-reload and the Torch
  clobber (the whole loop is stability-gated).

## Why this phase is atomic

The old receive path grants **every** received item — key items, runes, AND consumables — through
the SAME `GrantAction::Enqueue -> grant_full_id` call, deduped only by the received-index watermark.
Once the reconciler owns `ledger`, that call would grant a consumable the reconciler ALSO grants =
double. So enabling `ledger` and retiring the receive-path grant must happen **together**.

## Retire (in one commit with the env flip)

1. `git grep -n 'if dispatch.hook.grant_full_id(full_id, qty)'` (core.rs, the `GrantAction::Enqueue`
   arm) — delete the grant call and its H3 watermark-hold/rollback branch. The reconciler is now the
   sole grant path for received items (goods via `GrantUnique`, consumables via the ledger). Keep
   `process_received_item`'s NAME dispatch (progressive tier routing / region open naming) — only the
   `grant_full_id` placement is retired. Leave `dispatched_through` advancing.
2. `start_items_granted` START-ITEM DRAIN — `git grep -n 'crate::detour::grant_full_id(id, 1)'`
   (the `start_items_ok` loop, core.rs ~761-783). **Only retire this if start items have been folded
   into `build_desired_inputs`** (see the coverage caveat in README). If they have NOT, LEAVE it —
   start items are still slot-data-bulk and the reconciler does not yet own them. (Recommended: do
   the `build_desired_inputs` start-item fold FIRST as phase 3a, then retire this.)

## In-game verify

Fresh connect: all received flasks/stones land once. Die in the tutorial and reload: **no** second
flask grant (`ledger_count` stays 1; grep `[reconcile] applied ... GrantLedgered` fires once).
Reconnect mid-run: no re-grant of any consumable. Progressive item past its last tier: exactly one
Lord's Rune.

## Revert (has a data footprint)

Set `RECONCILE_APPLY=flags,goods` (drop `ledger`) and `git checkout core.rs` to restore the receive
grant. **Also delete `reconcile.json`** next to the client so a later re-enable starts its watermark
clean (a stale watermark would suppress grants the restored old path expects to make). This is the
only phase with persisted state.
