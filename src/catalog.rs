//! The small inputs: manifest.json, products.csv and campaigns.csv.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use crate::key::{self, Key};
use crate::sys;
use crate::ts;

pub const NONE: u32 = u32::MAX;

/// A fixed open-addressing map from packed IDs to small numbers.
struct KeyMap {
    slots: Vec<(Key, u32)>,
    mask: usize,
    shift: u32,
}

impl KeyMap {
    fn new(entries: usize) -> Self {
        let capacity = (entries * 2).max(16).next_power_of_two();
        Self { slots: vec![(0, NONE); capacity], mask: capacity - 1, shift: 64 - capacity.trailing_zeros() }
    }

    fn insert(&mut self, id: Key, value: u32) {
        let mut at = (sys::mix(id) >> self.shift) as usize;
        while self.slots[at].0 != 0 {
            if self.slots[at].0 == id {
                return;
            }
            at = (at + 1) & self.mask;
        }
        self.slots[at] = (id, value);
    }

    #[inline(always)]
    fn get(&self, id: Key) -> u32 {
        let mut at = (sys::mix(id) >> self.shift) as usize;
        loop {
            let slot = unsafe { *self.slots.get_unchecked(at) };
            if slot.0 == id || slot.0 == 0 {
                return slot.1;
            }
            at = (at + 1) & self.mask;
        }
    }
}

/// IDs that pack go in the fast map; any other spelling in the exact one.
struct IdMap {
    packed: KeyMap,
    other: HashMap<Vec<u8>, u32>,
}

impl IdMap {
    fn new(entries: &[(Vec<u8>, u32)]) -> Self {
        let mut map = Self { packed: KeyMap::new(entries.len()), other: HashMap::new() };
        for (id, value) in entries {
            let packed = key::pack(id);
            if packed != 0 {
                map.packed.insert(packed, *value);
            } else {
                map.other.entry(id.clone()).or_insert(*value);
            }
        }
        map
    }

    /// Looks up an ID in the reader's buffer (16 readable bytes at `id`).
    #[inline(always)]
    unsafe fn get(&self, id: *const u8, len: usize) -> u32 {
        let packed = key::pack_padded(id, len);
        if packed != 0 {
            self.packed.get(packed)
        } else {
            self.get_other(std::slice::from_raw_parts(id, len))
        }
    }

    #[cold]
    #[inline(never)]
    fn get_other(&self, id: &[u8]) -> u32 {
        self.other.get(id).copied().unwrap_or(NONE)
    }
}

pub struct Catalog {
    pub window_start: i64,
    pub window_end: i64,
    products: IdMap,
    /// Category names in byte order, `unknown` among them: a category's number is its rank.
    pub categories: Vec<Vec<u8>>,
    unknown: u32,
    campaigns: IdMap,
    pub campaign_ids: Vec<Vec<u8>>,
    pub channels: Vec<Vec<u8>>,
}

impl Catalog {
    /// The category number of a product; `unknown`'s if the product is not in the file.
    #[inline(always)]
    pub unsafe fn category(&self, product: *const u8, len: usize) -> u32 {
        let found = self.products.get(product, len);
        if found == NONE {
            self.unknown
        } else {
            found
        }
    }

    /// The campaign's number, or NONE if it is not in campaigns.csv.
    #[inline(always)]
    pub unsafe fn campaign(&self, id: *const u8, len: usize) -> u32 {
        self.campaigns.get(id, len)
    }
}

/// The two fields of each data line of a two-column file, by the CSV dialect.
fn two_columns(content: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut rows = Vec::new();
    let mut lines = content.split(|&b| b == b'\n');
    lines.next(); // header
    for line in lines {
        let mut parts = line.split(|&b| b == b',');
        let (Some(a), Some(b), None) = (parts.next(), parts.next(), parts.next()) else { continue };
        let (Some(a), Some(b)) = (unquoted(a), unquoted(b)) else { continue };
        rows.push((a.to_vec(), b.to_vec()));
    }
    rows
}

fn unquoted(field: &[u8]) -> Option<&[u8]> {
    match field.iter().filter(|&&b| b == b'"').count() {
        0 => Some(field),
        2 if field.len() >= 2 && field[0] == b'"' && field[field.len() - 1] == b'"' => Some(&field[1..field.len() - 1]),
        _ => None,
    }
}

fn manifest_time(text: &[u8], name: &str) -> i64 {
    let needle = format!("\"{name}\"");
    let found = text.windows(needle.len()).position(|w| w == needle.as_bytes());
    let value = found.and_then(|at| {
        let rest = &text[at + needle.len()..];
        let open = rest.iter().position(|&b| b == b'"')? + 1;
        let close = open + rest[open..].iter().position(|&b| b == b'"')?;
        Some(&rest[open..close])
    });
    let parsed = value.map_or(ts::BAD, |v| unsafe { ts::parse_plain(v.as_ptr(), v.len()) });
    if parsed == ts::BAD {
        eprintln!("pipeline: manifest.json has no valid {name}");
        std::process::exit(2);
    }
    parsed
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| {
        eprintln!("pipeline: cannot read {}: {e}", path.display());
        std::process::exit(2);
    })
}

pub fn load(data: &Path) -> Catalog {
    let manifest = read(&data.join("manifest.json"));
    let window_start = manifest_time(&manifest, "window_start");
    let window_end = manifest_time(&manifest, "window_end");

    let products = two_columns(&read(&data.join("products.csv")));
    let mut names: BTreeSet<&[u8]> = products.iter().map(|(_, category)| category.as_slice()).collect();
    names.insert(b"unknown");
    let categories: Vec<Vec<u8>> = names.iter().map(|n| n.to_vec()).collect();
    let rank: HashMap<&[u8], u32> = names.iter().enumerate().map(|(i, n)| (*n, i as u32)).collect();
    let product_entries: Vec<(Vec<u8>, u32)> =
        products.iter().map(|(id, category)| (id.clone(), rank[category.as_slice()])).collect();
    let unknown = rank[b"unknown".as_slice()];

    let campaigns = two_columns(&read(&data.join("campaigns.csv")));
    let campaign_entries: Vec<(Vec<u8>, u32)> =
        campaigns.iter().enumerate().map(|(i, (id, _))| (id.clone(), i as u32)).collect();

    Catalog {
        window_start,
        window_end,
        products: IdMap::new(&product_entries),
        categories,
        unknown,
        campaigns: IdMap::new(&campaign_entries),
        campaign_ids: campaigns.iter().map(|(id, _)| id.clone()).collect(),
        channels: campaigns.iter().map(|(_, channel)| channel.clone()).collect(),
    }
}
