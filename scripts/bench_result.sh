#!/bin/sh
# Waits for a bench workflow run and prints its timing lines: scripts/bench_result.sh [RUN_ID]
set -eu
id="${1:-$(gh run list --workflow bench.yml --limit 1 --json databaseId --jq '.[0].databaseId')}"
gh run watch "$id" --exit-status --interval 15 > /dev/null 2>&1 || true
echo "run $id: $(gh run view "$id" --json conclusion,headSha --jq '.conclusion + " " + .headSha[0:7]')"
gh run view "$id" --log 2> /dev/null \
  | awk -F'\t' '{ line = $NF; sub(/^[^ ]*Z /, "", line); print line }' \
  | grep -E "^Model name|^(bin/|SS_|\(defaults\)|NCPU|--- )|^container wall|^bare docker|^docker run of|^(base|head): |FAILED|rror:" || true
