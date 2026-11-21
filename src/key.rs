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

/// Packs eight 6-bit codes, one per byte with the first character's in the
/// lowest byte, into 48 bits with the first character's highest.
#[inline(always)]
fn squeeze(codes: u64) -> u64 {
    #[cfg(all(target_arch = "x86_64", target_feature = "bmi2"))]
    unsafe {
        std::arch::x86_64::_pext_u64(codes.swap_bytes(), 0x3f3f_3f3f_3f3f_3f3f)
    }
    #[cfg(not(all(target_arch = "x86_64", target_feature = "bmi2")))]
    {
        let x = codes.swap_bytes();
        let x = ((x >> 2) & 0x0fc0_0fc0_0fc0_0fc0) | (x & 0x003f_003f_003f_003f);
        let x = ((x >> 4) & 0x00ff_f000_00ff_f000) | (x & 0x0000_0fff_0000_0fff);
        ((x >> 8) & 0x0000_ffff_ff00_0000) | (x & 0x0000_0000_00ff_ffff)
    }
}

/// `len` bytes of 0xff, then zeros: `LENGTH_MASKS[16 - len..]` keeps the first `len` bytes of 16.
#[cfg(any(all(target_arch = "x86_64", target_feature = "ssse3"), target_arch = "aarch64"))]
static LENGTH_MASKS: [u8; 32] = [
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];
// A byte is in the alphabet when its two nibbles agree on a class:
// bit 0: 0-9 (3x, x <= 9)     bit 1: A-O (4x, x >= 1)   bit 2: P-Z (5x, x <= 10)
// bit 3: _   (5f)             bit 4: a-o (6x, x >= 1)   bit 5: p-z (7x, x <= 10)
#[cfg(any(all(target_arch = "x86_64", target_feature = "ssse3"), target_arch = "aarch64"))]
static CLASS_OF_HIGH: [u8; 16] = [0, 0, 0, 1, 2, 4 | 8, 16, 32, 0, 0, 0, 0, 0, 0, 0, 0];
#[cfg(any(all(target_arch = "x86_64", target_feature = "ssse3"), target_arch = "aarch64"))]
static CLASS_OF_LOW: [u8; 16] = [
    1 | 4 | 32, 1 | 2 | 4 | 16 | 32, 1 | 2 | 4 | 16 | 32, 1 | 2 | 4 | 16 | 32, 1 | 2 | 4 | 16 | 32,
    1 | 2 | 4 | 16 | 32, 1 | 2 | 4 | 16 | 32, 1 | 2 | 4 | 16 | 32, 1 | 2 | 4 | 16 | 32, 1 | 2 | 4 | 16 | 32,
    2 | 4 | 16 | 32, 2 | 16, 2 | 16, 2 | 16, 2 | 16, 2 | 8 | 16,
];
/// What to add to a byte to get its code, by high nibble; `_` needs 4 less than the rest of its row.
#[cfg(any(all(target_arch = "x86_64", target_feature = "ssse3"), target_arch = "aarch64"))]
static CODE_OFFSET: [u8; 16] = [
    0, 0, 0, 0u8.wrapping_sub(47), 0u8.wrapping_sub(54), 0u8.wrapping_sub(54), 0u8.wrapping_sub(59),
    0u8.wrapping_sub(59), 0, 0, 0, 0, 0, 0, 0, 0,
];

