//! JSON / XML import & export and a hand-written PDF (print) exporter.
//!
//! * Import: `import_file` turns a `.json` / `.xml` document into a `VecTable`, either by
//!   detecting the repeating record pattern (default) or losslessly as one row per leaf
//!   value (`preserve_structure`).
//! * Export: `export_file` streams a table out as JSON or XML; tables shaped like a
//!   lossless import (`path,value[,type]`) are reconstructed into the original document.
//! * PDF: `export_pdf` lays a table out into pages with a minimal PDF 1.4 writer using
//!   the standard Helvetica fonts (no font embedding, WinAnsiEncoding).

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use serde_json::Value;

use crate::table::{TableRead, VecTable, col_letter};

// ===========================================================================
// Shared helpers
// ===========================================================================

/// Accumulates rows whose columns are discovered on the fly (union of keys, first-seen order).
#[derive(Default)]
struct Collector {
    cols: Vec<String>,
    idx: HashMap<String, usize>,
    rows: Vec<Vec<String>>,
}

impl Collector {
    fn col(&mut self, name: &str) -> usize {
        if let Some(&i) = self.idx.get(name) {
            return i;
        }
        let i = self.cols.len();
        self.cols.push(name.to_string());
        self.idx.insert(name.to_string(), i);
        i
    }

    /// Set a value in `row`; repeated columns within one record are joined with "; ".
    fn set(&mut self, row: &mut Vec<String>, name: &str, val: &str) {
        let i = self.col(name);
        if row.len() <= i {
            row.resize(i + 1, String::new());
        }
        let cell = &mut row[i];
        if cell.is_empty() {
            cell.push_str(val);
        } else if !val.is_empty() {
            cell.push_str("; ");
            cell.push_str(val);
        }
    }

    fn finish(mut self) -> VecTable {
        let n = self.cols.len();
        for r in &mut self.rows {
            r.resize(n, String::new());
        }
        VecTable { header: Some(self.cols), rows: self.rows }
    }
}

fn join_key(prefix: &str, k: &str) -> String {
    if prefix.is_empty() { k.to_string() } else { format!("{prefix}.{k}") }
}

fn create(path: &Path) -> Result<BufWriter<File>, String> {
    File::create(path)
        .map(|f| BufWriter::with_capacity(1 << 16, f))
        .map_err(|e| format!("Cannot create {}: {e}", path.display()))
}

fn io_err(e: std::io::Error) -> String {
    format!("Write error: {e}")
}

fn index_list(sub: Option<&[usize]>, n: usize) -> Vec<usize> {
    match sub {
        Some(s) => s.iter().copied().filter(|&i| i < n).collect(),
        None => (0..n).collect(),
    }
}

// ===========================================================================
// Import
// ===========================================================================

/// Parse a .json or .xml file (decided by extension, falling back to sniffing the first
/// non-space char) into a table. See module docs for the two modes.
pub fn import_file(path: &Path, preserve_structure: bool) -> Result<VecTable, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    let is_xml = match ext.as_str() {
        "xml" => true,
        "json" => false,
        _ => match text.trim_start().chars().next() {
            Some('<') => true,
            Some('{') | Some('[') => false,
            _ => return Err("Unrecognised file: expected JSON or XML".into()),
        },
    };
    if is_xml {
        let root = parse_xml(text)?;
        Ok(if preserve_structure { xml_to_paths(&root) } else { xml_to_records(&root) })
    } else {
        let v: Value = serde_json::from_str(text).map_err(|e| format!("Invalid JSON: {e}"))?;
        Ok(if preserve_structure { json_to_paths(&v) } else { json_to_records(&v) })
    }
}

// ---------------------------------------------------------------------------
// JSON import
// ---------------------------------------------------------------------------

fn scalar_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn is_scalar(v: &Value) -> bool {
    !matches!(v, Value::Array(_) | Value::Object(_))
}

fn flatten_json(v: &Value, prefix: &str, c: &mut Collector, row: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            if m.is_empty() && !prefix.is_empty() {
                c.set(row, prefix, "");
            }
            for (k, x) in m {
                flatten_json(x, &join_key(prefix, k), c, row);
            }
        }
        Value::Array(a) if a.iter().all(is_scalar) => {
            let joined = a.iter().map(scalar_str).collect::<Vec<_>>().join("; ");
            c.set(row, if prefix.is_empty() { "value" } else { prefix }, &joined);
        }
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                flatten_json(x, &join_key(prefix, &i.to_string()), c, row);
            }
        }
        _ => c.set(row, if prefix.is_empty() { "value" } else { prefix }, &scalar_str(v)),
    }
}

/// Find the array with the most object elements (pre-order, first wins ties).
fn largest_object_array(v: &Value) -> Option<&Vec<Value>> {
    fn walk<'a>(v: &'a Value, best: &mut Option<(usize, &'a Vec<Value>)>) {
        match v {
            Value::Array(a) => {
                let n = a.iter().filter(|x| x.is_object()).count();
                if n > 0 && best.is_none_or(|(b, _)| n > b) {
                    *best = Some((n, a));
                }
                a.iter().for_each(|x| walk(x, best));
            }
            Value::Object(m) => m.values().for_each(|x| walk(x, best)),
            _ => {}
        }
    }
    let mut best = None;
    walk(v, &mut best);
    best.map(|(_, a)| a)
}

fn json_to_records(v: &Value) -> VecTable {
    let mut c = Collector::default();
    let items: Vec<&Value> = match largest_object_array(v) {
        Some(a) => a.iter().collect(),
        None => match v {
            Value::Array(a) => a.iter().collect(),
            _ => vec![v],
        },
    };
    for item in items {
        let mut row = Vec::new();
        flatten_json(item, "", &mut c, &mut row);
        c.rows.push(row);
    }
    c.finish()
}

fn pointer_escape(k: &str) -> String {
    k.replace('~', "~0").replace('/', "~1")
}

fn pointer_unescape(k: &str) -> String {
    k.replace("~1", "/").replace("~0", "~")
}

fn json_to_paths(v: &Value) -> VecTable {
    fn walk(v: &Value, path: &mut String, rows: &mut Vec<Vec<String>>) {
        let leaf = |val: String, ty: &str| vec![path.clone(), val, ty.to_string()];
        match v {
            Value::Object(m) if !m.is_empty() => {
                for (k, x) in m {
                    let len = path.len();
                    path.push('/');
                    path.push_str(&pointer_escape(k));
                    walk(x, path, rows);
                    path.truncate(len);
                }
            }
            Value::Array(a) if !a.is_empty() => {
                for (i, x) in a.iter().enumerate() {
                    let len = path.len();
                    path.push('/');
                    path.push_str(&i.to_string());
                    walk(x, path, rows);
                    path.truncate(len);
                }
            }
            Value::Object(_) => rows.push(leaf("{}".into(), "object")),
            Value::Array(_) => rows.push(leaf("[]".into(), "array")),
            Value::String(s) => rows.push(leaf(s.clone(), "string")),
            Value::Number(n) => rows.push(leaf(n.to_string(), "number")),
            Value::Bool(b) => rows.push(leaf(b.to_string(), "bool")),
            Value::Null => rows.push(leaf("null".into(), "null")),
        }
    }
    let mut rows = Vec::new();
    walk(v, &mut String::new(), &mut rows);
    VecTable { header: Some(vec!["path".into(), "value".into(), "type".into()]), rows }
}

