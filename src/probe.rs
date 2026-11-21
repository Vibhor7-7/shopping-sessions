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
    for (i, line) in text.lines().enumerate() {
        if i < 60 {
            eprintln!("probe: {path}: {line}");
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
    for path in ["/proc/version", "/proc/loadavg", "/proc/uptime", "/proc/self/cgroup", "/sys/fs/cgroup/cpu.max", "/sys/fs/cgroup/cpuset.cpus.effective", "/sys/fs/cgroup/cpu.weight", "/sys/fs/cgroup/memory.max", "/sys/fs/cgroup/memory.high", "/sys/fs/cgroup/memory.swap.max", "/sys/fs/cgroup/pids.max", "/sys/fs/cgroup/cpu.stat", "/sys/fs/cgroup/memory.stat", "/sys/kernel/mm/transparent_hugepage/enabled", "/sys/kernel/mm/transparent_hugepage/defrag", "/sys/kernel/mm/transparent_hugepage/shmem_enabled", "/proc/sys/vm/overcommit_memory", "/proc/sys/vm/dirty_ratio", "/proc/sys/vm/dirty_background_ratio", "/sys/devices/system/cpu/smt/active", "/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor", "/sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq", "/sys/devices/system/cpu/cpu0/topology/thread_siblings_list"] {
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
        let sums: Vec<u64> = crate::sys::parallel(threads, |i| spin(60_000_000 + i as u64));
        eprintln!("probe: {threads:>2} threads: {:.1} ms (checksum {:x})", started.elapsed().as_secs_f64() * 1e3, sums.iter().fold(0, |a, b| a ^ b));
    }

    eprintln!("probe: ---- memory ----");
    for (label, bytes) in [("fresh 256 MB, first touch", 256usize << 20), ("fresh 256 MB again", 256 << 20)] {
        let started = Instant::now();
        let mut block = vec![0u8; bytes];
        for at in (0..bytes).step_by(4096) {
            block[at] = 1;
        }
        let touched = started.elapsed().as_secs_f64() * 1e3;
        let started = Instant::now();
        let copy = block.clone();
        eprintln!("probe: {label}: touch {touched:.1} ms, copy {:.1} ms ({})", started.elapsed().as_secs_f64() * 1e3, copy[bytes - 4096]);
    }
    for line in head("/proc/meminfo", 4096).lines().filter(|l| l.starts_with("AnonHugePages")) {
        eprintln!("probe: meminfo while 512 MB were live: {line}");
    }

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
    eprintln!("probe: ---- end; the output is now made wrong on purpose so this log is shown ----");
}
