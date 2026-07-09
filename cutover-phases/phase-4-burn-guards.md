# Phase 4 — burn the dead idempotency guards

- **Env:** `RECONCILE_APPLY=all` (unset it; `all` is the default), `RECONCILE_DRYRUN` gone.
- **Reconciler owns:** everything it maps. With phases 1-3 live, the per-feature idempotency bools
  the old handlers needed are now dead weight. This phase is pure cleanup — no behavior change — so
  do it LAST and only after phases 1-3 have been in-game confirmed.

## Delete (each independently; `cargo build` after each)

Only remove a guard once its handler is gone and its field has NO remaining reader (`git grep` the
field name to confirm zero live uses before deleting the declaration + initializer + save-load):

- `start_items_granted`, `start_items_ok` — dead once phase 3 retires the start-item drain.
  Fields (core.rs struct + constructor), the `SaveState` read at `git grep -n 'st.start_items_granted'`,
  and any write. **Leave if phase 3 left the start-item drain in place.**
- `start_flags_done` — dead once the map-reveal/start-grace fold lands (coverage-caveat follow-up).
- `flag_poll_baseline` re-snapshot bools — the reconciler's stability gate + owned-flag model
  subsume the flag-poll new-save-default proxy; retire once confirmed the poll no longer needs the
  baseline to avoid re-snapshot eating checks.
- Any `notify_granted` / session grace `HashSet` / region bloom latch still present — `git grep` each;
  delete only if unreferenced.
- The seed-change scramble handling that `Reconciler::set_inputs` now covers
  (`git grep -n reset_for_new_seed`): the reconciler resets its own per-seed state + watermark on a
  genuine seed change, so the manual table-rebuild can be trimmed to the NON-reconciler tables only.

## In-game verify

Full regression: fresh connect, reconnect, seed-change reconnect, tutorial-death reload, lose+regain
a key item. No double-grants, no lost checks, no panics. `update_live` should now be, for grants:
nudge on events + `reconcile_io::tick()` per frame.

## Revert

Each deletion is a separate commit — `git revert` the specific one. No data footprint (the
`reconcile.json` watermark from phase 3 is unaffected).
