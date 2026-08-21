#!/usr/bin/env python3
"""Audit the marker flag band (`er-logic/src/marker.rs` `FlagBand`, 75000..=75119) against the
decompiled vanilla corpus, so the band's "nothing else touches it" claim is MEASURED, not reasoned.

WHY THIS EXISTS (issue #338). The marker band shipped as a PLACEHOLDER whose emptiness was argued
from legacy-grace allocation arithmetic (graces end at 74351, so the 75000 tail is free). The
module doc made the audit an explicit precondition for trusting the band, because the failure mode
is not theoretical: a foreign PRESENT write plus garbage in the IDENT bits reads as a VALID marker
with the wrong identity -> `WrongSaveAtConnect` for a character that never left its seed. Players
run third-party mods in-process; "we believe vanilla does not write here" is not a defence.

WHAT IT CHECKS, against an `elden_ring_artifacts/` tree:

  1. EMEVD (`event/**/*.emevd.dcx.js`) -- every decompiled event script.
     a. LITERAL scan: any integer in the band, anywhere in the file, reported with context. This is
        deliberately position-blind: it catches flag ids, batch endpoints, and literals feeding a
        VARIABLE (an in-band value held in a variable has to come from somewhere).
     b. RANGE scan: the six verbs that take a (start, end) span -- BatchSetEventFlags,
        BatchSetNetworkconnectedEventFlags, RandomlySetEventFlagInRange, CountEventFlags,
        AnyBatchEventFlags, AllBatchEventFlags -- parsed for literal endpoints and checked for
        overlap with the band. A batch op spanning the band writes it without naming any id in it,
        which the literal scan alone would miss.
  2. ESD talk scripts (`talk/**/*.py`) -- literal scan as 1a. The third award corpus.
  3. Everything else under the artifacts root (param CSVs, FMG text, ...) -- literal scan as 1a,
     but reported as NON-SCRIPT: only a script or the engine can write an event flag, so a param
     cell in the band's numeric range (AtkParam_Pc row ids 75000..75106, referenced by the
     `refId1..4` of Magic rows 7500/7510; GameAreaParam's `bonusSoul = 75000`) is listed for
     completeness and never decides the exit code.

WHAT IT CANNOT SEE, and says so: engine-hardcoded flag moves that no script expresses (the NG+
reset is the known one), and the binary .dcx flag-allocation lists. Those stay reasoning, and the
marker module doc names them.

Exit 0 = no script touches the band (non-script literals may still be listed). Exit 1 = at least
one EMEVD/ESD finding, each printed with file, line, and the matched text -- a finding is a REVIEW
ITEM, not automatically a veto.

Usage:  python3 tools/audit_marker_flag_band.py [--artifacts /path/to/elden_ring_artifacts]
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

# The band under audit. Mirrors `FlagBand::PLACEHOLDER` in crates/er-logic/src/marker.rs:
# base 75000, 120 flags, i.e. the inclusive range 75000..=75119. If the constant moves, this
# moves -- the audit is only meaningful for the band the code actually writes.
BAND_LO = 75_000
BAND_HI = 75_119

# Any integer literal inside the band. 75000..75099 | 75100..75119.
BAND_LITERAL = re.compile(r"\b(750\d\d|751[01]\d)\b")

# Verbs whose first two integer arguments are an inclusive (start, end) flag span. Enumerated by
# scanning every `*EventFlag*(` call name in the corpus (2026-08-21: 14 verbs, ~21k calls); every
# other verb is single-id and is covered by the position-blind literal scan.
RANGE_VERBS = (
    "BatchSetEventFlags",
    "BatchSetNetworkconnectedEventFlags",
    "RandomlySetEventFlagInRange",
    "CountEventFlags",
    "AnyBatchEventFlags",
    "AllBatchEventFlags",
)
RANGE_CALL = {
    verb: re.compile(re.escape(verb) + r"\(\s*(\d+)\s*,\s*(\d+)") for verb in RANGE_VERBS
}

# Text files worth scanning outside event/ and talk/. Binary containers (.dcx) are skipped:
# parsing the flag-allocation lists needs WitchyBND and is out of scope for a text audit.
TEXT_SUFFIXES = {".js", ".py", ".csv", ".txt", ".xml", ".json", ".tsv", ".yml", ".yaml"}


def scan_literals(path: Path, findings: list[str]) -> int:
    """Report every in-band integer literal, with line context. Returns literals seen."""
    seen = 0
    try:
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError as exc:
        findings.append(f"UNREADABLE {path}: {exc}")
        return 0
    for lineno, line in enumerate(lines, 1):
        for match in BAND_LITERAL.finditer(line):
            seen += 1
            findings.append(
                f"LITERAL {path}:{lineno}: `{match.group(1)}` in: {line.strip()[:160]}"
            )
    return seen


def scan_ranges(path: Path, findings: list[str]) -> tuple[int, int]:
    """Check every literal (start, end) span of the range verbs for band overlap."""
    ops = 0
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return 0, 0  # already reported by the literal pass
    for verb, pattern in RANGE_CALL.items():
        for match in pattern.finditer(text):
            ops += 1
            start, end = int(match.group(1)), int(match.group(2))
            if start <= BAND_HI and end >= BAND_LO:
                findings.append(
                    f"RANGE {path}: `{verb}({start}, {end}, ...)` spans "
                    f"{max(start, BAND_LO)}..{min(end, BAND_HI)} of the band"
                )
    return ops, len(list(BAND_LITERAL.finditer(text)))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--artifacts",
        type=Path,
        default=Path(__file__).resolve().parents[3] / "elden_ring_artifacts",
        help="path to the elden_ring_artifacts tree (default: beside the superproject clone)",
    )
    args = parser.parse_args()
    root: Path = args.artifacts
    if not root.is_dir():
        print(f"artifacts tree not found: {root}")
        print("point --artifacts at an elden_ring_artifacts/ checkout (Windows-only bundle).")
        return 2

    findings: list[str] = []
    # Param cells, FMG text and other non-script literals go here. Event flags are writable only
    # by a script (EMEVD/ESD) or the engine -- a CSV cell or a message string CANNOT set flag
    # 75000, however much its value resembles one. These are printed for completeness (a human
    # confirms each is not-a-flag once) but never decide the exit code.
    non_script: list[str] = []
    emevd_files = esd_files = other_files = 0
    range_ops = 0

    for path in sorted(root.rglob("*")):
        if not path.is_file() or path.suffix.lower() not in TEXT_SUFFIXES:
            continue
        rel = path.relative_to(root)
        if str(rel).startswith("event" + "\\") or str(rel).startswith("event/"):
            if path.name.endswith(".emevd.dcx.js"):
                emevd_files += 1
                ops, _ = scan_ranges(path, findings)
                range_ops += ops
                scan_literals(path, findings)
        elif str(rel).startswith("talk" + "\\") or str(rel).startswith("talk/"):
            if path.suffix == ".py":
                esd_files += 1
                scan_literals(path, findings)
        else:
            other_files += 1
            scan_literals(path, non_script)

    print(f"band under audit: {BAND_LO}..{BAND_HI} (inclusive, 120 flags)")
    print(f"EMEVD scripts scanned: {emevd_files}; range/batch ops checked: {range_ops}")
    print(f"ESD talk scripts scanned: {esd_files}; other text files scanned: {other_files}")
    if non_script:
        print(
            f"\n{len(non_script)} non-script literal(s) in the band's numeric range (params/FMG --"
            " cannot write event flags; listed for review):"
        )
        for item in non_script:
            print(f"  {item}")
    if findings:
        print(
            f"\n{len(findings)} SCRIPT FINDING(S) -- review each; a hit is not automatically"
            " a veto:"
        )
        for finding in findings:
            print(f"  {finding}")
        return 1
    print("\nCLEAN: no script literal or range op touches the band.")
    print("Not covered (engine-side, unscriptable): the NG+ flag reset and the binary")
    print("flag-allocation .dcx lists. Those remain reasoning, per the marker module doc.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