/// `pack` for an ID in the reader's buffer: the 16 bytes at `id` must be readable,
/// whatever `len` is. Same result as `pack`, 16 bytes at a time.
#[cfg(all(target_arch = "x86_64", target_feature = "ssse3"))]
#[inline(always)]
pub unsafe fn pack_padded(id: *const u8, len: usize) -> Key {
    use std::arch::x86_64::*;
    if len == 0 || len > 10 {
        return 0;
    }
    let bytes = _mm_loadu_si128(id as *const __m128i);
    let nibble = _mm_set1_epi8(0x0f);
    let high = _mm_and_si128(_mm_srli_epi16(bytes, 4), nibble);
    let low = _mm_and_si128(bytes, nibble);
    let table = |t: &[u8; 16]| _mm_loadu_si128(t.as_ptr() as *const __m128i);
    let class = _mm_and_si128(_mm_shuffle_epi8(table(&CLASS_OF_HIGH), high), _mm_shuffle_epi8(table(&CLASS_OF_LOW), low));
    let keep = _mm_loadu_si128(LENGTH_MASKS.as_ptr().add(16 - len) as *const __m128i);
    let outside = _mm_and_si128(_mm_cmpeq_epi8(class, _mm_setzero_si128()), keep);
    if _mm_movemask_epi8(outside) != 0 {
        return 0;
    }
    let underscore = _mm_and_si128(_mm_cmpeq_epi8(bytes, _mm_set1_epi8(b'_' as i8)), _mm_set1_epi8(-4));
    let codes = _mm_add_epi8(_mm_add_epi8(bytes, _mm_shuffle_epi8(table(&CODE_OFFSET), high)), underscore);
    let codes = _mm_and_si128(codes, keep);
    let first = _mm_cvtsi128_si64(codes) as u64;
    let last = _mm_extract_epi16(codes, 4) as u64;
    (squeeze(first) << 16) | ((last & 0xff) << 10) | ((last >> 8) << 4)
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
pub unsafe fn pack_padded(id: *const u8, len: usize) -> Key {
    use std::arch::aarch64::*;
    if len == 0 || len > 10 {
        return 0;
    }
    let bytes = vld1q_u8(id);
    let high = vshrq_n_u8(bytes, 4);
    let low = vandq_u8(bytes, vdupq_n_u8(0x0f));
    let class = vandq_u8(vqtbl1q_u8(vld1q_u8(CLASS_OF_HIGH.as_ptr()), high), vqtbl1q_u8(vld1q_u8(CLASS_OF_LOW.as_ptr()), low));
    let keep = vld1q_u8(LENGTH_MASKS.as_ptr().add(16 - len));
    let outside = vandq_u8(vceqzq_u8(class), keep);
    if vmaxvq_u8(outside) != 0 {
        return 0;
    }
    let underscore = vandq_u8(vceqq_u8(bytes, vdupq_n_u8(b'_')), vdupq_n_u8(0u8.wrapping_sub(4)));
    let codes = vaddq_u8(vaddq_u8(bytes, vqtbl1q_u8(vld1q_u8(CODE_OFFSET.as_ptr()), high)), underscore);
    let codes = vandq_u8(codes, keep);
    let first = vgetq_lane_u64(vreinterpretq_u64_u8(codes), 0);
    let last = vgetq_lane_u64(vreinterpretq_u64_u8(codes), 1);
    (squeeze(first) << 16) | ((last & 0xff) << 10) | (((last >> 8) & 0xff) << 4)
}

#[cfg(not(any(all(target_arch = "x86_64", target_feature = "ssse3"), target_arch = "aarch64")))]
#[inline(always)]
pub unsafe fn pack_padded(id: *const u8, len: usize) -> Key {
    pack(std::slice::from_raw_parts(id, len))
}

/// `key_of` for a non-empty ID in the reader's buffer (16 readable bytes at `id`).
#[inline(always)]
pub unsafe fn key_of_padded(id: *const u8, len: usize) -> Key {
    let key = pack_padded(id, len);
    if key != 0 {
        key
    } else {
        intern(std::slice::from_raw_parts(id, len))
    }
}

/// Which bytes are outside the alphabet, the other way round from `CLASS_OF_*`:
/// a byte is outside when its two nibbles share a bit here, so "is any of
/// these bytes outside" is a single test of the two tables' AND.
// bit 0: 3x, x >= 10      bit 1: 4x or 6x, x = 0     bit 2: 5x, x = 11 to 14
// bit 3: 7x, x >= 11      bit 4: any other high nibble, whatever the low one
#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
static OUTSIDE_BY_HIGH: [u8; 16] = [16, 16, 16, 1, 2, 4, 2, 8, 16, 16, 16, 16, 16, 16, 16, 16];
#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
static OUTSIDE_BY_LOW: [u8; 16] = [
    2 | 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 1 | 16, 1 | 4 | 8 | 16, 1 | 4 | 8 | 16, 1 | 4 | 8 | 16, 1 | 4 | 8 | 16,
    1 | 8 | 16,
];

/// The packed keys of two IDs at once, if both pack: `pack_padded` of each, or
/// `None` if either is empty, too long or holds a character outside the
/// alphabet. 16 bytes must be readable at each, whatever the lengths.
///
/// The two share one 256-bit register, a half each, and every step
/// `pack_padded` takes is taken once for both. The 6-bit codes are then put
/// side by side with two multiply-adds (pairs, then fours: a code times 64
/// plus the next, a pair times 4096 plus the next) and one shuffle that lays
/// the three fours of ten characters end to end, where one ID alone goes
/// through `pext` and a handful of shifts. About 40 instructions for the two
/// where two calls of `pack_padded` take about 80 (counted in the compiled
/// code; BENCH.md, experiment 35).
#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
#[inline(always)]
unsafe fn pack_two_padded(a: *const u8, a_len: usize, b: *const u8, b_len: usize) -> Option<(Key, Key)> {
    use crate::sys::InPlace;
    use std::arch::x86_64::*;
    // Both 1 to 10 characters long. The larger of the two decides, and 0 - 1 is
    // the largest number there is. (Not the two ORed together: 7 | 8 is 15.)
    if a_len.wrapping_sub(1).max(b_len.wrapping_sub(1)) > 9 {
        return None;
    }
    let both = |first: *const u8, second: *const u8| {
        _mm256_inserti128_si256(_mm256_castsi128_si256(_mm_loadu_si128(first as *const __m128i)), _mm_loadu_si128(second as *const __m128i), 1)
    };
    let twice = |t: &[u8; 16]| _mm256_broadcastsi128_si256(_mm_loadu_si128(t.as_ptr() as *const __m128i));
    let bytes = both(a, b);
    let keep = both(LENGTH_MASKS.as_ptr().add(16 - a_len), LENGTH_MASKS.as_ptr().add(16 - b_len));
    let nibble = _mm256_set1_epi8(0x0f);
    let high = _mm256_and_si256(_mm256_srli_epi16(bytes, 4), nibble);
    let low = _mm256_and_si256(bytes, nibble);
    let outside = _mm256_and_si256(_mm256_shuffle_epi8(twice(&OUTSIDE_BY_HIGH), high), _mm256_shuffle_epi8(twice(&OUTSIDE_BY_LOW), low));
    if _mm256_testz_si256(outside, keep) == 0 {
        return None;
    }
    let underscore = _mm256_and_si256(_mm256_cmpeq_epi8(bytes, _mm256_set1_epi8(b'_' as i8)), _mm256_set1_epi8(-4));
    let codes = _mm256_add_epi8(_mm256_add_epi8(bytes, _mm256_shuffle_epi8(twice(&CODE_OFFSET), high)), underscore);
    let codes = _mm256_and_si256(codes, keep);
    // Characters 0 to 3, 4 to 7 and 8 to 11 of each ID as 24 bits each, the first character highest.
    // The three constants from here on are read where they are used (`sys::InPlace`):
    // written as values, each was fetched into a register on every line first.
    static PAIRS: InPlace = InPlace::words([0x0140; 16]);
    static FOURS: InPlace = InPlace::words([0x1000, 1, 0x1000, 1, 0x1000, 1, 0x1000, 1, 0x1000, 1, 0x1000, 1, 0x1000, 1, 0x1000, 1]);
    // The key is the first four's 24 bits, the second's, and the 12 bits of
    // characters 8 and 9, with four zero bits below: as bytes, lowest first,
    // the upper two of the third four, then all three of the second and of the first.
    static ORDER: InPlace = InPlace::twice([9, 10, 4, 5, 6, 0, 1, 2, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
    let pairs = _mm256_maddubs_epi16(codes, PAIRS.read());
    let fours = _mm256_madd_epi16(pairs, FOURS.read());
    let keys = _mm256_shuffle_epi8(fours, ORDER.read());
    Some((_mm256_extract_epi64(keys, 0) as u64, _mm256_extract_epi64(keys, 2) as u64))
}

/// The keys of the two IDs a line starts with: `key_of_padded` of `a`, which
/// must not be empty, and of `b`, or 0 for an empty `b`. Both must be in the
/// reader's buffer (16 readable bytes at each, whatever the lengths).
///
/// Nearly every line has two IDs that pack, and those are packed together
/// (`pack_two_padded`). A line with an ID that does not pack, or with an empty
/// one, takes both the slow way, one at a time.
#[inline(always)]
pub unsafe fn keys_of_two_padded(a: *const u8, a_len: usize, b: *const u8, b_len: usize) -> (Key, Key) {
    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
    if let Some(keys) = pack_two_padded(a, a_len, b, b_len) {
        return keys;
    }
    keys_of_two_apart(a, a_len, b, b_len)
}

/// `keys_of_two_padded` one ID at a time: what it is defined as.
#[cfg_attr(all(target_arch = "x86_64", target_feature = "avx2"), cold, inline(never))]
#[cfg_attr(not(all(target_arch = "x86_64", target_feature = "avx2")), inline(always))]
unsafe fn keys_of_two_apart(a: *const u8, a_len: usize, b: *const u8, b_len: usize) -> (Key, Key) {
    (key_of_padded(a, a_len), if b_len == 0 { 0 } else { key_of_padded(b, b_len) })
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
#[cfg(test)]
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

/// The characters of a packed key, written as 16 bytes at `out`: the ID, then
/// bytes that mean nothing.
///
/// `squeeze` backwards: the first eight characters' codes are spread one to a
/// byte, and all ten are turned into characters at once. The codes rise with
/// the characters (1 to 10 the digits, 11 to 36 the capitals, 37 `_`, 38 to 63
/// the small letters), so a code's character is the code plus 47, plus the gaps
/// of the alphabet it has passed.
#[cfg(all(target_arch = "x86_64", target_feature = "bmi2"))]
#[inline(always)]
unsafe fn spell(key: Key, out: *mut u8) {
    use std::arch::x86_64::*;
    let first = _pdep_u64(key >> 16, 0x3f3f_3f3f_3f3f_3f3f).swap_bytes();
    let last = ((key >> 10) & 63) | (((key >> 4) & 63) << 8);
    let codes = _mm_set_epi64x(last as i64, first as i64);
    let above = |code: i8| _mm_cmpgt_epi8(codes, _mm_set1_epi8(code));
    let mut text = _mm_add_epi8(codes, _mm_set1_epi8((b'0' - 1) as i8));
    text = _mm_add_epi8(text, _mm_and_si128(above(10), _mm_set1_epi8((b'A' - b'9' - 1) as i8)));
    text = _mm_add_epi8(text, _mm_and_si128(above(36), _mm_set1_epi8((b'_' - b'Z' - 1) as i8)));
    text = _mm_add_epi8(text, _mm_and_si128(above(37), _mm_set1_epi8((b'a' - b'_' - 1) as i8)));
    _mm_storeu_si128(out as *mut __m128i, text);
}

#[cfg(not(all(target_arch = "x86_64", target_feature = "bmi2")))]
#[inline(always)]
unsafe fn spell(key: Key, out: *mut u8) {
    let mut text = [0u8; 16];
    for (at, byte) in text.iter_mut().take(10).enumerate() {
        *byte = DECODE[((key >> (58 - 6 * at)) & 63) as usize];
    }
    (out as *mut [u8; 16]).write_unaligned(text);
}

/// Appends the ID a key stands for the way a PLAIN Parquet page holds a string:
/// its length in four bytes, then its characters. `write_id` with the length
/// in front, and for a packed key without a loop over the characters: a row of
/// user_daily is a handful of instructions to name.
#[inline(always)]
pub fn push_plain(key: Key, out: &mut Vec<u8>) {
    if !is_packed(key) {
        return push_plain_exotic(key, out);
    }
    // Twenty bytes are written whatever the length; the ones past it are overwritten by the next ID.
    if out.capacity() - out.len() < 20 {
        make_room(out);
    }
    // The last character holds the key's lowest set bit, and characters are six bits each from bit 63 down.
    let len = (69 - key.trailing_zeros() as usize) / 6;
    unsafe {
        let at = out.as_mut_ptr().add(out.len());
        (at as *mut u32).write_unaligned((len as u32).to_le());
        spell(key, at.add(4));
        out.set_len(out.len() + 4 + len);
    }
}

#[cold]
#[inline(never)]
fn make_room(out: &mut Vec<u8>) {
    out.reserve(20);
}

#[cold]
#[inline(never)]
fn push_plain_exotic(key: Key, out: &mut Vec<u8>) {
    let exotic = EXOTIC.lock().unwrap();
    let id = &exotic.ids[(key >> 4) as usize];
    out.extend_from_slice(&(id.len() as u32).to_le_bytes());
    out.extend_from_slice(id);
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
    fn padded_pack_is_pack() {
        let mut state = 99u64;
        let mut next = move || {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        };
        let alphabet = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ_abcdefghijklmnopqrstuvwxyz";
        let mut packed = 0;
        for round in 0..300_000 {
            let mut buffer = [0u8; 32];
            for byte in &mut buffer {
                let r = next();
                // Mostly the alphabet; sometimes its neighbours or anything at all.
                *byte = match r % 40 {
                    0 => b"/:@[^`{\x7f -\"\r\t,\n\0"[(r >> 8) as usize % 16],
                    1 => (r >> 8) as u8,
                    _ => alphabet[(r >> 8) as usize % alphabet.len()],
                };
            }
            let len = (next() % 14) as usize;
            let expected = pack(&buffer[..len]);
            assert_eq!(unsafe { pack_padded(buffer.as_ptr(), len) }, expected, "round {round}: {:?}", &buffer[..len]);
            packed += usize::from(expected != 0);
        }
        // Every single byte value, alone and after a valid character.
        for byte in 0..=255u8 {
            let mut buffer = [b'u'; 32];
            buffer[0] = byte;
            assert_eq!(unsafe { pack_padded(buffer.as_ptr(), 1) }, pack(&buffer[..1]), "byte {byte}");
            buffer[0] = b'u';
            buffer[9] = byte;
            assert_eq!(unsafe { pack_padded(buffer.as_ptr(), 10) }, pack(&buffer[..10]), "byte {byte} last");
        }
        assert!(packed > 100_000, "{packed} packed");
    }

    /// `keys_of_two_padded` against `key_of`, one ID at a time: any two IDs side
    /// by side, of any lengths, with any bytes after them; every byte value in
    /// every place of either; an empty second ID.
    #[test]
    fn two_keys_at_once_are_key_of_each() {
        let mut state = 1234u64;
        let mut next = move || {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        };
        let expected = |id: &[u8]| if id.is_empty() { 0 } else { key_of(id) };
        // Two IDs that both pack are packed together, and no others are: the
        // slow way gives the same keys, so only this tells the two ways apart.
        let together = |a: &[u8], b: &[u8]| {
            #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
            {
                let both = (pack(a), pack(b));
                let got = unsafe { pack_two_padded(a.as_ptr(), a.len(), b.as_ptr(), b.len()) };
                assert_eq!(got, (both.0 != 0 && both.1 != 0).then_some(both), "together: {a:?} and {b:?}");
            }
            let _ = (a, b);
        };
        let alphabet = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ_abcdefghijklmnopqrstuvwxyz";
        // Every pair of lengths, empty and too long among them, of IDs that are otherwise fine.
        for a_len in 0..=13usize {
            for b_len in 0..=13usize {
                let buffer = *b"u123456789abcdefghijklmnopqrstuv,Zyxwvutsrqponmlkjihgfedcba987654";
                let (a, b) = (&buffer[..a_len], &buffer[33..33 + b_len]);
                together(a, b);
                if a_len != 0 {
                    assert_eq!(unsafe { keys_of_two_padded(a.as_ptr(), a_len, b.as_ptr(), b_len) }, (expected(a), expected(b)), "lengths {a_len} and {b_len}");
                }
            }
        }
        let (mut both_packed, mut one_exotic) = (0, 0);
        for round in 0..400_000 {
            let mut buffer = [0u8; 64];
            // Mostly the alphabet, so that two IDs often both pack; sometimes its neighbours or anything at all.
            let dirt = [200, 40, 12][round % 3];
            for byte in &mut buffer {
                let r = next();
                *byte = match r % dirt {
                    0 => b"/:@[^`{\x7f -\"\r\t,\n\0"[(r >> 8) as usize % 16],
                    1 => (r >> 8) as u8,
                    _ => alphabet[(r >> 8) as usize % alphabet.len()],
                };
            }
            let (a_len, b_len) = (1 + (next() % 13) as usize, (next() % 14) as usize);
            // The second ID anywhere after the first: next to it, overlapping its unused bytes, or far off.
            let b_at = a_len + (next() % 30) as usize;
            let (a, b) = (&buffer[..a_len], &buffer[b_at..b_at + b_len]);
            let got = unsafe { keys_of_two_padded(a.as_ptr(), a_len, b.as_ptr(), b_len) };
            assert_eq!(got, (expected(a), expected(b)), "round {round}: {a:?} and {b:?}");
            together(a, b);
            both_packed += usize::from(is_packed(got.0) && got.1 != 0 && is_packed(got.1));
            one_exotic += usize::from(is_packed(got.0) != is_packed(got.1) && got.1 != 0);
        }
        assert!(both_packed > 50_000 && one_exotic > 50_000, "{both_packed} pairs packed, {one_exotic} with one exotic");
        // Every byte value in every place of an ID of every length, as the first and as the second.
        for len in 1..=11usize {
            for place in 0..len {
                for byte in 0..=255u8 {
                    let mut buffer = [b'u'; 64];
                    buffer[32..].fill(b'Z');
                    buffer[place] = byte;
                    let (a, b) = (&buffer[..len], &buffer[32..32 + len]);
                    assert_eq!(unsafe { keys_of_two_padded(a.as_ptr(), len, b.as_ptr(), len) }, (expected(a), expected(b)), "byte {byte} at {place} of {len}");
                    assert_eq!(unsafe { keys_of_two_padded(b.as_ptr(), len, a.as_ptr(), len) }, (expected(b), expected(a)), "byte {byte} at {place} of {len}, second");
                    together(a, b);
                    together(b, a);
                    // The bytes after an ID are not part of it, whatever they are.
                    buffer[place] = b'u';
                    buffer[len] = byte;
                    buffer[32 + len] = byte;
                    let (a, b) = (&buffer[..len], &buffer[32..32 + len]);
                    assert_eq!(unsafe { keys_of_two_padded(a.as_ptr(), len, b.as_ptr(), len) }, (expected(a), expected(b)), "byte {byte} after {len}");
                    together(a, b);
                }
            }
        }
        // Every pair of characters next to each other, in every pair of places: the codes do not run into one another.
        for &first in alphabet {
            for &second in alphabet {
                for place in 0..9usize {
                    let mut buffer = [b'0'; 64];
                    buffer[place] = first;
                    buffer[place + 1] = second;
                    buffer[32 + place] = second;
                    buffer[33 + place] = first;
                    let (a, b) = (&buffer[..10], &buffer[32..42]);
                    assert_eq!(unsafe { keys_of_two_padded(a.as_ptr(), 10, b.as_ptr(), 10) }, (pack(a), pack(b)));
                }
            }
        }
    }

    /// `push_plain` against `write_id`: every length, every character in every
    /// place, exotic IDs in between, and a buffer that starts with no room.
    #[test]
    fn plain_spelling_is_write_id() {
        let mut state = 7u64;
        let mut next = move || {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        };
        let alphabet = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ_abcdefghijklmnopqrstuvwxyz";
        let mut ids: Vec<Vec<u8>> = Vec::new();
        for &character in alphabet {
            for len in 1..=10 {
                for place in 0..len {
                    let mut id = vec![b'u'; len];
                    id[place] = character;
                    ids.push(id);
                }
                ids.push(vec![character; len]);
            }
        }
        for round in 0..200_000 {
            let len = 1 + (next() % 10) as usize;
            let mut id: Vec<u8> = (0..len).map(|_| alphabet[(next() >> 8) as usize % alphabet.len()]).collect();
            match round % 500 {
                0 => id.extend_from_slice(b"_longer_than_ten"),
                1 => id[0] = b' ',
                2 => id = b"\xd9\xa2".to_vec(),
                _ => {}
            }
            ids.push(id);
        }
        let (mut got, mut expected) = (Vec::new(), Vec::new());
        let mut packed = 0;
        for id in &ids {
            let key = key_of(id);
            packed += usize::from(is_packed(key));
            push_plain(key, &mut got);
            let at = expected.len();
            expected.extend_from_slice(&[0; 4]);
            write_id(key, &mut expected);
            let len = (expected.len() - at - 4) as u32;
            expected[at..at + 4].copy_from_slice(&len.to_le_bytes());
            assert_eq!(&expected[at + 4..], &id[..], "write_id of {id:?}");
            assert_eq!(got.len(), expected.len(), "after {id:?}");
        }
        assert!(got == expected);
        assert!(packed > 200_000 && packed < ids.len(), "{packed} packed of {}", ids.len());
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
