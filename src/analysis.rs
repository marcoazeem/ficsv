//! "File Analysis" window: Statistics, Column Analysis, Unique Values and Pivot Table.
//!
//! All data is read through [`TableRead`]. Heavy computation runs on a background
//! thread (using rayon inside) and results are delivered through a channel. Each tab has
//! its own job; starting a new job for a tab cancels the previous one.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};

use eframe::egui;
use rayon::prelude::*;

use crate::table::TableRead;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AnalysisTab {
    Statistics,
    ColumnAnalysis,
    UniqueValues,
    PivotTable,
}

impl AnalysisTab {
    const ALL: [AnalysisTab; 4] = [
        AnalysisTab::Statistics,
        AnalysisTab::ColumnAnalysis,
        AnalysisTab::UniqueValues,
        AnalysisTab::PivotTable,
    ];

    fn idx(self) -> usize {
        match self {
            AnalysisTab::Statistics => 0,
            AnalysisTab::ColumnAnalysis => 1,
            AnalysisTab::UniqueValues => 2,
            AnalysisTab::PivotTable => 3,
        }
    }

    fn label(self) -> &'static str {
        match self {
            AnalysisTab::Statistics => "Statistics",
            AnalysisTab::ColumnAnalysis => "Column Analysis",
            AnalysisTab::UniqueValues => "Unique Values",
            AnalysisTab::PivotTable => "Pivot Table",
        }
    }
}

pub enum AnalysisAction {
    /// Unique Values: user clicked "Selected Values In" (keep=true) or "Selected Values Out" (keep=false).
    FilterValues { col: usize, values: Vec<String>, keep: bool },
}

// ---------------------------------------------------------------------------
// Value parsing helpers
// ---------------------------------------------------------------------------

/// Parses a cell as a number. Accepts optional surrounding whitespace and thousands
/// separators ("1,234.5"). Rejects NaN/infinity spellings.
pub(crate) fn parse_num(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let first = t.as_bytes()[0];
    if !(first.is_ascii_digit() || first == b'-' || first == b'+' || first == b'.') {
        return None;
    }
    if let Ok(v) = t.parse::<f64>() {
        return v.is_finite().then_some(v);
    }
    if t.contains(',') {
        // Thousands separators: groups of exactly three digits after each comma.
        let (int_part, _) = t.split_once('.').unwrap_or((t, ""));
        let digits = int_part.trim_start_matches(['-', '+']);
        let mut groups = digits.split(',');
        let head = groups.next().unwrap_or("");
        if head.is_empty() || head.len() > 3 || groups.clone().any(|g| g.len() != 3) {
            return None;
        }
        let v = t.replace(',', "").parse::<f64>().ok()?;
        return v.is_finite().then_some(v);
    }
    None
}

const DATE_FORMATS: &[&str] = &[
    "%Y-%m-%d", "%Y/%m/%d", "%Y.%m.%d", "%Y%m%d", "%m/%d/%Y", "%m-%d-%Y", "%m/%d/%y", "%d.%m.%Y",
    "%d.%m.%y", "%d-%m-%Y", "%b %d %Y", "%b %d, %Y", "%B %d %Y", "%B %d, %Y", "%d %b %Y",
    "%d %B %Y", "%d-%b-%Y", "%d-%b-%y", "%a, %d %b %Y", "%A, %B %d, %Y",
];

const DATETIME_FORMATS: &[&str] = &[
    "%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M",
    "%m/%d/%Y %H:%M:%S", "%m/%d/%Y %H:%M", "%m/%d/%Y %I:%M %p", "%m/%d/%Y %I:%M:%S %p",
    "%d.%m.%Y %H:%M:%S", "%d.%m.%Y %H:%M", "%b %d %Y %H:%M:%S", "%b %d, %Y %H:%M:%S",
];

/// Whether a (non-empty) value parses as a date or date-time in a common format.
pub(crate) fn is_date(s: &str) -> bool {
    use chrono::{DateTime, NaiveDate, NaiveDateTime};
    let t = s.trim();
    if t.len() < 6 || t.len() > 40 {
        return false;
    }
    // A pure 8-digit number is only a date in %Y%m%d with a plausible year.
    if t.bytes().all(|b| b.is_ascii_digit()) && t.len() != 8 {
        return false;
    }
    if DATE_FORMATS.iter().any(|f| NaiveDate::parse_from_str(t, f).is_ok()) {
        return true;
    }
    if DATETIME_FORMATS.iter().any(|f| NaiveDateTime::parse_from_str(t, f).is_ok()) {
        return true;
    }
    DateTime::parse_from_rfc3339(t).is_ok() || DateTime::parse_from_rfc2822(t).is_ok()
}

