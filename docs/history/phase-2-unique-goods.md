# Phase 2 — unique goods

- **Env:** `RECONCILE_APPLY=flags,goods`.
- **Reconciler owns:** key-item + great-rune GOODS — granted iff ABSENT from the live inventory
  (`GrantUnique` via `inventory_has_goods`), with companion flags set atomically. This is what
  structurally retires the great-rune double-grant AND makes key items **self-heal** if lost
  (the old index-watermark grant never re-granted a lost item).

## Nothing to delete yet (additive safety)

Phase 2 is intentionally **non-destructive**. The old receive-path first-grant
(`GrantAction::Enqueue -> hook.grant_full_id`, `git grep -n 'if dispatch.hook.grant_full_id(full_id, qty)'`)
can stay: the reconciler's `has_good` check makes it a **no-op** whenever the item is already
present, so the two coexist safely (receive grants once; reconciler only acts if the good goes
missing). Removing that first-grant is deferred to phase 3, where the whole received-item grant path
is handed to the reconciler atomically.

> Do NOT enable `goods` while a duplicate unconditional grant of the SAME good exists elsewhere —
> the reconciler guards itself with `has_good`, but a second *unconditional* grant path would still
> double. The receive Enqueue path is guarded by its watermark, so it is safe; audit any other.

## In-game verify

Receive a key item; confirm it lands once. Then drop/lose it (CE remove from inventory) and confirm
the reconciler re-grants it on the next stable tick (`[reconcile] applied ... GrantUnique`). Receive
a great rune twice (reconnect) — inventory shows exactly one, no "maximum allowed" popup.

## Revert

Unset the `goods` class (`RECONCILE_APPLY=flags`). No edit to revert (phase 2 deletes nothing).
