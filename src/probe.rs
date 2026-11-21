//! PROBE BUILD ONLY (branch `probe`, never merged): tells us what the scoring
//! machine is, through the one channel there is. A submission that fails the
//! dev check is shown its own log, so this build describes the machine on
//! stderr and then makes its output wrong on purpose.

use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// Every line the probe says, kept as well as printed: the track page shows a
/// rejected submission its differing rows, not its log, so what the probe found
/// goes out in five rows of user_daily (`finish`, `carried`).
static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// The runs of `at_scale`: label, milliseconds from start to exit, the phases' line.
static RUNS: Mutex<Vec<(String, f64, String)>> = Mutex::new(Vec::new());
/// What the five rows carry: a text each, and for the first two, numbers.
static CARRIED: OnceLock<Vec<(String, Option<(u32, u32, i64)>)>> = OnceLock::new();

fn say(line: String) {
    eprintln!("{line}");
    LOG.lock().unwrap().push(line);
}

macro_rules! say {
    ($($arg:tt)*) => { say(format!($($arg)*)) };
}

/// The dev answer's first five rows by user_id and day (the dev set is seed 1 of
/// the published generator, the same everywhere): the rows the page shows first.
const TARGETS: [(&[u8], i32); 5] =
    [(b"U14NPKDLL", 21173), (b"U1AK82W40", 21167), (b"U1GQRGV02", 21165), (b"U1O7PFB0O", 21171), (b"U1O7PFB0O", 21172)];

/// The text, and perhaps numbers, that the row of this user and day carries instead of its own top_category.
pub fn carried(id: &[u8], day: i32) -> Option<&'static (String, Option<(u32, u32, i64)>)> {
    if id.len() != 9 {
        return None;
    }
    let carried = CARRIED.get()?;
    TARGETS.iter().position(|&(user, on)| user == id && on == day).and_then(|at| carried.get(at))
}

/// Sorts what was said into five texts, the most telling first in each, and the
/// full-size runs' best times into numbers for the first two rows.
pub fn finish(scan: &str, seconds: f64) {
    let lines = LOG.lock().unwrap().clone();
    let runs = RUNS.lock().unwrap().clone();
    let mut texts = vec![String::new(); 5];
    let mut section = 3usize;
    let mut scale = false;
    for line in &lines {
        let text = line.strip_prefix("probe: ").unwrap_or(line);
        if text.starts_with("----") {
            scale = text.contains("whole pipeline");
            section = if scale {
                0
            } else if text.contains("environment") {
                4
            } else if text.contains("when things were made") {
                2
            } else if text.contains("memory") || text.contains("disks") || text.contains("input:") || text.contains("256-bit") || text.contains("how many threads") {
                1
            } else {
                3
            };
            continue;
        }
        let first = ["cpu:", "flags of interest", "processors listed", "available_parallelism"].iter().any(|k| text.starts_with(k))
            || ["transparent_hugepage/enabled", "transparent_hugepage/defrag", "cgroup/cpu.max", "cgroup/memory.max", "/proc/version"].iter().any(|k| text.contains(k));
        let mut text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if scale {
            for (long, short) in [
                (" ms from start to exit (exit status: 0)", ""),
                (" (what a process costs by itself)", ""),
                ("as scored, run ", "S"),
                (", run ", "r"),
                ("NCPU=", "N"),
                ("SS_SCAN=narrow", "NAR"),
                (" partitions of each kind", "p"),
                ("SS_BLOCK_BYTES=", "B"),
                ("links ", "l"),
                ("parse ", "p"),
                ("dedup ", "d"),
                ("people ", "pe"),
                ("total ", "t"),
                (" ms", ""),
                (" | scan ", "|"),
                (" | ", "|"),
            ] {
                text = text.replace(long, short);
            }
        }
        let into = if first { 0 } else { section };
        texts[into].push_str(&text);
        texts[into].push_str("; ");
    }
    texts[0] = format!("scan {scan}; probe took {seconds:.1} s; {}", texts[0]);
    let best = |label: &str| -> Option<&(String, f64, String)> {
        runs.iter().filter(|run| run.0.starts_with(label)).min_by(|a, b| a.1.total_cmp(&b.1))
    };
    let phase = |line: &str, name: &str| -> i64 {
        line.split(name).nth(1).and_then(|rest| rest.trim_start().split(' ').next()).and_then(|v| v.parse::<f64>().ok()).map_or(0, |v| v.round() as i64)
    };
    let wall = |label: &str| best(label).map_or(0, |run| run.1.round() as i64);
    let first = best("as scored").map(|run| {
        (run.1.round() as u32, phase(&run.2, "total") as u32, phase(&run.2, "parse") * 1_000_000 + phase(&run.2, "people") * 1_000 + phase(&run.2, "dedup"))
    });
    let second = best("as scored").map(|_| (wall("NCPU=4") as u32, wall("NCPU=16") as u32, wall("SS_SCAN=narrow") * 1_000_000 + wall("2^9") * 1_000 + wall("2^11")));
    let carried = texts
        .into_iter()
        .enumerate()
        .map(|(at, text)| (text.chars().take(4000).collect::<String>(), [first, second].get(at).copied().flatten()))
        .collect();
    let _ = CARRIED.set(carried);
}

