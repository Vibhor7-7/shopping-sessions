#!/usr/bin/env python3
"""
Whether two runs of the pipeline wrote the same tables, rows in any order and
in any number of files. Usage: same_output.py OUT_A OUT_B

For two runs of one build under different settings (thread counts, partition
counts, the scan loop) on a dataset too large for the reference.
"""
import sys

import polars as pl


def main():
    a_dir, b_dir = sys.argv[1], sys.argv[2]
    same = True
    for table in ("user_daily", "campaign_daily"):
        a = pl.read_parquet(f"{a_dir}/{table}/*.parquet")
        b = pl.read_parquet(f"{b_dir}/{table}/*.parquet")
        columns = sorted(a.columns)
        equal = sorted(b.columns) == columns and a.height == b.height and a.select(columns).sort(columns).equals(b.select(columns).sort(columns))
        print(f"{table}: {a.height} and {b.height} rows, {'IDENTICAL' if equal else 'DIFFERENT'}")
        same &= equal
    sys.exit(0 if same else 1)


if __name__ == "__main__":
    main()
