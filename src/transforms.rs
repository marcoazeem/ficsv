//! Cell transforms (Modern CSV's "Transform" commands) applied to a rectangular
//! selection. The app renders a form from each transform's `params`, then calls
//! [`apply`] with the selected block.

use crate::table::{Param, ParamKind, ParamValues, TransformOutput};
use chrono::format::{Item, StrftimeItems};
use chrono::{DateTime, Duration, FixedOffset, NaiveDate, NaiveDateTime, Utc};
use rand::RngExt;
use regex::Regex;

pub struct Transform {
    pub id: &'static str,
    pub name: &'static str,
    pub params: Vec<Param>,
}

// ---------------------------------------------------------------------------
// Catalogue
// ---------------------------------------------------------------------------

const NUM_DOT: &str = "dot decimal (1,234.5)";
const NUM_COMMA: &str = "comma decimal (1.234,5)";

const DT_CUSTOM: &str = "Custom";
const DT_ISO_DT: &str = "ISO 8601 date+time";
const DT_ISO_D: &str = "ISO 8601 date";
const DT_UNIX_S: &str = "Unix seconds";
const DT_UNIX_MS: &str = "Unix milliseconds";
const DT_EXCEL: &str = "Excel serial";
const DT_CHOICES: [&str; 6] = [DT_CUSTOM, DT_ISO_DT, DT_ISO_D, DT_UNIX_S, DT_UNIX_MS, DT_EXCEL];

const TS_ISO: &str = "ISO 8601";
const TS_UNIX_S: &str = "Unix seconds";
const TS_UNIX_MS: &str = "Unix milliseconds";
const TS_CUSTOM: &str = "Custom";

fn p(key: &'static str, label: &'static str, kind: ParamKind, default: &'static str, help: &'static str) -> Param {
    Param { key, label, kind, default, help }
}

fn t(id: &'static str, name: &'static str, params: Vec<Param>) -> Transform {
    Transform { id, name, params }
}

/// Every transform, in menu order.
pub fn all() -> Vec<Transform> {
    use ParamKind::*;
    vec![
        t("upper_case", "Upper Case", vec![]),
        t("lower_case", "Lower Case", vec![]),
        t("title_case", "Title Case", vec![]),
        t("upper_camel_case", "Upper Camel Case", vec![]),
        t("lower_camel_case", "Lower Camel Case", vec![]),
        t("upper_underscore_case", "Upper Underscore Case", vec![]),
        t("lower_underscore_case", "Lower Underscore Case", vec![]),
        t("trim", "Trim Whitespace", vec![]),
        t("acronym", "Convert to Acronym", vec![]),
        t(
            "keep_left",
            "Keep Left Characters",
            vec![p("n", "Characters", Int, "1", "Negative values remove that many characters from the end")],
        ),
        t(
            "keep_right",
            "Keep Right Characters",
            vec![p("n", "Characters", Int, "1", "Negative values remove that many characters from the start")],
        ),
        t(
            "keep_middle",
            "Keep Middle Characters",
            vec![
                p("start", "Start index", Int, "0", "0-based index of the first character to keep"),
                p("n", "Characters", Int, "1", "Number of characters to keep"),
            ],
        ),
        t(
            "zero_fill",
            "Zero Fill",
            vec![p("digits", "Digits", Int, "5", "Whole numbers are padded with leading zeros to this many digits")],
        ),
        t("remove_leading_zeros", "Remove Leading Zeros", vec![]),
        t(
            "number_format",
            "Change Number Format",
            vec![p("to", "Convert to", Choice(vec![NUM_DOT, NUM_COMMA]), NUM_DOT, "")],
        ),
        t(
            "set_decimals",
            "Set Number of Decimals",
            vec![p("decimals", "Decimals", Int, "2", "Numbers are rounded to this many decimal places")],
        ),
        t("prepend", "Prepend", vec![p("text", "Text", Text, "", "")]),
        t("append", "Append", vec![p("text", "Text", Text, "", "")]),
        t(
            "convert_datetime",
            "Convert Date/Time Format",
            vec![
                p("from", "From", Choice(DT_CHOICES.to_vec()), DT_ISO_DT, ""),
                p("from_custom", "From custom format", Text, "%m/%d/%Y", "chrono strftime syntax, used when From is Custom"),
                p("to", "To", Choice(DT_CHOICES.to_vec()), DT_ISO_D, ""),
                p("to_custom", "To custom format", Text, "%Y-%m-%d", "chrono strftime syntax, used when To is Custom"),
            ],
        ),
        t(
            "fill_series",
            "Fill Series",
            vec![
                p("start", "Start", Float, "1", ""),
                p("step", "Step", Float, "1", ""),
                p("words", "Words", Text, "", "optional comma-separated list to cycle instead of numbers"),
            ],
        ),
        t(
            "random_int",
            "Random Integer",
            vec![
                p("min", "Minimum", Int, "1", ""),
                p("max", "Maximum", Int, "100", ""),
                p("values", "Values", Text, "", "optional comma-separated set to pick from instead"),
            ],
        ),
        t(
            "random_float",
            "Random Decimal Number",
            vec![
                p("min", "Minimum", Float, "0", ""),
                p("max", "Maximum", Float, "1", ""),
                p("decimals", "Decimals", Int, "2", ""),
            ],
        ),
        t(
            "uuid",
            "Generate UUID",
            vec![
                p("version", "Version", Choice(vec!["v4", "v7"]), "v4", "v7 UUIDs are time-ordered"),
                p("format", "Format", Choice(vec!["standard", "uppercase", "braced"]), "standard", ""),
            ],
        ),
        t(
            "timestamp_now",
            "Insert Current Timestamp",
            vec![
                p("format", "Format", Choice(vec![TS_ISO, TS_UNIX_S, TS_UNIX_MS, TS_CUSTOM]), TS_ISO, ""),
                p("custom", "Custom format", Text, "%Y-%m-%d %H:%M:%S", "chrono strftime syntax, used when Format is Custom"),
            ],
        ),
        t("copy_main_cell", "Copy Main Cell to Selection", vec![]),
        t("transpose_selection", "Transpose Selection", vec![]),
        t(
            "reshape_row_wise",
            "Reshape Row-wise",
            vec![
                p("count", "Count", Int, "1", ""),
                p("dimension", "Dimension", Choice(vec!["columns", "rows"]), "columns", "Whether Count is the number of columns or rows"),
            ],
        ),
        t(
            "reshape_column_wise",
            "Reshape Column-wise",
            vec![
                p("count", "Count", Int, "1", ""),
                p("dimension", "Dimension", Choice(vec!["columns", "rows"]), "rows", "Whether Count is the number of columns or rows"),
            ],
        ),
        t("split_horizontal", "Split Horizontally", split_params()),
        t("split_vertical", "Split Vertically", split_params()),
        t("join_horizontal", "Join Horizontally", vec![p("separator", "Separator", Text, ",", "")]),
        t("join_vertical", "Join Vertically", vec![p("separator", "Separator", Text, ",", "")]),
    ]
}

