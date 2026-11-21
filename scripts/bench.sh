#!/bin/sh
# Times the pipeline on a dataset of a given size and reports wall time, phases
# and peak memory.
#
#   scripts/bench.sh 20M            native release build, 5 runs
#   scripts/bench.sh 60M 3          3 runs; the 5.5 GB dataset is deleted afterwards unless KEEP=1
#   scripts/bench.sh data/20M       an existing dataset folder
#   BENCH_DOCKER=1 scripts/bench.sh 20M
#                                   the Docker image instead, under the scorer's limits
#                                   (8 CPUs, 24 GB, read-only root, no network, uid 1000)
#
# Datasets come from the organizers' generator (.tools/generate) with a fixed
# seed per size and live in data/SIZE (not committed).
#
# What the numbers mean: the scorer is an 8-vCPU Intel Xeon; this is whatever
# machine you run it on. Use it to compare two builds on one machine, and for
# peak memory and scale. The x86 numbers that decide whether a change is kept
# come from the bench workflow (README, "Measuring").
set -eu
WHAT="${1:?usage: scripts/bench.sh SIZE|DIR [RUNS]}"; RUNS="${2:-5}"
BIN="${BIN:-target/release/pipeline}"
generated=""
if [ -d "$WHAT" ]; then
  DATA="$WHAT"; LABEL="$WHAT"
else
  case "$WHAT" in
    *[kK]) EVENTS=$(( ${WHAT%[kK]} * 1000 )) ;;
    *[mM]) EVENTS=$(( ${WHAT%[mM]} * 1000000 )) ;;
    *) EVENTS="$WHAT" ;;
  esac
  DATA="data/$WHAT"; LABEL="$WHAT"
  if [ ! -f "$DATA/manifest.json" ]; then
    mkdir -p data
    echo "bench: generating $EVENTS events into $DATA ..." >&2
    .tools/generate --seed "$(( EVENTS / 1000000 + 100 ))" --events "$EVENTS" --clicks "$(( EVENTS / 4 ))" --output "$DATA"
    generated=1
  fi
fi
OUT=$(mktemp -d)
cleanup() {
  rm -rf "$OUT"
  if [ -n "$generated" ] && [ "${EVENTS:-0}" -gt 30000000 ] && [ -z "${KEEP:-}" ]; then rm -rf "$DATA"; fi
}
trap cleanup EXIT
now() { perl -MTime::HiRes=time -e 'printf "%.0f\n", time * 1000'; }
bytes=$(cat "$DATA"/events/*.csv "$DATA"/clicks/*.csv "$DATA"/links/*.csv | wc -c | tr -d ' ')   # also warms the page cache
: > "$OUT/walls"
for run in $(seq 1 "$RUNS"); do
  rm -rf "$OUT/o"; mkdir -p "$OUT/o"
  start=$(now)
  if [ -n "${BENCH_DOCKER:-}" ]; then
    docker run --rm --platform "${PLATFORM:-linux/$(uname -m | sed 's/x86_64/amd64/;s/aarch64/arm64/')}" \
      --network none --read-only --tmpfs /tmp:rw,size=64m,mode=1777 --tmpfs /output:rw,size=4g,mode=1777 \
      --cap-drop ALL --security-opt no-new-privileges --pids-limit 4096 \
      --cpus "${CPUS:-8}" --memory "${MEM:-24g}" --memory-swap "${MEM:-24g}" --user 1000:1000 \
      -v "$(cd "$DATA" && pwd)":/data:ro -e DATA_DIR=/data -e OUTPUT_DIR=/output -e NCPU="${CPUS:-8}" -e MEM_GB=24 \
      "${IMAGE:-shopping-sessions:latest}" 2> "$OUT/err" || { echo "bench: run failed"; cat "$OUT/err"; exit 1; }
  else
    NCPU="${NCPU:-8}" "$BIN" "$DATA" "$OUT/o" 2> "$OUT/err" || { echo "bench: run failed"; cat "$OUT/err"; exit 1; }
  fi
  end=$(now)
  wall=$(( end - start ))
  echo "$wall" >> "$OUT/walls"
  echo "run $run: wall $wall ms; $(grep -E 'links .* ms' "$OUT/err" | sed -E 's/^pipeline: //; s/ \(pool.*//')"
done
sort -n "$OUT/walls" > "$OUT/sorted"
best=$(head -1 "$OUT/sorted"); median=$(sed -n "$(( (RUNS + 1) / 2 ))p" "$OUT/sorted")
peak=$(grep -o 'peak memory [0-9]* MB' "$OUT/err" || echo "peak memory: see /usr/bin/time (not Linux)")
mode="native $(uname -m), NCPU=${NCPU:-8}"; [ -z "${BENCH_DOCKER:-}" ] || mode="docker, ${CPUS:-8} CPUs, ${MEM:-24g}"
echo "bench $LABEL ($(( bytes >> 20 )) MB of CSV; $mode): best $best ms, median $median ms of $RUNS; $peak"
