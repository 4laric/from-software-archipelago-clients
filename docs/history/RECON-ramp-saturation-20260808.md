# RECON — the sphere ramp saturates against a capped ceiling (2026-08-08)

## Symptom, from a player

boblerrr, Discord 2026-08-08, unprompted, on a v0.3.7-era run:

> "scaling was honestly good for the most part my only complaint is potentially tier 11 ... i feel
> like i reached tier 11 too fast ... smaller scaling steps maybe ... it skipped tier 2 completely.
> it went tier 1 tier 4 tier 8 tier 11 i think"

Two complaints in one breath — steps too coarse, top reached too early. They are one defect.

## Measurement

Every distinct `(sphere target -> tier)` pair in `archipelago-2026-08-07 (8).log`.
Seed: `completion_scaling 4`, `completion_scaling_ceiling 4.844` (= tier 11), floor 0,
`12 region targets, max 10000`.

| sphere target | tier | speffect | regions |
|---|---|---|---|
| 0     | 0  | 7010 | 21000, 21010, 69300 |
| 2000  | 4  | 7050 | 68410, 68500 |
| 4000  | 8  | 7090 | 68300 |
| 6000  | 11 | 7120 | 20000 |
| 8000  | 11 | 7120 | 41020, 68400 |
| 10000 | 11 | 7120 | 69410 |

The target grid is quantized to 2000. **Targets 6000, 8000 and 10000 all resolve to tier 11**, so the
deepest kept region is scaled identically to a 6000 region: the top 40% of that seed's spine is flat.

## Cause

`tier_for_target` normalised the run's depth fraction against the LADDER and clamped afterwards:

```rust
let tier = (frac * (NUM_TIERS - 1) as f32).round() as usize;   // NUM_TIERS = 20
tier.clamp(floor, ceiling)
```

`round(frac * 19)` gives 0, 3.8, 7.6, 11.4, 15.2, 19 for those six targets. Everything above the
ceiling is discarded onto the ceiling. So a capped seed does not merely *stop* at its cap, it
**saturates** on it, and the ramp climbs at 1.9 tiers per 1000 target until it does — which is the
"skipped tier 2" half of the same report.

The formula reproduces the log tier-for-tier and speffect-for-speffect.

## Fix

Normalise to the band the seed actually chose:

```rust
let tier = floor + (frac * (ceiling - floor) as f32).round() as usize;
```

Same six targets, same ceiling: **0, 2, 4, 7, 9, 11** — six distinct tiers, no tie, ends unmoved.

⭐ `region_scaling_toast` already had this model: it prints the rung as a position *within*
`[floor, ceiling]`, "because a tier index means nothing outside the band the seed chose". Before this
fix the two disagreed, and a player on any of those three deep regions read the same `tier 11 of 11`.

## Why no existing test caught it

Every `tier_for_target` assertion in the suite was either **uncapped** — where the two formulas are
algebraically identical, since `ceiling == NUM_TIERS - 1` makes the multipliers equal — or an
**endpoint** of a capped band (`frac` 0 or 1), which both formulas fix.

The one combination that mattered, **capped AND interior**, was never asserted. That is exactly where
the defect lived. A test that only checks endpoints cannot see a curve's shape.

Three tests added: `a_capped_ramp_does_not_saturate_at_its_ceiling` (the motivating case, asserting
*saturation* rather than specific rungs so a future reshape is not a false regression),
`the_interior_of_a_capped_ramp_is_distributed_across_the_band`, and
`a_tighter_ceiling_still_spreads_across_the_spine`.

## Consequence for the ceiling default

This is a MECHANISM fix and deliberately changes **no default**. But it is a precondition for the
balance change under discussion (`completion_scaling_ceiling` 4.844 -> ~3.7):

**Lowering the ceiling alone would have made the reported symptom worse.** A lower cap is reached at
a shallower target, so the flat zone grows — at tier 9 saturation would start around target 4700,
flattening ~53% of the spine instead of ~40%. With this fix a tighter band stays spread, which is
what makes the cap a usable knob.

Ladder anchors for that decision: idx 8 = 3.25x HP / 2.279x atk, **idx 9 = 3.703x HP / 2.473x atk**,
idx 11 = 4.844x HP / 3.244x atk.

⚠️ `ceiling_tier_from_multiplier` is `rposition(|t| t.hp <= ceil_mult)` and idx 9's hp is **3.703**,
so a literal `3.7` resolves to **idx 8**, not 9. Pass >= 3.703 to land on 3.703x.

## Not changed

- No default moves; no option is added, narrowed or renamed.
- No slot_data key is read or written differently, so `CONTRACT_HASH` does not move and no version
  bump is implied. `requiresClientFeatures: scaling_ceiling` still gates the key's presence exactly
  as before.
- Uncapped seeds (the shipped default, `ceiling == NUM_TIERS - 1`) are bit-identical.