fn split_params() -> Vec<Param> {
    use ParamKind::*;
    vec![
        p("mode", "Split by", Choice(vec!["delimiter", "index"]), "delimiter", ""),
        p("delimiter", "Delimiter", Text, ",", "Used in delimiter mode; may be several characters"),
        p("index", "Index", Int, "1", "Used in index mode; negative counts from the right"),
        p("insert", "Insert new cells", Bool, "true", "Otherwise pieces overwrite neighbouring cells"),
    ]
}

// ---------------------------------------------------------------------------
// Parameter access
// ---------------------------------------------------------------------------

struct Args<'a> {
    values: &'a ParamValues,
    defs: &'a [Param],
}

impl Args<'_> {
    fn def(&self, key: &str) -> Option<&Param> {
        self.defs.iter().find(|p| p.key == key)
    }
    fn label(&self, key: &str) -> String {
        self.def(key).map_or_else(|| key.to_string(), |p| p.label.to_string())
    }
    fn text(&self, key: &str) -> String {
        match self.values.get(key) {
            Some(v) => v.clone(),
            None => self.def(key).map_or("", |p| p.default).to_string(),
        }
    }
    fn int(&self, key: &str) -> Result<i64, String> {
        let v = self.text(key);
        v.trim()
            .parse::<i64>()
            .map_err(|_| format!("{} must be a whole number", self.label(key)))
    }
    fn float(&self, key: &str) -> Result<f64, String> {
        let v = self.text(key);
        parse_number(v.trim()).ok_or_else(|| format!("{} must be a number", self.label(key)))
    }
    fn boolean(&self, key: &str) -> Result<bool, String> {
        match self.text(key).trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Ok(true),
            "false" | "0" | "no" | "" => Ok(false),
            _ => Err(format!("{} must be true or false", self.label(key))),
        }
    }
    fn choice(&self, key: &str) -> Result<String, String> {
        let v = self.text(key);
        if let Some(ParamKind::Choice(opts)) = self.def(key).map(|p| &p.kind)
            && !opts.iter().any(|o| *o == v)
        {
            return Err(format!("Invalid {}: {}", self.label(key).to_lowercase(), v));
        }
        Ok(v)
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

fn map_cells(block: &[Vec<String>], f: impl Fn(&str) -> String) -> TransformOutput {
    TransformOutput::same_shape(block.iter().map(|row| row.iter().map(|c| f(c)).collect()).collect())
}

fn fill_cells(block: &[Vec<String>], mut f: impl FnMut(usize, usize) -> String) -> TransformOutput {
    TransformOutput::same_shape(
        block
            .iter()
            .enumerate()
            .map(|(r, row)| (0..row.len()).map(|c| f(r, c)).collect())
            .collect(),
    )
}

/// Apply transform `id` to a rectangular block of selected cells (block[row][col], all rows same length).
pub fn apply(id: &str, params: &ParamValues, block: &[Vec<String>]) -> Result<TransformOutput, String> {
    let catalogue = all();
    let tr = catalogue.iter().find(|t| t.id == id).ok_or_else(|| format!("Unknown transform: {id}"))?;
    let a = Args { values: params, defs: &tr.params };

    Ok(match id {
        "upper_case" => map_cells(block, |s| s.to_uppercase()),
        "lower_case" => map_cells(block, |s| s.to_lowercase()),
        "title_case" => map_cells(block, title_case),
        "upper_camel_case" => map_cells(block, |s| camel_case(s, true)),
        "lower_camel_case" => map_cells(block, |s| camel_case(s, false)),
        "upper_underscore_case" => map_cells(block, |s| underscore_case(s, true)),
        "lower_underscore_case" => map_cells(block, |s| underscore_case(s, false)),
        "trim" => map_cells(block, |s| s.trim().to_string()),
        "acronym" => map_cells(block, acronym),
        "keep_left" => {
            let n = a.int("n")?;
            map_cells(block, |s| keep_left(s, n))
        }
        "keep_right" => {
            let n = a.int("n")?;
            map_cells(block, |s| keep_right(s, n))
        }
        "keep_middle" => {
            let start = a.int("start")?;
            let n = a.int("n")?;
            if start < 0 {
                return Err("Start index must be 0 or more".into());
            }
            if n < 0 {
                return Err("Characters must be 0 or more".into());
            }
            map_cells(block, |s| s.chars().skip(start as usize).take(n as usize).collect())
        }
        "zero_fill" => {
            let d = a.int("digits")?;
            if !(0..=1000).contains(&d) {
                return Err("Digits must be between 0 and 1000".into());
            }
            map_cells(block, |s| zero_fill(s, d as usize))
        }
        "remove_leading_zeros" => map_cells(block, remove_leading_zeros),
        "number_format" => {
            let to_comma = a.choice("to")? == NUM_COMMA;
            // Source notation: the one we're converting *from*.
            let re = if to_comma {
                Regex::new(r"^[+-]?(\d{1,3}(,\d{3})+|\d+)(\.\d+)?$")
            } else {
                Regex::new(r"^[+-]?(\d{1,3}(\.\d{3})+|\d+)(,\d+)?$")
            }
            .expect("valid regex");
            map_cells(block, |s| {
                let tr = s.trim();
                if re.is_match(tr) {
                    swap_separators(s)
                } else {
                    s.to_string()
                }
            })
        }
        "set_decimals" => {
            let d = a.int("decimals")?;
            if !(0..=20).contains(&d) {
                return Err("Decimals must be between 0 and 20".into());
            }
            map_cells(block, |s| match parse_number(s.trim()) {
                Some(v) => fix_neg_zero(format!("{:.*}", d as usize, v)),
                None => s.to_string(),
            })
        }
        "prepend" => {
            let text = a.text("text");
            map_cells(block, |s| format!("{text}{s}"))
        }
        "append" => {
            let text = a.text("text");
            map_cells(block, |s| format!("{s}{text}"))
        }
        "convert_datetime" => convert_datetime(&a, block)?,
        "fill_series" => fill_series(&a, block)?,
        "random_int" => random_int(&a, block)?,
        "random_float" => {
            let min = a.float("min")?;
            let max = a.float("max")?;
            let d = a.int("decimals")?;
            if !(0..=20).contains(&d) {
                return Err("Decimals must be between 0 and 20".into());
            }
            if min > max {
                return Err("Minimum must not exceed maximum".into());
            }
            let mut rng = rand::rng();
            fill_cells(block, |_, _| {
                let v: f64 = if min == max { min } else { rng.random_range(min..=max) };
                fix_neg_zero(format!("{:.*}", d as usize, v))
            })
        }
        "uuid" => {
            let v7 = a.choice("version")? == "v7";
            let fmt = a.choice("format")?;
            fill_cells(block, |_, _| {
                let u = if v7 { uuid::Uuid::now_v7() } else { uuid::Uuid::new_v4() };
                match fmt.as_str() {
                    "uppercase" => u.hyphenated().to_string().to_uppercase(),
                    "braced" => u.braced().to_string(),
                    _ => u.hyphenated().to_string(),
                }
            })
        }
        "timestamp_now" => {
            let now = chrono::Local::now();
            let s = match a.choice("format")?.as_str() {
                TS_UNIX_S => now.timestamp().to_string(),
                TS_UNIX_MS => now.timestamp_millis().to_string(),
                TS_CUSTOM => {
                    let f = a.text("custom");
                    check_strftime(&f)?;
                    let mut out = String::new();
                    use std::fmt::Write;
                    write!(out, "{}", now.format(&f)).map_err(|_| "Invalid custom format".to_string())?;
                    out
                }
                _ => now.format("%Y-%m-%dT%H:%M:%S%:z").to_string(),
            };
            map_cells(block, |_| s.clone())
        }
        "copy_main_cell" => {
            let main = block.first().and_then(|r| r.first()).cloned().unwrap_or_default();
            map_cells(block, |_| main.clone())
        }
        "transpose_selection" => transpose(block),
        "reshape_row_wise" => reshape(&a, block, false)?,
        "reshape_column_wise" => reshape(&a, block, true)?,
        "split_horizontal" => split(&a, block, false)?,
        "split_vertical" => split(&a, block, true)?,
        "join_horizontal" => {
            let sep = a.text("separator");
            TransformOutput::same_shape(block.iter().map(|row| vec![row.join(&sep)]).collect())
        }
        "join_vertical" => {
            let sep = a.text("separator");
            let w = block.first().map_or(0, |r| r.len());
            let row: Vec<String> = (0..w)
                .map(|c| block.iter().map(|r| r[c].as_str()).collect::<Vec<_>>().join(&sep))
                .collect();
            TransformOutput::same_shape(if block.is_empty() { vec![] } else { vec![row] })
        }
        _ => return Err(format!("Unknown transform: {id}")),
    })
}

// ---------------------------------------------------------------------------
// Text case helpers
// ---------------------------------------------------------------------------

fn title_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev: Option<char> = None;
    for ch in s.chars() {
        let start = prev.is_none_or(|p| !p.is_alphanumeric() && p != '\'' && p != '\u{2019}');
        if start {
            out.extend(ch.to_uppercase());
        } else {
            out.extend(ch.to_lowercase());
        }
        prev = Some(ch);
    }
    out
}

