#!/bin/sh
# Runs the reference (the slow oracle) on every fixture and on the dev set, and
# diffs its CSV tables with the expected ones, order-insensitively.
# Usage: scripts/check_reference.sh [REFERENCE_BINARY]
set -eu
REF="${1:-target/release/reference}"
TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
fail=0; n=0
same() { sort "$1" > "$TMP/a"; sort "$2" > "$TMP/b"; cmp -s "$TMP/a" "$TMP/b"; }
for case in fixtures/*/; do
  name=$(basename "$case"); n=$((n + 1))
  "$REF" "$case/input" "$TMP/$name" 2>/dev/null || { echo "FAIL $name: crashed"; fail=$((fail + 1)); continue; }
  for table in user_daily campaign_daily; do
    if ! same "$TMP/$name/$table.csv" "$case/expected/$table.csv"; then
      echo "FAIL $name $table"; diff "$TMP/a" "$TMP/b" | head -10 || true; fail=$((fail + 1))
    fi
  done
done
echo "fixtures: $n cases, $fail failures"
if [ -d dev ] && [ -d dev-answer ]; then
  "$REF" dev "$TMP/dev"
  for table in user_daily campaign_daily; do
    if same "$TMP/dev/$table.csv" "dev-answer/$table.csv"; then echo "dev $table: MATCH"; else
      echo "dev $table: DIFFER"; diff "$TMP/a" "$TMP/b" | head -10 || true; fail=$((fail + 1)); fi
  done
fi
exit $((fail > 0))
