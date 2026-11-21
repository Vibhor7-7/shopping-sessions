//! Partitioned record streams.
//!
//! While parsing, each thread appends records to one of P partitions. A
//! partition is a list of fixed-size chunks carved out of 4 MB slabs; a record
//! never moves once written, and every record has a global 32-bit index, so a
//! later phase can point at one (the dedup phase does, to drop the losing
//! copies of an event).

use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use crate::sys;

const SLAB_BITS: u32 = 18;
const SLAB_RECORDS: usize = 1 << SLAB_BITS;
const MAX_SLABS: usize = (1 << (32 - SLAB_BITS)) - 1;

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

struct Tail<T> {
    cur: *mut T,
    end: *mut T,
    /// Global index one past the chunk's last slot.
    end_index: u32,
}

/// One thread's write side: a tail per partition.
pub struct Scatter<'p, T> {
    pool: &'p Pool<T>,
    tails: Vec<Tail<T>>,
    /// Finished chunks per partition: (global index of the first record, records).
    chunks: Vec<Vec<(u32, u32)>>,
    chunk: u32,
    slab: *mut T,
    slab_index: u32,
    slab_left: u32,
}

/// What a thread leaves behind: per partition, its chunks.
pub type Chunks = Vec<Vec<(u32, u32)>>;

impl<'p, T: Copy> Scatter<'p, T> {
    /// `chunk` records per chunk, a power of two no larger than a slab.
    pub fn new(pool: &'p Pool<T>, partitions: usize, chunk: usize) -> Self {
        assert!(chunk.is_power_of_two() && chunk <= SLAB_RECORDS);
        Self {
            pool,
            tails: (0..partitions)
                .map(|_| Tail { cur: std::ptr::null_mut(), end: std::ptr::null_mut(), end_index: 0 })
                .collect(),
            chunks: vec![Vec::new(); partitions],
            chunk: chunk as u32,
            slab: std::ptr::null_mut(),
            slab_index: 0,
            slab_left: 0,
        }
    }

    /// Appends `record` to `partition` and returns its global index.
    #[inline(always)]
    pub fn push(&mut self, partition: usize, record: T) -> u32 {
        unsafe {
            if (*self.tails.as_ptr().add(partition)).cur == (*self.tails.as_ptr().add(partition)).end {
                self.refill(partition);
            }
            let tail = &mut *self.tails.as_mut_ptr().add(partition);
            tail.cur.write(record);
            let index = tail.end_index - tail.end.offset_from(tail.cur) as u32;
            tail.cur = tail.cur.add(1);
            index
        }
    }

    #[cold]
    #[inline(never)]
    fn refill(&mut self, partition: usize) {
        if !self.tails[partition].cur.is_null() {
            self.chunks[partition].push((self.tails[partition].end_index - self.chunk, self.chunk));
        }
        if self.slab_left == 0 {
            let (index, slab) = self.pool.grab();
            self.slab = slab;
            self.slab_index = index;
            self.slab_left = SLAB_RECORDS as u32;
        }
        let tail = &mut self.tails[partition];
        tail.cur = self.slab;
        unsafe {
            tail.end = self.slab.add(self.chunk as usize);
            self.slab = tail.end;
        }
        self.slab_index += self.chunk;
        tail.end_index = self.slab_index;
        self.slab_left -= self.chunk;
    }

    pub fn finish(mut self) -> Chunks {
        for (tail, chunks) in self.tails.iter().zip(&mut self.chunks) {
            if !tail.cur.is_null() {
                let free = unsafe { tail.end.offset_from(tail.cur) } as u32;
                if free < self.chunk {
                    chunks.push((tail.end_index - self.chunk, self.chunk - free));
                }
            }
        }
        self.chunks
    }
}
