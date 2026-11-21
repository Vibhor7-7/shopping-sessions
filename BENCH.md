# BENCH.md: every experiment, kept or not

## How to read this

- **Scorer** is the only number that counts: the track page's time for a commit, on the hidden ~20M-event practice set.
- **x86 A/B** is what decides whether a change is kept: the `bench` workflow builds two commits with the Dockerfile and runs them interleaved on one GitHub x86-64 machine (2 cores / 4 threads, stand-in 20M-event dataset from `src/bin/synth.rs`). Times are the in-process total in ms, minimum of 5 runs, base → head. Noise between repeats is about 1%. The CPU differs from run to run, so only compare within a row.
- **bench.sh 20M** is `scripts/bench.sh 20M` on the development Mac (ARM, 8 threads, the organizers' generator, seed 120): best / median wall ms of 5. The Mac is under memory pressure and its numbers wander by 10% or more; they are here because the rules ask for one, not because they decide anything.
- **Verified** means: 36 fixtures, `make check-dev`, `scripts/diff_vs_ref.sh` on seeds 2 to 5, on the native build and on an x86-64 build (`scripts/regress.sh`), then CI on real amd64.

## Scorer results

| When | Commit | Score | Note |
| --- | --- | --- | --- |
| 2026-11-20 | `d698c87` | 1.05 s | First submission. Debian runtime image, 115 MB. |
| 2026-11-20 | `7f9bffe` | 0.96 s | 18% less in-process time on x86 A/B, but only 9% less on the scorer. Leaderboard top: 0.40 s. |

The gap between "18% less in-process" and "9% less scored" is what experiment 16 is about.

## Experiments

Rows 1 to 15 were run before this log existed and are reconstructed from the benchmark runs; they have x86 A/B numbers but no `bench.sh` number.

| # | Change | Commit | x86 A/B total (ms) | bench.sh 20M | Verified | Verdict |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | Baseline: chunked reader, AVX2 delimiter scan, packed keys, union-find links, dedup stream, per-person sort | `d698c87` | 945 (Zen 4) | n/a | yes | kept |
| 2 | IDs packed 16 bytes at a time (SSE nibble tables + PEXT) | `20ceae3` | 1487 → 1359 (Zen 3), −8.6% | n/a | yes | kept |
| 3 | Sweep: read block 64K/128K/512K, person bits 7 to 10, dedup bits 7 to 10 | none | all within ±1% of defaults (Zen 3) | n/a | n/a | nothing changed |
| 4 | Link join prefetches its table lines; catalog loads without an allocation per row | `3d255f5` | 1378 → 1356 (Zen 3), −1.7% | n/a | yes | kept |
| 5 | Timestamps and link lookup written branch-free | `9304f81` | 1062 → 1070 (Granite Rapids), 0% | n/a | yes | no gain: the compiler rebuilt the branches (callgrind: instructions unchanged). Kept as the base for 6 |
| 6 | The same, forced with `hint::select_unpredictable`; field bounds in registers | `8cb3e8e` | 1147 → 1123 (Zen 4), −2%; parse −3.2% | n/a | yes | kept |
| 7 | Link table twice as large (fewer lookups spilling to a second cache line) | branch, deleted | 1031 → 1045 (Granite Rapids), +1.3% | n/a | yes | **reverted**: building it cost more than the lookups saved |
| 8 | Sessions without sorting: half-hour bins, two counting passes | `f761de9` | 1134 → 1103 (Zen 4), −2.7%; people −10% | n/a | yes | kept |
| 9 | Partition counts and chunk sizes follow the input size | `1c2d0ec` | 1087 → 1082 (Zen 4), noise | n/a | yes | kept, for the 60M final (2048 partitions instead of 512) |
| 10 | Input read in place through `mmap` instead of `read` | `d037d62`, reverted in `6bbe143` | 998 → 1073 (Granite Rapids), +7.5%; parse +11% | n/a | yes | **reverted**: page-table walks on 4 KB file pages cost more than the copy, on a VM |
| 11 | Timestamp fall-back kept a single branch with `black_box` | `6bbe143` | 1033 → 1032 (Granite Rapids), 0% | n/a | yes | kept: no speed change, but the code no longer depends on the compiler's mood (callgrind: 733k → 318k mispredictions) |
| 12 | Blocks of 2 MB and more on huge pages (own allocator); date table compared in two words | `0c6d2a0` | 885 → 852 (Zen 4), −3.7%; people −11.6% | n/a | yes | kept |
| 13 | Blocks of 32 KB to 2 MB on huge pages too | `ee6f925` | 975 → 968 (Granite Rapids), −0.7% | n/a | yes | kept |
| 14 | Link table as 4 IDs + 4 people per cache line; dedup with one two-bit array | `b58dc53` | 821 → 809 (Zen 4), −1.5% | n/a | yes | kept |
| 15 | Direct comparison of #1 with #11 | `d698c87` vs `6bbe143` | 945 → 825 (Zen 4), −12.7% | n/a | n/a | measurement only |
| — | Baseline for what follows | `7f9bffe` | — | 384 / 413, peak 1259 MB | yes | — |
| 16 | Runtime image BusyBox (2.3 MB) instead of Debian (115 MB); binary unchanged | `c5b2dfd` | 1261 → 1257 (Zen 3): in-process identical, as it must be. `docker run` wall on the runner is the same for both images (Docker mounts layers, it does not unpack them) | not rerun: same binary | yes | **awaiting a score.** Only the scorer can show whether its sandboxes pay for image size |

## Before each submission

| Commit | Clean image build | Fixtures (image, scorer flags) | check-dev | diff seeds 2 to 5 | 60M: time | 60M: peak memory | Image |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `c5b2dfd` | yes, `--no-cache`, amd64 | 36/36 (locally under emulation and on CI's real amd64) | MATCH (native build, and the image's output) | 16/16 runs match | x86 CI, Docker `--cpus 4 --memory 12g` (the runner has no more): **4.06 to 4.34 s** per `docker run`, 3.07 s inside the process. Mac, native: 4.4 s best of 3, on a machine short of memory and disk | **3.4 GB** (x86, 4 threads); 3.1 to 3.4 GB on the Mac | 2.3 MB root filesystem, 3 layers |

Notes on the 60M runs:
- No machine at hand has 8 CPUs and 24 GB for a container: the Mac's Docker VM has 8 GB, the CI runner 4 CPUs and 16 GB. The numbers above are under the largest limits each allows.
- The reference cannot run at 60M (it would need tens of GB), so correctness at that size rests on: two runs with different thread and partition counts (8 threads, 2^11/2^11 partitions; 3 threads, 2^8/2^12) giving identical tables (6,623,558 user rows; digests equal); `sessions` and `revenue_cents` summing to the same in both tables (10,012,569 and 19,153,547,521); and the diff against the reference at up to 3M events.
- On the CI runner, `docker run` at 60M takes about 1.0 s longer than the process inside it; at 20M the difference is 0.2 s (0.13 s of that is an empty container). What the extra 0.8 s is has not been found yet; see the list below.

## To try, most promising first

1. **Where container time goes outside the process.** At 60M, `docker run` takes 1.0 s more than the process reports; at 20M, 0.2 s. Time the gap with output on tmpfs, without a memory limit, and with timestamps from inside, to see whether it is our output, our memory, or the runtime.
2. **Smaller output.** `user_daily` is 78 MB of PLAIN Parquet for 20M events. Dictionary and run-length encodings would cut it to about a third: less to write, and less left to flush if the clock waits for that.
3. **AVX-512 delimiter scan.** The organizers say AVX-512 is there. Halves the instructions of stage one (about 12% of parse), but 512-bit code lowers the clock on Cascade Lake; needs a scorer run to judge, not CI (the CI machines are not Cascade Lake).
4. **Timestamps, keys, quote check**: instruction-level cuts, 1 to 3% each (callgrind lists them).
