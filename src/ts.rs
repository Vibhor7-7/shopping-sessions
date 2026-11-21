//! Timestamps, forms F1, F2 and F3 of rules.md, to epoch milliseconds.

pub const BAD: i64 = i64::MIN;
pub const DAY_MS: i64 = 86_400_000;

#[inline(always)]
pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    era * 146_097 + year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year - 719_468
}

/// The value of an ASCII digit, or something over 9 for any other byte.
#[inline(always)]
unsafe fn digit(p: *const u8, at: usize) -> u32 {
    u32::from((*p.add(at)).wrapping_sub(b'0'))
}

/// `YYYY-MM-DDTHH:MM:SS` at `p` (19 readable bytes) as epoch seconds, or BAD.
unsafe fn civil_seconds(p: *const u8) -> i64 {
    let y = [digit(p, 0), digit(p, 1), digit(p, 2), digit(p, 3)];
    let t = [
        digit(p, 5), digit(p, 6), digit(p, 8), digit(p, 9), digit(p, 11),
        digit(p, 12), digit(p, 14), digit(p, 15), digit(p, 17), digit(p, 18),
    ];
    // 9 - v wraps to a huge value exactly when v is not a digit.
    let mut not_digit = 0u32;
    for v in y {
        not_digit |= 9u32.wrapping_sub(v);
    }
    for v in t {
        not_digit |= 9u32.wrapping_sub(v);
    }
    let separators = (*p.add(4) == b'-')
        & (*p.add(7) == b'-')
        & (*p.add(10) == b'T')
        & (*p.add(13) == b':')
        & (*p.add(16) == b':');
    if (not_digit >> 31) != 0 || !separators {
        return BAD;
    }
    let year = y[0] * 1000 + y[1] * 100 + y[2] * 10 + y[3];
    let month = t[0] * 10 + t[1];
    let day = t[2] * 10 + t[3];
    let hour = t[4] * 10 + t[5];
    let minute = t[6] * 10 + t[7];
    let second = t[8] * 10 + t[9];
    if month == 0 || month > 12 || day == 0 || hour > 23 || minute > 59 || second > 59 {
        return BAD;
    }
    const DAYS: [u32; 13] = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    if day > DAYS[month as usize] + u32::from(month == 2 && leap) {
        return BAD;
    }
    days_from_civil(i64::from(year), i64::from(month), i64::from(day)) * 86_400
        + i64::from(hour * 3_600 + minute * 60 + second)
}

/// Digits only: epoch milliseconds, with any number of leading zeros.
#[inline(always)]
unsafe fn epoch_digits(p: *const u8, len: usize) -> i64 {
    let (mut at, mut len) = (0usize, len);
    if len > 17 {
        // Rare: leading zeros, or a number no window can hold.
        while len > 0 && *p.add(at) == b'0' {
            at += 1;
            len -= 1;
        }
        if len > 17 {
            return BAD;
        }
    }
    let mut value = 0i64;
    let mut not_digit = 0u32;
    for i in 0..len {
        let v = digit(p, at + i);
        not_digit |= 9u32.wrapping_sub(v);
        value = value * 10 + i64::from(v);
    }
    if (not_digit >> 31) != 0 {
        return BAD;
    }
    value
}

/// Epoch milliseconds of the `len` bytes at `p`, or BAD, one byte at a time.
/// This is the definition; `parse` is the same function made fast, and the
/// tests hold the two together.
#[allow(dead_code)]
pub unsafe fn parse_plain(p: *const u8, len: usize) -> i64 {
    if len == 20 && *p.add(19) == b'Z' {
        let seconds = civil_seconds(p);
        return if seconds == BAD { BAD } else { seconds * 1_000 };
    }
    if len == 25 {
        let sign = *p.add(19);
        if sign == b'+' || sign == b'-' {
            let seconds = civil_seconds(p);
            let (h0, h1, m0, m1) = (digit(p, 20), digit(p, 21), digit(p, 23), digit(p, 24));
            let not_digit = (9u32.wrapping_sub(h0) | 9u32.wrapping_sub(h1) | 9u32.wrapping_sub(m0) | 9u32.wrapping_sub(m1)) >> 31;
            let (hours, minutes) = (h0 * 10 + h1, m0 * 10 + m1);
            if seconds == BAD || not_digit != 0 || *p.add(22) != b':' || hours > 23 || minutes > 59 {
                return BAD;
            }
            let offset = i64::from((hours * 60 + minutes) * 60);
            let utc = if sign == b'+' { seconds - offset } else { seconds + offset };
            return utc * 1_000;
        }
    }
    if len == 0 {
        return BAD;
    }
    epoch_digits(p, len)
}

/// Midnights already worked out, by day of the month. A dataset's timestamps
/// fall on a few dozen dates, so nearly every date is a table hit: two compares
/// instead of validating a calendar date and counting its days.
pub struct Dates {
    slots: [Date; 64],
}

#[derive(Clone, Copy)]
struct Date {
    /// The eight bytes `YYYY-MM-`, as read from the text.
    head: u64,
    /// The three bytes `DDT` that follow them, in the low three bytes. An empty
    /// slot has the fourth byte set, which no three bytes of text can equal.
    tail: u32,
    /// Epoch seconds of that date's midnight.
    midnight: i64,
}

/// The `T` after the date, where it sits in `Date::tail`.
const T_AFTER_DAY: u32 = (b'T' as u32) << 16;

