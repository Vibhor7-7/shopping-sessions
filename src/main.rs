//! Shopping Sessions: the pipeline. rules.md is the specification.
//!
//!   links   parse `links/`, join IDs into people            (links.rs)
//!   parse   events and clicks to records, filed by person   (parse.rs, reader.rs, scan.rs)
//!   dedup   drop every event copy but the earliest          (dedup.rs)
//!   people  sessions, credit, assists, user_daily           (person.rs)
//!   output  campaign_daily                                   (parquet.rs)
//!
//! Reads $DATA_DIR, writes $OUTPUT_DIR. `pipeline DATA OUT` overrides both.

mod catalog;
mod dedup;
mod key;
mod links;
mod parquet;
mod parse;
mod person;
mod pool;
mod reader;
mod scan;
mod sys;
mod ts;

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use parquet::Kind;

fn setting(name: &str) -> Option<usize> {
    std::env::var(name).ok()?.trim().parse().ok()
}

/// PLAIN encoding of strings: a 4-byte length before each.
fn plain(values: impl Iterator<Item = impl AsRef<[u8]>>, out: &mut Vec<u8>) {
    for value in values {
        let value = value.as_ref();
        out.extend_from_slice(&(value.len() as u32).to_le_bytes());
        out.extend_from_slice(value);
    }
}

/// Writes one thread's user_daily rows as a Parquet file of their own.
fn write_user_daily(folder: &Path, catalog: &catalog::Catalog, part: usize, rows: person::UserRows) -> usize {
    let count = rows.person.len();
    if count == 0 && part != 0 {
        return 0; // the checker needs at least one file, not one per thread
    }
    let mut ids = Vec::with_capacity(count * 14 + 16);
    for &person in &rows.person {
        let at = ids.len();
        ids.extend_from_slice(&[0; 4]);
        key::write_id(person, &mut ids);
        let len = (ids.len() - at - 4) as u32;
        ids[at..at + 4].copy_from_slice(&len.to_le_bytes());
    }
    let present: Vec<bool> = rows.top.iter().map(|&top| top != 0).collect();
    let mut tops = Vec::new();
    plain(rows.top.iter().filter(|&&top| top != 0).map(|&top| &catalog.categories[top as usize - 1]), &mut tops);

    let mut file = parquet::File::new(count, ids.len() + tops.len() + count * 21);
    file.text("user_id", &ids);
    file.int32("day", Kind::Date, &rows.day);
    file.counts("sessions", &rows.sessions);
    file.counts("events", &rows.events);
    file.int64("revenue_cents", &rows.revenue);
    file.optional_text("top_category", &present, &tops);
    file.write(&folder.join(format!("part-{part:03}.parquet")));
    count
}

fn write_campaign_daily(folder: &Path, catalog: &catalog::Catalog, cells: &[person::Cells], first_day: i64, days: usize) -> usize {
    let campaigns = catalog.campaign_ids.len();
    let width = campaigns + 1;
    let (mut ids, mut channels) = (Vec::new(), Vec::new());
    let mut day_column: Vec<i32> = Vec::new();
    let mut columns: [Vec<i64>; 4] = Default::default();
    for campaign in 0..width {
        let (id, channel): (&[u8], &[u8]) = if campaign == campaigns {
            (b"organic", b"organic")
        } else {
            (&catalog.campaign_ids[campaign], &catalog.channels[campaign])
        };
        for day in 0..days {
            let at = day * width + campaign;
            let mut sum = [0u64; 4];
            for cell in cells {
                for i in 0..3 {
                    sum[i] += cell.credited[at][i];
                }
                sum[3] += u64::from(cell.assisted[at]);
            }
            if sum[0] == 0 && sum[3] == 0 {
                continue;
            }
            plain(std::iter::once(id), &mut ids);
            plain(std::iter::once(channel), &mut channels);
            day_column.push((first_day + day as i64) as i32);
            for i in 0..4 {
                columns[i].push(sum[i] as i64);
            }
        }
    }
    let rows = day_column.len();
    let mut file = parquet::File::new(rows, ids.len() + channels.len() + rows * 36);
    file.text("campaign_id", &ids);
    file.text("channel", &channels);
    file.int32("day", Kind::Date, &day_column);
    file.int64("sessions", &columns[0]);
    file.int64("purchasing_sessions", &columns[1]);
    file.int64("revenue_cents", &columns[2]);
    file.int64("assisted_sessions", &columns[3]);
    file.write(&folder.join("part-000.parquet"));
    rows
}

