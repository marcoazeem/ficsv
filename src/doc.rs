//! File loading, background row indexing and CSV record parsing.
//!
//! The file is memory-mapped and never fully parsed. A background thread scans
//! it once (quote-aware) and records the byte offset where each record starts.
//! Rows are parsed on demand when they are drawn, filtered or sorted.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};


use memchr::{memchr, memchr2_iter};
use memmap2::Mmap;

use crate::jobs::Job;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Encoding {
    Utf8,
    Windows1252,
}

impl Encoding {
    pub fn label(self) -> &'static str {
        match self {
            Encoding::Utf8 => "UTF-8",
            Encoding::Windows1252 => "Windows-1252",
        }
    }
}

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
}

pub enum Bytes {
    Map(Mmap),
    Empty,
}

impl std::ops::Deref for Bytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Bytes::Map(m) => &m[..],
            Bytes::Empty => &[],
        }
    }
}

/// Immutable view of the file on disk, shared with background jobs.
pub struct Source {
    pub bytes: Bytes,
    pub delimiter: u8,
    pub encoding: Encoding,
    pub line_ending: LineEnding,
    pub bom_len: usize,
    pub trailing_newline: bool,
}

impl Source {
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
        match self.encoding {
            Encoding::Utf8 => String::from_utf8_lossy(b),
            Encoding::Windows1252 => encoding_rs::WINDOWS_1252.decode_without_bom_handling(b).0,
        }
    }

    pub fn encode<'a>(&self, s: &'a str) -> Cow<'a, [u8]> {
        match self.encoding {
            Encoding::Utf8 => Cow::Borrowed(s.as_bytes()),
            Encoding::Windows1252 => encoding_rs::WINDOWS_1252.encode(s).0,
        }
    }
}

/// Iterator over the fields of one record (RFC 4180, lenient).
pub struct Fields<'a> {
    rec: &'a [u8],
    pos: usize,
    delim: u8,
    done: bool,
}