impl Dates {
    pub fn new() -> Self {
        Self { slots: [Date { head: 0, tail: u32::MAX, midnight: BAD }; 64] }
    }

    /// The slot for a day of the month, given its two characters: a different one for each of 01 to 31.
    #[inline(always)]
    fn slot(day: u16) -> usize {
        (((day >> 8) & 15) | ((day & 3) << 4)) as usize
    }

    /// A date not in the table: checks it in full and remembers it if it is real.
    #[cold]
    #[inline(never)]
    fn learn(&mut self, head: u64, day: u16) -> i64 {
        let mut text = [0u8; 19];
        text[..8].copy_from_slice(&head.to_le_bytes());
        text[8..10].copy_from_slice(&day.to_le_bytes());
        text[10..].copy_from_slice(b"T00:00:00");
        let midnight = unsafe { civil_seconds(text.as_ptr()) };
        if midnight != BAD {
            self.slots[Self::slot(day)] = Date { head, tail: u32::from(day) | T_AFTER_DAY, midnight };
        }
        midnight
    }
}

#[cfg(not(all(target_arch = "x86_64", target_feature = "sse4.1")))]
const ZEROS: u64 = 0x3030_3030_3030_3030;
/// Added to a byte that should be 0 to 9: the top bit stays clear only if it is.
#[cfg(not(all(target_arch = "x86_64", target_feature = "sse4.1")))]
const UP_TO_NINE: u64 = 0x7676_7676_7676_7676;
#[cfg(not(all(target_arch = "x86_64", target_feature = "sse4.1")))]
const TOP_BITS: u64 = 0x8080_8080_8080_8080;

/// `YYYY-MM-DDTHH:MM:SS` at `p` (20 readable bytes) as epoch seconds, or BAD.
/// The time of day is checked eight bytes at a time: after an XOR with the
/// expected text a digit is 0 to 9 and a separator is 0, and adding 0x76 or 0x7F
/// to each sets its top bit exactly when it is anything else.
#[inline(always)]
unsafe fn civil_seconds_fast(p: *const u8, dates: &mut Dates) -> i64 {
    let head = (p as *const u64).read_unaligned();
    let rest = (p.add(8) as *const u64).read_unaligned(); // DDTHH:MM
    let tail = (p.add(16) as *const u32).read_unaligned() & 0x00ff_ffff; // :SS
    let time = rest ^ 0x3030_3a30_3054_0000;
    let seconds = tail ^ 0x0030_303a;
    let wrong = ((time | time.wrapping_add(0x7676_7f76_767f_0000)) & 0x8080_8080_8080_0000)
        | u64::from((seconds | seconds.wrapping_add(0x0076_767f)) & 0x0080_8080);
    let hour = ((time >> 24) & 15) * 10 + ((time >> 32) & 15);
    let minute = ((time >> 48) & 15) * 10 + ((time >> 56) & 15);
    let second = u64::from(((seconds >> 8) & 15) * 10 + ((seconds >> 16) & 15));

    let day = rest as u16;
    let known = *dates.slots.get_unchecked(Dates::slot(day));
    let midnight =
        if known.head == head && known.tail == u32::from(day) | T_AFTER_DAY { known.midnight } else { dates.learn(head, day) };
    if wrong != 0 || hour > 23 || minute > 59 || second > 59 || midnight == BAD {
        return BAD;
    }
    midnight + (hour * 3_600 + minute * 60 + second) as i64
}

/// Eight digit values, the first in the lowest byte, as one number (Lemire's three multiplies).
#[cfg(not(all(target_arch = "x86_64", target_feature = "sse4.1")))]
#[inline(always)]
fn eight_digits(v: u64) -> u64 {
    let v = (v & 0x0f0f_0f0f_0f0f_0f0f).wrapping_mul(2_561) >> 8;
    let v = (v & 0x00ff_00ff_00ff_00ff).wrapping_mul(6_553_601) >> 16;
    (v & 0x0000_ffff_0000_ffff).wrapping_mul(42_949_672_960_001) >> 32
}

/// Thirteen digits: epoch milliseconds as they are written from 2001 to 2286.
#[inline(always)]
unsafe fn thirteen_digits(p: *const u8) -> i64 {
    let (value, ok) = thirteen(p);
    if ok {
        value
    } else {
        BAD
    }
}

/// The 13 bytes at `p` as a number, and whether they are all digits. Never
/// branches, so it can be worked out before the form of the text is known.
/// Reads the three bytes before `p` too (the reader's buffer always has them).
#[cfg(all(target_arch = "x86_64", target_feature = "sse4.1"))]
#[inline(always)]
unsafe fn thirteen(p: *const u8) -> (i64, bool) {
    use std::arch::x86_64::*;
    // Sixteen bytes ending at the last digit; the first three are not ours and are zeroed.
    let bytes = _mm_loadu_si128(p.sub(3) as *const __m128i);
    let ours = _mm_setr_epi8(0, 0, 0, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1);
    let digits = _mm_and_si128(_mm_sub_epi8(bytes, _mm_set1_epi8(b'0' as i8)), ours);
    let over = _mm_subs_epu8(digits, _mm_set1_epi8(9));
    let ok = _mm_testz_si128(over, over) != 0;
    // Pairs, fours, eights: each step a multiply-add of neighbours.
    let pairs = _mm_maddubs_epi16(digits, _mm_setr_epi8(10, 1, 10, 1, 10, 1, 10, 1, 10, 1, 10, 1, 10, 1, 10, 1));
    let fours = _mm_madd_epi16(pairs, _mm_setr_epi16(100, 1, 100, 1, 100, 1, 100, 1));
    let fours = _mm_packus_epi32(fours, fours);
    let eights = _mm_madd_epi16(fours, _mm_setr_epi16(10_000, 1, 10_000, 1, 10_000, 1, 10_000, 1));
    let high = _mm_cvtsi128_si32(eights) as u32 as u64; // digits 0 to 4
    let low = _mm_extract_epi32(eights, 1) as u32 as u64; // digits 5 to 12
    ((high * 100_000_000 + low) as i64, ok)
}

