# RECON: the down-state palette, swept — and phase 1b armed on it

2026-08-06. Companion to `RECON-scaling-down-primitive-20260805.md`, which established that the
runtime scaling model can only MULTIPLY and that the down primitive would have to be an off-label
composition. This one replaces the sampled palette that note worked from with a swept one, and
records the decisions phase 1b was built on.

## Method

Every one of the 11,325 `SpEffectParam` rows in `gen_inputs.db` was filtered on `maxHpRate < 1.0`
and `physicsAttackPowerRate < 1.0`, and each candidate full-column-diffed against the identity row
`7000`. "Usable" means: `effectEndurance -1` (infinite), `conditionHp -1` (unconditional),
`spCategory 0` (so rows stack), no `vfxId` / `iconId` / `stateInfo` / `cycleOccurrenceSpEffectId`,
and the seven `effectTarget*` flags left at 1.

`regist*ChangeRate 2 -> 1` is excluded from every diff count below. It sits on essentially every row
in the param including the `70xx` ladder rungs — a column default, not a side effect.

## The four usable rows, and nothing else in the game qualifies

| id | attack | HP | diffs vs `7000` | note |
|---|---|---|---|---|
| `20018008` | 0.70 | 1.00 | 5 | the attack rates and nothing else — the cleanest row in the param |
| `20018027` | 0.45 | 3.00 | 6 | rates only; the 3x HP is cancellable |
| `20018002` | 0.30 | 1.00 | 6 | rates + `targetPriority 0 -> -0.5` |
| `20018004` | 1.00 | 0.25 | 3 | `maxHpRate` + `targetPriority 0 -> 1`; proven live by probe P1 |

`20018004` is the **only** clean HP-down row in the game. The other 19 sub-1.0 `maxHpRate` rows are
disqualified: `1420` (0.5x) zeroes all seven `effectTarget*` flags and doubles
`defEnemyDmgCorrectRate`; `330800` / `500135` / `6083000` / `6083200` / `6160000` are player
talismans (`iconId` + `addXStatus` + `effectTargetEnemy 0`); the rest are timed or a different
`spCategory`. So the original design's "the down side has ONE HP step" is now verified rather than
assumed.

Two attack rows look usable and are not. `18684` / `18685` (attack **0.0**) zero every
`effectTarget*` flag and every `*DamageCutRate` — a nullifier that can apply to nothing. `6109000`
(0.95x) is a talisman. `19389` (0.35x) rides a `maxHpRate 2`.

## What this changed about the design

The shipped three-state design was `D1 = {20018008}`, `D2 = {20018004, 20018008}`,
`D3 = {20018004, 20018002}` — 0.70 and 0.30 on the attack axis with nothing between, on the axis the
phase buckets by.

`20018027` was not in it. Its 3x HP is not a defect but the canceller: `3.0 x 0.25 = 0.75`, so
`{20018027, 20018004}` is **0.45x attack at 0.75x HP**, and it is *cleaner* than D3 — six rate-only
diffs, no `targetPriority` change at all.

Selecting against the ladder's own step across all 190 `native > target` tier pairs, `20018027`
appears in the answer for roughly half of them while `20018002` — the row probe P1 was built around,
and the only one carrying a behavioural field — is selected for a handful of the deepest gaps. The
states doing almost all of the work are built entirely from rows with no behavioural fields.

## The selection rule

The target ratio is the ladder's own, `attack(target) / attack(native)` — not a ratio anyone argued
for. Attack is primary (every reported death in the 2026-08-04/05 Nexus thread is damage TAKEN, and
boblerrr is the control: "didn't seem too bad, but I also didn't get hit by them"). HP breaks ties.
Rounding is DOWN on attack: never leave the enemy stronger than the step asked for.

### The deadband, and why it is 0.90

The coarsest tool available is 0.70x, so anything the ladder wants between 0.70 and 1.0 has no
expressible answer — overshoot, or do nothing. Four cutoffs, measured:

| cutoff | no-ops / 190 | median overshoot | Altus 7 -> 5 |
|---|---|---|---|
| 1.00 (never under-reduce) | 0 | 1.25x | `20018008` |
| 0.95 | 18 | 1.22x | `20018008` |
| **0.90 (shipped)** | **34** | **1.21x** | **`20018008`** |
| 0.85 | 49 | 1.18x | **NoTouch** |

0.90 leaves alone the 34 pairs whose ladder step is under 10% — firing a 30% nerf at a 5% problem is
not a fix — while still moving everything the ladder treats as a real difference.

At 0.85 the motivating case falls into the deadband and the phase silently no-ops the defect it was
built for. `altus_at_sphere_zero_is_scaled_down` asserts against that directly. Alaric's call, after
being shown all four side by side.

## The motivating case (CONTRIBUTING rule 11)

Altus at sphere 0, measured 2026-08-05: target tier 5, area index 7 off a 302-enemy sample. Runged
neighbours normalised down to 2.266x while **61 unrunged enemies stood at full Altus strength** in
the same field — lizzymagala's "one super squishy and weak, the other insanely tanky". That is not a
bug in the tier; it is `NoTouch` doing what it promised in a region whose ground is above the target.
It now resolves to `Down({20018008})`.

## Down is RELATIVE where Apply is ABSOLUTE

`Apply` puts the target rung on an enemy whose base already encodes its native strength, so it
multiplies on top — the hazard documented on `presumed_native_tier`, and the shape of the v0.3.4
one-shotting reports. A down state cannot work that way: there is no rung to replace, so the only
correct move is the ratio between the two tiers, and `scale_one`'s `Down` arm deliberately does NOT
apply the region's rung. Making `Apply` relative to match is a real question and is not attempted
here.

## What is NOT verified

- **The attack magnitude in game.** `20018004`'s 0.25x HP was measured exactly (3954 -> 988) by
  probe P1. The attack rates were not: Alaric was killed before damage-taken could be read. The desk
  case is strong — `20018002` differs from the proven `20018004` only in the rate columns and
  `targetPriority`, and the five columns it writes are the same five the ladder writes — but it is
  an argument, not a measurement. Shipped on Alaric's call to let play validate.
- The old pass criterion "damage taken ~0.30x +/- 15%" is retired as unmeasurable: damage is not
  linear in attack power, so a working rate reads below 0.30x by an amount set by the player's
  defence. The falsifier is damage taken UNCHANGED.
- **The client half did not compile in the sandbox.** `eldenring-archipelago` needs MSVC/imgui/detour
  and Windows CI is its only gate. `er-logic` was built and tested host-side: 740 pass.

## Read this first in the log

`down-scaled N (settled M)` on the region line. If `down-scaled` stays high and `settled` stays 0
across repeat sweeps of one region, the down half is churning rather than converging and
`settled_on_downstate` is not matching — the `residue 306` failure, in the down half.