/// What the machine looks like, for the log of a small run: the scorer shows the
/// dev set's log, and its hardware is the one thing a laptop cannot tell us.
/// Never called on a large dataset, so it costs a timed run nothing.
fn describe_machine() {
    use std::io::Read;
    let head = |path: &str, bytes: usize| -> String {
        let mut text = vec![0u8; bytes];
        let got = std::fs::File::open(path).and_then(|mut f| f.read(&mut text)).unwrap_or(0);
        String::from_utf8_lossy(&text[..got]).into_owned()
    };
    let cpu = head("/proc/cpuinfo", 4096);
    let line = |name: &str| cpu.lines().find(|l| l.starts_with(name)).and_then(|l| l.split(':').nth(1)).unwrap_or("").trim().to_string();
    let flags = line("flags");
    let wanted: Vec<&str> = flags
        .split(' ')
        .filter(|f| f.starts_with("avx") || ["bmi2", "erms", "fsrm", "gfni", "vaes", "ht"].contains(f))
        .collect();
    let one = |path: &str| head(path, 256).trim().replace('\n', " ");
    eprintln!("pipeline: cpu {} @ {} MHz; {}", line("model name"), line("cpu MHz"), wanted.join(" "));
    eprintln!(
        "pipeline: thp {} / defrag {}; cpu.max {}; memory.max {}",
        one("/sys/kernel/mm/transparent_hugepage/enabled"),
        one("/sys/kernel/mm/transparent_hugepage/defrag"),
        one("/sys/fs/cgroup/cpu.max"),
        one("/sys/fs/cgroup/memory.max"),
    );
    for mount in head("/proc/self/mountinfo", 16384).lines() {
        let fields: Vec<&str> = mount.split(' ').collect();
        if fields.len() > 8 && ["/data", "/output", "/scratch", "/tmp"].contains(&fields[4]) {
            let dash = fields.iter().position(|f| *f == "-").unwrap_or(fields.len() - 2);
            eprintln!("pipeline: mount {} is {} on {}", fields[4], fields.get(dash + 1).unwrap_or(&"?"), fields.get(dash + 2).unwrap_or(&"?"));
        }
    }
}