#[cfg(not(all(target_arch = "x86_64", target_feature = "sse4.1")))]
#[inline(always)]
unsafe fn thirteen(p: *const u8) -> (i64, bool) {
    let first = (p as *const u64).read_unaligned() ^ ZEROS; // digits 0 to 7
    let last = (p.add(5) as *const u64).read_unaligned() ^ ZEROS; // digits 5 to 12
    let ok = ((first | first.wrapping_add(UP_TO_NINE)) | (last | last.wrapping_add(UP_TO_NINE))) & TOP_BITS == 0;
    ((eight_digits(first) * 100_000 + eight_digits(last) % 100_000) as i64, ok)
}

/// Seconds for each pair of digit values `tens | ones << 4`, or a value too large to be a time of day.
#[cfg(all(target_arch = "x86_64", target_feature = "bmi2"))]
const fn clock_table(limit: u32, unit: u32) -> [u32; 256] {
    let mut table = [1 << 20; 256];
    let mut at = 0;
    while at < 256 {
        let (tens, ones) = ((at & 15) as u32, (at >> 4) as u32);
        if tens <= 9 && ones <= 9 && tens * 10 + ones <= limit {
            table[at] = (tens * 10 + ones) * unit;
        }
        at += 1;
    }
    table
}
/// For the hours and minutes of an offset (`zone`).
#[cfg(all(target_arch = "x86_64", target_feature = "bmi2"))]
static HOURS: [u32; 256] = clock_table(23, 3_600);
#[cfg(all(target_arch = "x86_64", target_feature = "bmi2"))]
static MINUTES: [u32; 256] = clock_table(59, 60);

/// Two texts of eight bytes side by side, each to be read as `HH:MM:SS`: for
/// each, a number in the low half of its 64 bits, and eight bits (the first
/// text's are the low eight). A text is a real time of day exactly when its
/// eight bits are all set and its number is under 86,400, and then the number
/// is its seconds since midnight.
///
/// After an XOR with `00:00:00` a digit is 0 to 9 and a colon 0. Two
/// multiply-adds then weigh the six digits (10 and 1 for a pair, then 3,600,
/// 60 and 1), which is all the arithmetic there is. A bit is set for a byte
/// that is no more than the largest digit its place allows; that leaves hours
/// up to 29, and with minutes and seconds under 60 those are the numbers from
/// 86,400 on.
/// One pass for two texts costs what it costs for one, so an event line's `ts`
/// and `ingest_ts` share it (BENCH.md, experiment 42; until then each went
/// through a `pext` and three table reads of its own).
#[cfg(all(target_arch = "x86_64", target_feature = "sse4.1"))]
#[inline(always)]
unsafe fn clocks(texts: std::arch::x86_64::__m128i) -> (std::arch::x86_64::__m128i, u32) {
    use std::arch::x86_64::*;
    let digits = _mm_xor_si128(texts, _mm_set1_epi64x(i64::from_le_bytes(*b"00:00:00")));
    // H H : M M : S S, the most each place may hold.
    let most = _mm_set1_epi64x(i64::from_le_bytes([2, 9, 0, 5, 9, 0, 5, 9]));
    let within = _mm_cmpeq_epi8(_mm_max_epu8(digits, most), most);
    // Words: HH, ten times the minutes' first digit, their second digit, SS.
    let pairs = _mm_maddubs_epi16(digits, _mm_set1_epi64x(i64::from_le_bytes([10, 1, 0, 10, 1, 0, 10, 1])));
    let halves = _mm_madd_epi16(pairs, _mm_setr_epi16(3_600, 60, 60, 1, 3_600, 60, 60, 1));
    (_mm_add_epi32(halves, _mm_srli_epi64(halves, 32)), _mm_movemask_epi8(within) as u32)
}

/// `HH:MM:SS` at `p` as seconds since midnight, and whether it is a real time of day. Never branches.
#[cfg(all(target_arch = "x86_64", target_feature = "sse4.1"))]
#[inline(always)]
unsafe fn time_of_day(p: *const u8) -> (i64, bool) {
    use std::arch::x86_64::*;
    let (numbers, within) = clocks(_mm_loadl_epi64(p as *const __m128i));
    let seconds = _mm_cvtsi128_si32(numbers) as u32;
    (i64::from(seconds), (within as u8 == u8::MAX) & (seconds < 86_400))
}

#[cfg(not(all(target_arch = "x86_64", target_feature = "sse4.1")))]
#[inline(always)]
unsafe fn time_of_day(p: *const u8) -> (i64, bool) {
    let time = (p as *const u64).read_unaligned() ^ 0x3030_3a30_303a_3030;
    let wrong = (time | time.wrapping_add(0x7676_7f76_767f_7676)) & TOP_BITS;
    let hour = (time & 15) * 10 + ((time >> 8) & 15);
    let minute = ((time >> 24) & 15) * 10 + ((time >> 32) & 15);
    let second = ((time >> 48) & 15) * 10 + ((time >> 56) & 15);
    ((hour * 3_600 + minute * 60 + second) as i64, (wrong == 0) & (hour <= 23) & (minute <= 59) & (second <= 59))
}

