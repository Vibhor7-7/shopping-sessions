//! Dedup on event_id: keep the copy with the smallest ingest_ts.
//!
//! Only a few percent of events have more than one copy, so the work is finding
//! them. Each partition is scanned twice against a small table of two-bit
//! counters indexed by key hash: the first scan marks the slots hit more than
//! once, the second takes the copies in those slots, a few in a hundred, to a
//! small table keyed by event_id that holds the earliest copy of each so far.
//! A copy that meets another of its event there is compared with it, and the
//! event record of the one that loses is marked dropped. Nothing is sorted.

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::parse::{Copy_, Event, Parsed, DROPPED, NO_EVENT};
use crate::pool::{self, Pool};
use crate::sys;

/// The earliest copy met so far of each event, in an open-addressing table.
/// An event key is never 0, so a slot whose key is 0 is free.
struct Earliest {
    slots: Vec<Copy_>,
    mask: usize,
}

impl Earliest {
    fn new() -> Self {
        Self { slots: Vec::new(), mask: 0 }
    }

    /// Empties the table and makes it at most half full with `copies` in it.
    fn reset(&mut self, copies: usize) {
        let capacity = (copies * 2).next_power_of_two().max(16);
        self.slots.clear();
        self.slots.resize(capacity, Copy_ { event: 0, ingest: 0, at: 0 });
        self.mask = capacity - 1;
    }

    /// Takes one more copy; `hash` is any hash of its event key. If a copy of
    /// the same event is already here, the later of the two loses (the new one,
    /// if their `ingest_ts` are equal: then they are identical), the earlier one
    /// stays, and the loser's `at` is returned, which may be `NO_EVENT`.
    /// A copy that meets no other returns `NO_EVENT`.
    #[inline(always)]
    fn meet(&mut self, copy: Copy_, hash: u64) -> u32 {
        let mut at = hash as usize & self.mask;
        loop {
            let held = unsafe { self.slots.get_unchecked_mut(at) };
            if held.event == copy.event {
                if copy.ingest < held.ingest {
                    let lost = held.at;
                    *held = copy;
                    return lost;
                }
                return copy.at;
            }
            if held.event == 0 {
                *held = copy;
                return NO_EVENT;
            }
            at = (at + 1) & self.mask;
        }
    }
}

