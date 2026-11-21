//! Events and clicks: from CSV lines to partitioned records.
//!
//! Each event line that parses becomes one dedup record, keyed by event_id.
//! If the copy would also survive validation and the late cutoff, it becomes an
//! event record too, filed under its person, and the dedup record points at it.
//! Dedup then only has to drop the event records of losing copies: a copy that
//! is invalid has no event record, so when it wins, nothing of the event is left,
//! which is exactly "dedup comes before validation".
//!
//! Clicks need no dedup pass at all. Copies of a click are identical, so they
//! credit the same campaign and assist as the same campaign, once.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use crate::catalog::{self, Catalog};
use crate::key::{self, Key};
use crate::links::Links;
use crate::pool::{Chunks, Pool, Scatter};
use crate::reader::{self, Input, Job, Lines, ReadBuf};
use crate::sys;
use crate::ts;

/// One parsed copy of an event, for dedup.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Copy_ {
    pub event: Key,
    /// ingest_ts in seconds, offset so any plausible value fits.
    pub ingest: u32,
    /// Global index of this copy's event record, or NO_EVENT if it would not survive.
    pub at: u32,
}

/// An event that survives if its copy wins dedup.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Event {
    pub person: Key,
    /// Milliseconds since `Context::epoch`; DROPPED once a better copy is found.
    pub ts: u32,
    /// 0 for a view or cart; a purchase's price and category otherwise.
    pub purchase: u32,
}

#[derive(Clone, Copy)]
#[repr(C)]
pub struct Click {
    pub person: Key,
    pub click: Key,
    pub ts: u32,
    pub campaign: u32,
}

pub const NO_EVENT: u32 = u32::MAX;
pub const DROPPED: u32 = u32::MAX;
pub const CLICK_REACH_MS: i64 = 7 * ts::DAY_MS;

/// Purchases whose category number does not fit beside the price: only when
/// products.csv has more than 255 categories.
pub type WidePurchases = Mutex<Vec<(u32, u32)>>;

pub struct Context<'a> {
    pub catalog: &'a Catalog,
    pub links: &'a Links,
    /// Timestamps are stored relative to this: the start of the click window.
    pub epoch: i64,
    pub ingest_epoch: i64,
    pub person_shift: u32,
    pub dedup_shift: u32,
    pub wide: Option<&'a WidePurchases>,
}

impl Context<'_> {
    #[inline(always)]
    fn purchase(&self, price: u32, category: u32) -> u32 {
        match self.wide {
            None => price | (category << 24),
            Some(table) => {
                let mut table = table.lock().unwrap();
                table.push((price, category));
                table.len() as u32
            }
        }
    }
}

#[derive(Default, Clone, Copy)]
pub struct Stats {
    pub event_lines: u64,
    pub event_copies: u64,
    pub event_candidates: u64,
    pub click_lines: u64,
    pub clicks: u64,
}

/// How many lines pass between asking for a user's slot in the link table and
/// reading it. The table is far larger than the cache, so every lookup is a
/// memory fetch; started this early, the fetches overlap instead of each one
/// stalling the parser.
const RING: usize = 16;

/// A parsed event copy waiting for its user's link-table slot to arrive.
#[derive(Clone, Copy, Default)]
struct PendingEvent {
    user: Key,
    event: Key,
    ts: u32,
    purchase: u32,
    ingest: u32,
    alive: bool,
}

#[derive(Clone, Copy, Default)]
struct PendingClick {
    user: Key,
    click: Key,
    ts: u32,
    campaign: u32,
}

struct Sink<'a> {
    context: &'a Context<'a>,
    copies: Scatter<'a, Copy_>,
    events: Scatter<'a, Event>,
    clicks: Scatter<'a, Click>,
    waiting_events: [PendingEvent; RING],
    event_count: usize,
    waiting_clicks: [PendingClick; RING],
    click_count: usize,
    stats: Stats,
}

