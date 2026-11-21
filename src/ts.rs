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
#[inline(always)]
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

/// Epoch milliseconds of the `len` bytes at `p`, or BAD. Reads at most `len` bytes.
#[inline(always)]
pub unsafe fn parse(p: *const u8, len: usize) -> i64 {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(text: &str) -> i64 {
        unsafe { parse(text.as_ptr(), text.len()) }
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
