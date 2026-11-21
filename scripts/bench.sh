#!/bin/sh
# Best-of-N phase times of a native build: scripts/bench.sh DATA [RUNS] [BINARY]
# Reports the run with the smallest total, since the fastest run is the one the
# machine disturbed least. Local numbers rank changes; only the scorer's count.
set -eu
DATA="$1"; RUNS="${2:-7}"; BIN="${3:-target/release/pipeline}"
OUT=$(mktemp -d); trap 'rm -rf "$OUT"' EXIT
best=""; best_total=999999999
for _ in $(seq 1 "$RUNS"); do
  rm -rf "$OUT/o"
  line=$("$BIN" "$DATA" "$OUT/o" 2>&1 | grep -E 'links .* ms' | sed 's/ (pool.*//')
  total=$(echo "$line" | sed -E 's/.*total ([0-9]+)\.([0-9]).*/\1\2/')
  if [ "$total" -lt "$best_total" ]; then best_total=$total; best="$line"; fi
done
echo "NCPU=${NCPU:-all} $best"
