# Archive

**Archive — the pure-runtime reconciler cutover is complete; these documents
are not current guidance.** The reconciler now owns flags, goods, and the
consumable ledger by default (`DEFAULT_APPLY = ApplyClasses::ALL` in
`reconcile_io.rs`), and the old handlers survive only as `owns_*`-gated
fallbacks. For the current architecture see
[../ARCHITECTURE.md](../ARCHITECTURE.md).

Kept for history:

* `MIGRATION.md` — the reconciler strangler-migration design
  (was `crates/eldenring-archipelago/MIGRATION.md`).
* `WINDOWS-CUTOVER-CHECKLIST.md` — the operator runbook for the phased
  Windows cutover.
* `cutover-phases-README.md` + `phase-1-flags.md` … `phase-4-burn-guards.md` —
  the per-phase guided patches (were `cutover-phases/`).
