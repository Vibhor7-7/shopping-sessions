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
use crate::pool::{Arena, Chunks, Combine, Pool};
use crate::reader::{self, Fields, Input, Job, Lines, ReadBuf};
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

/// How many lines pass between asking for a user's slot in the link table and
/// writing the line's records. The table is far larger than the cache, so every
/// lookup is a memory fetch; started this early, the fetches overlap instead of
/// each one stalling the parser.
const RING: usize = 16;
/// How many lines after that request the slot is read (`look_up_*`). Reading it
/// names the person, so the partitions the records go to; those are asked for
/// then, `RING - LOOK` lines before they are written.
const LOOK: usize = 12;
/// Slots in the ring the waiting lines are kept in: twice `RING`, so the slot
/// a line is put in is never the one about to be settled. A line is stored
/// first and the old ones are settled and looked up after it, when nothing of
/// the new line is left in a register: the other way round, everything the
/// line had worked out was kept on the stack across both and fetched back
/// (BENCH.md, experiment 38).
const SLOTS: usize = 2 * RING;

/// A parsed event copy on its way to its partitions.
#[derive(Clone, Copy, Default)]
struct PendingEvent {
    /// The user until the link table has been read, then the person. 0 for a
    /// copy that would not survive: it leaves only its dedup record.
    user: Key,
    event: Key,
    ts: u32,
    purchase: u32,
    ingest: u32,
    /// Where the user's lookup in the link table starts; after the lookup, the
    /// two partitions: the person's in the low half, the event's in the high half.
    home: u32,
}

#[derive(Clone, Copy, Default)]
struct PendingClick {
    /// The user until the link table has been read, then the person.
    user: Key,
    click: Key,
    ts: u32,
    campaign: u32,
    /// Where the lookup starts; after it, the person's partition.
    home: u32,
}

struct Sink<'a> {
    context: &'a Context<'a>,
    copies: Combine<'a, Copy_, 8>,
    events: Combine<'a, Event, 8>,
    clicks: Combine<'a, Click, 8>,
    waiting_events: [PendingEvent; SLOTS],
    event_count: usize,
    waiting_clicks: [PendingClick; SLOTS],
    click_count: usize,
    dates: ts::Dates,
}

