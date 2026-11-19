//! Background work: filter/sort, find and save. All of these read the mmapped
//! file directly and use rayon to spread the work over all cores.

use std::borrow::Cow;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::Instant;

use rayon::prelude::*;

use crate::doc::{Edits, Fields, Index, Source};
use crate::query::{contains_ci, Query};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    View,
    Find,
    Save,
}

pub enum JobResult {
    View(Option<Vec<u32>>),
    Find(Option<(usize, usize)>),
    Saved { tmp: PathBuf, target: PathBuf },
    Error(String),
}

pub struct Job {
    pub kind: JobKind,
    pub cancel: Arc<AtomicBool>,
    pub rx: Receiver<JobResult>,
    pub started: Instant,
}

pub fn spawn(
    ctx: &egui::Context,
    kind: JobKind,
    f: impl FnOnce(&AtomicBool) -> JobResult + Send + 'static,
) -> Job {
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    let c2 = cancel.clone();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let r = f(&c2);
        if !c2.load(Ordering::Relaxed) {
            let _ = tx.send(r);
            ctx.request_repaint();
        }
    });
    Job { kind, cancel, rx, started: Instant::now() }
}

/// Inputs shared by jobs that read cells.
#[derive(Clone)]
pub struct Snapshot {
    pub src: Arc<Source>,
    pub index: Arc<Index>,
    pub edits: Arc<Edits>,
}

impl Snapshot {
    fn fields<'a>(&'a self, offsets: &[u64], rec: usize) -> Vec<Cow<'a, str>> {
        let raw = self.src.record(offsets, rec);
        let mut out: Vec<Cow<str>> = Fields::new(raw, self.src.delimiter)
            .map(|f| match f {
                Cow::Borrowed(b) => self.src.decode(b),
                Cow::Owned(b) => Cow::Owned(self.src.decode(&b).into_owned()),
            })
            .collect();
        if let Some(e) = self.edits.get(&(rec as u32)) {
            for (&c, v) in e {
                let c = c as usize;
                if out.len() <= c {
                    out.resize(c + 1, Cow::Borrowed(""));
                }
                out[c] = Cow::Borrowed(v.as_str());
            }
        }
        out
    }

    fn cell<'a>(&'a self, offsets: &[u64], rec: usize, col: usize) -> Cow<'a, str> {
        if let Some(v) = self.edits.get(&(rec as u32)).and_then(|e| e.get(&(col as u32))) {
            return Cow::Borrowed(v.as_str());
        }
        let raw = self.src.record(offsets, rec);
        match Fields::new(raw, self.src.delimiter).nth(col) {
            Some(Cow::Borrowed(b)) => self.src.decode(b),
            Some(Cow::Owned(b)) => Cow::Owned(self.src.decode(&b).into_owned()),
            None => Cow::Borrowed(""),
        }
    }
}

/// Compute the visible row order. Returns `None` when there is no filter or sort.
pub fn compute_view(
    snap: &Snapshot,
    first: usize,
    filter: Option<(&Query, Option<usize>)>,
    sort: Option<(usize, bool)>,
    cancel: &AtomicBool,
) -> Option<Vec<u32>> {
    if filter.is_none() && sort.is_none() {
        return None;
    }
    let offsets = snap.index.offsets.read().unwrap();
    let total = offsets.len();
    let first = first.min(total);

    let mut rows: Vec<u32> = match filter {
        Some((q, col)) => (first..total)
            .into_par_iter()
            .with_min_len(1024)
            .filter(|&r| {
                if cancel.load(Ordering::Relaxed) {
                    return false;
                }
                match col {
                    Some(c) => q.matches(&[snap.cell(&offsets, r, c)]),
                    None => q.matches(&snap.fields(&offsets, r)),
                }
            })
            .map(|r| r as u32)
            .collect(),
        None => (first as u32..total as u32).collect(),
    };
    if cancel.load(Ordering::Relaxed) {
        return None;
    }

    if let Some((col, asc)) = sort {
        // Numeric sort if every non-empty value in the column is a number.
        let numeric = rows.par_iter().all(|&r| {
            let v = snap.cell(&offsets, r as usize, col);
            let v = v.trim();
            v.is_empty() || v.parse::<f64>().is_ok()
        });
        let mut order: Vec<u32> = (0..rows.len() as u32).collect();
        if numeric {
            let keys: Vec<Option<f64>> = rows
                .par_iter()
                .map(|&r| snap.cell(&offsets, r as usize, col).trim().parse::<f64>().ok())
                .collect();
            order.par_sort_by(|&a, &b| {
                match (keys[a as usize], keys[b as usize]) {
                    (Some(x), Some(y)) => {
                        let o = x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal);
                        if asc { o } else { o.reverse() }
                    }
                    (Some(_), None) => std::cmp::Ordering::Less, // empties last
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                }
            });
        } else {
            let keys: Vec<Box<str>> = rows
                .par_iter()
                .map(|&r| snap.cell(&offsets, r as usize, col).to_lowercase().into_boxed_str())
                .collect();
            order.par_sort_by(|&a, &b| {
                let (x, y) = (&keys[a as usize], &keys[b as usize]);
                match (x.is_empty(), y.is_empty()) {
                    (false, true) => std::cmp::Ordering::Less,
                    (true, false) => std::cmp::Ordering::Greater,
                    _ => {
                        let o = x.cmp(y);
                        if asc { o } else { o.reverse() }
                    }
                }
            });
        }
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        rows = order.into_iter().map(|i| rows[i as usize]).collect();
    }
    Some(rows)
}

