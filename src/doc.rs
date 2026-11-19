//! Core table model.
//!
//! The file is memory-mapped (or transcoded into memory for multi-byte encodings) and
//! indexed by a background thread that records where each record starts. Nothing is
//! parsed up front. On top of the file sit three thin layers:
//!
//! * `rows`  – order of row ids (`None` = file order). Row id < NEW_ID is a disk record.
//! * `cols`  – order of column ids. Column id < NEW_ID is a field index on disk.
//! * `edits` – overridden cell values keyed by (row id, column id).
//!
//! That makes insert/delete/move/sort/duplicate cheap even for multi-GB files, and saving
//! streams unchanged records straight from disk.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use encoding_rs::Encoding;
use memchr::{memchr, memchr2_iter, memchr_iter};
use memmap2::Mmap;

use crate::jobs::Job;
use crate::table::{col_letter, TableRead};

pub type RowId = u32;
pub type ColId = u32;
pub const NEW_ID: u32 = 1 << 31;

// ---------------------------------------------------------------------------
// Source file
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineEnding {
    Lf,
    CrLf,
}

impl LineEnding {
    pub fn bytes(self) -> &'static [u8] {
        match self {
            LineEnding::Lf => b"\n",
            LineEnding::CrLf => b"\r\n",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            LineEnding::Lf => "LF",
            LineEnding::CrLf => "CRLF",
        }
    }
    pub fn platform() -> Self {
        if cfg!(windows) { LineEnding::CrLf } else { LineEnding::Lf }
    }
}

pub enum Bytes {
    Map(Mmap),
    Owned(Vec<u8>),
    Empty,
}

impl std::ops::Deref for Bytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Bytes::Map(m) => &m[..],
            Bytes::Owned(v) => &v[..],
            Bytes::Empty => &[],
        }
    }
}

/// How to load a file. `None` = auto-detect.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadOpts {
    pub delimiter: Option<u8>,
    pub encoding: Option<&'static Encoding>,
    /// RFC 4180 quote handling. `false` = "Delimiter only" parsing.
    pub quotes: bool,
}

impl Default for LoadOpts {
    fn default() -> Self {
        Self { delimiter: None, encoding: None, quotes: true }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quoting {
    /// RFC 4180: quote only when needed.
    AsNeeded,
    All,
    Never,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SaveOpts {
    pub delimiter: u8,
    pub encoding: &'static Encoding,
    pub line_ending: LineEnding,
    pub quoting: Quoting,
    pub bom: bool,
}

/// Immutable view of the file on disk, shared with background jobs.
pub struct Source {
    pub bytes: Bytes,
    /// Offset of the first record (after a BOM).
    pub start: usize,
    pub delimiter: u8,
    pub quotes: bool,
    pub encoding: &'static Encoding,
    /// True when `bytes` holds UTF-8 transcoded from `encoding`.
    pub transcoded: bool,
    pub had_bom: bool,
    pub line_ending: LineEnding,
    pub trailing_newline: bool,
}

impl Source {
    pub fn empty() -> Source {
        Source {
            bytes: Bytes::Empty,
            start: 0,
            delimiter: b',',
            quotes: true,
            encoding: encoding_rs::UTF_8,
            transcoded: false,
            had_bom: false,
            line_ending: LineEnding::platform(),
            trailing_newline: true,
        }
    }

    pub fn load(path: &Path, opts: &LoadOpts) -> Result<(Source, u64, Option<SystemTime>), String> {
        let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let meta = file.metadata().map_err(|e| e.to_string())?;
        let size = meta.len();
        let mtime = meta.modified().ok();
        let mut bytes = if size == 0 {
            Bytes::Empty
        } else {
            // SAFETY: the file may be modified by another process while mapped; the app
            // watches the modification time and reloads.
            Bytes::Map(unsafe { Mmap::map(&file) }.map_err(|e| e.to_string())?)
        };

        // Encoding
        let data = &bytes[..];
        let (bom_enc, bom_len) = match Encoding::for_bom(data) {
            Some((e, n)) => (Some(e), n),
            None => (None, 0),
        };
        let encoding = opts.encoding.or(bom_enc).unwrap_or_else(|| {
            let sample = &data[..data.len().min(4 << 20)];
            match std::str::from_utf8(sample) {
                Ok(_) => encoding_rs::UTF_8,
                Err(e) if e.error_len().is_none() => encoding_rs::UTF_8,
                Err(_) => encoding_rs::WINDOWS_1252,
            }
        });
        let had_bom = bom_len > 0;
        let mut start = if bom_enc == Some(encoding) { bom_len } else { 0 };
        let mut transcoded = false;
        if encoding != encoding_rs::UTF_8 && !encoding.is_single_byte() {
            // Multi-byte encodings (UTF-16, Shift-JIS, GB18030, …) are transcoded to UTF-8 in
            // memory so the byte-level scanner never splits a character.
            let (text, _) = encoding.decode_without_bom_handling(&data[start..]);
            bytes = Bytes::Owned(text.into_owned().into_bytes());
            start = 0;
            transcoded = true;
        }
        let data = &bytes[..];
        let sample = &data[start..data.len().min(start + (256 << 10))];
        let line_ending = match memchr(b'\n', sample) {
            Some(i) if i > 0 && sample[i - 1] == b'\r' => LineEnding::CrLf,
            Some(_) => LineEnding::Lf,
            None => LineEnding::platform(),
        };
        let delimiter = opts.delimiter.unwrap_or_else(|| detect_delimiter(sample, path, opts.quotes));
        let trailing_newline = data.len() > start && data.last() == Some(&b'\n');
        Ok((
            Source {
                bytes,
                start,
                delimiter,
                quotes: opts.quotes,
                encoding,
                transcoded,
                had_bom,
                line_ending,
                trailing_newline,
            },
            size,
            mtime,
        ))
    }

    /// Raw bytes of record `i`, without its line terminator.
    pub fn record<'a>(&'a self, offsets: &[u64], i: usize) -> &'a [u8] {
        let data = &self.bytes[..];
        let start = offsets[i] as usize;
        let mut end = offsets.get(i + 1).map(|&o| o as usize).unwrap_or(data.len());
        if end > start && data[end - 1] == b'\n' {
            end -= 1;
        }
        if end > start && data[end - 1] == b'\r' {
            end -= 1;
        }
        &data[start..end]
    }

    pub fn decode<'a>(&self, b: &'a [u8]) -> Cow<'a, str> {
        if self.transcoded || self.encoding == encoding_rs::UTF_8 {
            String::from_utf8_lossy(b)
        } else {
            self.encoding.decode_without_bom_handling(b).0
        }
    }

    pub fn fields<'a>(&self, raw: &'a [u8]) -> Fields<'a> {
        Fields::new(raw, self.delimiter, self.quotes)
    }

    pub fn decode_fields(&self, raw: &[u8]) -> Vec<String> {
        self.fields(raw).map(|f| self.decode(&f).into_owned()).collect()
    }
}

