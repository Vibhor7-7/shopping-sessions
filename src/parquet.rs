//! A minimal Parquet writer: one row group, one PLAIN, uncompressed data page
//! per column. That is all two flat tables need, it is the most widely read
//! corner of the format, and a PLAIN page of integers is the column as it lies
//! in memory: the file is never put together here, only handed to the kernel
//! piece by piece.
//!
//! The file layout and the Thrift compact encoding of the footer follow
//! https://parquet.apache.org/docs/file-format/ and parquet.thrift.

use std::borrow::Cow;
use std::io::{ErrorKind, IoSlice, Write};
use std::path::Path;

// parquet.thrift: Type
const INT32: i32 = 1;
const INT64: i32 = 2;
const BYTE_ARRAY: i32 = 6;
// parquet.thrift: ConvertedType
const UTF8: i32 = 0;
const DATE: i32 = 6;
// parquet.thrift: Encoding
const PLAIN: i32 = 0;
const RLE: i32 = 3;

// Thrift compact protocol type codes.
const T_I32: u8 = 5;
const T_I64: u8 = 6;
const T_BINARY: u8 = 8;
const T_LIST: u8 = 9;
const T_STRUCT: u8 = 12;

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Text,
    Date,
    Int32,
    Int64,
}

struct Column {
    name: &'static str,
    kind: Kind,
    optional: bool,
    values: i64,
    offset: i64,
    size: i64,
}

/// A Thrift compact-protocol writer, enough for Parquet's page headers and footer.
struct Thrift<'a> {
    out: &'a mut Vec<u8>,
    last: i16,
    stack: Vec<i16>,
}

impl<'a> Thrift<'a> {
    fn new(out: &'a mut Vec<u8>) -> Self {
        Self { out, last: 0, stack: Vec::new() }
    }

    fn varint(&mut self, mut value: u64) {
        while value >= 0x80 {
            self.out.push(value as u8 | 0x80);
            value >>= 7;
        }
        self.out.push(value as u8);
    }

    fn zigzag(&mut self, value: i64) {
        self.varint(((value << 1) ^ (value >> 63)) as u64);
    }

    fn header(&mut self, id: i16, kind: u8) {
        let delta = id - self.last;
        if (1..=15).contains(&delta) {
            self.out.push(((delta as u8) << 4) | kind);
        } else {
            self.out.push(kind);
            self.zigzag(i64::from(id));
        }
        self.last = id;
    }

    fn i32(&mut self, id: i16, value: i32) {
        self.header(id, T_I32);
        self.zigzag(i64::from(value));
    }

    fn i64(&mut self, id: i16, value: i64) {
        self.header(id, T_I64);
        self.zigzag(value);
    }

    fn string(&mut self, id: i16, value: &str) {
        self.header(id, T_BINARY);
        self.varint(value.len() as u64);
        self.out.extend_from_slice(value.as_bytes());
    }

    fn list(&mut self, id: i16, element: u8, len: usize) {
        self.header(id, T_LIST);
        if len < 15 {
            self.out.push(((len as u8) << 4) | element);
        } else {
            self.out.push(0xf0 | element);
            self.varint(len as u64);
        }
    }

    /// Starts a struct that is a field of the current one.
    fn begin(&mut self, id: i16) {
        self.header(id, T_STRUCT);
        self.stack.push(self.last);
        self.last = 0;
    }

    /// Starts a struct that is a list element.
    fn element(&mut self) {
        self.stack.push(self.last);
        self.last = 0;
    }

    fn end(&mut self) {
        self.out.push(0);
        self.last = self.stack.pop().unwrap();
    }
}

/// PLAIN encoding of fixed-width integers is their little-endian bytes: on the
/// little-endian targets this builds for, the slice itself.
#[cfg(target_endian = "little")]
fn little_endian<T: Copy>(values: &[T]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(values.as_ptr() as *const u8, std::mem::size_of_val(values)) }
}