/// Formats a number compactly: integers without decimals, others with up to 6 decimals.
pub(crate) fn fmt_num(v: f64) -> String {
    if !v.is_finite() {
        return String::new();
    }
    if v == v.trunc() && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let s = if v.abs() >= 1e15 || v.abs() < 1e-6 {
        format!("{:.6e}", v)
    } else {
        format!("{:.6}", v)
    };
    if s.contains('e') {
        return s;
    }
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Linear-interpolated percentile (inclusive, like Excel's PERCENTILE.INC) on sorted data.
fn percentile_sorted(sorted: &[f64], p: f64) -> f64 {
    let n = sorted.len();
    if n == 0 {
        return f64::NAN;
    }
    if n == 1 {
        return sorted[0];
    }
    let pos = p.clamp(0.0, 1.0) * (n - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = (lo + 1).min(n - 1);
    let frac = pos - lo as f64;
    sorted[lo] + (sorted[hi] - sorted[lo]) * frac
}

/// Compares two values "naturally": numerically when both are numbers, else as text.
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    match (parse_num(a), parse_num(b)) {
        (Some(x), Some(y)) => x.total_cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        _ => a.cmp(b),
    }
}

const KEY_SEP: char = '\u{1f}';

fn key_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let mut ia = a.split(KEY_SEP);
    let mut ib = b.split(KEY_SEP);
    loop {
        match (ia.next(), ib.next()) {
            (Some(x), Some(y)) => match natural_cmp(x, y) {
                std::cmp::Ordering::Equal => continue,
                o => return o,
            },
            (None, None) => return std::cmp::Ordering::Equal,
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
        }
    }
}

fn tsv_escape(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

// ---------------------------------------------------------------------------
// Pure computations
// ---------------------------------------------------------------------------

/// Descriptive statistics of a set of numeric values.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NumStats {
    pub count: usize,
    pub sum: f64,
    pub mean: f64,
    /// Biased (population) variance.
    pub variance: f64,
    /// Biased (population) standard deviation.
    pub std_dev: f64,
    pub median: f64,
    pub min: f64,
    pub max: f64,
    /// 20th, 40th, 60th, 80th percentiles.
    pub percentiles: [f64; 4],
}

impl NumStats {
    /// Computes stats from values (the vector is sorted in place).
    pub fn from_values(values: &mut [f64]) -> NumStats {
        let n = values.len();
        if n == 0 {
            return NumStats::default();
        }
        if n > 100_000 {
            values.par_sort_unstable_by(f64::total_cmp);
        } else {
            values.sort_unstable_by(f64::total_cmp);
        }
        let sum: f64 = values.iter().sum();
        let mean = sum / n as f64;
        let variance = values.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / n as f64;
        NumStats {
            count: n,
            sum,
            mean,
            variance,
            std_dev: variance.sqrt(),
            median: percentile_sorted(values, 0.5),
            min: values[0],
            max: values[n - 1],
            percentiles: [0.2, 0.4, 0.6, 0.8].map(|p| percentile_sorted(values, p)),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct StatsRow {
    pub name: String,
    pub stats: NumStats,
}

const STATS_HEADER: [&str; 13] = [
    "Column", "Count", "Sum", "Mean", "Variance", "Std Dev", "Median", "Min", "Max",
    "20th Percentile", "40th Percentile", "60th Percentile", "80th Percentile",
];

fn stats_to_table(rows: &[StatsRow]) -> StrTable {
    let body = rows
        .iter()
        .map(|r| {
            let s = &r.stats;
            let mut v = vec![r.name.clone(), s.count.to_string()];
            if s.count == 0 {
                v.extend(std::iter::repeat_n(String::new(), 11));
            } else {
                v.extend([s.sum, s.mean, s.variance, s.std_dev, s.median, s.min, s.max].map(fmt_num));
                v.extend(s.percentiles.map(fmt_num));
            }
            v
        })
        .collect();
    StrTable::new(vec![STATS_HEADER.iter().map(|s| s.to_string()).collect()], body)
}

fn collect_column_numbers(table: &dyn TableRead, col: usize, cancel: &AtomicBool) -> Vec<f64> {
    (0..table.nrows())
        .into_par_iter()
        .with_min_len(4096)
        .filter_map(|r| {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            parse_num(&table.cell(r, col))
        })
        .collect()
}

fn collect_cell_numbers(table: &dyn TableRead, cells: &[(usize, usize)], cancel: &AtomicBool) -> Vec<f64> {
    cells
        .par_iter()
        .with_min_len(4096)
        .filter_map(|&(r, c)| {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            parse_num(&table.cell(r, c))
        })
        .collect()
}

/// Statistics per column, or one combined "Selected Cells" row when `by_columns` is false.
/// `cells` restricts analysis to explicit cells (None = whole columns).
/// Returns None if cancelled.
pub fn compute_stats(
    table: &dyn TableRead,
    cols: &[usize],
    cells: Option<&[(usize, usize)]>,
    by_columns: bool,
    cancel: &AtomicBool,
) -> Option<Vec<StatsRow>> {
    let mut out = Vec::new();
    if by_columns {
        for &c in cols {
            let mut vals = match cells {
                Some(cells) => {
                    let sub: Vec<(usize, usize)> = cells.iter().copied().filter(|&(_, cc)| cc == c).collect();
                    collect_cell_numbers(table, &sub, cancel)
                }
                None => collect_column_numbers(table, c, cancel),
            };
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            out.push(StatsRow { name: table.column_name(c), stats: NumStats::from_values(&mut vals) });
        }
    } else {
        let mut vals = match cells {
            Some(cells) => collect_cell_numbers(table, cells, cancel),
            None => {
                let mut all = Vec::new();
                for &c in cols {
                    all.extend(collect_column_numbers(table, c, cancel));
                }
                all
            }
        };
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        out.push(StatsRow { name: "Selected Cells".into(), stats: NumStats::from_values(&mut vals) });
    }
    Some(out)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataType {
    Number,
    Date,
    String,
}

impl DataType {
    fn label(self) -> &'static str {
        match self {
            DataType::Number => "Number",
            DataType::Date => "Date",
            DataType::String => "String",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ColumnInfo {
    pub name: String,
    pub fields: usize,
    pub non_empty: usize,
    pub empty: usize,
    pub percent_non_empty: f64,
    pub min_len: usize,
    pub max_len: usize,
    pub data_type: DataType,
}

#[derive(Clone, Copy)]
struct ColAcc {
    non_empty: usize,
    min_len: usize,
    max_len: usize,
    all_num: bool,
    all_date: bool,
}

impl ColAcc {
    fn new() -> Self {
        ColAcc { non_empty: 0, min_len: usize::MAX, max_len: 0, all_num: true, all_date: true }
    }
    fn merge(self, o: ColAcc) -> ColAcc {
        ColAcc {
            non_empty: self.non_empty + o.non_empty,
            min_len: self.min_len.min(o.min_len),
            max_len: self.max_len.max(o.max_len),
            all_num: self.all_num && o.all_num,
            all_date: self.all_date && o.all_date,
        }
    }
}

const COLUMN_HEADER: [&str; 8] = [
    "Column", "Number of Fields", "Non-Empty Fields", "Empty Fields", "Percent Non-Empty",
    "Min Field Length", "Max Field Length", "Data Type",
];

/// Per-column field counts, lengths (in chars) and inferred data type. None if cancelled.
pub fn compute_column_analysis(table: &dyn TableRead, cols: &[usize], cancel: &AtomicBool) -> Option<Vec<ColumnInfo>> {
    let n = table.nrows();
    let mut out = Vec::with_capacity(cols.len());
    for &c in cols {
        let acc = (0..n)
            .into_par_iter()
            .with_min_len(4096)
            .fold(ColAcc::new, |mut a, r| {
                if cancel.load(Ordering::Relaxed) {
                    return a;
                }
                let v = table.cell(r, c);
                let len = v.chars().count();
                a.min_len = a.min_len.min(len);
                a.max_len = a.max_len.max(len);
                if !v.is_empty() {
                    a.non_empty += 1;
                    if a.all_num && parse_num(&v).is_none() {
                        a.all_num = false;
                    }
                    if a.all_date && !is_date(&v) {
                        a.all_date = false;
                    }
                }
                a
            })
            .reduce(ColAcc::new, ColAcc::merge);
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        // A column whose values are all numbers is Number even if they also parse as dates.
        let data_type = if acc.non_empty == 0 {
            DataType::String
        } else if acc.all_num {
            DataType::Number
        } else if acc.all_date {
            DataType::Date
        } else {
            DataType::String
        };
        out.push(ColumnInfo {
            name: table.column_name(c),
            fields: n,
            non_empty: acc.non_empty,
            empty: n - acc.non_empty,
            percent_non_empty: if n == 0 { 0.0 } else { acc.non_empty as f64 * 100.0 / n as f64 },
            min_len: if n == 0 { 0 } else { acc.min_len },
            max_len: acc.max_len,
            data_type,
        });
    }
    Some(out)
}

fn columns_to_table(rows: &[ColumnInfo]) -> StrTable {
    let body = rows
        .iter()
        .map(|r| {
            vec![
                r.name.clone(),
                r.fields.to_string(),
                r.non_empty.to_string(),
                r.empty.to_string(),
                format!("{:.2}%", r.percent_non_empty),
                r.min_len.to_string(),
                r.max_len.to_string(),
                r.data_type.label().to_string(),
            ]
        })
        .collect();
    StrTable::new(vec![COLUMN_HEADER.iter().map(|s| s.to_string()).collect()], body)
}

/// Maximum number of distinct values kept for display in the Unique Values tab.
pub const UNIQUE_DISPLAY_CAP: usize = 10_000;

#[derive(Clone, Debug, PartialEq)]
pub struct UniqueResult {
    pub col: usize,
    pub name: String,
    /// Total number of rows considered.
    pub total: usize,
    /// Number of distinct values (may exceed `values.len()` when capped).
    pub distinct: usize,
    /// (value, count) sorted by count desc then value; capped at `cap`.
    pub values: Vec<(String, usize)>,
}

/// Distinct values of a column with counts, sorted by count descending. None if cancelled.
pub fn compute_unique(table: &dyn TableRead, col: usize, cap: usize, cancel: &AtomicBool) -> Option<UniqueResult> {
    let n = table.nrows();
    let map = (0..n)
        .into_par_iter()
        .with_min_len(4096)
        .fold(HashMap::<String, usize>::new, |mut m, r| {
            if !cancel.load(Ordering::Relaxed) {
                *m.entry(table.cell(r, col)).or_insert(0) += 1;
            }
            m
        })
        .reduce(HashMap::new, |mut a, b| {
            let (mut big, small) = if a.len() >= b.len() { (std::mem::take(&mut a), b) } else { (b, a) };
            for (k, v) in small {
                *big.entry(k).or_insert(0) += v;
            }
            big
        });
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    let distinct = map.len();
    let mut values: Vec<(String, usize)> = map.into_iter().collect();
    let cmp = |a: &(String, usize), b: &(String, usize)| b.1.cmp(&a.1).then_with(|| natural_cmp(&a.0, &b.0));
    if values.len() > cap && cap > 0 {
        values.select_nth_unstable_by(cap - 1, cmp);
        values.truncate(cap);
    } else if cap == 0 {
        values.clear();
    }
    values.par_sort_unstable_by(cmp);
    Some(UniqueResult { col, name: table.column_name(col), total: n, distinct, values })
}

fn unique_to_table(u: &UniqueResult) -> StrTable {
    let body = u
        .values
        .iter()
        .map(|(v, c)| vec![v.clone(), c.to_string(), format!("{:.2}%", pct(*c, u.total))])
        .collect();
    StrTable::new(vec![vec![u.name.clone(), "Count".into(), "Percent".into()]], body)
}

fn pct(c: usize, total: usize) -> f64 {
    if total == 0 { 0.0 } else { c as f64 * 100.0 / total as f64 }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Agg {
    Sum,
    Mean,
    Median,
    Count,
    Variance,
    StdDev,
}

impl Agg {
    pub const ALL: [Agg; 6] = [Agg::Sum, Agg::Mean, Agg::Median, Agg::Count, Agg::Variance, Agg::StdDev];
    pub fn label(self) -> &'static str {
        match self {
            Agg::Sum => "Sum",
            Agg::Mean => "Mean",
            Agg::Median => "Median",
            Agg::Count => "Count",
            Agg::Variance => "Variance",
            Agg::StdDev => "Std Dev",
        }
    }
    fn apply(self, s: &NumStats) -> String {
        if self == Agg::Count {
            return s.count.to_string();
        }
        if s.count == 0 {
            return String::new();
        }
        fmt_num(match self {
            Agg::Sum => s.sum,
            Agg::Mean => s.mean,
            Agg::Median => s.median,
            Agg::Variance => s.variance,
            Agg::StdDev => s.std_dev,
            Agg::Count => unreachable!(),
        })
    }
}

/// A plain table of strings: one or more header rows plus body rows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StrTable {
    pub header: Vec<Vec<String>>,
    pub rows: Vec<Vec<String>>,
    /// Display widths in points (computed from content).
    widths: Vec<f32>,
}

impl StrTable {
    pub fn new(header: Vec<Vec<String>>, rows: Vec<Vec<String>>) -> Self {
        let ncols = header.iter().chain(rows.iter()).map(|r| r.len()).max().unwrap_or(0);
        let mut chars = vec![0usize; ncols];
        for r in header.iter().chain(rows.iter().take(2000)) {
            for (i, c) in r.iter().enumerate() {
                chars[i] = chars[i].max(c.chars().count());
            }
        }
        let widths = chars.iter().map(|&n| (n as f32 * 7.5 + 16.0).clamp(48.0, 320.0)).collect();
        StrTable { header, rows, widths }
    }

    pub fn to_tsv(&self) -> String {
        let mut s = String::new();
        for r in self.header.iter().chain(self.rows.iter()) {
            let line: Vec<String> = r.iter().map(|c| tsv_escape(c)).collect();
            s.push_str(&line.join("\t"));
            s.push('\n');
        }
        s
    }
}

#[derive(Default)]
struct Group {
    rows: usize,
    vals: Vec<Vec<f64>>,
}

/// Builds a pivot table. `left` columns group rows, `top` columns group output columns,
/// `data` columns are aggregated with each of `aggs`. With no data columns, row counts are
/// shown. Returns None if cancelled.
pub fn compute_pivot(
    table: &dyn TableRead,
    left: &[usize],
    top: &[usize],
    data: &[usize],
    aggs: &[Agg],
    cancel: &AtomicBool,
) -> Option<StrTable> {
    let n = table.nrows();
    let join = |r: usize, cols: &[usize]| -> String {
        let mut k = String::new();
        for (i, &c) in cols.iter().enumerate() {
            if i > 0 {
                k.push(KEY_SEP);
            }
            k.push_str(&table.cell(r, c));
        }
        k
    };
    let groups: HashMap<(String, String), Group> = (0..n)
        .into_par_iter()
        .with_min_len(4096)
        .fold(HashMap::<(String, String), Group>::new, |mut m, r| {
            if cancel.load(Ordering::Relaxed) {
                return m;
            }
            let g = m.entry((join(r, left), join(r, top))).or_insert_with(|| Group {
                rows: 0,
                vals: vec![Vec::new(); data.len()],
            });
            g.rows += 1;
            for (i, &c) in data.iter().enumerate() {
                if let Some(v) = parse_num(&table.cell(r, c)) {
                    g.vals[i].push(v);
                }
            }
            m
        })
        .reduce(HashMap::new, |mut a, b| {
            for (k, g) in b {
                match a.get_mut(&k) {
                    Some(e) => {
                        e.rows += g.rows;
                        for (dst, src) in e.vals.iter_mut().zip(g.vals) {
                            dst.extend(src);
                        }
                    }
                    None => {
                        a.insert(k, g);
                    }
                }
            }
            a
        });
    if cancel.load(Ordering::Relaxed) {
        return None;
    }

    let mut left_keys: Vec<String> = groups.keys().map(|k| k.0.clone()).collect::<HashSet<_>>().into_iter().collect();
    let mut top_keys: Vec<String> = groups.keys().map(|k| k.1.clone()).collect::<HashSet<_>>().into_iter().collect();
    left_keys.sort_by(|a, b| key_cmp(a, b));
    top_keys.sort_by(|a, b| key_cmp(a, b));
    if top_keys.is_empty() {
        top_keys.push(String::new());
    }
    if left_keys.is_empty() {
        left_keys.push(String::new());
    }

    // Value columns: (top key index, data index or None for row count, agg).
    let mut vcols: Vec<(usize, Option<usize>, Agg)> = Vec::new();
    for ti in 0..top_keys.len() {
        if data.is_empty() {
            vcols.push((ti, None, Agg::Count));
        } else {
            for di in 0..data.len() {
                for &a in aggs {
                    vcols.push((ti, Some(di), a));
                }
            }
        }
    }

    let lw = left.len().max(1);
    let mut header = Vec::new();
    for (lvl, &tc) in top.iter().enumerate() {
        let mut h = vec![String::new(); lw];
        h[lw - 1] = table.column_name(tc);
        for &(ti, _, _) in &vcols {
            h.push(top_keys[ti].split(KEY_SEP).nth(lvl).unwrap_or("").to_string());
        }
        header.push(h);
    }
    let mut h: Vec<String> = if left.is_empty() {
        vec![String::new()]
    } else {
        left.iter().map(|&c| table.column_name(c)).collect()
    };
    for &(_, di, a) in &vcols {
        h.push(match di {
            Some(di) => format!("{} of {}", a.label(), table.column_name(data[di])),
            None => "Count".to_string(),
        });
    }
    header.push(h);

    let rows: Vec<Vec<String>> = left_keys
        .par_iter()
        .map(|lk| {
            let mut row: Vec<String> = if left.is_empty() {
                vec!["All".to_string()]
            } else {
                lk.split(KEY_SEP).map(str::to_string).collect()
            };
            row.resize(lw, String::new());
            // Compute stats once per (top, data) pair.
            let mut cache: HashMap<(usize, usize), NumStats> = HashMap::new();
            for &(ti, di, a) in &vcols {
                let g = groups.get(&(lk.clone(), top_keys[ti].clone()));
                let cell = match (g, di) {
                    (None, _) => String::new(),
                    (Some(g), None) => g.rows.to_string(),
                    (Some(g), Some(di)) => {
                        let s = cache.entry((ti, di)).or_insert_with(|| {
                            let mut v = g.vals[di].clone();
                            NumStats::from_values(&mut v)
                        });
                        a.apply(s)
                    }
                };
                row.push(cell);
            }
            row
        })
        .collect();
    Some(StrTable::new(header, rows))
}

// ---------------------------------------------------------------------------
// Background jobs
// ---------------------------------------------------------------------------

enum JobOutput {
    Table(StrTable),
    Unique(UniqueResult, StrTable),
}

struct Job {
    cancel: Arc<AtomicBool>,
    rx: Receiver<JobOutput>,
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn spawn_job(
    ctx_slot: &Arc<Mutex<Option<egui::Context>>>,
    work: impl FnOnce(&AtomicBool) -> Option<JobOutput> + Send + 'static,
) -> Job {
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    let c2 = cancel.clone();
    let slot = ctx_slot.clone();
    std::thread::spawn(move || {
        if let Some(out) = work(&c2)
            && !c2.load(Ordering::Relaxed)
        {
            let _ = tx.send(out);
        }
        if let Some(ctx) = slot.lock().ok().and_then(|g| g.clone()) {
            ctx.request_repaint();
        }
    });
    Job { cancel, rx }
}

// ---------------------------------------------------------------------------
// Pivot configuration
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
struct PivotItem {
    col: usize,
    enabled: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct PivotConfig {
    left: Vec<PivotItem>,
    top: Vec<PivotItem>,
    data: Vec<PivotItem>,
    aggs: [bool; 6],
}

impl Default for PivotConfig {
    fn default() -> Self {
        PivotConfig { left: Vec::new(), top: Vec::new(), data: Vec::new(), aggs: [true, false, false, false, false, false] }
    }
}

impl PivotConfig {
    fn active(list: &[PivotItem]) -> Vec<usize> {
        list.iter().filter(|i| i.enabled).map(|i| i.col).collect()
    }
    fn active_aggs(&self) -> Vec<Agg> {
        Agg::ALL.iter().zip(self.aggs).filter(|(_, on)| *on).map(|(a, _)| *a).collect()
    }
}

/// Samples the first rows of a column to decide whether it is numeric.
fn sample_is_numeric(table: &dyn TableRead, col: usize) -> bool {
    let mut seen = 0;
    for r in 0..table.nrows().min(5000) {
        let v = table.cell(r, col);
        if v.trim().is_empty() {
            continue;
        }
        if parse_num(&v).is_none() {
            return false;
        }
        seen += 1;
        if seen >= 1000 {
            break;
        }
    }
    seen > 0
}

// ---------------------------------------------------------------------------
// Window
// ---------------------------------------------------------------------------

pub struct AnalysisWindow {
    open: bool,
    tab: AnalysisTab,
    table: Option<Arc<dyn TableRead>>,
    cols: Vec<usize>,
    cells: Option<Arc<Vec<(usize, usize)>>>,
    by_columns: bool,
    ctx_slot: Arc<Mutex<Option<egui::Context>>>,
    jobs: [Option<Job>; 4],
    results: [Option<StrTable>; 4],
    unique: Option<UniqueResult>,
    unique_sel: HashSet<usize>,
    unique_anchor: Option<usize>,
    pivot: PivotConfig,
    pivot_populated: bool,
    pivot_computed: Option<PivotConfig>,
}

impl Default for AnalysisWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl AnalysisWindow {
    pub fn new() -> Self {
        AnalysisWindow {
            open: false,
            tab: AnalysisTab::Statistics,
            table: None,
            cols: Vec::new(),
            cells: None,
            by_columns: true,
            ctx_slot: Arc::new(Mutex::new(None)),
            jobs: [None, None, None, None],
            results: [None, None, None, None],
            unique: None,
            unique_sel: HashSet::new(),
            unique_anchor: None,
            pivot: PivotConfig::default(),
            pivot_populated: false,
            pivot_computed: None,
        }
    }

    #[allow(dead_code)]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open on a tab and compute. `cols`: columns to analyse (None = all columns).
    /// `cells`: for Statistics "Selected Cells" mode: explicit (row, col) list (None = whole columns).
    pub fn open(
        &mut self,
        tab: AnalysisTab,
        table: Arc<dyn TableRead>,
        cols: Option<Vec<usize>>,
        cells: Option<Vec<(usize, usize)>>,
        by_columns: bool,
    ) {
        let ncols = table.ncols();
        self.cols = cols.unwrap_or_else(|| (0..ncols).collect());
        self.cols.retain(|&c| c < ncols);
        self.table = Some(table);
        self.cells = cells.map(Arc::new);
        self.by_columns = by_columns;
        self.jobs = [None, None, None, None]; // dropping cancels
        self.results = [None, None, None, None];
        self.unique = None;
        self.unique_sel.clear();
        self.unique_anchor = None;
        self.pivot = PivotConfig::default();
        self.pivot_populated = false;
        self.pivot_computed = None;
        self.open = true;
        self.tab = tab;
        self.compute(tab);
    }

    fn compute(&mut self, tab: AnalysisTab) {
        let Some(table) = self.table.clone() else { return };
        let cols = self.cols.clone();
        let job = match tab {
            AnalysisTab::Statistics => {
                let cells = self.cells.clone();
                let by_columns = self.by_columns;
                spawn_job(&self.ctx_slot, move |cancel| {
                    compute_stats(&*table, &cols, cells.as_deref().map(|v| v.as_slice()), by_columns, cancel)
                        .map(|r| JobOutput::Table(stats_to_table(&r)))
                })
            }
            AnalysisTab::ColumnAnalysis => spawn_job(&self.ctx_slot, move |cancel| {
                compute_column_analysis(&*table, &cols, cancel).map(|r| JobOutput::Table(columns_to_table(&r)))
            }),
            AnalysisTab::UniqueValues => {
                let col = cols.first().copied().unwrap_or(0);
                spawn_job(&self.ctx_slot, move |cancel| {
                    compute_unique(&*table, col, UNIQUE_DISPLAY_CAP, cancel).map(|u| {
                        let t = unique_to_table(&u);
                        JobOutput::Unique(u, t)
                    })
                })
            }
            AnalysisTab::PivotTable => {
                if !self.pivot_populated {
                    self.pivot_populated = true;
                    for &c in &self.cols {
                        let item = PivotItem { col: c, enabled: true };
                        if sample_is_numeric(&*table, c) {
                            self.pivot.data.push(item);
                        } else {
                            self.pivot.left.push(item);
                        }
                    }
                }
                let cfg = self.pivot.clone();
                self.pivot_computed = Some(cfg.clone());
                spawn_job(&self.ctx_slot, move |cancel| {
                    let left = PivotConfig::active(&cfg.left);
                    let top = PivotConfig::active(&cfg.top);
                    let data = PivotConfig::active(&cfg.data);
                    if left.is_empty() && top.is_empty() && data.is_empty() {
                        return Some(JobOutput::Table(StrTable::default()));
                    }
                    compute_pivot(&*table, &left, &top, &data, &cfg.active_aggs(), cancel).map(JobOutput::Table)
                })
            }
        };
        self.jobs[tab.idx()] = Some(job);
    }

    fn poll_jobs(&mut self) {
        for tab in AnalysisTab::ALL {
            let i = tab.idx();
            let Some(job) = &self.jobs[i] else { continue };
            match job.rx.try_recv() {
                Ok(JobOutput::Table(t)) => {
                    self.results[i] = Some(t);
                    self.jobs[i] = None;
                }
                Ok(JobOutput::Unique(u, t)) => {
                    self.unique = Some(u);
                    self.unique_sel.clear();
                    self.unique_anchor = None;
                    self.results[i] = Some(t);
                    self.jobs[i] = None;
                }
                Err(TryRecvError::Disconnected) => self.jobs[i] = None,
                Err(TryRecvError::Empty) => {}
            }
        }
    }

    /// Draw the window (call every frame). Returns an action for the app to perform.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<AnalysisAction> {
        if let Ok(mut g) = self.ctx_slot.lock() {
            *g = Some(ctx.clone());
        }
        if !self.open {
            return None;
        }
        self.poll_jobs();
        let mut open = self.open;
        let mut action = None;
        let win = egui::Window::new("File Analysis")
            .id(egui::Id::new("ficsv_file_analysis"))
            .open(&mut open)
            .default_size([820.0, 520.0])
            .resizable(true)
            .collapsible(false)
            .show(ctx, |ui| {
                action = self.ui(ui);
            });
        if let Some(win) = &win {
            crate::ui_kit::motion::pop_in(ctx, egui::Id::new("ficsv_file_analysis"), &win.response);
        }
        if !open {
            self.open = false;
            self.jobs = [None, None, None, None];
        }
        action
    }

    fn ui(&mut self, ui: &mut egui::Ui) -> Option<AnalysisAction> {
        // Tabs
        ui.horizontal(|ui| {
            for tab in AnalysisTab::ALL {
                if ui.selectable_label(self.tab == tab, tab.label()).clicked() && self.tab != tab {
                    self.tab = tab;
                    let i = tab.idx();
                    if self.results[i].is_none() && self.jobs[i].is_none() {
                        self.compute(tab);
                    }
                }
            }
        });
        ui.separator();

        let i = self.tab.idx();
        // Toolbar
        ui.horizontal(|ui| {
            if ui.button("Refresh").clicked() {
                self.compute(self.tab);
            }
            let has = self.results[i].as_ref().is_some_and(|t| !t.header.is_empty());
            if ui.add_enabled(has, egui::Button::new("Copy Whole Table")).clicked()
                && let Some(t) = &self.results[i]
            {
                ui.ctx().copy_text(t.to_tsv());
            }
            if self.jobs[i].is_some() {
                ui.spinner();
                ui.label("Computing…");
            }
            if let Some(t) = &self.table {
                ui.label(format!("{} rows", t.nrows()));
            }
        });
        ui.separator();

        match self.tab {
            AnalysisTab::Statistics | AnalysisTab::ColumnAnalysis => {
                if let Some(t) = &self.results[i] {
                    if t.rows.is_empty() {
                        ui.label("No columns to analyse.");
                    } else {
                        paint_table(ui, "analysis_tab_table", t, None);
                    }
                } else if self.jobs[i].is_none() {
                    ui.label("No results.");
                }
                None
            }
            AnalysisTab::UniqueValues => self.unique_ui(ui),
            AnalysisTab::PivotTable => {
                self.pivot_ui(ui);
                None
            }
        }
    }

    fn unique_ui(&mut self, ui: &mut egui::Ui) -> Option<AnalysisAction> {
        let Some(u) = &self.unique else {
            if self.jobs[AnalysisTab::UniqueValues.idx()].is_none() {
                ui.label("No results.");
            }
            return None;
        };
        let mut action = None;
        ui.horizontal(|ui| {
            ui.strong(format!("Column: {}", u.name));
            ui.label(format!("{} distinct values", u.distinct));
            let any = !self.unique_sel.is_empty();
            let selected = |sel: &HashSet<usize>| {
                let mut idx: Vec<usize> = sel.iter().copied().collect();
                idx.sort_unstable();
                idx.into_iter().filter_map(|i| u.values.get(i).map(|v| v.0.clone())).collect::<Vec<_>>()
            };
            if ui.add_enabled(any, egui::Button::new("Selected Values In")).clicked() {
                action = Some(AnalysisAction::FilterValues { col: u.col, values: selected(&self.unique_sel), keep: true });
            }
            if ui.add_enabled(any, egui::Button::new("Selected Values Out")).clicked() {
                action = Some(AnalysisAction::FilterValues { col: u.col, values: selected(&self.unique_sel), keep: false });
            }
        });
        if u.distinct > u.values.len() {
            ui.label(format!(
                "Showing the {} most frequent of {} distinct values.",
                u.values.len(),
                u.distinct
            ));
        }
        ui.label(egui::RichText::new("Click to select; Ctrl/Cmd-click to toggle, Shift-click for a range.").weak());

        let row_h = 20.0;
        let font = egui::TextStyle::Body.resolve(ui.style());
        let text_color = ui.visuals().text_color();
        let sel_fill = ui.visuals().selection.bg_fill;
        let sel_text = ui.visuals().selection.stroke.color;
        let bar_color = ui.visuals().widgets.active.bg_fill;
        let hover_fill = ui.visuals().widgets.hovered.weak_bg_fill;
        let max_count = u.values.first().map_or(1, |v| v.1).max(1);
        let val_w = 260.0;
        let cnt_w = 90.0;
        let pct_w = 80.0;

        // Header
        let (hrect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::hover());
        let hp = ui.painter_at(hrect);
        let strong = ui.visuals().strong_text_color();
        hp.text(hrect.left_center() + egui::vec2(4.0, 0.0), egui::Align2::LEFT_CENTER, "Value", font.clone(), strong);
        hp.text(egui::pos2(hrect.left() + val_w + cnt_w - 4.0, hrect.center().y), egui::Align2::RIGHT_CENTER, "Count", font.clone(), strong);
        hp.text(egui::pos2(hrect.left() + val_w + cnt_w + pct_w - 4.0, hrect.center().y), egui::Align2::RIGHT_CENTER, "Percent", font.clone(), strong);
        hp.text(egui::pos2(hrect.left() + val_w + cnt_w + pct_w + 8.0, hrect.center().y), egui::Align2::LEFT_CENTER, "Histogram", font.clone(), strong);

        let mut clicked: Option<usize> = None;
        let n = u.values.len();
        egui::ScrollArea::vertical()
            .id_salt("unique_values_scroll")
            .auto_shrink([false, false])
            .show_rows(ui, row_h, n, |ui, range| {
                for idx in range {
                    let (val, cnt) = &u.values[idx];
                    let (rect, resp) =
                        ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::click());
                    let selected = self.unique_sel.contains(&idx);
                    let p = ui.painter_at(rect);
                    if selected {
                        p.rect_filled(rect, 2.0, sel_fill);
                    } else if resp.hovered() {
                        p.rect_filled(rect, 2.0, hover_fill);
                    }
                    let tc = if selected { sel_text } else { text_color };
                    let vrect = egui::Rect::from_min_size(rect.min, egui::vec2(val_w - 8.0, row_h));
                    let shown: String = if val.is_empty() { "(empty)".into() } else { val.chars().take(200).collect() };
                    p.with_clip_rect(vrect).text(
                        rect.left_center() + egui::vec2(4.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        shown,
                        font.clone(),
                        if val.is_empty() { tc.gamma_multiply(0.6) } else { tc },
                    );
                    p.text(egui::pos2(rect.left() + val_w + cnt_w - 4.0, rect.center().y), egui::Align2::RIGHT_CENTER, cnt.to_string(), font.clone(), tc);
                    p.text(
                        egui::pos2(rect.left() + val_w + cnt_w + pct_w - 4.0, rect.center().y),
                        egui::Align2::RIGHT_CENTER,
                        format!("{:.2}%", pct(*cnt, u.total)),
                        font.clone(),
                        tc,
                    );
                    let bar_x = rect.left() + val_w + cnt_w + pct_w + 8.0;
                    let bar_max = (rect.right() - bar_x - 4.0).max(0.0);
                    let w = bar_max * (*cnt as f32 / max_count as f32);
                    if w > 0.0 {
                        p.rect_filled(
                            egui::Rect::from_min_size(egui::pos2(bar_x, rect.top() + 3.0), egui::vec2(w.max(1.0), row_h - 6.0)),
                            2.0,
                            bar_color,
                        );
                    }
                    if resp.clicked() {
                        clicked = Some(idx);
                    }
                    if resp.hovered() && !val.is_empty() && val.chars().count() > 30 {
                        resp.on_hover_text(val.as_str());
                    }
                }
            });

        if let Some(idx) = clicked {
            let m = ui.input(|i| i.modifiers);
            if m.shift && self.unique_anchor.is_some() {
                let a = self.unique_anchor.unwrap_or(idx);
                if !m.command {
                    self.unique_sel.clear();
                }
                for k in a.min(idx)..=a.max(idx) {
                    self.unique_sel.insert(k);
                }
            } else if m.command {
                if !self.unique_sel.remove(&idx) {
                    self.unique_sel.insert(idx);
                }
                self.unique_anchor = Some(idx);
            } else {
                self.unique_sel.clear();
                self.unique_sel.insert(idx);
                self.unique_anchor = Some(idx);
            }
        }
        action
    }

    fn pivot_ui(&mut self, ui: &mut egui::Ui) {
        let Some(table) = self.table.clone() else { return };
        let ncols = table.ncols();
        let names: Vec<String> = (0..ncols).map(|c| table.column_name(c)).collect();
        let mut refresh = false;

        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(250.0);
                egui::ScrollArea::vertical().id_salt("pivot_config_scroll").auto_shrink([true, false]).show(ui, |ui| {
                    for (li, title) in ["Left Headers", "Top Headers", "Data"].into_iter().enumerate() {
                        ui.strong(title);
                        let list = match li {
                            0 => &mut self.pivot.left,
                            1 => &mut self.pivot.top,
                            _ => &mut self.pivot.data,
                        };
                        let mut remove = None;
                        for (k, item) in list.iter_mut().enumerate() {
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut item.enabled, "")
                                    .on_hover_text("Uncheck to hide temporarily");
                                if ui.small_button("×").on_hover_text("Remove").clicked() {
                                    remove = Some(k);
                                }
                                ui.label(names.get(item.col).cloned().unwrap_or_default());
                            });
                        }
                        if let Some(k) = remove {
                            list.remove(k);
                        }
                        egui::ComboBox::from_id_salt(("pivot_add", li))
                            .selected_text("+ Add column")
                            .width(200.0)
                            .show_ui(ui, |ui| {
                                for (c, name) in names.iter().enumerate() {
                                    if ui.selectable_label(false, name).clicked() {
                                        list.push(PivotItem { col: c, enabled: true });
                                    }
                                }
                            });
                        ui.add_space(6.0);
                    }
                    ui.separator();
                    ui.strong("Aggregates");
                    ui.horizontal_wrapped(|ui| {
                        for (a, on) in Agg::ALL.iter().zip(self.pivot.aggs.iter_mut()) {
                            ui.checkbox(on, a.label());
                        }
                    });
                    ui.separator();
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("Refresh Data").clicked() {
                            refresh = true;
                        }
                        if ui.button("Clear Data").clicked() {
                            self.pivot.left.clear();
                            self.pivot.top.clear();
                            self.pivot.data.clear();
                        }
                        if ui.button("Swap Left and Top").clicked() {
                            std::mem::swap(&mut self.pivot.left, &mut self.pivot.top);
                        }
                    });
                });
            });
            ui.separator();
            ui.vertical(|ui| {
                let i = AnalysisTab::PivotTable.idx();
                match &self.results[i] {
                    Some(t) if !t.header.is_empty() => {
                        ui.horizontal(|ui| {
                            ui.label(format!("{} rows × {} columns", t.rows.len(), t.header[0].len()));
                            if ui.button("Copy Entire Table").clicked() {
                                ui.ctx().copy_text(t.to_tsv());
                            }
                        });
                        if paint_table(ui, "pivot_output", t, Some("Copy Entire Table")) {
                            ui.ctx().copy_text(t.to_tsv());
                        }
                    }
                    Some(_) => {
                        ui.label("Add columns to Left Headers, Top Headers or Data.");
                    }
                    None => {}
                }
            });
        });

        if refresh || self.pivot_computed.as_ref() != Some(&self.pivot) {
            self.compute(AnalysisTab::PivotTable);
        }
    }
}