pub fn encode_str<'a>(enc: &'static Encoding, s: &'a str) -> Cow<'a, [u8]> {
    if enc == encoding_rs::UTF_16LE {
        Cow::Owned(s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect())
    } else if enc == encoding_rs::UTF_16BE {
        Cow::Owned(s.encode_utf16().flat_map(|u| u.to_be_bytes()).collect())
    } else if enc == encoding_rs::UTF_8 {
        Cow::Borrowed(s.as_bytes())
    } else {
        match enc.encode(s).0 {
            Cow::Borrowed(b) => Cow::Borrowed(b),
            Cow::Owned(v) => Cow::Owned(v),
        }
    }
}

pub fn bom_bytes(enc: &'static Encoding) -> &'static [u8] {
    if enc == encoding_rs::UTF_8 {
        &[0xEF, 0xBB, 0xBF]
    } else if enc == encoding_rs::UTF_16LE {
        &[0xFF, 0xFE]
    } else if enc == encoding_rs::UTF_16BE {
        &[0xFE, 0xFF]
    } else {
        &[]
    }
}

/// Iterator over the fields of one record (RFC 4180, lenient).
pub struct Fields<'a> {
    rec: &'a [u8],
    pos: usize,
    delim: u8,
    quotes: bool,
    done: bool,
}

impl<'a> Fields<'a> {
    pub fn new(rec: &'a [u8], delim: u8, quotes: bool) -> Self {
        Self { rec, pos: 0, delim, quotes, done: false }
    }

    fn finish_after_quote(&mut self, field: Cow<'a, [u8]>, after: usize) -> Cow<'a, [u8]> {
        let r = self.rec;
        let (junk, next) = match memchr(self.delim, &r[after..]) {
            Some(d) => (&r[after..after + d], Some(after + d + 1)),
            None => (&r[after..], None),
        };
        match next {
            Some(n) => self.pos = n,
            None => self.done = true,
        }
        if junk.is_empty() {
            field
        } else {
            let mut v = field.into_owned();
            v.extend_from_slice(junk);
            Cow::Owned(v)
        }
    }
}

impl<'a> Iterator for Fields<'a> {
    type Item = Cow<'a, [u8]>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let r = self.rec;
        let p = self.pos;
        if self.quotes && p < r.len() && r[p] == b'"' {
            let mut p = p + 1;
            let mut seg = p;
            let mut buf: Option<Vec<u8>> = None;
            loop {
                match memchr(b'"', &r[p..]) {
                    None => {
                        self.done = true;
                        let s = &r[seg..];
                        return Some(match buf {
                            Some(mut b) => {
                                b.extend_from_slice(s);
                                Cow::Owned(b)
                            }
                            None => Cow::Borrowed(s),
                        });
                    }
                    Some(q) => {
                        let q = p + q;
                        if q + 1 < r.len() && r[q + 1] == b'"' {
                            buf.get_or_insert_with(Vec::new).extend_from_slice(&r[seg..=q]);
                            p = q + 2;
                            seg = p;
                        } else {
                            let s = &r[seg..q];
                            let field = match buf {
                                Some(mut b) => {
                                    b.extend_from_slice(s);
                                    Cow::Owned(b)
                                }
                                None => Cow::Borrowed(s),
                            };
                            return Some(self.finish_after_quote(field, q + 1));
                        }
                    }
                }
            }
        } else {
            match memchr(self.delim, &r[p..]) {
                Some(d) => {
                    self.pos = p + d + 1;
                    Some(Cow::Borrowed(&r[p..p + d]))
                }
                None => {
                    self.done = true;
                    Some(Cow::Borrowed(&r[p..]))
                }
            }
        }
    }
}

fn detect_delimiter(sample: &[u8], path: &Path, quotes: bool) -> u8 {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "tsv" | "tab" => return b'\t',
        "psv" => return b'|',
        _ => {}
    }
    let lines: Vec<&[u8]> = {
        let mut v: Vec<&[u8]> = sample.split(|&b| b == b'\n').collect();
        if v.len() > 1 {
            v.pop();
        }
        v.into_iter().filter(|l| !l.is_empty()).take(100).collect()
    };
    if lines.is_empty() {
        return b',';
    }
    let mut best = (b',', 0.0f64);
    for &d in &[b',', b'\t', b';', b'|', 0x1F] {
        let counts: Vec<usize> = lines.iter().map(|l| Fields::new(l, d, quotes).count()).collect();
        let mut freq: HashMap<usize, usize> = HashMap::new();
        for &c in &counts {
            *freq.entry(c).or_default() += 1;
        }
        let (&mode, &hits) = freq.iter().max_by_key(|(c, h)| (**h, **c)).unwrap();
        if mode <= 1 {
            continue;
        }
        let score = (hits as f64 / lines.len() as f64) * 1000.0 + mode as f64;
        if score > best.1 {
            best = (d, score);
        }
    }
    best.0
}

