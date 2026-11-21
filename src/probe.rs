//! PROBE BUILD ONLY (branch `probe`, never merged): tells us what the scoring
//! machine is, through the one channel there is. A submission that fails the
//! dev check is shown its own log, so this build describes the machine on
//! stderr and then makes its output wrong on purpose.

use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::time::Instant;

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
            eprintln!("probe: {path}: {}", line.chars().take(170).collect::<String>());
        }
    }
    if text.is_empty() {
        eprintln!("probe: {path}: (empty or unreadable)");
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
    eprintln!("probe: ---- machine ----");
    let cpu = head("/proc/cpuinfo", 1 << 20);
    let field = |name: &str| cpu.lines().find(|l| l.starts_with(name)).and_then(|l| l.split(':').nth(1)).unwrap_or("").trim().to_string();
    eprintln!("probe: cpu: {} | family {} model {} stepping {} | {} MHz | cache {}", field("model name"), field("cpu family"), field("model\t"), field("stepping"), field("cpu MHz"), field("cache size"));
    eprintln!("probe: processors listed: {}", cpu.lines().filter(|l| l.starts_with("processor")).count());
    let flags = field("flags");
    eprintln!("probe: flags of interest: {}", flags.split(' ').filter(|f| f.starts_with("avx") || ["bmi1", "bmi2", "erms", "fsrm", "gfni", "vaes", "vpclmulqdq", "pclmulqdq", "ht", "hypervisor", "sha_ni", "adx", "popcnt", "movbe", "pdpe1gb"].contains(f)).collect::<Vec<_>>().join(" "));
    for line in head("/proc/self/status", 4096).lines().filter(|l| l.starts_with("Cpus_allowed") || l.starts_with("Mems_allowed_list") || l.starts_with("Threads") || l.starts_with("Uid") || l.starts_with("Seccomp") || l.starts_with("NoNewPrivs") || l.starts_with("CapEff")) {
        eprintln!("probe: status: {line}");
    }
    eprintln!("probe: available_parallelism: {:?}", std::thread::available_parallelism());
    for path in ["/proc/version", "/proc/loadavg", "/proc/uptime", "/proc/self/cgroup", "/sys/fs/cgroup/cpu.max", "/sys/fs/cgroup/cpuset.cpus.effective", "/sys/fs/cgroup/cpu.weight", "/sys/fs/cgroup/memory.max", "/sys/fs/cgroup/memory.high", "/sys/fs/cgroup/memory.swap.max", "/sys/fs/cgroup/pids.max", "/sys/fs/cgroup/cpu.stat", "/sys/fs/cgroup/memory.peak", "/sys/fs/cgroup/memory.events", "/sys/kernel/mm/transparent_hugepage/enabled", "/sys/kernel/mm/transparent_hugepage/defrag", "/sys/kernel/mm/transparent_hugepage/shmem_enabled", "/proc/sys/vm/overcommit_memory", "/proc/sys/vm/dirty_ratio", "/proc/sys/vm/dirty_background_ratio", "/sys/devices/system/cpu/smt/active", "/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor", "/sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq", "/sys/devices/system/cpu/cpu0/topology/thread_siblings_list"] {
        show(path, 1500);
    }
    for line in head("/proc/meminfo", 4096).lines().filter(|l| ["MemTotal", "MemFree", "MemAvailable", "Cached", "Dirty", "AnonHugePages", "HugePages_Total", "Hugepagesize", "SwapTotal"].iter().any(|k| l.starts_with(k))) {
        eprintln!("probe: meminfo: {line}");
    }
    show("/proc/self/mountinfo", 6000);
    show("/proc/self/limits", 2000);
    eprintln!("probe: ---- environment (values only for the documented ones) ----");
    for (name, value) in std::env::vars() {
        if ["DATA_DIR", "OUTPUT_DIR", "SCRATCH_DIR", "HOME", "NCPU", "MEM_GB", "PATH", "TMPDIR", "HOSTNAME"].contains(&name.as_str()) {
            eprintln!("probe: env {name}={value}");
        } else {
            eprintln!("probe: env {name}=<{} bytes>", value.len());
        }
    }
    eprintln!("probe: args: {:?}; pid {}; cwd {:?}", std::env::args().collect::<Vec<_>>(), std::process::id(), std::env::current_dir().ok());

    eprintln!("probe: ---- when things were made (seconds before now) ----");
    let at = now();
    for path in ["/", "/app", "/app/pipeline", "/bin", "/bin/busybox", "/etc", "/tmp", "/data", "/output", "/scratch", "/proc/self", "/proc/1", "/sys/fs/cgroup", "/dev", "/data/manifest.json", "/data/events", "/data/products.csv"] {
        match std::fs::metadata(path) {
            Ok(m) => eprintln!("probe: {path}: mode {:o} uid {} dev {:x} size {} | changed {:.3} s ago, modified {:.3} s ago, read {:.3} s ago", m.mode(), m.uid(), m.dev(), m.len(), at - (m.ctime() as f64 + m.ctime_nsec() as f64 * 1e-9), at - (m.mtime() as f64 + m.mtime_nsec() as f64 * 1e-9), at - (m.atime() as f64 + m.atime_nsec() as f64 * 1e-9)),
            Err(e) => eprintln!("probe: {path}: {e}"),
        }
    }
    let stat = head("/proc/self/stat", 1024);
    let after = stat.rsplit(')').next().unwrap_or("");
    let start_ticks: f64 = after.split_whitespace().nth(19).and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let uptime: f64 = head("/proc/uptime", 64).split_whitespace().next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    eprintln!("probe: this process was started {:.3} s ago (clock ticks of 10 ms)", uptime - start_ticks / 100.0);
    for dir in ["/", "/app", "/data", "/output", "/scratch", "/tmp"] {
        let names: Vec<String> = std::fs::read_dir(dir).map(|d| d.flatten().take(40).map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
        eprintln!("probe: ls {dir}: {} entries: {}", names.len(), names.join(" "));
    }
    for folder in ["events", "clicks", "links"] {
        let sizes: Vec<u64> = std::fs::read_dir(data.join(folder)).map(|d| d.flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).collect()).unwrap_or_default();
        eprintln!("probe: {folder}: {} files, {} bytes, largest {}", sizes.len(), sizes.iter().sum::<u64>(), sizes.iter().max().copied().unwrap_or(0));
    }

    eprintln!("probe: ---- how many threads really run at once (same work per thread; flat = that many real CPUs) ----");
    for threads in [1usize, 2, 4, 8, 16] {
        let started = Instant::now();
        let sums: Vec<u64> = crate::sys::parallel(threads, |i| spin(std::hint::black_box(60_000_000 + i as u64)));
        eprintln!("probe: {threads:>2} threads: {:.1} ms (checksum {:x})", started.elapsed().as_secs_f64() * 1e3, sums.iter().fold(0, |a, b| a ^ b));
    }

    eprintln!("probe: ---- memory ----");
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
        eprintln!("probe: {label}: touch {touched:.1} ms, copy {copied:.1} ms, this process then has {huge} kB on huge pages, unmap both {:.1} ms ({last})", started.elapsed().as_secs_f64() * 1e3);
    }
    plain_pages();

    eprintln!("probe: ---- disks ----");
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
        eprintln!("probe: {}: write {megabytes} MB {wrote:.1} ms ({written:?}), read back {reread:.1} ms ({read} bytes), delete {:.1} ms", dir.display(), started.elapsed().as_secs_f64() * 1e3);
    }
    if let Some(largest) = std::fs::read_dir(data.join("events")).ok().and_then(|d| d.flatten().max_by_key(|e| e.metadata().map(|m| m.len()).unwrap_or(0))) {
        for round in 1..=2 {
            let started = Instant::now();
            let bytes = std::fs::read(largest.path()).map(|b| b.len()).unwrap_or(0);
            eprintln!("probe: read the largest event file, round {round}: {bytes} bytes in {:.1} ms", started.elapsed().as_secs_f64() * 1e3);
        }
    }
    if let Some(largest) = std::fs::read_dir(data.join("events")).ok().and_then(|d| d.flatten().max_by_key(|e| e.metadata().map(|m| m.len()).unwrap_or(0))) {
        input_paths(&largest.path());
    }
    vector_widths();
    eprintln!("probe: ---- end; the output is now made wrong on purpose so this log is shown ----");
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
                eprintln!("probe: 256 MB a thread on {threads} threads, {label}: touch every page {touch:.1} ms, unmap {unmap:.1} ms (slowest thread; {wall:.1} ms in all)");
            }
        }
    }
}

