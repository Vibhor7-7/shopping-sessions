//! Linked IDs: every ID that appears in `links/`, mapped to its person's name.
//!
//! The lines are parsed in parallel into edges, then joined with a lock-free
//! union-find laid out in one open-addressing table. A root always takes the
//! byte-wise smaller of two IDs as its parent's key, so when the joins are done
//! the root of every tree is the smallest ID of its person: the name rules.md
//! asks for, with no separate pass to find it.

use std::fs::File;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::key::{self, Key};
use crate::reader::{self, Input, Job, Lines, ReadBuf};
use crate::sys;

/// ID to person. IDs that no link line names are not in it: they are their own person.
pub struct Links {
    /// Slots of (ID key, person key); an ID key of 0 is an empty slot.
    table: sys::Shared<[u64; 2]>,
    mask: usize,
    shift: u32,
    pub ids: usize,
}

impl Links {
    /// Where the search for `id` starts: always the first of the four slots that
    /// share a cache line, so a lookup nearly always ends in the line it started
    /// in, the one line `prefetch` asked for.
    #[inline(always)]
    fn home(&self, id: Key) -> usize {
        home(id, self.shift)
    }

    /// Asks the CPU to fetch the slot `person(id)` will look at.
    #[inline(always)]
    pub fn prefetch(&self, id: Key) {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            use std::arch::x86_64::{_mm_prefetch, _MM_HINT_T0};
            _mm_prefetch(self.table.0.add(self.home(id)) as *const i8, _MM_HINT_T0);
        }
        #[cfg(target_arch = "aarch64")]
        unsafe {
            let slot = self.table.0.add(self.home(id));
            std::arch::asm!("prfm pldl1keep, [{slot}]", slot = in(reg) slot, options(nostack, preserves_flags, readonly));
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        let _ = id;
    }

    /// The name of the person `id` belongs to.
    #[inline(always)]
    pub fn person(&self, id: Key) -> Key {
        let mut at = self.home(id);
        loop {
            let slot = unsafe { *self.table.0.add(at) };
            if slot[0] == id {
                return slot[1];
            }
            if slot[0] == 0 {
                return id;
            }
            at = (at + 1) & self.mask;
        }
    }
}

#[inline(always)]
fn home(id: Key, shift: u32) -> usize {
    (sys::mix(id) >> shift) as usize & !3
}

struct LinkLines {
    edges: Vec<(Key, Key)>,
}

impl Lines for LinkLines {
    const COMMAS: usize = 1;

    #[inline(always)]
    unsafe fn line(&mut self, data: *const u8, start: usize, commas: *const u32, end: usize, quotes: u32) {
        let comma = *commas as usize;
        let (mut a0, mut a1, mut b0, mut b1) = (start, comma, comma + 1, end);
        let wrapped = reader::unquote(data, &mut a0, &mut a1) + reader::unquote(data, &mut b0, &mut b1);
        if quotes != 2 * wrapped || a0 == a1 || b0 == b1 {
            return; // malformed, or a side is empty
        }
        let a = key::key_of_padded(data.add(a0), a1 - a0);
        let b = key::key_of_padded(data.add(b0), b1 - b0);
        if a != b {
            self.edges.push((a, b)); // a self-link changes nothing
        }
    }
}

/// The union-find while it is being built: slot = [ID key, parent slot + 1 or 0 for a root].
struct Forest<'a> {
    slots: &'a [[AtomicU64; 2]],
    mask: usize,
    shift: u32,
}

impl Forest<'_> {
    /// The slot of `id`, claiming an empty one if it is new.
    #[inline]
    fn slot(&self, id: Key) -> usize {
        let mut at = home(id, self.shift);
        loop {
            let seen = self.slots[at][0].load(Ordering::Relaxed);
            if seen == id {
                return at;
            }
            if seen == 0 {
                match self.slots[at][0].compare_exchange(0, id, Ordering::AcqRel, Ordering::Acquire) {
                    Ok(_) => return at,
                    Err(other) if other == id => return at,
                    Err(_) => {}
                }
            }
            at = (at + 1) & self.mask;
        }
    }

    #[inline]
    fn find(&self, mut at: usize) -> usize {
        loop {
            let parent = self.slots[at][1].load(Ordering::Acquire);
            if parent == 0 {
                return at;
            }
            let up = (parent - 1) as usize;
            let grand = self.slots[up][1].load(Ordering::Acquire);
            if grand == 0 {
                return up;
            }
            // Path halving. Losing the race only means someone else shortened it.
            let _ = self.slots[at][1].compare_exchange_weak(parent, grand, Ordering::AcqRel, Ordering::Relaxed);
            at = up;
        }
    }

    fn join(&self, a: usize, b: usize) {
        loop {
            let (a, b) = (self.find(a), self.find(b));
            if a == b {
                return;
            }
            let (ka, kb) = (self.slots[a][0].load(Ordering::Relaxed), self.slots[b][0].load(Ordering::Relaxed));
            // The root with the larger ID goes under the one with the smaller.
            let (small, large) = if key::compare(ka, kb).is_lt() { (a, b) } else { (b, a) };
            if self.slots[large][1].compare_exchange(0, small as u64 + 1, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
                return;
            }
        }
    }
}

