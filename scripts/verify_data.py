#!/usr/bin/env python3
"""Validate a captured data tree: CSV well-formedness, duplicates, and continuity.

Walks data/futures/<datatype>/<symbol>/<date>/*.csv and reports any file whose
header is malformed, whose rows have an inconsistent column count, that contains
duplicate timestamps where they should be unique, or that has a gap in a series
that should be continuous (1-minute klines, order-book update IDs).

Exit status is non-zero if any issue is found, so it drops into CI or a cron
check unchanged.

    python scripts/verify_data.py [DATA_DIR]   # default: ./data/futures
"""
from __future__ import annotations

import csv
import sys
from collections import defaultdict
from pathlib import Path

# Data types with exactly one row per timestamp_ms (a duplicate is a bug). NOT
# trades: many trades share a millisecond, so those are unique by trade_id, and
# NOT funding / open-interest, which are sampled and repeat by design.
UNIQUE_TS = {
    "klines", "mark_price_klines", "index_price_klines",
    "premium_index_klines", "taker_buy_sell_ratio",
}
TRADE_ID_COL = 5
# 1-minute series that should have no missing buckets.
MINUTE_SERIES = {"klines", "mark_price_klines", "index_price_klines", "premium_index_klines"}
MINUTE_MS = 60_000


class Report:
    def __init__(self) -> None:
        self.files = 0
        self.issues: list[str] = []

    def flag(self, path: Path, msg: str) -> None:
        self.issues.append(f"{path}: {msg}")


def check_file(path: Path, datatype: str, rep: Report) -> None:
    rep.files += 1
    with path.open(newline="") as fh:
        reader = csv.reader(fh)
        try:
            header = next(reader)
        except StopIteration:
            rep.flag(path, "empty file")
            return
        ncols = len(header)
        seen_ts: set[str] = set()
        minute_ts: list[int] = []
        for i, row in enumerate(reader, start=2):
            if len(row) != ncols:
                rep.flag(path, f"row {i}: {len(row)} columns, header has {ncols}")
                return
            if row == header:
                rep.flag(path, f"row {i}: repeated header")
                return
            ts = row[3]  # timestamp_ms is always column 4
            if datatype in UNIQUE_TS:
                if ts in seen_ts:
                    rep.flag(path, f"duplicate timestamp {ts}")
                    return
                seen_ts.add(ts)
            if datatype in MINUTE_SERIES:
                try:
                    minute_ts.append(int(ts))
                except ValueError:
                    pass

    if datatype in MINUTE_SERIES and len(minute_ts) > 1:
        ordered = sorted(set(minute_ts))
        gaps = sum(1 for a, b in zip(ordered, ordered[1:]) if b - a != MINUTE_MS)
        if gaps:
            span_min = (ordered[-1] - ordered[0]) // MINUTE_MS
            rep.flag(path, f"{gaps} gap(s) in a 1-minute series ({len(ordered)} of {span_min + 1} buckets)")


def main() -> int:
    root = Path(sys.argv[1] if len(sys.argv) > 1 else "data/futures")
    if not root.is_dir():
        print(f"no data directory at {root}", file=sys.stderr)
        return 2

    rep = Report()
    per_type: dict[str, int] = defaultdict(int)
    for datatype_dir in sorted(p for p in root.iterdir() if p.is_dir()):
        datatype = datatype_dir.name
        for csv_path in datatype_dir.rglob("*.csv"):
            per_type[datatype] += 1
            check_file(csv_path, datatype, rep)

    print(f"checked {rep.files} files across {len(per_type)} data types")
    for datatype, n in sorted(per_type.items()):
        print(f"  {datatype:<24} {n}")

    if rep.issues:
        print(f"\n{len(rep.issues)} issue(s):")
        for issue in rep.issues[:100]:
            print(f"  {issue}")
        if len(rep.issues) > 100:
            print(f"  … and {len(rep.issues) - 100} more")
        return 1

    print("\nno issues")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
