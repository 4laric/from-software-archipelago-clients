# RECON: the sphere target was never stuck, and what that leaves

2026-08-06, from the same live log as `RECON-downstate-palette-20260806.md`.

## The claim that was wrong, twice over

Three separate notes recorded that the sphere gradient "never engages" — that every sweep across 12
readings, 4 regions and 3 seeds resolved `sphere target 0` or `unmapped`, so the floor was doing 100%
of the difficulty work. A suspected cause was recorded alongside it: an id-space mismatch, on the
reasoning that `regionSphereTargetRanges` is keyed in the 10000-30040 band while Liurnia and Altus
are play_region 62000 / 63000, i.e. sub-ids 620 / 630, which could not fall in those ranges.

Both halves are false.

**The id space is fine.** The live `kick-watch` line reads
`play_region 6200000 (sub 62000); range [62000,62000] flag 73202 = true`. play_region is **6200000**,
not 62000 — one more zero than the note assumed — so `sub = play_region/100` lands on 62000, exactly
the space the wire uses. It matched in the scaling wire and in all 116 area-lock ranges.

**The gradient is wired and it engages.** `enemy-scaling: enabled (Sphere), 18 region targets,
max 10000, floor tier 0`, and the visible prefix of the wire carries four distinct depths:
`[14000,14000,10000]`, `[15000,15000,10000]`, `[20010,20010,3333]`, `[22000,22000,6667]`, then zeros.

**The all-zero readings were a sampling artefact of where the player stood.** The whole session was
three places: `sub 0` (nothing resolved yet, at connect), `sub 10010` (Chapel, not in the wire), and
`sub 62000` — Liurnia, which in that seed genuinely *is* a target-0 region. Not one of the non-zero
regions was ever visited, and the earlier logs have the same shape. `sphere target 0` is evidence of
nothing until the sample includes a region the wire puts above 0.

One honest residual: 14 of the 18 wired regions sit at target 0, so the floor does legitimately do
most of the difficulty work. That is the `num_regions` sphere structure, not a defect — and it means
floor tuning matters more than the gradient does, which is the opposite of where the thread was going.

## What survives, and what this change does about it

`sub 0` and `sub 10010` both resolved unmapped, fell back to the floor tier, and **198 and 42 enemies
were swept in them** — rungs applied, and since #346 phase 1b, down states too. We were making a
difficulty statement about regions we could not name.

`tier_for_region` now returns `Option<usize>`, and the sweep declines outright when it is `None`.

This is not a new principle. `presumed_native_tier` already returns `None` for an enemy it cannot
place, and `scale_action` leaves that enemy exactly as vanilla shipped it — "absence is the safe
state, and it is load-bearing". A region we cannot identify is the same fact about a bigger object.

## Which regions this actually exposes — I got this wrong first, so here is the check

My first draft of this note said "unkept and sealed regions are still physically walkable, so this is
common in ordinary play". Alaric pushed back — region lock should kick you out — and he is right.
The claim came from an older note about a player reaching Rykard with Mt. Gelmir unkept, which I
carried forward as a premise instead of verifying.

Read out of `greenfield/eldenring/features/area_locks.py` and `region_play_ids.py`:

- `areaLockFlags` is emitted for **ALL 30 regions / 116 buckets**, kept and sealed alike. **Zero
  regions are ungated** — every one resolves an open flag. A sealed region's range is keyed to a flag
  that is never received and never lit, so the kick-watch permanently ejects the player. That was a
  deliberate 2026-07-08 fix, and its comment says why: without it "you can walk into a sealed
  sub-area ... where vanilla-suppress fires by item-id but there's no active check to grant -> DEAD
  DROPS."
- The scaling wire loops `for pid in REGION_PLAY_IDS[region]` over each kept region, so a kept region
  has **every** one of its buckets wired. There are no unscaled pockets inside a region you can fight
  through — which was the failure mode worth worrying about, Altus and Caelid each having 13 buckets.

**So the exposed population is the space outside `REGION_PLAY_IDS` entirely:** Roundtable Hold, the
Chapel of Anticipation (`sub 10010`, verified absent from the table), and the transient `sub 0` at
connect. Those are exactly the places that should never have carried a difficulty statement. This is
a fix, not a trade.

Enemies already carrying our state keep it. We stop touching the region; we do not undo it.

🛑 One thing this does NOT explain, and it is now an anomaly rather than a premise: a player reached
Rykard with Mt. Gelmir not among his kept regions. If every region is gated and sealed locks never
open, that should not have been possible. Worth its own issue — the candidates are a hole in the
kick-watch, a third-party loader, or that arena's bucket belonging to a different region.

## The toast had to move with it

`RegionScaling::Defaulted(usize)` carried the floor tier so the entry announcement could say "using
the floor, 1.14x". The moment the sweep stopped applying that, the announcement became actively
false — worse than the "tier 0" rendering the enum was originally built to prevent, which was merely
misleading.

It is now `RegionScaling::Unscaled`, carrying no number at all, and the toast reads "enemy scaling
not set for this area; enemies here are unchanged". A test asserts the string contains no tier and no
multiplier, because carrying no value is the only thing that makes the old failure impossible rather
than merely unlikely.

`the_announced_tier_is_the_applied_tier` survived the premise change untouched in spirit: "announced
equals applied" now includes announcing nothing when nothing is applied, and bucket `99999` is in its
list precisely to exercise that arm.

## Logging

`enemy-scaling: region N is not in the sphere wire -- left VANILLA`, once per region. The sweep is
throttled rather than one-shot, so an unconditional log would repeat for as long as the player stands
in an unwired area; keyed on the region so a transition still reports. That case is worth seeing — at
connect the region reads 0 until the game resolves one, and a sweep that silently does nothing is
otherwise indistinguishable from a broken sweep.
