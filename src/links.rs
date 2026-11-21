//! Linked IDs: every ID that appears in `links/`, mapped to its person's name.
//!
//! The lines are parsed in parallel into edges, then joined with a lock-free
//! union-find laid out in one open-addressing table. A root always takes the
//! byte-wise smaller of two IDs as its parent's key, so when the joins are done
//! the root of every tree is the smallest ID of its person: the name rules.md
//! asks for, with no separate pass to find it. One pass over the table then
//! puts that name beside every ID (`name_all`).

use std::fs::File;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::key::{self, Key};
use crate::reader::{self, Input, Job, Lines, ReadBuf};
use crate::sys;

/// ID to person. IDs that no link line names are not in it: they are their own person.
pub struct Links {
    /// Cache lines of four slots: four ID keys, then the four people they belong to.
    /// An ID key of 0 is an empty slot.
    table: sys::Shared<u64>,
    mask: usize,
    shift: u32,
    pub ids: usize,
    /// Milliseconds spent parsing, joining and naming, for the log.
    pub timing: [f64; 3],
}

impl Links {
    /// Where the search for `id` starts: always the first of the four slots that
    /// share a cache line, so a lookup nearly always ends in the line it started
    /// in, the one line `prefetch` asked for.
    #[inline(always)]
    fn home(&self, id: Key) -> usize {
        home(id, self.shift)
    }

    /// Where the lookup of `id` starts, and a request to the CPU to fetch that
    /// line now: the lookup itself comes some lines later (`person_at`).
    #[inline(always)]
    pub fn prefetch(&self, id: Key) -> u32 {
        let home = self.home(id);
        sys::prefetch(unsafe { self.table.0.add(id_word(home)) });
        home as u32
    }

    /// The name of the person `id` belongs to; `home` is what `prefetch(id)` returned.
    ///
    /// Whether an ID is linked is a coin toss from line to line, so the four
    /// slots of its cache line are compared at once and the answer selected,
    /// rather than branched on. Only an ID pushed out of its line (rare)
    /// takes the loop.
    #[inline(always)]
    pub fn person_at(&self, id: Key, home: u32) -> Key {
        unsafe {
            let line = self.table.0.add(id_word(home as usize)) as *const u64; // four IDs, then their four people
            let (found, empty) = line_masks(line, id);
            let stop = found | empty;
            if stop != 0 {
                let slot = stop.trailing_zeros() as usize; // the first slot that is `id` or free
                let person = *line.add(4 + slot);
                let is_found = (found >> slot) & 1 != 0;
                return std::hint::select_unpredictable(is_found, person, id);
            }
            self.person_from(id, (home as usize + 4) & self.mask)
        }
    }

    #[cold]
    #[inline(never)]
    fn person_from(&self, id: Key, mut at: usize) -> Key {
        loop {
            let held = unsafe { *self.table.0.add(id_word(at)) };
            if held == id {
                return unsafe { *self.table.0.add(id_word(at) + 4) };
            }
            if held == 0 {
                return id;
            }
            at = (at + 1) & self.mask;
        }
    }
}

/// Where slot `at`'s ID is in the table; the person it belongs to (or, while the
/// table is built, its parent) is four words on. Slots 4k to 4k+3 share a cache line.
#[inline(always)]
fn id_word(at: usize) -> usize {
    (at >> 2) * 8 + (at & 3)
}

/// For the four IDs at `line`: which are `id`, and which slots are free, one bit a slot.
#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
#[inline(always)]
unsafe fn line_masks(line: *const u64, id: Key) -> (u32, u32) {
    use std::arch::x86_64::*;
    let ids = _mm256_loadu_si256(line as *const __m256i);
    let bits = |v: __m256i| _mm256_movemask_pd(_mm256_castsi256_pd(v)) as u32;
    (bits(_mm256_cmpeq_epi64(ids, _mm256_set1_epi64x(id as i64))), bits(_mm256_cmpeq_epi64(ids, _mm256_setzero_si256())))
}