/// `time_of_day` of `a` and of `b`, in one pass where the CPU has one.
#[cfg(all(target_arch = "x86_64", target_feature = "sse4.1"))]
#[inline(always)]
unsafe fn times_of_day(a: *const u8, b: *const u8) -> ((i64, bool), (i64, bool)) {
    use std::arch::x86_64::*;
    let texts = _mm_insert_epi64(_mm_loadl_epi64(a as *const __m128i), (b as *const i64).read_unaligned(), 1);
    let (numbers, within) = clocks(texts);
    let (first, second) = (_mm_cvtsi128_si32(numbers) as u32, _mm_extract_epi32(numbers, 2) as u32);
    (
        (i64::from(first), (within as u8 == u8::MAX) & (first < 86_400)),
        (i64::from(second), ((within >> 8) as u8 == u8::MAX) & (second < 86_400)),
    )
}

#[cfg(not(all(target_arch = "x86_64", target_feature = "sse4.1")))]
#[inline(always)]
unsafe fn times_of_day(a: *const u8, b: *const u8) -> ((i64, bool), (i64, bool)) {
    (time_of_day(a), time_of_day(b))
}

/// `YYYY-MM-DDTHH:MM:SS` at `p` as epoch seconds, if its date is already in the
/// table and the rest is well formed; `time_of_day(p + 11)` is handed in.
/// Never branches and never learns a date.
#[inline(always)]
unsafe fn civil_known(p: *const u8, dates: &Dates, (seconds, time_ok): (i64, bool)) -> (i64, bool) {
    let head = (p as *const u64).read_unaligned();
    let tail = (p.add(8) as *const u32).read_unaligned() & 0x00ff_ffff; // DDT
    let known = dates.slots.get_unchecked(Dates::slot(tail as u16));
    // Two compares cover the date, the `T`, and whether the slot is filled at all.
    let ok = (known.head == head) & (known.tail == tail) & time_ok;
    (known.midnight.wrapping_add(seconds), ok)
}

/// `±HH:MM` at `p + 19` (the eight bytes from `p + 17` are read): the seconds
/// to add to the local time before it to get UTC, and whether it is an offset
/// at all. Never branches: which sign it has is as good as random.
#[inline(always)]
unsafe fn zone(p: *const u8) -> (i64, bool) {
    let tail = (p.add(17) as *const u64).read_unaligned(); // SS±HH:MM
    // As in `time_of_day`: a digit becomes 0 to 9 and the colon 0, or the top bit is set.
    let zone = tail ^ 0x3030_3a30_3000_0000;
    let wrong = (zone | zone.wrapping_add(0x7676_7f76_7600_0000)) & 0x8080_8080_8000_0000;
    let sign = (tail >> 16) as u8;
    #[cfg(all(target_arch = "x86_64", target_feature = "bmi2"))]
    let (offset, in_range) = {
        let digits = std::arch::x86_64::_pext_u64(zone, 0x0f0f_000f_0f00_0000) as usize;
        let offset = *HOURS.get_unchecked(digits & 255) + *MINUTES.get_unchecked(digits >> 8);
        (i64::from(offset), offset < 86_400)
    };
    #[cfg(not(all(target_arch = "x86_64", target_feature = "bmi2")))]
    let (offset, in_range) = {
        let hours = ((zone >> 24) & 15) * 10 + ((zone >> 32) & 15);
        let minutes = ((zone >> 48) & 15) * 10 + ((zone >> 56) & 15);
        (((hours * 60 + minutes) * 60) as i64, (hours <= 23) & (minutes <= 59))
    };
    // `+HH:MM` is ahead of UTC, so it is taken off.
    let to_utc = std::hint::select_unpredictable(sign == b'+', -offset, offset);
    (to_utc, (wrong == 0) & in_range & ((sign == b'+') | (sign == b'-')))
}

/// The offset form, `YYYY-MM-DDTHH:MM:SS±HH:MM`, finished from what
/// `civil_known` found in the same text (`seconds`, `civil_ok`): its epoch
/// milliseconds, and whether it is that form, well formed, on a date in the
/// table. Anything else is for `parse_general`.
#[inline(always)]
unsafe fn offset_known(p: *const u8, len: usize, seconds: i64, civil_ok: bool) -> (i64, bool) {
    let (to_utc, zone_ok) = zone(p);
    (seconds.wrapping_add(to_utc).wrapping_mul(1_000), (len == 25) & civil_ok & zone_ok)
}

/// `flag`, with the compiler kept from knowing how it was worked out, so that a
/// branch on it stays one branch. `std::hint::black_box` does that too, but
/// through memory: the flag is stored to the stack and compared there, on every
/// line, and the one branch of the line loop that is guessed wrong one time in
/// ten waits for that store before it can be found wrong. Here it stays in a
/// register (as `scan::wide::apart` keeps a bitmap).
#[inline(always)]
fn hidden(flag: bool) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        let mut bits = u8::from(flag);
        unsafe { std::arch::asm!("/* {0} */", inout(reg_byte) bits, options(pure, nomem, nostack, preserves_flags)) };
        bits != 0
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        std::hint::black_box(flag)
    }
}

