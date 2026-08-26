#!/usr/bin/env python3
"""Summarize a bb-ap-client `delivery-diagnostics.jsonl` (clients#445).

Groups terminal grant outcomes by item, status and INFERRED destination, and
prints a table meant to be pasted straight into clients#445.

    python3 summarize_delivery_diagnostics.py sessions/<seed>/delivery-diagnostics.jsonl

Why a script and not a client flag: the client's four positional arguments
(server, slot, config, ledger) are all required before it does anything, so a
`--summarize-diagnostics` flag would have to short-circuit the whole argument
parser and carry its own parse tests -- more client surface, and more of it on
the path a player launches, than this file is worth. The operator already
handles the .jsonl by hand (they mail it in like client.log); reading it with a
standalone script is the smaller honest option.

`inferred_destination` is an INFERENCE from read-back arithmetic. The client
cannot read Bloodborne's storage box. `storage_suspected` means "the cave
provably executed and the held stack did not absorb the delta", which is also
what a concurrent spend looks like. Do not report these counts as measured
storage routing.
"""

import argparse
import collections
import json
import sys


def load(path):
    records, malformed = [], 0
    with open(path, "r", encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            try:
                records.append(json.loads(line))
            except json.JSONDecodeError:
                malformed += 1
    return records, malformed


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("path", help="delivery-diagnostics.jsonl")
    args = parser.parse_args()

    records, malformed = load(args.path)
    if not records:
        print("no records in {}".format(args.path))
        return 0

    groups = collections.Counter()
    surpluses = collections.defaultdict(list)
    for record in records:
        key = (
            record.get("item_id_normalized"),
            record.get("lane") or "-",
            record.get("terminal_status", "?"),
            record.get("inferred_destination", "unknown"),
        )
        groups[key] += 1
        surplus = record.get("readback_surplus")
        if surplus is not None:
            surpluses[key].append(surplus)

    header = ("normalized_id", "lane", "terminal_status", "inferred_destination",
              "count", "surplus_min..max")
    rows = [header]
    for key, count in sorted(groups.items(), key=lambda item: (-item[1], str(item[0]))):
        normalized, lane, status, destination = key
        values = surpluses.get(key)
        span = "{}..{}".format(min(values), max(values)) if values else "-"
        rows.append((
            "0x{:08X}".format(normalized) if isinstance(normalized, int) else "?",
            lane, status, destination, str(count), span,
        ))

    widths = [max(len(row[i]) for row in rows) for i in range(len(header))]
    for index, row in enumerate(rows):
        print("  ".join(cell.ljust(widths[i]) for i, cell in enumerate(row)).rstrip())
        if index == 0:
            print("  ".join("-" * width for width in widths))

    total = len(records)
    suspected = sum(
        count for key, count in groups.items() if key[3] == "storage_suspected"
    )
    print("")
    print("{} terminal grants; {} inferred storage_suspected ({:.1f}%)".format(
        total, suspected, 100.0 * suspected / total))
    if malformed:
        print("{} unparseable line(s) skipped".format(malformed))
    print("inferred_destination is an INFERENCE from read-back arithmetic, not a "
          "read of the storage box; storage_suspected and a concurrent spend are "
          "indistinguishable to the client.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
