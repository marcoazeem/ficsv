//! Background work over a table snapshot: filtering, sorting, find/replace,
//! duplicate detection and saving. Uses rayon to spread work over all cores.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::Instant;

use rayon::prelude::*;
use regex::{Regex, RegexBuilder};

use crate::doc::{bom_bytes, encode_str, ColId, Quoting, RowId, SaveOpts, Snap, NEW_ID};
use crate::query::Query;

pub enum JobResult {
    Visible(Option<Vec<u32>>),
    Sorted { rows: Vec<RowId>, col: ColId, asc: bool },
    Found(Option<(usize, usize)>),
    FoundCount(usize),
    Cells(Vec<(usize, usize, String)>),
    RemoveRows(Vec<usize>),
    HideRows(Vec<RowId>),
    SelectRows(Vec<usize>),
    RemoveCols(Vec<usize>),
    HideCols(Vec<ColId>),
    SelectCols(Vec<usize>),
    Saved { tmp: PathBuf, target: PathBuf, replace_doc: bool },
    Done(String),
    Error(String),
}

pub struct Job {
    pub label: String,
    pub cancel: Arc<AtomicBool>,
    pub rx: Receiver<JobResult>,
    #[allow(dead_code)]
    pub started: Instant,
}

pub fn spawn(ctx: &egui::Context, label: impl Into<String>, f: impl FnOnce(&AtomicBool) -> JobResult + Send + 'static) -> Job {
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
    Job { label: label.into(), cancel, rx, started: Instant::now() }
}

// ---------------------------------------------------------------------------
// Filter
// ---------------------------------------------------------------------------

pub fn compute_visible(
    snap: &Snap,
    filters: &[(Query, Option<usize>)],
    hidden: &HashSet<RowId>,
    cancel: &AtomicBool,
) -> Option<Vec<u32>> {
    let off = snap.index.offsets.read().unwrap();
    let off: &[u64] = &off;
    if filters.is_empty() && hidden.is_empty() {
        return None;
    }
    let n = snap.total.saturating_sub(snap.hdr);
    let col_ids: Vec<Option<ColId>> = filters.iter().map(|(_, c)| c.map(|c| snap.cols[c])).collect();
    let v = (0..n)
        .into_par_iter()
        .with_min_len(1024)
        .filter(|&d| {
            if cancel.load(Ordering::Relaxed) {
                return false;
            }
            let id = snap.row_id(d + snap.hdr);
            if hidden.contains(&id) {
                return false;
            }
            let mut all: Option<Vec<String>> = None;
            filters.iter().zip(&col_ids).all(|((q, _), col)| match col {
                Some(c) => q.matches(&[snap.value_in(off, id, *c)]),
                None => q.matches(all.get_or_insert_with(|| snap.row_values_in(off, id))),
            })
        })
        .map(|d| d as u32)
        .collect();
    Some(v)
}

// ---------------------------------------------------------------------------
// Sort
// ---------------------------------------------------------------------------

/// Natural-ish comparison: numbers numerically, otherwise case-insensitive text; empties last.
pub fn sort_keys_numeric(vals: &[String]) -> bool {
    vals.iter().all(|v| {
        let t = v.trim();
        t.is_empty() || t.parse::<f64>().is_ok()
    })
}

/// Stable sort of data rows by table column `col`. Returns full row order (header first).
pub fn sort_rows(snap: &Snap, col: usize, asc: bool, cancel: &AtomicBool) -> Vec<RowId> {
    let off = snap.index.offsets.read().unwrap();
    let off: &[u64] = &off;
    let id = snap.cols[col];
    let n = snap.total.saturating_sub(snap.hdr);
    let ids: Vec<RowId> = (0..n).map(|d| snap.row_id(d + snap.hdr)).collect();
    let vals: Vec<String> = ids.par_iter().map(|&r| if cancel.load(Ordering::Relaxed) { String::new() } else { snap.value_in(off, r, id) }).collect();
    let order = sort_order(&vals, asc);
    let mut out: Vec<RowId> = (0..snap.hdr).map(|t| snap.row_id(t)).collect();
    out.extend(order.into_iter().map(|i| ids[i]));
    out
}