// ---------------------------------------------------------------------------
// XML DOM + import
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, PartialEq)]
struct XNode {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<XChild>,
}

#[derive(Debug, Clone, PartialEq)]
enum XChild {
    Elem(XNode),
    Text(String),
}

impl XNode {
    fn elems(&self) -> impl Iterator<Item = &XNode> {
        self.children.iter().filter_map(|c| match c {
            XChild::Elem(e) => Some(e),
            _ => None,
        })
    }
    fn text(&self) -> String {
        let parts: Vec<&str> = self
            .children
            .iter()
            .filter_map(|c| match c {
                XChild::Text(t) => Some(t.trim()),
                _ => None,
            })
            .filter(|t| !t.is_empty())
            .collect();
        parts.join(" ")
    }
    fn is_leaf(&self) -> bool {
        self.attrs.is_empty() && self.elems().next().is_none()
    }
    fn push_text(&mut self, t: &str) {
        if let Some(XChild::Text(last)) = self.children.last_mut() {
            last.push_str(t);
        } else {
            self.children.push(XChild::Text(t.to_string()));
        }
    }
}

/// Parse an XML document into a small DOM. Whitespace-only text nodes are dropped;
/// comments, PIs and the DOCTYPE are ignored.
fn parse_xml(text: &str) -> Result<XNode, String> {
    use quick_xml::events::{BytesStart, Event};
    use quick_xml::{XmlVersion, escape::resolve_predefined_entity};

    fn start(e: &BytesStart) -> Result<XNode, String> {
        let mut n = XNode { name: e.name().into_inner().to_string(), ..Default::default() };
        for a in e.attributes() {
            let a = a.map_err(|e| format!("Invalid XML attribute: {e}"))?;
            let v = a.normalized_value(XmlVersion::Implicit1_0).map_err(|e| format!("Invalid XML: {e}"))?;
            n.attrs.push((a.key.into_inner().to_string(), v.into_owned()));
        }
        Ok(n)
    }
    fn close(mut n: XNode, stack: &mut [XNode], root: &mut Option<XNode>) -> Result<(), String> {
        n.children.retain(|c| !matches!(c, XChild::Text(t) if t.trim().is_empty()));
        match stack.last_mut() {
            Some(p) => p.children.push(XChild::Elem(n)),
            None => {
                if root.is_some() {
                    return Err("Invalid XML: multiple root elements".into());
                }
                *root = Some(n);
            }
        }
        Ok(())
    }

    let mut reader = quick_xml::Reader::from_str(text);
    let mut stack: Vec<XNode> = Vec::new();
    let mut root: Option<XNode> = None;
    loop {
        let ev = reader
            .read_event()
            .map_err(|e| format!("Invalid XML at byte {}: {e}", reader.buffer_position()))?;
        match ev {
            Event::Start(e) => stack.push(start(&e)?),
            Event::Empty(e) => {
                let n = start(&e)?;
                close(n, &mut stack, &mut root)?;
            }
            Event::End(_) => {
                let n = stack.pop().ok_or("Invalid XML: unexpected end tag")?;
                close(n, &mut stack, &mut root)?;
            }
            Event::Text(t) => {
                if let Some(p) = stack.last_mut() {
                    p.push_text(&t.xml10_content());
                }
            }
            Event::CData(t) => {
                if let Some(p) = stack.last_mut() {
                    p.push_text(&t.xml10_content());
                }
            }
            Event::GeneralRef(r) => {
                if let Some(p) = stack.last_mut() {
                    let s: String = match r.resolve_char_ref() {
                        Ok(Some(ch)) => ch.to_string(),
                        _ => {
                            let name = r.xml10_content();
                            match resolve_predefined_entity(&name) {
                                Some(s) => s.to_string(),
                                None => format!("&{name};"),
                            }
                        }
                    };
                    p.push_text(&s);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err("Invalid XML: unexpected end of document".into());
    }
    root.ok_or_else(|| "Invalid XML: no root element".into())
}

fn flatten_xml(n: &XNode, prefix: &str, c: &mut Collector, row: &mut Vec<String>) {
    for (k, v) in &n.attrs {
        c.set(row, &join_key(prefix, &format!("@{k}")), v);
    }
    let has_children = n.elems().next().is_some();
    let text = n.text();
    if (has_children || !n.attrs.is_empty()) && !text.is_empty() {
        c.set(row, &join_key(prefix, "#text"), &text);
    }
    for ch in n.elems() {
        let key = join_key(prefix, &ch.name);
        if ch.is_leaf() {
            c.set(row, &key, &ch.text());
        } else {
            flatten_xml(ch, &key, c, row);
        }
    }
}

fn xml_to_records(root: &XNode) -> VecTable {
    // For every element name-path, the largest number of same-named siblings under one parent.
    let mut stats: HashMap<String, (usize, usize, usize)> = HashMap::new(); // path -> (max, depth, order)
    fn walk(n: &XNode, path: &str, depth: usize, stats: &mut HashMap<String, (usize, usize, usize)>) {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for ch in n.elems() {
            *counts.entry(&ch.name).or_default() += 1;
        }
        for ch in n.elems() {
            let p = format!("{path}/{}", ch.name);
            let k = counts[ch.name.as_str()];
            let order = stats.len();
            let e = stats.entry(p.clone()).or_insert((0, depth + 1, order));
            e.0 = e.0.max(k);
            walk(ch, &p, depth + 1, stats);
        }
    }
    let root_path = format!("/{}", root.name);
    walk(root, &root_path, 0, &mut stats);
    let best = stats
        .iter()
        .filter(|(_, s)| s.0 > 1)
        .min_by_key(|(_, s)| (std::cmp::Reverse(s.0), s.1, s.2))
        .map(|(p, _)| p.clone());

    let mut items: Vec<&XNode> = Vec::new();
    match best {
        Some(best) => {
            fn collect<'a>(n: &'a XNode, path: &str, target: &str, out: &mut Vec<&'a XNode>) {
                for ch in n.elems() {
                    let p = format!("{path}/{}", ch.name);
                    if p == target {
                        out.push(ch);
                    } else if target.starts_with(&format!("{p}/")) {
                        collect(ch, &p, target, out);
                    }
                }
            }
            collect(root, &root_path, &best, &mut items);
        }
        None => {
            // No repetition: descend through single-child wrappers to the record element.
            let mut n = root;
            loop {
                let mut it = n.elems();
                match (it.next(), it.next()) {
                    (Some(only), None) if n.attrs.is_empty() && n.text().is_empty() && !only.is_leaf() => n = only,
                    _ => break,
                }
            }
            items.push(n);
        }
    }

    let mut c = Collector::default();
    for item in items {
        let mut row = Vec::new();
        if item.is_leaf() {
            c.set(&mut row, &item.name, &item.text());
        } else {
            flatten_xml(item, "", &mut c, &mut row);
        }
        c.rows.push(row);
    }
    c.finish()
}

/// Lossless XML: one row per attribute, text node and empty element.
/// Paths look like `/root/item[2]/@id`, `/root/item[2]/name/text()`, `/p/text()[2]`.
/// Same-named siblings get a 1-based `[n]` index (omitted when the name is unique).
fn xml_to_paths(root: &XNode) -> VecTable {
    fn walk(n: &XNode, path: &str, rows: &mut Vec<Vec<String>>) {
        for (k, v) in &n.attrs {
            rows.push(vec![format!("{path}/@{k}"), v.clone()]);
        }
        if n.children.is_empty() && n.attrs.is_empty() {
            rows.push(vec![path.to_string(), String::new()]);
            return;
        }
        let mut totals: HashMap<&str, usize> = HashMap::new();
        for ch in n.elems() {
            *totals.entry(&ch.name).or_default() += 1;
        }
        let mut seen: HashMap<&str, usize> = HashMap::new();
        let mut texts = 0;
        for ch in &n.children {
            match ch {
                XChild::Text(t) => {
                    texts += 1;
                    let p = if texts == 1 { format!("{path}/text()") } else { format!("{path}/text()[{texts}]") };
                    rows.push(vec![p, t.clone()]);
                }
                XChild::Elem(e) => {
                    let i = seen.entry(&e.name).or_default();
                    *i += 1;
                    let p = if totals[e.name.as_str()] > 1 {
                        format!("{path}/{}[{}]", e.name, i)
                    } else {
                        format!("{path}/{}", e.name)
                    };
                    walk(e, &p, rows);
                }
            }
        }
    }
    let mut rows = Vec::new();
    walk(root, &format!("/{}", root.name), &mut rows);
    VecTable { header: Some(vec!["path".into(), "value".into()]), rows }
}

// ===========================================================================
// Export
// ===========================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Json,
    Xml,
}

/// Export the table (or a subset of rows/cols) as JSON or XML. See module docs.
pub fn export_file(
    table: &dyn TableRead,
    path: &Path,
    format: ExportFormat,
    typed: bool,
    rows: Option<&[usize]>,
    cols: Option<&[usize]>,
) -> Result<(), String> {
    let rows = index_list(rows, table.nrows());
    let cols = index_list(cols, table.ncols());

    if table.has_header() {
        let names: Vec<String> = cols.iter().map(|&c| table.column_name(c).trim().to_ascii_lowercase()).collect();
        let n: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        if n == ["path", "value"] || n == ["path", "value", "type"] {
            let pc = cols[0];
            let vc = cols[1];
            let tc = cols.get(2).copied();
            return match format {
                ExportFormat::Json => export_json_structured(table, path, &rows, pc, vc, tc, typed),
                ExportFormat::Xml => export_xml_structured(table, path, &rows, pc, vc),
            };
        }
    }

    let mut w = create(path)?;
    match format {
        ExportFormat::Json => write_json_records(table, &mut w, &rows, &cols, typed).map_err(io_err)?,
        ExportFormat::Xml => write_xml_records(table, &mut w, &rows, &cols).map_err(io_err)?,
    }
    w.flush().map_err(io_err)
}

/// Column keys, deduplicated by suffixing `_2`, `_3`, ...
fn unique_names(table: &dyn TableRead, cols: &[usize], f: impl Fn(String) -> String) -> Vec<String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut out = Vec::with_capacity(cols.len());
    for &c in cols {
        let base = f(table.column_name(c));
        let mut name = base.clone();
        let mut k = 1;
        while seen.contains_key(&name) {
            k += 1;
            name = format!("{base}_{k}");
        }
        seen.insert(name.clone(), 1);
        out.push(name);
    }
    out
}

