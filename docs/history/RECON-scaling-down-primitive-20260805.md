# RECON: is there a way to scale an enemy DOWN? (2026-08-05)

Context: issue [#346](https://github.com/4laric/er-archipelago/issues/346). The runtime scaling model
applies one rung of the vanilla ladder to every swept enemy. The ladder's bottom rung is `7010` =
**1.141x HP**, so an enemy vanilla ships *without* a rung — every named boss, and the hand-tuned NPCs
the Nexus report was about — can only ever be scaled **up**. An endgame-tuned NPC holding a check in a
sphere-0 region is a progression wall, and our "floor" makes it 14% worse.

Fixing that properly needs two things we did not have: a notion of an enemy's NATIVE strength, and a
primitive that can express a multiplier **below 1.0**. This document is the search for the second one.

## Method

`SpEffectParam.csv` extracted from the project's `gen_inputs.db` datamine bundle — the full table,
**11,325 rows**. Every row compared field-by-field against row `7000`, the identity row (1.0 everywhere),
so "side effect" means "differs from doing nothing", not "differs from the default".

## Finding 1 — `7400..7680` descends, but never crosses 1.0

| id | maxHpRate | physAtkPowerRate | physDiffenceRate | spCategory | effectEndurance | haveSoulRate |
|---|---|---|---|---|---|---|
| 7400 | 3.434 | 1.902 | 1.172 | 0 | -1 | 5 |
| 7430 | 2.355 | 1.680 | 1.175 | 0 | -1 | 5 |
| 7470 | 1.523 | 1.314 | 1.179 | 0 | -1 | 3 |
| 7510 | 1.253 | 1.141 | 1.183 | 0 | -1 | 3 |
| 7550 | **1.001** | 1.094 | 1.200 | 0 | -1 | 2 |
| 7560..7680 | 1.015 | 1.134 | 1.200 | 0 | -1 | 2 |

It floors at **1.001x HP / 1.094x attack** and then goes flat. The `haveSoulRate` column stepping
5 -> 4 -> 3 -> 2 gives it away: this is a **co-op guest-count ladder** (the more phantoms in the
session, the beefier the enemies), not a difficulty-down ladder. It is not a down primitive.

## Finding 2 — nothing in `7000..8000` is below 1.0 HP. Count: zero.

The rest of the block, for the record: `7000` is the identity row; `7210..7280` is a separate
5.3-5.8x run; `7800..7902` is `spCategory 140`, 1.25x-3.21x HP.

## Finding 3 — no SINGLE row in the game scales both HP and attack down

| query over all 11,325 rows | count |
|---|---|
| `maxHpRate < 1.0` | 20 |
| `physicsAttackPowerRate < 1.0` | 25 |
| **both** | **0** |

That is the headline. A one-row down-rung, the thing a "descending ladder" would have given us,
**does not exist anywhere in `SpEffectParam`**.

## Finding 4 — but a clean PAIR does, and `spCategory 0` stacks

`spCategory 0` effects stack — that is precisely why `scale_one` has to CLEAR the vanilla `70xx`
before applying ours. So two rows compose. Full delta against the identity row:

```
20018004  (8 differing fields)        20018002  (12 differing fields)      20018008  (11 fields)
  maxHpRate:      1 -> 0.25             physicsAttackPowerRate: 1 -> 0.3     physAtkPowerRate: 1 -> 0.7
  targetPriority: 0 -> 1                (+ magic/fire/thunder/dark 0.3)      (+ the other four 0.7)
  regist*ChangeRate: 2 -> 1 (x6)        targetPriority: 0 -> -0.5            regist*: 2 -> 1 (x6)
                                        regist*ChangeRate: 2 -> 1 (x6)
```

All three are `spCategory 0`, `effectEndurance -1` (infinite), `conditionHp -1` (unconditional), with
no `stateInfo`, no `vfxId`, no `cycleOccurrenceSpEffectId` and no icon. The `regist*ChangeRate: 2 -> 1`
block appears on essentially every row in the table, including the `70xx` ladder rungs themselves
(which set it to 2.018) — it is a column default, not a side effect.

- `20018004` + `20018002` = **0.25x HP, 0.30x all-element attack**
- `20018004` + `20018008` = 0.25x HP, 0.70x attack
- `20018008` alone = 1.0x HP, 0.70x attack

**What these rows are:** the `20018000..20018035` block is the DLC's summon/ally tuning set — the
`targetPriority` values (+1 on the squishy 0.25x-HP row, -0.5 on the damage-nerfed rows) read as
"draws aggro" / "deprioritized". The base-game `18000..18040` rows at the same offsets are all 1.0.
Using these on enemies is **off-label**; they are not a documented enemy ladder.

## What this means for the fix

1. Down-scaling is possible but **coarse**: HP has one usable step (0.25x), attack has four
   (0.3 / 0.45 / 0.7 / 0.95). A continuous `mult(target)/mult(native)` ratio cannot be expressed
   below 1.0 — the down side has to be a small set of named states, not a rung lookup.
2. `20018xxx` is outside BOTH `SCALING_ID_RANGE` (`7000..8000`) and `DLC_SCALING_ID_RANGE`
   (`20007000..20008000`). The sweep is stateless and re-derives everything from what the enemy
   currently carries, so anything we apply must also be something we recognise and can remove.
   The clear predicate needs an explicit **allowlist** of exactly the ids we apply —
   **not** a widened range, which would strip legitimate effects off DLC summons.
3. `targetPriority` is a real behaviour change (aggro/targeting), unmeasured on an enemy.
4. Nobody has ever confirmed a base-game enemy accepts a `20018xxx` row. **This must be probed live
   before anything is armed.** This file has already shipped one change that inferred a runtime
   meaning from a name, flagged it "UNVERIFIED IN-GAME", and broke enemy scaling for every player
   (see the `sweepable_characters` postmortem in `crates/eldenring-archipelago/src/scaling.rs`).

### Probe P1 — the gate on arming any down-state

Apply `20018004` + `20018002` to (a) one base-game trash enemy with a datamined HP value and (b) one
hand-tuned NPC. Pass requires all of:

- hits-to-kill falls to ~1/4 of baseline;
- damage taken from an identical attack falls to ~0.30x baseline, +/- 15%;
- a read-back of `special_effect.entries()` shows **both** ids carried;
- no vfx, no state anomaly, no visible break in aggro or search behaviour (watch spirit-ash targeting).

Any criterion failing = there is no down primitive in the game's param space, and the remaining
options are outside the current pure-runtime primitive.