pub fn delimiter_label(d: u8) -> String {
    match d {
        b',' => "Comma ,".into(),
        b'\t' => "Tab".into(),
        b';' => "Semicolon ;".into(),
        b'|' => "Pipe |".into(),
        0x1F => "Unit Sep (ASV)".into(),
        b' ' => "Space".into(),
        d => format!("{}", d as char),
    }
}

// ---------------------------------------------------------------------------
// Index
// ---------------------------------------------------------------------------

/// Byte offsets of every record start, filled in by a background thread.
pub struct Index {
    pub offsets: RwLock<Vec<u64>>,
    pub done: AtomicBool,
    pub scanned: AtomicU64,
    pub cancel: AtomicBool,
}

impl Index {
    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    /// Number of complete records available.
    pub fn count(&self) -> usize {
        let n = self.offsets.read().unwrap().len();
        if self.is_done() { n } else { n.saturating_sub(1) }
    }

    fn new_done() -> Index {
        Index {
            offsets: RwLock::new(Vec::new()),
            done: AtomicBool::new(true),
            scanned: AtomicU64::new(0),
            cancel: AtomicBool::new(false),
        }
    }
}

fn spawn_indexer(src: Arc<Source>, idx: Arc<Index>, ctx: egui::Context) {
    std::thread::spawn(move || {
        let data = &src.bytes[..];
        let n = data.len();
        let start = src.start;
        let mut batch: Vec<u64> = Vec::with_capacity(1 << 16);
        if start < n {
            batch.push(start as u64);
        }
        let mut threshold = 2_000; // small first batch so the first screen appears at once
        let flush = |batch: &mut Vec<u64>, scanned: usize| {
            idx.offsets.write().unwrap().extend_from_slice(batch);
            batch.clear();
            idx.scanned.store(scanned as u64, Ordering::Relaxed);
            ctx.request_repaint();
        };
        let mut push = |i: usize, batch: &mut Vec<u64>| -> bool {
            if i + 1 < n {
                batch.push((i + 1) as u64);
                if batch.len() >= threshold {
                    if idx.cancel.load(Ordering::Relaxed) {
                        return false;
                    }
                    flush(batch, i);
                    threshold = 1 << 16;
                }
            }
            true
        };
        if src.quotes {
            let mut in_quotes = false;
            for i in memchr2_iter(b'"', b'\n', &data[start..]) {
                let i = i + start;
                if data[i] == b'"' {
                    in_quotes = !in_quotes;
                } else if !in_quotes && !push(i, &mut batch) {
                    return;
                }
            }
        } else {
            for i in memchr_iter(b'\n', &data[start..]) {
                if !push(i + start, &mut batch) {
                    return;
                }
            }
        }
        flush(&mut batch, n);
        idx.done.store(true, Ordering::Release);
        ctx.request_repaint();
    });
}

// ---------------------------------------------------------------------------
// Snapshot: cheap, thread-safe read access to the table
// ---------------------------------------------------------------------------

/// row id -> column id -> value
pub type Edits = HashMap<RowId, HashMap<ColId, String>>;

#[derive(Clone)]
pub struct Snap {
    pub src: Arc<Source>,
    pub index: Arc<Index>,
    pub rows: Option<Arc<Vec<RowId>>>,
    pub cols: Arc<Vec<ColId>>,
    pub edits: Arc<Edits>,
    pub hdr: usize,
    pub total: usize,
}

impl Snap {
    pub fn row_id(&self, tpos: usize) -> RowId {
        match &self.rows {
            Some(r) => r[tpos],
            None => tpos as RowId,
        }
    }

    pub fn disk_fields(&self, id: RowId) -> Vec<String> {
        self.disk_fields_in(&self.index.offsets.read().unwrap(), id)
    }

    pub fn disk_fields_in(&self, offsets: &[u64], id: RowId) -> Vec<String> {
        let i = id as usize;
        if id >= NEW_ID || i >= offsets.len() {
            return Vec::new();
        }
        self.src.decode_fields(self.src.record(offsets, i))
    }

    /// Values of a row in table column order.
    pub fn row_values(&self, id: RowId) -> Vec<String> {
        self.row_values_in(&self.index.offsets.read().unwrap(), id)
    }

    /// Like `row_values`, with the offsets lock already held (use in hot loops).
    pub fn row_values_in(&self, offsets: &[u64], id: RowId) -> Vec<String> {
        let mut disk = self.disk_fields_in(offsets, id);
        let e = self.edits.get(&id);
        self.cols
            .iter()
            .map(|&c| {
                if let Some(v) = e.and_then(|e| e.get(&c)) {
                    return v.clone();
                }
                if c < NEW_ID {
                    disk.get_mut(c as usize).map(std::mem::take).unwrap_or_default()
                } else {
                    String::new()
                }
            })
            .collect()
    }

    pub fn value(&self, id: RowId, col: ColId) -> String {
        if let Some(v) = self.edits.get(&id).and_then(|e| e.get(&col)) {
            return v.clone();
        }
        if id >= NEW_ID || col >= NEW_ID {
            return String::new();
        }
        self.value_in(&self.index.offsets.read().unwrap(), id, col)
    }