/// Split a phrase into words on whitespace, `_`, `-`, and case boundaries
/// ("helloWorld" → hello, World; "HTTPServer" → HTTP, Server).
fn split_words(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut words = Vec::new();
    let mut cur = String::new();
    for (i, &ch) in chars.iter().enumerate() {
        if ch.is_whitespace() || ch == '_' || ch == '-' {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if ch.is_uppercase() && i > 0 && !cur.is_empty() {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|c| c.is_lowercase());
            if prev.is_lowercase() || prev.is_numeric() || (prev.is_uppercase() && next_lower) {
                words.push(std::mem::take(&mut cur));
            }
        }
        cur.push(ch);
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
}

fn capitalize(w: &str) -> String {
    let mut it = w.chars();
    match it.next() {
        Some(f) => f.to_uppercase().chain(it.flat_map(char::to_lowercase)).collect(),
        None => String::new(),
    }
}

fn camel_case(s: &str, upper_first: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, w) in split_words(s).iter().enumerate() {
        if i == 0 && !upper_first {
            out.push_str(&w.to_lowercase());
        } else {
            out.push_str(&capitalize(w));
        }
    }
    out
}

fn underscore_case(s: &str, upper: bool) -> String {
    split_words(s)
        .iter()
        .map(|w| if upper { w.to_uppercase() } else { w.to_lowercase() })
        .collect::<Vec<_>>()
        .join("_")
}

fn acronym(s: &str) -> String {
    s.split(|c: char| c.is_whitespace() || c == '-' || c == '_')
        .filter_map(|w| w.chars().find(|c| c.is_alphanumeric()))
        .flat_map(char::to_uppercase)
        .collect()
}

// ---------------------------------------------------------------------------
// Character / number helpers
// ---------------------------------------------------------------------------

fn keep_left(s: &str, n: i64) -> String {
    if n >= 0 {
        s.chars().take(n as usize).collect()
    } else {
        let len = s.chars().count();
        s.chars().take(len.saturating_sub(n.unsigned_abs() as usize)).collect()
    }
}

fn keep_right(s: &str, n: i64) -> String {
    let len = s.chars().count();
    if n >= 0 {
        s.chars().skip(len.saturating_sub(n as usize)).collect()
    } else {
        s.chars().skip(n.unsigned_abs() as usize).collect()
    }
}

fn zero_fill(s: &str, digits: usize) -> String {
    let (sign, rest) = match s.as_bytes().first() {
        Some(b'+') | Some(b'-') => s.split_at(1),
        _ => ("", s),
    };
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return s.to_string();
    }
    format!("{sign}{rest:0>digits$}")
}

fn remove_leading_zeros(s: &str) -> String {
    let (sign, rest) = match s.as_bytes().first() {
        Some(b'+') | Some(b'-') => s.split_at(1),
        _ => ("", s),
    };
    if !rest.starts_with('0') {
        return s.to_string();
    }
    let stripped = rest.trim_start_matches('0');
    if stripped.is_empty() || stripped.starts_with('.') || stripped.starts_with(',') {
        format!("{sign}0{stripped}")
    } else {
        format!("{sign}{stripped}")
    }
}

