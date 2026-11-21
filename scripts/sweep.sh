#!/bin/sh
# One build under several settings, runs interleaved, minimum of each phase:
#   scripts/sweep.sh DATA BINARY RUNS "SS_BLOCK_BYTES=65536;SS_BLOCK_BYTES=262144;..."
# An empty setting ("a;;b" or a leading ";") is the defaults.
set -eu
DATA="$1"; BIN="$2"; RUNS="$3"; SETTINGS="$4"
OUT=$(mktemp -d); trap 'rm -rf "$OUT"' EXIT
now() { perl -MTime::HiRes=time -e 'printf "%.1f\n", time * 1000'; }
count=$(echo "$SETTINGS" | awk -F';' '{print NF}')
for _ in $(seq 1 "$RUNS"); do
  for i in $(seq 1 "$count"); do
    settings=$(echo "$SETTINGS" | cut -d';' -f"$i")
    rm -rf "$OUT/o"
    start=$(now)
    env $settings "$BIN" "$DATA" "$OUT/o" 2> "$OUT/err" || { echo "FAILED: $settings"; cat "$OUT/err"; exit 1; }
    end=$(now)
    phases=$(grep -E 'links .* ms' "$OUT/err" | sed -E 's/.*links ([0-9.]+) ms, parse ([0-9.]+) ms, dedup ([0-9.]+) ms, people ([0-9.]+) ms, total ([0-9.]+) ms.*/\1 \2 \3 \4 \5/')
    echo "$phases $(echo "$end $start" | awk '{print $1 - $2}')" >> "$OUT/$i"
  done
done
for i in $(seq 1 "$count"); do
  settings=$(echo "$SETTINGS" | cut -d';' -f"$i")
  awk -v name="${settings:-(defaults)}" 'NR==1{for(i=1;i<=6;i++)m[i]=$i} {for(i=1;i<=6;i++)if($i<m[i])m[i]=$i} END{printf "%-44s links %6.1f  parse %7.1f  dedup %6.1f  people %6.1f  total %7.1f  wall %7.1f  (min of %d)\n", name, m[1], m[2], m[3], m[4], m[5], m[6], NR}' "$OUT/$i"
done