    pub fn value_in(&self, offsets: &[u64], id: RowId, col: ColId) -> String {
        if let Some(v) = self.edits.get(&id).and_then(|e| e.get(&col)) {
            return v.clone();
        }
        let i = id as usize;
        if id >= NEW_ID || col >= NEW_ID || i >= offsets.len() {
            return String::new();
        }
        let raw = self.src.record(offsets, i);
        self.src.fields(raw).nth(col as usize).map(|f| self.src.decode(&f).into_owned()).unwrap_or_default()
    }

    pub fn header_names(&self) -> Vec<String> {
        if self.hdr > 0 && self.total > 0 { self.row_values(self.row_id(0)) } else { Vec::new() }
    }
}

impl TableRead for Snap {
    fn nrows(&self) -> usize {
        self.total.saturating_sub(self.hdr)
    }
    fn ncols(&self) -> usize {
        self.cols.len()
    }
    fn cell(&self, row: usize, col: usize) -> String {
        if row >= self.nrows() || col >= self.cols.len() {
            return String::new();
        }
        self.value(self.row_id(row + self.hdr), self.cols[col])
    }
    fn column_name(&self, col: usize) -> String {
        if self.hdr > 0 && self.total > 0 && col < self.cols.len() {
            let v = self.value(self.row_id(0), self.cols[col]);
            if !v.is_empty() {
                return v;
            }
        }
        col_letter(col)
    }
    fn has_header(&self) -> bool {
        self.hdr > 0
    }
}

/// A table restricted to a subset of rows/columns (e.g. visible data only).
pub struct SubTable {
    pub inner: Snap,
    pub rows: Option<Arc<Vec<u32>>>,
    pub cols: Vec<usize>,
}

impl TableRead for SubTable {
    fn nrows(&self) -> usize {
        self.rows.as_ref().map_or(self.inner.nrows(), |r| r.len())
    }
    fn ncols(&self) -> usize {
        self.cols.len()
    }
    fn cell(&self, row: usize, col: usize) -> String {
        let r = self.rows.as_ref().map_or(row, |v| v.get(row).copied().unwrap_or(u32::MAX) as usize);
        match self.cols.get(col) {
            Some(&c) => self.inner.cell(r, c),
            None => String::new(),
        }
    }
    fn column_name(&self, col: usize) -> String {
        self.cols.get(col).map(|&c| self.inner.column_name(c)).unwrap_or_default()
    }
    fn has_header(&self) -> bool {
        self.inner.has_header()
    }
}

// ---------------------------------------------------------------------------
// Document
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Change {
    pub row: RowId,
    pub col: ColId,
    pub before: Option<String>,
    pub after: Option<String>,
}

