#!/bin/sh
# Differential test: generates datasets the organizers published no answer for
# and checks that the pipeline and the reference implementation (ref/, an
# independent, literal reading of rules.md) produce the same tables, byte for byte.
#
#   scripts/diff_vs_ref.sh                 seeds 2 to 5, 200,000 events each
#   scripts/diff_vs_ref.sh SEED SIZE       one seed; SIZE like 300000, 500k or 3M
#   scripts/diff_vs_ref.sh "15 62" 1M      several seeds
#
# Every dataset is run under four thread and partition settings.
# Needs .tools/generate (the organizers' generator), a release build, and uv (for polars).
set -eu
SEEDS="${1:-2 3 4 5}"
SIZE="${2:-200000}"
case "$SIZE" in
  *[kK]) EVENTS=$(( ${SIZE%[kK]} * 1000 )) ;;
  *[mM]) EVENTS=$(( ${SIZE%[mM]} * 1000000 )) ;;
  *) EVENTS="$SIZE" ;;
esac
CLICKS="${3:-$(( EVENTS / 4 ))}"
BIN="${BIN:-target/release/pipeline}"
WORK="${WORK:-$(mktemp -d)}"; trap 'rm -rf "$WORK"' EXIT
fail=0; runs=0
for seed in $SEEDS; do
  rm -rf "$WORK/data" "$WORK/ref" "$WORK/out"
  .tools/generate --seed "$seed" --events "$EVENTS" --clicks "$CLICKS" --output "$WORK/data"
  target/release/reference "$WORK/data" "$WORK/ref" 2>/dev/null
  for config in "NCPU=8" "NCPU=1" "NCPU=3 SS_PERSON_BITS=1 SS_DEDUP_BITS=1" "NCPU=5 SS_PERSON_BITS=9 SS_DEDUP_BITS=7"; do
    rm -rf "$WORK/out"; runs=$((runs + 1))
    env $config "$BIN" "$WORK/data" "$WORK/out" 2>/dev/null
    if uv run --quiet --python 3.12 --with polars python scripts/compare_exact.py "$WORK/out" "$WORK/ref" > "$WORK/log" 2>&1; then
      result=MATCH; else result=DIFFER; fail=$((fail + 1)); fi
    echo "seed $seed ($(grep -o '"window_start": "[^"]*"' "$WORK/data/manifest.json" | cut -d'"' -f4), $EVENTS events) [$config]: $result"
    [ "$result" = MATCH ] || cat "$WORK/log"
  done
done
echo "diff_vs_ref: $((runs - fail))/$runs runs match the reference"
exit $((fail > 0))
