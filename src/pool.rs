//! Partitioned record streams.
//!
//! While parsing, each thread appends records to one of P partitions. A
//! partition is a list of fixed-size chunks carved out of slabs; a record never
//! moves once it is in its chunk, and every record has a global 32-bit index,
//! so a later phase can point at one (the dedup phase does, to drop the losing
//! copies of an event).

use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use crate::sys;

const SLAB_BITS: u32 = 18;
const SLAB_RECORDS: usize = 1 << SLAB_BITS;
const MAX_SLABS: usize = (1 << (32 - SLAB_BITS)) - 1;

/// Whether `Combine::prefetch` asks for the partition's state as well as its
/// buffer. The states of a stream are 4 KB at 2^10 partitions.
const STATE_AHEAD: bool = false;

/// Slabs of `T`, shared by all threads.
pub struct Pool<T> {
    slabs: Box<[AtomicPtr<T>]>,
    next: AtomicUsize,
}

impl<T> Pool<T> {
    pub fn new() -> Self {
        Self { slabs: (0..MAX_SLABS).map(|_| AtomicPtr::new(std::ptr::null_mut())).collect(), next: AtomicUsize::new(0) }
    }

    /// A fresh slab: its first record's global index, and where it is.
    fn grab(&self) -> (u32, *mut T) {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        if id >= MAX_SLABS {
            eprintln!("pipeline: more than 2^32 records in one stream");
            std::process::abort();
        }
        let slab = sys::map::<T>(SLAB_RECORDS);
        self.slabs[id].store(slab, Ordering::Release);
        ((id << SLAB_BITS) as u32, slab)
    }

    /// The record with global index `at`.
    #[inline(always)]
    pub fn at(&self, at: u32) -> *mut T {
        unsafe {
            let slab = self.slabs.get_unchecked((at >> SLAB_BITS) as usize).load(Ordering::Relaxed);
            slab.add(at as usize & (SLAB_RECORDS - 1))
        }
    }

    pub fn records(&self) -> usize {
        self.next.load(Ordering::Relaxed) * SLAB_RECORDS
    }
}

/// What a thread leaves behind: per partition, its chunks as (global index of the first record, records).
pub type Chunks = Vec<Vec<(u32, u32)>>;

/// The memory one thread's `Combine`s keep their buffers and partition states
/// in: a single mapping that asks for a huge page.
///
/// A thread pushes to a couple of thousand partitions in no order, and every
/// push reads one state and writes into one buffer. Mapped on their own, the
/// buffers and states of the three streams were about 125 pages of 4 KB at
/// 2^10 partitions (the states were 16 bytes a partition then), and a
/// first-level TLB has 64 to 96 entries for the two threads of a core
/// together. On one huge page they are a single entry.
/// Measured (BENCH.md, experiment 32): parse 1 to 2% faster with two threads
/// to a core, no different with one. That the TLB is why is a reading of
/// those two numbers, not something counted.
pub struct Arena {
    next: usize,
    end: usize,
}

impl Arena {
    /// Room for `bytes`, in whole huge pages. Zeroed, never freed.
    pub fn new(bytes: usize) -> Self {
        let len = bytes.max(1).next_multiple_of(sys::HUGE);
        let start = sys::map::<u8>(len) as usize;
        Self { next: start, end: start + len }
    }

    /// `count` zeroed `T`s, the first on a cache line.
    fn take<T>(&mut self, count: usize) -> *mut T {
        let start = (self.next + 63) & !63;
        let bytes = count * std::mem::size_of::<T>();
        assert!(std::mem::align_of::<T>() <= 64 && start + bytes <= self.end, "arena too small");
        self.next = start + bytes;
        start as *mut T
    }
}

/// One thread's write side: for each partition, a write-combining buffer of `N` records.
///
/// A thread appends to a couple of thousand partitions in turn, each into
/// memory nothing has touched yet. Written directly, every new cache line of
/// every partition is first fetched from memory (only to be overwritten), and
/// with that many streams no prefetcher sees them coming. Here `N` records, a
/// whole number of cache lines, collect in a small buffer that stays in cache
/// and then go out in non-temporal stores, which fetch nothing.
///
/// All a partition keeps between two pushes is the global index of its next
/// record, four bytes. A chunk starts at a multiple of its size and a buffer
/// at a multiple of `N`, so that index says the rest: its low bits are the
/// slot in the buffer, the pool turns it into the place a full buffer goes
/// to, and a chunk is full when the index reaches a multiple of the chunk
/// size. For that to hold a partition always has a chunk: its first when the
/// `Combine` is made, the next as soon as one fills. (Until experiment 39 a
/// state was 16 bytes, with the buffer's place and the chunk's end, compared
/// on every push: BENCH.md.)
pub struct Combine<'p, T, const N: usize> {
    pool: &'p Pool<T>,
    /// `N` buffered records per partition, each buffer starting on a cache line.
    buffers: *mut [T; N],
    /// Per partition, the global index of the next record pushed to it. In the thread's `Arena` like the buffers.
    next: *mut u32,
    partitions: usize,
    chunks: Vec<Vec<(u32, u32)>>,
    chunk: u32,
    /// What is left of the slab chunks are carved from: the index of its next record, and how many there are.
    slab_index: u32,
    slab_left: u32,
}

