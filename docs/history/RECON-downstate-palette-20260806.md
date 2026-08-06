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

---

# Follow-up: the first live 1b log, and the two things it caught

Same day, from a build of PR #79. No errors, no crash.

**Every applied state matched the derivation exactly**, checked against `NATIVE_TIERS` on five
entities across four distinct lattice points — `[008]` at area index 2, `[004, 027]` at area index 5,
`[004, 008, 027]` for table tier 12, `[002, 004, 008]` for table tier 16.

**The HP half is confirmed in the field, twice, to the integer.** `523340014` went 1098 -> **274**
(0.25x exactly) and `1000000` went 1939 -> **1454** (0.75x exactly). The second is the one that
matters: it proves the `20018027` x `20018004` cancellation (3.0 x 0.25 = 0.75) works on a live
enemy — the row that was not in the original design at all.

Two entities carrying a 0.75 state showed HP unmoved 39s later. Either a recompute-timing artefact or
something real; one log cannot separate them, and nothing here is built on it.

## 1. `KeepDown` — the census was reporting the opposite of what happened

Region 0 logged `down-scaled 23 (settled 0)`, then `down-scaled 0 (settled 5)` with `left vanilla`
going 19 -> 37. **18 + 5 = 23, and 19 + 18 = 37.**

The area index goes `None` on a converged region **by design**: the histogram counts only
rung-AND-band carriers, and our own sweep strips them. So `presumed_native_tier` stops answering and
the 18 area-placed enemies resolve to `NoTouch`.

They kept their state — but only because `NoTouch` happens to return before the clear. Correct
behaviour resting on an accident, reporting itself as the opposite of the truth.

The asymmetry underneath is the real finding: **the up path self-anchors and the down path does
not.** An enemy scaled up carries a ladder rung afterwards, so every later sweep reads it as runged
and `Replace` re-derives it forever. A down state is not a rung.

`ScaleAction::KeepDown` makes it explicit and countable. Behaviourally identical to `NoTouch`, which
is precisely why it needed its own name.

## 2. `ClearDown` — a down state was unremovable

Found while writing (1)'s test, not in the log. `down_state_for` declines 34 of the 190 tier pairs
(the deadband), and `NoTouch` returns before the clear — so an enemy placed at one target and
re-swept at a target just below its native tier would keep a cut nothing justifies, forever. Same for
a target that rises to equal its native tier.

Safe to strip without replacing, uniquely on this path: the usual prohibition protects VANILLA state,
whose loss is irreversible because the sweep re-derives from what the enemy carries. A down state is
additive and ours.

## 3. `DOWN_TOLERANCE` — a near-tie cost a whole lattice step

`523210014`, native 9 at target 0, wanted 0.444 with a 0.45 state right there. A strict `<=` reached
past it to 0.315: **a 30% extra cut for a 1.4% miss.** The table has a worse case — native 10 -> 1
wants **0.450 exactly** and was still refused, because these are f32 ratios of f32 rates.

2% fixes 7 of the 156 acting pairs and moves median overshoot 1.214 -> 1.177. The acted-pair count is
unchanged, so it buys precision at zero coverage cost; a test asserts that count directly, because a
tolerance quietly becoming a second deadband is the way this goes wrong.

## Context that dwarfs all three

Every region in the log read `sphere target 0` or `unmapped`, so every one resolved to tier 0,
`ground > target` was true almost everywhere, and 1b fired its deepest states game-wide. In the same
session Liurnia's RUNGED enemies went `[7060, 7460]` -> `[7010]`, 382 -> 192 HP. **Liurnia is being
cut from both directions at once**, and neither half is 1b's doing. 1b only made the stuck sphere
target impossible to ignore.

---

# Follow-up 2: the area carve-out was direction-blind

From the second live log the same day. Both Liurnia buckets read `sphere target 0`, so tier 0
(`7010`, 1.141x HP) applied, while the census measured the ground at index **5** off 432
vanilla-shaped enemies, 422 of them agreeing. In bucket 62010 that produced:

- **216 runged enemies at ~0.50x vanilla** (1.141 / 2.266) — the floor, via `Replace`
- **312 of 355 unrunged untouched at 1.00x vanilla**
- 22 down-scaled, 20 area-placed

The floor was scaling ordinary trash *down* while the hand-tuned NPCs stayed at full strength — the
gap widening rather than closing, which is the "one super squishy, the other insanely tanky" report
arriving from the other direction.

The reason so few moved: `AREA_EXCLUDED` refuses the area's vouching for named, unrewarded
characters. **275 of its 411 rows have no `getSoul` tier either**, so they were `NoTouch` in BOTH
directions — unreachable by every mechanism the client has.

## The asymmetry

`area_may_vouch_for` exists because an area-derived delta multiplied on top of tuning that already
assumes the endgame is the v0.3.4 one-shotting bug — Vyke came out "crazy strong". Every word of that
justification is about scaling **up**. Downward the sign flips: attributing the ground to a
hand-tuned character makes it weaker, and this file's axiom is that under-scaling is a balance
blemish where over-scaling is a progression wall. Refusing to move it down protects nothing; it
leaves the wall standing.

So `presumed_native_tier_down` lets the area vouch for anyone, and `scale_action` consults the
guarded attribution for `Apply` and the permissive one for `Down`. The Vyke guard is untouched:
upward attribution still goes through `presumed_native_tier`, and nothing on the down path can
reproduce an up-scale.

## Why Vyke is the wrong case to calibrate on

Alaric set the rule, 2026-08-06: *"Vyke is fine but imagine this is a mountaintops fight and I have to
fight Okina. Or a gravesite plain start and I'm fighting Ancient Dragon Man."*

Vyke sits in Liurnia and came out merely reasonable — the mild end of the distribution, and a bad
place to set a rule. A randomised spine can open on a region whose hand-tuned duels assume the
endgame, and the carve-out was pinning every one of them at full strength precisely *because* they
are named and carry no rune reward. The trade is real and accepted: Vyke now takes a cut he does not
strictly need, so that Okina takes one he does.

## Census

`area-down N across M row(s) [ids]`, kept deliberately separate from `area-placed`. Up-placement and
down-placement now obey different rules, and one merged list would hide which rule moved a given
enemy. These are the enemies moved on their neighbours' evidence rather than their own, so this is
where a bad down-scale surfaces first — in our log rather than in a report.
