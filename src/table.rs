//! Shared interface between the core table model and feature modules
//! (transforms, analysis, import/export). Feature modules depend ONLY on this file.

use std::collections::HashMap;

/// Read-only access to a table. Coordinates are 0-based *data* rows (the header row,
/// if any, is not counted) and columns. Implementations are cheap to clone/share and
/// safe to read from background threads.
pub trait TableRead: Send + Sync {
    /// Number of data rows (excluding the header row).
    fn nrows(&self) -> usize;
    fn ncols(&self) -> usize;
    /// Cell text; returns "" when out of range.
    fn cell(&self, row: usize, col: usize) -> String;
    /// Header text for a column, or its letter (A, B, …) when there is no header.
    fn column_name(&self, col: usize) -> String;
    /// Whether the table has a header row.
    fn has_header(&self) -> bool;
}

/// Simple in-memory table, handy for tests and for imported data.
#[derive(Clone, Default, Debug)]
pub struct VecTable {
    pub header: Option<Vec<String>>,
    pub rows: Vec<Vec<String>>,
}

impl TableRead for VecTable {
    fn nrows(&self) -> usize {
        self.rows.len()
    }
    fn ncols(&self) -> usize {
        let h = self.header.as_ref().map_or(0, |h| h.len());
        self.rows.iter().map(|r| r.len()).max().unwrap_or(0).max(h)
    }
    fn cell(&self, row: usize, col: usize) -> String {
        self.rows.get(row).and_then(|r| r.get(col)).cloned().unwrap_or_default()
    }
    fn column_name(&self, col: usize) -> String {
        match self.header.as_ref().and_then(|h| h.get(col)) {
            Some(n) if !n.is_empty() => n.clone(),
            _ => col_letter(col),
        }
    }
    fn has_header(&self) -> bool {
        self.header.is_some()
    }
}

pub fn col_letter(mut c: usize) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b'A' + (c % 26) as u8);
        if c < 26 {
            break;
        }
        c = c / 26 - 1;
    }
    s.reverse();
    String::from_utf8(s).unwrap()
}

// ---------------------------------------------------------------------------
// Parameterised commands (used by transforms.rs). The app renders a generic form
// for `params`, then calls the module's apply function with the values by key.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum ParamKind {
    Text,
    Int,
    Float,
    Bool,
    Choice(Vec<&'static str>),
}

#[derive(Clone, Debug)]
pub struct Param {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: ParamKind,
    /// Default value as text ("true"/"false" for Bool, one of the options for Choice).
    pub default: &'static str,
    /// Optional one-line help shown under the field.
    pub help: &'static str,
}

pub type ParamValues = HashMap<String, String>;

/// Result of a transform applied to a rectangular selection.
///
/// The app first inserts `insert_cols` empty columns after the selection's last column
/// and `insert_rows` empty rows after its last row, then writes `cells` starting at the
/// selection's top-left cell. Selection cells not covered by `cells` are cleared.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TransformOutput {
    pub cells: Vec<Vec<String>>,
    pub insert_cols: usize,
    pub insert_rows: usize,
}

impl TransformOutput {
    pub fn same_shape(cells: Vec<Vec<String>>) -> Self {
        Self { cells, insert_cols: 0, insert_rows: 0 }
    }
}
