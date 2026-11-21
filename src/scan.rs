//! Stage one of the CSV reader: where the delimiters and the quotes are.
//!
//! rules.md makes this exact and simple. Only `\n` ends a line and every comma
//! separates fields, quoted or not, so both can be found with byte compares, 64
//! bytes at a time, with no state carried between blocks. Commas and newlines go
//! into one list: which of the two a delimiter is, stage two reads from the byte
//! itself, which it has in hand anyway. Quotes are only counted: a line is well
//! formed when its quotes are exactly the ones wrapping whole fields, which
//! stage two checks from the count.

/// Positions found in one buffer.
pub struct Index {
    /// One slot of `u32::MAX`, then the offsets of every `,` and `\n`, ascending,
    /// then 8 sentinels of `u32::MAX`. The slot in front stands for a delimiter at
    /// offset -1, so the line at offset 0 has one before it like every other line.
    delims: Vec<u32>,
    /// Quotes before each 64-byte block, and each block's quote bitmap.
    quotes_before: Vec<u32>,
    quote_bits: Vec<u64>,
}

impl Index {
    pub fn new(capacity: usize) -> Self {
        Self {
            delims: vec![u32::MAX; capacity + 81],
            quotes_before: vec![0; capacity / 64 + 3],
            quote_bits: vec![0; capacity / 64 + 3],
        }
    }

    pub fn capacity(&self) -> usize {
        self.delims.len() - 81
    }

    /// The first delimiter's slot. The slot before it is readable and holds `u32::MAX`.
    #[inline(always)]
    pub fn delims(&self) -> *const u32 {
        unsafe { self.delims.as_ptr().add(1) }
    }

    /// How many `"` lie before offset `at`.
    #[inline(always)]
    pub fn quotes_before(&self, at: usize) -> u32 {
        unsafe {
            let block = at >> 6;
            let below = (1u64 << (at & 63)) - 1;
            *self.quotes_before.get_unchecked(block) + (*self.quote_bits.get_unchecked(block) & below).count_ones()
        }
    }

    /// Indexes `len` bytes at `data`. The 64 bytes after them must be readable.
    pub unsafe fn build(&mut self, data: *const u8, len: usize) {
        self.build_with(chosen(), data, len)
    }

    /// `build` by one particular loop. Both leave the same index; the wide one
    /// may only be asked for on a CPU that has its instructions (`chosen` and
    /// `available` never name it on one that has not).
    pub unsafe fn build_with(&mut self, level: Level, data: *const u8, len: usize) {
        debug_assert!(len <= self.capacity());
        match level {
            #[cfg(target_arch = "x86_64")]
            Level::Wide => wide::build(self, data, len),
            _ => self.build_narrow(data, len),
        }
    }

    /// The loop every CPU this is built for can run: 64 bytes in two halves (or
    /// four quarters), the offsets taken out of the bitmap one bit at a time.
    unsafe fn build_narrow(&mut self, data: *const u8, len: usize) {
        let mut delim_out = self.delims.as_mut_ptr().add(1);
        let whole = len / 64;
        let mut quotes = 0u32;
        for block in 0..whole {
            let (delims, quote_bits) = masks(data.add(block * 64));
            *self.quotes_before.get_unchecked_mut(block) = quotes;
            *self.quote_bits.get_unchecked_mut(block) = quote_bits;
            quotes += quote_bits.count_ones();
            delim_out = flatten::<8>(delims, (block * 64) as u32, delim_out);
        }
        // The bytes past `len` in a last, partial block are not data. Outside the
        // loop, so that the whole blocks are not each asked whether they are it.
        let mut blocks = whole;
        if len & 63 != 0 {
            let (delims, quote_bits) = masks(data.add(whole * 64));
            let keep = (1u64 << (len & 63)) - 1;
            let quote_bits = quote_bits & keep;
            *self.quotes_before.get_unchecked_mut(whole) = quotes;
            *self.quote_bits.get_unchecked_mut(whole) = quote_bits;
            quotes += quote_bits.count_ones();
            delim_out = flatten::<8>(delims & keep, (whole * 64) as u32, delim_out);
            blocks += 1;
        }
        self.finish(blocks, quotes, delim_out);
    }

    /// What every loop ends with: the quote count after the last block, so that
    /// `quotes_before(len)` works when `len` is a multiple of 64, and the sentinels.
    #[inline(always)]
    unsafe fn finish(&mut self, blocks: usize, quotes: u32, delim_out: *mut u32) {
        *self.quotes_before.get_unchecked_mut(blocks) = quotes;
        *self.quote_bits.get_unchecked_mut(blocks) = 0;
        for i in 0..8 {
            *delim_out.add(i) = u32::MAX;
        }
    }
}

