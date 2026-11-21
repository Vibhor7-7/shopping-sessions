//! Reads one byte range of a CSV file and hands every line that starts in it,
//! split at its commas, to a sink.
//!
//! A file is cut into ranges so several threads can share it. A line belongs to
//! the range its first byte is in: a range skips the line it starts inside (or
//! the header, for the range at offset 0) and reads past its own end to finish
//! its last line. That is exact because only `\n` ends a line.

use std::fs::File;
use std::os::unix::fs::FileExt;

use crate::scan::Index;

/// Bytes kept readable before the data, so `data[-1]` is always in bounds.
const FRONT: usize = 64;
/// Bytes read per call: small enough to stay in the L2 cache while it is parsed.
const BLOCK: usize = 1 << 18;

pub struct ReadBuf {
    bytes: Vec<u8>,
    index: Index,
    /// Bytes per read: BLOCK, unless a test asks for something tiny to put block edges everywhere.
    block: usize,
}

impl ReadBuf {
    pub fn new() -> Self {
        let block = std::env::var("SS_BLOCK_BYTES").ok().and_then(|v| v.parse().ok()).unwrap_or(BLOCK).clamp(16, 1 << 24);
        let capacity = 2 * block;
        Self { bytes: vec![0; FRONT + capacity + 128], index: Index::new(capacity + 64), block }
    }

    /// Room for `len` bytes of data plus a newline and the vector overread.
    fn reserve(&mut self, len: usize) {
        if FRONT + len + 128 > self.bytes.len() {
            let capacity = (len + 1).next_power_of_two();
            self.bytes.resize(FRONT + capacity + 128, 0);
            self.index = Index::new(capacity + 64);
        }
    }
}

pub trait Lines {
    /// Commas in a well-formed line: one fewer than its fields.
    const COMMAS: usize;

    /// What may be one line: `delims[0]` is the offset of the newline before it
    /// (`u32::MAX`, which is -1, for the line at offset 0) and `delims[1..]` are
    /// the delimiters that follow, at least `COMMAS + 1` of them. Stage one does
    /// not say which of those are commas, so that is settled here, from the bytes:
    /// the line has the right number of fields exactly when the first `COMMAS`
    /// are commas and the next one is a newline. If so, the line is taken
    /// (`quotes` is how many double quotes lie between `delims[0]` and
    /// `delims[COMMAS + 1]`) and the answer is `true`. If not, nothing is done
    /// and the answer is `false`.
    unsafe fn line(&mut self, data: *const u8, delims: *const u32, quotes: u32) -> bool;
}

/// Whether delimiters 1 to `N - 1` after `delims[0]` are commas and delimiter
/// `N` is a newline: a line of `N` fields. All of them must be real delimiters.
#[inline(always)]
pub unsafe fn is_line<const N: usize>(data: *const u8, delims: *const u32) -> bool {
    let mut ok = *data.add(*delims.add(N) as usize) == b'\n';
    for comma in 1..N {
        ok &= *data.add(*delims.add(comma) as usize) != b'\n';
    }
    ok
}

/// A field's bounds once the quotes around it, if it is wrapped in a pair, are
/// left out; and 1 if it was wrapped. By value, so the bounds stay in registers.
#[inline(always)]
pub unsafe fn unquoted(data: *const u8, start: usize, end: usize) -> (usize, usize, u32) {
    let wrapped = (end - start >= 2) & (*data.add(start) == b'"') & (*data.add(end).sub(1) == b'"');
    (start + wrapped as usize, end - wrapped as usize, wrapped as u32)
}

/// Where each field of a line is, the quotes around the wrapped ones left out.
#[repr(C, align(32))]
pub struct Fields {
    pub start: [u32; 8],
    pub len: [u32; 8],
}

impl Fields {
    #[inline(always)]
    pub fn new() -> Self {
        Self { start: [0; 8], len: [0; 8] }
    }

    #[inline(always)]
    pub fn at(&self, field: usize) -> usize {
        self.start[field] as usize
    }

    #[inline(always)]
    pub fn len(&self, field: usize) -> usize {
        self.len[field] as usize
    }
}