impl<'a> Fields<'a> {
    pub fn new(rec: &'a [u8], delim: u8) -> Self {
        Self { rec, pos: 0, delim, done: false }
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
            // Malformed: text after the closing quote. Keep it rather than lose data.
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
        if p < r.len() && r[p] == b'"' {
            let mut p = p + 1;
            let mut seg = p;
            let mut buf: Option<Vec<u8>> = None;
            loop {
                match memchr(b'"', &r[p..]) {
                    None => {
                        // Unterminated quote: take the rest of the record.
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
}

fn spawn_indexer(src: Arc<Source>, idx: Arc<Index>, ctx: egui::Context) {
    std::thread::spawn(move || {
        let data = &src.bytes[..];
        let n = data.len();
        let start = src.bom_len;
        let mut batch: Vec<u64> = Vec::with_capacity(1 << 16);
        if start < n {
            batch.push(start as u64);
        }
        let mut in_quotes = false;
        let mut threshold = 2_000; // small first batch so the first screen appears instantly

        let flush = |batch: &mut Vec<u64>, scanned: usize| {
            idx.offsets.write().unwrap().extend_from_slice(batch);
            batch.clear();
            idx.scanned.store(scanned as u64, Ordering::Relaxed);
            ctx.request_repaint();
        };

        for i in memchr2_iter(b'"', b'\n', &data[start..]) {
            let i = i + start;
            if data[i] == b'"' {
                in_quotes = !in_quotes;
            } else if !in_quotes && i + 1 < n {
                batch.push((i + 1) as u64);
                if batch.len() >= threshold {
                    if idx.cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    flush(&mut batch, i);
                    threshold = 1 << 16;
                }
            }
        }
        flush(&mut batch, n);
        idx.done.store(true, Ordering::Release);
        ctx.request_repaint();
    });
}

fn detect_delimiter(sample: &[u8], path: &Path) -> u8 {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if ext == "tsv" || ext == "tab" {
        return b'\t';
    }
    if ext == "psv" {
        return b'|';
    }
    // Take complete lines only.
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
    for &d in &[b',', b'\t', b';', b'|'] {
        let counts: Vec<usize> = lines.iter().map(|l| Fields::new(l, d).count()).collect();
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

/// One undoable cell change.
#[derive(Clone)]
pub struct Change {
    pub rec: u32,
    pub col: u32,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// record -> column -> new value
pub type Edits = HashMap<u32, HashMap<u32, String>>;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Drag {
    None,
    Select,
    Resize(usize, f32),
    VScroll,
    HScroll,
}

pub struct CellEdit {
    pub row: usize,
    pub col: usize,
    pub text: String,
    pub needs_focus: bool,
}

pub struct GridState {
    pub top: usize,
    pub scroll_x: f32,
    pub scroll_acc: f32,
    pub cur: (usize, usize),
    pub anchor: (usize, usize),
    pub drag: Drag,
    pub edit: Option<CellEdit>,
    pub vis_rows: usize,
    pub body_w: f32,
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
        }
    }
}

pub struct Document {
    pub path: PathBuf,
    pub file_size: u64,
    pub src: Arc<Source>,
    pub index: Arc<Index>,
    pub edits: Edits,
    pub undo: Vec<Vec<Change>>,
    pub redo: Vec<Vec<Change>>,
    pub has_header: bool,
    pub ncols: usize,
    pub col_widths: Vec<f32>,
    pub widths_measured: bool,
    /// Visible record indices after filter/sort. `None` = all records in file order.
    pub view: Option<Arc<Vec<u32>>>,
    pub view_pending: bool,
    pub sort: Option<(usize, bool)>,
    pub filter_text: String,
    pub filter_col: Option<usize>,
    pub applied_filter: Option<(String, Option<usize>)>,
    pub grid: GridState,
    pub view_job: Option<Job>,
    pub aux_job: Option<Job>,
}

pub const DEFAULT_COL_W: f32 = 110.0;

impl Document {
    pub fn open(path: &Path, ctx: &egui::Context) -> Result<Document, String> {
        let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let file_size = file.metadata().map_err(|e| e.to_string())?.len();
        let bytes = if file_size == 0 {
            Bytes::Empty
        } else {
            // SAFETY: the file may be changed by another process while mapped; that is
            // the same trade-off every mmap-based viewer makes.
            Bytes::Map(unsafe { Mmap::map(&file) }.map_err(|e| e.to_string())?)
        };
        let data = &bytes[..];
        if data.starts_with(&[0xFF, 0xFE]) || data.starts_with(&[0xFE, 0xFF]) {
            return Err("UTF-16 files are not supported yet. Convert to UTF-8 first.".into());
        }
        let bom_len = if data.starts_with(&[0xEF, 0xBB, 0xBF]) { 3 } else { 0 };
        let sample = &data[bom_len..data.len().min(bom_len + (4 << 20))];
        let encoding = match std::str::from_utf8(sample) {
            Ok(_) => Encoding::Utf8,
            Err(e) if e.error_len().is_none() => Encoding::Utf8, // cut mid-character
            Err(_) => Encoding::Windows1252,
        };
        let line_ending = match memchr(b'\n', sample) {
            Some(i) if i > 0 && sample[i - 1] == b'\r' => LineEnding::CrLf,
            Some(_) => LineEnding::Lf,
            None if cfg!(windows) => LineEnding::CrLf,
            None => LineEnding::Lf,
        };
        let delimiter = detect_delimiter(&sample[..sample.len().min(256 << 10)], path);
        let trailing_newline = data.last() == Some(&b'\n');

        let src = Arc::new(Source { bytes, delimiter, encoding, line_ending, bom_len, trailing_newline });
        let index = Arc::new(Index {
            offsets: RwLock::new(Vec::new()),
            done: AtomicBool::new(false),
            scanned: AtomicU64::new(0),
            cancel: AtomicBool::new(false),
        });
        spawn_indexer(src.clone(), index.clone(), ctx.clone());

        Ok(Document {
            path: path.to_path_buf(),
            file_size,
            src,
            index,
            edits: Edits::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            has_header: true,
            ncols: 1,
            col_widths: vec![DEFAULT_COL_W],
            widths_measured: false,
            view: None,
            view_pending: false,
            sort: None,
            filter_text: String::new(),
            filter_col: None,
            applied_filter: None,
            grid: GridState::default(),
            view_job: None,
            aux_job: None,
        })
    }

    pub fn title(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "untitled".into())
    }

    pub fn is_dirty(&self) -> bool {
        !self.edits.is_empty()
    }

    /// Number of fully indexed records (including a header record).
    pub fn record_count(&self) -> usize {
        let n = self.index.offsets.read().unwrap().len();
        if self.index.is_done() { n } else { n.saturating_sub(1) }
    }

    pub fn first_rec(&self) -> usize {
        self.has_header as usize
    }

    pub fn view_len(&self) -> usize {
        match &self.view {
            Some(v) => v.len(),
            None => self.record_count().saturating_sub(self.first_rec()),
        }
    }

    pub fn rec_of(&self, row: usize) -> usize {
        match &self.view {
            Some(v) => v[row] as usize,
            None => row + self.first_rec(),
        }
    }

    pub fn index_progress(&self) -> f32 {
        if self.file_size == 0 {
            return 1.0;
        }
        self.index.scanned.load(Ordering::Relaxed) as f32 / self.file_size as f32
    }

    /// Decoded fields of a record with edits applied.
    pub fn row_fields(&self, rec: usize) -> Vec<String> {
        let offsets = self.index.offsets.read().unwrap();
        let mut out: Vec<String> = if rec < offsets.len() {
            let raw = self.src.record(&offsets, rec);
            Fields::new(raw, self.src.delimiter).map(|f| self.src.decode(&f).into_owned()).collect()
        } else {
            Vec::new()
        };
        if let Some(e) = self.edits.get(&(rec as u32)) {
            for (&c, v) in e {
                let c = c as usize;
                if out.len() <= c {
                    out.resize(c + 1, String::new());
                }
                out[c] = v.clone();
            }
        }
        out
    }

    pub fn cell(&self, rec: usize, col: usize) -> String {
        if let Some(v) = self.edits.get(&(rec as u32)).and_then(|e| e.get(&(col as u32))) {
            return v.clone();
        }
        self.row_fields(rec).into_iter().nth(col).unwrap_or_default()
    }

    pub fn is_edited(&self, rec: usize, col: usize) -> bool {
        self.edits.get(&(rec as u32)).is_some_and(|e| e.contains_key(&(col as u32)))
    }

    pub fn column_names(&self) -> Vec<String> {
        if self.has_header && self.record_count() > 0 { self.row_fields(0) } else { Vec::new() }
    }

    pub fn ensure_cols(&mut self, n: usize) {
        if n > self.ncols {
            self.ncols = n;
            self.col_widths.resize(n, DEFAULT_COL_W);
        }
    }

    /// Size columns from the first rows once enough of the file is indexed.
    pub fn measure_if_needed(&mut self) {
        if self.widths_measured {
            return;
        }
        let n = self.record_count();
        if n < 200 && !self.index.is_done() {
            return;
        }
        let mut widths: Vec<usize> = Vec::new();
        for rec in 0..n.min(200) {
            for (c, f) in self.row_fields(rec).iter().enumerate() {
                if widths.len() <= c {
                    widths.resize(c + 1, 0);
                }
                widths[c] = widths[c].max(f.chars().count());
            }
        }
        self.ensure_cols(widths.len().max(1));
        for (c, w) in widths.iter().enumerate() {
            self.col_widths[c] = (*w as f32 * 7.4 + 20.0).clamp(48.0, 340.0);
        }
        self.widths_measured = true;
    }

    fn set_edit(&mut self, rec: u32, col: u32, v: Option<String>) {
        match v {
            Some(v) => {
                self.edits.entry(rec).or_default().insert(col, v);
            }
            None => {
                if let Some(e) = self.edits.get_mut(&rec) {
                    e.remove(&col);
                    if e.is_empty() {
                        self.edits.remove(&rec);
                    }
                }
            }
        }
    }

    /// Apply a batch of (record, column, value) changes as one undo step.
    pub fn set_cells(&mut self, items: Vec<(usize, usize, String)>) {
        let mut batch = Vec::with_capacity(items.len());
        for (rec, col, val) in items {
            let before = self.edits.get(&(rec as u32)).and_then(|e| e.get(&(col as u32))).cloned();
            // Writing back the original value removes the edit entirely.
            let original = {
                let offsets = self.index.offsets.read().unwrap();
                if rec < offsets.len() {
                    let raw = self.src.record(&offsets, rec);
                    Fields::new(raw, self.src.delimiter).nth(col).map(|f| self.src.decode(&f).into_owned())
                } else {
                    None
                }
            };
            let after = if original.as_deref() == Some(val.as_str()) || (original.is_none() && val.is_empty()) {
                None
            } else {
                Some(val)
            };
            if before == after {
                continue;
            }
            self.ensure_cols(col + 1);
            self.set_edit(rec as u32, col as u32, after.clone());
            batch.push(Change { rec: rec as u32, col: col as u32, before, after });
        }
        if !batch.is_empty() {
            self.undo.push(batch);
            self.redo.clear();
        }
    }

    pub fn undo(&mut self) -> Option<(u32, u32)> {
        let batch = self.undo.pop()?;
        for ch in batch.iter().rev() {
            self.set_edit(ch.rec, ch.col, ch.before.clone());
        }
        let first = batch.first().map(|c| (c.rec, c.col));
        self.redo.push(batch);
        first
    }

    pub fn redo(&mut self) -> Option<(u32, u32)> {
        let batch = self.redo.pop()?;
        for ch in &batch {
            self.set_edit(ch.rec, ch.col, ch.after.clone());
        }
        let first = batch.first().map(|c| (c.rec, c.col));
        self.undo.push(batch);
        first
    }

    /// Scroll so the cursor cell is on screen.
    pub fn ensure_visible(&mut self) {
        let g = &mut self.grid;
        let (r, c) = g.cur;
        if r < g.top {
            g.top = r;
        } else if r >= g.top + g.vis_rows {
            g.top = r + 1 - g.vis_rows;
        }
        if c < self.col_widths.len() {
            let x0: f32 = self.col_widths[..c].iter().sum();
            let x1 = x0 + self.col_widths[c];
            if x0 < g.scroll_x {
                g.scroll_x = x0;
            } else if x1 > g.scroll_x + g.body_w {
                g.scroll_x = (x1 - g.body_w).min(x0);
            }
        }
    }

    /// Find the view row showing record `rec`, if it is visible.
    pub fn row_of_rec(&self, rec: usize) -> Option<usize> {
        match &self.view {
            Some(v) => v.iter().position(|&r| r as usize == rec),
            None => rec.checked_sub(self.first_rec()),
        }
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
