# Reconciler strangler cutover — phased, individually revertible

These files stage the DESTRUCTIVE half of the reconciler migration. **Nothing here is applied by
default.** The shipped/default state is: old handlers live + reconciler in **dry-run**
(`RECONCILE_DRYRUN=1` logs the plan, applies nothing). See `crates/eldenring-archipelago/MIGRATION.md`
for the design and `../WINDOWS-CUTOVER-CHECKLIST.md` for the full operator runbook.

## How the phases are controlled

The apply path is gated by two env vars (both read in `reconcile_io.rs`):

- `RECONCILE_DRYRUN=1` — compute + LOG the diff, apply nothing (phase 0).
- `RECONCILE_APPLY=<classes>` — when NOT in dry-run, which classes the reconciler may apply.
  Comma list of `flags`,`goods`,`ledger`; unset or `all` = everything. A disabled class stays owned
  by its OLD handler (the reconciler drops those actions — proven in
  `er-logic` `tick_with_classes_owns_only_enabled_classes`).

So a phase is: (a) set the env var, (b) apply the phase's handler-retirement edit, (c) build, (d)
run the in-game verify. Each phase is reversible on its own (revert the edit / drop the env var).

## Why these are GUIDED patches, not blind `git diff`s

The `eldenring-archipelago` crate is Windows-only (net/detour/hudhook/ilhook) and could NOT be
compiled in the authoring sandbox, so a blind line-delete diff can't be trusted to compile. Each
phase below gives the EXACT anchor (file + `git grep` string), the edit, what the reconciler owns
instead, the one-line in-game verify, and the revert. Apply one, `cargo build`, verify, then proceed.

## Coverage caveat (honest scope)

`core.rs::build_desired_inputs` currently maps the **received-item stream** only: region-lock
open/grace flags, key-item + great-rune goods & companion flags, progressive items, and received
consumables. It does NOT yet fold the slot-data BULK grants (start items, start graces,
`reveal_all_maps` map flags, goal-send). Those stay on their old handlers through all four phases
here; folding them in is a follow-up (extend `build_desired_inputs`, then a phase 1b/3b). The phases
below retire ONLY the handlers whose effect the reconciler already reproduces.

Phase order: **1 flags → 2 goods → 3 ledger (the atomic flip) → 4 burn dead guards.**