/// Splits what may be a line of `N` fields (at most 7) at its delimiters,
/// `delims` as `Lines::line` gets them. Returns how many fields were wrapped in
/// quotes, and whether this is a line of `N` fields at all (`is_line`); if it is
/// not, the bounds and the count mean nothing.
/// All fields at once: the four bytes around each delimiter hold the last
/// character of the field before it, the delimiter itself and the first
/// character of the field after it. With those side by side, the seven "is this
/// field wrapped" questions and the eight "is this a comma or a newline" ones
/// are a handful of vector instructions instead of rounds of loads and compares.
/// Reads `delims[0..9]`, and the 4 bytes from one before each of those offsets:
/// a slot past the last delimiter holds `u32::MAX`, and reads the bytes before
/// the data, which the reader keeps readable and which never hold a newline.
#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
#[inline(always)]
pub unsafe fn split<const N: usize>(data: *const u8, delims: *const u32, fields: &mut Fields) -> (u32, bool) {
    use std::arch::x86_64::*;
    let around = |i: usize| (data.offset(*delims.add(i) as i32 as isize - 1) as *const i32).read_unaligned();
    let low = _mm_insert_epi32(
        _mm_insert_epi32(_mm_insert_epi32(_mm_cvtsi32_si128(around(0)), around(1), 1), around(2), 2),
        around(3),
        3,
    );
    let high = _mm_insert_epi32(
        _mm_insert_epi32(_mm_insert_epi32(_mm_cvtsi32_si128(around(4)), around(5), 1), around(6), 2),
        around(7),
        3,
    );
    // Lane i: the bytes at delimiter i's offset - 1, +0, +1, +2.
    let around_all = _mm256_inserti128_si256(_mm256_castsi128_si256(low), high, 1);
    // Byte 1 of lane i is delimiter i itself: bit 4i + 1 says it is a newline.
    // Lanes 1 to N - 1 must be commas and lane N the newline that ends the line.
    let newlines = _mm256_movemask_epi8(_mm256_cmpeq_epi8(around_all, _mm256_set1_epi8(b'\n' as i8))) as u32;
    let is_line = newlines & (0x2222_2220 & (u32::MAX >> (28 - 4 * N))) == 2 << (4 * N);
    let quote = _mm256_cmpeq_epi8(around_all, _mm256_set1_epi8(b'"' as i8));
    // Field i lies between delimiters i and i + 1: it opens with the byte after
    // the first (byte 2 of lane i) and closes with the byte before the second
    // (byte 0 of lane i + 1).
    let closes = _mm256_permutevar8x32_epi32(quote, _mm256_setr_epi32(1, 2, 3, 4, 5, 6, 7, 7));
    let both = _mm256_and_si256(_mm256_srli_epi32(quote, 16), closes);
    let before = _mm256_loadu_si256(delims as *const __m256i);
    let after = _mm256_loadu_si256(delims.add(1) as *const __m256i);
    let one = _mm256_set1_epi32(1);
    let len = _mm256_sub_epi32(_mm256_sub_epi32(after, before), one);
    // Wrapped: a quote at both ends, and they are two different bytes.
    let wrapped = _mm256_and_si256(_mm256_and_si256(both, _mm256_cmpgt_epi32(len, one)), one);
    _mm256_store_si256(fields.start.as_mut_ptr() as *mut __m256i, _mm256_add_epi32(_mm256_add_epi32(before, one), wrapped));
    _mm256_store_si256(fields.len.as_mut_ptr() as *mut __m256i, _mm256_sub_epi32(len, _mm256_add_epi32(wrapped, wrapped)));
    let bits = _mm256_movemask_ps(_mm256_castsi256_ps(_mm256_slli_epi32(wrapped, 31))) as u32;
    ((bits & ((1 << N) - 1)).count_ones(), is_line)
}

#[cfg(not(all(target_arch = "x86_64", target_feature = "avx2")))]
#[inline(always)]
pub unsafe fn split<const N: usize>(data: *const u8, delims: *const u32, fields: &mut Fields) -> (u32, bool) {
    if !is_line::<N>(data, delims) {
        return (0, false);
    }
    let mut count = 0;
    for field in 0..N {
        let start = (*delims.add(field)).wrapping_add(1) as usize;
        let (from, to, wrapped) = unquoted(data, start, *delims.add(field + 1) as usize);
        fields.start[field] = from as u32;
        fields.len[field] = (to - from) as u32;
        count += wrapped;
    }
    (count, true)
}