#[cfg(not(all(target_arch = "x86_64", target_feature = "avx2")))]
#[inline(always)]
unsafe fn line_masks(line: *const u64, id: Key) -> (u32, u32) {
    let (mut found, mut empty) = (0u32, 0u32);
    for slot in 0..4 {
        let held = *line.add(slot);
        found |= u32::from(held == id) << slot;
        empty |= u32::from(held == 0) << slot;
    }
    (found, empty)
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
    unsafe fn line(&mut self, data: *const u8, delims: *const u32, quotes: u32) -> bool {
        if !reader::is_line::<2>(data, delims) {
            return false; // not two fields
        }
        let comma = *delims.add(1) as usize;
        let (a0, a1, w0) = reader::unquoted(data, (*delims).wrapping_add(1) as usize, comma);
        let (b0, b1, w1) = reader::unquoted(data, comma + 1, *delims.add(2) as usize);
        if quotes != 2 * (w0 + w1) || a0 == a1 || b0 == b1 {
            return true; // malformed, or a side is empty
        }
        let (a, b) = key::keys_of_two_padded(data.add(a0), a1 - a0, data.add(b0), b1 - b0);
        if a != b {
            self.edges.push((a, b)); // a self-link changes nothing
        }
        true
    }
}

/// What the four low bits of a pointer to a parent slot hold. No key has it
/// there (a packed key ends in 0000, an interned one in 0001), so a word that
/// holds a pointer can be told from one that already holds a name.
const POINTER: u64 = 2;

#[inline(always)]
fn pointer_to(slot: usize) -> u64 {
    ((slot as u64) << 4) | POINTER
}

#[inline(always)]
fn is_pointer(word: u64) -> bool {
    word & 15 == POINTER
}

/// The most edges between giving an edge's IDs their slots and joining them.
const RING: usize = 64;

/// How many edges lie between an edge's steps in `Forest::join_all`: from
/// claiming its slots to joining them (a power of two, at most `RING`), and
/// from asking for its home lines to claiming. `SS_JOIN_NEAR` and `SS_JOIN_FAR`
/// are measuring knobs.
fn join_distances() -> (usize, usize) {
    let setting = |name: &str, default: usize| std::env::var(name).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(default);
    (setting("SS_JOIN_NEAR", 8).clamp(1, RING).next_power_of_two(), setting("SS_JOIN_FAR", 8).min(1 << 16))
}

/// The slot a pointer points to.
#[inline(always)]
fn pointed(word: u64) -> usize {
    (word >> 4) as usize
}

/// The union-find while it is being built: each slot an ID key and, in the word
/// that will hold its person, a pointer to its parent slot, or 0 for a root.
struct Forest<'a> {
    words: &'a [AtomicU64],
    mask: usize,
    shift: u32,
}

