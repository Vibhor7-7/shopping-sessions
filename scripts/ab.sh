#!/bin/sh
# A/B two builds (or one build under two settings) on one dataset, runs
# interleaved so drift in the machine hits both alike:
#   scripts/ab.sh DATA BINARY_A BINARY_B [RUNS]
#   ENV_A="SS_BLOCK_BYTES=131072" ENV_B="" scripts/ab.sh DATA BIN BIN
# Prints each phase's minimum over the runs, and the minimum wall time of the
# whole process, which also holds what the phases do not: start-up and the
# kernel tearing the process down.
set -eu
DATA="$1"; A="$2"; B="$3"; RUNS="${4:-7}"
OUT=$(mktemp -d); trap 'rm -rf "$OUT"' EXIT
now() { perl -MTime::HiRes=time -e 'printf "%.1f\n", time * 1000'; }
: > "$OUT/a"; : > "$OUT/b"
for _ in $(seq 1 "$RUNS"); do
  for side in a b; do
    if [ "$side" = a ]; then bin="$A"; settings="${ENV_A:-}"; else bin="$B"; settings="${ENV_B:-}"; fi
    rm -rf "$OUT/o"
    start=$(now)
    env $settings "$bin" "$DATA" "$OUT/o" 2> "$OUT/err" || { echo "FAILED: $bin"; cat "$OUT/err"; exit 1; }
    end=$(now)
    phases=$(grep -E 'links .* ms' "$OUT/err" | sed -E 's/.*links ([0-9.]+) ms, parse ([0-9.]+) ms, dedup ([0-9.]+) ms, people ([0-9.]+) ms, total ([0-9.]+) ms.*/\1 \2 \3 \4 \5/')
    echo "$phases $(echo "$end $start" | awk '{print $1 - $2}')" >> "$OUT/$side"
  done
done
for side in a b; do
  if [ "$side" = a ]; then name="$A ${ENV_A:-}"; else name="$B ${ENV_B:-}"; fi
  awk -v name="$name" 'NR==1{for(i=1;i<=6;i++)m[i]=$i} {for(i=1;i<=6;i++)if($i<m[i])m[i]=$i} END{printf "%-44s links %6.1f  parse %7.1f  dedup %6.1f  people %6.1f  total %7.1f  wall %7.1f  (min of %d)\n", name, m[1], m[2], m[3], m[4], m[5], m[6], NR}' "$OUT/$side"
done