fn read_fully(file: &File, mut into: &mut [u8], mut at: u64) -> usize {
    let mut total = 0;
    while !into.is_empty() {
        match file.read_at(into, at) {
            Ok(0) => break,
            Ok(n) => {
                total += n;
                at += n as u64;
                into = &mut into[n..];
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => {
                eprintln!("pipeline: read failed: {e}");
                std::process::abort();
            }
        }
    }
    total
}

/// The first delimiter from `delims[from]` on that is a newline, or -1 if the
/// rest of the buffer holds none.
#[inline(always)]
unsafe fn next_newline(data: *const u8, delims: *const u32, mut from: isize) -> isize {
    loop {
        let at = *delims.offset(from);
        if at == u32::MAX {
            return -1;
        }
        if *data.add(at as usize) == b'\n' {
            return from;
        }
        from += 1;
    }
}

/// Feeds `sink` the lines of one indexed buffer, from the one at `start` (the
/// delimiter just before it is number `before`) up to `stop` or the last whole
/// line. Returns where the first line not taken starts, and whether that is
/// because `stop` was reached.
///
/// A function of its own, never inlined: this loop runs once a line with
/// everything `Lines::line` does inlined into it, and inside `walk` what `walk`
/// keeps between two reads (the file, the lengths, the read buffer) competed
/// with it for registers and was put on the stack and fetched back every line
/// (BENCH.md, experiment 38).
#[inline(never)]
unsafe fn lines<L: Lines>(data: *const u8, index: &Index, mut start: usize, mut before: isize, stop: usize, sink: &mut L) -> (usize, bool) {
    let delims = index.delims();
    let mut quotes_before = index.quotes_before(start);
    loop {
        if start >= stop {
            return (start, true);
        }
        // If this is a line with the right number of fields, its newline is
        // the delimiter COMMAS + 1 after the one before it. A slot past the
        // last delimiter holds `u32::MAX`: fewer delimiters than that are left.
        let end = *delims.offset(before + L::COMMAS as isize + 1);
        if end != u32::MAX {
            let quotes_at_end = index.quotes_before(end as usize);
            if sink.line(data, delims.offset(before), quotes_at_end - quotes_before) {
                quotes_before = quotes_at_end;
                before += L::COMMAS as isize + 1;
                start = end as usize + 1;
                continue;
            }
        }
        // Not such a line. If it ends in this buffer it has the wrong number
        // of fields (an empty line has none): malformed, dropped. If it does
        // not, it is the unfinished line at the end of what has been read.
        let newline = next_newline(data, delims, before + 1);
        if newline < 0 {
            return (start, false);
        }
        before = newline;
        start = *delims.offset(newline) as usize + 1;
        quotes_before = index.quotes_before(start);
    }
}

/// Feeds `sink` every data line of `file` that starts in `offset..offset + length`.
pub fn walk<L: Lines>(file: &File, offset: u64, length: u64, buf: &mut ReadBuf, sink: &mut L) {
    let range_end = offset + length;
    // Start one byte early: if that byte is a newline, a line starts exactly at `offset`.
    let mut next_read = offset.saturating_sub(1);
    let mut base = next_read; // file offset of data[0]
    let mut len = 0usize; // bytes of data held
    let mut skip = true; // the header, or the line this range starts inside
    let mut at_end = false;
    loop {
        if !at_end {
            let block = buf.block;
            buf.reserve(len + block);
            let got = read_fully(file, &mut buf.bytes[FRONT + len..FRONT + len + block], next_read);
            next_read += got as u64;
            len += got;
            if got < block {
                at_end = true;
                // A last line with no newline is still a line.
                if len > 0 && buf.bytes[FRONT + len - 1] != b'\n' {
                    buf.bytes[FRONT + len] = b'\n';
                    len += 1;
                }
            }
        }
        let data = unsafe { buf.bytes.as_ptr().add(FRONT) };
        unsafe { buf.index.build(data, len) };
        let index = &buf.index;
        let delims = index.delims();

        let mut start = 0usize; // where the next line starts
        // Which delimiter is the one just before `start`: -1 is the slot in front, the delimiter at offset -1.
        let mut before = -1isize;
        if skip {
            // The first newline ends the header, or the line this range starts inside.
            let first = unsafe { next_newline(data, delims, 0) };
            if first < 0 {
                if at_end {
                    return;
                }
                continue; // still inside the line to skip: read on
            }
            before = first;
            start = unsafe { *delims.offset(first) } as usize + 1;
            skip = false;
        }
        // Lines starting at or after `stop` belong to the next range.
        let stop = usize::try_from(range_end - base).unwrap_or(usize::MAX);
        let (start, done) = unsafe { lines(data, index, start, before, stop, sink) };
        if done || at_end || base + start as u64 >= range_end {
            return;
        }
        // Keep the unfinished line and read more behind it.
        buf.bytes.copy_within(FRONT + start..FRONT + len, FRONT);
        len -= start;
        base += start as u64;
    }
}

/// An input file and the ranges it is read in.
pub struct Input {
    pub file: File,
    pub len: u64,
}

/// The `*.csv` files directly in `dir`, opened. A missing folder holds none.
pub fn open_folder(dir: &std::path::Path) -> Vec<Input> {
    let mut paths = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.as_encoded_bytes();
            if name.ends_with(b".csv") && !name.starts_with(b".") {
                paths.push(entry.path());
            }
        }
    }
    paths.sort();
    let mut inputs = Vec::new();
    for path in paths {
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        if !meta.is_file() {
            continue;
        }
        match File::open(&path) {
            Ok(file) => inputs.push(Input { file, len: meta.len() }),
            Err(e) => {
                eprintln!("pipeline: cannot open {}: {e}", path.display());
                std::process::abort();
            }
        }
    }
    inputs
}