/// Typed JSON conversion of a cell: empty/null -> null, true/false -> bool, JSON numbers stay numbers.
fn typed_json(s: &str) -> Option<&str> {
    match s {
        "" | "null" => Some("null"),
        "true" | "false" => Some(s),
        _ if s.trim() == s && serde_json::from_str::<serde_json::Number>(s).is_ok() => Some(s),
        _ => None,
    }
}

fn write_json_records(
    table: &dyn TableRead,
    w: &mut impl Write,
    rows: &[usize],
    cols: &[usize],
    typed: bool,
) -> std::io::Result<()> {
    let keys: Vec<String> = unique_names(table, cols, |s| s)
        .into_iter()
        .map(|k| serde_json::to_string(&k).unwrap_or_default())
        .collect();
    if rows.is_empty() {
        return w.write_all(b"[]\n");
    }
    w.write_all(b"[\n")?;
    for (ri, &r) in rows.iter().enumerate() {
        if cols.is_empty() {
            w.write_all(b"  {}")?;
        } else {
            w.write_all(b"  {\n")?;
            for (ci, &c) in cols.iter().enumerate() {
                let v = table.cell(r, c);
                write!(w, "    {}: ", keys[ci])?;
                match typed.then(|| typed_json(&v)).flatten() {
                    Some(lit) => w.write_all(lit.as_bytes())?,
                    None => serde_json::to_writer(&mut *w, &v).map_err(std::io::Error::other)?,
                }
                w.write_all(if ci + 1 < cols.len() { b",\n" } else { b"\n" })?;
            }
            w.write_all(b"  }")?;
        }
        w.write_all(if ri + 1 < rows.len() { b",\n" } else { b"\n" })?;
    }
    w.write_all(b"]\n")
}

/// Sanitise an XML element name: invalid chars -> '_', leading non-letter -> '_' prefix.
fn xml_name(s: &str, allow_colon: bool) -> String {
    let mut out: String = s
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' || c == '-' || c == '.' || (allow_colon && c == ':') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if !out.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_') {
        out.insert(0, '_');
    }
    out
}

fn xml_escape(s: &str, attr: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attr => out.push_str("&quot;"),
            '\n' if attr => out.push_str("&#10;"),
            '\t' if attr => out.push_str("&#9;"),
            '\r' => out.push_str("&#13;"),
            '\t' | '\n' => out.push(c),
            c if (c as u32) < 0x20 || c == '\u{fffe}' || c == '\u{ffff}' => {}
            c => out.push(c),
        }
    }
    out
}