impl Forest<'_> {
    #[inline(always)]
    fn id(&self, at: usize) -> &AtomicU64 {
        unsafe { self.words.get_unchecked(id_word(at)) }
    }

    #[inline(always)]
    fn parent(&self, at: usize) -> &AtomicU64 {
        unsafe { self.words.get_unchecked(id_word(at) + 4) }
    }

    /// The slot of `id`, claiming an empty one if it is new.
    ///
    /// How far down its home line an ID sits is as good as random, so the four
    /// slots of the line are compared at once and the first that holds the ID
    /// or is free is the one, with no loop over them. Whether the ID is new
    /// (two times in three) is as random, and is branched on all the same: a
    /// slot that already holds the ID is returned as it is, and only a free one
    /// is claimed. Claiming either way, which fails harmlessly on a slot that
    /// holds the ID, needs no branch but a locked instruction for nothing one
    /// time in three, and it writes to a line other threads may be reading:
    /// measured, it loses on Intel's CPUs what it gains on one of AMD's
    /// (BENCH.md, experiment 34). Only a full line, or a free slot another
    /// thread took for another ID in between, goes on to the search slot by slot.
    #[inline(always)]
    fn slot(&self, id: Key) -> usize {
        let home = home(id, self.shift);
        let mut stop = 0u32;
        for slot in 0..4 {
            let held = self.id(home + slot).load(Ordering::Relaxed);
            stop |= u32::from((held == id) | (held == 0)) << slot;
        }
        if stop != 0 {
            let at = home + stop.trailing_zeros() as usize;
            if self.id(at).load(Ordering::Relaxed) == id {
                return at;
            }
            match self.id(at).compare_exchange(0, id, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return at,
                Err(held) if held == id => return at,
                Err(_) => {}
            }
        }
        self.slot_from(id, home)
    }

    /// The same, searching slot by slot from `at`: no slot before `at` in the
    /// ID's search order holds it or is free.
    #[cold]
    #[inline(never)]
    fn slot_from(&self, id: Key, mut at: usize) -> usize {
        loop {
            let seen = self.id(at).load(Ordering::Relaxed);
            if seen == id {
                return at;
            }
            if seen == 0 {
                match self.id(at).compare_exchange(0, id, Ordering::AcqRel, Ordering::Acquire) {
                    Ok(_) => return at,
                    Err(other) if other == id => return at,
                    Err(_) => {}
                }
            }
            at = (at + 1) & self.mask;
        }
    }

    /// The root of `at`'s tree. Four times in five `at` is the root itself and
    /// nearly always otherwise its parent is; which, is as good as random, so
    /// the slot to look at is selected and one predictable branch asks whether
    /// it is a root. A longer path takes `find`.
    #[inline(always)]
    fn root(&self, at: usize) -> usize {
        let parent = self.parent(at).load(Ordering::Acquire);
        let up = std::hint::select_unpredictable(parent == 0, at, pointed(parent));
        if self.parent(up).load(Ordering::Acquire) == 0 {
            return up;
        }
        self.find(at)
    }

    #[inline(never)]
    fn find(&self, mut at: usize) -> usize {
        loop {
            let parent = self.parent(at).load(Ordering::Acquire);
            if parent == 0 {
                return at;
            }
            let up = pointed(parent);
            let grand = self.parent(up).load(Ordering::Acquire);
            if grand == 0 {
                return up;
            }
            // Path halving. Losing the race only means someone else shortened it.
            let _ = self.parent(at).compare_exchange_weak(parent, grand, Ordering::AcqRel, Ordering::Relaxed);
            at = up;
        }
    }

    fn join(&self, a: usize, b: usize) {
        loop {
            let (a, b) = (self.root(a), self.root(b));
            if a == b {
                return;
            }
            let (ka, kb) = (self.id(a).load(Ordering::Relaxed), self.id(b).load(Ordering::Relaxed));
            // The root with the larger ID goes under the one with the smaller:
            // which that is, is a coin toss.
            let a_is_smaller = key::compare(ka, kb).is_lt();
            let small = std::hint::select_unpredictable(a_is_smaller, a, b);
            let large = std::hint::select_unpredictable(a_is_smaller, b, a);
            if self.parent(large).compare_exchange(0, pointer_to(small), Ordering::AcqRel, Ordering::Relaxed).is_ok() {
                return;
            }
        }
    }

    /// Asks for the line of the slot that `at`'s pointer leads to, the one line
    /// `find(at)` reads that nobody has asked for yet. A root has no pointer and
    /// asks for its own line, which it is in: whether a slot is a root is as good
    /// as random from edge to edge, so the address is selected, not branched on.
    #[inline(always)]
    fn ask_for_parent(&self, at: usize) {
        let word = self.parent(at).load(Ordering::Relaxed);
        sys::prefetch(self.id(std::hint::select_unpredictable(word == 0, at, pointed(word))));
    }

    /// Joins the two IDs of every edge in `edges`.
    ///
    /// The table is far larger than the cache and every step of a join lands
    /// somewhere new, so an edge goes through three steps, each `near` or `far`
    /// edges after the one before, with the lines the next step reads on their
    /// way meanwhile: the two IDs' home lines are asked for; `far` edges later
    /// the IDs are given their slots there, and the lines of those slots'
    /// parents are asked for; `near` edges later the two are joined. The joins
    /// may be made in any order (a tree's root is its smallest ID whatever the
    /// order), and a slot claimed early is a root nobody has joined yet, which
    /// is what it would be if its edge came later in the file.
    fn join_all(&self, edges: &[(Key, Key)], near: usize, far: usize) {
        debug_assert!(near.is_power_of_two() && near <= RING);
        let mut ring = [(0usize, 0usize); RING];
        for at in 0..edges.len() + near {
            if let Some(&(a, b)) = edges.get(at + far) {
                sys::prefetch(self.id(home(a, self.shift)));
                sys::prefetch(self.id(home(b, self.shift)));
            }
            let turn = at & (near - 1);
            if at >= near {
                let (a, b) = unsafe { *ring.get_unchecked(turn) }; // the edge `near` before this one
                self.join(a, b);
            }
            if let Some(&(a, b)) = edges.get(at) {
                let (a, b) = (self.slot(a), self.slot(b));
                self.ask_for_parent(a);
                self.ask_for_parent(b);
                unsafe { *ring.get_unchecked_mut(turn) = (a, b) };
            }
        }
    }

    /// Gives the ID in slot `at`, which holds a pointer, its person's name: the
    /// ID of its tree's root. Followed upwards, the pointers end at a slot whose
    /// word is 0 (the root, which its own thread has not reached yet: its ID is
    /// the name) or at one that already holds a name (the root, or an ID between
    /// that its thread has named: the same name). Which of the two is a coin
    /// toss, so the name is selected, not branched on; both are in the one cache
    /// line that was asked for when `at` was put in the queue.
    #[inline(always)]
    fn name(&self, at: usize) {
        let mut up = pointed(self.parent(at).load(Ordering::Relaxed));
        loop {
            let word = self.parent(up).load(Ordering::Relaxed);
            if is_pointer(word) {
                up = pointed(word); // an ID the joins left more than one step from its root
                continue;
            }
            let name = std::hint::select_unpredictable(word == 0, self.id(up).load(Ordering::Relaxed), word);
            self.parent(at).store(name, Ordering::Relaxed);
            return;
        }
    }

    /// Names every ID in slots `from..to`, in one pass, and counts them.
    ///
    /// Nothing here branches on what a slot holds: whether it is empty, a root
    /// or linked to another ID is as good as random from slot to slot. Every
    /// slot's word is written: a root's with its own ID, an empty slot's with
    /// the 0 it had, a pointer with itself. A slot that holds a pointer is also
    /// put in a queue and the line of the slot it points to asked for; it is
    /// named `BATCH` to `2 * BATCH` queued slots later, a batch at a time, when
    /// that line has had time to arrive and while the lines of the slots queued
    /// since are on their way. (A slot that is not queued asks for its own line.)
    ///
    /// Threads share the table and each writes only the words of its own slots.
    /// A word another thread reads is 0, a pointer or a name, before and after
    /// each of those writes, and `name` is right for all three.
    fn name_all(&self, from: usize, to: usize) -> usize {
        const BATCH: usize = 16;
        const QUEUE: usize = 4 * BATCH;
        let mut queue = [0usize; QUEUE];
        let (mut queued, mut named) = (0usize, 0usize);
        let mut ids = 0usize;
        for at in from..to {
            let id = self.id(at).load(Ordering::Relaxed);
            let word = self.parent(at).load(Ordering::Relaxed);
            let linked = is_pointer(word);
            ids += usize::from(id != 0);
            self.parent(at).store(std::hint::select_unpredictable(word == 0, id, word), Ordering::Relaxed);
            unsafe { *queue.get_unchecked_mut(queued % QUEUE) = at };
            queued += usize::from(linked);
            sys::prefetch(self.id(std::hint::select_unpredictable(linked, pointed(word), at)));
            if queued - named >= 2 * BATCH {
                for _ in 0..BATCH {
                    self.name(unsafe { *queue.get_unchecked(named % QUEUE) });
                    named += 1;
                }
            }
        }
        while named < queued {
            self.name(queue[named % QUEUE]);
            named += 1;
        }
        ids
    }
}