#[derive(Default)]
pub struct UndoStep {
    pub rows: Option<Option<Arc<Vec<RowId>>>>,
    pub cols: Option<Arc<Vec<ColId>>>,
    pub cells: Vec<Change>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Drag {
    None,
    Select,
    Resize(ColId, f32),
    VScroll,
    HScroll,
}

pub struct CellEdit {
    pub row: usize, // view row; usize::MAX = header row
    pub col: usize, // view col
    pub text: String,
    pub needs_focus: bool,
}

pub struct GridState {
    pub top: usize,
    pub scroll_x: f32,
    pub scroll_acc: f32,
    /// Selection in view coordinates.
    pub cur: (usize, usize),
    pub anchor: (usize, usize),
    pub drag: Drag,
    pub edit: Option<CellEdit>,
    pub vis_rows: usize,
    pub body_w: f32,
    /// Columns to auto-fit on the next frame (table column positions).
    pub autofit: Vec<usize>,
    /// Last press on a column border: (time in seconds, column) for double-click detection.
    pub last_border_press: Option<(f64, usize)>,
}

impl Default for GridState {
    fn default() -> Self {
        Self {
            top: 0,
            scroll_x: 0.0,
            scroll_acc: 0.0,
            cur: (0, 0),
            anchor: (0, 0),
            drag: Drag::None,
            edit: None,
            vis_rows: 30,
            body_w: 800.0,
            autofit: Vec::new(),
            last_border_press: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Filter {
    /// Column id, or None for any column.
    pub col: Option<ColId>,
    pub text: String,
}

pub struct Document {
    pub path: Option<PathBuf>,
    pub file_size: u64,
    pub mtime: Option<SystemTime>,
    pub load_opts: LoadOpts,
    pub save_opts: SaveOpts,
    pub src: Arc<Source>,
    pub index: Arc<Index>,
    pub rows: Option<Arc<Vec<RowId>>>,
    pub cols: Arc<Vec<ColId>>,
    pub disk_cols: u32,
    next_row: u32,
    next_col: u32,
    pub edits: Arc<Edits>,
    pub undo: Vec<UndoStep>,
    pub redo: Vec<UndoStep>,
    pub dirty: bool,
    pub has_header: bool,
    pub read_only: bool,
    pub widths: HashMap<ColId, f32>,
    pub widths_measured: bool,
    pub filters: Vec<Filter>,
    pub hidden_rows: HashSet<RowId>,
    pub hidden_cols: HashSet<ColId>,
    /// Visible data-row positions after filters/hidden rows; None = all.
    pub visible: Option<Arc<Vec<u32>>>,
    /// Visible table column positions.
    pub vcols: Vec<usize>,
    /// Filters/hidden rows need re-evaluating.
    pub view_dirty: bool,
    pub sort: Option<(ColId, bool)>,
    pub grid: GridState,
    pub view_job: Option<Job>,
    pub aux_job: Option<Job>,
}

pub const DEFAULT_COL_W: f32 = 110.0;

impl Document {
    pub fn open(path: &Path, opts: LoadOpts, ctx: &egui::Context) -> Result<Document, String> {
        let (src, size, mtime) = Source::load(path, &opts)?;
        let src = Arc::new(src);
        let index = Arc::new(Index {
            offsets: RwLock::new(Vec::new()),
            done: AtomicBool::new(false),
            scanned: AtomicU64::new(0),
            cancel: AtomicBool::new(false),
        });
        spawn_indexer(src.clone(), index.clone(), ctx.clone());
        let mut d = Document::from_parts(Some(path.to_path_buf()), src, index, opts);
        d.file_size = size;
        d.mtime = mtime;
        Ok(d)
    }

    pub fn new_empty(rows: usize, cols: usize) -> Document {
        let mut d = Document::from_parts(None, Arc::new(Source::empty()), Arc::new(Index::new_done()), LoadOpts::default());
        d.has_header = false;
        d.rows = Some(Arc::new((0..rows as u32).map(|i| NEW_ID + i).collect()));
        d.next_row = rows as u32;
        d.cols = Arc::new((0..cols as u32).map(|i| NEW_ID + i).collect());
        d.next_col = cols as u32;
        d.widths_measured = true;
        d.refresh_vcols();
        d
    }

    /// Build a document from in-memory data (used by JSON/XML import).
    pub fn from_table(header: Option<Vec<String>>, data: Vec<Vec<String>>) -> Document {
        let ncols = data.iter().map(|r| r.len()).max().unwrap_or(0).max(header.as_ref().map_or(0, |h| h.len())).max(1);
        let has_header = header.is_some();
        let all: Vec<Vec<String>> = header.into_iter().chain(data).collect();
        let mut d = Document::new_empty(all.len(), ncols);
        d.has_header = has_header;
        let edits = Arc::make_mut(&mut d.edits);
        for (r, row) in all.into_iter().enumerate() {
            let m: HashMap<ColId, String> =
                row.into_iter().enumerate().filter(|(_, v)| !v.is_empty()).map(|(c, v)| (NEW_ID + c as u32, v)).collect();
            if !m.is_empty() {
                edits.insert(NEW_ID + r as u32, m);
            }
        }
        d.widths_measured = false;
        d.dirty = true;
        d
    }

    fn from_parts(path: Option<PathBuf>, src: Arc<Source>, index: Arc<Index>, load_opts: LoadOpts) -> Document {
        let save_opts = SaveOpts {
            delimiter: src.delimiter,
            encoding: src.encoding,
            line_ending: src.line_ending,
            quoting: Quoting::AsNeeded,
            bom: src.had_bom,
        };
        Document {
            path,
            file_size: 0,
            mtime: None,
            load_opts,
            save_opts,
            src,
            index,
            rows: None,
            cols: Arc::new(vec![0]),
            disk_cols: 1,
            next_row: 0,
            next_col: 0,
            edits: Arc::new(Edits::new()),
            undo: Vec::new(),
            redo: Vec::new(),
            dirty: false,
            has_header: true,
            read_only: false,
            widths: HashMap::new(),
            widths_measured: false,
            filters: Vec::new(),
            hidden_rows: HashSet::new(),
            hidden_cols: HashSet::new(),
            visible: None,
            vcols: vec![0],
            view_dirty: false,
            sort: None,
            grid: GridState::default(),
            view_job: None,
            aux_job: None,
        }
    }

    pub fn title(&self) -> String {
        match &self.path {
            Some(p) => p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            None => "Untitled".into(),
        }
    }

    pub fn snap(&self) -> Snap {
        Snap {
            src: self.src.clone(),
            index: self.index.clone(),
            rows: self.rows.clone(),
            cols: self.cols.clone(),
            edits: self.edits.clone(),
            hdr: self.hdr(),
            total: self.total_rows(),
        }
    }

    pub fn hdr(&self) -> usize {
        self.has_header as usize
    }

    pub fn total_rows(&self) -> usize {
        match &self.rows {
            Some(r) => r.len(),
            None => self.index.count(),
        }
    }

    pub fn nrows(&self) -> usize {
        self.total_rows().saturating_sub(self.hdr())
    }

    pub fn ncols(&self) -> usize {
        self.cols.len()
    }

    pub fn row_id(&self, tpos: usize) -> RowId {
        match &self.rows {
            Some(r) => r[tpos],
            None => tpos as RowId,
        }
    }

    pub fn data_row_id(&self, d: usize) -> RowId {
        self.row_id(d + self.hdr())
    }

    pub fn index_progress(&self) -> f32 {
        if self.file_size == 0 || self.index.is_done() {
            return 1.0;
        }
        self.index.scanned.load(Ordering::Relaxed) as f32 / self.src.bytes.len().max(1) as f32
    }

    pub fn ready(&self) -> bool {
        self.index.is_done()
    }

    // ---------- view (filters / hidden) ----------

    pub fn vrows(&self) -> usize {
        match &self.visible {
            Some(v) => v.len(),
            None => self.nrows(),
        }
    }

    pub fn vrow_to_data(&self, vr: usize) -> usize {
        match &self.visible {
            Some(v) => v.get(vr).map_or(usize::MAX, |&d| d as usize),
            None => vr,
        }
    }

    pub fn vcol_to_col(&self, vc: usize) -> usize {
        self.vcols.get(vc).copied().unwrap_or(usize::MAX)
    }

    pub fn col_to_vcol(&self, c: usize) -> Option<usize> {
        self.vcols.iter().position(|&x| x == c)
    }

    pub fn data_to_vrow(&self, d: usize) -> Option<usize> {
        match &self.visible {
            Some(v) => v.binary_search(&(d as u32)).ok(),
            None => (d < self.nrows()).then_some(d),
        }
    }

    pub fn refresh_vcols(&mut self) {
        self.vcols = (0..self.cols.len()).filter(|&c| !self.hidden_cols.contains(&self.cols[c])).collect();
        if self.vcols.is_empty() && !self.cols.is_empty() {
            self.hidden_cols.clear();
            self.vcols = (0..self.cols.len()).collect();
        }
    }

    pub fn has_view_filter(&self) -> bool {
        !self.filters.is_empty() || !self.hidden_rows.is_empty()
    }

    pub fn width_of(&self, col: usize) -> f32 {
        self.cols.get(col).and_then(|id| self.widths.get(id)).copied().unwrap_or(DEFAULT_COL_W)
    }

    pub fn col_widths_visible(&self) -> Vec<f32> {
        self.vcols.iter().map(|&c| self.width_of(c)).collect()
    }

    /// Grow the disk column set when wider records are discovered.
    pub fn ensure_disk_cols(&mut self, n: usize) {
        let n = n as u32;
        if n > self.disk_cols {
            let cols = Arc::make_mut(&mut self.cols);
            cols.extend(self.disk_cols..n);
            self.disk_cols = n;
            self.refresh_vcols();
        }
    }

    /// Size columns from the first rows once enough of the file is indexed.
    /// `measure` returns the rendered width of a text in pixels.
    pub fn measure_if_needed(&mut self, measure: &dyn Fn(&str) -> f32, max_w: f32) {
        if self.widths_measured {
            return;
        }
        let n = self.total_rows();
        if n < 200 && !self.index.is_done() {
            return;
        }
        let snap = self.snap();
        let mut maxfields = 0;
        if self.rows.is_none() {
            for rec in 0..n.min(500) {
                maxfields = maxfields.max(snap.disk_fields(rec as RowId).len());
            }
            self.ensure_disk_cols(maxfields.max(1));
        }
        let snap = self.snap();
        let mut widths = vec![0f32; self.cols.len()];
        for t in 0..n.min(200) {
            for (c, v) in snap.row_values(snap.row_id(t)).iter().enumerate() {
                if !v.is_empty() {
                    widths[c] = widths[c].max(measure(v));
                }
            }
        }
        for (c, w) in widths.iter().enumerate() {
            self.widths.insert(self.cols[c], (w + 24.0).clamp(48.0, max_w));
        }
        self.widths_measured = true;
    }

    /// Fit a column to its content (header + rows around the current scroll position).
    pub fn autofit_col(&mut self, col: usize, measure: &dyn Fn(&str) -> f32, max_w: f32) {
        let snap = self.snap();
        let id = self.cols[col];
        let mut w: f32 = 0.0;
        for t in 0..self.hdr() {
            // Header text + room for the sort/filter markers.
            w = w.max(measure(&snap.value(snap.row_id(t), id)) + 24.0);
        }
        let start = self.grid.top.saturating_sub(200);
        for vr in start..(self.grid.top + 800).min(self.vrows()) {
            let d = self.vrow_to_data(vr);
            let v = snap.value(self.data_row_id(d), id);
            if !v.is_empty() {
                w = w.max(measure(&v));
            }
        }
        self.widths.insert(id, (w + 24.0).clamp(36.0, max_w));
    }

    // ---------- reading ----------

    pub fn cell_t(&self, tpos: usize, col: usize) -> String {
        if tpos >= self.total_rows() || col >= self.cols.len() {
            return String::new();
        }
        self.snap().value(self.row_id(tpos), self.cols[col])
    }

    pub fn cell(&self, data_row: usize, col: usize) -> String {
        self.cell_t(data_row + self.hdr(), col)
    }

    pub fn is_edited(&self, id: RowId, col: ColId) -> bool {
        id >= NEW_ID || self.edits.get(&id).is_some_and(|e| e.contains_key(&col))
    }

    // ---------- editing ----------

    fn edit_get(&self, row: RowId, col: ColId) -> Option<String> {
        self.edits.get(&row).and_then(|e| e.get(&col)).cloned()
    }

    fn edit_set(&mut self, row: RowId, col: ColId, v: Option<String>) {
        let edits = Arc::make_mut(&mut self.edits);
        match v {
            Some(v) => {
                edits.entry(row).or_default().insert(col, v);
            }
            None => {
                if let Some(e) = edits.get_mut(&row) {
                    e.remove(&col);
                    if e.is_empty() {
                        edits.remove(&row);
                    }
                }
            }
        }
    }

    fn push_step(&mut self, step: UndoStep) {
        if step.rows.is_none() && step.cols.is_none() && step.cells.is_empty() {
            return;
        }
        if step.rows.is_some() {
            self.view_dirty = true;
        }
        if step.cols.is_some() {
            self.refresh_vcols();
        }
        self.undo.push(step);
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.dirty = true;
    }

    /// Changes for a set of (table row, col, value) without pushing an undo step.
    fn cell_changes(&mut self, items: Vec<(usize, usize, String)>) -> Vec<Change> {
        let snap = self.snap();
        let mut out = Vec::with_capacity(items.len());
        for (t, c, val) in items {
            if t >= snap.total || c >= self.cols.len() {
                continue;
            }
            let row = snap.row_id(t);
            let col = self.cols[c];
            let before = self.edit_get(row, col);
            let original = if row < NEW_ID && col < NEW_ID {
                let mut s = snap.clone();
                s.edits = Arc::new(Edits::new());
                Some(s.value(row, col))
            } else {
                None
            };
            let after = match &original {
                Some(o) if *o == val => None,
                None if val.is_empty() => None,
                _ => Some(val),
            };
            if before == after {
                continue;
            }
            self.edit_set(row, col, after.clone());
            out.push(Change { row, col, before, after });
        }
        out
    }

    /// Set cells given table-row positions (header included). One undo step.
    pub fn set_cells_t(&mut self, items: Vec<(usize, usize, String)>) {
        let cells = self.cell_changes(items);
        self.push_step(UndoStep { cells, ..Default::default() });
        if !self.filters.is_empty() {
            // Edited rows stay visible until filters are refreshed (like Modern CSV).
        }
    }

    /// Set cells given data-row positions.
    pub fn set_cells(&mut self, items: Vec<(usize, usize, String)>) {
        let h = self.hdr();
        self.set_cells_t(items.into_iter().map(|(r, c, v)| (r + h, c, v)).collect());
    }

    fn apply_step(&mut self, step: &mut UndoStep, forward: bool) {
        if let Some(rows) = step.rows.take() {
            let cur = std::mem::replace(&mut self.rows, rows);
            step.rows = Some(cur);
            self.view_dirty = true;
        }
        if let Some(cols) = step.cols.take() {
            let cur = std::mem::replace(&mut self.cols, cols);
            step.cols = Some(cur);
            self.refresh_vcols();
        }
        let cells = std::mem::take(&mut step.cells);
        if forward {
            for ch in &cells {
                self.edit_set(ch.row, ch.col, ch.after.clone());
            }
        } else {
            for ch in cells.iter().rev() {
                self.edit_set(ch.row, ch.col, ch.before.clone());
            }
        }
        step.cells = cells;
    }

    pub fn undo(&mut self) -> bool {
        let Some(mut step) = self.undo.pop() else { return false };
        self.apply_step(&mut step, false);
        self.redo.push(step);
        self.dirty = true;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(mut step) = self.redo.pop() else { return false };
        self.apply_step(&mut step, true);
        self.undo.push(step);
        self.dirty = true;
        true
    }

    // ---------- structure ----------

    fn rows_vec(&self) -> Vec<RowId> {
        match &self.rows {
            Some(r) => (**r).clone(),
            None => (0..self.index.count() as RowId).collect(),
        }
    }

    fn new_row_ids(&mut self, n: usize) -> Vec<RowId> {
        let ids = (0..n as u32).map(|i| NEW_ID + self.next_row + i).collect();
        self.next_row += n as u32;
        ids
    }

    fn new_col_ids(&mut self, n: usize) -> Vec<ColId> {
        let ids = (0..n as u32).map(|i| NEW_ID + self.next_col + i).collect();
        self.next_col += n as u32;
        ids
    }

    /// Replace the row order (table positions incl. header) with undo.
    pub fn set_rows(&mut self, rows: Vec<RowId>, cells: Vec<Change>) {
        let old = std::mem::replace(&mut self.rows, Some(Arc::new(rows)));
        self.push_step(UndoStep { rows: Some(old), cols: None, cells });
    }

    pub fn set_cols(&mut self, cols: Vec<ColId>, cells: Vec<Change>) {
        let old = std::mem::replace(&mut self.cols, Arc::new(cols));
        self.refresh_vcols();
        self.push_step(UndoStep { rows: None, cols: Some(old), cells });
    }

    /// Insert `n` empty rows before data row `at`.
    pub fn insert_rows(&mut self, at: usize, n: usize) {
        let mut rows = self.rows_vec();
        let ids = self.new_row_ids(n);
        let t = (at + self.hdr()).min(rows.len());
        rows.splice(t..t, ids);
        self.set_rows(rows, Vec::new());
    }

    pub fn remove_rows(&mut self, data_rows: &HashSet<usize>) {
        let h = self.hdr();
        let rows: Vec<RowId> =
            self.rows_vec().into_iter().enumerate().filter(|(t, _)| *t < h || !data_rows.contains(&(t - h))).map(|(_, r)| r).collect();
        self.set_rows(rows, Vec::new());
    }

    /// Move data rows `range` so they start at data position `to` (in the list without them).
    pub fn move_rows(&mut self, range: std::ops::Range<usize>, to: usize) {
        let h = self.hdr();
        let mut rows = self.rows_vec();
        let moved: Vec<RowId> = rows.drain(range.start + h..range.end + h).collect();
        let t = (to + h).min(rows.len());
        rows.splice(t..t, moved);
        self.set_rows(rows, Vec::new());
    }

    /// Duplicate data rows `range` `times` times, inserting the copies right after.
    pub fn duplicate_rows(&mut self, range: std::ops::Range<usize>, times: usize) {
        let h = self.hdr();
        let snap = self.snap();
        let mut rows = self.rows_vec();
        let src: Vec<Vec<String>> = range.clone().map(|d| snap.row_values(snap.row_id(d + h))).collect();
        let ids = self.new_row_ids(src.len() * times);
        let edits = Arc::make_mut(&mut self.edits);
        let mut cells = Vec::new();
        for (k, &id) in ids.iter().enumerate() {
            let vals = &src[k % src.len()];
            let m: HashMap<ColId, String> =
                vals.iter().enumerate().filter(|(_, v)| !v.is_empty()).map(|(c, v)| (self.cols[c], v.clone())).collect();
            for (&c, v) in &m {
                cells.push(Change { row: id, col: c, before: None, after: Some(v.clone()) });
            }
            edits.insert(id, m);
        }
        let t = range.end + h;
        rows.splice(t..t, ids);
        self.set_rows(rows, cells);
    }

    pub fn insert_cols(&mut self, at: usize, n: usize) {
        let mut cols = (*self.cols).clone();
        let ids = self.new_col_ids(n);
        let at = at.min(cols.len());
        cols.splice(at..at, ids.iter().copied());
        for id in ids {
            self.widths.insert(id, DEFAULT_COL_W);
        }
        self.set_cols(cols, Vec::new());
    }

    pub fn remove_cols(&mut self, set: &HashSet<usize>) {
        let cols: Vec<ColId> = self.cols.iter().enumerate().filter(|(c, _)| !set.contains(c)).map(|(_, &id)| id).collect();
        self.set_cols(cols, Vec::new());
    }

    pub fn move_cols(&mut self, range: std::ops::Range<usize>, to: usize) {
        let mut cols = (*self.cols).clone();
        let moved: Vec<ColId> = cols.drain(range).collect();
        let t = to.min(cols.len());
        cols.splice(t..t, moved);
        self.set_cols(cols, Vec::new());
    }

    /// Duplicate columns; copies every value (can be slow on very large files).
    pub fn duplicate_cols(&mut self, range: std::ops::Range<usize>, times: usize) {
        let snap = self.snap();
        let src_ids: Vec<ColId> = range.clone().map(|c| self.cols[c]).collect();
        let ids = self.new_col_ids(src_ids.len() * times);
        let mut cells = Vec::new();
        let total = snap.total;
        let edits = Arc::make_mut(&mut self.edits);
        for t in 0..total {
            let row = snap.row_id(t);
            for (k, &nid) in ids.iter().enumerate() {
                let v = snap.value(row, src_ids[k % src_ids.len()]);
                if !v.is_empty() {
                    edits.entry(row).or_default().insert(nid, v.clone());
                    cells.push(Change { row, col: nid, before: None, after: Some(v) });
                }
            }
        }
        for (k, &nid) in ids.iter().enumerate() {
            let w = self.widths.get(&src_ids[k % src_ids.len()]).copied().unwrap_or(DEFAULT_COL_W);
            self.widths.insert(nid, w);
        }
        let mut cols = (*self.cols).clone();
        cols.splice(range.end..range.end, ids);
        self.set_cols(cols, cells);
    }

    /// Replace the whole table content with an in-memory grid (used by transpose).
    pub fn replace_all(&mut self, grid: Vec<Vec<String>>) {
        let nrows = grid.len();
        let ncols = grid.iter().map(|r| r.len()).max().unwrap_or(0).max(1);
        let row_ids = self.new_row_ids(nrows);
        let col_ids = self.new_col_ids(ncols);
        let mut cells = Vec::new();
        let edits = Arc::make_mut(&mut self.edits);
        for (r, row) in grid.into_iter().enumerate() {
            for (c, v) in row.into_iter().enumerate() {
                if !v.is_empty() {
                    edits.entry(row_ids[r]).or_default().insert(col_ids[c], v.clone());
                    cells.push(Change { row: row_ids[r], col: col_ids[c], before: None, after: Some(v) });
                }
            }
        }
        let old_rows = std::mem::replace(&mut self.rows, Some(Arc::new(row_ids)));
        let old_cols = std::mem::replace(&mut self.cols, Arc::new(col_ids));
        self.widths_measured = false;
        self.push_step(UndoStep { rows: Some(old_rows), cols: Some(old_cols), cells });
        self.refresh_vcols();
    }

    /// Selection rectangle in view coordinates (min, max).
    pub fn sel_range(&self) -> ((usize, usize), (usize, usize)) {
        let (a, b) = (self.grid.anchor, self.grid.cur);
        ((a.0.min(b.0), a.1.min(b.1)), (a.0.max(b.0), a.1.max(b.1)))
    }

    /// Selected data rows (visible only), clamped.
    pub fn sel_data_rows(&self) -> Vec<usize> {
        let n = self.vrows();
        if n == 0 {
            return Vec::new();
        }
        let ((r0, _), (r1, _)) = self.sel_range();
        (r0..=r1.min(n - 1)).map(|vr| self.vrow_to_data(vr)).collect()
    }

    /// Selected table columns (visible only), clamped.
    pub fn sel_cols(&self) -> Vec<usize> {
        let n = self.vcols.len();
        if n == 0 {
            return Vec::new();
        }
        let ((_, c0), (_, c1)) = self.sel_range();
        (c0..=c1.min(n - 1)).map(|vc| self.vcols[vc]).collect()
    }

    /// Scroll so the cursor cell is on screen.
    pub fn ensure_visible(&mut self) {
        let widths = self.col_widths_visible();
        let g = &mut self.grid;
        let (r, c) = g.cur;
        if r < g.top {
            g.top = r;
        } else if r >= g.top + g.vis_rows {
            g.top = r + 1 - g.vis_rows;
        }
        if c < widths.len() {
            let x0: f32 = widths[..c].iter().sum();
            let x1 = x0 + widths[c];
            if x0 < g.scroll_x {
                g.scroll_x = x0;
            } else if x1 > g.scroll_x + g.body_w {
                g.scroll_x = (x1 - g.body_w).min(x0);
            }
        }
    }

    pub fn clamp_cursor(&mut self) {
        let nr = self.vrows().max(1) - 1;
        let nc = self.vcols.len().max(1) - 1;
        let g = &mut self.grid;
        g.cur = (g.cur.0.min(nr), g.cur.1.min(nc));
        g.anchor = (g.anchor.0.min(nr), g.anchor.1.min(nc));
    }

    pub fn select(&mut self, cur: (usize, usize), anchor: (usize, usize)) {
        self.grid.cur = cur;
        self.grid.anchor = anchor;
        self.clamp_cursor();
        self.ensure_visible();
    }

    /// Can the raw bytes of an unedited disk record be copied verbatim when saving?
    pub fn cols_identity(&self) -> bool {
        self.cols.len() == self.disk_cols as usize && self.cols.iter().enumerate().all(|(i, &c)| c == i as u32)
    }
}

impl Drop for Document {
    fn drop(&mut self) {
        self.index.cancel.store(true, Ordering::Relaxed);
        for job in [&self.view_job, &self.aux_job].into_iter().flatten() {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }
}