/// Stable ordering permutation for a list of values.
pub fn sort_order(vals: &[String], asc: bool) -> Vec<usize> {
    let mut order: Vec<usize> = (0..vals.len()).collect();
    if sort_keys_numeric(vals) {
        let keys: Vec<Option<f64>> = vals.par_iter().map(|v| v.trim().parse::<f64>().ok()).collect();
        order.par_sort_by(|&a, &b| match (keys[a], keys[b]) {
            (Some(x), Some(y)) => {
                let o = x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal);
                if asc { o } else { o.reverse() }
            }
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });
    } else {
        let keys: Vec<String> = vals.par_iter().map(|v| v.to_lowercase()).collect();
        order.par_sort_by(|&a, &b| match (keys[a].is_empty(), keys[b].is_empty()) {
            (false, true) => std::cmp::Ordering::Less,
            (true, false) => std::cmp::Ordering::Greater,
            _ => {
                let o = keys[a].cmp(&keys[b]);
                if asc { o } else { o.reverse() }
            }
        });
    }
    order
}

// ---------------------------------------------------------------------------
// Find / replace
// ---------------------------------------------------------------------------

#[derive(Clone, Default, PartialEq)]
pub struct FindOpts {
    pub text: String,
    pub case: bool,
    pub regex: bool,
    pub whole_word: bool,
    pub whole_cell: bool,
    pub selection_only: bool,
    pub highlight: bool,
}

impl FindOpts {
    pub fn compile(&self) -> Result<Option<Regex>, String> {
        if self.text.is_empty() {
            return Ok(None);
        }
        let mut p = if self.regex { self.text.clone() } else { regex::escape(&self.text) };
        if self.whole_word {
            p = format!(r"\b(?:{p})\b");
        }
        if self.whole_cell {
            p = format!("^(?:{p})$");
        }
        RegexBuilder::new(&p).case_insensitive(!self.case).build().map(Some).map_err(|e| e.to_string())
    }
}

/// Region to search, in view coordinates.
#[derive(Clone)]
pub struct Region {
    pub visible: Option<Arc<Vec<u32>>>,
    pub vcols: Vec<usize>,
    pub nrows: usize,
    /// Restrict to rect ((r0,c0),(r1,c1)) when searching the selection.
    pub rect: Option<((usize, usize), (usize, usize))>,
}

impl Region {
    fn data_row(&self, vr: usize) -> usize {
        match &self.visible {
            Some(v) => v[vr] as usize,
            None => vr,
        }
    }
    fn rows(&self) -> std::ops::Range<usize> {
        match self.rect {
            Some(((r0, _), (r1, _))) => r0..(r1 + 1).min(self.nrows),
            None => 0..self.nrows,
        }
    }
    fn cols(&self) -> std::ops::Range<usize> {
        match self.rect {
            Some(((_, c0), (_, c1))) => c0..(c1 + 1).min(self.vcols.len()),
            None => 0..self.vcols.len(),
        }
    }
}