fn write_xml_records(table: &dyn TableRead, w: &mut impl Write, rows: &[usize], cols: &[usize]) -> std::io::Result<()> {
    let names = unique_names(table, cols, |s| xml_name(&s, false));
    w.write_all(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<rows>\n")?;
    for &r in rows {
        w.write_all(b"  <row>\n")?;
        for (ci, &c) in cols.iter().enumerate() {
            let v = table.cell(r, c);
            let n = &names[ci];
            if v.is_empty() {
                writeln!(w, "    <{n}/>")?;
            } else {
                writeln!(w, "    <{n}>{}</{n}>", xml_escape(&v, false))?;
            }
        }
        w.write_all(b"  </row>\n")?;
    }
    w.write_all(b"</rows>\n")
}

// ---------------------------------------------------------------------------
// Structured (lossless) reconstruction
// ---------------------------------------------------------------------------

/// Ordered tree used to rebuild JSON from pointer rows.
enum PNode {
    Leaf(Value),
    Branch { keys: Vec<String>, kids: Vec<PNode>, index: HashMap<String, usize> },
}

impl PNode {
    fn branch() -> Self {
        PNode::Branch { keys: Vec::new(), kids: Vec::new(), index: HashMap::new() }
    }

    fn insert(&mut self, segs: &[String], leaf: Value) {
        let Some((first, rest)) = segs.split_first() else {
            // A leaf at a position that already has children keeps the children
            // (e.g. an "{}" row next to real members), unless it is a real value.
            if !(matches!(self, PNode::Branch { .. }) && is_empty_container(&leaf)) {
                *self = PNode::Leaf(leaf);
            }
            return;
        };
        if !matches!(self, PNode::Branch { .. }) {
            *self = PNode::branch();
        }
        let PNode::Branch { keys, kids, index } = self else { unreachable!() };
        let i = match index.get(first) {
            Some(&i) => i,
            None => {
                keys.push(first.clone());
                kids.push(PNode::Leaf(Value::Null));
                index.insert(first.clone(), kids.len() - 1);
                kids.len() - 1
            }
        };
        if !rest.is_empty() && matches!(kids[i], PNode::Leaf(_)) {
            kids[i] = PNode::branch();
        }
        kids[i].insert(rest, leaf);
    }

    fn into_value(self) -> Value {
        match self {
            PNode::Leaf(v) => v,
            PNode::Branch { keys, kids, .. } => {
                let idx: Option<Vec<usize>> = keys
                    .iter()
                    .map(|k| {
                        if k == "0" || (!k.starts_with('0') && !k.is_empty() && k.bytes().all(|b| b.is_ascii_digit())) {
                            k.parse::<usize>().ok()
                        } else {
                            None
                        }
                    })
                    .collect();
                if let Some(idx) = idx {
                    let mut sorted = idx.clone();
                    sorted.sort_unstable();
                    if sorted.iter().enumerate().all(|(i, &k)| i == k) {
                        let mut slots: Vec<Option<Value>> = (0..kids.len()).map(|_| None).collect();
                        for (i, kid) in idx.into_iter().zip(kids) {
                            slots[i] = Some(kid.into_value());
                        }
                        return Value::Array(slots.into_iter().map(|v| v.unwrap_or(Value::Null)).collect());
                    }
                }
                Value::Object(keys.into_iter().zip(kids).map(|(k, v)| (k, v.into_value())).collect())
            }
        }
    }
}

fn is_empty_container(v: &Value) -> bool {
    matches!(v, Value::Object(m) if m.is_empty()) || matches!(v, Value::Array(a) if a.is_empty())
}

fn export_json_structured(
    table: &dyn TableRead,
    path: &Path,
    rows: &[usize],
    pc: usize,
    vc: usize,
    tc: Option<usize>,
    typed: bool,
) -> Result<(), String> {
    let mut root = PNode::Leaf(Value::Null);
    for &r in rows {
        let p = table.cell(r, pc);
        let v = table.cell(r, vc);
        let ty = tc.map(|c| table.cell(r, c).trim().to_ascii_lowercase()).unwrap_or_default();
        let val = match ty.as_str() {
            "string" => Value::String(v),
            "number" => serde_json::from_str::<serde_json::Number>(v.trim())
                .map(Value::Number)
                .unwrap_or(Value::String(v)),
            "bool" | "boolean" => Value::Bool(v.trim().eq_ignore_ascii_case("true")),
            "null" => Value::Null,
            "object" => Value::Object(Default::default()),
            "array" | "array-empty" => Value::Array(Vec::new()),
            _ => match (typed, typed_json(&v)) {
                (true, Some(lit)) => serde_json::from_str(lit).unwrap_or(Value::String(v)),
                _ => Value::String(v),
            },
        };
        let segs: Vec<String> = if p.is_empty() {
            Vec::new()
        } else {
            p.strip_prefix('/').unwrap_or(&p).split('/').map(pointer_unescape).collect()
        };
        root.insert(&segs, val);
    }
    let value = root.into_value();
    let mut w = create(path)?;
    serde_json::to_writer_pretty(&mut w, &value).map_err(|e| format!("Write error: {e}"))?;
    w.write_all(b"\n").map_err(io_err)?;
    w.flush().map_err(io_err)
}

/// Builder node for XML reconstruction (children keyed by "name[n]" / "#text[n]").
#[derive(Default)]
struct BNode {
    node: XNode,
    kids: Vec<Option<BNode>>, // parallel to node.children; Some for elements
    index: HashMap<String, usize>,
}

impl BNode {
    fn child(&mut self, key: &str, name: &str) -> &mut BNode {
        let i = match self.index.get(key) {
            Some(&i) => i,
            None => {
                self.node.children.push(XChild::Elem(XNode::default()));
                self.kids.push(Some(BNode {
                    node: XNode { name: name.to_string(), ..Default::default() },
                    ..Default::default()
                }));
                self.index.insert(key.to_string(), self.kids.len() - 1);
                self.kids.len() - 1
            }
        };
        self.kids[i].as_mut().expect("element child")
    }

    fn set_text(&mut self, key: &str, v: String) {
        match self.index.get(key) {
            Some(&i) => self.node.children[i] = XChild::Text(v),
            None => {
                self.node.children.push(XChild::Text(v));
                self.kids.push(None);
                self.index.insert(key.to_string(), self.kids.len() - 1);
            }
        }
    }

    fn finish(self) -> XNode {
        let mut n = self.node;
        for (slot, kid) in n.children.iter_mut().zip(self.kids) {
            if let Some(k) = kid {
                *slot = XChild::Elem(k.finish());
            }
        }
        n
    }
}

/// Split "name[3]" into ("name", 3); index defaults to 1.
fn split_index(seg: &str) -> (&str, usize) {
    if let Some(open) = seg.rfind('[')
        && seg.ends_with(']')
        && let Ok(i) = seg[open + 1..seg.len() - 1].trim().parse::<usize>()
    {
        return (&seg[..open], i);
    }
    (seg, 1)
}

fn export_xml_structured(table: &dyn TableRead, path: &Path, rows: &[usize], pc: usize, vc: usize) -> Result<(), String> {
    let mut doc = BNode::default();
    for &r in rows {
        let p = table.cell(r, pc);
        let v = table.cell(r, vc);
        let segs: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
        let Some((last, parents)) = segs.split_last() else { continue };
        let mut cur = &mut doc;
        for s in parents {
            let (name, i) = split_index(s);
            let name = xml_name(name, true);
            cur = cur.child(&format!("{name}[{i}]"), &name);
        }
        if let Some(attr) = last.strip_prefix('@') {
            let attr = xml_name(attr, true);
            match cur.node.attrs.iter_mut().find(|(k, _)| *k == attr) {
                Some(a) => a.1 = v,
                None => cur.node.attrs.push((attr, v)),
            }
        } else if last.starts_with("text()") {
            let (_, i) = split_index(last);
            cur.set_text(&format!("#text[{i}]"), v);
        } else {
            let (name, i) = split_index(last);
            let name = xml_name(name, true);
            let el = cur.child(&format!("{name}[{i}]"), &name);
            if !v.is_empty() {
                el.set_text("#text[1]", v);
            }
        }
    }
    let doc = doc.finish();
    let mut tops: Vec<XNode> = doc
        .children
        .into_iter()
        .filter_map(|c| match c {
            XChild::Elem(e) => Some(e),
            _ => None,
        })
        .collect();
    let root = if tops.len() == 1 {
        tops.pop().unwrap()
    } else {
        XNode { name: "root".into(), attrs: Vec::new(), children: tops.into_iter().map(XChild::Elem).collect() }
    };
    let mut w = create(path)?;
    w.write_all(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n").map_err(io_err)?;
    write_xnode(&mut w, &root, 0, true).map_err(io_err)?;
    w.flush().map_err(io_err)
}

fn write_xnode(w: &mut impl Write, n: &XNode, depth: usize, pretty: bool) -> std::io::Result<()> {
    if pretty {
        w.write_all("  ".repeat(depth).as_bytes())?;
    }
    write!(w, "<{}", n.name)?;
    for (k, v) in &n.attrs {
        write!(w, " {k}=\"{}\"", xml_escape(v, true))?;
    }
    if n.children.is_empty() {
        w.write_all(b"/>")?;
    } else {
        w.write_all(b">")?;
        // Indent only element-only content, so mixed content keeps its exact text.
        let inner_pretty = pretty && n.children.iter().all(|c| matches!(c, XChild::Elem(_)));
        for c in &n.children {
            match c {
                XChild::Text(t) => w.write_all(xml_escape(t, false).as_bytes())?,
                XChild::Elem(e) => {
                    if inner_pretty {
                        w.write_all(b"\n")?;
                    }
                    write_xnode(w, e, depth + 1, inner_pretty)?;
                }
            }
        }
        if inner_pretty {
            w.write_all(b"\n")?;
            w.write_all("  ".repeat(depth).as_bytes())?;
        }
        write!(w, "</{}>", n.name)?;
    }
    if pretty && depth == 0 {
        w.write_all(b"\n")?;
    }
    Ok(())
}

// ===========================================================================
// PDF
// ===========================================================================

#[derive(Clone, Debug)]
pub struct PdfOptions {
    pub landscape: bool,
    pub paper_a4: bool,
    pub font_size: f32,
    pub repeat_headers: bool,
    pub labels: bool,
    pub gridlines: bool,
    pub center_horizontally: bool,
    pub center_vertically: bool,
    pub row_wise_page_order: bool,
    pub title: Option<String>,
    pub page_numbers: bool,
}

impl Default for PdfOptions {
    fn default() -> Self {
        Self {
            landscape: false,
            paper_a4: false,
            font_size: 9.0,
            repeat_headers: true,
            labels: false,
            gridlines: true,
            center_horizontally: false,
            center_vertically: false,
            row_wise_page_order: false,
            title: None,
            page_numbers: true,
        }
    }
}

/// Helvetica AFM widths for 0x20..=0x7E (1/1000 em).
const HELV: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, // 0x20
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, // 0x30
    1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, // 0x40
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556, // 0x50
    333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556, // 0x60
    556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584, // 0x70
];

/// Helvetica-Bold AFM widths for 0x20..=0x7E.
const HELV_BOLD: [u16; 95] = [
    278, 333, 474, 556, 556, 889, 722, 238, 333, 333, 389, 584, 278, 333, 278, 278, // 0x20
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 333, 333, 584, 584, 584, 611, // 0x30
    975, 722, 722, 722, 722, 667, 611, 778, 722, 278, 556, 722, 611, 833, 722, 778, // 0x40
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 333, 278, 333, 584, 556, // 0x50
    333, 556, 611, 556, 611, 556, 333, 611, 611, 278, 278, 556, 278, 889, 611, 611, // 0x60
    611, 611, 389, 556, 333, 611, 556, 778, 556, 556, 500, 389, 280, 389, 584, // 0x70
];

/// Width (1/1000 em) of a WinAnsi byte. Bytes above 0x7E use approximations.
fn glyph_width(b: u8, bold: bool) -> u16 {
    match b {
        0x20..=0x7E => (if bold { &HELV_BOLD } else { &HELV })[(b - 0x20) as usize],
        0x85 | 0x89 | 0x97 | 0x99 => 1000, // … ‰ — ™
        0x95 => 350,                       // •
        0x91 | 0x92 | 0x82 => 222,
        0x8B | 0x9B => 333,
        0xC6 | 0x8C => 1000, // Æ Œ
        0xE6 | 0x9C => 944,  // æ œ
        0xA0 => 278,
        0xC0..=0xDE => 722, // accented capitals (approx.)
        _ => 556,
    }
}

/// Map a char to its WinAnsiEncoding byte ('?' when not representable).
fn win_ansi(c: char) -> u8 {
    let u = c as u32;
    match u {
        0x20..=0x7E | 0xA0..=0xFF => u as u8,
        0x09 | 0x0A | 0x0D => b' ',
        _ => match c {
            '€' => 0x80,
            '‚' => 0x82,
            'ƒ' => 0x83,
            '„' => 0x84,
            '…' => 0x85,
            '†' => 0x86,
            '‡' => 0x87,
            'ˆ' => 0x88,
            '‰' => 0x89,
            'Š' => 0x8A,
            '‹' => 0x8B,
            'Œ' => 0x8C,
            'Ž' => 0x8E,
            '\u{2018}' => 0x91,
            '\u{2019}' => 0x92,
            '\u{201C}' => 0x93,
            '\u{201D}' => 0x94,
            '•' => 0x95,
            '–' => 0x96,
            '—' => 0x97,
            '˜' => 0x98,
            '™' => 0x99,
            'š' => 0x9A,
            '›' => 0x9B,
            'œ' => 0x9C,
            'ž' => 0x9E,
            'Ÿ' => 0x9F,
            _ => b'?',
        },
    }
}

fn encode(s: &str) -> Vec<u8> {
    s.chars().map(win_ansi).collect()
}

fn text_width(bytes: &[u8], bold: bool, fs: f32) -> f32 {
    bytes.iter().map(|&b| glyph_width(b, bold) as f32).sum::<f32>() * fs / 1000.0
}

/// Encode and truncate with "…" so the text fits in `max_w` points.
fn fit_text(s: &str, max_w: f32, bold: bool, fs: f32) -> Vec<u8> {
    let bytes = encode(s);
    if text_width(&bytes, bold, fs) <= max_w {
        return bytes;
    }
    let ell = glyph_width(0x85, bold) as f32 * fs / 1000.0;
    let mut w = 0.0;
    let mut out = Vec::new();
    for b in bytes {
        let gw = glyph_width(b, bold) as f32 * fs / 1000.0;
        if w + gw + ell > max_w {
            break;
        }
        w += gw;
        out.push(b);
    }
    if ell <= max_w {
        out.push(0x85);
    }
    out
}

/// PDF literal string with `( ) \` escaped and non-ASCII bytes as octal escapes.
fn pdf_string(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() + 2);
    s.push('(');
    for &b in bytes {
        match b {
            b'(' | b')' | b'\\' => {
                s.push('\\');
                s.push(b as char);
            }
            0x20..=0x7E => s.push(b as char),
            _ => s.push_str(&format!("\\{b:03o}")),
        }
    }
    s.push(')');
    s
}