/// A test's override of how many bytes each range of a file holds.
pub fn job_bytes() -> Option<u64> {
    std::env::var("SS_JOB_BYTES").ok()?.parse::<u64>().ok().map(|bytes| bytes.max(1))
}

#[derive(Clone, Copy)]
pub struct Job {
    pub file: u32,
    pub offset: u64,
    pub length: u64,
}

/// Cuts every file into ranges of about `target` bytes, largest files first so
/// the long ones are not what the last thread is left holding.
pub fn jobs(inputs: &[Input], target: u64) -> Vec<Job> {
    let mut order: Vec<usize> = (0..inputs.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(inputs[i].len));
    let mut jobs = Vec::new();
    for i in order {
        let len = inputs[i].len;
        let pieces = len.div_ceil(target).max(1);
        let size = len.div_ceil(pieces).max(1);
        let mut offset = 0;
        while offset < len {
            let length = size.min(len - offset);
            jobs.push(Job { file: i as u32, offset, length });
            offset += length;
        }
    }
    jobs
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

    /// What a sink is owed for one line: its bytes, its quotes, and each field
    /// without the quotes wrapped around it.
    #[derive(Debug, PartialEq)]
    struct Seen {
        line: Vec<u8>,
        quotes: u32,
        wrapped: u32,
        fields: Vec<Vec<u8>>,
    }

    /// Takes lines of `N` fields through `split`, as the event and click sinks do.
    struct Split<const N: usize>(Vec<Seen>);
    /// Takes lines of `N` fields through `is_line` and `unquoted`, as the link sink does.
    struct Plain<const N: usize>(Vec<Seen>);

    unsafe fn seen(data: *const u8, delims: *const u32, fields: usize, quotes: u32, wrapped: u32, bounds: Vec<(usize, usize)>) -> Seen {
        let (from, to) = ((*delims).wrapping_add(1) as usize, *delims.add(fields) as usize);
        Seen {
            line: std::slice::from_raw_parts(data.add(from), to - from).to_vec(),
            quotes,
            wrapped,
            fields: bounds.iter().map(|&(at, len)| std::slice::from_raw_parts(data.add(at), len).to_vec()).collect(),
        }
    }

    macro_rules! split_sink {
        ($n:literal) => {
            impl Lines for Split<$n> {
                const COMMAS: usize = $n - 1;
                unsafe fn line(&mut self, data: *const u8, delims: *const u32, quotes: u32) -> bool {
                    let mut fields = Fields::new();
                    let (wrapped, shaped) = split::<$n>(data, delims, &mut fields);
                    assert_eq!(shaped, is_line::<$n>(data, delims));
                    if shaped {
                        let bounds = (0..$n).map(|i| (fields.at(i), fields.len(i))).collect();
                        self.0.push(seen(data, delims, $n, quotes, wrapped, bounds));
                    }
                    shaped
                }
            }
            impl Lines for Plain<$n> {
                const COMMAS: usize = $n - 1;
                unsafe fn line(&mut self, data: *const u8, delims: *const u32, quotes: u32) -> bool {
                    if !is_line::<$n>(data, delims) {
                        return false;
                    }
                    let mut wrapped = 0;
                    let mut bounds = Vec::new();
                    for field in 0..$n {
                        let (from, to, w) = unquoted(data, (*delims.add(field)).wrapping_add(1) as usize, *delims.add(field + 1) as usize);
                        bounds.push((from, to - from));
                        wrapped += w;
                    }
                    self.0.push(seen(data, delims, $n, quotes, wrapped, bounds));
                    true
                }
            }
        };
    }
    split_sink!(2);
    split_sink!(4);
    split_sink!(7);

    /// The definition: every line after the first that starts in the range and has `fields` fields.
    fn expected(content: &[u8], fields: usize) -> Vec<Seen> {
        let mut lines: Vec<&[u8]> = content.split(|&b| b == b'\n').collect();
        if content.is_empty() || content.ends_with(b"\n") {
            lines.pop(); // nothing after the last newline: not a line
        }
        let mut found = Vec::new();
        for line in lines.into_iter().skip(1) {
            let parts: Vec<&[u8]> = line.split(|&b| b == b',').collect();
            if parts.len() != fields {
                continue;
            }
            let mut wrapped = 0;
            let unwrapped = parts
                .iter()
                .map(|part| {
                    if part.len() >= 2 && part[0] == b'"' && part[part.len() - 1] == b'"' {
                        wrapped += 1;
                        part[1..part.len() - 1].to_vec()
                    } else {
                        part.to_vec()
                    }
                })
                .collect();
            found.push(Seen { line: line.to_vec(), quotes: line.iter().filter(|&&b| b == b'"').count() as u32, wrapped, fields: unwrapped });
        }
        found
    }

    fn read_with<L: Lines>(input: &Input, block: usize, target: u64, sink: &mut L) {
        // Ranges in file order, each with a buffer of its own or one that has been used before.
        let mut pieces = jobs(std::slice::from_ref(input), target);
        pieces.sort_by_key(|job| job.offset);
        let capacity = 2 * block;
        let mut buf = ReadBuf { bytes: vec![0; FRONT + capacity + 128], index: Index::new(capacity + 64), block };
        for job in pieces {
            walk(&input.file, job.offset, job.length, &mut buf, sink);
        }
    }

    #[test]
    fn every_line_reaches_its_sink_once_whatever_the_blocks_and_ranges() {
        let path = std::env::temp_dir().join(format!("shopping-sessions-walk-{}.csv", std::process::id()));
        let mut state = 2026;
        let mut lines_seen = [0usize; 3];
        for round in 0..1_500 {
            // Fields are short, so that lines of 2, 4 and 7 fields all turn up, with
            // empty lines, quotes in and out of place, and long lines now and then.
            let len = (next(&mut state) % 900) as usize;
            let newline_every = 6 + next(&mut state) % 60;
            let mut content: Vec<u8> = (0..len)
                .map(|_| {
                    let r = next(&mut state);
                    match r % newline_every {
                        0 => b'\n',
                        1..=4 if r >> 20 & 1 == 0 => b',',
                        5 if r >> 21 & 3 == 0 => b'"',
                        6 if r >> 23 & 7 == 0 => b'\r',
                        _ => b'a' + (r >> 32) as u8 % 26,
                    }
                })
                .collect();
            if round % 3 == 0 && !content.is_empty() {
                *content.last_mut().unwrap() = b'\n';
            }
            // Wrap some fields in quotes, as the generator does.
            let mut at = 0;
            while at < content.len() {
                let end = content[at..].iter().position(|&b| b == b',' || b == b'\n').map_or(content.len(), |p| at + p);
                if next(&mut state) % 5 == 0 && end - at >= 2 {
                    content[at] = b'"';
                    content[end - 1] = b'"';
                }
                at = end + 1;
            }
            std::fs::write(&path, &content).unwrap();
            let input = Input { file: File::open(&path).unwrap(), len: content.len() as u64 };
            let whole = content.len() as u64 + 1;
            for (block, target) in [(16, whole), (17, 7), (16, 1), (64, 50), (61, 33), (128, whole), (1000, 129), (4096, whole)] {
                let mut two = Plain::<2>(Vec::new());
                read_with(&input, block, target, &mut two);
                assert_eq!(two.0, expected(&content, 2), "round {round}, 2 fields, block {block}, ranges of {target}");
                let mut two = Split::<2>(Vec::new());
                read_with(&input, block, target, &mut two);
                assert_eq!(two.0, expected(&content, 2), "round {round}, 2 fields split, block {block}, ranges of {target}");
                let mut four = Split::<4>(Vec::new());
                read_with(&input, block, target, &mut four);
                assert_eq!(four.0, expected(&content, 4), "round {round}, 4 fields, block {block}, ranges of {target}");
                let mut four = Plain::<4>(Vec::new());
                read_with(&input, block, target, &mut four);
                assert_eq!(four.0, expected(&content, 4), "round {round}, 4 fields plain, block {block}, ranges of {target}");
                let mut seven = Split::<7>(Vec::new());
                read_with(&input, block, target, &mut seven);
                assert_eq!(seven.0, expected(&content, 7), "round {round}, 7 fields, block {block}, ranges of {target}");
                lines_seen[0] += two.0.len();
                lines_seen[1] += four.0.len();
                lines_seen[2] += seven.0.len();
            }
        }
        std::fs::remove_file(&path).ok();
        assert!(lines_seen.iter().all(|&n| n > 2_000), "{lines_seen:?}");
    }
}