/// Find the next cell (in view order, after `start`) containing `needle_lc`. Wraps around.
pub fn find_next(
    snap: &Snapshot,
    view: Option<Arc<Vec<u32>>>,
    first: usize,
    nrows: usize,
    start: (usize, usize),
    needle_lc: &str,
    cancel: &AtomicBool,
) -> Option<(usize, usize)> {
    if nrows == 0 {
        return None;
    }
    let offsets = snap.index.offsets.read().unwrap();
    let rec_of = |row: usize| match &view {
        Some(v) => v[row] as usize,
        None => row + first,
    };
    let hit_in = |row: usize, min_col: usize, max_col: usize| -> Option<usize> {
        snap.fields(&offsets, rec_of(row))
            .iter()
            .enumerate()
            .skip(min_col)
            .take(max_col.saturating_sub(min_col))
            .find(|(_, f)| contains_ci(f, needle_lc))
            .map(|(c, _)| c)
    };
    let (sr, sc) = (start.0.min(nrows - 1), start.1);
    if let Some(c) = hit_in(sr, sc + 1, usize::MAX) {
        return Some((sr, c));
    }
    let search = |range: std::ops::Range<usize>| {
        range
            .into_par_iter()
            .with_min_len(512)
            .find_map_first(|r| {
                if cancel.load(Ordering::Relaxed) {
                    return None;
                }
                hit_in(r, 0, usize::MAX).map(|c| (r, c))
            })
    };
    if let Some(hit) = search(sr + 1..nrows) {
        return Some(hit);
    }
    if let Some(hit) = search(0..sr) {
        return Some(hit);
    }
    hit_in(sr, 0, sc + 1).map(|c| (sr, c))
}

fn write_field(out: &mut Vec<u8>, s: &str, src: &Source) {
    let d = src.delimiter as char;
    let needs_quotes = s.contains(|c| c == d || c == '"' || c == '\n' || c == '\r');
    let bytes = src.encode(s);
    if needs_quotes {
        out.push(b'"');
        for &b in bytes.iter() {
            if b == b'"' {
                out.push(b'"');
            }
            out.push(b);
        }
        out.push(b'"');
    } else {
        out.extend_from_slice(&bytes);
    }
}

/// Stream the file to a temp file next to `target`, re-serialising only edited rows.
pub fn save(snap: &Snapshot, target: &Path, cancel: &AtomicBool) -> Result<PathBuf, String> {
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = target.with_file_name(format!(".{name}.ficsv-tmp"));
    let run = || -> std::io::Result<()> {
        let mut w = BufWriter::with_capacity(4 << 20, File::create(&tmp)?);
        let src = &snap.src;
        let offsets = snap.index.offsets.read().unwrap();
        let eol = src.line_ending.bytes();
        w.write_all(&src.bytes[..src.bom_len])?;
        let mut line = Vec::new();
        for i in 0..offsets.len() {
            if i % 65536 == 0 && cancel.load(Ordering::Relaxed) {
                return Err(std::io::Error::other("cancelled"));
            }
            if i > 0 {
                w.write_all(eol)?;
            }
            match snap.edits.get(&(i as u32)) {
                None => w.write_all(src.record(&offsets, i))?,
                Some(_) => {
                    line.clear();
                    for (c, f) in snap.fields(&offsets, i).iter().enumerate() {
                        if c > 0 {
                            line.push(src.delimiter);
                        }
                        write_field(&mut line, f, src);
                    }
                    w.write_all(&line)?;
                }
            }
        }
        if src.trailing_newline && !offsets.is_empty() {
            w.write_all(eol)?;
        }
        w.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        Ok(())
    };
    match run() {
        Ok(()) => Ok(tmp),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e.to_string())
        }
    }
}