impl Sink<'_> {
    /// Reads the link table for an event copy: from here on it knows its person
    /// and both its partitions, and the CPU has been asked for them.
    #[inline(always)]
    fn look_up_event(&mut self, slot: usize) {
        let context = self.context;
        let pending = unsafe { self.waiting_events.get_unchecked_mut(slot) };
        let mut person_partition = 0;
        if pending.user != 0 {
            let person = context.links.person_at(pending.user, pending.home);
            pending.user = person;
            person_partition = (sys::mix(person) >> context.person_shift) as usize;
            self.events.prefetch(person_partition);
        }
        let event_partition = (sys::mix(pending.event) >> context.dedup_shift) as usize;
        self.copies.prefetch(event_partition);
        pending.home = person_partition as u32 | (event_partition as u32) << 16;
    }

    /// Files an event copy that has been looked up: its event record if it would survive, and its dedup record.
    #[inline(always)]
    fn settle_event(&mut self, pending: PendingEvent) {
        let mut at = NO_EVENT;
        if pending.user != 0 {
            let partition = (pending.home & 0xffff) as usize;
            at = self.events.push(partition, Event { person: pending.user, ts: pending.ts, purchase: pending.purchase });
        }
        let partition = (pending.home >> 16) as usize;
        self.copies.push(partition, Copy_ { event: pending.event, ingest: pending.ingest, at });
    }

    #[inline(always)]
    fn look_up_click(&mut self, slot: usize) {
        let context = self.context;
        let pending = unsafe { self.waiting_clicks.get_unchecked_mut(slot) };
        let person = context.links.person_at(pending.user, pending.home);
        pending.user = person;
        let partition = (sys::mix(person) >> context.person_shift) as usize;
        self.clicks.prefetch(partition);
        pending.home = partition as u32;
    }

    #[inline(always)]
    fn settle_click(&mut self, pending: PendingClick) {
        self.clicks.push(pending.home as usize, Click { person: pending.user, click: pending.click, ts: pending.ts, campaign: pending.campaign });
    }

    /// Files everything still waiting, oldest first; the newest `LOOK` have not been looked up yet.
    fn drain(&mut self) {
        let count = self.event_count;
        for line in count.saturating_sub(RING)..count {
            if line + LOOK >= count {
                self.look_up_event(line & (SLOTS - 1));
            }
            self.settle_event(self.waiting_events[line & (SLOTS - 1)]);
        }
        self.event_count = 0;
        let count = self.click_count;
        for line in count.saturating_sub(RING)..count {
            if line + LOOK >= count {
                self.look_up_click(line & (SLOTS - 1));
            }
            self.settle_click(self.waiting_clicks[line & (SLOTS - 1)]);
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
    unsafe fn line(&mut self, data: *const u8, delims: *const u32, quotes: u32) -> bool {
        let sink = &mut *self.0;
        let context = sink.context;
        let mut fields = Fields::new();
        let (wrapped, is_line) = reader::split::<7>(data, delims, &mut fields);
        if !is_line {
            return false; // not seven fields
        }
        let (id, id_end) = (fields.at(0), fields.at(0) + fields.len(0));
        let (user_at, user_end) = (fields.at(1), fields.at(1) + fields.len(1));
        let (ts_at, ts_end) = (fields.at(2), fields.at(2) + fields.len(2));
        let (ingest_at, ingest_end) = (fields.at(3), fields.at(3) + fields.len(3));
        let (kind_at, kind_end) = (fields.at(4), fields.at(4) + fields.len(4));
        let (product_at, product_end) = (fields.at(5), fields.at(5) + fields.len(5));
        let (price_at, price_end) = (fields.at(6), fields.at(6) + fields.len(6));
        // Quotes anywhere but around whole fields make the line malformed; so does an empty event_id.
        if quotes != 2 * wrapped || id == id_end {
            return true;
        }
        let (ingest_seconds, ingest) = ts::parse_utc(data.add(ingest_at), ingest_end - ingest_at, &mut sink.dates);
        let at_ms = ts::parse(data.add(ts_at), ts_end - ts_at, &mut sink.dates);
        let catalog = context.catalog;

        let mut alive = user_end != user_at
            && product_end != product_at
            && ingest != ts::BAD
            && at_ms != ts::BAD
            && at_ms >= catalog.window_start
            && at_ms < catalog.window_end
            && ingest.wrapping_sub(at_ms) <= ts::DAY_MS;
        let mut purchase = 0u32;
        if alive {
            let kind = kind_end - kind_at;
            if kind == 4 {
                // Nine in ten are views, the rest carts, in no order: compared, not branched on.
                let word = (data.add(kind_at) as *const u32).read_unaligned();
                alive = (word == VIEW) | (word == CART);
            } else if kind == 8 && (data.add(kind_at) as *const u64).read_unaligned() == PURCHASE {
                let cents = price(data, price_at, price_end);
                if cents == 0 {
                    alive = false;
                } else {
                    purchase = context.purchase(cents, catalog.category(data.add(product_at), product_end - product_at));
                }
            } else {
                alive = false;
            }
        }
        let ingest = if ingest == ts::BAD {
            u32::MAX
        } else {
            (ingest_seconds - context.ingest_epoch).clamp(0, i64::from(u32::MAX) - 1) as u32
        };
        // Both IDs at once, and last: nothing between here and the record needs
        // a register they would have to wait in. An empty user_id, which no line
        // survives, has the key 0.
        let (event, user_key) = key::keys_of_two_padded(data.add(id), id_end - id, data.add(user_at), user_end - user_at);
        let (mut user, mut home) = (0, 0);
        if alive {
            user = user_key;
            home = context.links.prefetch(user);
        }
        // The line first, into a slot nothing is waiting in; then the line `RING`
        // before it is filed and the one `LOOK` before it looked up.
        let count = sink.event_count;
        sink.waiting_events[count & (SLOTS - 1)] =
            PendingEvent { user, event, ts: at_ms.wrapping_sub(context.epoch) as u32, purchase, ingest, home };
        sink.event_count = count + 1;
        if count >= RING {
            let ready = sink.waiting_events[(count - RING) & (SLOTS - 1)];
            sink.settle_event(ready);
        }
        if count >= LOOK {
            sink.look_up_event((count - LOOK) & (SLOTS - 1));
        }
        true
    }
}

impl Lines for ClickLines<'_, '_> {
    const COMMAS: usize = 3;

    #[inline(always)]
    unsafe fn line(&mut self, data: *const u8, delims: *const u32, quotes: u32) -> bool {
        let sink = &mut *self.0;
        let context = sink.context;
        let mut fields = Fields::new();
        let (wrapped, is_line) = reader::split::<4>(data, delims, &mut fields);
        if !is_line {
            return false; // not four fields
        }
        let (id, id_end) = (fields.at(0), fields.at(0) + fields.len(0));
        let (user_at, user_end) = (fields.at(1), fields.at(1) + fields.len(1));
        let (campaign_at, campaign_end) = (fields.at(2), fields.at(2) + fields.len(2));
        let (ts_at, ts_end) = (fields.at(3), fields.at(3) + fields.len(3));
        if quotes != 2 * wrapped || id == id_end || user_at == user_end {
            return true; // malformed, no click_id, or no user_id
        }
        let at_ms = ts::parse(data.add(ts_at), ts_end - ts_at, &mut sink.dates);
        let catalog = context.catalog;
        if at_ms == ts::BAD || at_ms < context.epoch || at_ms >= catalog.window_end {
            return true;
        }
        let campaign = catalog.campaign(data.add(campaign_at), campaign_end - campaign_at);
        if campaign == catalog::NONE {
            return true;
        }
        let (click, user) = key::keys_of_two_padded(data.add(id), id_end - id, data.add(user_at), user_end - user_at);
        let home = context.links.prefetch(user);
        let count = sink.click_count;
        sink.waiting_clicks[count & (SLOTS - 1)] =
            PendingClick { user, click, ts: (at_ms - context.epoch) as u32, campaign, home };
        sink.click_count = count + 1;
        if count >= RING {
            let ready = sink.waiting_clicks[(count - RING) & (SLOTS - 1)];
            sink.settle_click(ready);
        }
        if count >= LOOK {
            sink.look_up_click((count - LOOK) & (SLOTS - 1));
        }
        true
    }
}

pub struct Pools {
    pub copies: Pool<Copy_>,
    pub events: Pool<Event>,
    pub clicks: Pool<Click>,
}

/// Records per chunk, for each stream.
#[derive(Clone, Copy)]
pub struct ChunkSizes {
    pub copies: usize,
    pub events: usize,
    pub clicks: usize,
}

/// What one thread wrote, per partition.
pub struct Parsed {
    pub copies: Chunks,
    pub events: Chunks,
    pub clicks: Chunks,
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
    chunks: ChunkSizes,
) -> Vec<Parsed> {
    let total: u64 = events.iter().chain(clicks).map(|i| i.len).sum();
    let target = reader::job_bytes().unwrap_or((total / (threads as u64 * 16)).clamp(1 << 16, 4 << 20));
    let event_jobs: Vec<Job> = reader::jobs(events, target);
    let click_jobs: Vec<Job> = reader::jobs(clicks, target);
    let next = AtomicUsize::new(0);
    sys::parallel(threads, |_| {
        let mut buf = ReadBuf::new();
        // The three streams' buffers and states side by side on one huge page (pool::Arena).
        let mut arena = Arena::new(
            Combine::<Copy_, 8>::arena_bytes(dedup_partitions)
                + Combine::<Event, 8>::arena_bytes(person_partitions)
                + Combine::<Click, 8>::arena_bytes(person_partitions),
        );
        let mut sink = Sink {
            context,
            copies: Combine::new(&pools.copies, &mut arena, dedup_partitions, chunks.copies),
            events: Combine::new(&pools.events, &mut arena, person_partitions, chunks.events),
            clicks: Combine::new(&pools.clicks, &mut arena, person_partitions, chunks.clicks),
            waiting_events: [PendingEvent::default(); SLOTS],
            event_count: 0,
            waiting_clicks: [PendingClick::default(); SLOTS],
            click_count: 0,
            dates: ts::Dates::new(),
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
        }
    })
}