fn swap_separators(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '.' => ',',
            ',' => '.',
            c => c,
        })
        .collect()
}

/// Parse a plain decimal number (optional sign, digits, '.', exponent). Rejects
/// "inf"/"nan" and anything with other characters.
fn parse_number(s: &str) -> Option<f64> {
    if s.is_empty()
        || !s.bytes().any(|b| b.is_ascii_digit())
        || !s.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.' | b'e' | b'E'))
    {
        return None;
    }
    s.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// "-0.00" → "0.00"
fn fix_neg_zero(s: String) -> String {
    if s.starts_with('-') && s[1..].bytes().all(|b| b == b'0' || b == b'.') {
        s[1..].to_string()
    } else {
        s
    }
}

/// Format with at most `max_dec` decimals, trimming trailing zeros.
fn fmt_trim(v: f64, max_dec: usize) -> String {
    let mut s = format!("{:.*}", max_dec, v);
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    fix_neg_zero(s)
}

// ---------------------------------------------------------------------------
// Date / time
// ---------------------------------------------------------------------------

fn check_strftime(f: &str) -> Result<(), String> {
    if f.is_empty() {
        return Err("Custom format must not be empty".into());
    }
    if StrftimeItems::new(f).any(|i| matches!(i, Item::Error)) {
        return Err(format!("Invalid date format: {f}"));
    }
    Ok(())
}

/// A parsed moment. Naive inputs are treated as UTC with `has_offset = false`.
struct Moment {
    dt: DateTime<FixedOffset>,
    has_offset: bool,
}

impl Moment {
    fn naive(n: NaiveDateTime) -> Self {
        Moment { dt: n.and_utc().fixed_offset(), has_offset: false }
    }
}

fn excel_epoch() -> NaiveDateTime {
    NaiveDate::from_ymd_opt(1899, 12, 30).unwrap().and_hms_opt(0, 0, 0).unwrap()
}

fn midnight(d: NaiveDate) -> NaiveDateTime {
    d.and_hms_opt(0, 0, 0).unwrap()
}

fn parse_moment(s: &str, from: &str, custom: &str) -> Option<Moment> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    match from {
        DT_CUSTOM => {
            if let Ok(dt) = DateTime::parse_from_str(s, custom) {
                return Some(Moment { dt, has_offset: true });
            }
            if let Ok(n) = NaiveDateTime::parse_from_str(s, custom) {
                return Some(Moment::naive(n));
            }
            NaiveDate::parse_from_str(s, custom).ok().map(|d| Moment::naive(midnight(d)))
        }
        DT_ISO_DT | DT_ISO_D => {
            if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
                return Some(Moment { dt, has_offset: true });
            }
            for f in ["%Y-%m-%dT%H:%M:%S%.f%z", "%Y-%m-%d %H:%M:%S%.f%z"] {
                if let Ok(dt) = DateTime::parse_from_str(s, f) {
                    return Some(Moment { dt, has_offset: true });
                }
            }
            for f in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M"] {
                if let Ok(n) = NaiveDateTime::parse_from_str(s, f) {
                    return Some(Moment::naive(n));
                }
            }
            NaiveDate::parse_from_str(s, "%Y-%m-%d").ok().map(|d| Moment::naive(midnight(d)))
        }
        DT_UNIX_S | DT_UNIX_MS => {
            let v = parse_number(s)?;
            let ms = if from == DT_UNIX_S { v * 1000.0 } else { v };
            if ms.abs() > 1e17 {
                return None;
            }
            DateTime::<Utc>::from_timestamp_millis(ms.round() as i64)
                .map(|d| Moment { dt: d.fixed_offset(), has_offset: false })
        }
        DT_EXCEL => {
            let v = parse_number(s)?;
            if v.abs() > 1e8 {
                return None;
            }
            let ms = (v * 86_400_000.0).round() as i64;
            excel_epoch().checked_add_signed(Duration::milliseconds(ms)).map(Moment::naive)
        }
        _ => None,
    }
}

fn format_moment(m: &Moment, to: &str, custom: &str) -> Option<String> {
    let local = m.dt.naive_local();
    Some(match to {
        DT_CUSTOM => {
            use std::fmt::Write;
            let mut out = String::new();
            write!(out, "{}", m.dt.format(custom)).ok()?;
            out
        }
        DT_ISO_DT => {
            let mut s = if m.dt.timestamp_subsec_nanos() != 0 {
                local.format("%Y-%m-%dT%H:%M:%S%.3f").to_string()
            } else {
                local.format("%Y-%m-%dT%H:%M:%S").to_string()
            };
            if m.has_offset {
                s.push_str(&m.dt.format("%:z").to_string());
            }
            s
        }
        DT_ISO_D => local.format("%Y-%m-%d").to_string(),
        DT_UNIX_S => {
            let ms = m.dt.timestamp_millis();
            if ms % 1000 == 0 { (ms / 1000).to_string() } else { fmt_trim(ms as f64 / 1000.0, 3) }
        }
        DT_UNIX_MS => m.dt.timestamp_millis().to_string(),
        DT_EXCEL => {
            let ms = (local - excel_epoch()).num_milliseconds();
            fmt_trim(ms as f64 / 86_400_000.0, 10)
        }
        _ => return None,
    })
}

fn convert_datetime(a: &Args, block: &[Vec<String>]) -> Result<TransformOutput, String> {
    let from = a.choice("from")?;
    let to = a.choice("to")?;
    let from_custom = a.text("from_custom");
    let to_custom = a.text("to_custom");
    if from == DT_CUSTOM {
        check_strftime(&from_custom)?;
    }
    if to == DT_CUSTOM {
        check_strftime(&to_custom)?;
    }
    Ok(map_cells(block, |s| {
        parse_moment(s, &from, &from_custom)
            .and_then(|m| format_moment(&m, &to, &to_custom))
            .unwrap_or_else(|| s.to_string())
    }))
}

// ---------------------------------------------------------------------------
// Generators
// ---------------------------------------------------------------------------

fn split_list(s: &str) -> Vec<String> {
    if s.trim().is_empty() {
        return vec![];
    }
    s.split(',').map(|w| w.trim().to_string()).collect()
}

fn decimals_in(s: &str) -> usize {
    let s = s.trim();
    if s.contains(['e', 'E']) {
        return 10;
    }
    s.split_once('.').map_or(0, |(_, f)| f.len())
}

