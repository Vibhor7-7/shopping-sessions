//! The oracle: rules.md read literally, one thread, std collections, no tricks.
//!
//! Usage: reference DATA_DIR OUT_DIR
//! Writes OUT_DIR/user_daily.csv and OUT_DIR/campaign_daily.csv in the format of
//! dev-answer/ and fixtures/*/expected/, so `sort | diff` or scripts/compare.py
//! can check it. It shares no code with the fast pipeline, so the two agreeing
//! on a dataset means two independent implementations agree.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::Write;
use std::path::Path;

const DAY_MS: i64 = 86_400_000;
const SESSION_GAP_MS: i64 = 1_800_000;
const CLICK_REACH_MS: i64 = 7 * DAY_MS;

type Bytes = Vec<u8>;

// ---------------------------------------------------------------- CSV dialect

/// The fields of a line, quotes removed, or None if the line is malformed.
fn fields(line: &[u8], count: usize) -> Option<Vec<&[u8]>> {
    if line.is_empty() {
        return None;
    }
    let parts: Vec<&[u8]> = line.split(|&b| b == b',').collect();
    if parts.len() != count {
        return None;
    }
    let mut out = Vec::with_capacity(count);
    for part in parts {
        let quotes = part.iter().filter(|&&b| b == b'"').count();
        if quotes == 0 {
            out.push(part);
        } else if quotes == 2 && part.len() >= 2 && part[0] == b'"' && part[part.len() - 1] == b'"' {
            out.push(&part[1..part.len() - 1]);
        } else {
            return None;
        }
    }
    Some(out)
}

/// The data lines of a file: everything after the header row. Only `\n` ends a line.
fn data_lines(content: &[u8]) -> Vec<&[u8]> {
    let mut lines: Vec<&[u8]> = content.split(|&b| b == b'\n').collect();
    // "a\nb\n" splits into ["a", "b", ""]: the last piece is not a line.
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    if !lines.is_empty() {
        lines.remove(0);
    }
    lines
}

fn csv_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.ends_with(".csv") && !name.starts_with('.') && entry.path().is_file() {
                out.push(entry.path());
            }
        }
    }
    out.sort();
    out
}

// ------------------------------------------------------------------ timestamps

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    era * 146_097 + year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let doe = shifted - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, doy - (153 * mp + 2) / 5 + 1)
}

fn number(text: &[u8]) -> Option<i64> {
    if text.is_empty() || !text.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(text.iter().fold(0i64, |acc, &b| acc * 10 + i64::from(b - b'0')))
}

/// `YYYY-MM-DDTHH:MM:SS` as seconds since the epoch, or None if it is not a real instant.
fn civil_seconds(text: &[u8]) -> Option<i64> {
    if text.len() != 19 || text[4] != b'-' || text[7] != b'-' || text[10] != b'T' || text[13] != b':' || text[16] != b':' {
        return None;
    }
    let year = number(&text[0..4])?;
    let month = number(&text[5..7])?;
    let day = number(&text[8..10])?;
    let hour = number(&text[11..13])?;
    let minute = number(&text[14..16])?;
    let second = number(&text[17..19])?;
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => 28 + i64::from(leap),
        _ => return None,
    };
    if day < 1 || day > days_in_month || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Milliseconds since the epoch for forms F1, F2 and F3; None for anything else.