pub fn find(snap: &Snap, region: &Region, re: &Regex, start: (usize, usize), forward: bool, cancel: &AtomicBool) -> Option<(usize, usize)> {
    let off = snap.index.offsets.read().unwrap();
    let off: &[u64] = &off;
    let rows = region.rows();
    let cols = region.cols();
    if rows.is_empty() || cols.is_empty() {
        return None;
    }
    let row_hits = |vr: usize| -> Vec<usize> {
        let vals = snap.row_values_in(off, snap.row_id(region.data_row(vr) + snap.hdr));
        cols.clone().filter(|&vc| vals.get(region.vcols[vc]).is_some_and(|v| re.is_match(v))).collect()
    };
    let (sr, sc) = (start.0.clamp(rows.start, rows.end - 1), start.1);
    let h = row_hits(sr);
    let same_row = if forward { h.iter().find(|&&c| c > sc) } else { h.iter().rev().find(|&&c| c < sc) };
    if let Some(&c) = same_row {
        return Some((sr, c));
    }
    let scan = |range: Vec<std::ops::Range<usize>>| -> Option<(usize, usize)> {
        for r in range {
            let hit = if forward {
                r.into_par_iter().with_min_len(256).find_map_first(|vr| {
                    if cancel.load(Ordering::Relaxed) {
                        return None;
                    }
                    row_hits(vr).first().map(|&c| (vr, c))
                })
            } else {
                r.into_par_iter().with_min_len(256).find_map_last(|vr| {
                    if cancel.load(Ordering::Relaxed) {
                        return None;
                    }
                    row_hits(vr).last().map(|&c| (vr, c))
                })
            };
            if hit.is_some() {
                return hit;
            }
        }
        None
    };
    let hit = if forward {
        scan(vec![sr + 1..rows.end, rows.start..sr])
    } else {
        scan(vec![rows.start..sr, sr + 1..rows.end])
    };
    hit.or_else(|| {
        // Wrap to the other cells of the starting row.
        if forward { h.first().map(|&c| (sr, c)) } else { h.last().map(|&c| (sr, c)) }
    })
}

pub fn count_matches(snap: &Snap, region: &Region, re: &Regex, cancel: &AtomicBool) -> usize {
    let off = snap.index.offsets.read().unwrap();
    let off: &[u64] = &off;
    let cols = region.cols();
    region
        .rows()
        .into_par_iter()
        .with_min_len(256)
        .map(|vr| {
            if cancel.load(Ordering::Relaxed) {
                return 0;
            }
            let vals = snap.row_values_in(off, snap.row_id(region.data_row(vr) + snap.hdr));
            cols.clone().filter(|&vc| vals.get(region.vcols[vc]).is_some_and(|v| re.is_match(v))).count()
        })
        .sum()
}

