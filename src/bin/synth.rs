//! A stand-in dataset for benchmarks where the organizers' generator is not at
//! hand (CI). It is NOT the generator and proves nothing about correctness on
//! its own: it writes data with the same shapes (ID lengths, the three
//! timestamp forms, quoting, duplicates, linked IDs, shuffled part files) so
//! the pipeline does the same kind of work per line. The reference still has to
//! agree with the pipeline on it, which is one more cross-check.
//!
//! Usage: synth --events E --clicks C --seed S --output DIR

use std::fs;
use std::io::Write;
use std::path::Path;

const DAY: i64 = 86_400_000;
const WINDOW_DAYS: i64 = 14;
const SLOTS: usize = 168; // two-hour slots in the window

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, bound: u64) -> u64 {
        ((u128::from(self.next()) * u128::from(bound)) >> 64) as u64
    }
    fn between(&mut self, low: i64, high: i64) -> i64 {
        low + self.below((high - low + 1) as u64) as i64
    }
    fn chance(&mut self, p: f64) -> bool {
        ((self.next() >> 11) as f64 / (1u64 << 53) as f64) < p
    }
}

fn scramble(value: u64, key: u64) -> u64 {
    const MASK: u64 = (1 << 40) - 1;
    let mut mixed = (value.wrapping_mul(0x9E37_79B9_7F4B) ^ key) & MASK;
    mixed ^= mixed >> 17;
    mixed.wrapping_mul(0xD6E8_FEB8_6659) & MASK
}

fn base36(out: &mut Vec<u8>, mut value: u64) {
    let mut buffer = [0u8; 13];
    let mut at = buffer.len();
    loop {
        at -= 1;
        buffer[at] = b"0123456789abcdefghijklmnopqrstuvwxyz"[(value % 36) as usize];
        value /= 36;
        if value == 0 {
            break;
        }
    }
    out.extend_from_slice(&buffer[at..]);
}

fn civil(ms: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = ms.div_euclid(DAY);
    let seconds = ms.rem_euclid(DAY) / 1000;
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let doe = shifted - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, doy - (153 * mp + 2) / 5 + 1, seconds / 3600, seconds / 60 % 60, seconds % 60)
}

fn write_utc(out: &mut Vec<u8>, ms: i64) {
    let (y, mo, d, h, mi, s) = civil(ms);
    write!(out, "{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z").unwrap();
}

fn write_ts(out: &mut Vec<u8>, rng: &mut Rng, ms: i64) {
    let draw = rng.below(10);
    if ms % 1000 != 0 || draw >= 7 {
        write!(out, "{ms}").unwrap();
    } else if draw == 6 {
        let offset = [-600i64, -300, -240, 60, 120, 330, 540, 720][rng.below(8) as usize];
        let (y, mo, d, h, mi, s) = civil(ms + offset * 60_000);
        let sign = if offset < 0 { '-' } else { '+' };
        write!(out, "{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}{sign}{:02}:{:02}", offset.abs() / 60, offset.abs() % 60).unwrap();
    } else {
        write_utc(out, ms);
    }
}

/// One folder's part files: lines are collected per file, then shuffled and written.
struct Parts {
    bytes: Vec<Vec<u8>>,
    starts: Vec<Vec<u32>>,
    shares: Vec<f64>,
}

impl Parts {
    fn new(rng: &mut Rng, count: usize) -> Self {
        // Zipf sizes in a shuffled order, two files left with only a header.
        let mut weights: Vec<f64> = (0..count).map(|rank| if rank + 2 >= count { 0.0 } else { 1.0 / (rank + 1) as f64 }).collect();
        for last in (1..count).rev() {
            weights.swap(last, rng.below(last as u64 + 1) as usize);
        }
        let total: f64 = weights.iter().sum();
        let mut running = 0.0;
        let shares = weights.iter().map(|w| { running += w / total; running }).collect();
        Self { bytes: vec![Vec::new(); count], starts: vec![Vec::new(); count], shares }
    }