/// Epoch milliseconds of the `len` bytes at `p`, or BAD.
///
/// Which form a `ts` takes is as good as random from line to line, so the two
/// common ones (13 digits; UTC on a date already seen) are both worked out
/// without a branch and the answer picked by the length. One line in ten has
/// neither, and nearly all of those have the offset form on a date already
/// seen: that one is finished here, from the date and time of day already in
/// hand. What is left (a date not yet in the table, a bad timestamp, digits
/// that are not thirteen) goes to `parse_general`. `p` must be in the
/// reader's buffer: 3 bytes before it and 32 after are read whatever `len` is.
#[inline(always)]
pub unsafe fn parse(p: *const u8, len: usize, dates: &mut Dates) -> i64 {
    parse_timed(p, len, dates, time_of_day(p.add(11)))
}

/// `parse`, given `time_of_day(p + 11)`.
#[inline(always)]
unsafe fn parse_timed(p: *const u8, len: usize, dates: &mut Dates, time: (i64, bool)) -> i64 {
    let (digits, digits_ok) = thirteen(p);
    let (seconds, civil_ok) = civil_known(p, dates, time);
    let is_digits = len == 13;
    let is_utc = (len == 20) & (*p.add(19) == b'Z');
    // Selected, not branched on: left to itself the compiler turns this back into
    // a branch on the form, which is the one thing here that cannot be predicted.
    let value = std::hint::select_unpredictable(is_digits, digits, seconds.wrapping_mul(1_000));
    let ok = std::hint::select_unpredictable(is_digits, digits_ok, is_utc & civil_ok);
    // `ok` is true nine times in ten, so this branch is cheap, but only as one
    // branch: the compiler splits a branch on "A and B, or C and D" into a branch
    // on each, and the first of those is the form again. `hidden` keeps from it
    // how `ok` was made, so it cannot (measured: 700,000 mispredictions per million lines without it).
    if hidden(ok) {
        return value;
    }
    // Taken by nearly every line that gets here, so it is a branch the CPU predicts.
    let (value, ok) = offset_known(p, len, seconds, civil_ok);
    if ok {
        value
    } else {
        parse_general(p, len, dates)
    }
}

/// A field that is always UTC on a date already seen, as `ingest_ts` is: its
/// epoch seconds and its epoch milliseconds, or BAD twice. Unlike `ts`, its form
/// never changes from line to line, so every check here is a branch the CPU
/// predicts, and the seconds come out without a division. Anything unexpected
/// goes to `parse_general` and gets the same answer the slow way.
#[inline(always)]
#[allow(dead_code)]
pub unsafe fn parse_utc(p: *const u8, len: usize, dates: &mut Dates) -> (i64, i64) {
    parse_utc_timed(p, len, dates, time_of_day(p.add(11)))
}

/// `parse_utc`, given `time_of_day(p + 11)`.
#[inline(always)]
unsafe fn parse_utc_timed(p: *const u8, len: usize, dates: &mut Dates, (seconds, time_ok): (i64, bool)) -> (i64, i64) {
    let head = (p as *const u64).read_unaligned();
    let tail = (p.add(8) as *const u32).read_unaligned() & 0x00ff_ffff; // DDT
    let known = dates.slots.get_unchecked(Dates::slot(tail as u16));
    if len == 20 && *p.add(19) == b'Z' && known.head == head && known.tail == tail && time_ok {
        let seconds = known.midnight + seconds;
        (seconds, seconds * 1_000)
    } else {
        parse_utc_general(p, len, dates)
    }
}

/// An event line's two timestamps: its `ts` as `parse` gives it, and its
/// `ingest_ts` as `parse_utc` does. Both hold a time of day at the same place
/// (when `ts` is not digits only), and the two are worked out in one pass
/// (`times_of_day`). `ts` and `ingest` must each be in the reader's buffer, as
/// for `parse`.
#[inline(always)]
pub unsafe fn parse_both(ts: *const u8, ts_len: usize, ingest: *const u8, ingest_len: usize, dates: &mut Dates) -> (i64, (i64, i64)) {
    let (ts_time, ingest_time) = times_of_day(ts.add(11), ingest.add(11));
    let ingest = parse_utc_timed(ingest, ingest_len, dates, ingest_time);
    (parse_timed(ts, ts_len, dates, ts_time), ingest)
}

#[cold]
#[inline(never)]
unsafe fn parse_utc_general(p: *const u8, len: usize, dates: &mut Dates) -> (i64, i64) {
    let milliseconds = parse_general(p, len, dates);
    if milliseconds == BAD {
        (BAD, BAD)
    } else {
        (milliseconds.div_euclid(1_000), milliseconds)
    }
}