/// Paints a virtualised table with sticky header rows. Returns true when the
/// context-menu copy entry (if `copy_label` is given) was clicked.
fn paint_table(ui: &mut egui::Ui, id: &str, t: &StrTable, copy_label: Option<&str>) -> bool {
    let row_h = 20.0;
    let font = egui::TextStyle::Body.resolve(ui.style());
    let text_color = ui.visuals().text_color();
    let strong = ui.visuals().strong_text_color();
    let stripe = ui.visuals().faint_bg_color;
    let line = ui.visuals().widgets.noninteractive.bg_stroke;
    let total_w: f32 = t.widths.iter().sum::<f32>().max(1.0);
    let mut copy = false;

    let draw_row = |ui: &mut egui::Ui, cells: &[String], header: bool, striped: bool, copy: &mut bool| {
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(total_w, row_h), egui::Sense::click());
        let p = ui.painter_at(rect);
        if striped {
            p.rect_filled(rect, 0.0, stripe);
        }
        let mut x = rect.left();
        for (ci, w) in t.widths.iter().enumerate() {
            let cell = egui::Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(*w, row_h));
            if let Some(s) = cells.get(ci) {
                let shown: String = s.chars().take(300).collect();
                let numeric = !header && ci > 0 && parse_num(s).is_some();
                let (pos, align) = if numeric {
                    (egui::pos2(cell.right() - 6.0, cell.center().y), egui::Align2::RIGHT_CENTER)
                } else {
                    (egui::pos2(cell.left() + 6.0, cell.center().y), egui::Align2::LEFT_CENTER)
                };
                p.with_clip_rect(cell.shrink2(egui::vec2(2.0, 0.0))).text(
                    pos,
                    align,
                    shown,
                    font.clone(),
                    if header { strong } else { text_color },
                );
            }
            x += w;
            p.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], line);
        }
        if header {
            p.line_segment([rect.left_bottom(), rect.right_bottom()], line);
        }
        if let Some(label) = copy_label {
            resp.context_menu(|ui| {
                if ui.button(label).clicked() {
                    *copy = true;
                    ui.close();
                }
            });
        }
    };

    egui::ScrollArea::horizontal().id_salt((id, "h")).auto_shrink([false, true]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for h in &t.header {
            draw_row(ui, h, true, false, &mut copy);
        }
        egui::ScrollArea::vertical()
            .id_salt((id, "v"))
            .auto_shrink([false, false])
            .show_rows(ui, row_h, t.rows.len(), |ui, range| {
                ui.set_min_width(total_w);
                for r in range {
                    draw_row(ui, &t.rows[r], false, r % 2 == 1, &mut copy);
                }
            });
    });
    copy
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::VecTable;

    fn tbl(header: &[&str], rows: &[&[&str]]) -> VecTable {
        VecTable {
            header: Some(header.iter().map(|s| s.to_string()).collect()),
            rows: rows.iter().map(|r| r.iter().map(|s| s.to_string()).collect()).collect(),
        }
    }

    fn no_cancel() -> AtomicBool {
        AtomicBool::new(false)
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn analysis_parse_num() {
        assert_eq!(parse_num(" 3.5 "), Some(3.5));
        assert_eq!(parse_num("-2"), Some(-2.0));
        assert_eq!(parse_num("1,234.5"), Some(1234.5));
        assert_eq!(parse_num("1,23"), None);
        assert_eq!(parse_num("abc"), None);
        assert_eq!(parse_num("NaN"), None);
        assert_eq!(parse_num("inf"), None);
        assert_eq!(parse_num(""), None);
    }

    #[test]
    fn analysis_dates() {
        for d in ["2024-01-05", "1/5/2024", "05.01.2024", "Jan 5 2024", "Jan 5, 2024", "2024-01-05T10:20:30", "5 January 2024"] {
            assert!(is_date(d), "{d}");
        }
        for d in ["hello", "12345", "2024-13-45", "3.5"] {
            assert!(!is_date(d), "{d}");
        }
    }

    #[test]
    fn analysis_fmt_num() {
        assert_eq!(fmt_num(3.0), "3");
        assert_eq!(fmt_num(2.5), "2.5");
        assert_eq!(fmt_num(1.0 / 3.0), "0.333333");
    }

    #[test]
    fn analysis_stats_per_column() {
        let t = tbl(&["a", "b"], &[&["1", "x"], &["2", ""], &["3", "5"], &["4", "y"], &["", "7"]]);
        let r = compute_stats(&t, &[0, 1], None, true, &no_cancel()).unwrap();
        assert_eq!(r.len(), 2);
        let a = &r[0].stats;
        assert_eq!(r[0].name, "a");
        assert_eq!(a.count, 4);
        assert!(close(a.sum, 10.0));
        assert!(close(a.mean, 2.5));
        assert!(close(a.variance, 1.25));
        assert!(close(a.std_dev, 1.25f64.sqrt()));
        assert!(close(a.median, 2.5));
        assert!(close(a.min, 1.0) && close(a.max, 4.0));
        // PERCENTILE.INC on [1,2,3,4]: 20th = 1.6, 80th = 3.4
        assert!(close(a.percentiles[0], 1.6));
        assert!(close(a.percentiles[3], 3.4));
        let b = &r[1].stats;
        assert_eq!(b.count, 2);
        assert!(close(b.sum, 12.0));
    }

    #[test]
    fn analysis_stats_selected_cells_combined() {
        let t = tbl(&["a", "b"], &[&["1", "10"], &["2", "20"], &["3", "30"]]);
        let cells = vec![(0, 0), (1, 1), (2, 1)];
        let r = compute_stats(&t, &[0, 1], Some(&cells), false, &no_cancel()).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].name, "Selected Cells");
        assert_eq!(r[0].stats.count, 3);
        assert!(close(r[0].stats.sum, 51.0));
        let r = compute_stats(&t, &[0, 1], Some(&cells), true, &no_cancel()).unwrap();
        assert_eq!(r[0].stats.count, 1);
        assert_eq!(r[1].stats.count, 2);
        // Empty column → blank stats in table form
        let t2 = tbl(&["e"], &[&["x"]]);
        let r = compute_stats(&t2, &[0], None, true, &no_cancel()).unwrap();
        let st = stats_to_table(&r);
        assert_eq!(st.rows[0][1], "0");
        assert_eq!(st.rows[0][2], "");
    }

    #[test]
    fn analysis_stats_cancel() {
        let t = tbl(&["a"], &[&["1"]]);
        let c = AtomicBool::new(true);
        assert!(compute_stats(&t, &[0], None, true, &c).is_none());
    }

    #[test]
    fn analysis_column_analysis() {
        let t = tbl(
            &["n", "d", "s", "e"],
            &[
                &["1", "2024-01-05", "hello", ""],
                &["2.5", "1/6/2024", "", ""],
                &["", "Jan 5 2024", "héllo!", ""],
            ],
        );
        let mixed = tbl(&["m"], &[&["1"], &["2024-01-05"]]);
        let m = compute_column_analysis(&mixed, &[0], &no_cancel()).unwrap();
        assert_eq!(m[0].data_type, DataType::String);
        let r = compute_column_analysis(&t, &[0, 1, 2, 3], &no_cancel()).unwrap();
        assert_eq!(r[0].fields, 3);
        assert_eq!(r[0].non_empty, 2);
        assert_eq!(r[0].empty, 1);
        assert!((r[0].percent_non_empty - 66.666).abs() < 0.01);
        assert_eq!(r[0].min_len, 0);
        assert_eq!(r[0].max_len, 3);
        assert_eq!(r[0].data_type, DataType::Number);
        assert_eq!(r[1].data_type, DataType::Date);
        assert_eq!(r[2].data_type, DataType::String);
        assert_eq!(r[2].max_len, 6); // chars, not bytes
        assert_eq!(r[3].non_empty, 0);
        assert_eq!(r[3].data_type, DataType::String);
    }

    #[test]
    fn analysis_unique_values() {
        let t = tbl(&["c"], &[&["b"], &["a"], &["b"], &[""], &["c"], &["b"], &["a"]]);
        let u = compute_unique(&t, 0, 100, &no_cancel()).unwrap();
        assert_eq!(u.total, 7);
        assert_eq!(u.distinct, 4);
        assert_eq!(u.values[0], ("b".to_string(), 3));
        assert_eq!(u.values[1], ("a".to_string(), 2));
        assert_eq!(u.values.len(), 4);
        let capped = compute_unique(&t, 0, 2, &no_cancel()).unwrap();
        assert_eq!(capped.distinct, 4);
        assert_eq!(capped.values, vec![("b".to_string(), 3), ("a".to_string(), 2)]);
        let tsv = unique_to_table(&u).to_tsv();
        assert!(tsv.starts_with("c\tCount\tPercent\nb\t3\t42.86%\n"));
    }

    #[test]
    fn analysis_pivot_basic() {
        let t = tbl(
            &["region", "year", "sales"],
            &[
                &["East", "2023", "10"],
                &["East", "2024", "20"],
                &["West", "2023", "5"],
                &["East", "2023", "30"],
                &["West", "2024", "x"],
            ],
        );
        let p = compute_pivot(&t, &[0], &[1], &[2], &[Agg::Sum, Agg::Count], &no_cancel()).unwrap();
        assert_eq!(p.header.len(), 2);
        assert_eq!(p.header[0], vec!["year", "2023", "2023", "2024", "2024"]);
        assert_eq!(p.header[1], vec!["region", "Sum of sales", "Count of sales", "Sum of sales", "Count of sales"]);
        assert_eq!(p.rows[0], vec!["East", "40", "2", "20", "1"]);
        assert_eq!(p.rows[1], vec!["West", "5", "1", "", "0"]);
    }

    #[test]
    fn analysis_pivot_no_top_no_data() {
        let t = tbl(&["k", "v"], &[&["b", "1"], &["a", "2"], &["b", "4"]]);
        let p = compute_pivot(&t, &[0], &[], &[], &[Agg::Sum], &no_cancel()).unwrap();
        assert_eq!(p.header, vec![vec!["k".to_string(), "Count".to_string()]]);
        assert_eq!(p.rows, vec![vec!["a".to_string(), "1".to_string()], vec!["b".to_string(), "2".to_string()]]);
        let p = compute_pivot(&t, &[0], &[], &[1], &[Agg::Mean, Agg::Median, Agg::Variance, Agg::StdDev], &no_cancel()).unwrap();
        assert_eq!(p.rows[1], vec!["b", "2.5", "2.5", "2.25", "1.5"]);
        // No left headers → single "All" row
        let p = compute_pivot(&t, &[], &[], &[1], &[Agg::Sum], &no_cancel()).unwrap();
        assert_eq!(p.rows, vec![vec!["All".to_string(), "7".to_string()]]);
    }

    #[test]
    fn analysis_pivot_numeric_sort() {
        let t = tbl(&["k"], &[&["10"], &["9"], &["100"]]);
        let p = compute_pivot(&t, &[0], &[], &[], &[], &no_cancel()).unwrap();
        let keys: Vec<&str> = p.rows.iter().map(|r| r[0].as_str()).collect();
        assert_eq!(keys, vec!["9", "10", "100"]);
    }

    #[test]
    fn analysis_window_background_compute() {
        let t: Arc<dyn TableRead> = Arc::new(tbl(&["a", "b"], &[&["1", "x"], &["2", "y"]]));
        let mut w = AnalysisWindow::new();
        w.open(AnalysisTab::PivotTable, t, None, None, true);
        assert!(w.is_open());
        assert_eq!(w.pivot.left.iter().map(|i| i.col).collect::<Vec<_>>(), vec![1]);
        assert_eq!(w.pivot.data.iter().map(|i| i.col).collect::<Vec<_>>(), vec![0]);
        for _ in 0..200 {
            w.poll_jobs();
            if w.results[AnalysisTab::PivotTable.idx()].is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let r = w.results[AnalysisTab::PivotTable.idx()].as_ref().expect("pivot result");
        assert_eq!(r.rows.len(), 2);
        assert_eq!(r.rows[0], vec!["x", "1"]);
    }
}