/// Writes `pieces` one after another, in as few system calls as the kernel takes them in.
fn write_pieces(file: &mut std::fs::File, pieces: &[Cow<[u8]>]) -> std::io::Result<()> {
    let mut slices: Vec<IoSlice> = pieces.iter().filter(|piece| !piece.is_empty()).map(|piece| IoSlice::new(piece)).collect();
    let mut left = &mut slices[..];
    while !left.is_empty() {
        match file.write_vectored(left) {
            Ok(0) => return Err(ErrorKind::WriteZero.into()),
            Ok(written) => IoSlice::advance_slices(&mut left, written),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            // A kernel that refuses this much in one call (macOS, past 2 GB) still takes it a piece at a time.
            Err(e) if e.kind() == ErrorKind::InvalidInput => {
                for slice in left.iter() {
                    file.write_all(slice)?;
                }
                return Ok(());
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Builds one Parquet file. Add every column with the same number of rows, then `write`.
///
/// The file is a list of pieces: the page headers and the footer, made here,
/// and between them the columns where the caller keeps them. Nothing is copied
/// until `write` gives the list to the kernel.
pub struct File<'a> {
    pieces: Vec<Cow<'a, [u8]>>,
    /// The bytes in `pieces`: where in the file the next one starts.
    len: usize,
    columns: Vec<Column>,
    rows: i64,
}

impl<'a> File<'a> {
    pub fn new(rows: usize) -> Self {
        let mut file = Self { pieces: Vec::new(), len: 0, columns: Vec::new(), rows: rows as i64 };
        file.piece(Cow::Borrowed(&b"PAR1"[..]));
        file
    }

    fn piece(&mut self, piece: Cow<'a, [u8]>) {
        self.len += piece.len();
        self.pieces.push(piece);
    }

    /// A column whose page body is `body`, piece after piece: for a required column the PLAIN values, nothing else.
    fn column<const N: usize>(&mut self, name: &'static str, kind: Kind, optional: bool, body: [Cow<'a, [u8]>; N]) {
        let offset = self.len;
        if self.rows > 0 {
            let body_len: usize = body.iter().map(|piece| piece.len()).sum();
            let mut bytes = Vec::with_capacity(32);
            let mut header = Thrift::new(&mut bytes);
            header.i32(1, 0); // DATA_PAGE
            header.i32(2, body_len as i32);
            header.i32(3, body_len as i32);
            header.begin(5);
            header.i32(1, self.rows as i32);
            header.i32(2, PLAIN);
            header.i32(3, RLE);
            header.i32(4, RLE);
            header.end();
            header.out.push(0);
            self.piece(Cow::Owned(bytes));
            for piece in body {
                self.piece(piece);
            }
        }
        let size = (self.len - offset) as i64;
        self.columns.push(Column { name, kind, optional, values: self.rows, offset: offset as i64, size });
    }

    pub fn int32(&mut self, name: &'static str, kind: Kind, values: &'a [i32]) {
        assert_eq!(values.len() as i64, self.rows);
        self.column(name, kind, false, [Cow::Borrowed(little_endian(values))]);
    }

    pub fn int64(&mut self, name: &'static str, values: &'a [i64]) {
        assert_eq!(values.len() as i64, self.rows);
        self.column(name, Kind::Int64, false, [Cow::Borrowed(little_endian(values))]);
    }

    /// Non-negative counts: 32-bit when they all fit (the checker takes any integer width), 64-bit otherwise.
    pub fn counts(&mut self, name: &'static str, values: &'a [u32]) {
        assert_eq!(values.len() as i64, self.rows);
        if values.iter().all(|&v| v <= i32::MAX as u32) {
            self.column(name, Kind::Int32, false, [Cow::Borrowed(little_endian(values))]);
        } else {
            let wide: Vec<u8> = values.iter().flat_map(|&v| i64::from(v).to_le_bytes()).collect();
            self.column(name, Kind::Int64, false, [Cow::Owned(wide)]);
        }
    }

    /// A required string column. `plain` is the values already PLAIN-encoded: a 4-byte length, then the bytes, each.
    pub fn text(&mut self, name: &'static str, plain: &'a [u8]) {
        self.column(name, Kind::Text, false, [Cow::Borrowed(plain)]);
    }

    /// A nullable string column: `present[i]` says whether row i has a value; `plain` holds the present ones.
    pub fn optional_text(&mut self, name: &'static str, present: &[bool], plain: &'a [u8]) {
        assert_eq!(present.len() as i64, self.rows);
        // Definition levels, 1 bit each, as one bit-packed run of the RLE/bit-packing hybrid,
        // their length in four bytes before them.
        let groups = present.len().div_ceil(8);
        let mut levels = Vec::with_capacity(groups + 16);
        levels.extend_from_slice(&[0; 4]);
        let mut run = (groups as u64) << 1 | 1;
        while run >= 0x80 {
            levels.push(run as u8 | 0x80);
            run >>= 7;
        }
        levels.push(run as u8);
        for group in present.chunks(8) {
            let mut byte = 0u8;
            for (bit, &is) in group.iter().enumerate() {
                byte |= u8::from(is) << bit;
            }
            levels.push(byte);
        }
        let len = (levels.len() - 4) as u32;
        levels[..4].copy_from_slice(&len.to_le_bytes());
        self.column(name, Kind::Text, true, [Cow::Owned(levels), Cow::Borrowed(plain)]);
    }

    pub fn write(mut self, path: &Path) {
        let total: i64 = self.columns.iter().map(|c| c.size).sum();
        let mut footer = Vec::with_capacity(1024);
        let mut meta = Thrift::new(&mut footer);
        meta.i32(1, 1); // version
        meta.list(2, T_STRUCT, self.columns.len() + 1);
        meta.element();
        meta.i32(3, 0); // REQUIRED
        meta.string(4, "schema");
        meta.i32(5, self.columns.len() as i32);
        meta.end();
        for column in &self.columns {
            meta.element();
            meta.i32(1, match column.kind {
                Kind::Text => BYTE_ARRAY,
                Kind::Date | Kind::Int32 => INT32,
                Kind::Int64 => INT64,
            });
            meta.i32(3, i32::from(column.optional));
            meta.string(4, column.name);
            match column.kind {
                Kind::Text => {
                    meta.i32(6, UTF8);
                    meta.begin(10); // LogicalType
                    meta.begin(1); // STRING
                    meta.end();
                    meta.end();
                }
                Kind::Date => {
                    meta.i32(6, DATE);
                    meta.begin(10);
                    meta.begin(6); // DATE
                    meta.end();
                    meta.end();
                }
                Kind::Int32 | Kind::Int64 => {}
            }
            meta.end();
        }
        meta.i64(3, self.rows);
        if self.rows == 0 {
            meta.list(4, T_STRUCT, 0);
        } else {
            meta.list(4, T_STRUCT, 1);
            meta.element(); // RowGroup
            meta.list(1, T_STRUCT, self.columns.len());
            for column in &self.columns {
                meta.element(); // ColumnChunk
                meta.i64(2, column.offset);
                meta.begin(3); // ColumnMetaData
                meta.i32(1, match column.kind {
                    Kind::Text => BYTE_ARRAY,
                    Kind::Date | Kind::Int32 => INT32,
                    Kind::Int64 => INT64,
                });
                meta.list(2, T_I32, 2);
                meta.zigzag(i64::from(PLAIN));
                meta.zigzag(i64::from(RLE));
                meta.list(3, T_BINARY, 1);
                meta.varint(column.name.len() as u64);
                meta.out.extend_from_slice(column.name.as_bytes());
                meta.i32(4, 0); // UNCOMPRESSED
                meta.i64(5, column.values);
                meta.i64(6, column.size);
                meta.i64(7, column.size);
                meta.i64(9, column.offset);
                meta.end();
                meta.end();
            }
            meta.i64(2, total);
            meta.i64(3, self.rows);
            meta.end();
        }
        meta.string(6, "shopping-sessions");
        meta.out.push(0);
        let footer_len = footer.len() as u32;
        footer.extend_from_slice(&footer_len.to_le_bytes());
        footer.extend_from_slice(b"PAR1");
        self.piece(Cow::Owned(footer));
        use std::os::unix::fs::OpenOptionsExt;
        let written = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o666)
            .open(path)
            .and_then(|mut file| write_pieces(&mut file, &self.pieces));
        if let Err(e) = written {
            eprintln!("pipeline: cannot write {}: {e}", path.display());
            std::process::exit(1);
        }
    }
}