impl Sink<'_> {
    /// Files an event copy: its event record if it would survive, and its dedup record.
    #[inline(always)]
    fn settle_event(&mut self, pending: PendingEvent) {
        let context = self.context;
        let mut at = NO_EVENT;
        if pending.alive {
            let person = context.links.person(pending.user);
            let partition = (sys::mix(person) >> context.person_shift) as usize;
            at = self.events.push(partition, Event { person, ts: pending.ts, purchase: pending.purchase });
        }
        let partition = (sys::mix(pending.event) >> context.dedup_shift) as usize;
        self.copies.push(partition, Copy_ { event: pending.event, ingest: pending.ingest, at });
    }

    #[inline(always)]
    fn settle_click(&mut self, pending: PendingClick) {
        let context = self.context;
        let person = context.links.person(pending.user);
        let partition = (sys::mix(person) >> context.person_shift) as usize;
        self.clicks.push(partition, Click { person, click: pending.click, ts: pending.ts, campaign: pending.campaign });
    }

    /// Files everything still waiting.
    fn drain(&mut self) {
        for i in 0..self.event_count.min(RING) {
            self.settle_event(self.waiting_events[i]);
        }
        self.event_count = 0;
        for i in 0..self.click_count.min(RING) {
            self.settle_click(self.waiting_clicks[i]);
        }
        self.click_count = 0;
    }
}

struct EventLines<'s, 'a>(&'s mut Sink<'a>);
struct ClickLines<'s, 'a>(&'s mut Sink<'a>);

const VIEW: u32 = u32::from_le_bytes(*b"view");
const CART: u32 = u32::from_le_bytes(*b"cart");
const PURCHASE: u64 = u64::from_le_bytes(*b"purchase");

/// price_cents of a purchase: digits only, 1 to 10,000,000, leading zeros allowed. 0 if invalid.
#[inline(always)]
unsafe fn price(data: *const u8, mut start: usize, end: usize) -> u32 {
    if start == end {
        return 0;
    }
    while end - start > 8 {
        if *data.add(start) != b'0' {
            return 0; // not a digit, or more than 8 significant digits
        }
        start += 1;
    }
    let mut value = 0u32;
    let mut not_digit = 0u32;
    for at in start..end {
        let digit = u32::from((*data.add(at)).wrapping_sub(b'0'));
        not_digit |= 9u32.wrapping_sub(digit);
        value = value.wrapping_mul(10).wrapping_add(digit);
    }
    if (not_digit >> 31) != 0 || value > 10_000_000 {
        0
    } else {
        value
    }
}

impl Lines for EventLines<'_, '_> {
    const COMMAS: usize = 6;

    #[inline(always)]
    unsafe fn line(&mut self, data: *const u8, start: usize, commas: *const u32, end: usize, quotes: u32) {
        let sink = &mut *self.0;
        let context = sink.context;
        sink.stats.event_lines += 1;
        let mut from = [0usize; 7];
        let mut to = [0usize; 7];
        from[0] = start;
        for i in 0..6 {
            let comma = *commas.add(i) as usize;
            to[i] = comma;
            from[i + 1] = comma + 1;
        }
        to[6] = end;
        let mut wrapped = 0;
        for i in 0..7 {
            wrapped += reader::unquote(data, &mut from[i], &mut to[i]);
        }
        // Quotes anywhere but around whole fields make the line malformed; so does an empty event_id.
        if quotes != 2 * wrapped || from[0] == to[0] {
            return;
        }
        let field = |i: usize| std::slice::from_raw_parts(data.add(from[i]), to[i] - from[i]);
        let event = key::key_of(field(0));
        let ingest = ts::parse(data.add(from[3]), to[3] - from[3]);
        let at_ms = ts::parse(data.add(from[2]), to[2] - from[2]);
        let catalog = context.catalog;

        let mut alive = to[1] != from[1]
            && to[5] != from[5]
            && ingest != ts::BAD
            && at_ms != ts::BAD
            && at_ms >= catalog.window_start
            && at_ms < catalog.window_end
            && ingest.wrapping_sub(at_ms) <= ts::DAY_MS;
        let mut purchase = 0u32;
        if alive {
            let kind = to[4] - from[4];
            if kind == 4 {
                let word = (data.add(from[4]) as *const u32).read_unaligned();
                alive = word == VIEW || word == CART;
            } else if kind == 8 && (data.add(from[4]) as *const u64).read_unaligned() == PURCHASE {
                let cents = price(data, from[6], to[6]);
                if cents == 0 {
                    alive = false;
                } else {
                    purchase = context.purchase(cents, catalog.category(field(5)));
                }
            } else {
                alive = false;
            }
        }
        let mut user = 0;
        if alive {
            user = key::key_of(field(1));
            context.links.prefetch(user);
            sink.stats.event_candidates += 1;
        }
        let ingest = if ingest == ts::BAD {
            u32::MAX
        } else {
            (ingest.div_euclid(1_000) - context.ingest_epoch).clamp(0, i64::from(u32::MAX) - 1) as u32
        };
        let slot = sink.event_count & (RING - 1);
        if sink.event_count >= RING {
            let ready = sink.waiting_events[slot];
            sink.settle_event(ready);
        }
        sink.waiting_events[slot] =
            PendingEvent { user, event, ts: at_ms.wrapping_sub(context.epoch) as u32, purchase, ingest, alive };
        sink.event_count += 1;
        sink.stats.event_copies += 1;
    }
}