fn num(x: f32) -> String {
    let s = format!("{x:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.to_string() }
}

struct PdfWriter {
    w: BufWriter<File>,
    pos: usize,
    offsets: Vec<usize>, // index = object number; 0 unused
}

impl PdfWriter {
    fn raw(&mut self, b: &[u8]) -> Result<(), String> {
        self.w.write_all(b).map_err(io_err)?;
        self.pos += b.len();
        Ok(())
    }
    fn obj(&mut self, id: usize, body: &[u8]) -> Result<(), String> {
        if self.offsets.len() <= id {
            self.offsets.resize(id + 1, 0);
        }
        self.offsets[id] = self.pos;
        self.raw(format!("{id} 0 obj\n").as_bytes())?;
        self.raw(body)?;
        self.raw(b"\nendobj\n")
    }
    fn stream(&mut self, id: usize, data: &[u8]) -> Result<(), String> {
        let mut body = format!("<< /Length {} >>\nstream\n", data.len()).into_bytes();
        body.extend_from_slice(data);
        body.extend_from_slice(b"\nendstream");
        self.obj(id, &body)
    }
}

struct Band {
    start: usize,
    end: usize,
    header: bool,
}

/// Lay the table out into pages and write a PDF. Returns the number of pages.
pub fn export_pdf(
    table: &dyn TableRead,
    path: &Path,
    opts: &PdfOptions,
    rows: Option<&[usize]>,
    cols: Option<&[usize]>,
) -> Result<usize, String> {
    let rows = index_list(rows, table.nrows());
    let cols = index_list(cols, table.ncols());

    let (mut pw, mut ph) = if opts.paper_a4 { (595.28f32, 841.89f32) } else { (612.0, 792.0) };
    if opts.landscape {
        std::mem::swap(&mut pw, &mut ph);
    }
    let fs = if opts.font_size.is_finite() { opts.font_size.clamp(4.0, 48.0) } else { 9.0 };
    let margin = 36.0;
    let pad = (fs * 0.4).max(2.0);
    let row_h = fs * 1.5;
    let avail_w = pw - 2.0 * margin;
    let avail_h = ph - 2.0 * margin;
    let title_fs = fs + 3.0;
    let title_h = if opts.title.as_deref().is_some_and(|t| !t.is_empty()) { title_fs * 1.8 } else { 0.0 };
    let footer_h = if opts.page_numbers { fs * 2.2 } else { 0.0 };
    let has_header = table.has_header();
    let letters_h = if opts.labels { row_h } else { 0.0 };
    let header_h = if has_header { row_h } else { 0.0 };

    // Row number label column.
    let label_w = if opts.labels {
        let maxn = rows.iter().max().map_or(1, |&r| r + 1);
        (text_width(maxn.to_string().as_bytes(), false, fs) + 2.0 * pad).max(fs * 2.0)
    } else {
        0.0
    };

    // Column widths from content (sampled), capped at 200pt and the printable width.
    const SAMPLE: usize = 10_000;
    let max_col = 200.0f32.min(avail_w - label_w).max(fs * 2.0);
    let widths: Vec<f32> = cols
        .iter()
        .map(|&c| {
            let mut w = if has_header { text_width(&encode(&table.column_name(c)), true, fs) } else { 0.0 };
            if opts.labels {
                w = w.max(text_width(col_letter(c).as_bytes(), false, fs));
            }
            for &r in rows.iter().take(SAMPLE) {
                if w >= max_col {
                    break;
                }
                w = w.max(text_width(&encode(&table.cell(r, c)), false, fs));
            }
            (w + 2.0 * pad).clamp(fs * 2.0, max_col)
        })
        .collect();

    // Horizontal bands (ranges of positions within `cols`).
    let mut col_bands: Vec<(usize, usize)> = Vec::new();
    let mut s = 0;
    while s < cols.len() {
        let mut e = s;
        let mut w = label_w;
        while e < cols.len() && (e == s || w + widths[e] <= avail_w + 0.01) {
            w += widths[e];
            e += 1;
        }
        col_bands.push((s, e));
        s = e;
    }
    if col_bands.is_empty() {
        col_bands.push((0, 0));
    }

    // Vertical bands.
    let body_h = avail_h - title_h - footer_h;
    let mut row_bands: Vec<Band> = Vec::new();
    let mut s = 0;
    loop {
        let header = has_header && (row_bands.is_empty() || opts.repeat_headers);
        let room = body_h - letters_h - if header { header_h } else { 0.0 };
        let per = ((room / row_h).floor() as usize).max(1);
        let e = (s + per).min(rows.len());
        row_bands.push(Band { start: s, end: e, header });
        s = e;
        if s >= rows.len() {
            break;
        }
    }

    let mut pages: Vec<(usize, usize)> = Vec::new(); // (row band, col band)
    if opts.row_wise_page_order {
        for rb in 0..row_bands.len() {
            for cb in 0..col_bands.len() {
                pages.push((rb, cb));
            }
        }
    } else {
        for cb in 0..col_bands.len() {
            for rb in 0..row_bands.len() {
                pages.push((rb, cb));
            }
        }
    }
    let total = pages.len();

    let mut pdf = PdfWriter { w: create(path)?, pos: 0, offsets: vec![0; 5] };
    pdf.raw(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n")?;
    pdf.obj(1, b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.obj(3, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>")?;
    pdf.obj(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>")?;

    let numeric = |s: &str| {
        let t = s.trim();
        !t.is_empty() && t.parse::<f64>().is_ok_and(|v| v.is_finite())
    };

    let mut kids = String::new();
    for (pi, &(rb, cb)) in pages.iter().enumerate() {
        let band = &row_bands[rb];
        let (cs, ce) = col_bands[cb];
        let mut c = String::new();
        let text = |c: &mut String, bold: bool, x: f32, y: f32, bytes: &[u8]| {
            if !bytes.is_empty() {
                c.push_str(&format!(
                    "BT /{} {} Tf {} {} Td {} Tj ET\n",
                    if bold { "F2" } else { "F1" },
                    num(fs),
                    num(x),
                    num(y),
                    pdf_string(bytes)
                ));
            }
        };

        if let Some(t) = opts.title.as_deref().filter(|t| !t.is_empty()) {
            let b = fit_text(t, avail_w, true, title_fs);
            let x = margin + (avail_w - text_width(&b, true, title_fs)) / 2.0;
            c.push_str(&format!(
                "BT /F2 {} Tf {} {} Td {} Tj ET\n",
                num(title_fs),
                num(x),
                num(ph - margin - title_fs),
                pdf_string(&b)
            ));
        }

        let nrows = band.end - band.start;
        let hdr_h = if band.header { header_h } else { 0.0 };
        let content_w = label_w + widths[cs..ce].iter().sum::<f32>();
        let content_h = letters_h + hdr_h + nrows as f32 * row_h;
        let x0 = margin + if opts.center_horizontally { ((avail_w - content_w) / 2.0).max(0.0) } else { 0.0 };
        let y0 = ph - margin - title_h - if opts.center_vertically { ((body_h - content_h) / 2.0).max(0.0) } else { 0.0 };
        let baseline = |top: f32| top - row_h / 2.0 - fs * 0.35;

        // Column x positions.
        let mut xs = vec![x0 + label_w];
        for w in &widths[cs..ce] {
            xs.push(xs.last().unwrap() + w);
        }

        let mut top = y0;
        // Label shading.
        if opts.labels {
            c.push_str("0.92 g\n");
            c.push_str(&format!("{} {} {} {} re f\n", num(x0), num(top - letters_h), num(content_w), num(letters_h)));
            c.push_str(&format!(
                "{} {} {} {} re f\n",
                num(x0),
                num(y0 - content_h),
                num(label_w),
                num(content_h - letters_h)
            ));
            c.push_str("0 g\n");
        }
        if band.header {
            c.push_str("0.85 g\n");
            c.push_str(&format!(
                "{} {} {} {} re f\n0 g\n",
                num(x0 + label_w),
                num(top - letters_h - hdr_h),
                num(content_w - label_w),
                num(hdr_h)
            ));
        }
        if opts.labels {
            for (k, &col) in cols[cs..ce].iter().enumerate() {
                let b = encode(&col_letter(col));
                let x = xs[k] + (widths[cs + k] - text_width(&b, false, fs)) / 2.0;
                text(&mut c, false, x, baseline(top), &b);
            }
            top -= letters_h;
        }
        if band.header {
            for (k, &col) in cols[cs..ce].iter().enumerate() {
                let b = fit_text(&table.column_name(col), widths[cs + k] - 2.0 * pad, true, fs);
                text(&mut c, true, xs[k] + pad, baseline(top), &b);
            }
            top -= hdr_h;
        }
        for &r in &rows[band.start..band.end] {
            if opts.labels {
                let b = encode(&(r + 1).to_string());
                text(&mut c, false, x0 + label_w - pad - text_width(&b, false, fs), baseline(top), &b);
            }
            for (k, &col) in cols[cs..ce].iter().enumerate() {
                let v = table.cell(r, col);
                if v.is_empty() {
                    continue;
                }
                let b = fit_text(&v, widths[cs + k] - 2.0 * pad, false, fs);
                let x = if numeric(&v) { xs[k + 1] - pad - text_width(&b, false, fs) } else { xs[k] + pad };
                text(&mut c, false, x, baseline(top), &b);
            }
            top -= row_h;
        }

        if opts.gridlines && content_w > 0.0 && content_h > 0.0 {
            c.push_str("0.5 w 0.6 G\n");
            let bottom = y0 - content_h;
            let mut y = y0;
            let lines = (content_h / row_h).round() as usize;
            for _ in 0..=lines {
                c.push_str(&format!("{} {} m {} {} l\n", num(x0), num(y), num(x0 + content_w), num(y)));
                y -= row_h;
            }
            let mut vx = vec![x0];
            vx.extend(xs.iter().copied().filter(|&x| x > x0 + 0.01));
            for x in vx {
                c.push_str(&format!("{} {} m {} {} l\n", num(x), num(y0), num(x), num(bottom)));
            }
            c.push_str("S 0 G\n");
        }

        if opts.page_numbers {
            let b = encode(&format!("Page {} of {}", pi + 1, total));
            let x = (pw - text_width(&b, false, fs)) / 2.0;
            text(&mut c, false, x, margin, &b);
        }

        let page_id = 5 + 2 * pi;
        let content_id = page_id + 1;
        pdf.obj(
            page_id,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Resources << /Font << /F1 3 0 R /F2 4 0 R >> >> /Contents {} 0 R >>",
                num(pw),
                num(ph),
                content_id
            )
            .as_bytes(),
        )?;
        pdf.stream(content_id, c.as_bytes())?;
        kids.push_str(&format!("{page_id} 0 R "));
    }

    pdf.obj(2, format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.trim_end(), total).as_bytes())?;

    let xref_pos = pdf.pos;
    let n = pdf.offsets.len();
    let mut x = format!("xref\n0 {n}\n0000000000 65535 f \n");
    for &off in &pdf.offsets[1..] {
        x.push_str(&format!("{off:010} 00000 n \n"));
    }
    x.push_str(&format!("trailer\n<< /Size {n} /Root 1 0 R >>\nstartxref\n{xref_pos}\n%%EOF\n"));
    pdf.raw(x.as_bytes())?;
    pdf.w.flush().map_err(io_err)?;
    Ok(total)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tmp(ext: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("ficsv_formats_{}_{}_{n}.{ext}", std::process::id(), uuid::Uuid::new_v4()))
    }

    fn write(ext: &str, s: &str) -> PathBuf {
        let p = tmp(ext);
        std::fs::write(&p, s).unwrap();
        p
    }

    fn col(t: &VecTable, name: &str) -> usize {
        t.header.as_ref().unwrap().iter().position(|h| h == name).unwrap_or_else(|| panic!("no column {name}"))
    }

    #[test]
    fn json_array_of_objects() {
        let p = write("json", r#"[{"a":1,"b":"x"},{"a":2,"c":true},{"b":null}]"#);
        let t = import_file(&p, false).unwrap();
        assert_eq!(t.header.as_ref().unwrap(), &["a", "b", "c"]);
        assert_eq!(t.rows, vec![vec!["1", "x", ""], vec!["2", "", "true"], vec!["", "", ""]]);
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn json_nested_flattening() {
        let p = write(
            "json",
            r#"{"meta":{"count":2},"data":{"people":[
                {"name":"Ann","address":{"city":"Oslo","zip":"0150"},"tags":["a","b"]},
                {"name":"Bob","address":{"city":"Rome"},"tags":[],"pets":[{"kind":"cat"}]}]}}"#,
        );
        let t = import_file(&p, false).unwrap();
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[0][col(&t, "address.city")], "Oslo");
        assert_eq!(t.rows[1][col(&t, "address.city")], "Rome");
        assert_eq!(t.rows[0][col(&t, "tags")], "a; b");
        assert_eq!(t.rows[1][col(&t, "pets.0.kind")], "cat");
        assert_eq!(t.rows[1][col(&t, "address.zip")], "");
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn xml_repeating_with_attributes() {
        let p = write(
            "xml",
            r#"<?xml version="1.0"?>
            <catalog>
              <info>x</info>
              <book id="1" lang="en"><title>Rust &amp; You</title><price cur="EUR">10</price></book>
              <book id="2"><title><![CDATA[<XML>]]></title><author>A</author><author>B</author></book>
              <book id="3"/>
            </catalog>"#,
        );
        let t = import_file(&p, false).unwrap();
        assert_eq!(t.rows.len(), 3);
        assert_eq!(t.rows[0][col(&t, "@id")], "1");
        assert_eq!(t.rows[0][col(&t, "@lang")], "en");
        assert_eq!(t.rows[0][col(&t, "title")], "Rust & You");
        assert_eq!(t.rows[0][col(&t, "price.@cur")], "EUR");
        assert_eq!(t.rows[0][col(&t, "price.#text")], "10");
        assert_eq!(t.rows[1][col(&t, "title")], "<XML>");
        assert_eq!(t.rows[1][col(&t, "author")], "A; B");
        assert_eq!(t.rows[2][col(&t, "@id")], "3");
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn json_preserve_round_trip() {
        let src = r#"{"name":"x","n":1.5,"big":12345678901234,"ok":true,"none":null,
            "e":{},"ea":[],"arr":[1,"two",{"k/~":[null,false]}],"o":{"z":1,"a":{"b":"c"}},"":"empty key"}"#;
        let p = write("json", src);
        let t = import_file(&p, true).unwrap();
        assert_eq!(t.header.as_ref().unwrap(), &["path", "value", "type"]);
        let out = tmp("json");
        export_file(&t, &out, ExportFormat::Json, false, None, None).unwrap();
        let a: Value = serde_json::from_str(src).unwrap();
        let b: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(a, b);
        // Key order preserved too.
        assert_eq!(serde_json::to_string(&a).unwrap(), serde_json::to_string(&b).unwrap());
        std::fs::remove_file(p).ok();
        std::fs::remove_file(out).ok();
    }

    #[test]
    fn xml_preserve_round_trip() {
        let src = r#"<?xml version="1.0"?>
<root xmlns:x="urn:x" ver="1">
  <item id="a&quot;b"><name>One &lt;1&gt;</name><x:tag>t</x:tag></item>
  <item id="2"><name>Two</name><empty/></item>
  <note>mixed <b>bold</b> tail</note>
  <item><name/></item>
</root>"#;
        let p = write("xml", src);
        let t = import_file(&p, true).unwrap();
        assert_eq!(t.header.as_ref().unwrap(), &["path", "value"]);
        assert!(t.rows.iter().any(|r| r[0] == "/root/item[1]/@id" && r[1] == "a\"b"));
        let out = tmp("xml");
        export_file(&t, &out, ExportFormat::Xml, false, None, None).unwrap();
        let a = parse_xml(src).unwrap();
        let b = parse_xml(&std::fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(a, b);
        std::fs::remove_file(p).ok();
        std::fs::remove_file(out).ok();
    }

    fn sample() -> VecTable {
        VecTable {
            header: Some(vec!["Name".into(), "Age".into(), "1st place".into(), "Name".into()]),
            rows: vec![
                vec!["Ann <&>".into(), "30".into(), "true".into(), "".into()],
                vec!["Bob".into(), "007".into(), "".into(), "x".into()],
            ],
        }
    }

    #[test]
    fn json_export_plain_and_typed() {
        let t = sample();
        let out = tmp("json");
        export_file(&t, &out, ExportFormat::Json, false, None, None).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(v[0]["Age"], "30");
        assert_eq!(v[1]["Name_2"], "x");
        export_file(&t, &out, ExportFormat::Json, true, Some(&[0, 1]), Some(&[1, 2])).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(v[0]["Age"], 30);
        assert_eq!(v[0]["1st place"], true);
        assert_eq!(v[1]["Age"], "007");
        assert_eq!(v[1]["1st place"], Value::Null);
        assert!(v[0].get("Name").is_none());
        std::fs::remove_file(out).ok();
    }

    #[test]
    fn xml_export_records_round_trip() {
        let t = sample();
        let out = tmp("xml");
        export_file(&t, &out, ExportFormat::Xml, false, None, None).unwrap();
        let s = std::fs::read_to_string(&out).unwrap();
        assert!(s.contains("<_1st_place>true</_1st_place>"));
        assert!(s.contains("Ann &lt;&amp;&gt;"));
        let back = import_file(&out, false).unwrap();
        assert_eq!(back.rows.len(), 2);
        assert_eq!(back.rows[0][col(&back, "Name")], "Ann <&>");
        assert_eq!(back.rows[1][col(&back, "Name_2")], "x");
        std::fs::remove_file(out).ok();
    }

    fn check_pdf(bytes: &[u8]) -> usize {
        assert!(bytes.starts_with(b"%PDF-1.4"));
        // Byte-for-byte ASCII view so string offsets equal file offsets.
        let s: String = bytes.iter().map(|&b| if b < 0x80 { b as char } else { '?' }).collect();
        assert!(s.trim_end().ends_with("%%EOF"));
        let sx = s.rfind("startxref\n").unwrap();
        let xref_pos: usize = s[sx + 10..].lines().next().unwrap().trim().parse().unwrap();
        assert!(bytes[xref_pos..].starts_with(b"xref\n"));
        let mut lines = s[xref_pos..].lines().skip(1);
        let n: usize = lines.next().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
        lines.next(); // free entry
        for id in 1..n {
            let line = lines.next().unwrap();
            assert_eq!(line.len(), 19, "xref entry must be 20 bytes incl. EOL");
            let off: usize = line[..10].parse().unwrap();
            let want = format!("{id} 0 obj");
            assert!(bytes[off..].starts_with(want.as_bytes()), "object {id} not at {off}");
        }
        // Stream lengths are correct.
        let mut i = 0;
        while let Some(p) = s[i..].find("/Length ") {
            let at = i + p + 8;
            let len: usize = s[at..].split(' ').next().unwrap().parse().unwrap();
            let start = at + s[at..].find("stream\n").unwrap() + 7;
            assert!(s[start + len..].starts_with("\nendstream"));
            i = start + len;
        }
        n
    }

    #[test]
    fn pdf_output_is_valid() {
        let mut t = VecTable { header: Some(vec!["id".into(), "text (paren) \\ é ☃".into(), "n".into()]), rows: vec![] };
        for i in 0..300 {
            t.rows.push(vec![i.to_string(), "word ".repeat(i % 60), format!("{}.5", i)]);
        }
        let out = tmp("pdf");
        let opts = PdfOptions { title: Some("Report".into()), labels: true, ..Default::default() };
        let pages = export_pdf(&t, &out, &opts, None, None).unwrap();
        assert!(pages > 1);
        let bytes = std::fs::read(&out).unwrap();
        check_pdf(&bytes);
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains(&format!("/Count {pages}")));
        assert!(s.contains(&format!("(Page 1 of {pages})")));
        assert!(s.contains("\\(paren\\) \\\\ \\351 ?")); // escaped, WinAnsi é, snowman -> ?
        assert!(s.contains("\\205")); // truncated with ellipsis

        // Many columns, landscape A4, across-then-down, subsets; and an empty table.
        let wide = VecTable { header: None, rows: (0..50).map(|r| (0..40).map(|c| format!("r{r}c{c} long-ish")).collect()).collect() };
        let o2 = PdfOptions { landscape: true, paper_a4: true, row_wise_page_order: true, center_horizontally: true, center_vertically: true, ..Default::default() };
        let pages = export_pdf(&wide, &out, &o2, Some(&[0, 5, 9]), None).unwrap();
        assert!(pages > 1);
        check_pdf(&std::fs::read(&out).unwrap());
        assert_eq!(export_pdf(&VecTable::default(), &out, &PdfOptions::default(), None, None).unwrap(), 1);
        check_pdf(&std::fs::read(&out).unwrap());
        std::fs::remove_file(out).ok();
    }

    #[test]
    fn helpers() {
        assert_eq!(xml_name("1st place", false), "_1st_place");
        assert_eq!(xml_name("", false), "_");
        assert_eq!(xml_name("a:b", false), "a_b");
        assert_eq!(text_width(b"Hello", false, 1000.0), 722.0 + 556.0 + 222.0 + 222.0 + 556.0);
        assert_eq!(num(3.0), "3");
        assert_eq!(num(2.25), "2.25");
    }

    /// Manual: `cargo test --release formats::tests::perf_large -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn perf_large() {
        let n = 1_000_000;
        let t = VecTable {
            header: Some(vec!["id".into(), "name".into(), "value".into(), "flag".into()]),
            rows: (0..n).map(|i| vec![i.to_string(), format!("name {i}"), format!("{}.25", i % 977), (i % 2 == 0).to_string()]).collect(),
        };
        for (ext, f) in [("json", ExportFormat::Json), ("xml", ExportFormat::Xml)] {
            let out = tmp(ext);
            let st = std::time::Instant::now();
            export_file(&t, &out, f, true, None, None).unwrap();
            eprintln!("{ext}: {:?}", st.elapsed());
            std::fs::remove_file(out).ok();
        }
        let out = std::env::var("FICSV_PDF_OUT").map(PathBuf::from).unwrap_or_else(|_| tmp("pdf"));
        let st = std::time::Instant::now();
        let pages = export_pdf(&t, &out, &PdfOptions { labels: true, title: Some("Big".into()), ..Default::default() }, None, None).unwrap();
        eprintln!("pdf: {pages} pages in {:?}", st.elapsed());
        check_pdf(&std::fs::read(&out).unwrap());
    }
}