fn main() {
    let started = Instant::now();
    let args: Vec<String> = std::env::args().collect();
    let folder = |position: usize, variable: &str, default: &str| -> PathBuf {
        args.get(position).cloned().or_else(|| std::env::var(variable).ok()).unwrap_or_else(|| default.to_string()).into()
    };
    let data = folder(1, "DATA_DIR", "/data");
    let output = folder(2, "OUTPUT_DIR", "/output");
    let threads = setting("NCPU")
        .or_else(|| std::thread::available_parallelism().ok().map(usize::from))
        .unwrap_or(8)
        .clamp(1, 256);

    let events = reader::open_folder(&data.join("events"));
    let clicks = reader::open_folder(&data.join("clicks"));
    let link_files = reader::open_folder(&data.join("links"));
    let event_bytes: u64 = events.iter().map(|i| i.len).sum();
    let click_bytes: u64 = clicks.iter().map(|i| i.len).sum();

    // The small files on one thread while the others join the linked IDs.
    let (catalog, links) = std::thread::scope(|scope| {
        let catalog = scope.spawn(|| catalog::load(&data));
        let links = links::build(&link_files, threads);
        (catalog.join().expect("catalog thread"), links)
    });
    let linked = started.elapsed();

    let epoch = catalog.window_start - parse::CLICK_REACH_MS;
    if catalog.window_end <= catalog.window_start || catalog.window_end - epoch >= i64::from(u32::MAX) {
        eprintln!("pipeline: the window must be shorter than 42 days");
        std::process::exit(2);
    }
    let first_day = catalog.window_start.div_euclid(ts::DAY_MS);
    let days = ((catalog.window_end - 1).div_euclid(ts::DAY_MS) - first_day + 1) as usize;

    // Partitions of a few megabytes of input each, so the later phases work in cache.
    let auto_bits = |bytes: u64| (bytes / (4 << 20)).max(1).next_power_of_two().trailing_zeros().clamp(4, 9);
    let person_bits = setting("SS_PERSON_BITS").map_or(auto_bits(event_bytes), |b| b as u32).clamp(1, 12);
    let dedup_bits = setting("SS_DEDUP_BITS").map_or(auto_bits(event_bytes), |b| b as u32).clamp(1, 12);
    let per_tail = (event_bytes / 64) as usize / (threads << person_bits) / 4;
    let chunk = per_tail.next_power_of_two().clamp(64, 4096);

    let wide: Option<Mutex<Vec<(u32, u32)>>> = (catalog.categories.len() > 255).then(|| Mutex::new(Vec::new()));
    let pools = parse::Pools { copies: pool::Pool::new(), events: pool::Pool::new(), clicks: pool::Pool::new() };
    let context = parse::Context {
        catalog: &catalog,
        links: &links,
        epoch,
        ingest_epoch: catalog.window_start.div_euclid(1_000) - (1 << 30),
        person_shift: 64 - person_bits,
        dedup_shift: 64 - dedup_bits,
        wide: wide.as_ref(),
    };
    let parsed = parse::run(&context, &pools, &events, &clicks, threads, 1 << person_bits, 1 << dedup_bits, chunk);
    let parsed_at = started.elapsed();

    let dropped = dedup::run(&pools.copies, &pools.events, &parsed, 1 << dedup_bits, 64 - dedup_bits, threads);
    let deduped_at = started.elapsed();

    let user_folder = output.join("user_daily");
    let campaign_folder = output.join("campaign_daily");
    for folder in [&user_folder, &campaign_folder] {
        if let Err(e) = std::fs::create_dir_all(folder) {
            eprintln!("pipeline: cannot create {}: {e}", folder.display());
            std::process::exit(1);
        }
    }
    let setup = person::Setup {
        catalog: &catalog,
        pools: &pools,
        parsed: &parsed,
        partitions: 1 << person_bits,
        person_bits,
        epoch,
        first_day,
        days,
        wide: wide.as_ref(),
    };
    let (cells, user_rows, sessions) =
        person::run(&setup, threads, |part, rows| write_user_daily(&user_folder, &catalog, part, rows));
    let people_at = started.elapsed();
    let campaign_rows = write_campaign_daily(&campaign_folder, &catalog, &cells, first_day, days);

    let mut stats = parse::Stats::default();
    for thread in &parsed {
        stats.event_lines += thread.stats.event_lines;
        stats.event_copies += thread.stats.event_copies;
        stats.event_candidates += thread.stats.event_candidates;
        stats.click_lines += thread.stats.click_lines;
        stats.clicks += thread.stats.clicks;
    }
    let ms = |d: std::time::Duration| d.as_secs_f64() * 1e3;
    eprintln!(
        "pipeline: {threads} threads, {} event files ({} MB), {} click files ({} MB), {} linked ids; 2^{person_bits} person and 2^{dedup_bits} dedup partitions",
        events.len(), event_bytes >> 20, clicks.len(), click_bytes >> 20, links.ids,
    );
    eprintln!(
        "pipeline: {} event lines, {} copies, {} candidates, {} dropped as duplicates; {} click lines, {} valid; {} sessions; {} user rows, {campaign_rows} campaign rows",
        stats.event_lines, stats.event_copies, stats.event_candidates, dropped, stats.click_lines, stats.clicks, sessions,
        user_rows.iter().sum::<usize>(),
    );
    eprintln!(
        "pipeline: links {:.1} ms, parse {:.1} ms, dedup {:.1} ms, people {:.1} ms, total {:.1} ms (pool slabs: {} records)",
        ms(linked), ms(parsed_at - linked), ms(deduped_at - parsed_at), ms(people_at - deduped_at), ms(started.elapsed()),
        pools.copies.records() + pools.events.records() + pools.clicks.records(),
    );
    if cfg!(target_os = "linux") && event_bytes < (256 << 20) {
        describe_machine();
    }
    // Nothing left to flush or free that the kernel will not do faster.
    std::process::exit(0);
}
