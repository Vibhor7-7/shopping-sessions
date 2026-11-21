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
    slots: [Date; 32],
}

#[derive(Clone, Copy)]
struct Date {
    /// The eight bytes `YYYY-MM-` and the two bytes `DD`, as read from the text.
    head: u64,
    day: u16,
    /// Epoch seconds of that date's midnight.
    midnight: i64,
}

impl Dates {
    pub fn new() -> Self {
        // No text made of digits and dashes is all zero bytes, and if a line does hold
        // ten zero bytes there, the "hit" answers BAD, which is right.
        Self { slots: [Date { head: 0, day: 0, midnight: BAD }; 32] }
    }

    /// A date not in the table: checks it in full and remembers it if it is real.
    #[cold]
    #[inline(never)]
    fn learn(&mut self, head: u64, day: u16, slot: usize) -> i64 {
        let mut text = [0u8; 19];
        text[..8].copy_from_slice(&head.to_le_bytes());
        text[8..10].copy_from_slice(&day.to_le_bytes());
        text[10..].copy_from_slice(b"T00:00:00");
        let midnight = unsafe { civil_seconds(text.as_ptr()) };
        if midnight != BAD {
            self.slots[slot] = Date { head, day, midnight };
        }
        midnight
    }
}

const ZEROS: u64 = 0x3030_3030_3030_3030;
/// Added to a byte that should be 0 to 9: the top bit stays clear only if it is.
const UP_TO_NINE: u64 = 0x7676_7676_7676_7676;
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
    let slot = (((day & 15) * 10 + ((day >> 8) & 15)) & 31) as usize;
    let known = *dates.slots.get_unchecked(slot);
    let midnight = if known.head == head && known.day == day { known.midnight } else { dates.learn(head, day, slot) };
    if wrong != 0 || hour > 23 || minute > 59 || second > 59 || midnight == BAD {
        return BAD;
    }
    midnight + (hour * 3_600 + minute * 60 + second) as i64
}

/// Eight digit values, the first in the lowest byte, as one number (Lemire's three multiplies).
#[inline(always)]
fn eight_digits(v: u64) -> u64 {
    let v = (v & 0x0f0f_0f0f_0f0f_0f0f).wrapping_mul(2_561) >> 8;
    let v = (v & 0x00ff_00ff_00ff_00ff).wrapping_mul(6_553_601) >> 16;
    (v & 0x0000_ffff_0000_ffff).wrapping_mul(42_949_672_960_001) >> 32
}

/// Thirteen digits: epoch milliseconds as they are written from 2001 to 2286.
#[inline(always)]
unsafe fn thirteen_digits(p: *const u8) -> i64 {
    let first = (p as *const u64).read_unaligned() ^ ZEROS; // digits 0 to 7
    let last = (p.add(5) as *const u64).read_unaligned() ^ ZEROS; // digits 5 to 12
    if ((first | first.wrapping_add(UP_TO_NINE)) | (last | last.wrapping_add(UP_TO_NINE))) & TOP_BITS != 0 {
        return BAD;
    }
    (eight_digits(first) * 100_000 + eight_digits(last) % 100_000) as i64
}

/// Epoch milliseconds of the `len` bytes at `p`, or BAD. Reads at most `len` bytes.
#[inline(always)]
pub unsafe fn parse(p: *const u8, len: usize, dates: &mut Dates) -> i64 {
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

    fn both(text: &[u8], dates: &mut Dates) -> i64 {
        let mut padded = text.to_vec();
        padded.extend_from_slice(&[b',', b'x', 0xff, 0, b'9', b'Z', b'\n', b'"']);
        let fast = unsafe { parse(padded.as_ptr(), text.len(), dates) };
        // Twice: once learning the date, once from the table.
        assert_eq!(unsafe { parse(padded.as_ptr(), text.len(), dates) }, fast, "{:?}", String::from_utf8_lossy(text));
        assert_eq!(unsafe { parse_plain(padded.as_ptr(), text.len()) }, fast, "{:?}", String::from_utf8_lossy(text));
        fast
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