impl Lines for ClickLines<'_, '_> {
    const COMMAS: usize = 3;

    #[inline(always)]
    unsafe fn line(&mut self, data: *const u8, start: usize, commas: *const u32, end: usize, quotes: u32) {
        let sink = &mut *self.0;
        let context = sink.context;
        sink.stats.click_lines += 1;
        let mut from = [0usize; 4];
        let mut to = [0usize; 4];
        from[0] = start;
        for i in 0..3 {
            let comma = *commas.add(i) as usize;
            to[i] = comma;
            from[i + 1] = comma + 1;
        }
        to[3] = end;
        let mut wrapped = 0;
        for i in 0..4 {
            wrapped += reader::unquote(data, &mut from[i], &mut to[i]);
        }
        if quotes != 2 * wrapped || from[0] == to[0] || from[1] == to[1] {
            return; // malformed, no click_id, or no user_id
        }
        let field = |i: usize| std::slice::from_raw_parts(data.add(from[i]), to[i] - from[i]);
        let at_ms = ts::parse(data.add(from[3]), to[3] - from[3]);
        let catalog = context.catalog;
        if at_ms == ts::BAD || at_ms < context.epoch || at_ms >= catalog.window_end {
            return;
        }
        let campaign = catalog.campaign(field(2));
        if campaign == catalog::NONE {
            return;
        }
        let user = key::key_of(field(1));
        context.links.prefetch(user);
        let slot = sink.click_count & (RING - 1);
        if sink.click_count >= RING {
            let ready = sink.waiting_clicks[slot];
            sink.settle_click(ready);
        }
        sink.waiting_clicks[slot] =
            PendingClick { user, click: key::key_of(field(0)), ts: (at_ms - context.epoch) as u32, campaign };
        sink.click_count += 1;
        sink.stats.clicks += 1;
    }
}

pub struct Pools {
    pub copies: Pool<Copy_>,
    pub events: Pool<Event>,
    pub clicks: Pool<Click>,
}

/// What one thread wrote, per partition.
pub struct Parsed {
    pub copies: Chunks,
    pub events: Chunks,
    pub clicks: Chunks,
    pub stats: Stats,
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    context: &Context,
    pools: &Pools,
    events: &[Input],
    clicks: &[Input],
    threads: usize,
    person_partitions: usize,
    dedup_partitions: usize,
    chunk: usize,
) -> Vec<Parsed> {
    let total: u64 = events.iter().chain(clicks).map(|i| i.len).sum();
    let target = reader::job_bytes().unwrap_or((total / (threads as u64 * 16)).clamp(1 << 16, 4 << 20));
    let event_jobs: Vec<Job> = reader::jobs(events, target);
    let click_jobs: Vec<Job> = reader::jobs(clicks, target);
    let next = AtomicUsize::new(0);
    sys::parallel(threads, |_| {
        let mut buf = ReadBuf::new();
        let mut sink = Sink {
            context,
            copies: Scatter::new(&pools.copies, dedup_partitions, chunk),
            events: Scatter::new(&pools.events, person_partitions, chunk),
            clicks: Scatter::new(&pools.clicks, person_partitions, chunk),
            waiting_events: [PendingEvent::default(); RING],
            event_count: 0,
            waiting_clicks: [PendingClick::default(); RING],
            click_count: 0,
            stats: Stats::default(),
        };
        loop {
            let at = next.fetch_add(1, Ordering::Relaxed);
            if let Some(job) = event_jobs.get(at) {
                reader::walk(&events[job.file as usize].file, job.offset, job.length, &mut buf, &mut EventLines(&mut sink));
            } else if let Some(job) = click_jobs.get(at - event_jobs.len()) {
                reader::walk(&clicks[job.file as usize].file, job.offset, job.length, &mut buf, &mut ClickLines(&mut sink));
            } else {
                break;
            }
        }
        sink.drain();
        Parsed {
            copies: sink.copies.finish(),
            events: sink.events.finish(),
            clicks: sink.clicks.finish(),
            stats: sink.stats,
        }
    })
}