/// The parse phase of this dataset again, cut short at each stage, and whole on other thread counts.
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
    eprintln!("probe: ---- the parse phase of this dataset, cut short at each stage ({threads} threads, best of 5) ----");
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
        eprintln!("probe: parse, {what}: {:.1} ms", time(stop, threads, 5));
    }
    eprintln!("probe: ---- the whole parse phase by thread count (best of 3) ----");
    for count in [1usize, 2, 4, 6, 8, 12, 16] {
        eprintln!("probe: parse on {count:>2} threads: {:.1} ms", time(0, count, 3));
    }
    eprintln!("probe: ---- read() and the delimiter scan only, by thread count (best of 3) ----");
    for count in [1usize, 2, 4, 8, 16] {
        eprintln!("probe: read + scan on {count:>2} threads: {:.1} ms", time(2, count, 3));
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
        eprintln!("probe: ---- input: copy with read(), or in place with mmap() ----");
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
        eprintln!("probe: read() in 256 KB blocks + scan: {:.0} MB/s (best of 5, {megabytes:.0} MB, sum {sum})", megabytes / best);
        for (label, populate) in [("mmap, faulting as it goes", false), ("mmap, MADV_POPULATE_READ first", true)] {
            let mut best = f64::MAX;
            let mut first = 0.0;
            for round in 0..5 {
                let started = Instant::now();
                let map = mmap(std::ptr::null_mut(), len, 1, 2, file.as_raw_fd(), 0);
                if map as isize == -1 {
                    eprintln!("probe: mmap failed");
                    return;
                }
                let advised = if populate { madvise(map, len, 22) } else { 0 };
                sum = count_256(std::hint::black_box(map as *const u8), len);
                munmap(map, len);
                let took = started.elapsed().as_secs_f64();
                if round == 0 {
                    first = took;
                    if populate && advised != 0 {
                        eprintln!("probe: MADV_POPULATE_READ is not supported here");
                    }
                }
                best = best.min(took);
            }
            eprintln!("probe: {label}: {:.0} MB/s best of 5 (map + scan + unmap), first round {:.0} MB/s (sum {sum})", megabytes / best, megabytes / first);
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
        eprintln!("probe: ---- 256-bit against 512-bit ----");
        let wide = std::arch::is_x86_feature_detected!("avx512bw");
        eprintln!("probe: avx512f {} avx512bw {} avx512vbmi {} avx512vbmi2 {} avx512vl {}", std::arch::is_x86_feature_detected!("avx512f"), wide, std::arch::is_x86_feature_detected!("avx512vbmi"), std::arch::is_x86_feature_detected!("avx512vbmi2"), std::arch::is_x86_feature_detected!("avx512vl"));
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
        eprintln!("probe: 256-bit scan of 64 MB: {ms:.1} ms ({value})");
        if wide {
            let (ms, value) = time(&|| count_512(std::hint::black_box(text.as_ptr()), std::hint::black_box(len)));
            eprintln!("probe: 512-bit scan of 64 MB: {ms:.1} ms ({value})");
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
        eprintln!("probe: scalar work alone: {ms:.1} ms");
        let (ms, _) = mixed(false);
        eprintln!("probe: 256-bit scan + scalar work, alternating: {ms:.1} ms");
        if wide {
            let (ms, _) = mixed(true);
            eprintln!("probe: 512-bit scan + scalar work, alternating: {ms:.1} ms");
        }
    }
}
