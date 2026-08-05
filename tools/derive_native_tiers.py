#!/usr/bin/env python3
"""Derive per-npc NATIVE scaling tiers from NpcParam, and emit `er-logic/src/native_tiers.rs`.

WHY THIS EXISTS (issue #346). The client re-tiers enemies onto the vanilla area-scaling ladder
(`7010`..`7200`). An enemy that vanilla ships WITHOUT a ladder rung is hand-tuned -- its base stats
already are its difficulty -- so applying a rung to it is not a normalization, it is a raw multiplier
on top of tuning that already accounts for where you meet it. 2825 of 7039 NpcParam rows are in that
class. Without a notion of native strength there is no way to tell "this enemy should be scaled up"
from "this enemy is already stronger than the target".

WHY `getSoul` AND NOT `hp`. Measured on the 4214 rows that DO carry a rung:

  * effective HP (`hp` x the rung's maxHpRate) is NOT monotone in rung and the interquartile bands
    overlap badly -- rung 7010's q3 is 834 while rung 7090's q1 is 481. HP-derivation is noise.
  * `getSoul` -- the rune reward -- has **Spearman 0.996** against rung, with tight bands and only
    two local inversions (7050/7060, 7140/7150). It is the game's own authored encoding of how hard
    the designers thought an enemy was, which is exactly the quantity we need.

So: fit rung <- getSoul on the RUNGED population (which has both), then read the fit off for the
unrunged rows that have a `getSoul`.

WHAT THIS DELIBERATELY DOES NOT DO. 2440 of the 2825 unrunged rows have `getSoul == 0`, and every
named boss is among them -- boss rune rewards live in GameAreaParam, per arena, not in NpcParam.
Those rows are simply ABSENT from the emitted table, and absence means "never touch, at any tier".
Absence is the safe state by construction: a row nobody has classified -- including one a future
patch adds -- defaults to vanilla rather than to a buff. Covering bosses needs the GameAreaParam
arena join and its OWN calibration curve (arena rune values are on a different scale; reusing this
curve on them would be a silent unit error). That is deliberately out of scope here.

TIES RESOLVE UPWARD, ALWAYS. Overestimating an enemy's native strength means the up-only gate
declines to scale it: a balance blemish. Underestimating it means we apply a rung to something
already strong: a progression wall. Every boundary in this file rounds toward the stronger rung.

Usage:  python3 tools/derive_native_tiers.py --npc-param /path/to/NpcParam.csv \
                                             --out crates/er-logic/src/native_tiers.rs
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import statistics
import sys
from collections import defaultdict

# The 20 base rungs, in ladder order. Mirrors `SCALING_TIERS` in er-logic/src/scaling.rs; the
# ladder-mirror gate is what keeps the two from drifting.
BASE_RUNGS = [7010 + 10 * i for i in range(20)]
SLOTS = [f"spEffectID{i}" for i in range(32)]


def rung_index(param_id: int) -> int | None:
    """Ladder index (0..19) for a carried SpEffect id, or None if it is not a rung.

    The DLC block `20007xxx` is the SAME ladder re-emitted in the DLC's +20,000,000 param space and
    running further up; anything from it is at or above the top of the base ladder, so it clamps to
    the top index. That is the honest reading -- we cannot express those multipliers on the base
    ladder anyway.
    """
    if param_id in BASE_RUNGS:
        return BASE_RUNGS.index(param_id)
    if 20007000 <= param_id < 20008000:
        return len(BASE_RUNGS) - 1
    return None


def read_rows(path: str) -> list[dict]:
    with open(path, encoding="utf-8-sig", newline="") as fh:
        return list(csv.DictReader(fh))


def as_int(row: dict, key: str) -> int:
    try:
        return int(row.get(key, "") or 0)
    except ValueError:
        return 0


def carried_rung(row: dict) -> int | None:
    for slot in SLOTS:
        idx = rung_index(as_int(row, slot))
        if idx is not None:
            return idx
    return None


def isotonic(values: list[float]) -> list[float]:
    """Pool-adjacent-violators: the shortest monotone non-decreasing sequence closest to `values`.

    The rung->getSoul curve is monotone in truth (harder enemy, bigger reward) but the SAMPLE has
    two local inversions. Pooling them is the principled fix: it merges the offending rungs into one
    step rather than inventing an ordering the data does not support.
    """
    out = [(v, 1) for v in values]
    i = 0
    while i < len(out) - 1:
        if out[i][0] <= out[i + 1][0]:
            i += 1
            continue
        (v0, w0), (v1, w1) = out[i], out[i + 1]
        out[i : i + 2] = [((v0 * w0 + v1 * w1) / (w0 + w1), w0 + w1)]
        i = max(i - 1, 0)
    flat: list[float] = []
    for v, w in out:
        flat.extend([v] * w)
    return flat


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--npc-param", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--game-build", default="1.16.2 (Steam depot 1245621)")
    args = ap.parse_args()

    raw = open(args.npc_param, "rb").read()
    sha = hashlib.sha256(raw).hexdigest()
    rows = read_rows(args.npc_param)

    runged: dict[int, list[int]] = defaultdict(list)
    unrunged: list[dict] = []
    for row in rows:
        idx = carried_rung(row)
        if idx is None:
            unrunged.append(row)
        else:
            soul = as_int(row, "getSoul")
            if soul > 0:
                runged[idx].append(soul)

    # --- calibrate: median getSoul per rung, then force monotone -------------------------------
    present = sorted(k for k in runged if len(runged[k]) >= 5)
    medians = [statistics.median(runged[k]) for k in present]
    fitted = isotonic(medians)

    # COLLAPSE POOLED RUNGS FIRST. The isotonic fit merges rungs the data cannot separate (here the
    # top three all land on the same fitted median), and a group of rungs sharing one median is one
    # band, not several. Emitting a boundary per rung would produce DUPLICATE edges, and a duplicate
    # edge is not a cosmetic problem: the first match wins, so the pooled rungs above it become
    # unreachable and the bias-up rule silently inverts to bias-down. Each pooled group therefore
    # contributes ONE band, carrying the group's HIGHEST index -- ties resolve upward.
    groups: list[tuple[float, int]] = []  # (fitted median, highest rung index in the group)
    for value, idx in zip(fitted, present):
        if groups and abs(groups[-1][0] - value) < 1e-9:
            groups[-1] = (groups[-1][0], max(groups[-1][1], idx))
        else:
            groups.append((value, idx))

    # Band boundaries at GEOMETRIC midpoints between adjacent group medians. Geometric because
    # getSoul spans two orders of magnitude and its steps are multiplicative, not additive; an
    # arithmetic midpoint would put nearly every boundary up against the lower rung.
    bounds: list[tuple[float, int]] = []
    for i in range(len(groups) - 1):
        lo, hi = groups[i][0], groups[i + 1][0]
        edge = (lo * hi) ** 0.5 if lo > 0 and hi > 0 else (lo + hi) / 2
        bounds.append((edge, groups[i][1]))
    top_index = groups[-1][1]

    def native_for(soul: int) -> int:
        """Lowest rung index whose band contains `soul`. Ties round UP -- see module docstring."""
        for edge, idx in bounds:
            if soul < edge:
                return idx
            if soul == edge:
                continue  # exactly on a boundary: fall through to the STRONGER rung
        return top_index

    derived = sorted(
        (as_int(r, "ID"), as_int(r, "getSoul"), native_for(as_int(r, "getSoul")))
        for r in unrunged
        if as_int(r, "getSoul") > 0
    )

    # THE AREA FALLBACK'S EXCLUSION SET (issue #346), keyed PER CHARACTER.
    #
    # `nameId` is the healthbar name: ordinary enemies have none (6323 of 7039 rows are nameId 0),
    # while bosses and named NPCs do -- and their rune reward lives in GameAreaParam, per arena, so
    # they read `getSoul == 0` here.
    #
    # WHY IT IS EMITTED. The client may attribute a strength to an unrunged enemy from the AREA it
    # stands in, which is right for trash tuned to that ground and WRONG for these: their base
    # already assumes you meet them late, so an area-derived delta multiplies on top of endgame
    # tuning. Measured 2026-08-05: a Vyke row and Vyke's Finger Maiden, area-placed at tier 11 in a
    # Liurnia whose ground is index 5, came out "crazy strong" in play.
    #
    # ⭐⭐⭐ WHY PER CHARACTER AND NOT PER ROW. One character owns several rows and they DISAGREE:
    # Vyke has a 500-reward row and three reward-less ones; Gideon has three at 4000 and one at 0.
    # Keyed per row, which variant the game happens to instantiate decides whether that character is
    # placed by its own reward or left vanilla -- two encounters with "Vyke" scaling two ways, with
    # nothing in the data marking them as the same character. Keyed per nameId, one character has one
    # behaviour, which is the thing a player (or a bug report) can actually reason about.
    #
    # THE COST, PAID DELIBERATELY: 136 rows carrying a REAL reward are excluded with their siblings
    # (Gideon 4000, Dung Eater 3000, Arghanthy 5040, Blaidd/Fia/Latenna 2000). Those are measurements
    # taken on the enemy itself and we are choosing not to use them. It lands in the UNDER-scaled
    # direction -- a blemish, never a wall -- which is the side of the asymmetry this issue keeps.
    # It does not widen the character set at all: 137 characters either way, 275 rows -> 411.
    by_name: dict[int, list[dict]] = defaultdict(list)
    for row in unrunged:
        name_id = as_int(row, "nameId")
        if name_id:
            by_name[name_id].append(row)
    carved_names = {
        name_id
        for name_id, rs in by_name.items()
        if any(as_int(r, "getSoul") <= 0 for r in rs)
    }
    area_excluded = sorted(
        as_int(r, "ID") for r in unrunged if as_int(r, "nameId") in carved_names
    )

    n_zero = sum(1 for r in unrunged if as_int(r, "getSoul") <= 0)
    n_runged = sum(len(v) for v in runged.values())

    with open(args.out, "w", encoding="utf-8", newline="\n") as fh:
        w = fh.write
        w("// @generated by tools/derive_native_tiers.py -- DO NOT EDIT BY HAND.\n")
        w("//\n")
        w("// Regenerate, never hand-merge. A conflict in this file is resolved by re-running the\n")
        w("// script, not by picking a side; CI asserts the emitted bytes match a re-derivation.\n")
        w("//\n")
        w(f"// source:      NpcParam.csv  sha256 {sha}\n")
        w(f"// game build:  {args.game_build}\n")
        w(f"// population:  {len(rows)} rows -- {n_runged} runged (calibration set),\n")
        w(f"//              {len(derived)} unrunged with a rune reward (emitted below),\n")
        w(f"//              {n_zero} unrunged without one (ABSENT on purpose = never touched),\n")
        w(f"//              {len(area_excluded)} rows across {len(carved_names)} NAMED characters are excluded\n")
        w("//              from the AREA fallback (below) -- a set that CROSSES the split above,\n")
        w("//              since a carved character's rewarded rows go with its reward-less ones.\n")
        w("\n")
        w("//! Native scaling tiers for enemies vanilla ships WITHOUT a ladder rung (issue #346).\n")
        w("//!\n")
        w("//! An `npc_param_id` present here has a derived NATIVE ladder index: how strong the game's\n")
        w("//! own designers thought it was, read off the rune reward. An `npc_param_id` ABSENT here\n")
        w("//! has no native tier we can defend from its OWN stats -- see `native_tier`, and see\n")
        w("//! `NAMED_UNREWARDED` for the subset the area may not speak for either.\n")
        w("//!\n")
        w("//! The key is `ChrIns::npc_param_id` (8-9 digits), NOT `ChrIns::npc_id` (the 4-digit\n")
        w("//! chr/model id). The two id spaces OVERLAP, so keying this on the wrong one resolves a\n")
        w("//! few rows to confident WRONG answers rather than to nothing.\n")
        w("\n")
        w("/// `(npc_param_id, native ladder index)`, sorted so lookup can binary-search.\n")
        w("pub const NATIVE_TIERS: &[(i32, u8)] = &[\n")
        for npc_id, soul, idx in derived:
            w(f"    ({npc_id}, {idx}), // getSoul {soul}\n")
        w("];\n\n")
        w("/// The calibration curve, kept for the tests that prove the emitted table follows it.\n")
        w("/// `(upper getSoul bound, ladder index)` -- a reward BELOW the bound takes that index.\n")
        w("pub const GETSOUL_BANDS: &[(f32, u8)] = &[\n")
        for edge, idx in bounds:
            w(f"    ({edge:.3f}, {idx}),\n")
        w("];\n\n")
        w(f"/// Index taken by any reward above the last band.\npub const TOP_BAND_INDEX: u8 = {top_index};\n")
        w("\n")
        w("/// Rows the AREA fallback may not speak for, keyed PER CHARACTER (`nameId`).\n")
        w("///\n")
        w("/// A named character with ANY unrunged, reward-less row has ALL of its unrunged rows here.\n")
        w("/// 🛑 Their bases already assume a late encounter, so an area-derived delta multiplies on top\n")
        w("/// of endgame tuning -- measured in play 2026-08-05, when a Vyke row and Vyke's Finger Maiden\n")
        w("/// were area-placed at tier 11 in a Liurnia whose ground reads index 5.\n")
        w("///\n")
        w("/// ⭐⭐⭐ PER CHARACTER, NOT PER ROW, because a character's rows disagree: Vyke has a\n")
        w("/// 500-reward row and three reward-less ones, Gideon three at 4000 and one at 0. Keyed per\n")
        w("/// row, which variant spawned would decide how that character scales. The cost is paid\n")
        w("/// knowingly: rows with a REAL reward are excluded alongside their siblings, which lands in\n")
        w("/// the UNDER-scaled direction -- a blemish, never a wall.\n")
        w("///\n")
        w("/// Sorted, for binary search.\n")
        w("pub const AREA_EXCLUDED: &[i32] = &[\n")
        for npc_id in area_excluded:
            w(f"    {npc_id},\n")
        w("];\n")

    print(f"rows={len(rows)} runged={n_runged} derived={len(derived)} absent={n_zero}")
    print(f"bands={len(bounds)} top_index={top_index}")
    print("fitted medians:", [round(v, 1) for v in fitted])
    return 0


if __name__ == "__main__":
    sys.exit(main())