/// Any timestamp, by form: what `parse` and `parse_utc` fall back on. It is the
/// only place a date is learned.
#[inline(never)]
pub unsafe fn parse_general(p: *const u8, len: usize, dates: &mut Dates) -> i64 {
    if len == 20 {
        if *p.add(19) == b'Z' {
            let seconds = civil_seconds_fast(p, dates);
            return if seconds == BAD { BAD } else { seconds * 1_000 };
        }
    } else if len == 13 {
        return thirteen_digits(p);
    } else if len == 25 {
        let sign = *p.add(19);
        if sign == b'+' || sign == b'-' {
            let seconds = civil_seconds_fast(p, dates);
            let (h0, h1, m0, m1) = (digit(p, 20), digit(p, 21), digit(p, 23), digit(p, 24));
            let not_digit = (9u32.wrapping_sub(h0) | 9u32.wrapping_sub(h1) | 9u32.wrapping_sub(m0) | 9u32.wrapping_sub(m1)) >> 31;
            let (hours, minutes) = (h0 * 10 + h1, m0 * 10 + m1);
            if seconds == BAD || not_digit != 0 || *p.add(22) != b':' || hours > 23 || minutes > 59 {
                return BAD;
            }
            let offset = i64::from((hours * 60 + minutes) * 60);
            let utc = if sign == b'+' { seconds - offset } else { seconds + offset };
            return utc * 1_000;
        }
    }
    if len == 0 {
        return BAD;
    }
    epoch_digits(p, len)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both implementations, which must agree; the text is padded as the reader's buffer is.
    fn ts(text: &str) -> i64 {
        let mut dates = Dates::new();
        both(text.as_bytes(), &mut dates)
    }

    /// What `parse_utc` owes for a timestamp of `milliseconds`: its whole seconds too.
    fn utc_pair(milliseconds: i64) -> (i64, i64) {
        if milliseconds == BAD {
            (BAD, BAD)
        } else {
            (milliseconds.div_euclid(1_000), milliseconds)
        }
    }

    fn both(text: &[u8], dates: &mut Dates) -> i64 {
        // As in the reader's buffer: other bytes before and after the field.
        let mut padded = b"9,\"1".to_vec();
        let start = padded.len();
        padded.extend_from_slice(text);
        padded.extend_from_slice(b",x\xff\09Z\n\"2026-10-03T23:58:10Z,1791071890000,,,,,,,,,,,,,,,,,,,,,,,,,,,");
        let (p, len) = unsafe { (padded.as_ptr().add(start), text.len()) };
        let plain = unsafe { parse_plain(p, len) };
        let shown = String::from_utf8_lossy(text).into_owned();
        // Twice each: once learning the date, once from the table.
        for round in 0..2 {
            assert_eq!(unsafe { parse(p, len, dates) }, plain, "parse, round {round}: {shown:?}");
            assert_eq!(unsafe { parse_utc(p, len, dates) }, utc_pair(plain), "parse_utc, round {round}: {shown:?}");
            assert_eq!(unsafe { parse_general(p, len, dates) }, plain, "parse_general, round {round}: {shown:?}");
        }
        // As an event line's pair, on either side (the same text on the other).
        for round in 0..2 {
            let dates = if round == 0 { &mut *dates } else { &mut Dates::new() };
            assert_eq!(unsafe { parse_both(p, len, p, len, dates) }, (plain, utc_pair(plain)), "parse_both, round {round}: {shown:?}");
        }
        // And with nothing learned yet.
        assert_eq!(unsafe { parse(p, len, &mut Dates::new()) }, plain, "parse, fresh: {shown:?}");
        assert_eq!(unsafe { parse_utc(p, len, &mut Dates::new()) }, utc_pair(plain), "parse_utc, fresh: {shown:?}");
        // Which way an offset form goes. `parse_general` gives the same answer, so
        // the lines above cannot tell: `parse` must finish it itself exactly when it
        // is that form and good (its date is in the table by now, being a real one).
        let (value, finished) = unsafe { finished_here(p, len, dates) };
        let good_offset_form = len == 25 && plain != BAD && text[4] == b'-';
        assert_eq!(finished, good_offset_form, "which way: {shown:?}");
        if finished {
            assert_eq!(value, plain, "finished in parse: {shown:?}");
        }
        assert!(!unsafe { finished_here(p, len, &Dates::new()) }.1, "finished on a date not in the table: {shown:?}");
        plain
    }

    /// What `parse` makes of a text that is not 13 digits or UTC: `offset_known`, fed as `parse` feeds it.
    unsafe fn finished_here(p: *const u8, len: usize, dates: &Dates) -> (i64, bool) {
        let (seconds, civil_ok) = civil_known(p, dates, time_of_day(p.add(11)));
        offset_known(p, len, seconds, civil_ok)
    }

    fn next(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    #[test]
    fn fast_and_plain_agree_on_everything() {
        let mut state = 42;
        let mut dates = Dates::new();
        let (mut good, mut bad) = (0, 0);
        let odd: &[u8] = b"0123456789-:TZ+ tz./\0\r\"9";
        for round in 0..400_000 {
            let r = next(&mut state);
            let year = 1990 + r % 60;
            let month = (r >> 8) % 14;
            let day = (r >> 16) % 33;
            let (hour, minute, second) = ((r >> 24) % 26, (r >> 32) % 62, (r >> 40) % 62);
            let mut text = match round % 4 {
                0 => format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"),
                1 => format!(
                    "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}{}{:02}:{:02}",
                    if r >> 63 == 0 { '+' } else { '-' }, (r >> 46) % 25, (r >> 52) % 61
                ),
                2 => format!("{}", 1_600_000_000_000 + r % 400_000_000_000),
                _ => format!("{:0width$}", r % 10u64.pow(1 + (r >> 50) as u32 % 18), width = (r >> 56) as usize % 22),
            }
            .into_bytes();
            // Most stay as written; the rest get a byte changed, added or dropped.
            let change = next(&mut state);
            if !text.is_empty() {
                let at = (change >> 8) as usize % text.len();
                match change % 8 {
                    0 => text[at] = odd[(change >> 32) as usize % odd.len()],
                    1 => text.insert(at, odd[(change >> 32) as usize % odd.len()]),
                    2 => {
                        text.remove(at);
                    }
                    3 => text[at] = (change >> 40) as u8,
                    _ => {}
                }
            }
            if both(&text, &mut dates) == BAD {
                bad += 1;
            } else {
                good += 1;
            }
        }
        assert!(good > 50_000 && bad > 50_000, "{good} good, {bad} bad");
    }

    #[test]
    fn the_three_forms_agree() {
        assert_eq!(ts("2026-10-03T23:58:10Z"), 1_791_071_890_000);
        assert_eq!(ts("2026-10-03T19:58:10-04:00"), 1_791_071_890_000);
        assert_eq!(ts("1791071890000"), 1_791_071_890_000);
        assert_eq!(ts("0001791071890000"), 1_791_071_890_000);
        assert_eq!(ts("000000000000000000001791071890000"), 1_791_071_890_000);
        assert_eq!(ts("2026-10-04T01:30:00+02:00"), ts("2026-10-03T23:30:00Z"));
        assert_eq!(ts("2026-10-03T10:00:00-00:00"), ts("2026-10-03T10:00:00Z"));
        assert_eq!(ts("2026-10-03T10:00:00+23:59"), ts("2026-10-02T10:01:00Z"));
        assert_eq!(ts("86400000"), 86_400_000);
        assert_eq!(ts("1970-01-01T00:00:00Z"), 0);
        assert_eq!(ts("2028-02-29T12:00:00Z"), ts("2028-03-01T12:00:00Z") - DAY_MS);
        assert_eq!(ts("2000-02-29T00:00:00Z"), 951_782_400_000);
    }

    /// Every offset there is, and every byte value in every place of one: each is
    /// finished in `parse` itself when it is good and never when it is not.
    #[test]
    fn every_offset_on_a_known_date_is_finished_in_parse() {
        let mut dates = Dates::new();
        let (mut good, mut bad) = (0, 0);
        for sign in ['+', '-'] {
            for hours in 0..24 {
                for minutes in 0..60 {
                    let text = format!("2026-10-03T19:58:10{sign}{hours:02}:{minutes:02}");
                    let offset = (hours * 3_600 + minutes * 60) * 1_000;
                    let utc = 1_791_057_490_000 + if sign == '+' { -offset } else { offset };
                    assert_eq!(both(text.as_bytes(), &mut dates), utc, "{text}");
                    good += 1;
                }
            }
        }
        for text in ["2026-10-03T19:58:10-04:00", "2026-10-31T00:00:00+12:00", "2026-10-01T23:59:59-10:00", "2028-02-29T07:08:09+05:30"] {
            for at in 0..text.len() {
                for byte in 0..=255u8 {
                    let mut changed = text.as_bytes().to_vec();
                    changed[at] = byte;
                    if both(&changed, &mut dates) == BAD {
                        bad += 1;
                    } else {
                        good += 1;
                    }
                    // One byte more or fewer is another form or none.
                    changed.insert(at, byte);
                    both(&changed, &mut dates);
                    changed.drain(at..at + 2);
                    both(&changed, &mut dates);
                }
            }
        }
        assert!(good > 3_000 && bad > 20_000, "{good} good, {bad} bad");
    }

    /// The definition of a time of day: `HH:MM:SS` from 00:00:00 to 23:59:59, as seconds.
    fn plain_time(text: &[u8; 8]) -> Option<i64> {
        let digit = |at: usize| text[at].is_ascii_digit().then(|| i64::from(text[at] - b'0'));
        if text[2] != b':' || text[5] != b':' {
            return None;
        }
        let (hour, minute, second) = (digit(0)? * 10 + digit(1)?, digit(3)? * 10 + digit(4)?, digit(6)? * 10 + digit(7)?);
        (hour <= 23 && minute <= 59 && second <= 59).then_some(hour * 3_600 + minute * 60 + second)
    }

    /// `time_of_day` of `a`, and `times_of_day` of `a` and `b` on either side, against the definition.
    fn check_times(a: &[u8; 8], b: &[u8; 8]) -> (bool, bool) {
        let found = |(seconds, real): (i64, bool)| real.then_some(seconds);
        let (plain_a, plain_b) = (plain_time(a), plain_time(b));
        unsafe {
            assert_eq!(found(time_of_day(a.as_ptr())), plain_a, "{:?}", String::from_utf8_lossy(a));
            let (first, second) = times_of_day(a.as_ptr(), b.as_ptr());
            assert_eq!((found(first), found(second)), (plain_a, plain_b), "{:?} with {:?}", String::from_utf8_lossy(a), String::from_utf8_lossy(b));
            let (first, second) = times_of_day(b.as_ptr(), a.as_ptr());
            assert_eq!((found(first), found(second)), (plain_b, plain_a), "{:?} with {:?}", String::from_utf8_lossy(b), String::from_utf8_lossy(a));
        }
        (plain_a.is_some(), plain_b.is_some())
    }

    /// The times of day straight, not through `parse`: one that was wrongly found
    /// bad would still come out right there, the slow way, and nothing would show
    /// (BENCH.md, "What 35 says"). Every second of the day beside another time,
    /// and every byte value in every place of both sides.
    #[test]
    fn every_time_of_day_and_every_byte_in_one() {
        let text = |seconds: u32| -> [u8; 8] {
            format!("{:02}:{:02}:{:02}", seconds / 3_600, seconds / 60 % 60, seconds % 60).into_bytes().try_into().unwrap()
        };
        for seconds in 0..86_400 {
            let other = (seconds * 7 + 12_345) % 86_400;
            assert_eq!(check_times(&text(seconds), &text(other)), (true, true));
            assert_eq!(plain_time(&text(seconds)), Some(i64::from(seconds)));
        }
        let (mut good, mut bad) = (0, 0);
        for base in [*b"00:00:00", *b"23:59:59", *b"19:58:10", *b"09:30:05", *b"20:00:49"] {
            for other in [*b"12:34:56", *b"24:00:00", *b"\0\0\0\0\0\0\0\0", *b"\xff\xff\xff\xff\xff\xff\xff\xff", *b"00:00:00"] {
                for at in 0..8 {
                    for byte in 0..=255u8 {
                        let mut changed = base;
                        changed[at] = byte;
                        if check_times(&changed, &other).0 {
                            good += 1;
                        } else {
                            bad += 1;
                        }
                    }
                }
            }
        }
        // Every pair of digit values in each of the three places, the other two left alone.
        for place in [0, 3, 6] {
            for tens in 0..=255u8 {
                for ones in 0..=255u8 {
                    let mut changed = *b"11:22:33";
                    (changed[place], changed[place + 1]) = (tens, ones);
                    if check_times(&changed, b"23:59:59").0 {
                        good += 1;
                    } else {
                        bad += 1;
                    }
                }
            }
        }
        assert!(good > 1_000 && bad > 200_000, "{good} good, {bad} bad");
    }

    /// An event line's two timestamps are each their own: every pairing of the
    /// forms, good and bad, gives what `parse` and `parse_utc` give one at a time.
    #[test]
    fn two_timestamps_at_once_are_each_their_own() {
        let texts = [
            "2026-10-03T23:58:10Z", "2026-10-03T19:58:10-04:00", "1791071890000", "2026-10-04T00:00:00Z", "2026-10-03T24:00:00Z",
            "2026-10-03T23:60:10Z", "2026-10-03T23:58:60Z", "2026-10-03T2a:58:10Z", "2026-10-03T23:58:10", "", "0001791071890000",
            "2026-10-05T07:08:09+05:30", "2026-10-05T07:08:09+05:60", "2026-02-30T07:08:09Z", "2026-10-03T19-58-10Z", "86400000",
            "2026-10-03T00:00:00Z", "2026-10-03T23:59:59Z", "2026-10-03T29:00:00Z", "1791071890000.0",
        ];
        let padded = |text: &str| {
            let mut bytes = b"9,\"1".to_vec();
            bytes.extend_from_slice(text.as_bytes());
            bytes.extend_from_slice(b",x\xff\09Z\n\"2026-10-03T23:58:10Z,1791071890000,,,,,,,,,,,,,,,,,,,,,,,,,,,");
            bytes
        };
        let mut dates = Dates::new();
        let mut seen = [0usize; 4];
        for round in 0..2 {
            for a in texts {
                for b in texts {
                    let (pa, pb) = (padded(a), padded(b));
                    unsafe {
                        let (a_at, b_at) = (pa.as_ptr().add(4), pb.as_ptr().add(4));
                        let (plain_a, plain_b) = (parse_plain(a_at, a.len()), parse_plain(b_at, b.len()));
                        assert_eq!(parse_both(a_at, a.len(), b_at, b.len(), &mut dates), (plain_a, utc_pair(plain_b)), "round {round}: {a:?} with {b:?}");
                        assert_eq!(parse_both(a_at, a.len(), b_at, b.len(), &mut Dates::new()), (plain_a, utc_pair(plain_b)), "fresh: {a:?} with {b:?}");
                        seen[usize::from(plain_a != BAD) * 2 + usize::from(plain_b != BAD)] += 1;
                    }
                }
            }
        }
        assert!(seen.iter().all(|&n| n >= 100), "{seen:?}");
    }

    #[test]
    fn nothing_matches_an_empty_date_slot() {
        for text in ["\0\0\0\0\0\0\0\0\0\0\010:11:12Z", "\0\0\0\0\0\0\0\0\0\0T10:11:12Z", "\0\0\0\0\0\0\0\000T10:11:12Z"] {
            assert_eq!(ts(text), BAD, "{text:?}");
        }
    }

    #[test]
    fn bad_forms_are_bad() {
        for text in [
            "", "2026-10-03T23:58:10.000Z", "2026-10-03T23:58:10z", "2026-10-03t23:58:10Z",
            "2026-10-03 23:58:10Z", "2026-10-03T23:58:10", "1791071890000.0", "1.79e12", "-5",
            "2026-13-03T23:58:10Z", "2026-02-30T23:58:10Z", "2027-02-29T23:58:10Z", "2100-02-29T00:00:00Z",
            "2026-10-03T24:58:10Z", "2026-10-03T23:60:10Z", "2026-10-03T23:58:60Z", "2026-00-03T23:58:10Z",
            "2026-10-00T23:58:10Z", "2026-10-32T23:58:10Z", "2026-04-31T00:00:00Z",
            "2026-10-03T19:58:10+24:00", "2026-10-03T19:58:10+05:60", "2026-10-03T19:58:10+0200",
            "2026-10-03T19:58:10 04:00", "2026-10-03T19:58:10+4:00", "2026-10-03T19:58:10+04:0a",
            "179102160000000000000", "１791071890000", "١٧٩١٠٢١٦٠٠٠٠٠", "2026-10-03T23:58:10Z\r",
            "1791071890000\r", "+1791071890000", " 1791071890000", "2026/10/03T23:58:10Z", "20261003T235810Z",
            "２026-10-03T10:00:00Z", "2026-1０-03T10:0Z",
        ] {
            assert_eq!(ts(text), BAD, "{text:?} should be bad");
        }
    }
}
