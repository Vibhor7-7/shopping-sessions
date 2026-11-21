# Goal
Fastest correct implementation of rules.md. Score = container wall time
(start to exit) on ~20M events in practice, ~60M in the final (3 min hard cap).
Output that is almost right scores ZERO. Correctness beats speed, always.

# Hard constraints (runtime)
- linux/amd64, 8 vCPU, 24 GB RAM, NO swap, no network
- /data read-only, /scratch writable (spills), /tmp only 64 MB in-memory
- Root FS read-only, runs as uid 1000
- Engine startup and CSV reading count toward the time

# Verification loop (run before claiming ANY result)
1. Fixtures: make native-fixtures
   (the Docker image under the scorer's flags: make fixtures)
2. Dev check: make check-dev
3. Differential: ./scripts/diff_vs_ref.sh <seed> <size>
   (compares output to the reference impl in ref/ across seeds 2..5)
4. Benchmark: ./scripts/bench.sh 20M (and 60M before any submission)

`scripts/regress.sh` runs 1 to 3 in one go, on the native build and on an
x86-64 (AVX2) build, with unit tests and fuzzing on top.

# Rules for you
- Never say something is faster without a bench.sh number.
- Never say something is correct without fixtures + check-dev + diff passing.
- Log every experiment in BENCH.md: change, time, pass/fail, kept/reverted.
- One optimization at a time. Revert anything that breaks correctness.
- rules.md is the spec. If it is ambiguous, list the ambiguity and ask me.

# Optimization loop (repeat this a lot)
Do the top item from the list in BENCH.md. Run the full verification loop.
Log it in BENCH.md. If it is slower or wrong, revert it.

# Before each real submission
Clean build of the Docker image. Run fixtures, check-dev, the diff on seeds
2 to 5, and bench at 60M under the container limits. Report the 60M time, peak
memory, and image size. Flag anything that could fail on hidden data.

# Where the numbers come from
- `scripts/bench.sh` runs on the machine at hand. On the development Mac that
  is an ARM laptop: its numbers rank two builds and give peak memory, but they
  are not the scorer's. The 60M run there cannot be given 24 GB.
- The x86-64 numbers that decide whether a change is kept come from the bench
  workflow: `gh workflow run bench.yml -f base=<commit>`, then
  `scripts/bench_result.sh`. It builds both commits with the Dockerfile and
  runs them interleaved on a GitHub x86 machine (about 1% noise). Runner CPUs
  differ from run to run, so compare only within one run.
- Only a score from the track page is the truth. Record each one in BENCH.md.