pub fn build(inputs: &[Input], threads: usize) -> Links {
    let started = std::time::Instant::now();
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
    let parsed = started.elapsed().as_secs_f64() * 1e3;
    let mut links = join_and_name(&edge_lists, if edges < 20_000 { 1 } else { threads }, join_distances());
    links.timing[0] = parsed;
    links
}

/// The table for these edges (pairs of different IDs), joined and named on
/// `threads` threads; `near` and `far` are the join's distances (`join_distances`).
fn join_and_name(edge_lists: &[Vec<(Key, Key)>], threads: usize, (near, far): (usize, usize)) -> Links {
    let since = |from: std::time::Instant| from.elapsed().as_secs_f64() * 1e3;
    let edges: usize = edge_lists.iter().map(Vec::len).sum();
    // Every edge names at most two new IDs; keep the table under three quarters
    // full (in practice about a third, since most IDs are named by several lines).
    let capacity = (edges * 2 * 4 / 3).max(16).next_power_of_two();
    let table = sys::map::<u64>(capacity * 2);
    let (mask, shift) = (capacity - 1, 64 - capacity.trailing_zeros());
    if edges == 0 {
        return Links { table: sys::Shared(table), mask, shift, ids: 0, timing: [0.0; 3] };
    }
    let shared = sys::Shared(table);
    let joining = std::time::Instant::now();

    // Join. The table is all zeros: every slot empty, every node a root.
    sys::parallel(threads, |me| {
        let forest = Forest {
            words: unsafe { std::slice::from_raw_parts({ shared }.0 as *const AtomicU64, capacity * 2) },
            mask,
            shift,
        };
        for list in edge_lists.iter().skip(me).step_by(threads) {
            forest.join_all(list, near, far);
        }
    });
    let joined = since(joining);
    let naming = std::time::Instant::now();

    // Replace every pointer with the ID of its tree's root, which is the person's
    // name, and give every root its own: each thread a share of the slots.
    let counts = sys::parallel(threads, |me| {
        let forest = Forest {
            words: unsafe { std::slice::from_raw_parts({ shared }.0 as *const AtomicU64, capacity * 2) },
            mask,
            shift,
        };
        forest.name_all(capacity * me / threads, capacity * (me + 1) / threads)
    });
    Links { table: shared, mask, shift, ids: counts.iter().sum(), timing: [0.0, joined, since(naming)] }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn next(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// The definition: IDs that edges connect, however far apart, are one
    /// person, named by the byte-wise smallest of them.
    fn names_by_definition(edges: &[(Key, Key)]) -> HashMap<Key, Key> {
        fn root(parent: &mut HashMap<Key, Key>, id: Key) -> Key {
            let mut top = id;
            while parent[&top] != top {
                top = parent[&top];
            }
            let mut at = id;
            while at != top {
                at = parent.insert(at, top).unwrap();
            }
            top
        }
        let mut parent: HashMap<Key, Key> = HashMap::new();
        for &(a, b) in edges {
            parent.entry(a).or_insert(a);
            parent.entry(b).or_insert(b);
            let (a, b) = (root(&mut parent, a), root(&mut parent, b));
            parent.insert(a, b);
        }
        let ids: Vec<Key> = parent.keys().copied().collect();
        let mut smallest: HashMap<Key, Key> = HashMap::new();
        for &id in &ids {
            let top = root(&mut parent, id);
            let held = smallest.entry(top).or_insert(id);
            if key::compare(id, *held).is_lt() {
                *held = id;
            }
        }
        ids.iter().map(|&id| (id, smallest[&root(&mut parent, id)])).collect()
    }

    /// An ID's key: most pack, some are interned (a space does not pack), so both kinds of key meet in one tree.
    fn id(number: u64) -> Key {
        let text = if number % 7 == 3 { format!("u {number}") } else { format!("u{number}") };
        key::key_of(text.as_bytes())
    }

    /// The join's distances a round is run under: the ones the pipeline uses,
    /// and the ends of what the knobs allow (an edge joined as soon as it has
    /// its slots, or 64 edges later; home lines asked for late, early or at once).
    fn distances(round: u64) -> (usize, usize) {
        [(8, 8), (1, 8), (64, 0), (2, 1), (16, 300), (4, 16)][(round % 6) as usize]
    }

    /// Joins `edges`, dealt out to `lists` lists, on `threads` threads, and holds every ID to the definition.
    fn check(edges: &[(Key, Key)], lists: usize, threads: usize, distances: (usize, usize), what: &str) {
        let mut edge_lists: Vec<Vec<(Key, Key)>> = vec![Vec::new(); lists];
        for (at, &edge) in edges.iter().enumerate() {
            edge_lists[at * lists / edges.len().max(1)].push(edge);
        }
        let links = join_and_name(&edge_lists, threads, distances);
        let expected = names_by_definition(edges);
        assert_eq!(links.ids, expected.len(), "{what}: how many IDs");
        for (&id, &name) in &expected {
            assert_eq!(links.person_at(id, links.prefetch(id)), name, "{what}: the person of {id:#x}");
        }
        // An ID no edge names is its own person.
        for number in 0..64 {
            let stranger = id(1 << 40 | number);
            assert_eq!(links.person_at(stranger, links.prefetch(stranger)), stranger, "{what}: a stranger");
        }
        // Nothing of the forest is left in the table: every slot is empty or holds an ID and a name.
        let words = unsafe { std::slice::from_raw_parts(links.table.0, (links.mask + 1) * 2) };
        for at in 0..=links.mask {
            let (held, name) = (words[id_word(at)], words[id_word(at) + 4]);
            assert_eq!(expected.get(&held).copied().unwrap_or(0), name, "{what}: slot {at}");
        }
    }

    #[test]
    fn every_id_gets_the_smallest_id_of_its_person() {
        let mut state = 33;
        // Trees of every shape the joins can leave, small enough to run thousands.
        for round in 0..1_500u64 {
            let count = 1 + (next(&mut state) % 300) as usize;
            let people = 1 + next(&mut state) % (2 * count as u64 + 1);
            let mut edges: Vec<(Key, Key)> = Vec::new();
            let base = round << 20;
            match round % 5 {
                // Any two IDs of a small or a large pool: from all singletons to one person.
                0 | 1 => {
                    for _ in 0..count {
                        let (a, b) = (next(&mut state) % people, next(&mut state) % people);
                        if a != b {
                            edges.push((id(base + a), id(base + b)));
                        }
                    }
                }
                // Chains, each ID linked to the next, read from the small end, the
                // large end (one thread leaves each ID a step from the one before:
                // the longest paths there are) or in any order.
                2 | 3 => {
                    let mut chain: Vec<u64> = (0..count as u64).collect();
                    if round % 2 == 0 {
                        chain.reverse();
                    }
                    if round % 3 == 0 {
                        for at in (1..chain.len()).rev() {
                            chain.swap(at, (next(&mut state) % (at as u64 + 1)) as usize);
                        }
                    }
                    let length = 2 + next(&mut state) % 120;
                    for &at in &chain {
                        if (at + 1) % length != 0 {
                            edges.push((id(base + at), id(base + at + 1)));
                        }
                    }
                }
                // A shared device: many small people, each joined to one ID, some lines repeated either way round.
                _ => {
                    let device = id(base + next(&mut state) % (3 * count as u64));
                    for at in 0..count as u64 {
                        let (first, second) = (id(base + 3 * at), id(base + 3 * at + 1));
                        edges.push((second, first));
                        if at % 3 != 0 && first != device {
                            edges.push((first, device));
                        }
                        if at % 5 == 0 {
                            edges.push((first, second));
                        }
                    }
                }
            }
            if edges.is_empty() {
                continue;
            }
            let threads = [1, 2, 3, 8][(round / 5 % 4) as usize];
            let lists = 1 + (next(&mut state) % 4) as usize;
            let distances = distances(round / 20);
            check(&edges, lists, threads, distances, &format!("round {round}, {threads} threads, distances {distances:?}"));
        }
    }

    /// Large enough that the threads meet in the table: people like the
    /// generator's (a few IDs each, linked to an earlier one), long chains, and
    /// shared devices that tie hundreds of people into one.
    #[test]
    fn threads_naming_one_table_agree_with_the_definition() {
        let mut state = 34;
        for round in 0..12u64 {
            let mut edges: Vec<(Key, Key)> = Vec::new();
            let base = (round + 1) << 32;
            let mut number = 0u64;
            while edges.len() < 60_000 {
                let r = next(&mut state);
                let ids = match r % 100 {
                    0 => 50 + (r >> 8) % 350,
                    1..=44 => 2,
                    45..=74 => 3,
                    _ => 4 + (r >> 8) % 5,
                };
                let chained = r % 100 == 0;
                let mut order: Vec<u64> = (1..ids).collect();
                if chained {
                    for at in (1..order.len()).rev() {
                        order.swap(at, (next(&mut state) % (at as u64 + 1)) as usize);
                    }
                }
                for at in order {
                    let earlier = if chained { at - 1 } else { next(&mut state) % at };
                    let (a, b) = (id(base + number + at), id(base + number + earlier));
                    edges.push(if next(&mut state) % 2 == 0 { (a, b) } else { (b, a) });
                }
                if r % 250 == 7 {
                    edges.push((id(base + number), id(base + (1 << 30) + r % 3))); // a shared device
                }
                number += ids;
            }
            // In the order a shuffled file gives them.
            for at in (1..edges.len()).rev() {
                edges.swap(at, (next(&mut state) % (at as u64 + 1)) as usize);
            }
            let threads = [8, 4, 3, 1][(round % 4) as usize];
            let distances = distances(round / 4);
            check(&edges, threads, threads, distances, &format!("large round {round}, {threads} threads, distances {distances:?}"));
        }
    }

    /// The tag that tells a pointer from a name is one no key carries.
    #[test]
    fn no_key_looks_like_a_pointer() {
        let mut state = 35;
        for number in 0..200_000u64 {
            let r = next(&mut state);
            let text = match number % 4 {
                0 => format!("u{r}"),
                1 => format!("{:x}_{}", r, number),
                2 => format!("U{}", r % 100_000),
                _ => format!("odd id {}", r % 10_000),
            };
            let key = key::key_of(&text.as_bytes()[..text.len().min(1 + (r >> 60) as usize)]);
            assert!(key != 0 && !is_pointer(key), "{text:?} has the key {key:#x}");
        }
        for slot in [0usize, 1, 2, 15, 16, 1 << 20, (1 << 40) + 5] {
            let word = pointer_to(slot);
            assert!(word != 0 && is_pointer(word) && pointed(word) == slot);
        }
    }
}