pub fn build(inputs: &[Input], threads: usize) -> Links {
    // Parse.
    let total: u64 = inputs.iter().map(|i| i.len).sum();
    let target = reader::job_bytes().unwrap_or((total / (threads as u64 * 4)).clamp(1 << 16, 1 << 22));
    let jobs: Vec<Job> = reader::jobs(inputs, target);
    let parse_threads = if total < (1 << 20) { 1 } else { threads };
    let next = AtomicUsize::new(0);
    let edge_lists: Vec<Vec<(Key, Key)>> = sys::parallel(parse_threads, |_| {
        let mut buf = ReadBuf::new();
        let mut lines = LinkLines { edges: Vec::new() };
        while let Some(job) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) {
            let file: &File = &inputs[job.file as usize].file;
            reader::walk(file, job.offset, job.length, &mut buf, &mut lines);
        }
        lines.edges
    });
    let edges: usize = edge_lists.iter().map(Vec::len).sum();

    // Every edge names at most two new IDs; keep the table under three quarters
    // full (in practice about a third, since most IDs are named by several lines).
    let capacity = (edges * 2 * 4 / 3).max(16).next_power_of_two();
    let table = sys::map::<[u64; 2]>(capacity);
    let (mask, shift) = (capacity - 1, 64 - capacity.trailing_zeros());
    if edges == 0 {
        return Links { table: sys::Shared(table), mask, shift, ids: 0 };
    }
    let shared = sys::Shared(table);
    let join_threads = if edges < 20_000 { 1 } else { threads };

    // Join. The table is all zeros: every slot empty, every node a root.
    sys::parallel(join_threads, |me| {
        let forest = Forest {
            slots: unsafe { std::slice::from_raw_parts({ shared }.0 as *const [AtomicU64; 2], capacity) },
            mask,
            shift,
        };
        for list in edge_lists.iter().skip(me).step_by(join_threads) {
            for &(a, b) in list {
                let (a, b) = (forest.slot(a), forest.slot(b));
                forest.join(a, b);
            }
        }
    });

    // Point every ID straight at its root...
    let ranges = |me: usize| (capacity * me / join_threads, capacity * (me + 1) / join_threads);
    sys::parallel(join_threads, |me| {
        let forest = Forest {
            slots: unsafe { std::slice::from_raw_parts({ shared }.0 as *const [AtomicU64; 2], capacity) },
            mask,
            shift,
        };
        let (from, to) = ranges(me);
        for at in from..to {
            if forest.slots[at][0].load(Ordering::Relaxed) != 0 {
                let root = forest.find(at);
                if root != at {
                    forest.slots[at][1].store(root as u64 + 1, Ordering::Release);
                }
            }
        }
    });
    // ...then replace the pointer with the root's ID, which is the person's name.
    // Each thread writes only the second word of its own slots and reads only
    // first words, which no longer change.
    let counts = sys::parallel(join_threads, |me| {
        let slots = unsafe { std::slice::from_raw_parts({ shared }.0 as *const [AtomicU64; 2], capacity) };
        let (from, to) = ranges(me);
        let mut ids = 0usize;
        for at in from..to {
            let id = slots[at][0].load(Ordering::Relaxed);
            if id != 0 {
                ids += 1;
                let parent = slots[at][1].load(Ordering::Relaxed);
                let name = if parent == 0 { id } else { slots[(parent - 1) as usize][0].load(Ordering::Relaxed) };
                slots[at][1].store(name, Ordering::Relaxed);
            }
        }
        ids
    });
    Links { table: shared, mask, shift, ids: counts.iter().sum() }
}