/// Which loop builds the index.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
pub enum Level {
    /// What the build's own target allows (AVX2 on the scorer's build, NEON on ARM).
    Narrow,
    /// AVX-512 instructions, on 256-bit registers.
    Wide,
}

impl Level {
    pub fn name(self) -> &'static str {
        match self {
            Level::Narrow => "narrow",
            Level::Wide => "avx512",
        }
    }
}

/// The loops this CPU can run, `Narrow` first.
#[cfg(test)]
pub fn available() -> Vec<Level> {
    #[allow(unused_mut)]
    let mut levels = vec![Level::Narrow];
    #[cfg(target_arch = "x86_64")]
    if wide::supported() {
        levels.push(Level::Wide);
    }
    levels
}

/// The loop `build` uses: decided once, from the CPU the process finds itself
/// on. The wide one wherever it runs, unless `SS_SCAN=narrow` asks for the
/// other (for measuring one against the other on one machine).
#[inline(always)]
pub fn chosen() -> Level {
    #[cfg(target_arch = "x86_64")]
    {
        wide::chosen()
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        Level::Narrow
    }
}

/// Writes the offsets of the set bits. The first `N` are written without a
/// branch (the bits past the last are garbage that the next call overwrites),
/// so the only branch is the rare block with more than `N`.
#[inline(always)]
unsafe fn flatten<const N: usize>(mut bits: u64, base: u32, out: *mut u32) -> *mut u32 {
    let count = bits.count_ones() as usize;
    for i in 0..N {
        *out.add(i) = base + bits.trailing_zeros();
        bits &= bits.wrapping_sub(1);
    }
    if count > N {
        let mut at = N;
        while bits != 0 {
            *out.add(at) = base + bits.trailing_zeros();
            bits &= bits - 1;
            at += 1;
        }
    }
    out.add(count)
}

/// Bitmaps of the delimiters (`,` and `\n` together) and of `"` in the 64 bytes at `p`.
#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
#[inline(always)]
unsafe fn masks(p: *const u8) -> (u64, u64) {
    use std::arch::x86_64::*;
    let low = _mm256_loadu_si256(p as *const __m256i);
    let high = _mm256_loadu_si256(p.add(32) as *const __m256i);
    let (comma, newline, quote) = (_mm256_set1_epi8(b',' as i8), _mm256_set1_epi8(b'\n' as i8), _mm256_set1_epi8(b'"' as i8));
    let bits = |a: __m256i, b: __m256i| -> u64 {
        u64::from(_mm256_movemask_epi8(a) as u32) | (u64::from(_mm256_movemask_epi8(b) as u32) << 32)
    };
    // Either delimiter: the two compares are joined before they leave the vector.
    let delims = bits(
        _mm256_or_si256(_mm256_cmpeq_epi8(low, comma), _mm256_cmpeq_epi8(low, newline)),
        _mm256_or_si256(_mm256_cmpeq_epi8(high, comma), _mm256_cmpeq_epi8(high, newline)),
    );
    (delims, bits(_mm256_cmpeq_epi8(low, quote), _mm256_cmpeq_epi8(high, quote)))
}