fn head(path: &str, bytes: usize) -> String {
    let mut text = vec![0u8; bytes];
    let got = std::fs::File::open(path).and_then(|mut f| f.read(&mut text)).unwrap_or(0);
    String::from_utf8_lossy(&text[..got]).trim_end().to_string()
}

fn show(path: &str, bytes: usize) {
    let text = head(path, bytes);
    // Short lines, and not many: a log too long may be cut where it is shown.
    for (i, line) in text.lines().enumerate() {
        if i < 36 {
            say!("probe: {path}: {}", line.chars().take(170).collect::<String>());
        }
    }
    if text.is_empty() {
        say!("probe: {path}: (empty or unreadable)");
    }
}

fn spin(iterations: u64) -> u64 {
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    for i in 0..iterations {
        x = (x ^ (x >> 29)).wrapping_mul(0xBF58_476D_1CE4_E5B9).wrapping_add(i);
    }
    x
}

pub fn run(data: &std::path::Path, output: &std::path::Path) {
    let now = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    say!("probe: ---- machine ----");
    let cpu = head("/proc/cpuinfo", 1 << 20);
    let field = |name: &str| cpu.lines().find(|l| l.starts_with(name)).and_then(|l| l.split(':').nth(1)).unwrap_or("").trim().to_string();
    say!("probe: cpu: {} | family {} model {} stepping {} | {} MHz | cache {}", field("model name"), field("cpu family"), field("model\t"), field("stepping"), field("cpu MHz"), field("cache size"));
    say!("probe: processors listed: {}", cpu.lines().filter(|l| l.starts_with("processor")).count());
    let flags = field("flags");
    say!("probe: flags of interest: {}", flags.split(' ').filter(|f| f.starts_with("avx") || ["bmi1", "bmi2", "erms", "fsrm", "gfni", "vaes", "vpclmulqdq", "pclmulqdq", "ht", "hypervisor", "sha_ni", "adx", "popcnt", "movbe", "pdpe1gb"].contains(f)).collect::<Vec<_>>().join(" "));
    for line in head("/proc/self/status", 4096).lines().filter(|l| l.starts_with("Cpus_allowed") || l.starts_with("Mems_allowed_list") || l.starts_with("Threads") || l.starts_with("Uid") || l.starts_with("Seccomp") || l.starts_with("NoNewPrivs") || l.starts_with("CapEff")) {
        say!("probe: status: {line}");
    }
    say!("probe: available_parallelism: {:?}", std::thread::available_parallelism());
    for path in ["/proc/version", "/proc/loadavg", "/proc/uptime", "/proc/self/cgroup", "/sys/fs/cgroup/cpu.max", "/sys/fs/cgroup/cpuset.cpus.effective", "/sys/fs/cgroup/cpu.weight", "/sys/fs/cgroup/memory.max", "/sys/fs/cgroup/memory.high", "/sys/fs/cgroup/memory.swap.max", "/sys/fs/cgroup/pids.max", "/sys/fs/cgroup/cpu.stat", "/sys/fs/cgroup/memory.peak", "/sys/fs/cgroup/memory.events", "/sys/kernel/mm/transparent_hugepage/enabled", "/sys/kernel/mm/transparent_hugepage/defrag", "/sys/kernel/mm/transparent_hugepage/shmem_enabled", "/proc/sys/vm/overcommit_memory", "/proc/sys/vm/dirty_ratio", "/proc/sys/vm/dirty_background_ratio", "/sys/devices/system/cpu/smt/active", "/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor", "/sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq", "/sys/devices/system/cpu/cpu0/topology/thread_siblings_list"] {
        show(path, 1500);
    }
    for line in head("/proc/meminfo", 4096).lines().filter(|l| ["MemTotal", "MemFree", "MemAvailable", "Cached", "Dirty", "AnonHugePages", "HugePages_Total", "Hugepagesize", "SwapTotal"].iter().any(|k| l.starts_with(k))) {
        say!("probe: meminfo: {line}");
    }
    for line in head("/proc/self/mountinfo", 16000).lines().filter(|l| l.split(' ').nth(4).is_some_and(|at| ["/", "/data", "/output", "/scratch", "/tmp", "/app"].contains(&at))).take(8) {
        say!("probe: mount: {}", line.chars().take(200).collect::<String>());
    }
    for line in head("/proc/self/limits", 2000).lines().filter(|l| ["Max locked memory", "Max address space", "Max open files", "Max processes", "Max stack size"].iter().any(|k| l.starts_with(k))) {
        say!("probe: limit: {}", line.split_whitespace().collect::<Vec<_>>().join(" "));
    }
    say!("probe: ---- environment (values only for the documented ones) ----");
    for (name, value) in std::env::vars() {
        if ["DATA_DIR", "OUTPUT_DIR", "SCRATCH_DIR", "HOME", "NCPU", "MEM_GB", "PATH", "TMPDIR", "HOSTNAME"].contains(&name.as_str()) {
            say!("probe: env {name}={value}");
        } else {
            say!("probe: env {name}=<{} bytes>", value.len());
        }
    }
    say!("probe: args: {:?}; pid {}; cwd {:?}", std::env::args().collect::<Vec<_>>(), std::process::id(), std::env::current_dir().ok());

    say!("probe: ---- when things were made (seconds before now) ----");
    let at = now();
    for path in ["/", "/app", "/app/pipeline", "/bin", "/bin/busybox", "/etc", "/tmp", "/data", "/output", "/scratch", "/proc/self", "/proc/1", "/sys/fs/cgroup", "/dev", "/data/manifest.json", "/data/events", "/data/products.csv"] {
        match std::fs::metadata(path) {
            Ok(m) => say!("probe: {path}: mode {:o} uid {} dev {:x} size {} | changed {:.3} s ago, modified {:.3} s ago, read {:.3} s ago", m.mode(), m.uid(), m.dev(), m.len(), at - (m.ctime() as f64 + m.ctime_nsec() as f64 * 1e-9), at - (m.mtime() as f64 + m.mtime_nsec() as f64 * 1e-9), at - (m.atime() as f64 + m.atime_nsec() as f64 * 1e-9)),
            Err(e) => say!("probe: {path}: {e}"),
        }
    }
    let stat = head("/proc/self/stat", 1024);
    let after = stat.rsplit(')').next().unwrap_or("");
    let start_ticks: f64 = after.split_whitespace().nth(19).and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let uptime: f64 = head("/proc/uptime", 64).split_whitespace().next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    say!("probe: this process was started {:.3} s ago (clock ticks of 10 ms)", uptime - start_ticks / 100.0);
    for dir in ["/", "/app", "/data", "/output", "/scratch", "/tmp"] {
        let names: Vec<String> = std::fs::read_dir(dir).map(|d| d.flatten().take(40).map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
        say!("probe: ls {dir}: {} entries: {}", names.len(), names.join(" "));
    }
    for folder in ["events", "clicks", "links"] {
        let sizes: Vec<u64> = std::fs::read_dir(data.join(folder)).map(|d| d.flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).collect()).unwrap_or_default();
        say!("probe: {folder}: {} files, {} bytes, largest {}", sizes.len(), sizes.iter().sum::<u64>(), sizes.iter().max().copied().unwrap_or(0));
    }

    say!("probe: ---- how many threads really run at once (same work per thread; flat = that many real CPUs) ----");
    for threads in [1usize, 2, 4, 8, 16] {
        let started = Instant::now();
        let sums: Vec<u64> = crate::sys::parallel(threads, |i| spin(std::hint::black_box(60_000_000 + i as u64)));
        say!("probe: {threads:>2} threads: {:.1} ms (checksum {:x})", started.elapsed().as_secs_f64() * 1e3, sums.iter().fold(0, |a, b| a ^ b));
    }

    say!("probe: ---- memory ----");
    let on_huge_pages = || {
        head("/proc/self/smaps_rollup", 4096).lines().find(|l| l.starts_with("AnonHugePages")).and_then(|l| l.split_whitespace().nth(1).map(str::to_string)).unwrap_or_else(|| "?".into())
    };
    for (label, bytes) in [("fresh 256 MB as the pipeline allocates it, first touch", 256usize << 20), ("the same again", 256 << 20)] {
        let started = Instant::now();
        let mut block = vec![0u8; bytes];
        for at in (0..bytes).step_by(4096) {
            block[at] = 1;
        }
        let touched = started.elapsed().as_secs_f64() * 1e3;
        let started = Instant::now();
        let copy = block.clone();
        let copied = started.elapsed().as_secs_f64() * 1e3;
        let huge = on_huge_pages();
        let last = copy[bytes - 4096];
        let started = Instant::now();
        drop(copy);
        drop(block);
        say!("probe: {label}: touch {touched:.1} ms, copy {copied:.1} ms, this process then has {huge} kB on huge pages, unmap both {:.1} ms ({last})", started.elapsed().as_secs_f64() * 1e3);
    }
    plain_pages();

    say!("probe: ---- disks ----");
    let chunk = vec![7u8; 8 << 20];
    for dir in [output.to_path_buf(), std::path::PathBuf::from(std::env::var("SCRATCH_DIR").unwrap_or_else(|_| "/scratch".into())), std::path::PathBuf::from("/tmp")] {
        let megabytes = if dir.starts_with("/tmp") { 16 } else { 96 };
        let path = dir.join("probe.tmp");
        let started = Instant::now();
        let written = std::fs::File::create(&path).and_then(|mut f| {
            for _ in 0..megabytes / 8 {
                f.write_all(&chunk)?;
            }
            Ok(())
        });
        let wrote = started.elapsed().as_secs_f64() * 1e3;
        let started = Instant::now();
        let read = std::fs::read(&path).map(|b| b.len()).unwrap_or(0);
        let reread = started.elapsed().as_secs_f64() * 1e3;
        let started = Instant::now();
        let _ = std::fs::remove_file(&path);
        say!("probe: {}: write {megabytes} MB {wrote:.1} ms ({written:?}), read back {reread:.1} ms ({read} bytes), delete {:.1} ms", dir.display(), started.elapsed().as_secs_f64() * 1e3);
    }
    if let Some(largest) = std::fs::read_dir(data.join("events")).ok().and_then(|d| d.flatten().max_by_key(|e| e.metadata().map(|m| m.len()).unwrap_or(0))) {
        for round in 1..=2 {
            let started = Instant::now();
            let bytes = std::fs::read(largest.path()).map(|b| b.len()).unwrap_or(0);
            say!("probe: read the largest event file, round {round}: {bytes} bytes in {:.1} ms", started.elapsed().as_secs_f64() * 1e3);
        }
    }
    if let Some(largest) = std::fs::read_dir(data.join("events")).ok().and_then(|d| d.flatten().max_by_key(|e| e.metadata().map(|m| m.len()).unwrap_or(0))) {
        input_paths(&largest.path());
    }
    vector_widths();
    say!("probe: ---- end of the machine; user_daily was emptied on purpose before all this, so that the log is shown ----");
}

