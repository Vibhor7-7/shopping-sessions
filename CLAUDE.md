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

# How the work is done here
- Submissions are the user's: hand over a full commit hash whose CI is green,
  with the report "Before each real submission" asks for. They submit it on the
  track page (2 an hour) and report the score. Never claim a scored time.
- An experiment is a branch: push it, then
  `gh workflow run bench.yml --ref <branch> -f base=<commit it grew from>`,
  read it with `scripts/bench_result.sh <run id>`. Kept: fast-forward main and
  delete the branch. Not kept: delete the branch. Either way a row in BENCH.md.
- The same workflow takes `-f events=60000000` (the final's size),
  `-f runs=N`, and `-f sweep="A=1;B=2 C=3" -f sweep_threads="4 1"` to try
  settings on one commit. `profile.yml` gives instructions and mispredictions
  per source line; `overhead.yml` splits a container's time around the process.
- The final is scored at 60M: check a change there too before keeping it, when
  it touches memory layout, buffers or partition counts.
- Do not generate the 60M dataset on the development Mac: the disk has room
  for neither it nor an honest timing. 60M numbers come from the workflow.
- Branch `probe` is not for scoring: it fails the dev check on purpose so the
  track page shows its log (see BENCH.md). Merge main into it before use.
- In zsh, `env $settings command` does not split `$settings`; use
  `sh -c "$settings command"`.

# Unattended cycles (overnight.sh)
- `overnight.sh` starts one Claude Code session per cycle with `overnight.md` as
  its whole prompt; the user set this up on 2026-11-21. In those sessions, and
  only there, the session itself submits on the track page, once a cycle, under
  overnight.md's conditions. Everywhere else submissions stay the user's.
- overnight.md is written for any repository. Its checks, here:
  - "bench.sh at 60M" is the bench workflow with `-f events=60000000` (this Mac
    cannot hold the dataset), and "under 60s" is that run's `docker run` time.
  - "at least 5% faster than the last submitted version" is the bench workflow's
    4-thread total with `-f base=<the last commit in SUBMISSIONS.md>`, in both of
    its repeats. `bench.sh 20M` on the Mac wanders by more than 5%: log it, do
    not decide by it.
  - A change that passes and is faster stays in main even while main is less
    than 5% ahead of the last submitted commit. The submission waits for a
    later cycle; the gains add up.
- SUBMISSIONS.md is the record of every submission and how the track page
  works; MORNING.md is the note the user reads first.
- The probe is never submitted by an unattended cycle.