#[cfg(all(target_arch = "x86_64", not(target_feature = "avx2")))]
#[inline(always)]
unsafe fn masks(p: *const u8) -> (u64, u64) {
    use std::arch::x86_64::*;
    let v = [
        _mm_loadu_si128(p as *const __m128i),
        _mm_loadu_si128(p.add(16) as *const __m128i),
        _mm_loadu_si128(p.add(32) as *const __m128i),
        _mm_loadu_si128(p.add(48) as *const __m128i),
    ];
    let (comma, newline, quote) = (_mm_set1_epi8(b',' as i8), _mm_set1_epi8(b'\n' as i8), _mm_set1_epi8(b'"' as i8));
    let (mut delims, mut quotes) = (0u64, 0u64);
    for (i, part) in v.iter().enumerate() {
        let either = _mm_or_si128(_mm_cmpeq_epi8(*part, comma), _mm_cmpeq_epi8(*part, newline));
        delims |= u64::from(_mm_movemask_epi8(either) as u16) << (16 * i);
        quotes |= u64::from(_mm_movemask_epi8(_mm_cmpeq_epi8(*part, quote)) as u16) << (16 * i);
    }
    (delims, quotes)
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
unsafe fn masks(p: *const u8) -> (u64, u64) {
    use std::arch::aarch64::*;
    let v = [vld1q_u8(p), vld1q_u8(p.add(16)), vld1q_u8(p.add(32)), vld1q_u8(p.add(48))];
    const WEIGHTS: [u8; 16] = [1, 2, 4, 8, 16, 32, 64, 128, 1, 2, 4, 8, 16, 32, 64, 128];
    let weights = vld1q_u8(WEIGHTS.as_ptr());
    // One bit a byte, from four vectors of 0x00 or 0xff bytes.
    let bits = |m: [uint8x16_t; 4]| -> u64 {
        let t0 = vandq_u8(m[0], weights);
        let t1 = vandq_u8(m[1], weights);
        let t2 = vandq_u8(m[2], weights);
        let t3 = vandq_u8(m[3], weights);
        let sum = vpaddq_u8(vpaddq_u8(t0, t1), vpaddq_u8(t2, t3));
        let sum = vpaddq_u8(sum, sum);
        vgetq_lane_u64(vreinterpretq_u64_u8(sum), 0)
    };
    let (comma, newline, quote) = (vdupq_n_u8(b','), vdupq_n_u8(b'\n'), vdupq_n_u8(b'"'));
    let either = |part: uint8x16_t| vorrq_u8(vceqq_u8(part, comma), vceqq_u8(part, newline));
    (
        bits([either(v[0]), either(v[1]), either(v[2]), either(v[3])]),
        bits([vceqq_u8(v[0], quote), vceqq_u8(v[1], quote), vceqq_u8(v[2], quote), vceqq_u8(v[3], quote)]),
    )
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
#[inline(always)]
unsafe fn masks(p: *const u8) -> (u64, u64) {
    masks_plain(p)
}

/// The definition the vector versions are tested against.
#[allow(dead_code)]
unsafe fn masks_plain(p: *const u8) -> (u64, u64) {
    let (mut delims, mut quotes) = (0u64, 0u64);
    for i in 0..64 {
        let byte = *p.add(i);
        delims |= u64::from(byte == b'\n' || byte == b',') << i;
        quotes |= u64::from(byte == b'"') << i;
    }
    (delims, quotes)
}

/// Stage one with AVX-512, for the CPUs that have it (Ice Lake and later, Zen 4
/// and later). The build targets x86-64-v3, so this loop is compiled with its
/// own instruction set and chosen when the process starts.
///
/// Two things are shorter than in the narrow loop. A compare leaves its bitmap
/// in a mask register directly, with no `movemask`; and `vpcompressb` (VBMI2)
/// turns a delimiter bitmap into the offsets of its set bits at once, where the
/// narrow loop takes them out one at a time, eight to a block whether or not
/// the block has eight.
///
/// It works on 256-bit registers. The same loop on 512-bit registers has half
/// the instructions and was no faster where it was measured (Ice Lake, BENCH.md
/// experiment 29), and some CPUs lower their clock for 512-bit registers, for
/// everything that runs after as well.
#[cfg(target_arch = "x86_64")]
mod wide {
    use super::{Index, Level};
    use std::arch::x86_64::*;
    use std::sync::atomic::{AtomicU8, Ordering};

    /// 0 until the choice is made, then 1 for the narrow loop and 2 for this one.
    static CHOSEN: AtomicU8 = AtomicU8::new(0);

    /// Whether this CPU, and the kernel under it, run the wide loop.
    pub fn supported() -> bool {
        is_x86_feature_detected!("avx512f")
            && is_x86_feature_detected!("avx512bw")
            && is_x86_feature_detected!("avx512vl")
            && is_x86_feature_detected!("avx512vbmi2")
            && is_x86_feature_detected!("avx2")
            && is_x86_feature_detected!("popcnt")
    }

    #[inline(always)]
    pub fn chosen() -> Level {
        match CHOSEN.load(Ordering::Relaxed) {
            0 => choose(),
            1 => Level::Narrow,
            _ => Level::Wide,
        }
    }

    #[cold]
    #[inline(never)]
    fn choose() -> Level {
        let narrow_asked = std::env::var("SS_SCAN").is_ok_and(|asked| asked.trim() == "narrow");
        let level = if supported() && !narrow_asked { Level::Wide } else { Level::Narrow };
        CHOSEN.store(if level == Level::Wide { 2 } else { 1 }, Ordering::Relaxed);
        level
    }

    /// By low nibble, the delimiter that has it: `\n` is 0x0a and `,` is 0x2c.
    /// `pshufb` with a byte as its own index gives this table's entry for the
    /// byte's low nibble, or 0 for a byte of 0x80 and over; that equals the byte
    /// itself exactly when the byte is one of the two. 0xff stands for "none": no
    /// byte under 0x80 equals it.
    static DELIMITER_BY_LOW_NIBBLE: [u8; 16] =
        [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, b'\n', 0xff, b',', 0xff, 0xff, 0xff];

    /// 0, 1, 2, ... 31: compressed under a bitmap, the offsets of its set bits.
    static OFFSETS: [u8; 32] = {
        let mut offsets = [0u8; 32];
        let mut at = 0;
        while at < 32 {
            offsets[at] = at as u8;
            at += 1;
        }
        offsets
    };

    /// `bits`, with the compiler kept from knowing where they came from.
    #[inline(always)]
    fn apart(mut bits: u32) -> u32 {
        unsafe { std::arch::asm!("/* {0:e} */", inout(reg) bits, options(pure, nomem, nostack, preserves_flags)) };
        bits
    }

    /// 32 bytes at a time. A half-block's first 8 delimiters are written without
    /// a branch (32 bytes of CSV seldom hold more); the slots past the last are
    /// garbage that the next half overwrites, as in the narrow loop.
    #[target_feature(enable = "avx512f,avx512bw,avx512vl,avx512vbmi2,avx2,popcnt")]
    pub unsafe fn build(index: &mut Index, data: *const u8, len: usize) {
        let table = _mm256_broadcastsi128_si256(_mm_loadu_si128(DELIMITER_BY_LOW_NIBBLE.as_ptr() as *const __m128i));
        let quote = _mm256_set1_epi8(b'"' as i8);
        let offsets = _mm256_loadu_si256(OFFSETS.as_ptr() as *const __m256i);
        let quotes_before = index.quotes_before.as_mut_ptr();
        let quote_bits_out = index.quote_bits.as_mut_ptr();
        let mut out = index.delims.as_mut_ptr().add(1);
        let mut quotes = 0u32;
        // The 32 bytes at offset `$at`: lists their delimiters, gives their quote
        // bitmap. `$keep` clears the bits of bytes that are not data.
        macro_rules! half {
            ($at:expr, $keep:expr) => {{
                let at: usize = $at;
                let bytes = _mm256_loadu_si256(data.add(at) as *const __m256i);
                let delims: u32 = _mm256_cmpeq_epi8_mask(_mm256_shuffle_epi8(table, bytes), bytes) & $keep;
                let quote_bits: u32 = _mm256_cmpeq_epi8_mask(bytes, quote) & $keep;
                let found = _mm256_maskz_compress_epi8(delims, offsets);
                let base = _mm256_set1_epi32(at as u32 as i32);
                // Eight of the offsets found, from bytes in the half to offsets in the buffer.
                macro_rules! eight {
                    ($part:expr) => {
                        _mm256_add_epi32(_mm256_cvtepu8_epi32($part), base)
                    };
                }
                let low = _mm256_castsi256_si128(found);
                _mm256_storeu_si256(out as *mut __m256i, eight!(low));
                let count = delims.count_ones() as usize;
                if count > 8 {
                    let high = _mm256_extracti128_si256::<1>(found);
                    _mm256_storeu_si256(out.add(8) as *mut __m256i, eight!(_mm_srli_si128::<8>(low)));
                    _mm256_storeu_si256(out.add(16) as *mut __m256i, eight!(high));
                    _mm256_storeu_si256(out.add(24) as *mut __m256i, eight!(_mm_srli_si128::<8>(high)));
                }
                out = out.add(count);
                quote_bits
            }};
        }
        macro_rules! block {
            ($block:expr, $keep:expr) => {{
                let block: usize = $block;
                let keep: u64 = $keep;
                let low = half!(block * 64, keep as u32);
                let high = half!(block * 64 + 32, (keep >> 32) as u32);
                // Left to itself the compiler sees two bitmaps joined and makes them
                // one 512-bit compare, the one thing this loop is there to avoid.
                let quote_bits = u64::from(apart(low)) | u64::from(high) << 32;
                *quotes_before.add(block) = quotes;
                *quote_bits_out.add(block) = quote_bits;
                quotes += quote_bits.count_ones();
            }};
        }
        let whole = len / 64;
        for block in 0..whole {
            block!(block, u64::MAX);
        }
        // The bytes past `len` in a last, partial block are not data.
        let mut blocks = whole;
        if len & 63 != 0 {
            block!(whole, (1u64 << (len & 63)) - 1);
            blocks += 1;
        }
        index.finish(blocks, quotes, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small deterministic generator, so the test needs no crate.
    fn next(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    #[test]
    fn vector_masks_match_the_plain_ones() {
        let mut state = 1;
        for _ in 0..20_000 {
            let mut block = [0u8; 64];
            for byte in &mut block {
                *byte = match next(&mut state) % 9 {
                    0 => b'\n',
                    1 | 2 => b',',
                    3 => b'"',
                    4 => (next(&mut state) & 0xff) as u8,
                    _ => b'a',
                };
            }
            unsafe { assert_eq!(masks(block.as_ptr()), masks_plain(block.as_ptr())) };
        }
    }

    #[test]
    fn index_lists_every_delimiter() {
        let mut state = 7;
        for round in 0..300 {
            let len = (next(&mut state) % 700) as usize + usize::from(round % 3 == 0) * 64;
            let mut data = vec![b'x'; len + 64];
            for byte in data.iter_mut().take(len) {
                *byte = match next(&mut state) % 12 {
                    0 => b'\n',
                    1..=4 => b',',
                    5 => b'"',
                    _ => b'q',
                };
            }
            // Delimiters past `len` must not be reported.
            for byte in data.iter_mut().skip(len) {
                *byte = b',';
            }
            check(&data, len, 1024);
        }
    }

    /// Builds the index of `data[..len]` with every loop this CPU can run, twice
    /// each into one index (the second time over what the first left behind),
    /// and holds each to the definition.
    fn check(data: &[u8], len: usize, capacity: usize) {
        let delims: Vec<u32> = (0..len).filter(|&i| data[i] == b',' || data[i] == b'\n').map(|i| i as u32).collect();
        for level in available() {
            let mut index = Index::new(capacity);
            for round in 0..2 {
                unsafe { index.build_with(level, data.as_ptr(), len) };
                let found = unsafe { std::slice::from_raw_parts(index.delims().sub(1), delims.len() + 9) };
                assert_eq!(found[0], u32::MAX, "{level:?}, round {round}");
                assert_eq!(&found[1..=delims.len()], &delims[..], "{level:?}, round {round}, {len} bytes");
                assert_eq!(&found[delims.len() + 1..], &[u32::MAX; 8], "{level:?}, round {round}");
                let mut quotes = 0;
                for at in 0..=len {
                    assert_eq!(index.quotes_before(at), quotes, "{level:?}: quotes before {at} of {len}");
                    if at < len && data[at] == b'"' {
                        quotes += 1;
                    }
                }
            }
        }
    }

    /// Every loop against the definition on what the test above does not reach:
    /// stretches with more delimiters than are written without a branch (up to
    /// nothing else), every byte value, every length around the block sizes, and
    /// an index with exactly the room it was asked for.
    #[test]
    fn every_loop_lists_every_delimiter() {
        let mut state = 29;
        for round in 0..6_000 {
            let len = match round % 4 {
                0 => (next(&mut state) % 200) as usize,
                1 => 64 * (1 + (next(&mut state) % 6) as usize) + (next(&mut state) % 3) as usize - 1,
                _ => (next(&mut state) % 1500) as usize,
            };
            // How dense the delimiters are changes from stretch to stretch.
            let mut data = vec![0u8; len + 64];
            let mut density = 2;
            for (at, byte) in data.iter_mut().enumerate() {
                if at % 48 == 0 {
                    density = [1, 2, 3, 6, 12, 40][(next(&mut state) % 6) as usize];
                }
                let r = next(&mut state);
                *byte = match r % density {
                    0 if r >> 20 & 3 == 0 => b'\n',
                    0 => b',',
                    1 if r >> 22 & 3 == 0 => b'"',
                    1 if r >> 24 & 3 == 0 => (r >> 32) as u8,
                    // The bytes a nibble away from a delimiter, and those with the top bit set.
                    2 if r >> 26 & 1 == 0 => [0x0a, 0x1a, 0x2a, 0x8a, 0x0c, 0x1c, 0x2c, 0xac, 0x22, 0xa2, 0x00, 0xff][(r >> 40) as usize % 12],
                    _ => b'a' + (r >> 48) as u8 % 26,
                };
            }
            check(&data, len, len.max(1));
        }
        // Nothing but delimiters, and nothing but quotes.
        for len in [0, 1, 31, 32, 33, 63, 64, 65, 127, 128, 129, 1000] {
            for fill in [b',', b'\n', b'"'] {
                check(&vec![fill; len + 64], len, len.max(1));
            }
        }
    }

    /// Says in the test log which loops the tests above ran on this machine
    /// (`cargo test -- --nocapture`), and holds `chosen` to what it promises.
    #[test]
    fn the_chosen_loop_is_one_this_cpu_can_run() {
        let levels = available();
        eprintln!("scan loops tested on this CPU: {levels:?}; chosen: {:?}", chosen());
        assert_eq!(levels[0], Level::Narrow);
        assert!(levels.contains(&chosen()));
    }
}
