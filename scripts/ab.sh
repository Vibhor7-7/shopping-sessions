#!/bin/sh
# A/B two native builds on one dataset, runs interleaved so drift in the machine
# hits both alike: scripts/ab.sh DATA BINARY_A BINARY_B [RUNS]
# Prints each phase's minimum over the runs, per binary.
set -eu
DATA="$1"; A="$2"; B="$3"; RUNS="${4:-7}"
OUT=$(mktemp -d); trap 'rm -rf "$OUT"' EXIT
: > "$OUT/a"; : > "$OUT/b"
for _ in $(seq 1 "$RUNS"); do
  for side in a b; do
    [ "$side" = a ] && bin="$A" || bin="$B"
    rm -rf "$OUT/o"
    "$bin" "$DATA" "$OUT/o" 2>&1 | grep -E 'links .* ms' | sed -E 's/.*links ([0-9.]+) ms, parse ([0-9.]+) ms, dedup ([0-9.]+) ms, people ([0-9.]+) ms, total ([0-9.]+) ms.*/\1 \2 \3 \4 \5/' >> "$OUT/$side"
  done
done
for side in a b; do
  [ "$side" = a ] && bin="$A" || bin="$B"
  awk -v name="$bin" 'NR==1{for(i=1;i<=5;i++)m[i]=$i} {for(i=1;i<=5;i++)if($i<m[i])m[i]=$i} END{printf "%-34s links %6.1f  parse %7.1f  dedup %6.1f  people %6.1f  total %7.1f  (min of %d, NCPU=%s)\n", name, m[1], m[2], m[3], m[4], m[5], NR, ENVIRON["NCPU"]}' "$OUT/$side"
done
