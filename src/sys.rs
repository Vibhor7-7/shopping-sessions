//! The few things std does not give us: large lazily-committed allocations and
//! huge-page advice. Declared by hand so the crate needs nothing from crates.io.

use std::ffi::c_void;

extern "C" {
    fn mmap(addr: *mut c_void, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut c_void;
    #[cfg(target_os = "linux")]
    fn madvise(addr: *mut c_void, len: usize, advice: i32) -> i32;
}

const PROT_READ_WRITE: i32 = 1 | 2;
const MAP_PRIVATE: i32 = 2;
#[cfg(target_os = "linux")]
const MAP_ANONYMOUS: i32 = 0x20;
#[cfg(not(target_os = "linux"))]
const MAP_ANONYMOUS: i32 = 0x1000;
#[cfg(target_os = "linux")]
const MAP_NORESERVE: i32 = 0x4000;
#[cfg(not(target_os = "linux"))]
const MAP_NORESERVE: i32 = 0;
#[cfg(target_os = "linux")]
const MADV_HUGEPAGE: i32 = 14;

const HUGE: usize = 2 << 20;

extern "C" {
    #[cfg(target_os = "linux")]
    fn umask(mask: u32) -> u32;
    #[cfg(not(target_os = "linux"))]
    fn umask(mask: u16) -> u16;
}

/// Lets everything this process creates be read, written and removed by anyone.
/// The process runs as uid 1000 and the scorer that collects and clears the
/// output is someone else; nothing it has to do should depend on a chmod.
pub fn open_permissions() {
    unsafe {
        umask(0);
    }
}

/// `count` zeroed `T`s, never freed. The memory is committed page by page as it
/// is first written, so reserving far more than is used costs nothing. Large
/// blocks are aligned to 2 MB and advised for transparent huge pages, which
/// cuts both the page faults while filling them and the teardown at exit.
pub fn map<T>(count: usize) -> *mut T {
    let bytes = (count.max(1) * std::mem::size_of::<T>() + 4095) & !4095;
    let big = bytes >= HUGE;
    let len = if big { bytes + HUGE } else { bytes };
    let raw = unsafe {
        mmap(std::ptr::null_mut(), len, PROT_READ_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0)
    };
    if raw as isize == -1 || raw.is_null() {
        eprintln!("pipeline: mmap of {len} bytes failed");
        std::process::abort();
    }
    if !big {
        return raw as *mut T;
    }
    let aligned = (raw as usize + HUGE - 1) & !(HUGE - 1);
    #[cfg(target_os = "linux")]
    unsafe {
        madvise(aligned as *mut c_void, bytes, MADV_HUGEPAGE);
    }
    aligned as *mut T
}

extern "C" {
    #[cfg(not(target_os = "linux"))]
    fn madvise(addr: *mut c_void, len: usize, advice: i32) -> i32;
}

/// A whole file mapped read-only, or None if it cannot be (the reader then falls back to `read`).
pub fn map_file(file: &std::fs::File, len: u64) -> Option<*const u8> {
    use std::os::fd::AsRawFd;
    const PROT_READ: i32 = 1;
    let at = unsafe { mmap(std::ptr::null_mut(), len as usize, PROT_READ, MAP_PRIVATE, file.as_raw_fd(), 0) };
    if at as isize == -1 || at.is_null() {
        None
    } else {
        Some(at as *const u8)
    }
}

/// Asks the kernel to put the whole pages of `at..at + len` of a file mapping
/// into the page table in one go (and read them in, if they are not cached),
/// rather than taking a fault every sixteen pages.
pub fn populate(at: *const u8, len: usize) {
    #[cfg(target_os = "linux")]
    advise_pages(at, len, 22); // MADV_POPULATE_READ, Linux 5.14; an older kernel just declines
    #[cfg(not(target_os = "linux"))]
    advise_pages(at, len, 3); // MADV_WILLNEED
}

/// Drops the page-table entries of the whole pages inside `at..at + len`, so
/// they are not all left for the kernel to take apart, one thread, at exit.
/// Touching the range again simply maps it again.
pub fn release(at: *const u8, len: usize) {
    advise_pages(at, len, 4); // MADV_DONTNEED
}

fn advise_pages(at: *const u8, len: usize, advice: i32) {
    let from = (at as usize + 4095) & !4095;
    let to = (at as usize + len) & !4095;
    if to > from {
        unsafe {
            madvise(from as *mut c_void, to - from, advice);
        }
    }
}

/// A raw pointer that may cross threads. Every use is a phase in which each
/// thread touches its own records, with a join between phases.
#[derive(Clone, Copy)]
pub struct Shared<T>(pub *mut T);
unsafe impl<T> Send for Shared<T> {}
unsafe impl<T> Sync for Shared<T> {}

/// Runs `work(i)` on `count` threads and returns the results in thread order.
pub fn parallel<R: Send>(count: usize, work: impl Fn(usize) -> R + Sync) -> Vec<R> {
    if count <= 1 {
        return vec![work(0)];
    }
    std::thread::scope(|scope| {
        let work = &work;
        let handles: Vec<_> = (0..count).map(|i| scope.spawn(move || work(i))).collect();
        handles.into_iter().map(|h| h.join().expect("worker thread")).collect()
    })
}

/// Asks the CPU to start fetching the cache line at `at`. Never faults.
#[inline(always)]
pub fn prefetch<T>(at: *const T) {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        std::arch::x86_64::_mm_prefetch(at as *const i8, std::arch::x86_64::_MM_HINT_T0);
    }
    #[cfg(target_arch = "aarch64")]
    unsafe {
        std::arch::asm!("prfm pldl1keep, [{at}]", at = in(reg) at, options(nostack, preserves_flags, readonly));
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    let _ = at;
}

/// One multiply, all 64 bits mixed: the two halves of a 128-bit product folded together.
#[inline(always)]
pub fn mix(key: u64) -> u64 {
    let product = u128::from(key ^ 0x2d35_8dcc_aa6c_78a5) * 0x8bb8_4b93_962e_acc9_u128;
    (product as u64) ^ ((product >> 64) as u64)
}