fn timestamp(text: &[u8]) -> Option<i64> {
    if text.len() == 20 && text[19] == b'Z' {
        return civil_seconds(&text[..19]).map(|s| s * 1_000);
    }
    if text.len() == 25 && (text[19] == b'+' || text[19] == b'-') {
        let local = civil_seconds(&text[..19])?;
        if text[22] != b':' {
            return None;
        }
        let hours = number(&text[20..22])?;
        let minutes = number(&text[23..25])?;
        if hours > 23 || minutes > 59 {
            return None;
        }
        let offset = (hours * 60 + minutes) * 60;
        let utc = if text[19] == b'+' { local - offset } else { local + offset };
        return Some(utc * 1_000);
    }
    if !text.is_empty() && text.iter().all(u8::is_ascii_digit) {
        let significant: &[u8] = {
            let zeros = text.iter().take_while(|&&b| b == b'0').count();
            &text[zeros..]
        };
        // More digits than any window can hold: a number, but never inside one.
        if significant.len() > 17 {
            return None;
        }
        return Some(significant.iter().fold(0i64, |acc, &b| acc * 10 + i64::from(b - b'0')));
    }
    None
}

fn manifest_time(text: &str, key: &str) -> i64 {
    let at = text.find(&format!("\"{key}\"")).unwrap_or_else(|| panic!("manifest has no {key}"));
    let rest = &text[at + key.len() + 2..];
    let open = rest.find('"').expect("manifest value") + 1;
    let close = open + rest[open..].find('"').expect("manifest value end");
    timestamp(rest[open..close].as_bytes()).unwrap_or_else(|| panic!("manifest {key} is not a timestamp"))
}

// ------------------------------------------------------------------ linked IDs

#[derive(Default)]
struct People {
    index: HashMap<Bytes, usize>,
    ids: Vec<Bytes>,
    parent: Vec<usize>,
    name: HashMap<usize, Bytes>,
}

impl People {
    fn node(&mut self, id: &[u8]) -> usize {
        if let Some(&at) = self.index.get(id) {
            return at;
        }
        let at = self.ids.len();
        self.index.insert(id.to_vec(), at);
        self.ids.push(id.to_vec());
        self.parent.push(at);
        at
    }

    fn find(&mut self, mut at: usize) -> usize {
        while self.parent[at] != at {
            self.parent[at] = self.parent[self.parent[at]];
            at = self.parent[at];
        }
        at
    }

    fn join(&mut self, a: &[u8], b: &[u8]) {
        let (a, b) = (self.node(a), self.node(b));
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.parent[a] = b;
        }
    }

    /// Name every person by the byte-wise smallest of their IDs.
    fn finish(&mut self) {
        for at in 0..self.ids.len() {
            let root = self.find(at);
            let id = self.ids[at].clone();
            match self.name.get_mut(&root) {
                Some(name) if *name <= id => {}
                Some(name) => *name = id,
                None => {
                    self.name.insert(root, id);
                }
            }
        }
    }

    fn person(&mut self, id: &[u8]) -> Bytes {
        match self.index.get(id).copied() {
            Some(at) => {
                let root = self.find(at);
                self.name[&root].clone()
            }
            None => id.to_vec(),
        }
    }
}

// ------------------------------------------------------------------------ main

struct Event {
    ts: i64,
    purchase: bool,
    price: i64,
    product: Bytes,
}

struct Click {
    ts: i64,
    id: Bytes,
    campaign: Bytes,
}