/// Copies `lines` 64-byte lines (both ends 64-byte aligned) without reading the destination into cache first.
#[inline(always)]
unsafe fn stream_lines(from: *const u8, to: *mut u8, lines: usize) {
    #[cfg(all(target_arch = "x86_64", target_feature = "avx"))]
    {
        use std::arch::x86_64::*;
        for half in 0..2 * lines {
            _mm256_stream_si256(to.add(32 * half) as *mut __m256i, _mm256_load_si256(from.add(32 * half) as *const __m256i));
        }
    }
    #[cfg(all(target_arch = "x86_64", not(target_feature = "avx")))]
    {
        use std::arch::x86_64::*;
        for part in 0..4 * lines {
            _mm_stream_si128(to.add(16 * part) as *mut __m128i, _mm_load_si128(from.add(16 * part) as *const __m128i));
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    std::ptr::copy_nonoverlapping(from, to, 64 * lines);
}

impl<'p, T: Copy, const N: usize> Combine<'p, T, N> {
    /// Cache lines in a full buffer.
    const LINES: usize = N * std::mem::size_of::<T>() / 64;

    /// What `new` takes from the arena for this many partitions.
    pub const fn arena_bytes(partitions: usize) -> usize {
        partitions * (N * std::mem::size_of::<T>() + std::mem::size_of::<u32>()) + 2 * 64
    }

    /// `chunk` records per chunk: a power of two, at least `N`, no larger than a slab.
    /// The buffers and states come out of `arena`, which must have `arena_bytes(partitions)` left.
    pub fn new(pool: &'p Pool<T>, arena: &mut Arena, partitions: usize, chunk: usize) -> Self {
        assert!(N.is_power_of_two() && (N * std::mem::size_of::<T>()) % 64 == 0 && std::mem::align_of::<T>() <= 64);
        assert!(chunk.is_power_of_two() && chunk >= N && chunk <= SLAB_RECORDS);
        let mut combine = Self {
            pool,
            // Every buffer starts on a cache line: the first does, and each is a whole number of lines.
            buffers: arena.take::<[T; N]>(partitions),
            next: arena.take::<u32>(partitions),
            partitions,
            chunks: vec![Vec::new(); partitions],
            chunk: chunk as u32,
            slab_index: 0,
            slab_left: 0,
        };
        // Every partition's first chunk. Nothing of a chunk is touched before a
        // buffer is written to it, so one that stays empty costs address space only.
        for partition in 0..partitions {
            let first = combine.carve();
            unsafe { combine.next.add(partition).write(first) };
        }
        combine
    }

    /// Asks the CPU for what the next `push` to `partition` will write into: its
    /// buffer. A thread writes to its partitions in no order, and the buffers
    /// of a thousand partitions are far more than the first cache level holds,
    /// so a push whose partition was not announced a few lines earlier starts
    /// with a wait.
    #[inline(always)]
    pub fn prefetch(&self, partition: usize) {
        unsafe {
            if STATE_AHEAD {
                sys::prefetch(self.next.add(partition));
            }
            let buffer = self.buffers.add(partition) as *const u8;
            for line in 0..Self::LINES {
                sys::prefetch(buffer.add(64 * line));
            }
        }
    }

    /// Appends `record` to `partition` and returns its global index. The record
    /// is only in its final place once its buffer has filled, or after `finish`.
    #[inline(always)]
    pub fn push(&mut self, partition: usize, record: T) -> u32 {
        unsafe {
            let next = self.next.add(partition);
            let index = *next;
            let after = index + 1;
            let buffer = self.buffers.add(partition) as *mut T;
            buffer.add(index as usize & (N - 1)).write(record);
            *next = after;
            if after as usize & (N - 1) == 0 {
                // The buffer is full. Its first record's index says where in the chunk it goes.
                stream_lines(buffer as *const u8, self.pool.at(after - N as u32) as *mut u8, Self::LINES);
                if after & (self.chunk - 1) == 0 {
                    self.refill(partition);
                }
            }
            index
        }
    }

    /// The next chunk of the thread's slab: the global index of its first record.
    fn carve(&mut self) -> u32 {
        if self.slab_left == 0 {
            self.slab_index = self.pool.grab().0;
            self.slab_left = SLAB_RECORDS as u32;
        }
        let first = self.slab_index;
        self.slab_index += self.chunk;
        self.slab_left -= self.chunk;
        first
    }

    /// `partition`'s chunk has just been filled, and its last buffer written to it: a new one.
    #[cold]
    #[inline(never)]
    fn refill(&mut self, partition: usize) {
        assert!(partition < self.partitions);
        let end = unsafe { self.next.add(partition).read() };
        self.chunks[partition].push((end - self.chunk, self.chunk));
        let first = self.carve();
        unsafe { self.next.add(partition).write(first) };
    }

    /// Puts the records still in buffers in their places and returns the chunks.
    pub fn finish(mut self) -> Chunks {
        for partition in 0..self.partitions {
            let index = unsafe { self.next.add(partition).read() };
            // A chunk that is full was listed when it filled, and the index is in the next one already.
            let used = index & (self.chunk - 1);
            let buffered = index & (N as u32 - 1);
            if buffered > 0 {
                unsafe {
                    std::ptr::copy_nonoverlapping(self.buffers.add(partition) as *const T, self.pool.at(index - buffered), buffered as usize)
                };
            }
            if used > 0 {
                self.chunks[partition].push((index - used, used));
            }
        }
        // Non-temporal stores are only ordered with everything else once fenced.
        #[cfg(target_arch = "x86_64")]
        unsafe {
            std::arch::x86_64::_mm_sfence();
        }
        self.chunks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn next(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    /// Two writers on one pool, taking turns. Whatever the partition count, the
    /// chunk size and the number of pushes: `push` returns the index its record
    /// is found at afterwards, a partition's chunks hold exactly its records in
    /// the order they were pushed, and no chunk but a partition's last is part full.
    fn check<const WORDS: usize>(partitions: usize, chunk: usize, pushes: usize, skew: bool) {
        let what = format!("{WORDS} words, {partitions} partitions, chunks of {chunk}, {pushes} pushes, skew {skew}");
        let pool = Pool::<[u64; WORDS]>::new();
        let mut arena = Arena::new(2 * Combine::<[u64; WORDS], 8>::arena_bytes(partitions));
        let mut writers = [
            Combine::<[u64; WORDS], 8>::new(&pool, &mut arena, partitions, chunk),
            Combine::<[u64; WORDS], 8>::new(&pool, &mut arena, partitions, chunk),
        ];
        let mut expected = vec![vec![Vec::new(); partitions]; 2];
        let mut pushed = Vec::with_capacity(pushes);
        let mut state = 0x9e37_79b9_7f4a_7c15 ^ (partitions * 31 + chunk * 7 + pushes) as u64;
        for number in 0..pushes {
            let writer = (next(&mut state) % 3 == 0) as usize;
            let mut partition = next(&mut state) as usize % partitions;
            if skew && next(&mut state) % 4 != 0 {
                partition %= 2; // most of it to two partitions, so that they go through many chunks
            }
            let mut record = [0u64; WORDS];
            record[0] = number as u64 + 1;
            record[WORDS - 1] ^= (partition as u64) << 40 | (writer as u64) << 60;
            writers[writer].prefetch(partition);
            let index = writers[writer].push(partition, record);
            expected[writer][partition].push(record);
            pushed.push((index, record));
        }
        let [first, second] = writers;
        let lists = [first.finish(), second.finish()];
        for (index, record) in &pushed {
            assert_eq!(unsafe { *pool.at(*index) }, *record, "{what}: the record at index {index}");
        }
        let mut seen = std::collections::HashSet::new();
        for (writer, chunks) in lists.iter().enumerate() {
            assert_eq!(chunks.len(), partitions, "{what}");
            for (partition, list) in chunks.iter().enumerate() {
                let mut found = Vec::new();
                for (at, &(start, count)) in list.iter().enumerate() {
                    assert!(count > 0 && count as usize <= chunk, "{what}: a chunk of {count}");
                    assert!(count as usize == chunk || at + 1 == list.len(), "{what}: a part-full chunk that is not the last");
                    assert_eq!(start as usize % chunk, 0, "{what}: a chunk starting at {start}");
                    for index in start..start + count {
                        assert!(seen.insert(index), "{what}: index {index} in two chunks");
                        found.push(unsafe { *pool.at(index) });
                    }
                }
                assert_eq!(found, expected[writer][partition], "{what}: writer {writer}, partition {partition}");
            }
        }
        assert_eq!(seen.len(), pushes, "{what}");
    }

    #[test]
    fn every_record_is_in_its_partition_in_order_at_its_index() {
        // Around a buffer of 8 and around a chunk: nothing, one short, exactly full, one over.
        for pushes in [0, 1, 7, 8, 9, 15, 16, 17, 63, 64, 65, 127, 128, 129, 1000] {
            check::<2>(1, 8, pushes, false);
            check::<2>(1, 64, pushes, false);
            check::<3>(1, 16, pushes, false);
            check::<2>(3, 8, pushes, true);
        }
        check::<2>(16, 64, 100_000, false);
        check::<3>(16, 64, 100_000, false);
        check::<2>(64, 16, 300_000, true);
        check::<3>(1024, 256, 300_000, true);
        check::<2>(1024, 1024, 200_000, false);
        check::<2>(4096, 4096, 50_000, false);
        // More than a slab through one partition, and chunks as large as a slab.
        check::<2>(2, 4096, 3 * SLAB_RECORDS + 5, true);
        check::<2>(2, SLAB_RECORDS, 2 * SLAB_RECORDS + 9, true);
    }
}
