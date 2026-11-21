#!/bin/sh
# Everything that must hold before a commit goes to the scorer, on this machine:
# unit tests, the 36 fixtures and the dev answer on the native build and on an
# x86-64 (AVX2) build run under Rosetta, then the pipeline against the reference
# on other seeds and on fuzzed data. Usage: scripts/regress.sh [FUZZ_ROUNDS]
set -eu
CARGO="${CARGO:-$HOME/.cargo/bin/cargo}"
PY="uv run --quiet --python 3.12 --with polars python"
ROUNDS="${1:-25}"
X86=target/x86_64-apple-darwin/release/pipeline
$CARGO build --release 2>&1 | grep -E "^(warning|error)" -A8 || true
RUSTFLAGS="-C target-cpu=x86-64-v3" $CARGO build --release --target x86_64-apple-darwin --bin pipeline 2>&1 | grep -E "^(warning|error)" -A8 || true
echo "unit tests (arm):  $($CARGO test --release 2>&1 | grep -E 'test result' | head -1)"
echo "unit tests (x86):  $(RUSTFLAGS='-C target-cpu=x86-64-v3' $CARGO test --release --target x86_64-apple-darwin --bin pipeline 2>&1 | grep -E 'test result|error' | head -1)"
for bin in target/release/pipeline "$X86"; do
  echo "$bin"
  echo "  fixtures:            $($PY scripts/run_fixtures.py fixtures -- "$bin" 2>&1 | tail -1)"
  echo "  fixtures, tiny reads: $(SS_JOB_BYTES=7 SS_BLOCK_BYTES=17 $PY scripts/run_fixtures.py fixtures -- "$bin" 2>&1 | tail -1)"
  rm -rf out; "$bin" dev out 2>/dev/null
  echo "  dev answer:          $($PY scripts/compare.py out dev-answer | tail -1)"
  echo "  fuzz:                $(BIN="$bin" SEED=$(date +%s) scripts/fuzz.sh "$ROUNDS" 20000 2>&1 | tail -1)"
done
echo "diff vs reference, seeds 2 to 5: $(scripts/diff_vs_ref.sh "2 3 4 5" 300k 2>&1 | tail -1)"
echo "diff vs reference, leap day and year end: $(scripts/diff_vs_ref.sh "15 29" 300k 2>&1 | tail -1)"
