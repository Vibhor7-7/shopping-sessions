#!/bin/sh
# Fuzzes the fast pipeline against the reference: scripts/fuzz.sh ROUNDS [EVENTS]
# KEEP=DIR saves the datasets that disagree. Each round generates a dataset, dirties it with scripts/fuzz.py, and runs the
# pipeline with random thread counts, partition counts and tiny read sizes.
set -eu
ROUNDS="${1:-10}"; EVENTS="${2:-20000}"; BIN="${BIN:-target/release/pipeline}"
WORK=$(mktemp -d); trap 'rm -rf "$WORK"' EXIT
fail=0
for round in $(seq 1 "$ROUNDS"); do
  seed=$((round * 7919 + ${SEED:-0}))
  .tools/generate --seed "$seed" --events "$EVENTS" --clicks $((EVENTS / 3)) --output "$WORK/base" >/dev/null
  python3 scripts/fuzz.py "$WORK/base" "$WORK/data" "$seed"
  rm -rf "$WORK/ref"; target/release/reference "$WORK/data" "$WORK/ref" 2>/dev/null
  for config in "NCPU=8" "NCPU=$((seed % 5 + 1)) SS_JOB_BYTES=$((seed % 977 + 1)) SS_BLOCK_BYTES=$((seed % 211 + 16)) SS_PERSON_BITS=$((seed % 7 + 1)) SS_DEDUP_BITS=$((seed % 5 + 1))"; do
    rm -rf "$WORK/out"
    if ! env $config "$BIN" "$WORK/data" "$WORK/out" 2>"$WORK/err"; then echo "round $round [$config]: CRASH"; tail -3 "$WORK/err"; fail=$((fail + 1)); continue; fi
    if uv run --quiet --python 3.12 --with polars python scripts/compare_exact.py "$WORK/out" "$WORK/ref" > "$WORK/log" 2>&1; then :; else
      echo "round $round seed $seed [$config]: DIFFER"; cat "$WORK/log"; fail=$((fail + 1)); [ -z "${KEEP:-}" ] || cp -R "$WORK/data" "$KEEP/fuzz-fail-$seed"; fi
  done
done
echo "fuzz: $ROUNDS rounds, $fail failures"
exit $((fail > 0))