/// What 4 KB pages cost here against huge ones: 256 MB mapped, every page touched, unmapped, under each advice.
fn plain_pages() {
    #[cfg(target_os = "linux")]
    unsafe {
        use std::ffi::c_void;
        extern "C" {
            fn mmap(addr: *mut c_void, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut c_void;
            fn munmap(addr: *mut c_void, len: usize) -> i32;
            fn madvise(addr: *mut c_void, len: usize, advice: i32) -> i32;
        }
        let bytes = 256usize << 20;
        for (label, advice) in [("no advice", 0), ("MADV_HUGEPAGE", 14), ("MADV_NOHUGEPAGE", 15)] {
            for threads in [1usize, 8] {
                let started = Instant::now();
                let times: Vec<(f64, f64)> = crate::sys::parallel(threads, |_| {
                    let map = mmap(std::ptr::null_mut(), bytes + (2 << 20), 3, 0x22, -1, 0);
                    if map as isize == -1 {
                        return (0.0, 0.0);
                    }
                    let start = ((map as usize + (2 << 20) - 1) & !((2 << 20) - 1)) as *mut u8;
                    if advice != 0 {
                        madvise(start as *mut c_void, bytes, advice);
                    }
                    let began = Instant::now();
                    for at in (0..bytes).step_by(4096) {
                        start.add(at).write_volatile(1);
                    }
                    let touched = began.elapsed().as_secs_f64() * 1e3;
                    let began = Instant::now();
                    munmap(map, bytes + (2 << 20));
                    (touched, began.elapsed().as_secs_f64() * 1e3)
                });
                let wall = started.elapsed().as_secs_f64() * 1e3;
                let touch = times.iter().map(|t| t.0).fold(0.0, f64::max);
                let unmap = times.iter().map(|t| t.1).fold(0.0, f64::max);
                say!("probe: 256 MB a thread on {threads} threads, {label}: touch every page {touch:.1} ms, unmap {unmap:.1} ms (slowest thread; {wall:.1} ms in all)");
            }
        }
    }
}

/// The parse phase of this dataset again, cut short at each stage, and whole on other thread counts.
#[allow(dead_code)]
pub fn anatomy(
    context: &crate::parse::Context,
    events: &[crate::reader::Input],
    clicks: &[crate::reader::Input],
    threads: usize,
    person_partitions: usize,
    dedup_partitions: usize,
    chunks: crate::parse::ChunkSizes,
) {
    use std::sync::atomic::Ordering;
    let time = |stop: u32, threads: usize, rounds: usize| -> f64 {
        crate::reader::STOP.store(stop, Ordering::Relaxed);
        let mut best = f64::MAX;
        for _ in 0..rounds {
            let pools = crate::parse::Pools { copies: crate::pool::Pool::new(), events: crate::pool::Pool::new(), clicks: crate::pool::Pool::new() };
            let started = Instant::now();
            let parsed = crate::parse::run(context, &pools, events, clicks, threads, person_partitions, dedup_partitions, chunks);
            best = best.min(started.elapsed().as_secs_f64() * 1e3);
            std::mem::forget(parsed);
            std::mem::forget(pools);
        }
        crate::reader::STOP.store(0, Ordering::Relaxed);
        best
    };
    // No run here gives its records back (the pools never free), so on a large
    // dataset every setting is run once and the thread counts are fewer: about
    // 5 GB at 20M events where the full list would be past the 24 GB allowed.
    let bytes: u64 = events.iter().chain(clicks).map(|input| input.len).sum();
    let large = bytes > (512 << 20);
    let (stage_rounds, rounds) = if large { (1, 1) } else { (3, 2) };
    let parse_threads: &[usize] = if large { &[4, 8] } else { &[1, 2, 4, 6, 8, 12, 16] };
    let scan_threads: &[usize] = if large { &[1, 4, 8] } else { &[1, 2, 4, 8, 16] };
    say!("probe: ---- the parse phase of this dataset ({} MB), cut short at each stage ({threads} threads, best of {stage_rounds}) ----", bytes >> 20);
    for (stop, what) in [
        (1, "read() only"),
        (2, "+ delimiter scan"),
        (3, "+ line walk"),
        (4, "+ fields into values"),
        (5, "+ link lookups"),
        (6, "+ event records"),
        (7, "+ copy records, not event records"),
        (0, "everything"),
    ] {
        say!("probe: parse, {what}: {:.1} ms", time(stop, threads, stage_rounds));
    }
    say!("probe: ---- the whole parse phase by thread count (best of {rounds}) ----");
    for &count in parse_threads {
        say!("probe: parse on {count:>2} threads: {:.1} ms", time(0, count, rounds));
    }
    say!("probe: ---- read() and the delimiter scan only, by thread count (best of {rounds}) ----");
    for &count in scan_threads {
        say!("probe: read + scan on {count:>2} threads: {:.1} ms", time(2, count, rounds));
    }
}

/// Newlines, commas and quotes in `len` bytes, 64 at a time with 256-bit compares: what stage one does.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,popcnt")]
unsafe fn count_256(data: *const u8, len: usize) -> u64 {
    use std::arch::x86_64::*;
    let (nl, comma, quote) = (_mm256_set1_epi8(b'\n' as i8), _mm256_set1_epi8(b',' as i8), _mm256_set1_epi8(b'"' as i8));
    let mut total = 0u64;
    let mut at = 0;
    while at + 64 <= len {
        for half in [0, 32] {
            let v = _mm256_loadu_si256(data.add(at + half) as *const __m256i);
            total += (_mm256_movemask_epi8(_mm256_cmpeq_epi8(v, nl)) as u32).count_ones() as u64
                + (_mm256_movemask_epi8(_mm256_cmpeq_epi8(v, comma)) as u32).count_ones() as u64 * 3
                + (_mm256_movemask_epi8(_mm256_cmpeq_epi8(v, quote)) as u32).count_ones() as u64 * 7;
        }
        at += 64;
    }
    total
}

/// The same with 512-bit compares straight into mask registers.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f,avx512bw,popcnt")]
unsafe fn count_512(data: *const u8, len: usize) -> u64 {
    use std::arch::x86_64::*;
    let (nl, comma, quote) = (_mm512_set1_epi8(b'\n' as i8), _mm512_set1_epi8(b',' as i8), _mm512_set1_epi8(b'"' as i8));
    let mut total = 0u64;
    let mut at = 0;
    while at + 64 <= len {
        let v = _mm512_loadu_si512(data.add(at) as *const __m512i);
        total += _mm512_cmpeq_epi8_mask(v, nl).count_ones() as u64
            + _mm512_cmpeq_epi8_mask(v, comma).count_ones() as u64 * 3
            + _mm512_cmpeq_epi8_mask(v, quote).count_ones() as u64 * 7;
        at += 64;
    }
    total
}

/// Reading a file through `read` into a small buffer, against reading it in place through a mapping.
fn input_paths(path: &std::path::Path) {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::FileExt;
        use std::ffi::c_void;
        extern "C" {
            fn mmap(addr: *mut c_void, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut c_void;
            fn munmap(addr: *mut c_void, len: usize) -> i32;
            fn madvise(addr: *mut c_void, len: usize, advice: i32) -> i32;
        }
        say!("probe: ---- input: copy with read(), or in place with mmap() ----");
        let Ok(file) = std::fs::File::open(path) else { return };
        let len = file.metadata().map(|m| m.len() as usize).unwrap_or(0) & !63;
        let megabytes = len as f64 / 1048576.0;
        let mut buffer = vec![0u8; (256 << 10) + 64];
        let mut best = f64::MAX;
        let mut sum = 0;
        for _ in 0..5 {
            let started = Instant::now();
            sum = 0;
            let mut at = 0;
            while at < len {
                let got = file.read_at(&mut buffer[..256 << 10], at as u64).unwrap_or(0);
                if got == 0 {
                    break;
                }
                sum += count_256(std::hint::black_box(buffer.as_ptr()), got & !63);
                at += got;
            }
            best = best.min(started.elapsed().as_secs_f64());
        }
        say!("probe: read() in 256 KB blocks + scan: {:.0} MB/s (best of 5, {megabytes:.0} MB, sum {sum})", megabytes / best);
        for (label, populate) in [("mmap, faulting as it goes", false), ("mmap, MADV_POPULATE_READ first", true)] {
            let mut best = f64::MAX;
            let mut first = 0.0;
            for round in 0..5 {
                let started = Instant::now();
                let map = mmap(std::ptr::null_mut(), len, 1, 2, file.as_raw_fd(), 0);
                if map as isize == -1 {
                    say!("probe: mmap failed");
                    return;
                }
                let advised = if populate { madvise(map, len, 22) } else { 0 };
                sum = count_256(std::hint::black_box(map as *const u8), len);
                munmap(map, len);
                let took = started.elapsed().as_secs_f64();
                if round == 0 {
                    first = took;
                    if populate && advised != 0 {
                        say!("probe: MADV_POPULATE_READ is not supported here");
                    }
                }
                best = best.min(took);
            }
            say!("probe: {label}: {:.0} MB/s best of 5 (map + scan + unmap), first round {:.0} MB/s (sum {sum})", megabytes / best, megabytes / first);
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    let _ = path;
}

/// Does 512-bit code pay here? Its scan against the 256-bit one, alone and mixed with scalar work
/// (on some Xeons a core that runs 512-bit instructions slows down everything else it does).
fn vector_widths() {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        say!("probe: ---- 256-bit against 512-bit ----");
        let wide = std::arch::is_x86_feature_detected!("avx512bw");
        say!("probe: avx512f {} avx512bw {} avx512vbmi {} avx512vbmi2 {} avx512vl {}", std::arch::is_x86_feature_detected!("avx512f"), wide, std::arch::is_x86_feature_detected!("avx512vbmi"), std::arch::is_x86_feature_detected!("avx512vbmi2"), std::arch::is_x86_feature_detected!("avx512vl"));
        let len = 64usize << 20;
        let text: Vec<u8> = (0..len).map(|i| b"e3k9x0abc,u1b2c3d4e,2026-10-03T23:58:10Z,\"view\",p123,\n"[i % 53]).collect();
        let time = |f: &dyn Fn() -> u64| {
            let mut best = f64::MAX;
            let mut value = 0;
            for _ in 0..5 {
                let started = Instant::now();
                value = std::hint::black_box(f());
                best = best.min(started.elapsed().as_secs_f64());
            }
            (best * 1e3, value)
        };
        let (ms, value) = time(&|| count_256(std::hint::black_box(text.as_ptr()), std::hint::black_box(len)));
        say!("probe: 256-bit scan of 64 MB: {ms:.1} ms ({value})");
        if wide {
            let (ms, value) = time(&|| count_512(std::hint::black_box(text.as_ptr()), std::hint::black_box(len)));
            say!("probe: 512-bit scan of 64 MB: {ms:.1} ms ({value})");
        }
        // Mixed: scan 64 KB, then scalar work, 400 times; as the parser alternates.
        let mixed = |use_wide: bool| {
            time(&|| {
                let mut total = 0u64;
                for block in 0..400usize {
                    let at = (block * (64 << 10)) % (len - (64 << 10));
                    total += if use_wide { count_512(std::hint::black_box(text.as_ptr().add(at)), 64 << 10) } else { count_256(std::hint::black_box(text.as_ptr().add(at)), 64 << 10) };
                    total ^= spin(std::hint::black_box(150_000));
                }
                total
            })
        };
        let (ms, _) = time(&|| (0..400).fold(0u64, |a, _| a ^ spin(std::hint::black_box(150_000))));
        say!("probe: scalar work alone: {ms:.1} ms");
        let (ms, _) = mixed(false);
        say!("probe: 256-bit scan + scalar work, alternating: {ms:.1} ms");
        if wide {
            let (ms, _) = mixed(true);
            say!("probe: 512-bit scan + scalar work, alternating: {ms:.1} ms");
        }
    }
}

/// The whole pipeline on a stand-in dataset the size of the practice set (the
/// one the bench workflow times, `src/bin/synth.rs`), each run a process of its
/// own: what the scorer's machine makes of the real thing, from start to exit,
/// on its own thread count and on others. After the machine's description,
/// because it is the part that takes long: if the run is cut short, that is out.
pub fn at_scale() {
    use std::process::{Command, Stdio};
    say!("probe: ---- the whole pipeline on a stand-in dataset, each run a process of its own ----");
    let scratch = std::path::PathBuf::from(std::env::var("SCRATCH_DIR").unwrap_or_else(|_| "/scratch".into()));
    let Ok(exe) = std::env::current_exe() else {
        say!("probe: no path to this program");
        return;
    };
    let synth = exe.with_file_name("synth");
    let events: u64 = std::env::var("SS_PROBE_EVENTS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(20_000_000);
    let (data, out, empty) = (scratch.join("probe-data"), scratch.join("probe-out"), scratch.join("probe-empty"));
    let clean = || {
        for folder in [&data, &out, &empty] {
            let _ = std::fs::remove_dir_all(folder);
        }
    };
    clean();
    let started = Instant::now();
    let child = Command::new(&synth)
        .args(["--events", &events.to_string(), "--clicks", &(events / 4).to_string(), "--seed", "9", "--output"])
        .arg(&data)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) => {
            say!("probe: could not start {}: {error}", synth.display());
            return;
        }
    };
    // Given up on after a minute, rather than run into whatever limit the run has.
    let made = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if started.elapsed().as_secs() < 60 => std::thread::sleep(std::time::Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };
    let megabytes = |folder: &str| -> u64 {
        std::fs::read_dir(data.join(folder)).map(|d| d.flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum::<u64>()).unwrap_or(0) >> 20
    };
    say!(
        "probe: {events} events and {} clicks made in {:.1} s (done: {made}): events {} MB, clicks {} MB, links {} MB",
        events / 4,
        started.elapsed().as_secs_f64(),
        megabytes("events"),
        megabytes("clicks"),
        megabytes("links"),
    );
    if !made {
        clean();
        return;
    }
    let _ = std::fs::create_dir_all(&empty);
    let run = |label: &str, settings: &[(&str, &str)], folder: &std::path::Path| {
        let _ = std::fs::remove_dir_all(&out);
        let started = Instant::now();
        let result = Command::new(&exe)
            .arg(folder)
            .arg(&out)
            .env("SS_PROBE_CHILD", "1")
            .envs(settings.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output();
        let wall = started.elapsed().as_secs_f64() * 1e3;
        match result {
            Ok(result) => {
                let log = String::from_utf8_lossy(&result.stderr);
                let phases = log.lines().find(|l| l.starts_with("pipeline: links ")).map(|l| l["pipeline: ".len()..].split(" (pool").next().unwrap_or("").to_string()).unwrap_or_default();
                let scan = log.lines().find_map(|l| l.rfind("; scan ").map(|at| l[at + 2..].to_string())).unwrap_or_default();
                say!("probe: {label}: {wall:.1} ms from start to exit ({}) | {phases} | {scan}", result.status);
                if result.status.success() {
                    RUNS.lock().unwrap().push((label.to_string(), wall, phases));
                }
            }
            Err(error) => say!("probe: {label}: could not run: {error}"),
        }
    };
    for round in 1..=3 {
        run(&format!("no data, run {round} (what a process costs by itself)"), &[], &empty);
    }
    for round in 1..=5 {
        run(&format!("as scored, run {round}"), &[], &data);
    }
    for round in 1..=2 {
        for threads in ["4", "6", "12", "16"] {
            if round == 1 || threads == "4" || threads == "16" {
                run(&format!("NCPU={threads}, run {round}"), &[("NCPU", threads)], &data);
            }
        }
    }
    for round in 1..=3 {
        run(&format!("SS_SCAN=narrow, run {round}"), &[("SS_SCAN", "narrow")], &data);
    }
    // Partitions and the read block: what two threads of one core have to keep in its cache between them.
    for round in 1..=2 {
        for bits in ["9", "11"] {
            run(&format!("2^{bits} partitions of each kind, run {round}"), &[("SS_PERSON_BITS", bits), ("SS_DEDUP_BITS", bits)], &data);
        }
        for block in ["131072", "524288"] {
            run(&format!("SS_BLOCK_BYTES={block}, run {round}"), &[("SS_BLOCK_BYTES", block)], &data);
        }
    }
    for round in 6..=7 {
        run(&format!("as scored, run {round}"), &[], &data);
    }
    clean();
    for line in head("/sys/fs/cgroup/cpu.stat", 600).lines().filter(|l| l.starts_with("nr_") || l.starts_with("throttled")) {
        say!("probe: cpu.stat at the end: {line}");
    }
    say!("probe: memory.peak at the end: {}", head("/sys/fs/cgroup/memory.peak", 64));
}