/// Compute replacements (table row, table col, new value) for every match in the region.
pub fn replace_all(snap: &Snap, region: &Region, re: &Regex, with: &str, use_regex: bool, cancel: &AtomicBool) -> Vec<(usize, usize, String)> {
    let off = snap.index.offsets.read().unwrap();
    let off: &[u64] = &off;
    let cols = region.cols();
    region
        .rows()
        .into_par_iter()
        .with_min_len(256)
        .flat_map_iter(|vr| {
            let mut out = Vec::new();
            if cancel.load(Ordering::Relaxed) {
                return out;
            }
            let t = region.data_row(vr) + snap.hdr;
            let vals = snap.row_values_in(off, snap.row_id(t));
            for vc in cols.clone() {
                let c = region.vcols[vc];
                if let Some(v) = vals.get(c) {
                    if re.is_match(v) {
                        let nv = if use_regex {
                            re.replace_all(v, with).into_owned()
                        } else {
                            re.replace_all(v, regex::NoExpand(with)).into_owned()
                        };
                        out.push((t, c, nv));
                    }
                }
            }
            out
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Duplicates / empties
// ---------------------------------------------------------------------------

fn hash_vals<'a>(it: impl Iterator<Item = &'a String>) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for v in it {
        v.hash(&mut h);
        0xFFu8.hash(&mut h);
    }
    h.finish()
}

/// Data rows that duplicate an earlier row (comparing `cols`, or all when empty).
pub fn duplicate_rows(snap: &Snap, cols: &[usize], cancel: &AtomicBool) -> Vec<usize> {
    let off = snap.index.offsets.read().unwrap();
    let off: &[u64] = &off;
    let n = snap.total.saturating_sub(snap.hdr);
    let key = |d: usize| -> Vec<String> {
        let vals = snap.row_values_in(off, snap.row_id(d + snap.hdr));
        if cols.is_empty() {
            // Ignore trailing empties so ragged rows compare equal.
            let mut v = vals;
            while v.last().is_some_and(|s| s.is_empty()) {
                v.pop();
            }
            v
        } else {
            cols.iter().map(|&c| vals.get(c).cloned().unwrap_or_default()).collect()
        }
    };
    let hashes: Vec<u64> = (0..n).into_par_iter().map(|d| if cancel.load(Ordering::Relaxed) { 0 } else { hash_vals(key(d).iter()) }).collect();
    let mut seen: HashMap<u64, Vec<usize>> = HashMap::new();
    let mut dups = Vec::new();
    for (d, h) in hashes.into_iter().enumerate() {
        let entry = seen.entry(h).or_default();
        if !entry.is_empty() {
            let k = key(d);
            if entry.iter().any(|&e| key(e) == k) {
                dups.push(d);
                continue;
            }
        }
        entry.push(d);
    }
    dups
}

pub fn empty_rows(snap: &Snap, cols: &[usize], cancel: &AtomicBool) -> Vec<usize> {
    let off = snap.index.offsets.read().unwrap();
    let off: &[u64] = &off;
    let n = snap.total.saturating_sub(snap.hdr);
    (0..n)
        .into_par_iter()
        .filter(|&d| {
            if cancel.load(Ordering::Relaxed) {
                return false;
            }
            let vals = snap.row_values_in(off, snap.row_id(d + snap.hdr));
            if cols.is_empty() {
                vals.iter().all(|v| v.trim().is_empty())
            } else {
                cols.iter().all(|&c| vals.get(c).is_none_or(|v| v.trim().is_empty()))
            }
        })
        .collect()
}

/// Per-column value fingerprints over the given data rows (all when None).
fn column_hashes(snap: &Snap, rows: Option<&[usize]>, cancel: &AtomicBool) -> (Vec<u64>, Vec<bool>) {
    let off = snap.index.offsets.read().unwrap();
    let off: &[u64] = &off;
    let n = snap.total.saturating_sub(snap.hdr);
    let rows: Vec<usize> = rows.map(|r| r.to_vec()).unwrap_or_else(|| (0..n).collect());
    let nc = snap.cols.len();
    let (hs, empties) = rows
        .par_iter()
        .fold(
            || (vec![std::collections::hash_map::DefaultHasher::new(); nc], vec![true; nc]),
            |(mut hs, mut em), &d| {
                if cancel.load(Ordering::Relaxed) {
                    return (hs, em);
                }
                let vals = snap.row_values_in(off, snap.row_id(d + snap.hdr));
                for c in 0..nc {
                    // Order-dependent hash per row: mix in the row number.
                    (d, &vals[c]).hash(&mut hs[c]);
                    if !vals[c].trim().is_empty() {
                        em[c] = false;
                    }
                }
                (hs, em)
            },
        )
        .map(|(hs, em)| (hs.into_iter().map(|h| h.finish()).collect::<Vec<u64>>(), em))
        .reduce(
            || (vec![0u64; nc], vec![true; nc]),
            |(a, ea), (b, eb)| {
                (a.iter().zip(&b).map(|(x, y)| x.wrapping_add(*y)).collect(), ea.iter().zip(&eb).map(|(x, y)| *x && *y).collect())
            },
        );
    (hs, empties)
}

pub fn duplicate_cols(snap: &Snap, rows: Option<&[usize]>, cancel: &AtomicBool) -> Vec<usize> {
    let (hs, _) = column_hashes(snap, rows, cancel);
    let mut seen = HashSet::new();
    (0..hs.len()).filter(|&c| !seen.insert(hs[c])).collect()
}

pub fn empty_cols(snap: &Snap, rows: Option<&[usize]>, cancel: &AtomicBool) -> Vec<usize> {
    let (_, em) = column_hashes(snap, rows, cancel);
    (0..em.len()).filter(|&c| em[c]).collect()
}

// ---------------------------------------------------------------------------
// Save
// ---------------------------------------------------------------------------

fn write_field(out: &mut Vec<u8>, s: &str, o: &SaveOpts) {
    let d = o.delimiter as char;
    let quote = match o.quoting {
        Quoting::All => true,
        Quoting::Never => false,
        Quoting::AsNeeded => s.contains(|c| c == d || c == '"' || c == '\n' || c == '\r'),
    };
    if quote {
        let q = s.replace('"', "\"\"");
        out.extend_from_slice(&encode_str(o.encoding, "\""));
        out.extend_from_slice(&encode_str(o.encoding, &q));
        out.extend_from_slice(&encode_str(o.encoding, "\""));
    } else {
        out.extend_from_slice(&encode_str(o.encoding, s));
    }
}

pub struct SavePlan {
    pub opts: SaveOpts,
    /// Unedited disk records may be copied byte-for-byte.
    pub raw_ok: bool,
    pub cols_identity: bool,
    /// Visible-only export: (visible data rows, table cols).
    pub subset: Option<(Option<Arc<Vec<u32>>>, Vec<usize>)>,
}

/// Stream the table to a temp file next to `target`. Returns the temp path.
pub fn save(snap: &Snap, plan: &SavePlan, target: &Path, cancel: &AtomicBool) -> Result<PathBuf, String> {
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = target.with_file_name(format!(".{name}.ficsv-tmp"));
    let o = &plan.opts;
    let run = || -> std::io::Result<()> {
        let mut w = BufWriter::with_capacity(4 << 20, File::create(&tmp)?);
        let src = &snap.src;
        let offsets = snap.index.offsets.read().unwrap();
        let eol = encode_str(o.encoding, if o.line_ending.bytes() == b"\n" { "\n" } else { "\r\n" }).into_owned();
        let delim = encode_str(o.encoding, &(o.delimiter as char).to_string()).into_owned();
        if o.bom {
            w.write_all(bom_bytes(o.encoding))?;
        }
        let mut line = Vec::new();
        let write_vals = |line: &mut Vec<u8>, vals: &[String]| {
            line.clear();
            for (c, f) in vals.iter().enumerate() {
                if c > 0 {
                    line.extend_from_slice(&delim);
                }
                write_field(line, f, o);
            }
        };
        let rows: Box<dyn Iterator<Item = usize>> = match &plan.subset {
            None => Box::new(0..snap.total),
            Some((vis, _)) => {
                let hdr = 0..snap.hdr;
                match vis {
                    Some(v) => {
                        let v = v.clone();
                        let h = snap.hdr;
                        Box::new(hdr.chain((0..v.len()).map(move |i| v[i] as usize + h)))
                    }
                    None => Box::new(0..snap.total),
                }
            }
        };
        let mut first = true;
        for (k, t) in rows.enumerate() {
            if k % 65536 == 0 && cancel.load(Ordering::Relaxed) {
                return Err(std::io::Error::other("cancelled"));
            }
            if !first {
                w.write_all(&eol)?;
            }
            first = false;
            let id = snap.row_id(t);
            let edited = snap.edits.get(&id);
            if let Some((_, cols)) = &plan.subset {
                let vals = snap.row_values_in(&offsets, id);
                let sub: Vec<String> = cols.iter().map(|&c| vals.get(c).cloned().unwrap_or_default()).collect();
                write_vals(&mut line, &sub);
                w.write_all(&line)?;
                continue;
            }
            if id < NEW_ID && (id as usize) < offsets.len() && plan.raw_ok && edited.is_none() {
                w.write_all(src.record(&offsets, id as usize))?;
                continue;
            }
            let mut vals = snap.row_values_in(&offsets, id);
            if plan.cols_identity && id < NEW_ID && (id as usize) < offsets.len() {
                // Keep ragged rows ragged: don't pad short records with empty fields.
                let disk_n = src.fields(src.record(&offsets, id as usize)).count();
                let edit_n = edited.map_or(0, |e| e.keys().map(|&c| c as usize + 1).max().unwrap_or(0));
                vals.truncate(disk_n.max(edit_n));
            }
            write_vals(&mut line, &vals);
            w.write_all(&line)?;
        }
        if !first && (snap.src.trailing_newline || plan.subset.is_some() || snap.rows.is_some()) {
            w.write_all(&eol)?;
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
