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
| 17 | Measurement: where a container's time goes outside the process (`overhead` workflow, 60M events, GitHub's Docker) | `c24627f` | per run: ~80 ms before `main`, ~2880 ms in the process, ~45 ms after it with output on tmpfs, ~80 ms with output on disk. The first run after an image is built: 6 s before `main`, once | n/a | n/a | measurement only. Under Docker, 0.13 to 0.16 s per run is outside the process, whatever the image and limits; writing 222 MB to disk adds ~35 ms after the process and nothing inside it. Says nothing about the scorer's `runc` sandboxes |
| — | Thread count on 2 cores / 4 hyperthreads (Ice Lake Xeon 8370C, the scorer's class of CPU) | `1cac78b` | 2 threads 1812, 3 threads 1643, **4 threads 1492**, 6 threads 1518, 8 threads 1523 | n/a | n/a | measurement only: the second hyperthread of a core is worth 21%; more threads than hyperthreads costs 2% |
| 18 | Event and copy records leave the parser through write-combining buffers and non-temporal stores (no cache line of fresh output is fetched just to be overwritten) | `9fcffee` | **1514 → 1334 (Ice Lake 8370C), −12%**; parse 1110 → 931 (−16%); one thread: parse 2688 → 2152 (−20%) | 374 / 401, peak 1269 MB (ARM has no such stores; no change expected there) | yes | **kept** |
| — | Sweep on top of 18: read block 16K/32K/64K/128K (default 256K); partition bits 9/9 and 11/10 (default 10/10) | `476a8a8` | defaults 791 (Zen 4); 16K 813, 32K 804, 64K 791, 128K 798; bits 9/9 797, 11/10 800 | n/a | n/a | nothing changed: the defaults are the best or tied |
| 19 | Click records through write-combining buffers too (8 records, three cache lines) | `daa71f5` | 1249 → 1240 (Zen 3), −0.7%; one thread: parse −2.5% | with 20 | yes | kept |
| 20 | Eight records to a buffer for the 16-byte streams: half the flush branches, twice the buffer | `35d159d` | 1239 → 1227 (Zen 3), −0.9%; one thread: parse −2.2% | 374 / 455, peak 1267 MB | yes | kept |

## The probe (branch `probe`, commit `20969e6`)

Not for scoring. It runs the pipeline, logs a description of the machine (CPU model and AVX-512 subsets, whether it is a virtual machine, cgroup CPU and memory limits, huge-page settings, every mount, when the sandbox's files were created, how many threads really run at once, memory and disk speed, `read()` against `mmap()`, 256-bit against 512-bit scans) and then writes an empty `user_daily` so that the dev check fails: a failed dev check is the only case in which the track page shows a submission its log. A rejected submission does not change the best score; it does use one of the two submissions an hour.

What its answers decide:
- virtual machine or bare metal, and the `read()`/`mmap()` numbers: whether experiment 10 should be retried;
- AVX-512 subsets and the alternating 256/512-bit timing: whether a 512-bit stage one pays;
- the creation times of `/app/pipeline` and `/`: whether a sandbox is unpacked per run (experiment 16);
- thread scaling: whether 8 threads have 8 hyperthreads to themselves;
- `/output` speed and huge-page mode: whether smaller output or other memory layouts matter.

## Before each submission

| Commit | Clean image build | Fixtures (image, scorer flags) | check-dev | diff seeds 2 to 5 | 60M: time | 60M: peak memory | Image |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `c5b2dfd` | yes, `--no-cache`, amd64 | 36/36 (locally under emulation and on CI's real amd64) | MATCH (native build, and the image's output) | 16/16 runs match | x86 CI, Docker `--cpus 4 --memory 12g` (the runner has no more): **4.06 to 4.34 s** per `docker run`, 3.07 s inside the process. Mac, native: 4.4 s best of 3, on a machine short of memory and disk | **3.4 GB** (x86, 4 threads); 3.1 to 3.4 GB on the Mac | 2.3 MB root filesystem, 3 layers |

Notes on the 60M runs:
- No machine at hand has 8 CPUs and 24 GB for a container: the Mac's Docker VM has 8 GB, the CI runner 4 CPUs and 16 GB. The numbers above are under the largest limits each allows.
- The reference cannot run at 60M (it would need tens of GB), so correctness at that size rests on: two runs with different thread and partition counts (8 threads, 2^11/2^11 partitions; 3 threads, 2^8/2^12) giving identical tables (6,623,558 user rows; digests equal); `sessions` and `revenue_cents` summing to the same in both tables (10,012,569 and 19,153,547,521); and the diff against the reference at up to 3M events.
- On the CI runner, `docker run` at 60M takes about 1.0 s longer than the process inside it; at 20M the difference is 0.2 s (0.13 s of that is an empty container). What the extra 0.8 s is has not been found yet; see the list below.

## To try, most promising first

1. **Read the scorer**: the score of `c5b2dfd` (experiment 16) and, if spent, the probe's log. Most of what follows depends on them.
2. **`mmap` again, if the scorer is bare metal.** It lost on virtual machines because of page-table walks (experiment 10); on bare metal those are cheap and the copy it saves is 6% of the run.
3. **AVX-512 stage one**, if the probe shows it does not slow the scalar code around it.
4. **Smaller output.** `user_daily` is 78 MB of PLAIN Parquet at 20M events. Dictionary and run-length encodings would cut it to about a third. Worth 1% under Docker (experiment 17); more only if the scorer's `/output` is slow.
5. **Instruction-level cuts** in timestamps, keys and the quote check: 1 to 3% each (callgrind lists them).