/// Returns how many event records were dropped.
pub fn run(
    copies: &Pool<Copy_>,
    events: &Pool<Event>,
    parsed: &[Parsed],
    partitions: usize,
    dedup_shift: u32,
    threads: usize,
    filter_slots: usize,
) -> u64 {
    let lists: Vec<&pool::Chunks> = parsed.iter().map(|p| &p.copies).collect();
    let next = AtomicUsize::new(0);
    let partition_bits = 64 - dedup_shift;
    let dropped = sys::parallel(threads, |_| {
        // Two bits a slot, 32 slots a word: seen once, seen again.
        let mut state: Vec<u64> = Vec::new();
        let mut earliest = Earliest::new();
        let mut dropped = 0u64;
        loop {
            let partition = next.fetch_add(1, Ordering::Relaxed);
            if partition >= partitions {
                break;
            }
            let count: usize = lists.iter().map(|chunks| chunks[partition].iter().map(|c| c.1 as usize).sum::<usize>()).sum();
            if count < 2 {
                continue;
            }
            // `filter_slots` slots a copy: 16 keep chance collisions near 6%.
            let slots = (count * filter_slots).next_power_of_two().max(1024);
            let slot_shift = 64 - slots.trailing_zeros();
            state.clear();
            state.resize(slots / 32, 0);
            // The partition took the hash's top bits; the slot takes the ones below.
            let slot = |hash: u64| ((hash << partition_bits) >> slot_shift) as usize;
            // How many copies came to a slot that had been hit before.
            let mut late = 0usize;
            for chunks in &lists {
                for &(start, len) in &chunks[partition] {
                    let records = unsafe { std::slice::from_raw_parts(copies.at(start), len as usize) };
                    for copy in records {
                        let at = slot(sys::mix(copy.event));
                        unsafe {
                            let word = state.get_unchecked_mut(at >> 5);
                            let shift = (at & 31) * 2;
                            let seen = (*word >> shift) & 1;
                            late += seen as usize;
                            *word |= (1 | (seen << 1)) << shift;
                        }
                    }
                }
            }
            if late == 0 {
                continue; // no slot was hit twice: no event has two copies
            }
            // A slot hit more than once holds one copy that was not late and at
            // least one that was: at most twice as many copies as late ones.
            earliest.reset(2 * late);
            for chunks in &lists {
                for &(start, len) in &chunks[partition] {
                    let records = unsafe { std::slice::from_raw_parts(copies.at(start), len as usize) };
                    for copy in records {
                        let hash = sys::mix(copy.event);
                        let at = slot(hash);
                        if unsafe { *state.get_unchecked(at >> 5) } >> ((at & 31) * 2 + 1) & 1 != 0 {
                            // The filter's slot came from the hash's high bits; the table takes its low ones.
                            let lost = earliest.meet(*copy, hash);
                            if lost != NO_EVENT {
                                unsafe { (*events.at(lost)).ts = DROPPED };
                                dropped += 1;
                            }
                        }
                    }
                }
            }
        }
        dropped
    });
    dropped.iter().sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn next(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// The definition: of the copies of one event, the one with the smallest
    /// `ingest` stays and every other loses. Here every copy has an `at` of its
    /// own, so the losers can be told apart; where the smallest `ingest` is
    /// shared, any one of those that share it may be the one that stays.
    #[test]
    fn every_copy_but_the_earliest_of_its_event_loses_once() {
        let mut state = 30;
        let mut earliest = Earliest::new();
        let (mut met, mut tied) = (0usize, 0usize);
        for round in 0..3_000 {
            let count = 1 + (next(&mut state) % 400) as usize;
            // Few events and few instants, so copies meet and tie; sometimes many of one event.
            let event_range = 1 + next(&mut state) % (2 * count as u64 + 1);
            let ingest_range = 1 + next(&mut state) % 6;
            let weak_hash = round % 4 == 0; // every copy starts at one of 4 slots: long probe runs
            let copies: Vec<Copy_> = (0..count)
                .map(|i| Copy_ {
                    event: (1 + next(&mut state) % event_range) << 4,
                    ingest: if next(&mut state) % 50 == 0 { u32::MAX } else { (next(&mut state) % ingest_range) as u32 },
                    at: i as u32,
                })
                .collect();
            // As `run` sizes it: for at most twice as many copies as were late. Here, simply all of them.
            earliest.reset(copies.len());
            let mut lost = vec![0u32; count];
            for copy in &copies {
                let hash = if weak_hash { sys::mix(copy.event) & 3 } else { sys::mix(copy.event) };
                let loser = earliest.meet(*copy, hash);
                if loser != NO_EVENT {
                    lost[loser as usize] += 1;
                    met += 1;
                }
            }
            let mut events: Vec<u64> = copies.iter().map(|c| c.event).collect();
            events.sort_unstable();
            events.dedup();
            for event in events {
                let of_event: Vec<&Copy_> = copies.iter().filter(|c| c.event == event).collect();
                let smallest = of_event.iter().map(|c| c.ingest).min().unwrap();
                let stayed: Vec<&&Copy_> = of_event.iter().filter(|c| lost[c.at as usize] == 0).collect();
                assert_eq!(stayed.len(), 1, "round {round}: event {event} keeps {} copies", stayed.len());
                assert_eq!(stayed[0].ingest, smallest, "round {round}: event {event} kept a later copy");
                assert!(of_event.iter().all(|c| lost[c.at as usize] <= 1), "round {round}: a copy of {event} lost twice");
                tied += usize::from(of_event.iter().filter(|c| c.ingest == smallest).count() > 1);
            }
        }
        assert!(met > 100_000 && tied > 10_000, "{met} copies lost, {tied} events tied");
    }

    /// A loser without an event record is reported as `NO_EVENT`, like a copy that met nothing:
    /// either way there is no record to drop.
    #[test]
    fn a_copy_without_an_event_record_has_nothing_to_drop() {
        let mut earliest = Earliest::new();
        earliest.reset(4);
        let copy = |ingest: u32, at: u32| Copy_ { event: 7 << 4, ingest, at };
        assert_eq!(earliest.meet(copy(50, NO_EVENT), 1), NO_EVENT); // first of its event
        assert_eq!(earliest.meet(copy(60, 3), 1), 3); // later than the invalid copy: dropped
        assert_eq!(earliest.meet(copy(40, 5), 1), NO_EVENT); // earlier: the invalid copy loses, nothing to drop
        assert_eq!(earliest.meet(copy(45, NO_EVENT), 1), NO_EVENT); // later, and invalid itself
        assert_eq!(earliest.meet(copy(40, 9), 1), 9); // identical to the one kept: one of the two goes
        assert_eq!(earliest.meet(copy(10, NO_EVENT), 1), 5); // an invalid copy earlier than all: the kept one is dropped
        assert_eq!(earliest.meet(copy(20, 11), 1), 11); // and every valid copy after it
    }
}
