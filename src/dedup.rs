//! Dedup on event_id: keep the copy with the smallest ingest_ts.
//!
//! Only a few percent of events have more than one copy, so the work is finding
//! them. Each partition is scanned twice against a small bitmap of key hashes:
//! the first scan marks the hash slots hit more than once, the second collects
//! the copies in those slots. Only those few are sorted, by (event_id,
//! ingest_ts); in each run of one event_id the first copy wins and the event
//! records of the others are marked dropped.

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::parse::{Copy_, Event, Parsed, DROPPED, NO_EVENT};
use crate::pool::{self, Pool};
use crate::sys;

/// Returns how many event records were dropped.
pub fn run(
    copies: &Pool<Copy_>,
    events: &Pool<Event>,
    parsed: &[Parsed],
    partitions: usize,
    dedup_shift: u32,
    threads: usize,
) -> u64 {
    let lists: Vec<&pool::Chunks> = parsed.iter().map(|p| &p.copies).collect();
    let next = AtomicUsize::new(0);
    let partition_bits = 64 - dedup_shift;
    let dropped = sys::parallel(threads, |_| {
        let mut seen: Vec<u64> = Vec::new();
        let mut again: Vec<u64> = Vec::new();
        let mut suspects: Vec<Copy_> = Vec::new();
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
            // 16 slots a copy keeps chance collisions near 6%.
            let bits = (count * 16).next_power_of_two().max(1024);
            let slot_shift = 64 - bits.trailing_zeros();
            seen.clear();
            seen.resize(bits / 64, 0);
            again.clear();
            again.resize(bits / 64, 0);
            // The partition took the hash's top bits; the slot takes the ones below.
            let slot = |copy: &Copy_| ((sys::mix(copy.event) << partition_bits) >> slot_shift) as usize;
            for chunks in &lists {
                for &(start, len) in &chunks[partition] {
                    let records = unsafe { std::slice::from_raw_parts(copies.at(start), len as usize) };
                    for copy in records {
                        let at = slot(copy);
                        let (word, bit) = (at >> 6, 1u64 << (at & 63));
                        unsafe {
                            let had = *seen.get_unchecked(word) & bit;
                            *again.get_unchecked_mut(word) |= had;
                            *seen.get_unchecked_mut(word) |= bit;
                        }
                    }
                }
            }
            suspects.clear();
            for chunks in &lists {
                for &(start, len) in &chunks[partition] {
                    let records = unsafe { std::slice::from_raw_parts(copies.at(start), len as usize) };
                    for copy in records {
                        let at = slot(copy);
                        if unsafe { *again.get_unchecked(at >> 6) } & (1u64 << (at & 63)) != 0 {
                            suspects.push(*copy);
                        }
                    }
                }
            }
            suspects.sort_unstable_by_key(|copy| (copy.event, copy.ingest));
            let mut i = 0;
            while i < suspects.len() {
                let mut j = i + 1;
                while j < suspects.len() && suspects[j].event == suspects[i].event {
                    // A later copy, or an identical one with the same ingest_ts: it loses.
                    if suspects[j].at != NO_EVENT {
                        unsafe { (*events.at(suspects[j].at)).ts = DROPPED };
                        dropped += 1;
                    }
                    j += 1;
                }
                i = j;
            }
        }
        dropped
    });
    dropped.iter().sum()
}
