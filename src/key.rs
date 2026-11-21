//! IDs as 64-bit keys.
//!
//! An ID of up to 10 characters from `[0-9A-Z_a-z]` packs exactly into a u64,
//! 6 bits a character, first character highest. The codes rise with the ASCII
//! values and 0 is the padding, so comparing two packed keys as integers is
//! comparing the IDs byte by byte: the order rules.md asks for when it names a
//! person or breaks a tie between clicks.
//!
//! Every ID the generator writes fits. Anything else (longer, or holding a
//! space, a tab, a `\r`, non-ASCII) is "exotic": interned in a table and given
//! a key with the low tag bit set. That path is slow and exact; it exists so
//! the rules hold for any input, not because scored data takes it.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

pub type Key = u64;

const fn encode_table() -> [u8; 256] {
    let mut table = [0u8; 256];
    let mut code = 1u8;
    let mut c = b'0';
    while c <= b'9' {
        table[c as usize] = code;
        code += 1;
        c += 1;
    }
    c = b'A';
    while c <= b'Z' {
        table[c as usize] = code;
        code += 1;
        c += 1;
    }
    table[b'_' as usize] = code;
    code += 1;
    c = b'a';
    while c <= b'z' {
        table[c as usize] = code;
        code += 1;
        c += 1;
    }
    table
}

const fn decode_table() -> [u8; 64] {
    let encode = encode_table();
    let mut table = [0u8; 64];
    let mut c = 0;
    while c < 256 {
        if encode[c] != 0 {
            table[encode[c] as usize] = c as u8;
        }
        c += 1;
    }
    table
}

static ENCODE: [u8; 256] = encode_table();
static DECODE: [u8; 64] = decode_table();

struct Exotic {
    index: HashMap<Vec<u8>, u32>,
    ids: Vec<Vec<u8>>,
}

static EXOTIC: LazyLock<Mutex<Exotic>> =
    LazyLock::new(|| Mutex::new(Exotic { index: HashMap::new(), ids: Vec::new() }));

/// The packed key of `id`, or 0 if it does not pack (empty, too long, or a character outside the alphabet).
#[inline(always)]
pub fn pack(id: &[u8]) -> Key {
    let len = id.len();
    if len == 0 || len > 10 {
        return 0;
    }
    let mut key = 0u64;
    let mut ok = true;
    for &byte in id {
        let code = ENCODE[byte as usize] as u64;
        ok &= code != 0;
        key = (key << 6) | code;
    }
    if ok {
        key << (64 - 6 * len as u32)
    } else {
        0
    }
}

#[cold]
#[inline(never)]
fn intern(id: &[u8]) -> Key {
    let mut exotic = EXOTIC.lock().unwrap();
    let at = match exotic.index.get(id) {
        Some(&at) => at,
        None => {
            let at = exotic.ids.len() as u32;
            exotic.index.insert(id.to_vec(), at);
            exotic.ids.push(id.to_vec());
            at
        }
    };
    (u64::from(at) << 4) | 1
}

/// The key of a non-empty ID. Equal IDs get equal keys and different IDs different keys.
#[inline(always)]
pub fn key_of(id: &[u8]) -> Key {
    let key = pack(id);
    if key != 0 {
        key
    } else {
        intern(id)
    }
}

#[inline(always)]
fn is_packed(key: Key) -> bool {
    key & 15 == 0
}

/// Appends the ID a key stands for.
#[inline]
pub fn write_id(key: Key, out: &mut Vec<u8>) {
    if is_packed(key) {
        let mut rest = key;
        while rest != 0 {
            out.push(DECODE[(rest >> 58) as usize]);
            rest <<= 6;
        }
    } else {
        out.extend_from_slice(&EXOTIC.lock().unwrap().ids[(key >> 4) as usize]);
    }
}

#[cold]
#[inline(never)]
fn compare_slow(a: Key, b: Key) -> Ordering {
    let (mut x, mut y) = (Vec::new(), Vec::new());
    write_id(a, &mut x);
    write_id(b, &mut y);
    x.cmp(&y)
}

/// Byte-wise order of the two IDs.
#[inline(always)]
pub fn compare(a: Key, b: Key) -> Ordering {
    if is_packed(a | b) {
        a.cmp(&b)
    } else {
        compare_slow(a, b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_order_is_byte_order() {
        let ids: [&[u8]; 14] = [
            b"0", b"9", b"A", b"U1", b"Z", b"_", b"a", b"u", b"u1", b"u10", b"u1a", b"u2", b"uzzzzzzzzz", b"z",
        ];
        for a in ids {
            for b in ids {
                assert_eq!(compare(key_of(a), key_of(b)), a.cmp(b), "{a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn exotic_ids_round_trip_and_order() {
        let ids: [&[u8]; 9] = [b" u1", b"u1 ", b"u1", b"A\tB", b"user_id_a", b"e3\r", b"u1234567890", b"\xd9\xa2", b"u"];
        for a in ids {
            let mut back = Vec::new();
            write_id(key_of(a), &mut back);
            assert_eq!(back, a);
            assert_eq!(key_of(a), key_of(a));
            for b in ids {
                assert_eq!(compare(key_of(a), key_of(b)), a.cmp(b), "{a:?} vs {b:?}");
                assert_eq!(key_of(a) == key_of(b), a == b);
            }
        }
    }
}