    fn push(&mut self, rng: &mut Rng, fields: &[Vec<u8>]) {
        let draw = (rng.next() >> 11) as f64 / (1u64 << 53) as f64;
        let part = self.shares.partition_point(|&bound| bound <= draw).min(self.shares.len() - 1);
        self.starts[part].push(self.bytes[part].len() as u32);
        let line = &mut self.bytes[part];
        for (i, field) in fields.iter().enumerate() {
            if i > 0 {
                line.push(b',');
            }
            let quote = rng.chance(0.2);
            if quote {
                line.push(b'"');
            }
            line.extend_from_slice(field);
            if quote {
                line.push(b'"');
            }
        }
        if rng.chance(0.0005) {
            line.extend_from_slice(b",x"); // a malformed line
        }
        line.push(b'\n');
    }

    fn write(&mut self, rng: &mut Rng, folder: &Path, header: &str) {
        fs::create_dir_all(folder).unwrap();
        for part in 0..self.bytes.len() {
            let bytes = &self.bytes[part];
            let starts = &self.starts[part];
            let mut order: Vec<u32> = (0..starts.len() as u32).collect();
            for last in (1..order.len()).rev() {
                order.swap(last, rng.below(last as u64 + 1) as usize);
            }
            let mut out = Vec::with_capacity(bytes.len() + header.len() + 1);
            out.extend_from_slice(header.as_bytes());
            out.push(b'\n');
            for &line in &order {
                let from = starts[line as usize] as usize;
                let to = starts.get(line as usize + 1).map_or(bytes.len(), |&next| next as usize);
                out.extend_from_slice(&bytes[from..to]);
            }
            fs::write(folder.join(format!("part-{part:05}.csv")), out).unwrap();
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let value = |name: &str| args.iter().position(|a| a == name).and_then(|at| args.get(at + 1)).cloned();
    let (Some(events), Some(clicks), Some(output)) = (value("--events"), value("--clicks"), value("--output")) else {
        eprintln!("usage: synth --events E --clicks C [--seed S] --output DIR");
        std::process::exit(2);
    };
    let events: u64 = events.parse().unwrap();
    let clicks: u64 = clicks.parse().unwrap();
    let seed: u64 = value("--seed").map_or(1, |s| s.parse().unwrap());
    let output = Path::new(&output);
    let mut rng = Rng(seed.wrapping_mul(0x2545_F491_4F6C_DD1D));
    let window_start = (20_727 + rng.below(700) as i64) * DAY; // a midnight from late 2026 on
    let (user_key, event_key, click_key) = (rng.next(), rng.next(), rng.next());

    let users = (events / 30).max(20) as usize;
    // Zipf-like activity, and five whales with 1.5% of the events each.
    let whale = events * 15 / 1000;
    let mut weight_total = 0.0;
    let weights: Vec<f64> = (0..users).map(|rank| { let w = ((rank + 10) as f64).powf(-0.75); weight_total += w; w }).collect();
    let rest = events - whale * 5;
    let mut event_parts = Parts::new(&mut rng, 64);
    let mut click_parts = Parts::new(&mut rng, 24);
    let mut link_parts = Parts::new(&mut rng, 12);
    let clicks_per_session = clicks as f64 / (events as f64 / 5.5 * 0.65);
    let kiosks: Vec<Vec<u8>> = (0..4).map(|k| { let mut id = b"u".to_vec(); base36(&mut id, scramble((1 << 39) + k, user_key)); id }).collect();

    let (mut event_index, mut click_index) = (0u64, 0u64);
    let mut running = 0.0;
    let mut given = 0u64;
    let mut fields: Vec<Vec<u8>> = vec![Vec::new(); 7];
    for user in 0..users {
        let count = if user < 5 {
            whale
        } else {
            running += weights[user] / weight_total;
            let through = ((running * rest as f64) as u64).min(rest);
            let n = through - given;
            given = through;
            n
        };
        if count == 0 {
            continue;
        }
        let aliases = match rng.below(100) {
            0..45 => 1,
            45..75 => 2,
            75..88 => 3,
            _ => rng.between(4, 8),
        } as usize;
        let ids: Vec<Vec<u8>> = (0..aliases).map(|a| { let mut id = b"u".to_vec(); base36(&mut id, scramble(user as u64 * 512 + a as u64, user_key)); id }).collect();
        for alias in 1..aliases {
            let earlier = rng.below(alias as u64) as usize;
            for _ in 0..1 + usize::from(rng.chance(0.08)) {
                let (a, b) = if rng.chance(0.5) { (alias, earlier) } else { (earlier, alias) };
                link_parts.push(&mut rng, &[ids[a].clone(), ids[b].clone()]);
            }
        }
        if rng.chance(0.004) {
            let kiosk = kiosks[rng.below(4) as usize].clone();
            let mine = ids[rng.below(aliases as u64) as usize].clone();
            link_parts.push(&mut rng, &[mine, kiosk]);
        }

        // Sessions in their own two-hour slots, events seconds to minutes apart.
        let sessions = ((count + 2) / 5).clamp(1, SLOTS as u64) as usize;
        let mut used = [false; SLOTS];
        let mut starts = Vec::with_capacity(sessions);
        let clicker = user < 5 || rng.chance(0.65);
        let mut left = count;
        for session in 0..sessions {
            let slot = loop {
                let slot = rng.below(SLOTS as u64) as usize;
                if !used[slot] {
                    used[slot] = true;
                    break slot;
                }
            };
            let size = if session + 1 == sessions { left } else { (count / sessions as u64).max(1).min(left) };
            left -= size;
            let mut ts = window_start + slot as i64 * 2 * 3_600_000 + rng.below(25 * 60) as i64 * 1000
                + if rng.chance(0.03) { rng.between(1, 999) } else { 0 };
            starts.push(ts);
            let gap_limit = (29 * 60 / size.max(1) as i64).clamp(1, 120);
            for _ in 0..size {
                let alias = if aliases == 1 || rng.chance(0.5) { 0 } else { rng.below(aliases as u64) as usize };
                fields[0].clear();
                fields[0].push(b'e');
                base36(&mut fields[0], scramble(event_index, event_key));
                event_index += 1;
                fields[1].clear();
                if !rng.chance(0.002) {
                    fields[1].extend_from_slice(&ids[alias]);
                }
                fields[2].clear();
                if rng.chance(0.005) {
                    match rng.below(4) {
                        0 => {}
                        1 => write!(fields[2], "{ts}.0").unwrap(),
                        2 => write_utc(&mut fields[2], window_start - 1000),
                        _ => { write_utc(&mut fields[2], ts); fields[2].pop(); }
                    }
                } else {
                    write_ts(&mut fields[2], &mut rng, ts);
                }
                let ceiling = ts + (1000 - ts.rem_euclid(1000)) % 1000;
                let ingest = ceiling + if rng.chance(0.01) { rng.between(86_401, 400_000) } else { rng.between(0, 600) } * 1000;
                fields[3].clear();
                write_utc(&mut fields[3], ingest);
                let kind = rng.below(100);
                fields[4].clear();
                fields[4].extend_from_slice(if rng.chance(0.001) { b"View".as_slice() } else if kind < 3 { b"purchase" } else if kind < 13 { b"cart" } else { b"view" });
                let r = (rng.next() >> 11) as f64 / (1u64 << 53) as f64;
                let product = if rng.chance(0.005) { 50_001 + rng.below(40_000) } else { 1 + (r * r * 50_000.0) as u64 };
                fields[5].clear();
                write!(fields[5], "p{product}").unwrap();
                let price = 100 + product * 7919 % 49_900;
                fields[6].clear();
                if kind < 3 {
                    if rng.chance(0.05) { fields[6].extend_from_slice(b"19.99"); } else { write!(fields[6], "{price}").unwrap(); }
                } else if rng.below(20) >= 14 {
                    write!(fields[6], "{price}").unwrap();
                }
                event_parts.push(&mut rng, &fields);
                if rng.chance(0.03) {
                    for _ in 0..1 + rng.below(3) {
                        fields[3].clear();
                        write_utc(&mut fields[3], ingest + rng.between(1, 600) * 1000);
                        event_parts.push(&mut rng, &fields);
                    }
                }
                ts += rng.between(1, gap_limit) * 1000;
            }
            if left == 0 {
                break;
            }
        }
        if clicker {
            let expected = starts.len() as f64 * clicks_per_session;
            let mut n = expected as u64 + u64::from(rng.chance(expected.fract()));
            if user < 5 {
                n = clicks / 100;
            }
            for _ in 0..n {
                let start = starts[rng.below(starts.len() as u64) as usize];
                let reach = [600, 3_600, 86_400, 7 * 86_400, 8 * 86_400][rng.below(5) as usize];
                let ts = (start - rng.below(reach) as i64 * 1000).max(window_start - 7 * DAY);
                let alias = if aliases == 1 || rng.chance(0.5) { 0 } else { rng.below(aliases as u64) as usize };
                let mut row: Vec<Vec<u8>> = vec![Vec::new(); 4];
                row[0].push(b'k');
                base36(&mut row[0], scramble(click_index, click_key));
                click_index += 1;
                row[1].extend_from_slice(&ids[alias]);
                let campaign = if rng.chance(0.3) { 1 + rng.below(4) } else if rng.chance(0.005) { 2_001 + rng.below(100) } else {
                    let r = (rng.next() >> 11) as f64 / (1u64 << 53) as f64;
                    1 + (r * r * r * 2_000.0) as u64
                };
                write!(row[2], "c_{campaign}").unwrap();
                write_ts(&mut row[3], &mut rng, ts);
                for _ in 0..1 + usize::from(rng.chance(0.02)) {
                    click_parts.push(&mut rng, &row);
                }
            }
        }
    }

    fs::create_dir_all(output).unwrap();
    event_parts.write(&mut rng, &output.join("events"), "event_id,user_id,ts,ingest_ts,event_type,product_id,price_cents");
    click_parts.write(&mut rng, &output.join("clicks"), "click_id,user_id,campaign_id,ts");
    link_parts.write(&mut rng, &output.join("links"), "user_id_a,user_id_b");
    const CATEGORIES: [&str; 20] = [
        "apparel", "automotive", "baby", "beauty", "books", "electronics", "garden", "grocery", "home", "jewelry",
        "kitchen", "music", "office", "outdoors", "pets", "shoes", "sports", "tools", "toys", "video_games",
    ];
    let mut products = b"product_id,category\n".to_vec();
    for product in 1..=50_000u64 {
        writeln!(products, "p{product},{}", CATEGORIES[rng.below(20) as usize]).unwrap();
    }
    fs::write(output.join("products.csv"), products).unwrap();
    let mut campaigns = b"campaign_id,channel\n".to_vec();
    for campaign in 1..=2_000u64 {
        writeln!(campaigns, "c_{campaign},{}", ["affiliate", "display", "email", "search", "social"][rng.below(5) as usize]).unwrap();
    }
    fs::write(output.join("campaigns.csv"), campaigns).unwrap();
    let (mut start, mut end) = (Vec::new(), Vec::new());
    write_utc(&mut start, window_start);
    write_utc(&mut end, window_start + WINDOW_DAYS * DAY);
    let manifest = format!(
        "{{\n  \"window_start\": \"{}\",\n  \"window_end\": \"{}\"\n}}\n",
        String::from_utf8(start).unwrap(),
        String::from_utf8(end).unwrap()
    );
    fs::write(output.join("manifest.json"), manifest).unwrap();
    eprintln!("synth: {event_index} events, {click_index} clicks, {users} users");
}