#[derive(Default)]
struct UserDay {
    sessions: i64,
    events: i64,
    revenue: i64,
    categories: BTreeMap<Bytes, i64>,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: reference DATA_DIR OUT_DIR");
        std::process::exit(2);
    }
    let (data, out) = (Path::new(&args[1]), Path::new(&args[2]));

    let manifest = fs::read_to_string(data.join("manifest.json")).expect("manifest.json");
    let window_start = manifest_time(&manifest, "window_start");
    let window_end = manifest_time(&manifest, "window_end");

    let mut category: HashMap<Bytes, Bytes> = HashMap::new();
    for line in data_lines(&fs::read(data.join("products.csv")).expect("products.csv")) {
        if let Some(f) = fields(line, 2) {
            category.insert(f[0].to_vec(), f[1].to_vec());
        }
    }
    let mut channel: HashMap<Bytes, Bytes> = HashMap::new();
    for line in data_lines(&fs::read(data.join("campaigns.csv")).expect("campaigns.csv")) {
        if let Some(f) = fields(line, 2) {
            channel.insert(f[0].to_vec(), f[1].to_vec());
        }
    }

    // Linked IDs: parse, join, name.
    let mut people = People::default();
    for path in csv_files(&data.join("links")) {
        let content = fs::read(&path).expect("link file");
        for line in data_lines(&content) {
            let Some(f) = fields(line, 2) else { continue };
            if f[0].is_empty() || f[1].is_empty() {
                continue;
            }
            people.join(f[0], f[1]);
        }
    }
    people.finish();

    // Events 1 and 2: parse, then keep the copy of each event_id with the smallest ingest_ts.
    let mut kept: HashMap<Bytes, (i64, Vec<Bytes>)> = HashMap::new();
    for path in csv_files(&data.join("events")) {
        let content = fs::read(&path).expect("event file");
        for line in data_lines(&content) {
            let Some(f) = fields(line, 7) else { continue };
            if f[0].is_empty() {
                continue;
            }
            let ingest = timestamp(f[3]).expect("ingest_ts is always valid F1");
            let replace = match kept.get(f[0]) {
                Some((best, _)) => ingest < *best,
                None => true,
            };
            if replace {
                kept.insert(f[0].to_vec(), (ingest, f.iter().map(|x| x.to_vec()).collect()));
            }
        }
    }

    // Events 3 and 4: validate the kept copy, drop late ones.
    let mut events_of: HashMap<Bytes, Vec<Event>> = HashMap::new();
    for (_, (ingest, f)) in kept {
        let (user, ts, kind, product, price) = (&f[1], &f[2], &f[4], &f[5], &f[6]);
        if user.is_empty() || product.is_empty() {
            continue;
        }
        let Some(ts) = timestamp(ts) else { continue };
        if ts < window_start || ts >= window_end {
            continue;
        }
        let purchase = match kind.as_slice() {
            b"view" | b"cart" => false,
            b"purchase" => true,
            _ => continue,
        };
        let mut cents = 0;
        if purchase {
            if price.is_empty() || !price.iter().all(u8::is_ascii_digit) {
                continue;
            }
            let zeros = price.iter().take_while(|&&b| b == b'0').count();
            let significant = &price[zeros..];
            if significant.is_empty() || significant.len() > 8 {
                continue;
            }
            cents = number(significant).unwrap();
            if !(1..=10_000_000).contains(&cents) {
                continue;
            }
        }
        if ingest - ts > DAY_MS {
            continue;
        }
        let person = people.person(user);
        events_of.entry(person).or_default().push(Event { ts, purchase, price: cents, product: product.clone() });
    }

    // Clicks 1 to 3: parse, dedup, validate.
    let mut seen_clicks: BTreeSet<Bytes> = BTreeSet::new();
    let mut clicks_of: HashMap<Bytes, Vec<Click>> = HashMap::new();
    for path in csv_files(&data.join("clicks")) {
        let content = fs::read(&path).expect("click file");
        for line in data_lines(&content) {
            let Some(f) = fields(line, 4) else { continue };
            if f[0].is_empty() || !seen_clicks.insert(f[0].to_vec()) {
                continue;
            }
            if f[1].is_empty() || !channel.contains_key(f[2]) {
                continue;
            }
            let Some(ts) = timestamp(f[3]) else { continue };
            if ts < window_start - CLICK_REACH_MS || ts >= window_end {
                continue;
            }
            let person = people.person(f[1]);
            clicks_of.entry(person).or_default().push(Click { ts, id: f[0].to_vec(), campaign: f[2].to_vec() });
        }
    }

    // Sessions, credit, assists, and the two tables.
    let mut user_daily: BTreeMap<(Bytes, i64), UserDay> = BTreeMap::new();
    // campaign -> day -> [sessions, purchasing_sessions, revenue_cents, assisted_sessions]
    let mut campaign_daily: BTreeMap<(Bytes, i64), [i64; 4]> = BTreeMap::new();
    let no_clicks: Vec<Click> = Vec::new();
    for (person, mut events) in events_of {
        events.sort_by_key(|e| e.ts);
        let clicks = clicks_of.get(&person).unwrap_or(&no_clicks);
        let mut at = 0;
        while at < events.len() {
            let start = events[at].ts;
            let mut end = at + 1;
            while end < events.len() && events[end].ts - events[end - 1].ts <= SESSION_GAP_MS {
                end += 1;
            }
            let session = &events[at..end];
            let day = start.div_euclid(DAY_MS);
            let revenue: i64 = session.iter().filter(|e| e.purchase).map(|e| e.price).sum();
            let purchased = session.iter().any(|e| e.purchase);

            let row = user_daily.entry((person.clone(), day)).or_default();
            row.sessions += 1;
            row.events += session.len() as i64;
            row.revenue += revenue;
            for e in session.iter().filter(|e| e.purchase) {
                let name = category.get(&e.product).cloned().unwrap_or_else(|| b"unknown".to_vec());
                *row.categories.entry(name).or_default() += e.price;
            }

            let qualifying: Vec<&Click> =
                clicks.iter().filter(|c| c.ts <= start && start - c.ts <= CLICK_REACH_MS).collect();
            let credited: Bytes = qualifying
                .iter()
                .max_by(|a, b| a.ts.cmp(&b.ts).then_with(|| b.id.cmp(&a.id)))
                .map(|c| c.campaign.clone())
                .unwrap_or_else(|| b"organic".to_vec());
            let cell = campaign_daily.entry((credited, day)).or_default();
            cell[0] += 1;
            cell[1] += i64::from(purchased);
            cell[2] += revenue;
            let assisting: BTreeSet<&Bytes> = qualifying.iter().map(|c| &c.campaign).collect();
            for campaign in assisting {
                campaign_daily.entry((campaign.clone(), day)).or_default()[3] += 1;
            }
            at = end;
        }
    }

    fs::create_dir_all(out).expect("output folder");
    let date = |day: i64| {
        let (y, m, d) = civil_from_days(day);
        format!("{y:04}-{m:02}-{d:02}")
    };
    let mut users = Vec::new();
    users.extend_from_slice(b"user_id,day,sessions,events,revenue_cents,top_category\n");
    for ((person, day), row) in &user_daily {
        // BTreeMap order is byte-wise, so the first maximum is the byte-wise smallest.
        let mut top: Option<(&Bytes, i64)> = None;
        for (name, &sum) in &row.categories {
            if top.is_none_or(|(_, best)| sum > best) {
                top = Some((name, sum));
            }
        }
        users.extend_from_slice(person);
        write!(users, ",{},{},{},{},", date(*day), row.sessions, row.events, row.revenue).unwrap();
        if let Some((name, _)) = top {
            users.extend_from_slice(name);
        }
        users.push(b'\n');
    }
    fs::write(out.join("user_daily.csv"), users).expect("user_daily.csv");

    let mut campaigns = Vec::new();
    campaigns.extend_from_slice(b"campaign_id,channel,day,sessions,purchasing_sessions,revenue_cents,assisted_sessions\n");
    for ((campaign, day), cell) in &campaign_daily {
        campaigns.extend_from_slice(campaign);
        campaigns.push(b',');
        campaigns.extend_from_slice(channel.get(campaign).map_or(b"organic".as_slice(), |c| c.as_slice()));
        writeln!(campaigns, ",{},{},{},{},{}", date(*day), cell[0], cell[1], cell[2], cell[3]).unwrap();
    }
    fs::write(out.join("campaign_daily.csv"), campaigns).expect("campaign_daily.csv");
    eprintln!("reference: {} user rows, {} campaign rows", user_daily.len(), campaign_daily.len());
}
