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

    /// One line with the right number of commas: `data[start..end]`, whose commas
    /// are at `commas[0..COMMAS]`, and which holds `quotes` double quotes.
    unsafe fn line(&mut self, data: *const u8, start: usize, commas: *const u32, end: usize, quotes: u32);
}

/// Shrinks a field to what is inside its quotes, if it is wrapped in a pair. Returns 1 if it was.
#[inline(always)]
pub unsafe fn unquote(data: *const u8, start: &mut usize, end: &mut usize) -> u32 {
    let wrapped = (*end - *start >= 2) & (*data.add(*start) == b'"') & (*data.add(*end).sub(1) == b'"');
    *start += wrapped as usize;
    *end -= wrapped as usize;
    wrapped as u32
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
        let count = index.newline_count;
        let newlines = index.newlines.as_ptr();
        let commas = index.commas.as_ptr();

        let mut line = 0usize; // which newline ends the next line
        let mut start = 0usize; // where the next line starts
        if skip {
            if count == 0 {
                if at_end {
                    return;
                }
                continue; // still inside the line to skip: read on
            }
            start = unsafe { *newlines } as usize + 1;
            line = 1;
            skip = false;
        }
        // Lines starting at or after `stop` belong to the next range.
        let stop = usize::try_from(range_end - base).unwrap_or(usize::MAX);
        let mut comma = 0usize;
        let mut done = false;
        unsafe {
            while *commas.add(comma) < start as u32 {
                comma += 1;
            }
            let mut quotes_before = index.quotes_before(start);
            while line < count {
                if start >= stop {
                    done = true;
                    break;
                }
                let end = *newlines.add(line) as usize;
                let quotes_at_end = index.quotes_before(end);
                let quotes = quotes_at_end - quotes_before;
                quotes_before = quotes_at_end;
                let end32 = end as u32;
                if *commas.add(comma + L::COMMAS - 1) < end32 && *commas.add(comma + L::COMMAS) > end32 {
                    sink.line(data, start, commas.add(comma), end, quotes);
                    comma += L::COMMAS;
                } else {
                    // Wrong number of fields (an empty line has none): malformed, dropped.
                    while *commas.add(comma) < end32 {
                        comma += 1;
                    }
                }
                start = end + 1;
                line += 1;
            }
        }
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
