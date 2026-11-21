# Shopping Sessions

A hand-written pipeline for the Shopping Sessions track: dirty event, click and link CSVs in, two exact daily Parquet tables out. `rules.md` is the specification.

It is one static Rust binary with no dependencies. The CSV dialect in `rules.md` does not match any engine's reader (a `\r` is data, every comma splits, a quote is only legal around a whole field), so the parser is written to the rules instead of bending a general one to them.

## Run it

```sh
make build        # docker buildx build --platform linux/amd64 -t shopping-sessions:latest .
make fixtures     # the image on all 36 edge-case fixtures, with the scorer's flags
make run-dev      # the image on dev/, timed
make check-dev    # exact diff against dev-answer/
```

On an Apple-silicon Mac add `PLATFORM=linux/arm64` to run natively instead of under emulation.

Without Docker, with a Rust toolchain:

```sh
make native-fixtures   # cargo build --release, then the 36 fixtures
make native-dev        # the dev set, diffed against dev-answer/
make test              # unit tests
```

The test scripts need `uv` (they run `uv run --with polars`); set `PY=python3` if you have Python 3.10+ with polars installed.

## How it works

| Phase | What happens | File |
| --- | --- | --- |
| links | `links/` is parsed in parallel into edges and joined with a lock-free union-find. A root always adopts the byte-wise smaller ID, so each tree's root is the person's name. | `src/links.rs` |
| parse | Files are cut into ranges; each thread reads a range in 256 KB blocks, finds every newline, comma and quote 64 bytes at a time (AVX2), then validates each line. A surviving event is filed under its person; every parsed copy also leaves a small dedup record keyed by `event_id`. | `src/reader.rs`, `src/scan.rs`, `src/parse.rs` |
| dedup | Per partition, a bitmap finds the few `event_id`s seen twice; only those copies are sorted, and every copy but the earliest has its event dropped. | `src/dedup.rs` |
| people | Per partition, events and clicks are grouped by person, sorted by time, and walked once: sessions, the crediting click, the assisting campaigns. Each thread writes its own `user_daily` file. | `src/person.rs` |
| output | `campaign_daily`, and a minimal Parquet writer (PLAIN, uncompressed). | `src/main.rs`, `src/parquet.rs` |

Things that make it fast:

- **IDs are 64-bit keys.** An ID of up to 10 characters from `[0-9A-Z_a-z]` packs into a `u64` whose integer order is the ID's byte order (`src/key.rs`). Every ID the generator writes fits. Any other ID (a space, a tab, a `\r`, too long) is interned on a slow, exact path, so the rules hold for any input.
- **Dedup without a second pass over events.** A copy that would fail validation gets no event record, so if it is the earliest copy, dropping the other copies' records leaves nothing: "dedup comes before validation" for free.
- **Clicks need no dedup.** Copies of a click are identical, so they credit the same campaign and assist as the same campaign once.
- **Lookups are prefetched.** The link table is larger than the cache; each line asks for its slot 16 lines before reading it.

Exactness limits, none of which the generator can reach: the window must be shorter than 42 days (timestamps are stored as 32-bit offsets), and `ingest_ts` is compared in whole seconds (it is always form F1).

## Testing

- `fixtures/`: the organizers' 36 hand-written cases.
- `dev-answer/`: the published answer for the 1M-event dev set (not committed; unzip `dev-answer.zip` here).
- `src/bin/reference.rs`: a second, deliberately slow implementation that reads `rules.md` literally and shares no code with the pipeline. `make reference` checks it against the fixtures and the dev answer.
- `scripts/cross_check.sh`: the pipeline against the reference on other seeds (leap-day and year-boundary windows included), with different thread and partition counts.
- `scripts/fuzz.sh`: the same comparison on data dirtied far past what the generator does, read in tiny blocks and tiny file ranges so every boundary is hit.

`dev/` and the generator are not in this repository; `make dev` builds the dev set if `generate` is on your `PATH` or in `.tools/`.

## Tuning knobs

Environment variables, for experiments; the defaults are what the scorer runs.

| Variable | Meaning |
| --- | --- |
| `NCPU` | Threads (the scorer sets 8) |
| `SS_PERSON_BITS`, `SS_DEDUP_BITS` | log2 of the partition counts |
| `SS_JOB_BYTES`, `SS_BLOCK_BYTES` | File range and read block sizes; tiny values are for tests |
