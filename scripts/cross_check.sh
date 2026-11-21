#!/bin/sh
# Generates datasets the organizers published no answer for, and checks that the
# fast pipeline and the reference (two independent implementations) agree on them.
# Usage: scripts/cross_check.sh "SEEDS" EVENTS CLICKS   e.g. scripts/cross_check.sh "2 3 4" 200000 50000
# Needs: .tools/generate, target/release/{pipeline,reference}, uv (for polars).
set -eu
SEEDS="${1:-2 3 4 5}"; EVENTS="${2:-200000}"; CLICKS="${3:-50000}"
WORK="${WORK:-$(mktemp -d)}"; trap 'rm -rf "$WORK"' EXIT
fail=0
for seed in $SEEDS; do
  rm -rf "$WORK/data" "$WORK/ref" "$WORK/out"
  .tools/generate --seed "$seed" --events "$EVENTS" --clicks "$CLICKS" --output "$WORK/data"
  target/release/reference "$WORK/data" "$WORK/ref" 2>/dev/null
  for config in "NCPU=8" "NCPU=1" "NCPU=3 SS_PERSON_BITS=1 SS_DEDUP_BITS=1" "NCPU=5 SS_PERSON_BITS=9 SS_DEDUP_BITS=7"; do
    rm -rf "$WORK/out"
    env $config target/release/pipeline "$WORK/data" "$WORK/out" 2>/dev/null
    if uv run --quiet --python 3.12 --with polars python scripts/compare_exact.py "$WORK/out" "$WORK/ref" > "$WORK/log" 2>&1; then
      result=MATCH; else result=DIFFER; fail=$((fail + 1)); fi
    echo "seed $seed ($(grep -o '"window_start": "[^"]*"' "$WORK/data/manifest.json" | cut -d'"' -f4)) [$config]: $result"
    [ "$result" = MATCH ] || cat "$WORK/log"
  done
done
exit $((fail > 0))