fn fill_series(a: &Args, block: &[Vec<String>]) -> Result<TransformOutput, String> {
    let nrows = block.len();
    let words = split_list(&a.text("words"));
    if !words.is_empty() {
        return Ok(fill_cells(block, |r, c| words[(c * nrows + r) % words.len()].clone()));
    }
    let start = a.float("start")?;
    let step = a.float("step")?;
    let dec = decimals_in(&a.text("start")).max(decimals_in(&a.text("step"))).min(15);
    Ok(fill_cells(block, |r, c| {
        let v = start + (c * nrows + r) as f64 * step;
        fmt_trim(v, dec)
    }))
}

fn random_int(a: &Args, block: &[Vec<String>]) -> Result<TransformOutput, String> {
    let mut rng = rand::rng();
    let values = split_list(&a.text("values"));
    if !values.is_empty() {
        return Ok(fill_cells(block, |_, _| values[rng.random_range(0..values.len())].clone()));
    }
    let min = a.int("min")?;
    let max = a.int("max")?;
    if min > max {
        return Err("Minimum must not exceed maximum".into());
    }
    Ok(fill_cells(block, |_, _| rng.random_range(min..=max).to_string()))
}

// ---------------------------------------------------------------------------
// Shape-changing
// ---------------------------------------------------------------------------

fn dims(block: &[Vec<String>]) -> (usize, usize) {
    (block.len(), block.first().map_or(0, |r| r.len()))
}

fn transpose(block: &[Vec<String>]) -> TransformOutput {
    let (h, w) = dims(block);
    let cells: Vec<Vec<String>> = (0..w).map(|c| (0..h).map(|r| block[r][c].clone()).collect()).collect();
    TransformOutput { cells, insert_rows: w.saturating_sub(h), insert_cols: h.saturating_sub(w) }
}

fn reshape(a: &Args, block: &[Vec<String>], column_wise: bool) -> Result<TransformOutput, String> {
    let count = a.int("count")?;
    if count < 1 {
        return Err("Count must be at least 1".into());
    }
    let count = count as usize;
    let by_rows = a.choice("dimension")? == "rows";
    let (h, w) = dims(block);
    let flat: Vec<&String> = if column_wise {
        (0..w).flat_map(|c| (0..h).map(move |r| &block[r][c])).collect()
    } else {
        block.iter().flatten().collect()
    };
    let n = flat.len();
    if n == 0 {
        return Ok(TransformOutput::default());
    }
    let other = n.div_ceil(count);
    let (nh, nw) = if by_rows { (count, other) } else { (other, count) };
    let mut cells = vec![vec![String::new(); nw]; nh];
    for (i, v) in flat.into_iter().enumerate() {
        let (r, c) = if column_wise { (i % nh, i / nh) } else { (i / nw, i % nw) };
        cells[r][c] = v.clone();
    }
    Ok(TransformOutput { cells, insert_rows: nh.saturating_sub(h), insert_cols: nw.saturating_sub(w) })
}

enum Splitter {
    Delim(String),
    Index(i64),
}

impl Splitter {
    fn pieces<'s>(&self, s: &'s str) -> Vec<&'s str> {
        match self {
            Splitter::Delim(d) => s.split(d.as_str()).collect(),
            Splitter::Index(i) => {
                let len = s.chars().count() as i64;
                let at = if *i >= 0 { (*i).min(len) } else { (len + *i).max(0) } as usize;
                let byte = s.char_indices().nth(at).map_or(s.len(), |(b, _)| b);
                vec![&s[..byte], &s[byte..]]
            }
        }
    }
}

