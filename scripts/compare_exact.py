#!/usr/bin/env python3
"""
Byte-exact, order-insensitive diff of the pipeline's Parquet tables against the
reference's CSV tables. Usage: compare_exact.py PIPELINE_OUT REFERENCE_OUT

scripts/compare.py is the organizers' checker and stays the judge for the dev
answer. This one exists for fuzzing: an ID may hold a tab or a `\\r`, which a
CSV reader does not round-trip, so here the Parquet rows are rendered to the
reference's line format and the lines are compared as bytes.
"""
import glob
import sys

import polars as pl

TABLES = {
    "user_daily": ["user_id", "day", "sessions", "events", "revenue_cents", "top_category"],
    "campaign_daily": ["campaign_id", "channel", "day", "sessions", "purchasing_sessions", "revenue_cents", "assisted_sessions"],
}


def text(value):
    if value is None:
        return b""
    if isinstance(value, bytes):
        return value
    return str(value).encode()


def main():
    out, ref = sys.argv[1], sys.argv[2]
    same = True
    for table, columns in TABLES.items():
        files = sorted(glob.glob(f"{out}/{table}/*.parquet"))
        frame = pl.concat([pl.read_parquet(f) for f in files]).select(columns)
        strings = [c for c in columns if frame.schema[c] == pl.String]
        frame = frame.with_columns([pl.col(c).cast(pl.Binary) for c in strings])
        ours = sorted(b",".join(text(v) for v in row) for row in frame.rows())
        lines = open(f"{ref}/{table}.csv", "rb").read().split(b"\n")
        theirs = sorted(line for line in lines[1:] if line)
        if ours != theirs:
            same = False
            a, b = set(ours), set(theirs)
            print(f"{table}: {len(ours)} vs {len(theirs)} rows; only in pipeline {len(a - b)}, only in reference {len(b - a)}")
            for line in sorted(a - b)[:5]:
                print("  pipeline :", line)
            for line in sorted(b - a)[:5]:
                print("  reference:", line)
        else:
            print(f"{table}: {len(ours)} rows match")
    print("MATCH" if same else "DIFFER")
    return 0 if same else 1


if __name__ == "__main__":
    sys.exit(main())