fn split(a: &Args, block: &[Vec<String>], vertical: bool) -> Result<TransformOutput, String> {
    let splitter = if a.choice("mode")? == "index" {
        Splitter::Index(a.int("index")?)
    } else {
        let d = a.text("delimiter");
        if d.is_empty() {
            return Err("Delimiter must not be empty".into());
        }
        Splitter::Delim(d)
    };
    let insert = a.boolean("insert")?;
    let (h, w) = dims(block);
    let pieces: Vec<Vec<Vec<&str>>> =
        block.iter().map(|row| row.iter().map(|c| splitter.pieces(c)).collect()).collect();

    if !vertical {
        let maxp: Vec<usize> = (0..w).map(|c| pieces.iter().map(|r| r[c].len()).max().unwrap_or(1).max(1)).collect();
        let out_w: usize = maxp.iter().sum();
        let cells = pieces
            .iter()
            .map(|row| {
                let mut out = Vec::with_capacity(out_w);
                for (c, ps) in row.iter().enumerate() {
                    out.extend((0..maxp[c]).map(|k| ps.get(k).map_or_else(String::new, |s| s.to_string())));
                }
                out
            })
            .collect();
        Ok(TransformOutput { cells, insert_cols: if insert { out_w - w } else { 0 }, insert_rows: 0 })
    } else {
        let mut cells = Vec::new();
        for row in &pieces {
            let maxp = row.iter().map(|p| p.len()).max().unwrap_or(1).max(1);
            for k in 0..maxp {
                cells.push(row.iter().map(|ps| ps.get(k).map_or_else(String::new, |s| s.to_string())).collect());
            }
        }
        let out_h = cells.len();
        Ok(TransformOutput { cells, insert_rows: if insert { out_h - h } else { 0 }, insert_cols: 0 })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn pv(pairs: &[(&str, &str)]) -> ParamValues {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }
    fn b(rows: &[&[&str]]) -> Vec<Vec<String>> {
        rows.iter().map(|r| r.iter().map(|s| s.to_string()).collect()).collect()
    }
    fn run(id: &str, params: &[(&str, &str)], rows: &[&[&str]]) -> TransformOutput {
        apply(id, &pv(params), &b(rows)).unwrap()
    }
    fn one(id: &str, params: &[(&str, &str)], cell: &str) -> String {
        run(id, params, &[&[cell]]).cells[0][0].clone()
    }

    #[test]
    fn catalogue_ids_unique_and_defaults_valid() {
        let all = all();
        let mut ids: Vec<_> = all.iter().map(|t| t.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), all.len());
        for t in &all {
            for p in &t.params {
                if let ParamKind::Choice(o) = &p.kind {
                    assert!(o.contains(&p.default), "{} {}", t.id, p.key);
                }
            }
            // every transform runs with defaults on a small block
            let blk = b(&[&["a b", "1"], &["", "x,y"]]);
            assert!(apply(t.id, &ParamValues::new(), &blk).is_ok(), "{}", t.id);
            assert!(apply(t.id, &ParamValues::new(), &[]).is_ok(), "{}", t.id);
        }
        assert!(apply("nope", &ParamValues::new(), &[]).is_err());
    }

    #[test]
    fn cases() {
        assert_eq!(one("upper_case", &[], "héllo ß"), "HÉLLO SS");
        assert_eq!(one("lower_case", &[], "HÉLLO"), "héllo");
        assert_eq!(one("title_case", &[], "hello wORLD don't-stop"), "Hello World Don't-Stop");
        assert_eq!(one("title_case", &[], ""), "");
        assert_eq!(one("upper_camel_case", &[], "hello world_foo-bar"), "HelloWorldFooBar");
        assert_eq!(one("lower_camel_case", &[], "Hello World"), "helloWorld");
        assert_eq!(one("lower_camel_case", &[], "HTTPServer error"), "httpServerError");
        assert_eq!(one("upper_underscore_case", &[], "helloWorld foo"), "HELLO_WORLD_FOO");
        assert_eq!(one("lower_underscore_case", &[], "  Hello  World-Again "), "hello_world_again");
        assert_eq!(one("lower_underscore_case", &[], "ÉtéChaud"), "été_chaud");
        assert_eq!(one("upper_camel_case", &[], ""), "");
    }

    #[test]
    fn trim_and_acronym() {
        assert_eq!(one("trim", &[], " \t a b \n"), "a b");
        assert_eq!(one("acronym", &[], "portable network graphics"), "PNG");
        assert_eq!(one("acronym", &[], "  as soon-as possible "), "ASAP");
        assert_eq!(one("acronym", &[], "été (chaud)"), "ÉC");
        assert_eq!(one("acronym", &[], ""), "");
    }

    #[test]
    fn keep() {
        assert_eq!(one("keep_left", &[("n", "2")], "héllo"), "hé");
        assert_eq!(one("keep_left", &[("n", "-2")], "héllo"), "hél");
        assert_eq!(one("keep_left", &[("n", "-10")], "abc"), "");
        assert_eq!(one("keep_left", &[("n", "10")], "abc"), "abc");
        assert_eq!(one("keep_right", &[("n", "2")], "héllo"), "lo");
        assert_eq!(one("keep_right", &[("n", "-1")], "héllo"), "éllo");
        assert_eq!(one("keep_right", &[("n", "9")], ""), "");
        assert_eq!(one("keep_middle", &[("start", "1"), ("n", "3")], "héllo"), "éll");
        assert_eq!(one("keep_middle", &[("start", "9"), ("n", "3")], "abc"), "");
        assert!(apply("keep_middle", &pv(&[("start", "-1")]), &b(&[&["a"]])).is_err());
        assert!(apply("keep_left", &pv(&[("n", "x")]), &b(&[&["a"]])).is_err());
    }

    #[test]
    fn zeros() {
        assert_eq!(one("zero_fill", &[("digits", "4")], "42"), "0042");
        assert_eq!(one("zero_fill", &[("digits", "4")], "-42"), "-0042");
        assert_eq!(one("zero_fill", &[("digits", "2")], "12345"), "12345");
        assert_eq!(one("zero_fill", &[("digits", "4")], "4.2"), "4.2");
        assert_eq!(one("zero_fill", &[("digits", "4")], ""), "");
        assert_eq!(one("zero_fill", &[("digits", "4")], "-"), "-");
        assert!(apply("zero_fill", &pv(&[("digits", "-1")]), &b(&[&["1"]])).is_err());
        assert_eq!(one("remove_leading_zeros", &[], "007"), "7");
        assert_eq!(one("remove_leading_zeros", &[], "000.5"), "0.5");
        assert_eq!(one("remove_leading_zeros", &[], "0"), "0");
        assert_eq!(one("remove_leading_zeros", &[], "000"), "0");
        assert_eq!(one("remove_leading_zeros", &[], "-007"), "-7");
        assert_eq!(one("remove_leading_zeros", &[], "100"), "100");
        assert_eq!(one("remove_leading_zeros", &[], ""), "");
    }

    #[test]
    fn number_formats() {
        let comma = [("to", NUM_COMMA)];
        let dot = [("to", NUM_DOT)];
        assert_eq!(one("number_format", &comma, "1,234.5"), "1.234,5");
        assert_eq!(one("number_format", &comma, "-1234567.25"), "-1234567,25");
        assert_eq!(one("number_format", &comma, "1,234,567"), "1.234.567");
        assert_eq!(one("number_format", &comma, "abc"), "abc");
        assert_eq!(one("number_format", &comma, "1,2,3"), "1,2,3");
        assert_eq!(one("number_format", &dot, "1.234,5"), "1,234.5");
        assert_eq!(one("number_format", &dot, "3,14"), "3.14");
        assert_eq!(one("number_format", &dot, "3.14"), "3.14");
        assert_eq!(one("number_format", &dot, ""), "");
        assert!(apply("number_format", &pv(&[("to", "bogus")]), &b(&[&["1"]])).is_err());
    }

    #[test]
    fn decimals() {
        assert_eq!(one("set_decimals", &[("decimals", "2")], "3.14159"), "3.14");
        assert_eq!(one("set_decimals", &[("decimals", "0")], "2.6"), "3");
        assert_eq!(one("set_decimals", &[("decimals", "3")], "-1"), "-1.000");
        assert_eq!(one("set_decimals", &[("decimals", "1")], "-0.01"), "0.0");
        assert_eq!(one("set_decimals", &[("decimals", "2")], "1e3"), "1000.00");
        assert_eq!(one("set_decimals", &[("decimals", "2")], "inf"), "inf");
        assert_eq!(one("set_decimals", &[("decimals", "2")], "abc"), "abc");
        assert_eq!(one("set_decimals", &[("decimals", "2")], ""), "");
        assert!(apply("set_decimals", &pv(&[("decimals", "-1")]), &b(&[&["1"]])).is_err());
    }

    #[test]
    fn prepend_append() {
        assert_eq!(one("prepend", &[("text", "→")], "x"), "→x");
        assert_eq!(one("append", &[("text", "!")], ""), "!");
        assert_eq!(one("append", &[], "x"), "x");
    }

    #[test]
    fn datetime() {
        let conv = |from: &str, fc: &str, to: &str, tc: &str, cell: &str| {
            one("convert_datetime", &[("from", from), ("from_custom", fc), ("to", to), ("to_custom", tc)], cell)
        };
        assert_eq!(conv(DT_CUSTOM, "%m/%d/%Y", DT_ISO_D, "", "03/15/2024"), "2024-03-15");
        assert_eq!(conv(DT_CUSTOM, "%m/%d/%Y", DT_ISO_D, "", "not a date"), "not a date");
        assert_eq!(conv(DT_CUSTOM, "%m/%d/%Y", DT_ISO_D, "", ""), "");
        assert_eq!(conv(DT_ISO_DT, "", DT_CUSTOM, "%d.%m.%Y %H:%M", "2024-03-15T13:45:00"), "15.03.2024 13:45");
        assert_eq!(conv(DT_ISO_DT, "", DT_ISO_DT, "", "2024-03-15 13:45:00"), "2024-03-15T13:45:00");
        assert_eq!(conv(DT_ISO_DT, "", DT_ISO_DT, "", "2024-03-15T13:45:00.250"), "2024-03-15T13:45:00.250");
        assert_eq!(conv(DT_ISO_DT, "", DT_ISO_DT, "", "2024-03-15T13:45:00+02:00"), "2024-03-15T13:45:00+02:00");
        assert_eq!(conv(DT_ISO_DT, "", DT_UNIX_S, "", "2024-03-15T13:45:00+02:00"), "1710503100");
        assert_eq!(conv(DT_ISO_DT, "", DT_UNIX_S, "", "1970-01-01T00:00:00Z"), "0");
        assert_eq!(conv(DT_ISO_D, "", DT_UNIX_MS, "", "1970-01-02"), "86400000");
        assert_eq!(conv(DT_UNIX_S, "", DT_ISO_DT, "", "86400"), "1970-01-02T00:00:00");
        assert_eq!(conv(DT_UNIX_S, "", DT_UNIX_MS, "", "1.5"), "1500");
        assert_eq!(conv(DT_UNIX_MS, "", DT_UNIX_S, "", "1500"), "1.5");
        assert_eq!(conv(DT_UNIX_MS, "", DT_ISO_D, "", "-86400000"), "1969-12-31");
        assert_eq!(conv(DT_EXCEL, "", DT_ISO_DT, "", "45366.5"), "2024-03-15T12:00:00");
        assert_eq!(conv(DT_EXCEL, "", DT_ISO_D, "", "1"), "1899-12-31");
        assert_eq!(conv(DT_ISO_D, "", DT_EXCEL, "", "2024-03-15"), "45366");
        assert_eq!(conv(DT_ISO_DT, "", DT_EXCEL, "", "2024-03-15T06:00:00"), "45366.25");
        assert_eq!(conv(DT_UNIX_S, "", DT_ISO_D, "", "abc"), "abc");
        assert_eq!(conv(DT_UNIX_S, "", DT_ISO_D, "", "1e30"), "1e30");
        // custom output with %z on a naive value must not panic
        assert_eq!(conv(DT_ISO_D, "", DT_CUSTOM, "%Y %z", "2024-01-02"), "2024 +0000");
        let bad = pv(&[("from", DT_CUSTOM), ("from_custom", "%Q")]);
        assert!(apply("convert_datetime", &bad, &b(&[&["x"]])).is_err());
        let bad = pv(&[("to", DT_CUSTOM), ("to_custom", "")]);
        assert!(apply("convert_datetime", &bad, &b(&[&["x"]])).is_err());
    }

    #[test]
    fn series() {
        let o = run("fill_series", &[("start", "1"), ("step", "1")], &[&["", ""], &["", ""], &["", ""]]);
        assert_eq!(o.cells, b(&[&["1", "4"], &["2", "5"], &["3", "6"]]));
        let o = run("fill_series", &[("start", "0.1"), ("step", "0.2")], &[&[""], &[""], &[""]]);
        assert_eq!(o.cells, b(&[&["0.1"], &["0.3"], &["0.5"]]));
        let o = run("fill_series", &[("start", "1"), ("step", "0.5")], &[&[""], &[""], &[""]]);
        assert_eq!(o.cells, b(&[&["1"], &["1.5"], &["2"]]));
        let o = run("fill_series", &[("start", "10"), ("step", "-5")], &[&[""], &[""], &[""]]);
        assert_eq!(o.cells, b(&[&["10"], &["5"], &["0"]]));
        let o = run("fill_series", &[("words", "Mon, Tue")], &[&["", ""], &["", ""]]);
        assert_eq!(o.cells, b(&[&["Mon", "Mon"], &["Tue", "Tue"]]));
        assert!(apply("fill_series", &pv(&[("step", "x")]), &b(&[&[""]])).is_err());
    }

    #[test]
    fn randoms() {
        let blk = vec![vec![String::new(); 10]; 10];
        let o = apply("random_int", &pv(&[("min", "-3"), ("max", "3")]), &blk).unwrap();
        for v in o.cells.iter().flatten() {
            let n: i64 = v.parse().unwrap();
            assert!((-3..=3).contains(&n));
        }
        let o = apply("random_int", &pv(&[("values", "a, b,c")]), &blk).unwrap();
        assert!(o.cells.iter().flatten().all(|v| ["a", "b", "c"].contains(&v.as_str())));
        assert!(apply("random_int", &pv(&[("min", "5"), ("max", "1")]), &blk).is_err());
        let o = apply("random_float", &pv(&[("min", "1"), ("max", "2"), ("decimals", "3")]), &blk).unwrap();
        for v in o.cells.iter().flatten() {
            assert_eq!(v.split('.').nth(1).unwrap().len(), 3);
            let f: f64 = v.parse().unwrap();
            assert!((1.0..=2.0).contains(&f));
        }
        assert_eq!(one("random_float", &[("min", "2"), ("max", "2"), ("decimals", "0")], ""), "2");
        assert!(apply("random_float", &pv(&[("min", "3"), ("max", "1")]), &blk).is_err());
    }

    #[test]
    fn uuids() {
        let s = one("uuid", &[], "");
        assert_eq!(s.len(), 36);
        assert_eq!(uuid::Uuid::parse_str(&s).unwrap().get_version_num(), 4);
        let s = one("uuid", &[("version", "v7"), ("format", "uppercase")], "");
        assert_eq!(s, s.to_uppercase());
        assert_eq!(uuid::Uuid::parse_str(&s).unwrap().get_version_num(), 7);
        let s = one("uuid", &[("format", "braced")], "");
        assert!(s.starts_with('{') && s.ends_with('}') && s.len() == 38);
        let o = run("uuid", &[], &[&["", ""]]);
        assert_ne!(o.cells[0][0], o.cells[0][1]);
        assert!(apply("uuid", &pv(&[("version", "v9")]), &b(&[&[""]])).is_err());
    }

    #[test]
    fn timestamps() {
        let s = one("timestamp_now", &[("format", TS_UNIX_S)], "");
        assert!(s.parse::<i64>().unwrap() > 1_700_000_000);
        let s = one("timestamp_now", &[("format", TS_UNIX_MS)], "");
        assert!(s.parse::<i64>().unwrap() > 1_700_000_000_000);
        let s = one("timestamp_now", &[], "");
        assert!(DateTime::parse_from_rfc3339(&s).is_ok(), "{s}");
        let s = one("timestamp_now", &[("format", TS_CUSTOM), ("custom", "%Y")], "");
        assert_eq!(s.len(), 4);
        assert!(apply("timestamp_now", &pv(&[("format", TS_CUSTOM), ("custom", "%Q")]), &b(&[&[""]])).is_err());
    }

    #[test]
    fn copy_main() {
        let o = run("copy_main_cell", &[], &[&["x", "y"], &["", "z"]]);
        assert_eq!(o.cells, b(&[&["x", "x"], &["x", "x"]]));
    }

    #[test]
    fn transpose_shapes() {
        let o = run("transpose_selection", &[], &[&["a", "b", "c"]]);
        assert_eq!(o.cells, b(&[&["a"], &["b"], &["c"]]));
        assert_eq!((o.insert_rows, o.insert_cols), (2, 0));
        let o = run("transpose_selection", &[], &[&["a"], &["b"]]);
        assert_eq!(o.cells, b(&[&["a", "b"]]));
        assert_eq!((o.insert_rows, o.insert_cols), (0, 1));
        let o = run("transpose_selection", &[], &[&["a", "b"], &["c", "d"]]);
        assert_eq!(o.cells, b(&[&["a", "c"], &["b", "d"]]));
        assert_eq!((o.insert_rows, o.insert_cols), (0, 0));
    }

    #[test]
    fn reshapes() {
        let blk: &[&[&str]] = &[&["1", "2", "3"], &["4", "5", "6"]];
        let o = run("reshape_row_wise", &[("count", "2"), ("dimension", "columns")], blk);
        assert_eq!(o.cells, b(&[&["1", "2"], &["3", "4"], &["5", "6"]]));
        assert_eq!((o.insert_rows, o.insert_cols), (1, 0));
        let o = run("reshape_row_wise", &[("count", "1"), ("dimension", "rows")], blk);
        assert_eq!(o.cells, b(&[&["1", "2", "3", "4", "5", "6"]]));
        assert_eq!((o.insert_rows, o.insert_cols), (0, 3));
        let o = run("reshape_column_wise", &[("count", "3"), ("dimension", "rows")], blk);
        // column-wise flatten: 1 4 2 5 3 6, refilled down columns of 3
        assert_eq!(o.cells, b(&[&["1", "5"], &["4", "3"], &["2", "6"]]));
        assert_eq!((o.insert_rows, o.insert_cols), (1, 0));
        let o = run("reshape_row_wise", &[("count", "4"), ("dimension", "columns")], blk);
        assert_eq!(o.cells, b(&[&["1", "2", "3", "4"], &["5", "6", "", ""]]));
        assert_eq!((o.insert_rows, o.insert_cols), (0, 1));
        assert!(apply("reshape_row_wise", &pv(&[("count", "0")]), &b(blk)).is_err());
    }

    #[test]
    fn split_h() {
        let o = run("split_horizontal", &[("delimiter", ", ")], &[&["a, b, c", "x"], &["d", "y, z"]]);
        assert_eq!(o.cells, b(&[&["a", "b", "c", "x", ""], &["d", "", "", "y", "z"]]));
        assert_eq!(o.insert_cols, 3);
        let o = run("split_horizontal", &[("insert", "false")], &[&["a,b"]]);
        assert_eq!(o.cells, b(&[&["a", "b"]]));
        assert_eq!(o.insert_cols, 0);
        let o = run("split_horizontal", &[("mode", "index"), ("index", "2")], &[&["héllo"], &[""]]);
        assert_eq!(o.cells, b(&[&["hé", "llo"], &["", ""]]));
        let o = run("split_horizontal", &[("mode", "index"), ("index", "-1")], &[&["abc"]]);
        assert_eq!(o.cells, b(&[&["ab", "c"]]));
        let o = run("split_horizontal", &[], &[&[""]]);
        assert_eq!(o.cells, b(&[&[""]]));
        assert_eq!(o.insert_cols, 0);
        assert!(apply("split_horizontal", &pv(&[("delimiter", "")]), &b(&[&["a"]])).is_err());
        assert!(apply("split_horizontal", &pv(&[("insert", "maybe")]), &b(&[&["a"]])).is_err());
    }

    #[test]
    fn split_v() {
        let o = run("split_vertical", &[], &[&["a,b", "x"], &["c", "y"]]);
        assert_eq!(o.cells, b(&[&["a", "x"], &["b", ""], &["c", "y"]]));
        assert_eq!(o.insert_rows, 1);
        let o = run("split_vertical", &[("insert", "false")], &[&["a,b,c"]]);
        assert_eq!(o.cells.len(), 3);
        assert_eq!(o.insert_rows, 0);
    }

    #[test]
    fn joins() {
        let o = run("join_horizontal", &[("separator", "-")], &[&["a", "", "c"], &["d", "e", "f"]]);
        assert_eq!(o.cells, b(&[&["a--c"], &["d-e-f"]]));
        let o = run("join_vertical", &[("separator", " ")], &[&["a", "b"], &["c", "d"]]);
        assert_eq!(o.cells, b(&[&["a c", "b d"]]));
        let o = run("join_vertical", &[], &[]);
        assert!(o.cells.is_empty());
    }

    #[test]
    fn large_block_is_fast() {
        let blk = vec![vec!["Hello World 1,234.5".to_string(); 100]; 2_000];
        let t0 = std::time::Instant::now();
        for id in ["upper_camel_case", "number_format", "set_decimals"] {
            apply(id, &ParamValues::new(), &blk).unwrap();
        }
        let blk = vec![vec!["2024-03-15T13:45:00".to_string(); 100]; 2_000];
        apply("convert_datetime", &ParamValues::new(), &blk).unwrap();
        assert!(t0.elapsed().as_secs() < 30);
    }
}
