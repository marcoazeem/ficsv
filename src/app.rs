//! Application shell: tabs, menus, command palette, dialogs and command handlers.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Align, Color32, Context, Key, Layout, RichText, Ui};
use regex::Regex;

use crate::analysis::{AnalysisAction, AnalysisTab, AnalysisWindow};
use crate::commands::{self, Cmd};
use crate::doc::{delimiter_label, Document, Filter, LineEnding, LoadOpts, Quoting, SubTable};
use crate::formats::{self, ExportFormat, PdfOptions};
use crate::grid::{self, GridEvent};
use crate::jobs::{self, FindOpts, JobResult, Region, SavePlan};
use crate::query::{escape_value, Query};
use crate::settings::{config_dir, ExternalChange, Settings, Theme};
use crate::table::{col_letter, Param, ParamKind, ParamValues, TableRead};
use crate::transforms;
use crate::ui_kit;

const MAX_BLOCK: usize = 5_000_000;
pub const APP_NAME: &str = "FI CSV";

const ENCODINGS: &[&str] = &[
    "UTF-8", "UTF-16LE", "UTF-16BE", "windows-1252", "ISO-8859-1", "ISO-8859-2", "ISO-8859-15", "windows-1250", "windows-1251",
    "windows-1253", "windows-1254", "windows-1255", "windows-1256", "windows-1257", "windows-1258", "KOI8-R", "KOI8-U",
    "Shift_JIS", "EUC-JP", "ISO-2022-JP", "EUC-KR", "GBK", "gb18030", "Big5",
];
const DELIMS: &[(u8, &str)] = &[(b',', "Comma ,"), (b'\t', "Tab"), (b';', "Semicolon ;"), (b'|', "Pipe |"), (b' ', "Space"), (0x1F, "ASV (0x1F)")];

enum PromptAction {
    Transform(usize),
    InsertRows,
    InsertCols,
    DupRows,
    DupCols,
    GoToCell,
    SelectTo,
    MoveRowsAbove,
    MoveColsLeftOf,
    ColWidth,
    DuplicateFile,
    RenameFile,
}

struct Prompt {
    title: String,
    params: Vec<Param>,
    values: ParamValues,
    action: PromptAction,
    focus: bool,
}

struct LookupState {
    src_doc: usize,
    src_key: usize,
    src_vals: Vec<bool>,
    dst_key: usize,
    insert: bool,
    dst_start: usize,
}

enum Dialog {
    ConfirmClose { tabs: Vec<usize>, quit: bool },
    Filters { col: Option<usize>, text: String, focus: bool },
    Preferences,
    Lookup(LookupState),
    Pdf { opts: PdfOptions, selection_only: bool, print: bool, title: bool },
    Export { format: ExportFormat, typed: bool, visible_only: bool },
    Import { path: PathBuf, preserve: bool },
    DeleteFile(usize),
    ExternalChanged(usize),
    About,
}

#[derive(Clone)]
enum PalItem {
    Cmd(Cmd),
    Transform(usize),
    File(PathBuf),
}

struct Palette {
    query: String,
    sel: usize,
    files: bool,
    focus: bool,
}

pub struct App {
    docs: Vec<Document>,
    active: usize,
    settings: Settings,
    status: Option<(String, Instant)>,
    find: FindOpts,
    find_open: bool,
    replace_open: bool,
    replace_text: String,
    find_focus: bool,
    find_cache: Option<(FindOpts, Option<Regex>)>,
    find_count: Option<usize>,
    palette: Option<Palette>,
    prompt: Option<Prompt>,
    dialog: Option<Dialog>,
    analysis: AnalysisWindow,
    transforms: Vec<transforms::Transform>,
    editor: Option<((usize, usize, usize), String)>,
    allow_quit: bool,
    last_mtime_check: Instant,
    folder_files: Vec<PathBuf>,
    applied_theme: Option<(Theme, f32)>,
    logo: Option<egui::TextureHandle>,
    window_title: String,
    /// Paths still being saved before their tabs close (and whether to quit afterwards).
    close_after_save: Option<(HashSet<PathBuf>, bool)>,
}

fn fmt_num(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn fmt_size(b: u64) -> String {
    let b = b as f64;
    if b >= 1e9 {
        format!("{:.2} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.1} KB", b / 1e3)
    } else {
        format!("{b} B")
    }
}

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn param(key: &'static str, label: &'static str, kind: ParamKind, default: &'static str) -> Param {
    Param { key, label, kind, default, help: "" }
}

fn pv(values: &ParamValues, key: &str) -> String {
    values.get(key).cloned().unwrap_or_default()
}

fn pnum(values: &ParamValues, key: &str) -> Option<i64> {
    values.get(key).and_then(|v| v.trim().parse().ok())
}

fn is_data_file(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
        Some("csv" | "tsv" | "txt" | "tab" | "psv" | "dsv" | "dat")
    )
}

fn scan_folder(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 3 || out.len() > 5000 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        let hidden = p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.'));
        if hidden {
            continue;
        }
        if p.is_dir() {
            scan_folder(&p, out, depth + 1);
        } else if is_data_file(&p) {
            out.push(p);
        }
    }
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, files: Vec<PathBuf>) -> Self {
        let settings = Settings::load();
        let font_warning = crate::rtl::install_fonts(&cc.egui_ctx, &settings.dhivehi_font);
        let mut app = App {
            docs: Vec::new(),
            active: 0,
            settings,
            status: None,
            find: FindOpts { highlight: true, ..Default::default() },
            find_open: false,
            replace_open: false,
            replace_text: String::new(),
            find_focus: false,
            find_cache: None,
            find_count: None,
            palette: None,
            prompt: None,
            dialog: None,
            analysis: AnalysisWindow::new(),
            transforms: transforms::all(),
            editor: None,
            allow_quit: false,
            last_mtime_check: Instant::now(),
            folder_files: Vec::new(),
            applied_theme: None,
            logo: None,
            window_title: String::new(),
            close_after_save: None,
        };
        app.rescan_folders();
        if let Some(w) = font_warning {
            app.msg(w);
        }
        for f in files {
            app.open_path(&cc.egui_ctx, &f);
        }
        app
    }

    // ------------------------------------------------------------------
    // helpers
    // ------------------------------------------------------------------

    fn msg(&mut self, m: impl Into<String>) {
        self.status = Some((m.into(), Instant::now()));
    }

    fn doc(&mut self) -> Option<&mut Document> {
        self.docs.get_mut(self.active)
    }

    /// Active document, ready for structural edits.
    fn doc_edit(&mut self) -> Option<&mut Document> {
        let i = self.active;
        let Some(d) = self.docs.get(i) else { return None };
        if d.read_only {
            self.msg("File is read-only (File ▸ Toggle Read-Only)");
            return None;
        }
        if !d.ready() {
            self.msg("Still indexing the file — try again in a moment");
            return None;
        }
        self.docs.get_mut(i)
    }

    fn rescan_folders(&mut self) {
        let mut files = Vec::new();
        for f in &self.settings.folders {
            scan_folder(Path::new(f), &mut files, 0);
        }
        self.folder_files = files;
    }

    fn find_regex(&mut self) -> Option<Regex> {
        if let Some((o, r)) = &self.find_cache {
            if *o == self.find {
                return r.clone();
            }
        }
        let r = self.find.compile().ok().flatten();
        self.find_cache = Some((self.find.clone(), r.clone()));
        self.find_count = None;
        r
    }

    fn load_opts_for(&self, path: &Path) -> LoadOpts {
        let mut o = LoadOpts::default();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if let Some(d) = self.settings.ext_delimiters.get(&ext.to_ascii_lowercase()) {
                o.delimiter = match d.as_str() {
                    "\\t" | "tab" => Some(b'\t'),
                    s => s.bytes().next(),
                };
            }
        }
        if !self.settings.auto_detect_delimiter && o.delimiter.is_none() {
            o.delimiter = Some(b',');
        }
        o
    }

    pub fn open_path(&mut self, ctx: &Context, path: &Path) {
        if let Some(i) = self.docs.iter().position(|d| d.path.as_deref() == Some(path)) {
            self.active = i;
            return;
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        if ext == "json" || ext == "xml" {
            self.dialog = Some(Dialog::Import { path: path.to_path_buf(), preserve: false });
            return;
        }
        match Document::open(path, self.load_opts_for(path), ctx) {
            Ok(mut d) => {
                d.has_header = self.settings.default_header;
                let mb = d.file_size / 1_000_000;
                if self.settings.read_only_threshold_mb > 0 && mb >= self.settings.read_only_threshold_mb {
                    d.read_only = true;
                }
                self.docs.push(d);
                self.active = self.docs.len() - 1;
                self.settings.push_recent(path);
                self.settings.save();
            }
            Err(e) => self.msg(format!("Could not open: {e}")),
        }
    }

    fn reload(&mut self, ctx: &Context, i: usize, opts: Option<LoadOpts>) {
        let Some(old) = self.docs.get(i) else { return };
        let Some(path) = old.path.clone() else { return };
        let opts = opts.unwrap_or_else(|| old.load_opts.clone());
        match Document::open(&path, opts, ctx) {
            Ok(mut d) => {
                let old = &self.docs[i];
                d.has_header = old.has_header;
                d.read_only = old.read_only;
                d.grid.top = old.grid.top;
                d.grid.cur = old.grid.cur;
                d.grid.anchor = old.grid.cur;
                if self.settings.retain_filters_on_refresh && old.cols_identity() {
                    d.filters = old.filters.clone();
                    d.view_dirty = !d.filters.is_empty();
                }
                for (k, id) in old.cols.iter().enumerate() {
                    if let Some(w) = old.widths.get(id) {
                        d.widths.insert(k as u32, *w);
                    }
                }
                d.widths_measured = !d.widths.is_empty();
                self.docs[i] = d;
            }
            Err(e) => self.msg(format!("Reload failed: {e}")),
        }
    }

    fn sel_block(&self) -> Option<(Vec<Vec<String>>, (usize, usize), (usize, usize))> {
        let d = self.docs.get(self.active)?;
        if d.vrows() == 0 || d.vcols.is_empty() {
            return None;
        }
        let ((r0, c0), (r1, c1)) = d.sel_range();
        let r1 = r1.min(d.vrows() - 1);
        let c1 = c1.min(d.vcols.len() - 1);
        if (r1 + 1 - r0) * (c1 + 1 - c0) > MAX_BLOCK {
            return None;
        }
        let snap = d.snap();
        let block = (r0..=r1)
            .map(|vr| {
                let vals = snap.row_values(d.data_row_id(d.vrow_to_data(vr)));
                (c0..=c1).map(|vc| vals.get(d.vcols[vc]).cloned().unwrap_or_default()).collect()
            })
            .collect();
        Some((block, (r0, c0), (r1, c1)))
    }

    /// Write a block at view position (r0, c0); cells of `clear` rect not covered are emptied.
    fn write_block(&mut self, at: (usize, usize), block: &[Vec<String>], clear: Option<((usize, usize), (usize, usize))>) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        let mut items = Vec::new();
        let mut covered = HashSet::new();
        for (i, row) in block.iter().enumerate() {
            let vr = at.0 + i;
            if vr >= d.vrows() {
                break;
            }
            for (j, v) in row.iter().enumerate() {
                let vc = at.1 + j;
                if vc >= d.vcols.len() {
                    break;
                }
                covered.insert((vr, vc));
                items.push((d.vrow_to_data(vr), d.vcols[vc], v.clone()));
            }
        }
        if let Some(((r0, c0), (r1, c1))) = clear {
            for vr in r0..=r1.min(d.vrows().saturating_sub(1)) {
                for vc in c0..=c1.min(d.vcols.len().saturating_sub(1)) {
                    if !covered.contains(&(vr, vc)) {
                        items.push((d.vrow_to_data(vr), d.vcols[vc], String::new()));
                    }
                }
            }
        }
        d.set_cells(items);
    }

    fn table_view(&self) -> Option<Arc<dyn TableRead>> {
        let d = self.docs.get(self.active)?;
        Some(Arc::new(SubTable { inner: d.snap(), rows: d.visible.clone(), cols: d.vcols.clone() }))
    }

    fn start_aux(&mut self, ctx: &Context, label: &str, f: impl FnOnce(&std::sync::atomic::AtomicBool) -> JobResult + Send + 'static) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        if let Some(j) = &d.aux_job {
            j.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        d.aux_job = Some(jobs::spawn(ctx, label, f));
    }

    fn after_edit(&mut self, ctx: &Context) {
        if self.settings.save_after_each_edit {
            if let Some(d) = self.docs.get(self.active) {
                if d.path.is_some() && d.dirty {
                    self.save(ctx, self.active, None, false);
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // saving
    // ------------------------------------------------------------------

    fn save(&mut self, ctx: &Context, i: usize, target: Option<PathBuf>, visible_only: bool) {
        let Some(d) = self.docs.get(i) else { return };
        let target = match target.or_else(|| if visible_only { None } else { d.path.clone() }) {
            Some(t) => t,
            None => {
                let mut dlg = rfd::FileDialog::new().add_filter("CSV", &["csv", "tsv", "txt"]).set_file_name(d.title());
                if let Some(dir) = d.path.as_ref().and_then(|p| p.parent()) {
                    dlg = dlg.set_directory(dir);
                }
                match dlg.save_file() {
                    Some(p) => p,
                    None => return,
                }
            }
        };
        let d = &self.docs[i];
        if !d.ready() {
            self.msg("Still indexing — save again in a moment");
            return;
        }
        let src = &d.src;
        let o = d.save_opts.clone();
        let raw_ok = d.cols_identity()
            && !src.transcoded
            && src.quotes
            && o.delimiter == src.delimiter
            && o.encoding == src.encoding
            && o.quoting == Quoting::AsNeeded;
        let plan = SavePlan {
            opts: o,
            raw_ok,
            cols_identity: d.cols_identity(),
            subset: visible_only.then(|| (d.visible.clone(), d.vcols.clone())),
        };
        let snap = d.snap();
        let replace_doc = !visible_only;
        let t2 = target.clone();
        let job = jobs::spawn(ctx, "Saving", move |cancel| match jobs::save(&snap, &plan, &t2, cancel) {
            Ok(tmp) => JobResult::Saved { tmp, target: t2, replace_doc },
            Err(e) => JobResult::Error(format!("Save failed: {e}")),
        });
        self.docs[i].aux_job = Some(job);
    }

    fn finish_save(&mut self, ctx: &Context, i: usize, tmp: PathBuf, target: PathBuf, replace_doc: bool) {
        if !replace_doc {
            match std::fs::rename(&tmp, &target) {
                Ok(()) => self.msg(format!("Saved visible data to {}", target.display())),
                Err(e) => self.msg(format!("Could not write {}: {e}", target.display())),
            }
            return;
        }
        // Release the memory map before replacing the file (required on Windows).
        let old = self.docs.remove(i);
        let state = (
            old.has_header,
            old.read_only,
            old.grid.top,
            old.grid.cur,
            old.cols.iter().map(|id| old.widths.get(id).copied()).collect::<Vec<_>>(),
            old.filters
                .iter()
                .map(|f| (f.col.and_then(|c| old.cols.iter().position(|&x| x == c)), f.text.clone()))
                .collect::<Vec<_>>(),
            old.hidden_cols.iter().filter_map(|c| old.cols.iter().position(|x| x == c)).collect::<Vec<_>>(),
            old.save_opts.clone(),
        );
        drop(old);
        let renamed = std::fs::rename(&tmp, &target);
        if let Err(e) = &renamed {
            self.msg(format!("Could not replace file ({e}). Your data is in {}", tmp.display()));
        }
        let path = if renamed.is_ok() { target.clone() } else { tmp.clone() };
        let opts = LoadOpts { delimiter: Some(state.7.delimiter), encoding: Some(state.7.encoding), quotes: true };
        match Document::open(&path, opts, ctx) {
            Ok(mut d) => {
                d.has_header = state.0;
                d.read_only = state.1;
                d.grid.top = state.2;
                d.grid.cur = state.3;
                d.grid.anchor = state.3;
                for (k, w) in state.4.iter().enumerate() {
                    if let Some(w) = w {
                        d.widths.insert(k as u32, *w);
                    }
                }
                d.widths_measured = true;
                d.filters = state.5.into_iter().map(|(c, text)| Filter { col: c.map(|c| c as u32), text }).collect();
                d.hidden_cols = state.6.into_iter().map(|c| c as u32).collect();
                d.view_dirty = !d.filters.is_empty();
                d.save_opts = state.7;
                if !d.hidden_cols.is_empty() {
                    // vcols are refreshed once the column count is known.
                    d.widths_measured = false;
                }
                self.docs.insert(i, d);
                self.active = i;
                if renamed.is_ok() {
                    self.msg(format!("Saved {}", target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
                    self.settings.push_recent(&target);
                    // Part of "Save" in the unsaved-changes dialog: close the tab (and quit) once saved.
                    if let Some((waiting, quit)) = &mut self.close_after_save {
                        if waiting.remove(&target) {
                            self.docs.remove(i);
                            self.active = self.active.min(self.docs.len().saturating_sub(1));
                            if waiting.is_empty() {
                                let quit = *quit;
                                self.close_after_save = None;
                                if quit {
                                    self.allow_quit = true;
                                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                                }
                            }
                        }
                    }
                } else {
                    self.close_after_save = None;
                }
            }
            Err(e) => self.msg(format!("Saved, but reopening failed: {e}")),
        }
    }

    // ------------------------------------------------------------------
    // view maintenance & job polling
    // ------------------------------------------------------------------

    fn update_views(&mut self, ctx: &Context) {
        let mut errors = Vec::new();
        for d in &mut self.docs {
            if !d.view_dirty || !d.ready() {
                continue;
            }
            d.view_dirty = false;
            if !d.has_view_filter() {
                d.visible = None;
                if let Some(j) = d.view_job.take() {
                    j.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                d.clamp_cursor();
                continue;
            }
            let mut parsed = Vec::new();
            let mut bad = Vec::new();
            for (k, f) in d.filters.iter().enumerate() {
                match Query::parse(&f.text) {
                    Ok(q) => parsed.push((q, f.col.and_then(|c| d.cols.iter().position(|&x| x == c)))),
                    Err(e) => {
                        errors.push(format!("Filter \"{}\": {e}", f.text));
                        bad.push(k);
                    }
                }
            }
            for k in bad.into_iter().rev() {
                d.filters.remove(k);
            }
            let snap = d.snap();
            let hidden = d.hidden_rows.clone();
            if let Some(j) = d.view_job.take() {
                j.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            d.view_job = Some(jobs::spawn(ctx, "Filtering", move |c| JobResult::Visible(jobs::compute_visible(&snap, &parsed, &hidden, c))));
        }
        if let Some(e) = errors.pop() {
            self.msg(e);
        }
    }

    fn poll_jobs(&mut self, ctx: &Context) {
        let mut results = Vec::new();
        for (i, d) in self.docs.iter_mut().enumerate() {
            for slot in [&mut d.view_job, &mut d.aux_job] {
                if let Some(j) = slot {
                    match j.rx.try_recv() {
                        Ok(r) => {
                            *slot = None;
                            results.push((i, r));
                        }
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => *slot = None,
                        Err(_) => {}
                    }
                }
            }
        }
        for (i, r) in results {
            self.handle_result(ctx, i, r);
        }
    }

    fn handle_result(&mut self, ctx: &Context, i: usize, r: JobResult) {
        match r {
            JobResult::Saved { tmp, target, replace_doc } => {
                self.finish_save(ctx, i, tmp, target, replace_doc);
                return;
            }
            JobResult::Error(e) => {
                self.msg(e);
                self.close_after_save = None; // a failed save cancels "save and close"
                return;
            }
            JobResult::Done(m) => {
                self.msg(m);
                return;
            }
            _ => {}
        }
        let Some(d) = self.docs.get_mut(i) else { return };
        let mut message = None;
        match r {
            JobResult::Visible(v) => {
                d.visible = v.map(Arc::new);
                d.clamp_cursor();
                d.grid.top = d.grid.top.min(d.vrows().saturating_sub(1));
            }
            JobResult::Sorted { rows, col, asc } => {
                d.set_rows(rows, Vec::new());
                d.sort = Some((col, asc));
            }
            JobResult::Found(hit) => match hit {
                Some((r, c)) => d.select((r, c), (r, c)),
                None => message = Some("No matches".to_string()),
            },
            JobResult::FoundCount(n) => {
                self.find_count = Some(n);
                message = Some(format!("{} matches", fmt_num(n)));
            }
            JobResult::Cells(items) => {
                let n = items.len();
                d.set_cells_t(items);
                message = Some(format!("Replaced {} cells", fmt_num(n)));
            }
            JobResult::RemoveRows(rows) => {
                let n = rows.len();
                d.remove_rows(&rows.into_iter().collect());
                message = Some(format!("Removed {} rows", fmt_num(n)));
            }
            JobResult::HideRows(ids) => {
                let n = ids.len();
                d.hidden_rows.extend(ids);
                d.view_dirty = true;
                message = Some(format!("Hid {} rows", fmt_num(n)));
            }
            JobResult::SelectRows(rows) => {
                // Single-rectangle selection: show only the matching rows instead.
                let keep: HashSet<usize> = rows.iter().copied().collect();
                let ids: Vec<u32> = (0..d.nrows()).filter(|r| !keep.contains(r)).map(|r| d.data_row_id(r)).collect();
                d.hidden_rows.extend(ids);
                d.view_dirty = true;
                message = Some(format!("Showing {} matching rows (Show All Hidden Rows to restore)", fmt_num(rows.len())));
            }
            JobResult::RemoveCols(cols) => {
                let n = cols.len();
                d.remove_cols(&cols.into_iter().collect());
                message = Some(format!("Removed {n} columns"));
            }
            JobResult::HideCols(ids) => {
                let n = ids.len();
                d.hidden_cols.extend(ids);
                d.refresh_vcols();
                d.clamp_cursor();
                message = Some(format!("Hid {n} columns"));
            }
            JobResult::SelectCols(cols) => {
                let keep: HashSet<usize> = cols.iter().copied().collect();
                let ids: Vec<u32> = (0..d.ncols()).filter(|c| !keep.contains(c)).map(|c| d.cols[c]).collect();
                d.hidden_cols.extend(ids);
                d.refresh_vcols();
                d.clamp_cursor();
                message = Some(format!("Showing {} matching columns (Show All Hidden Columns to restore)", cols.len()));
            }
            _ => {}
        }
        if let Some(m) = message {
            self.msg(m);
        }
    }

    fn check_external_changes(&mut self, ctx: &Context) {
        if self.last_mtime_check.elapsed() < Duration::from_secs(2) || self.settings.external_change == ExternalChange::Ignore {
            return;
        }
        self.last_mtime_check = Instant::now();
        for i in 0..self.docs.len() {
            let d = &self.docs[i];
            let Some(p) = &d.path else { continue };
            if d.aux_job.is_some() {
                continue;
            }
            let Ok(m) = std::fs::metadata(p).and_then(|m| m.modified()) else { continue };
            if d.mtime.is_some_and(|old| old != m) {
                self.docs[i].mtime = Some(m);
                if self.settings.external_change == ExternalChange::Refresh && !self.docs[i].dirty {
                    self.reload(ctx, i, None);
                    self.msg("File changed on disk — reloaded");
                } else if self.dialog.is_none() {
                    self.dialog = Some(Dialog::ExternalChanged(i));
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // commands
    // ------------------------------------------------------------------

    fn ask(&mut self, title: &str, params: Vec<Param>, action: PromptAction) {
        let values = params.iter().map(|p| (p.key.to_string(), p.default.to_string())).collect();
        self.prompt = Some(Prompt { title: title.into(), params, values, action, focus: true });
    }

    fn sel_count_rows(&self) -> usize {
        self.docs.get(self.active).map_or(1, |d| d.sel_data_rows().len().max(1))
    }

    fn contiguous_rows(&mut self) -> Option<std::ops::Range<usize>> {
        let d = self.docs.get(self.active)?;
        let rows = d.sel_data_rows();
        let (&a, &b) = (rows.first()?, rows.last()?);
        if b + 1 - a != rows.len() {
            self.msg("Selection spans filtered-out rows — clear filters first");
            return None;
        }
        Some(a..b + 1)
    }

    fn contiguous_cols(&mut self) -> Option<std::ops::Range<usize>> {
        let d = self.docs.get(self.active)?;
        let cols = d.sel_cols();
        let (&a, &b) = (cols.first()?, cols.last()?);
        if b + 1 - a != cols.len() {
            self.msg("Selection spans hidden columns — show them first");
            return None;
        }
        Some(a..b + 1)
    }

    pub fn run(&mut self, ctx: &Context, cmd: Cmd) {
        use Cmd::*;
        let has_doc = !self.docs.is_empty();
        match cmd {
            // ---------------- File ----------------
            NewFile => {
                self.docs.push(Document::new_empty(50, 10));
                self.active = self.docs.len() - 1;
            }
            OpenFile => {
                let mut dlg = rfd::FileDialog::new()
                    .add_filter("Delimited files", &["csv", "tsv", "txt", "tab", "psv", "dsv", "dat"])
                    .add_filter("JSON / XML", &["json", "xml"])
                    .add_filter("All files", &["*"]);
                if let Some(dir) = self.docs.get(self.active).and_then(|d| d.path.as_ref()).and_then(|p| p.parent()) {
                    dlg = dlg.set_directory(dir);
                }
                if let Some(paths) = dlg.pick_files() {
                    for p in paths {
                        self.open_path(ctx, &p);
                    }
                }
            }
            OpenFolder => {
                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                    let s = dir.to_string_lossy().into_owned();
                    if !self.settings.folders.contains(&s) {
                        self.settings.folders.push(s);
                    }
                    self.settings.show_sidebar = true;
                    self.settings.save();
                    self.rescan_folders();
                }
            }
            CloseFolder => {
                self.settings.folders.pop();
                self.settings.save();
                self.rescan_folders();
            }
            GoToFile => self.palette = Some(Palette { query: String::new(), sel: 0, files: true, focus: true }),
            SaveFile if has_doc => {
                if self.docs[self.active].read_only {
                    self.msg("Read-only file: use Save File As");
                } else {
                    self.save(ctx, self.active, None, false);
                }
            }
            SaveAs if has_doc => {
                let d = &self.docs[self.active];
                let mut dlg = rfd::FileDialog::new().set_file_name(d.title());
                if let Some(dir) = d.path.as_ref().and_then(|p| p.parent()) {
                    dlg = dlg.set_directory(dir);
                }
                if let Some(p) = dlg.save_file() {
                    self.save(ctx, self.active, Some(p), false);
                }
            }
            SaveAll => {
                for i in 0..self.docs.len() {
                    if self.docs[i].dirty && self.docs[i].path.is_some() && !self.docs[i].read_only {
                        self.save(ctx, i, None, false);
                    }
                }
            }
            SaveVisibleAs if has_doc => {
                if let Some(p) = rfd::FileDialog::new().set_file_name(format!("visible-{}", self.docs[self.active].title())).save_file() {
                    self.save(ctx, self.active, Some(p), true);
                }
            }
            SaveParams | ReloadParams | ToggleMetadata => {
                self.settings.show_metadata = !self.settings.show_metadata || cmd != ToggleMetadata;
            }
            Reload if has_doc => self.reload(ctx, self.active, None),
            ToggleReadOnly if has_doc => {
                let d = &mut self.docs[self.active];
                d.read_only = !d.read_only;
                let m = if d.read_only { "Read-only on" } else { "Read-only off" };
                self.msg(m);
            }
            ImportJsonXml => {
                if let Some(p) = rfd::FileDialog::new().add_filter("JSON / XML", &["json", "xml"]).pick_file() {
                    self.dialog = Some(Dialog::Import { path: p, preserve: false });
                }
            }
            ExportJsonXml if has_doc => self.dialog = Some(Dialog::Export { format: ExportFormat::Json, typed: false, visible_only: true }),
            ExportPdf | Print if has_doc => {
                self.dialog = Some(Dialog::Pdf { opts: PdfOptions::default(), selection_only: false, print: cmd == Print, title: true })
            }
            DuplicateFile | RenameFile if has_doc => {
                let Some(p) = self.docs[self.active].path.clone() else {
                    self.msg("Save the file first");
                    return;
                };
                let name: &'static str = Box::leak(p.file_name().unwrap_or_default().to_string_lossy().into_owned().into_boxed_str());
                let (title, action) = if cmd == DuplicateFile { ("Duplicate File", PromptAction::DuplicateFile) } else { ("Rename File", PromptAction::RenameFile) };
                self.ask(title, vec![param("name", "New file name", ParamKind::Text, name)], action);
            }
            MoveFile if has_doc => {
                let Some(p) = self.docs[self.active].path.clone() else { return };
                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                    let target = dir.join(p.file_name().unwrap_or_default());
                    self.move_file(ctx, &p, &target, false);
                }
            }
            DeleteFile if has_doc && self.docs[self.active].path.is_some() => self.dialog = Some(Dialog::DeleteFile(self.active)),
            ShowInFileSystem | OpenInTextEditor | OpenTerminal if has_doc => {
                let Some(p) = self.docs[self.active].path.clone() else { return };
                let dir = p.parent().map(Path::to_path_buf).unwrap_or_default();
                let r = match cmd {
                    ShowInFileSystem => reveal(&p),
                    OpenInTextEditor => open_in_editor(&p, &self.settings.text_editor),
                    _ => open_terminal(&dir),
                };
                if let Err(e) = r {
                    self.msg(e);
                }
            }
            CopyFileName | CopyFilePath | CopyFolderPath if has_doc => {
                if let Some(p) = &self.docs[self.active].path {
                    let s = match cmd {
                        CopyFileName => p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                        CopyFilePath => std::fs::canonicalize(p).unwrap_or(p.clone()).to_string_lossy().into_owned(),
                        _ => p.parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default(),
                    };
                    ctx.copy_text(s);
                    self.msg("Copied");
                }
            }
            TabLeft if has_doc => self.active = (self.active + self.docs.len() - 1) % self.docs.len(),
            TabRight if has_doc => self.active = (self.active + 1) % self.docs.len(),
            CloseFile if has_doc => self.close_tabs(vec![self.active]),
            CloseAll => self.close_tabs((0..self.docs.len()).collect()),
            CloseOthers => self.close_tabs((0..self.docs.len()).filter(|&i| i != self.active).collect()),
            Exit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),

            // ---------------- Edit ----------------
            Undo => {
                if let Some(d) = self.doc() {
                    if !d.undo() {
                        self.msg("Nothing to undo");
                    }
                }
            }
            Redo => {
                if let Some(d) = self.doc() {
                    if !d.redo() {
                        self.msg("Nothing to redo");
                    }
                }
            }
            Copy | CopyWithHeader | Cut if has_doc => {
                let r = grid::copy_selection(ctx, &self.docs[self.active], cmd == CopyWithHeader);
                match r {
                    Err(e) => self.msg(e),
                    Ok(()) if cmd == Cut => {
                        if let Some(d) = self.doc_edit() {
                            grid::clear_selection(d);
                        }
                    }
                    Ok(()) => {}
                }
            }
            Paste => {
                // Clipboard reads arrive as Event::Paste; ask the OS via the viewport.
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            }
            Clear => {
                if let Some(d) = self.doc_edit() {
                    grid::clear_selection(d);
                }
            }
            EditCell => {
                if let Some(d) = self.doc() {
                    grid::start_edit(d, None);
                }
            }
            Find => {
                self.find_open = true;
                self.find_focus = true;
                self.prefill_find();
            }
            Replace => {
                self.find_open = true;
                self.replace_open = true;
                self.find_focus = true;
                self.prefill_find();
            }
            FindNext | FindPrev => self.find_step(ctx, cmd == FindNext),
            FindAll => self.find_all(ctx),
            ReplaceNext => self.replace_next(ctx),
            ReplaceAll => self.replace_all(ctx),

            // ---------------- Select ----------------
            SelectAll => {
                if let Some(d) = self.doc() {
                    let (r, c) = (d.vrows().saturating_sub(1), d.vcols.len().saturating_sub(1));
                    d.grid.anchor = (0, 0);
                    d.grid.cur = (r, c);
                }
            }
            SelectRows => {
                if let Some(d) = self.doc() {
                    let ((r0, _), (r1, _)) = d.sel_range();
                    d.grid.anchor = (r0, 0);
                    d.grid.cur = (r1, d.vcols.len().saturating_sub(1));
                }
            }
            SelectCols => {
                if let Some(d) = self.doc() {
                    let ((_, c0), (_, c1)) = d.sel_range();
                    d.grid.anchor = (0, c0);
                    d.grid.cur = (d.vrows().saturating_sub(1), c1);
                }
            }
            SelectTo => self.ask("Select From Main Cell To…", vec![
                param("row", "Row number", ParamKind::Int, ""),
                param("col", "Column (letter or number)", ParamKind::Text, ""),
            ], PromptAction::SelectTo),
            GoToCell => self.ask("Go to Cell", vec![
                param("row", "Row number (blank = keep)", ParamKind::Text, ""),
                param("col", "Column letter/number (blank = keep)", ParamKind::Text, ""),
            ], PromptAction::GoToCell),
            GoTop | GoBottom | GoFarLeft | GoFarRight => {
                if let Some(d) = self.doc() {
                    let (mut r, mut c) = d.grid.cur;
                    match cmd {
                        GoTop => r = 0,
                        GoBottom => r = d.vrows().saturating_sub(1),
                        GoFarLeft => c = 0,
                        _ => c = d.vcols.len().saturating_sub(1),
                    }
                    d.select((r, c), (r, c));
                }
            }
            SelectDupRows | SelectDupRowsSelCols | RemoveDupRows | RemoveDupRowsSelCols if has_doc => {
                if !self.docs[self.active].ready() {
                    return;
                }
                let d = &self.docs[self.active];
                let cols = if matches!(cmd, SelectDupRowsSelCols | RemoveDupRowsSelCols) { d.sel_cols() } else { Vec::new() };
                let snap = d.snap();
                let remove = matches!(cmd, RemoveDupRows | RemoveDupRowsSelCols);
                if remove && self.doc_edit().is_none() {
                    return;
                }
                self.start_aux(ctx, "Finding duplicates", move |c| {
                    let rows = jobs::duplicate_rows(&snap, &cols, c);
                    if remove { JobResult::RemoveRows(rows) } else { JobResult::SelectRows(rows) }
                });
            }
            SelectDupCols | RemoveDupCols | RemoveDupColsSelRows if has_doc => {
                if (cmd != SelectDupCols && self.doc_edit().is_none()) || !self.docs[self.active].ready() {
                    return;
                }
                let d = &self.docs[self.active];
                let rows = (cmd == RemoveDupColsSelRows).then(|| d.sel_data_rows());
                let snap = d.snap();
                let remove = cmd != SelectDupCols;
                self.start_aux(ctx, "Finding duplicate columns", move |c| {
                    let cols = jobs::duplicate_cols(&snap, rows.as_deref(), c);
                    if remove {
                        JobResult::RemoveCols(cols)
                    } else {
                        let dups: HashSet<usize> = cols.iter().copied().collect();
                        // Show originals and their duplicates.
                        let all: Vec<usize> = (0..snap.cols.len()).filter(|c| dups.contains(c)).collect();
                        JobResult::SelectCols(all)
                    }
                });
            }

            // ---------------- Insert ----------------
            InsertRowsBefore | InsertRowsAfter => {
                let n = self.sel_count_rows();
                let Some(d) = self.doc_edit() else { return };
                let rows = d.sel_data_rows();
                let at = if cmd == InsertRowsBefore { rows.first().copied().unwrap_or(0) } else { rows.last().map_or(d.nrows(), |r| r + 1) };
                d.insert_rows(at, n);
                d.view_dirty = true;
            }
            InsertRowsMulti => self.ask("Insert Rows Before", vec![param("n", "Number of rows", ParamKind::Int, "10")], PromptAction::InsertRows),
            InsertColsBefore | InsertColsAfter => {
                let Some(d) = self.doc_edit() else { return };
                let cols = d.sel_cols();
                let n = cols.len().max(1);
                let at = if cmd == InsertColsBefore { cols.first().copied().unwrap_or(0) } else { cols.last().map_or(d.ncols(), |c| c + 1) };
                d.insert_cols(at, n);
            }
            InsertColsMulti => self.ask("Insert Columns Before", vec![param("n", "Number of columns", ParamKind::Int, "5")], PromptAction::InsertCols),
            InsertCellsDown | InsertCellsRight | RemoveCellsUp | RemoveCellsLeft => self.shift_cells(cmd),
            AddIdColumn => self.add_id_column(),

            // ---------------- Remove ----------------
            RemoveRows => {
                let Some(d) = self.doc_edit() else { return };
                let rows: HashSet<usize> = d.sel_data_rows().into_iter().collect();
                d.remove_rows(&rows);
                d.grid.anchor = d.grid.cur;
            }
            RemoveCols => {
                let Some(d) = self.doc_edit() else { return };
                let cols: HashSet<usize> = d.sel_cols().into_iter().collect();
                if cols.len() >= d.ncols() {
                    self.msg("Can't remove every column");
                    return;
                }
                d.remove_cols(&cols);
                d.clamp_cursor();
                d.grid.anchor = d.grid.cur;
            }
            RemoveEmptyRows | RemoveEmptyRowsSelCols | HideEmptyRows | RemoveExtraneous if has_doc => {
                let hide = cmd == HideEmptyRows;
                if !hide && self.doc_edit().is_none() {
                    return;
                }
                let d = &self.docs[self.active];
                if !d.ready() {
                    return;
                }
                let cols = if cmd == RemoveEmptyRowsSelCols { d.sel_cols() } else { Vec::new() };
                let snap = d.snap();
                let extraneous = cmd == RemoveExtraneous;
                self.start_aux(ctx, "Finding empty rows", move |c| {
                    let mut rows = jobs::empty_rows(&snap, &cols, c);
                    if extraneous {
                        let n = snap.total.saturating_sub(snap.hdr);
                        let mut trailing = Vec::new();
                        let mut expect = n;
                        for &r in rows.iter().rev() {
                            if r + 1 == expect {
                                trailing.push(r);
                                expect = r;
                            } else {
                                break;
                            }
                        }
                        rows = trailing;
                    }
                    if hide {
                        JobResult::HideRows(rows.into_iter().map(|d| snap.row_id(d + snap.hdr)).collect())
                    } else {
                        JobResult::RemoveRows(rows)
                    }
                });
                if extraneous {
                    // Trailing empty columns are cheap enough to compute right away.
                    let d = &self.docs[self.active];
                    let snap = d.snap();
                    let empty = jobs::empty_cols(&snap, None, &std::sync::atomic::AtomicBool::new(false));
                    let mut trailing = HashSet::new();
                    for c in (0..d.ncols()).rev() {
                        if empty.contains(&c) {
                            trailing.insert(c);
                        } else {
                            break;
                        }
                    }
                    if !trailing.is_empty() && trailing.len() < d.ncols() {
                        self.docs[self.active].remove_cols(&trailing);
                    }
                }
            }
            RemoveEmptyCols | RemoveEmptyColsSelRows | HideEmptyCols if has_doc => {
                let hide = cmd == HideEmptyCols;
                if !hide && self.doc_edit().is_none() {
                    return;
                }
                let d = &self.docs[self.active];
                if !d.ready() {
                    return;
                }
                let rows = (cmd == RemoveEmptyColsSelRows).then(|| d.sel_data_rows());
                let snap = d.snap();
                self.start_aux(ctx, "Finding empty columns", move |c| {
                    let cols = jobs::empty_cols(&snap, rows.as_deref(), c);
                    if hide { JobResult::HideCols(cols.iter().map(|&c| snap.cols[c]).collect()) } else { JobResult::RemoveCols(cols) }
                });
            }
            RemoveFilteredHidden => {
                let Some(d) = self.doc_edit() else { return };
                if let Some(v) = d.visible.clone() {
                    let keep: HashSet<u32> = v.iter().copied().collect();
                    let gone: HashSet<usize> = (0..d.nrows()).filter(|r| !keep.contains(&(*r as u32))).collect();
                    d.remove_rows(&gone);
                }
                let hidden: HashSet<usize> = (0..d.ncols()).filter(|&c| d.hidden_cols.contains(&d.cols[c])).collect();
                if !hidden.is_empty() {
                    d.remove_cols(&hidden);
                }
                d.filters.clear();
                d.hidden_rows.clear();
                d.hidden_cols.clear();
                d.refresh_vcols();
                d.view_dirty = true;
            }

            // ---------------- Duplicate / move ----------------
            DuplicateRows => {
                if let Some(r) = self.contiguous_rows() {
                    if let Some(d) = self.doc_edit() {
                        d.duplicate_rows(r, 1);
                    }
                }
            }
            DuplicateRowsMulti => self.ask("Duplicate Rows", vec![param("n", "Number of copies", ParamKind::Int, "2")], PromptAction::DupRows),
            DuplicateCols => {
                if let Some(r) = self.contiguous_cols() {
                    if let Some(d) = self.doc_edit() {
                        d.duplicate_cols(r, 1);
                    }
                }
            }
            DuplicateColsMulti => self.ask("Duplicate Columns", vec![param("n", "Number of copies", ParamKind::Int, "2")], PromptAction::DupCols),
            DupDown | DupUp => {
                let Some((block, (r0, c0), (r1, c1))) = self.sel_block() else { return };
                if self.doc_edit().is_none() {
                    return;
                }
                if r0 == r1 {
                    let target = if cmd == DupDown { r0 + 1 } else { r0.wrapping_sub(1) };
                    if target < self.docs[self.active].vrows() {
                        self.write_block((target, c0), &block, None);
                        self.docs[self.active].select((target, c1), (target, c0));
                    }
                } else {
                    let src = if cmd == DupDown { block[0].clone() } else { block[block.len() - 1].clone() };
                    let filled: Vec<Vec<String>> = (r0..=r1).map(|_| src.clone()).collect();
                    self.write_block((r0, c0), &filled, None);
                }
            }
            MoveCellUp | MoveCellDown | MoveCellLeft | MoveCellRight => self.move_cells(cmd),
            MoveRowsUp | MoveRowsDown => {
                let Some(r) = self.contiguous_rows() else { return };
                let Some(d) = self.doc_edit() else { return };
                if d.has_view_filter() {
                    self.msg("Clear filters before moving rows");
                    return;
                }
                let n = r.len();
                if cmd == MoveRowsUp && r.start > 0 {
                    d.move_rows(r.clone(), r.start - 1);
                    d.grid.cur.0 -= 1;
                    d.grid.anchor.0 -= 1;
                } else if cmd == MoveRowsDown && r.end < d.nrows() {
                    d.move_rows(r.clone(), r.start + 1);
                    d.grid.cur.0 += 1;
                    d.grid.anchor.0 += 1;
                }
                let _ = n;
                d.ensure_visible();
            }
            MoveColsLeft | MoveColsRight => {
                let Some(r) = self.contiguous_cols() else { return };
                let Some(d) = self.doc_edit() else { return };
                if !d.hidden_cols.is_empty() {
                    self.msg("Show hidden columns before moving columns");
                    return;
                }
                if cmd == MoveColsLeft && r.start > 0 {
                    d.move_cols(r.clone(), r.start - 1);
                    d.grid.cur.1 -= 1;
                    d.grid.anchor.1 -= 1;
                } else if cmd == MoveColsRight && r.end < d.ncols() {
                    d.move_cols(r.clone(), r.start + 1);
                    d.grid.cur.1 += 1;
                    d.grid.anchor.1 += 1;
                }
                d.ensure_visible();
            }
            MoveRowsAbove => self.ask("Move Rows Just Above Row", vec![param("row", "Row number", ParamKind::Int, "1")], PromptAction::MoveRowsAbove),
            MoveColsLeftOf => self.ask("Move Columns Left Of", vec![param("col", "Column letter/number", ParamKind::Text, "A")], PromptAction::MoveColsLeftOf),

            // ---------------- Sort ----------------
            SortColAsc | SortColDesc if has_doc => {
                let d = &self.docs[self.active];
                let c = d.vcol_to_col(d.grid.cur.1);
                if c < d.ncols() {
                    self.sort_by(ctx, c, cmd == SortColAsc);
                }
            }
            SortRowAsc | SortRowDesc => {
                let Some(d) = self.doc_edit() else { return };
                if d.nrows() == 0 {
                    return;
                }
                let dr = d.vrow_to_data(d.grid.cur.0);
                let vals = d.snap().row_values(d.data_row_id(dr));
                let order = jobs::sort_order(&vals, cmd == SortRowAsc);
                let cols: Vec<u32> = order.into_iter().map(|i| d.cols[i]).collect();
                d.set_cols(cols, Vec::new());
            }
            SortCellsVAsc | SortCellsVDesc | SortCellsHAsc | SortCellsHDesc => {
                let Some((mut block, (r0, c0), _)) = self.sel_block() else { return };
                if self.doc_edit().is_none() {
                    return;
                }
                let asc = matches!(cmd, SortCellsVAsc | SortCellsHAsc);
                if matches!(cmd, SortCellsHAsc | SortCellsHDesc) {
                    for row in &mut block {
                        let order = jobs::sort_order(row, asc);
                        *row = order.into_iter().map(|i| row[i].clone()).collect();
                    }
                } else if let Some(w) = block.first().map(|r| r.len()) {
                    for c in 0..w {
                        let col: Vec<String> = block.iter().map(|r| r[c].clone()).collect();
                        for (i, k) in jobs::sort_order(&col, asc).into_iter().enumerate() {
                            block[i][c] = col[k].clone();
                        }
                    }
                }
                self.write_block((r0, c0), &block, None);
            }

            // ---------------- Filter / hide ----------------
            AddFilter => {
                if has_doc {
                    self.dialog = Some(Dialog::Filters { col: None, text: String::new(), focus: true });
                }
            }
            FilterSelCol | ViewFilters if has_doc => {
                let d = &self.docs[self.active];
                let col = (cmd == FilterSelCol).then(|| d.vcol_to_col(d.grid.cur.1)).filter(|&c| c < d.ncols());
                self.dialog = Some(Dialog::Filters { col, text: String::new(), focus: true });
            }
            FilterSelContents if has_doc => {
                let d = &mut self.docs[self.active];
                if d.vrows() == 0 {
                    return;
                }
                let dr = d.vrow_to_data(d.grid.cur.0);
                for c in d.sel_cols() {
                    let v = d.cell(dr, c);
                    let text = format!("={}", escape_value(&v));
                    d.filters.push(Filter { col: Some(d.cols[c]), text });
                }
                d.view_dirty = true;
            }
            FilterValues | UniqueValues if has_doc => self.open_analysis(AnalysisTab::UniqueValues, Some(vec![self.docs[self.active].grid.cur.1]), None, true),
            RefreshFilters if has_doc => self.docs[self.active].view_dirty = true,
            RemoveFilters if has_doc => {
                let d = &mut self.docs[self.active];
                d.filters.clear();
                d.view_dirty = true;
            }
            HideRows | HideAllButRows if has_doc => {
                let d = &mut self.docs[self.active];
                let sel: HashSet<usize> = d.sel_data_rows().into_iter().collect();
                if cmd == HideRows {
                    let ids: Vec<u32> = sel.iter().map(|&r| d.data_row_id(r)).collect();
                    d.hidden_rows.extend(ids);
                } else {
                    let ids: Vec<u32> = (0..d.nrows()).filter(|r| !sel.contains(r)).map(|r| d.data_row_id(r)).collect();
                    d.hidden_rows.extend(ids);
                }
                d.view_dirty = true;
                d.grid.anchor = d.grid.cur;
            }
            HideCols | HideAllButCols if has_doc => {
                let d = &mut self.docs[self.active];
                let sel: HashSet<usize> = d.sel_cols().into_iter().collect();
                let ids: Vec<u32> = (0..d.ncols()).filter(|c| sel.contains(c) == (cmd == HideCols)).map(|c| d.cols[c]).collect();
                d.hidden_cols.extend(ids);
                d.refresh_vcols();
                d.clamp_cursor();
                d.grid.anchor = d.grid.cur;
            }
            ShowAllRows if has_doc => {
                let d = &mut self.docs[self.active];
                d.hidden_rows.clear();
                d.view_dirty = true;
            }
            ShowAllCols if has_doc => {
                let d = &mut self.docs[self.active];
                d.hidden_cols.clear();
                d.refresh_vcols();
            }

            // ---------------- Transform ----------------
            Transform => self.palette = Some(Palette { query: "transform ".into(), sel: 0, files: false, focus: true }),
            TransposeTable => {
                let Some(d) = self.doc_edit() else { return };
                let (rows, cols) = (d.total_rows(), d.ncols());
                if rows * cols > MAX_BLOCK {
                    self.msg("Table too large to transpose (limit 5M cells)");
                    return;
                }
                let snap = d.snap();
                let grid: Vec<Vec<String>> = (0..rows).map(|t| snap.row_values(snap.row_id(t))).collect();
                let t: Vec<Vec<String>> = (0..cols).map(|c| grid.iter().map(|r| r[c].clone()).collect()).collect();
                d.filters.clear();
                d.hidden_rows.clear();
                d.hidden_cols.clear();
                d.replace_all(t);
                d.select((0, 0), (0, 0));
            }

            // ---------------- Headers ----------------
            ToggleHeader | RemoveHeader if has_doc => {
                let d = &mut self.docs[self.active];
                d.has_header = cmd == ToggleHeader && !d.has_header;
                d.view_dirty = true;
                d.visible = None;
                d.clamp_cursor();
            }
            SetHeaderRow => {
                let Some(d) = self.doc_edit() else { return };
                if d.nrows() == 0 {
                    return;
                }
                let dr = d.vrow_to_data(d.grid.cur.0);
                if !d.has_header {
                    d.has_header = true;
                    if dr > 0 {
                        d.move_rows(dr - 1..dr, 0);
                    }
                } else {
                    // Move the chosen row to the top; old header becomes data.
                    let h = d.hdr();
                    let mut rows: Vec<u32> = (0..d.total_rows()).map(|t| d.row_id(t)).collect();
                    let r = rows.remove(dr + h);
                    rows.insert(0, r);
                    d.set_rows(rows, Vec::new());
                }
                d.view_dirty = true;
            }

            // ---------------- Analysis ----------------
            OpenAnalysis | StatsAll if has_doc => self.open_analysis(AnalysisTab::Statistics, None, None, true),
            StatsSelCols if has_doc => {
                let cols = self.sel_vcols();
                self.open_analysis(AnalysisTab::Statistics, Some(cols), None, true)
            }
            StatsSelCells | StatsSelCellsByCols if has_doc => {
                let d = &self.docs[self.active];
                let ((r0, c0), (r1, c1)) = d.sel_range();
                let r1 = r1.min(d.vrows().saturating_sub(1));
                if (r1 + 1 - r0) * (c1 + 1 - c0) > MAX_BLOCK {
                    self.msg("Selection too large");
                    return;
                }
                let cells: Vec<(usize, usize)> = (r0..=r1).flat_map(|r| (c0..=c1).map(move |c| (r, c))).collect();
                let cols = (c0..=c1).collect();
                self.open_analysis(AnalysisTab::Statistics, Some(cols), Some(cells), cmd == StatsSelCellsByCols)
            }
            ColAnalysisAll if has_doc => self.open_analysis(AnalysisTab::ColumnAnalysis, None, None, true),
            ColAnalysisSel if has_doc => {
                let cols = self.sel_vcols();
                self.open_analysis(AnalysisTab::ColumnAnalysis, Some(cols), None, true)
            }
            Pivot if has_doc => {
                let cols = self.sel_vcols();
                self.open_analysis(AnalysisTab::PivotTable, Some(cols), None, true)
            }
            ColumnLookup if has_doc => {
                let d = &self.docs[self.active];
                let dst_key = d.vcol_to_col(d.grid.cur.1).min(d.ncols().saturating_sub(1));
                let src_doc = self.active;
                let n = self.docs[src_doc].ncols();
                self.dialog = Some(Dialog::Lookup(LookupState {
                    src_doc,
                    src_key: 0,
                    src_vals: vec![false; n],
                    dst_key,
                    insert: true,
                    dst_start: d.ncols(),
                }));
            }

            // ---------------- View ----------------
            CommandPalette => self.palette = Some(Palette { query: String::new(), sel: 0, files: false, focus: true }),
            Preferences => self.dialog = Some(Dialog::Preferences),
            ToggleSidebar => self.settings.show_sidebar = !self.settings.show_sidebar,
            ToggleEditorPane => self.settings.show_editor_pane = !self.settings.show_editor_pane,
            ToggleRowLabels => self.settings.show_row_labels = !self.settings.show_row_labels,
            ToggleColLabels => self.settings.show_col_labels = !self.settings.show_col_labels,
            SetColWidth => self.ask("Set Column Width", vec![param("w", "Width in pixels", ParamKind::Int, "120")], PromptAction::ColWidth),
            AutofitCols => {
                if let Some(d) = self.doc() {
                    let cols = d.sel_cols();
                    d.grid.autofit.extend(cols);
                }
            }
            ZoomIn => self.settings.font_size = (self.settings.font_size + 1.0).min(28.0),
            ZoomOut => self.settings.font_size = (self.settings.font_size - 1.0).max(9.0),
            ThemeLight | ThemeDark | ThemeDracula | ThemeSolDark | ThemeSolLight => {
                self.settings.theme = match cmd {
                    ThemeLight => Theme::Light,
                    ThemeDark => Theme::Dark,
                    ThemeDracula => Theme::Dracula,
                    ThemeSolDark => Theme::SolarizedDark,
                    _ => Theme::SolarizedLight,
                };
                self.settings.save();
            }

            // ---------------- Web ----------------
            FollowLinks | WebSearch | EmailAddresses | MapSearch => self.web(cmd),
            About => self.dialog = Some(Dialog::About),
            _ => {}
        }
    }

    fn sel_vcols(&self) -> Vec<usize> {
        let d = &self.docs[self.active];
        let ((_, c0), (_, c1)) = d.sel_range();
        (c0..=c1.min(d.vcols.len().saturating_sub(1))).collect()
    }

    fn open_analysis(&mut self, tab: AnalysisTab, cols: Option<Vec<usize>>, cells: Option<Vec<(usize, usize)>>, by_cols: bool) {
        if let Some(t) = self.table_view() {
            self.analysis.open(tab, t, cols, cells, by_cols);
        }
    }

    fn sort_by(&mut self, ctx: &Context, col: usize, asc: bool) {
        let Some(d) = self.doc_edit() else { return };
        let snap = d.snap();
        let id = d.cols[col];
        self.start_aux(ctx, "Sorting", move |c| JobResult::Sorted { rows: jobs::sort_rows(&snap, col, asc, c), col: id, asc });
    }

    fn prefill_find(&mut self) {
        if !self.find.text.is_empty() {
            return;
        }
        if let Some(d) = self.docs.get(self.active) {
            if d.vrows() > 0 && d.grid.cur != d.grid.anchor {
                return;
            }
        }
    }

    fn find_region(&self, d: &Document) -> Region {
        Region {
            visible: d.visible.clone(),
            vcols: d.vcols.clone(),
            nrows: d.vrows(),
            rect: self.find.selection_only.then(|| d.sel_range()),
        }
    }

    fn find_step(&mut self, ctx: &Context, forward: bool) {
        let Some(re) = self.find_regex() else {
            self.find_open = true;
            self.find_focus = true;
            return;
        };
        let Some(d) = self.docs.get(self.active) else { return };
        if !d.ready() {
            self.msg("Still indexing…");
            return;
        }
        let region = self.find_region(d);
        let snap = d.snap();
        let start = d.grid.cur;
        self.start_aux(ctx, "Searching", move |c| JobResult::Found(jobs::find(&snap, &region, &re, start, forward, c)));
    }

    fn find_all(&mut self, ctx: &Context) {
        let Some(re) = self.find_regex() else { return };
        let Some(d) = self.docs.get(self.active) else { return };
        let region = self.find_region(d);
        let snap = d.snap();
        self.start_aux(ctx, "Counting", move |c| JobResult::FoundCount(jobs::count_matches(&snap, &region, &re, c)));
    }

    fn replace_next(&mut self, ctx: &Context) {
        let Some(re) = self.find_regex() else { return };
        let use_regex = self.find.regex;
        let with = self.replace_text.clone();
        let Some(d) = self.doc_edit() else { return };
        if d.vrows() > 0 && !d.vcols.is_empty() {
            let (r, c) = d.grid.cur;
            let dr = d.vrow_to_data(r);
            let col = d.vcols[c];
            let v = d.cell(dr, col);
            if re.is_match(&v) {
                let nv = if use_regex { re.replace_all(&v, with.as_str()).into_owned() } else { re.replace_all(&v, regex::NoExpand(&with)).into_owned() };
                d.set_cells(vec![(dr, col, nv)]);
            }
        }
        self.find_step(ctx, true);
    }

    fn replace_all(&mut self, ctx: &Context) {
        let Some(re) = self.find_regex() else { return };
        let use_regex = self.find.regex;
        let with = self.replace_text.clone();
        if self.doc_edit().is_none() {
            return;
        }
        let d = &self.docs[self.active];
        let region = self.find_region(d);
        let snap = d.snap();
        self.start_aux(ctx, "Replacing", move |c| JobResult::Cells(jobs::replace_all(&snap, &region, &re, &with, use_regex, c)));
    }

    fn shift_cells(&mut self, cmd: Cmd) {
        let Some(d) = self.doc_edit() else { return };
        if d.has_view_filter() || !d.hidden_cols.is_empty() {
            self.msg("Clear filters and hidden columns first");
            return;
        }
        let ((r0, c0), (r1, c1)) = d.sel_range();
        let (h, w) = (r1 + 1 - r0, c1 + 1 - c0);
        let snap = d.snap();
        let hdr = d.hdr();
        let mut items = Vec::new();
        match cmd {
            Cmd::InsertCellsRight | Cmd::RemoveCellsLeft => {
                if cmd == Cmd::InsertCellsRight {
                    // Grow the table if the last column would lose data.
                    d.insert_cols(d.ncols(), w);
                }
                let snap = d.snap();
                let nc = d.ncols();
                for r in r0..=r1.min(d.nrows().saturating_sub(1)) {
                    let vals = snap.row_values(snap.row_id(r + hdr));
                    for c in c0..nc {
                        let v = if cmd == Cmd::InsertCellsRight {
                            if c < c0 + w { String::new() } else { vals[c - w].clone() }
                        } else {
                            vals.get(c + w).cloned().unwrap_or_default()
                        };
                        items.push((r, c, v));
                    }
                }
            }
            _ => {
                let n = d.nrows();
                if n > 2_000_000 {
                    self.msg("Too many rows to shift cells vertically");
                    return;
                }
                if cmd == Cmd::InsertCellsDown {
                    d.insert_rows(n, h);
                }
                let snap = if cmd == Cmd::InsertCellsDown { d.snap() } else { snap };
                let n = d.nrows();
                for c in c0..=c1 {
                    let id = d.cols[c];
                    for r in r0..n {
                        let v = if cmd == Cmd::InsertCellsDown {
                            if r < r0 + h { String::new() } else { snap.value(snap.row_id(r - h + hdr), id) }
                        } else if r + h < n {
                            snap.value(snap.row_id(r + h + hdr), id)
                        } else {
                            String::new()
                        };
                        items.push((r, c, v));
                    }
                }
            }
        }
        d.set_cells(items);
    }

    fn move_cells(&mut self, cmd: Cmd) {
        let Some((block, (r0, c0), (r1, c1))) = self.sel_block() else { return };
        let Some(d) = self.doc_edit() else { return };
        let (dr, dc): (isize, isize) = match cmd {
            Cmd::MoveCellUp => (-1, 0),
            Cmd::MoveCellDown => (1, 0),
            Cmd::MoveCellLeft => (0, -1),
            _ => (0, 1),
        };
        let (nr, nc) = (d.vrows() as isize, d.vcols.len() as isize);
        let (nr0, nc0) = (r0 as isize + dr, c0 as isize + dc);
        let (nr1, nc1) = (r1 as isize + dr, c1 as isize + dc);
        if nr0 < 0 || nc0 < 0 || nr1 >= nr || nc1 >= nc {
            return;
        }
        // The line of cells being displaced moves to the opposite side.
        let (lr, lc) = if dr < 0 { (nr0 as usize, c0) } else if dr > 0 { (nr1 as usize, c0) } else if dc < 0 { (r0, nc0 as usize) } else { (r0, nc1 as usize) };
        let line: Vec<String> = if dr != 0 {
            (c0..=c1).map(|vc| d.cell(d.vrow_to_data(lr), d.vcols[vc])).collect()
        } else {
            (r0..=r1).map(|vr| d.cell(d.vrow_to_data(vr), d.vcols[lc])).collect()
        };
        let mut items = Vec::new();
        for (i, row) in block.iter().enumerate() {
            for (j, v) in row.iter().enumerate() {
                let vr = (r0 + i) as isize + dr;
                let vc = (c0 + j) as isize + dc;
                items.push((d.vrow_to_data(vr as usize), d.vcols[vc as usize], v.clone()));
            }
        }
        for (k, v) in line.into_iter().enumerate() {
            let (vr, vc) = if dr < 0 {
                (r1, c0 + k)
            } else if dr > 0 {
                (r0, c0 + k)
            } else if dc < 0 {
                (r0 + k, c1)
            } else {
                (r0 + k, c0)
            };
            items.push((d.vrow_to_data(vr), d.vcols[vc], v));
        }
        d.set_cells(items);
        let shift = |p: (usize, usize)| ((p.0 as isize + dr) as usize, (p.1 as isize + dc) as usize);
        d.grid.cur = shift(d.grid.cur);
        d.grid.anchor = shift(d.grid.anchor);
        d.ensure_visible();
    }

    fn add_id_column(&mut self) {
        let Some(d) = self.doc_edit() else { return };
        if d.nrows() > 5_000_000 {
            self.msg("Too many rows for an ID column");
            return;
        }
        let first_is_id = d.has_header && d.cell_t(0, 0).eq_ignore_ascii_case("id");
        if !first_is_id {
            d.insert_cols(0, 1);
        }
        let mut items: Vec<(usize, usize, String)> = Vec::new();
        let h = d.hdr();
        if d.has_header {
            items.push((0, 0, "ID".into()));
        }
        for r in 0..d.nrows() {
            items.push((r + h, 0, (r + 1).to_string()));
        }
        d.set_cells_t(items);
    }

    fn web(&mut self, cmd: Cmd) {
        let Some((block, _, _)) = self.sel_block() else { return };
        let mut urls = Vec::new();
        match cmd {
            Cmd::FollowLinks => {
                for v in block.iter().flatten() {
                    let v = v.trim();
                    if v.starts_with("http://") || v.starts_with("https://") {
                        urls.push(v.to_string());
                    } else if v.starts_with("www.") {
                        urls.push(format!("https://{v}"));
                    }
                }
            }
            Cmd::WebSearch => {
                for v in block.iter().flatten().filter(|v| !v.trim().is_empty()) {
                    urls.push(format!("{}{}", self.settings.search_engine, url_encode(v.trim())));
                }
            }
            Cmd::MapSearch => {
                for row in &block {
                    let q = row.iter().filter(|v| !v.trim().is_empty()).cloned().collect::<Vec<_>>().join(" ");
                    if !q.is_empty() {
                        urls.push(format!("{}{}", self.settings.map_engine, url_encode(&q)));
                    }
                }
            }
            _ => {
                let emails: Vec<String> = block.iter().flatten().filter(|v| v.contains('@')).map(|v| v.trim().to_string()).collect();
                if !emails.is_empty() {
                    urls.push(format!("mailto:{}", emails.join(",")));
                }
            }
        }
        if urls.len() > 25 {
            self.msg(format!("Too many ({}) — select at most 25", urls.len()));
            return;
        }
        if urls.is_empty() {
            self.msg("Nothing to open in the selection");
        }
        for u in urls {
            let _ = open::that(u);
        }
    }

    fn close_tabs(&mut self, mut tabs: Vec<usize>) {
        let dirty: Vec<usize> = tabs.iter().copied().filter(|&i| self.docs.get(i).is_some_and(|d| d.dirty)).collect();
        if !dirty.is_empty() {
            self.dialog = Some(Dialog::ConfirmClose { tabs, quit: false });
            return;
        }
        tabs.sort_unstable();
        for i in tabs.into_iter().rev() {
            if i < self.docs.len() {
                self.docs.remove(i);
            }
        }
        self.active = self.active.min(self.docs.len().saturating_sub(1));
    }

    fn move_file(&mut self, ctx: &Context, from: &Path, to: &Path, copy: bool) {
        if to.exists() {
            self.msg(format!("{} already exists", to.display()));
            return;
        }
        if copy {
            match std::fs::copy(from, to) {
                Ok(_) => self.open_path(ctx, to),
                Err(e) => self.msg(format!("Copy failed: {e}")),
            }
            return;
        }
        let i = self.active;
        let header = self.docs[i].has_header;
        // Release the memory map first (Windows can't rename a mapped file).
        let old = self.docs.remove(i);
        drop(old);
        let res = std::fs::rename(from, to).or_else(|_| std::fs::copy(from, to).and_then(|_| std::fs::remove_file(from)));
        let path = if res.is_ok() { to } else { from };
        if let Err(e) = res {
            self.msg(format!("Move failed: {e}"));
        }
        match Document::open(path, self.load_opts_for(path), ctx) {
            Ok(mut d) => {
                d.has_header = header;
                self.docs.insert(i, d);
                self.active = i;
            }
            Err(e) => self.msg(e),
        }
    }

    fn parse_col(&self, s: &str) -> Option<usize> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        if let Ok(n) = s.parse::<i64>() {
            return usize::try_from(n - self.settings.first_col_index).ok();
        }
        let mut n = 0usize;
        for ch in s.to_ascii_uppercase().chars() {
            if !ch.is_ascii_uppercase() {
                return None;
            }
            n = n * 26 + (ch as usize - 'A' as usize + 1);
        }
        Some(n - 1)
    }

    fn parse_row(&self, s: &str) -> Option<usize> {
        s.trim().parse::<i64>().ok().and_then(|n| usize::try_from(n - self.settings.first_row_index).ok())
    }

    fn submit_prompt(&mut self, ctx: &Context, p: Prompt) {
        let v = &p.values;
        let n = pnum(v, "n").unwrap_or(1).clamp(1, 1_000_000) as usize;
        match p.action {
            PromptAction::Transform(i) => self.apply_transform(i, v),
            PromptAction::InsertRows => {
                if let Some(d) = self.doc_edit() {
                    let at = d.sel_data_rows().first().copied().unwrap_or(0);
                    d.insert_rows(at, n);
                }
            }
            PromptAction::InsertCols => {
                if let Some(d) = self.doc_edit() {
                    let at = d.sel_cols().first().copied().unwrap_or(0);
                    d.insert_cols(at, n);
                }
            }
            PromptAction::DupRows => {
                if let Some(r) = self.contiguous_rows() {
                    if let Some(d) = self.doc_edit() {
                        d.duplicate_rows(r, n);
                    }
                }
            }
            PromptAction::DupCols => {
                if let Some(r) = self.contiguous_cols() {
                    if let Some(d) = self.doc_edit() {
                        d.duplicate_cols(r, n);
                    }
                }
            }
            PromptAction::GoToCell | PromptAction::SelectTo => {
                let row = self.parse_row(&pv(v, "row"));
                let col = self.parse_col(&pv(v, "col"));
                let Some(d) = self.doc() else { return };
                let r = row.and_then(|r| d.data_to_vrow(r)).unwrap_or(d.grid.cur.0);
                let c = col.and_then(|c| d.col_to_vcol(c)).unwrap_or(d.grid.cur.1);
                let anchor = if matches!(p.action, PromptAction::SelectTo) { d.grid.cur } else { (r, c) };
                d.select((r, c), anchor);
            }
            PromptAction::MoveRowsAbove => {
                let Some(target) = self.parse_row(&pv(v, "row")) else { return };
                let Some(r) = self.contiguous_rows() else { return };
                let Some(d) = self.doc_edit() else { return };
                let to = if target > r.end { target - r.len() } else { target.min(r.start) };
                d.move_rows(r, to.min(d.nrows()));
            }
            PromptAction::MoveColsLeftOf => {
                let Some(target) = self.parse_col(&pv(v, "col")) else { return };
                let Some(r) = self.contiguous_cols() else { return };
                let Some(d) = self.doc_edit() else { return };
                let to = if target > r.end { target - r.len() } else { target.min(r.start) };
                d.move_cols(r, to.min(d.ncols()));
            }
            PromptAction::ColWidth => {
                let w = pnum(v, "w").unwrap_or(120).clamp(20, 4000) as f32;
                if let Some(d) = self.doc() {
                    for c in d.sel_cols() {
                        let id = d.cols[c];
                        d.widths.insert(id, w);
                    }
                }
            }
            PromptAction::DuplicateFile | PromptAction::RenameFile => {
                let name = pv(v, "name");
                let Some(from) = self.docs.get(self.active).and_then(|d| d.path.clone()) else { return };
                if name.trim().is_empty() {
                    return;
                }
                let to = from.with_file_name(name.trim());
                self.move_file(ctx, &from, &to, matches!(p.action, PromptAction::DuplicateFile));
            }
        }
    }

    fn apply_transform(&mut self, i: usize, values: &ParamValues) {
        let Some((block, (r0, c0), (r1, c1))) = self.sel_block() else {
            self.msg("Select up to 5M cells first");
            return;
        };
        if self.doc_edit().is_none() {
            return;
        }
        let t = &self.transforms[i];
        let out = match transforms::apply(t.id, values, &block) {
            Ok(o) => o,
            Err(e) => {
                self.msg(format!("{}: {e}", t.name));
                return;
            }
        };
        let fits = out.insert_cols == 0
            && out.insert_rows == 0
            && out.cells.len() <= r1 + 1 - r0
            && out.cells.iter().all(|r| r.len() <= c1 + 1 - c0);
        if fits {
            self.write_block((r0, c0), &out.cells, Some(((r0, c0), (r1, c1))));
            return;
        }
        let d = &mut self.docs[self.active];
        if d.has_view_filter() || !d.hidden_cols.is_empty() {
            self.msg("This transform changes the table shape — clear filters/hidden columns first");
            return;
        }
        if out.insert_rows > 0 {
            d.insert_rows(r1 + 1, out.insert_rows);
        }
        if out.insert_cols > 0 {
            d.insert_cols(c1 + 1, out.insert_cols);
        }
        // Grow the table if the output still extends beyond it.
        let need_rows = r0 + out.cells.len();
        if need_rows > d.nrows() {
            d.insert_rows(d.nrows(), need_rows - d.nrows());
        }
        let need_cols = c0 + out.cells.iter().map(|r| r.len()).max().unwrap_or(0);
        if need_cols > d.ncols() {
            d.insert_cols(d.ncols(), need_cols - d.ncols());
        }
        d.view_dirty = true;
        d.visible = None;
        d.refresh_vcols();
        self.write_block((r0, c0), &out.cells, Some(((r0, c0), (r1, c1))));
    }

    fn run_lookup(&mut self, st: &LookupState) {
        let Some(src) = self.docs.get(st.src_doc) else { return };
        let vals: Vec<usize> = st.src_vals.iter().enumerate().filter(|(_, b)| **b).map(|(i, _)| i).collect();
        if vals.is_empty() {
            self.msg("Choose at least one value column");
            return;
        }
        if vals.contains(&st.src_key) && st.src_doc == self.active {
            self.msg("Value columns can't include the key column");
            return;
        }
        let s = src.snap();
        let src_names = s.header_names();
        let mut map = std::collections::HashMap::new();
        for r in 0..s.nrows() {
            let row = s.row_values(s.row_id(r + s.hdr));
            let key = row.get(st.src_key).cloned().unwrap_or_default();
            map.entry(key).or_insert_with(|| vals.iter().map(|&c| row.get(c).cloned().unwrap_or_default()).collect::<Vec<_>>());
        }
        let Some(d) = self.doc_edit() else { return };
        let start = if st.insert {
            let at = (st.dst_key + 1).min(d.ncols());
            d.insert_cols(at, vals.len());
            at
        } else {
            st.dst_start
        };
        if start + vals.len() > d.ncols() {
            let extra = start + vals.len() - d.ncols();
            d.insert_cols(d.ncols(), extra);
        }
        let snap = d.snap();
        let key_id = d.cols[st.dst_key];
        let mut items = Vec::new();
        let mut hits = 0;
        if d.has_header && st.insert {
            for (k, &c) in vals.iter().enumerate() {
                items.push((0, start + k, src_names.get(c).cloned().unwrap_or_else(|| col_letter(c))));
            }
        }
        let h = d.hdr();
        for r in 0..d.nrows() {
            let key = snap.value(snap.row_id(r + h), key_id);
            let found = map.get(&key);
            if found.is_some() {
                hits += 1;
            }
            for k in 0..vals.len() {
                items.push((r + h, start + k, found.map(|f| f[k].clone()).unwrap_or_default()));
            }
        }
        d.set_cells_t(items);
        self.msg(format!("Column lookup: matched {} rows", fmt_num(hits)));
    }

    // ------------------------------------------------------------------
    // UI pieces
    // ------------------------------------------------------------------

    fn apply_theme(&mut self, ctx: &Context) {
        let key = (self.settings.theme, self.settings.font_size);
        if self.applied_theme == Some(key) {
            return;
        }
        self.applied_theme = Some(key);
        ui_kit::apply_style(ctx, self.settings.theme, self.settings.font_size);
    }

    fn logo(&mut self, ctx: &Context) -> egui::TextureHandle {
        self.logo
            .get_or_insert_with(|| {
                let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon/icon-256.png")).unwrap_or_default();
                let img = egui::ColorImage::from_rgba_unmultiplied([icon.width as usize, icon.height as usize], &icon.rgba);
                ctx.load_texture("logo", img, egui::TextureOptions::LINEAR)
            })
            .clone()
    }

    fn toolbar(&mut self, ui: &mut Ui) {
        use ui_kit::{divider, icon, tool_button};
        let ctx = ui.ctx().clone();
        let d = self.docs.get(self.active);
        let has = d.is_some();
        let can_undo = d.is_some_and(|d| !d.undo.is_empty());
        let can_redo = d.is_some_and(|d| !d.redo.is_empty());
        let dirty = d.is_some_and(|d| d.dirty);
        let nf = d.map_or(0, |d| d.filters.len());
        let mut fire = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let mut t = |ui: &mut Ui, ic: &str, c: Cmd, enabled: bool| {
                let sc = commands::shortcut_text(&ctx, c);
                let tip = if sc.is_empty() { c.name().to_string() } else { format!("{}   {sc}", c.name()) };
                if tool_button(ui, ic, &tip, enabled).clicked() {
                    fire = Some(c);
                }
            };
            t(ui, icon::FILE_PLUS, Cmd::NewFile, true);
            t(ui, icon::FOLDER_OPEN, Cmd::OpenFile, true);
            t(ui, icon::FLOPPY_DISK, Cmd::SaveFile, has && dirty);
            divider(ui);
            t(ui, icon::UNDO, Cmd::Undo, can_undo);
            t(ui, icon::REDO, Cmd::Redo, can_redo);
            divider(ui);
            t(ui, icon::SEARCH, Cmd::Find, has);
            t(ui, icon::FUNNEL, Cmd::AddFilter, has);
            if nf > 0 {
                t(ui, icon::FUNNEL_X, Cmd::RemoveFilters, true);
            }
            t(ui, icon::SORT_ASC, Cmd::SortColAsc, has);
            t(ui, icon::SORT_DESC, Cmd::SortColDesc, has);
            divider(ui);
            t(ui, icon::PLUS, Cmd::InsertRowsAfter, has);
            t(ui, icon::TRASH, Cmd::RemoveRows, has);
            t(ui, icon::MAGIC, Cmd::Transform, has);
            t(ui, icon::CHART_BAR, Cmd::OpenAnalysis, has);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                t(ui, icon::GEAR, Cmd::Preferences, true);
                t(ui, icon::SIDEBAR, Cmd::ToggleSidebar, true);
                t(ui, icon::COMMAND, Cmd::CommandPalette, true);
            });
        });
        if let Some(c) = fire {
            self.run(&ctx, c);
        }
    }

    fn welcome(&mut self, ui: &mut Ui) {
        use ui_kit::icon;
        let ctx = ui.ctx().clone();
        let logo = self.logo(&ctx);
        let mut action: Option<PalItem> = None;
        let v = ui.visuals().clone();
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.16).max(24.0));
            ui.add(egui::Image::new(&logo).fit_to_exact_size(egui::vec2(88.0, 88.0)));
            ui.add_space(10.0);
            ui.label(RichText::new(APP_NAME).font(ui_kit::semibold(28.0)));
            ui.label(RichText::new("Fast CSV editor for big files").color(v.weak_text_color()));
            ui.add_space(22.0);
            let w = 2.0 * 150.0 + 10.0;
            ui.allocate_ui_with_layout(egui::vec2(w, 36.0), Layout::left_to_right(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                let a = ui_kit::accent(ui);
                let open = egui::Button::new((ui_kit::icon_text(icon::FOLDER_OPEN, 16.0).color(Color32::WHITE), RichText::new("Open File…").color(Color32::WHITE).font(ui_kit::semibold(14.0))))
                    .fill(a)
                    .corner_radius(8)
                    .min_size(egui::vec2(150.0, 36.0));
                if ui.add(open).clicked() {
                    action = Some(PalItem::Cmd(Cmd::OpenFile));
                }
                let new = egui::Button::new((ui_kit::icon_text(icon::FILE_PLUS, 16.0), RichText::new("New File").size(14.0))).corner_radius(8).min_size(egui::vec2(150.0, 36.0));
                if ui.add(new).clicked() {
                    action = Some(PalItem::Cmd(Cmd::NewFile));
                }
            });
            let recent: Vec<String> = self.settings.recent.iter().filter(|r| Path::new(r).exists()).take(8).cloned().collect();
            if !recent.is_empty() {
                ui.add_space(30.0);
                ui.allocate_ui_with_layout(egui::vec2(460.0, 0.0), Layout::top_down(Align::Min), |ui| {
                    ui_kit::section(ui, "Recent");
                    for r in recent {
                        let p = Path::new(&r);
                        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        let dir = p.parent().map(|d| d.display().to_string()).unwrap_or_default();
                        let (rect, resp) = ui.allocate_exact_size(egui::vec2(460.0, 40.0), egui::Sense::click());
                        if resp.hovered() {
                            ui.painter().rect_filled(rect, 8, v.widgets.hovered.weak_bg_fill);
                        }
                        let pnt = ui.painter();
                        pnt.text(egui::pos2(rect.left() + 12.0, rect.center().y), egui::Align2::LEFT_CENTER, icon::FILE_TEXT, ui_kit::icon_font(18.0), ui_kit::accent(ui));
                        pnt.text(egui::pos2(rect.left() + 40.0, rect.top() + 12.0), egui::Align2::LEFT_CENTER, name, egui::FontId::proportional(13.5), v.text_color());
                        let clip = rect.shrink2(egui::vec2(8.0, 0.0));
                        pnt.with_clip_rect(clip).text(egui::pos2(rect.left() + 40.0, rect.top() + 28.0), egui::Align2::LEFT_CENTER, dir, egui::FontId::proportional(11.5), v.weak_text_color());
                        if resp.clicked() {
                            action = Some(PalItem::File(PathBuf::from(&r)));
                        }
                    }
                });
            }
            ui.add_space(26.0);
            let hint = format!("Drop a CSV anywhere  ·  {} for all commands", commands::shortcut_text(&ctx, Cmd::CommandPalette));
            ui.label(RichText::new(hint).size(12.0).color(v.weak_text_color()));
        });
        if let Some(a) = action {
            self.run_item(&ctx, a);
        }
    }

    fn save_and_close(&mut self, ctx: &Context, tabs: Vec<usize>, quit: bool) {
        let mut waiting = HashSet::new();
        let mut close_now = Vec::new();
        for &i in &tabs {
            let Some(d) = self.docs.get(i) else { continue };
            if !d.dirty {
                close_now.push(i);
                continue;
            }
            let target = match (&d.path, d.read_only) {
                (Some(p), false) => p.clone(),
                _ => match rfd::FileDialog::new().set_file_name(d.title()).save_file() {
                    Some(p) => p,
                    None => return, // cancelled: keep everything open
                },
            };
            waiting.insert(target.clone());
            self.save(ctx, i, Some(target), false);
        }
        close_now.sort_unstable();
        for i in close_now.into_iter().rev() {
            if i < self.docs.len() && self.docs[i].aux_job.is_none() {
                self.docs.remove(i);
            }
        }
        self.active = self.active.min(self.docs.len().saturating_sub(1));
        if waiting.is_empty() {
            if quit {
                self.allow_quit = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        } else {
            self.close_after_save = Some((waiting, quit));
        }
    }

    fn handle_shortcuts(&mut self, ctx: &Context) {
        let focused = ctx.memory(|m| m.focused());
        let text_focus = focused.is_some() && focused != Some(grid::grid_id());
        let modal = self.prompt.is_some() || self.palette.is_some() || self.dialog.is_some();
        let mut cmds: Vec<Cmd> = commands::ALL.iter().copied().filter(|c| c.shortcut().is_some()).collect();
        let weight = |c: &Cmd| {
            let m = c.shortcut().unwrap().modifiers;
            m.alt as u8 + m.shift as u8 + (m.command || m.ctrl || m.mac_cmd) as u8
        };
        cmds.sort_by_key(|c| std::cmp::Reverse(weight(c)));
        let mut fire = Vec::new();
        ctx.input_mut(|i| {
            if i.consume_key(egui::Modifiers { shift: true, ..egui::Modifiers::COMMAND }, Key::Z) && !text_focus && !modal {
                fire.push(Cmd::Redo);
            }
            for c in cmds {
                if (text_focus || modal) && commands::is_grid_only(c) {
                    continue;
                }
                if modal && !matches!(c, Cmd::CommandPalette) {
                    continue;
                }
                if i.consume_shortcut(&c.shortcut().unwrap()) {
                    fire.push(c);
                }
            }
        });
        for c in fire {
            self.run(ctx, c);
        }
    }

    fn menu_bar(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let mut run = None;
        let mut tops: Vec<egui::Response> = Vec::new();
        egui::MenuBar::new().ui(ui, |ui| {
            for &menu in commands::MENUS {
                let top = ui.menu_button(menu, |ui| {
                    if menu == "File" {
                        ui.menu_button("Open Recent", |ui| {
                            for r in self.settings.recent.clone() {
                                if ui.button(&r).clicked() {
                                    self.open_path(&ctx, Path::new(&r));
                                    ui.close();
                                }
                            }
                        });
                    }
                    if menu == "Transform" {
                        for (i, t) in self.transforms.iter().enumerate() {
                            if ui.button(t.name).clicked() {
                                run = Some(PalItem::Transform(i));
                                ui.close();
                            }
                        }
                        ui.separator();
                    }
                    for &c in commands::ALL.iter().filter(|c| c.category() == menu && **c != Cmd::Transform) {
                        let sc = commands::shortcut_text(&ctx, c);
                        if ui.add(egui::Button::new(c.name()).shortcut_text(sc)).clicked() {
                            run = Some(PalItem::Cmd(c));
                            ui.close();
                        }
                    }
                });
                tops.push(top.response);
            }
        });
        menu_hover_switch(&ctx, &tops);
        if let Some(item) = run {
            self.run_item(&ctx, item);
        }
    }

    fn run_item(&mut self, ctx: &Context, item: PalItem) {
        match item {
            PalItem::Cmd(c) => self.run(ctx, c),
            PalItem::File(p) => self.open_path(ctx, &p),
            PalItem::Transform(i) => {
                let t = &self.transforms[i];
                if t.params.is_empty() {
                    self.apply_transform(i, &ParamValues::new());
                } else {
                    let (name, params) = (t.name.to_string(), t.params.clone());
                    self.ask(&name, params, PromptAction::Transform(i));
                }
            }
        }
    }

    fn tab_bar(&mut self, ui: &mut Ui) {
        use egui::{pos2, vec2, CornerRadius, FontId, Sense, Stroke};
        let mut close = None;
        let mut activate = None;
        let mut new = false;
        let v = ui.visuals().clone();
        let accent = v.selection.stroke.color;
        egui::ScrollArea::horizontal().scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for i in 0..self.docs.len() {
                    let d = &self.docs[i];
                    let active = i == self.active;
                    let title = d.title();
                    let font = if active { ui_kit::semibold(13.0) } else { FontId::proportional(13.0) };
                    let text_w = ui.painter().layout_no_wrap(title.clone(), font.clone(), Color32::WHITE).size().x;
                    let lock_w = if d.read_only { 18.0 } else { 0.0 };
                    let w = (text_w + lock_w + 50.0).clamp(96.0, 260.0);
                    let (rect, resp) = ui.allocate_exact_size(vec2(w, 32.0), Sense::click());
                    let x_rect = egui::Rect::from_center_size(pos2(rect.right() - 16.0, rect.center().y), vec2(20.0, 20.0));
                    let x_resp = ui.interact(x_rect, egui::Id::new(("tab-x", i)), Sense::click());
                    let hovered = resp.hovered() || x_resp.hovered();
                    let p = ui.painter();
                    let top = CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 };
                    if active {
                        p.rect_filled(rect, top, v.extreme_bg_color);
                        p.hline(rect.left() + 6.0..=rect.right() - 6.0, rect.top() + 1.0, Stroke::new(2.0, accent));
                    } else if hovered {
                        p.rect_filled(rect.shrink2(vec2(0.0, 3.0)), CornerRadius::same(6), v.widgets.hovered.weak_bg_fill);
                    }
                    let fg = if active { v.text_color() } else { v.weak_text_color() };
                    let mut x = rect.left() + 12.0;
                    if d.read_only {
                        p.text(pos2(x, rect.center().y), egui::Align2::LEFT_CENTER, ui_kit::icon::LOCK, ui_kit::icon_font(13.0), fg);
                        x += lock_w;
                    }
                    let clip = egui::Rect::from_min_max(pos2(x, rect.top()), pos2(x_rect.left() - 4.0, rect.bottom()));
                    p.with_clip_rect(clip).text(pos2(x, rect.center().y), egui::Align2::LEFT_CENTER, &title, font, fg);
                    if hovered || active {
                        if x_resp.hovered() {
                            p.rect_filled(x_rect, CornerRadius::same(4), v.widgets.hovered.weak_bg_fill.gamma_multiply(1.6));
                        }
                        if d.dirty && !hovered {
                            p.circle_filled(x_rect.center(), 4.0, accent);
                        } else {
                            p.text(x_rect.center(), egui::Align2::CENTER_CENTER, ui_kit::icon::X, ui_kit::icon_font(13.0), fg);
                        }
                    } else if d.dirty {
                        p.circle_filled(x_rect.center(), 4.0, fg);
                    }
                    if x_resp.clicked() || resp.middle_clicked() {
                        close = Some(i);
                    } else if resp.clicked() {
                        activate = Some(i);
                    }
                    let tip = d.path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "Not saved yet".into());
                    resp.on_hover_text(tip);
                }
                ui.add_space(4.0);
                if ui_kit::tool_button(ui, ui_kit::icon::PLUS, "New file", true).clicked() {
                    new = true;
                }
            });
        });
        if let Some(i) = activate {
            self.active = i;
        }
        if let Some(i) = close {
            self.close_tabs(vec![i]);
        }
        if new {
            let ctx = ui.ctx().clone();
            self.run(&ctx, Cmd::NewFile);
        }
    }

    fn find_bar(&mut self, ui: &mut Ui) {
        use ui_kit::{chip, icon, tool_button};
        let ctx = ui.ctx().clone();
        let mut action: Option<Cmd> = None;
        let v = ui.visuals().clone();
        let field = |ui: &mut Ui, ic: &str, add: &mut dyn FnMut(&mut Ui)| {
            egui::Frame::new()
                .fill(v.extreme_bg_color)
                .stroke(egui::Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color))
                .corner_radius(7)
                .inner_margin(egui::Margin::symmetric(8, 3))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(ui_kit::icon_text(ic, 14.0).color(v.weak_text_color()));
                        add(ui);
                    });
                });
        };
        ui.horizontal(|ui| {
            let mut enter = false;
            let mut shift = false;
            let focus = std::mem::take(&mut self.find_focus);
            let text = &mut self.find.text;
            field(ui, icon::SEARCH, &mut |ui: &mut Ui| {
                let r = ui.add(egui::TextEdit::singleline(text).frame(egui::Frame::NONE).desired_width(240.0).hint_text("Find in table"));
                if focus {
                    r.request_focus();
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    enter = true;
                    shift = ui.input(|i| i.modifiers.shift);
                    r.request_focus();
                }
            });
            if enter {
                action = Some(if shift { Cmd::FindPrev } else { Cmd::FindNext });
            }
            if tool_button(ui, icon::CARET_UP, "Previous match   ⇧F3", true).clicked() {
                action = Some(Cmd::FindPrev);
            }
            if tool_button(ui, icon::CARET_DOWN, "Next match   F3", true).clicked() {
                action = Some(Cmd::FindNext);
            }
            ui.add_space(4.0);
            chip(ui, &mut self.find.case, icon::CASE, "Match case");
            chip(ui, &mut self.find.whole_word, icon::WORD, "Whole word");
            chip(ui, &mut self.find.whole_cell, icon::BRACKETS, "Whole cell");
            chip(ui, &mut self.find.regex, icon::REGEX, "Regular expression");
            chip(ui, &mut self.find.selection_only, icon::SELECTION, "In selection only");
            chip(ui, &mut self.find.highlight, icon::HIGHLIGHT, "Highlight matches");
            ui.add_space(4.0);
            if let Err(e) = self.find.compile() {
                ui.colored_label(v.error_fg_color, e.lines().last().unwrap_or("invalid pattern").to_string());
            } else if let Some(n) = self.find_count {
                ui.weak(format!("{} matches", fmt_num(n)));
            } else if !self.find.text.is_empty() && ui.link("Count").clicked() {
                action = Some(Cmd::FindAll);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if tool_button(ui, icon::X, "Close   Esc", true).clicked() {
                    self.find_open = false;
                    self.replace_open = false;
                }
                chip(ui, &mut self.replace_open, icon::PENCIL, "Replace");
            });
        });
        if self.replace_open {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                let rt = &mut self.replace_text;
                field(ui, icon::PENCIL, &mut |ui: &mut Ui| {
                    ui.add(egui::TextEdit::singleline(rt).frame(egui::Frame::NONE).desired_width(240.0).hint_text("Replace with ($1 in regex mode)"));
                });
                if ui_kit::secondary(ui, "Replace").clicked() {
                    action = Some(Cmd::ReplaceNext);
                }
                if ui_kit::secondary(ui, "Replace All").clicked() {
                    action = Some(Cmd::ReplaceAll);
                }
                if let Some(re) = self.find.compile().ok().flatten() {
                    if let Some(d) = self.docs.get(self.active) {
                        if d.vrows() > 0 && !d.vcols.is_empty() {
                            let cell = d.cell(d.vrow_to_data(d.grid.cur.0), d.vcol_to_col(d.grid.cur.1));
                            if re.is_match(&cell) {
                                let preview = if self.find.regex {
                                    re.replace_all(&cell, self.replace_text.as_str()).into_owned()
                                } else {
                                    re.replace_all(&cell, regex::NoExpand(&self.replace_text)).into_owned()
                                };
                                let short = |s: &str| s.chars().take(40).collect::<String>();
                                ui.weak(format!("{}  →  {}", short(&cell), short(&preview)));
                            }
                        }
                    }
                }
            });
        }
        if ui.input(|i| i.key_pressed(Key::Escape)) && ctx.memory(|m| m.focused()).is_some_and(|f| f != grid::grid_id()) {
            self.find_open = false;
            self.replace_open = false;
        }
        if let Some(c) = action {
            self.run(&ctx, c);
        }
    }

    fn metadata_bar(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let Some(d) = self.docs.get_mut(self.active) else { return };
        let mut reload: Option<LoadOpts> = None;
        ui.horizontal_wrapped(|ui| {
            ui.strong(d.title());
            if let Some(p) = &d.path {
                ui.weak(p.parent().map(|p| p.display().to_string()).unwrap_or_default());
            }
            ui.separator();
            ui.label(fmt_size(d.file_size));
            ui.label(format!("{} × {}", fmt_num(d.nrows()), d.ncols()));
            ui.separator();
            ui.checkbox(&mut d.read_only, "Read-only");
            let mut h = d.has_header;
            if ui.checkbox(&mut h, "Header row").changed() {
                d.has_header = h;
                d.view_dirty = true;
                d.visible = None;
                d.clamp_cursor();
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Load:").strong());
            let mut lo = d.load_opts.clone();
            egui::ComboBox::from_id_salt("ld").selected_text(lo.delimiter.map_or("Auto".into(), delimiter_label)).show_ui(ui, |ui| {
                ui.selectable_value(&mut lo.delimiter, None, "Auto");
                for (b, l) in DELIMS {
                    ui.selectable_value(&mut lo.delimiter, Some(*b), *l);
                }
            });
            egui::ComboBox::from_id_salt("le").selected_text(lo.encoding.map_or("Auto", |e| e.name())).show_ui(ui, |ui| {
                ui.selectable_value(&mut lo.encoding, None, "Auto");
                for name in ENCODINGS {
                    if let Some(e) = encoding_rs::Encoding::for_label(name.as_bytes()) {
                        ui.selectable_value(&mut lo.encoding, Some(e), *name);
                    }
                }
            });
            egui::ComboBox::from_id_salt("lq").selected_text(if lo.quotes { "RFC 4180" } else { "Delimiter only" }).show_ui(ui, |ui| {
                ui.selectable_value(&mut lo.quotes, true, "RFC 4180");
                ui.selectable_value(&mut lo.quotes, false, "Delimiter only");
            });
            ui.weak(format!(
                "(detected: {}, {}, {})",
                delimiter_label(d.src.delimiter),
                d.src.encoding.name(),
                d.src.line_ending.label()
            ));
            if d.path.is_some() && ui.button("⟳ Reload").clicked() {
                reload = Some(lo.clone());
            }
            d.load_opts = lo;
            ui.separator();
            ui.label(RichText::new("Save:").strong());
            let so = &mut d.save_opts;
            egui::ComboBox::from_id_salt("sd").selected_text(delimiter_label(so.delimiter)).show_ui(ui, |ui| {
                for (b, l) in DELIMS {
                    ui.selectable_value(&mut so.delimiter, *b, *l);
                }
            });
            egui::ComboBox::from_id_salt("se").selected_text(so.encoding.name()).show_ui(ui, |ui| {
                for name in ENCODINGS {
                    if let Some(e) = encoding_rs::Encoding::for_label(name.as_bytes()) {
                        ui.selectable_value(&mut so.encoding, e, *name);
                    }
                }
            });
            egui::ComboBox::from_id_salt("sl").selected_text(so.line_ending.label()).show_ui(ui, |ui| {
                ui.selectable_value(&mut so.line_ending, LineEnding::Lf, "LF");
                ui.selectable_value(&mut so.line_ending, LineEnding::CrLf, "CRLF");
            });
            egui::ComboBox::from_id_salt("sq")
                .selected_text(match so.quoting {
                    Quoting::AsNeeded => "Quote as needed",
                    Quoting::All => "Quote all",
                    Quoting::Never => "Never quote",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut so.quoting, Quoting::AsNeeded, "Quote as needed");
                    ui.selectable_value(&mut so.quoting, Quoting::All, "Quote all");
                    ui.selectable_value(&mut so.quoting, Quoting::Never, "Never quote");
                });
            ui.checkbox(&mut so.bom, "BOM");
        });
        if let Some(o) = reload {
            let i = self.active;
            self.reload(&ctx, i, Some(o));
        }
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        use ui_kit::icon;
        let ctx = ui.ctx().clone();
        let mut open_filters = false;
        let v = ui.visuals().clone();
        let accent = v.selection.stroke.color;
        let small = |t: String| RichText::new(t).size(12.0).color(v.weak_text_color());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let Some(d) = self.docs.get(self.active) else {
                ui.label(small(format!("{APP_NAME} {}", env!("CARGO_PKG_VERSION"))));
                return;
            };
            if !d.ready() {
                ui.spinner();
                ui.label(small(format!("Indexing {:.0}% · {} rows", d.index_progress() * 100.0, fmt_num(d.total_rows()))));
            } else {
                ui_kit::icon_label(ui, icon::TABLE, small(format!("{} rows · {} columns", fmt_num(d.nrows()), d.ncols())), Some(v.weak_text_color()));
            }
            if d.visible.is_some() || !d.filters.is_empty() || d.vcols.len() != d.ncols() {
                let text = format!("{} of {} rows", fmt_num(d.vrows()), fmt_num(d.nrows()));
                let b = egui::Button::new((ui_kit::icon_text(icon::FUNNEL, 12.0).color(accent), RichText::new(text).size(12.0).color(accent)))
                    .fill(accent.gamma_multiply(0.14))
                    .corner_radius(10);
                if ui.add(b).on_hover_text("Filters and hidden rows — click to manage").clicked() {
                    open_filters = true;
                }
            }
            for j in [&d.view_job, &d.aux_job].into_iter().flatten() {
                ui.spinner();
                ui.label(small(format!("{}…", j.label)));
            }
            if d.vrows() > 0 && !d.vcols.is_empty() {
                let (r, c) = d.grid.cur;
                let dr = d.vrow_to_data(r);
                let col = d.vcol_to_col(c);
                let cref = format!(
                    "{}{}",
                    if self.settings.col_letters { col_letter(col) } else { format!("{}:", col as i64 + self.settings.first_col_index) },
                    dr as i64 + self.settings.first_row_index
                );
                ui.label(RichText::new(cref).size(12.0).font(ui_kit::semibold(12.0)));
                let ((r0, c0), (r1, c1)) = d.sel_range();
                let cells = (r1 + 1 - r0) * (c1 + 1 - c0);
                if cells > 1 && cells <= 20_000 {
                    let snap = d.snap();
                    let (mut sum, mut cnt, mut nonempty) = (0.0, 0usize, 0usize);
                    for vr in r0..=r1.min(d.vrows() - 1) {
                        let vals = snap.row_values(d.data_row_id(d.vrow_to_data(vr)));
                        for vc in c0..=c1.min(d.vcols.len() - 1) {
                            let s = vals.get(d.vcols[vc]).map(|s| s.trim()).unwrap_or("");
                            if !s.is_empty() {
                                nonempty += 1;
                            }
                            if let Ok(x) = s.parse::<f64>() {
                                sum += x;
                                cnt += 1;
                            }
                        }
                    }
                    let mut t = format!("{} cells · {} filled", fmt_num(cells), fmt_num(nonempty));
                    if cnt > 0 {
                        t.push_str(&format!(" · sum {} · avg {}", trim_f(sum), trim_f(sum / cnt as f64)));
                    }
                    ui.label(small(t));
                } else if cells > 1 {
                    ui.label(small(format!("{} cells", fmt_num(cells))));
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for t in [d.src.line_ending.label().to_string(), d.src.encoding.name().to_string(), delimiter_label(d.src.delimiter)] {
                    ui_kit::kbd(ui, &t);
                }
                if d.read_only {
                    ui_kit::icon_label(ui, icon::LOCK, RichText::new("Read-only").size(12.0), Some(v.warn_fg_color));
                }
            });
        });
        if open_filters {
            self.run(&ctx, Cmd::ViewFilters);
        }
    }

    fn sidebar(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let mut open: Option<PathBuf> = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.strong("Open Files");
            for i in 0..self.docs.len() {
                if ui.selectable_label(i == self.active, self.docs[i].title()).clicked() {
                    self.active = i;
                }
            }
            for folder in &self.settings.folders {
                ui.separator();
                ui.strong(Path::new(folder).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(folder.clone()));
                for f in self.folder_files.iter().filter(|f| f.starts_with(folder)) {
                    let rel = f.strip_prefix(folder).unwrap_or(f).display().to_string();
                    if ui.selectable_label(false, rel).clicked() {
                        open = Some(f.clone());
                    }
                }
            }
        });
        if let Some(p) = open {
            self.open_path(&ctx, &p);
        }
    }

    fn editor_pane(&mut self, ui: &mut Ui) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        if d.vrows() == 0 || d.vcols.is_empty() {
            return;
        }
        let (r, c) = d.grid.cur;
        let key = (self.active, d.vrow_to_data(r), d.vcol_to_col(c));
        if self.editor.as_ref().map(|e| e.0) != Some(key) {
            self.editor = Some((key, d.cell(key.1, key.2)));
        }
        let (_, text) = self.editor.as_mut().unwrap();
        ui.horizontal(|ui| {
            ui.strong("Editor");
            ui.weak("(changes apply when focus leaves the editor)");
        });
        let resp = egui::ScrollArea::vertical()
            .show(ui, |ui| ui.add_sized(ui.available_size(), egui::TextEdit::multiline(text).interactive(!d.read_only)))
            .inner;
        if resp.lost_focus() && !d.read_only && *text != d.cell(key.1, key.2) {
            d.set_cells(vec![(key.1, key.2, text.clone())]);
        }
    }

    fn palette_ui(&mut self, ctx: &Context) {
        use ui_kit::icon;
        let Some(p) = &mut self.palette else { return };
        let mut items: Vec<(i32, String, String, String, PalItem)> = Vec::new(); // score, title, subtitle, shortcut, item
        if p.files {
            let mut seen = HashSet::new();
            let paths = self
                .docs
                .iter()
                .filter_map(|d| d.path.clone())
                .chain(self.folder_files.iter().cloned())
                .chain(self.settings.recent.iter().map(PathBuf::from));
            for path in paths {
                if !seen.insert(path.clone()) {
                    continue;
                }
                let s = path.display().to_string();
                if let Some(sc) = commands::fuzzy_score(&p.query, &s) {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let dir = path.parent().map(|d| d.display().to_string()).unwrap_or_default();
                    items.push((sc, name, dir, String::new(), PalItem::File(path)));
                }
            }
        } else {
            for &c in commands::ALL {
                let label = format!("{} {}", c.category(), c.name());
                if let Some(sc) = commands::fuzzy_score(&p.query, &label) {
                    items.push((sc, c.name().into(), c.category().into(), commands::shortcut_text(ctx, c), PalItem::Cmd(c)));
                }
            }
            for (i, t) in self.transforms.iter().enumerate() {
                let label = format!("Transform {}", t.name);
                if let Some(sc) = commands::fuzzy_score(&p.query, &label) {
                    items.push((sc, t.name.into(), "Transform".into(), String::new(), PalItem::Transform(i)));
                }
            }
        }
        items.sort_by(|a, b| b.0.cmp(&a.0));
        items.truncate(200);
        p.sel = p.sel.min(items.len().saturating_sub(1));
        let mut chosen = None;
        let v = ctx.global_style().visuals.clone();
        let accent = v.selection.stroke.color;
        let frame = egui::Frame::new()
            .fill(v.window_fill)
            .stroke(v.window_stroke)
            .shadow(v.window_shadow)
            .corner_radius(14)
            .inner_margin(egui::Margin::same(10));
        let area = egui::Modal::default_area(egui::Id::new("palette-area")).anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 90.0));
        let resp = egui::Modal::new(egui::Id::new("palette")).area(area).frame(frame).backdrop_color(Color32::from_black_alpha(70)).show(ctx, |ui| {
            ui.set_width(620.0);
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                ui.label(ui_kit::icon_text(icon::SEARCH, 18.0).color(v.weak_text_color()));
                let hint = if p.files { "Go to file…" } else { "Search commands and transforms…" };
                let r = ui.add(
                    egui::TextEdit::singleline(&mut p.query).frame(egui::Frame::NONE).hint_text(hint).font(egui::FontId::proportional(17.0)).desired_width(f32::INFINITY),
                );
                if p.focus {
                    r.request_focus();
                    p.focus = false;
                }
                if r.changed() {
                    p.sel = 0;
                }
            });
            ui.add_space(6.0);
            ui.separator();
            ui.input(|i| {
                if i.key_pressed(Key::ArrowDown) {
                    p.sel = (p.sel + 1).min(items.len().saturating_sub(1));
                }
                if i.key_pressed(Key::ArrowUp) {
                    p.sel = p.sel.saturating_sub(1);
                }
                if i.key_pressed(Key::Enter) {
                    chosen = items.get(p.sel).map(|x| x.4.clone());
                }
            });
            egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                if items.is_empty() {
                    ui.add_space(12.0);
                    ui.vertical_centered(|ui| ui.weak("No matches"));
                    ui.add_space(12.0);
                }
                for (k, (_, title, sub, shortcut, item)) in items.iter().enumerate() {
                    let sel = k == p.sel;
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 34.0), egui::Sense::click());
                    if sel {
                        ui.painter().rect_filled(rect, 8, accent.gamma_multiply(0.18));
                        resp.scroll_to_me(None);
                    } else if resp.hovered() {
                        ui.painter().rect_filled(rect, 8, v.widgets.hovered.weak_bg_fill);
                    }
                    let ic = match item {
                        PalItem::File(_) => icon::FILE_TEXT,
                        PalItem::Transform(_) => icon::MAGIC,
                        PalItem::Cmd(_) => icon::COMMAND,
                    };
                    let pnt = ui.painter();
                    let fg = if sel { accent } else { v.weak_text_color() };
                    pnt.text(egui::pos2(rect.left() + 14.0, rect.center().y), egui::Align2::LEFT_CENTER, ic, ui_kit::icon_font(15.0), fg);
                    let title_g = pnt.text(egui::pos2(rect.left() + 40.0, rect.center().y), egui::Align2::LEFT_CENTER, title, egui::FontId::proportional(14.0), v.text_color());
                    pnt.with_clip_rect(rect.shrink(4.0)).text(
                        egui::pos2(title_g.right() + 10.0, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        sub,
                        egui::FontId::proportional(12.0),
                        v.weak_text_color(),
                    );
                    if !shortcut.is_empty() {
                        pnt.text(egui::pos2(rect.right() - 12.0, rect.center().y), egui::Align2::RIGHT_CENTER, shortcut, egui::FontId::proportional(12.0), v.weak_text_color());
                    }
                    if resp.clicked() {
                        chosen = Some(item.clone());
                    }
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                ui.label(RichText::new("↑↓ navigate    ↵ run    esc close").size(11.5).color(v.weak_text_color()));
            });
        });
        if resp.should_close() || chosen.is_some() {
            self.palette = None;
        }
        if let Some(item) = chosen {
            self.run_item(ctx, item);
        }
    }

    fn prompt_ui(&mut self, ctx: &Context) {
        let Some(p) = &mut self.prompt else { return };
        let is_transform = matches!(p.action, PromptAction::Transform(_));
        let ic = if is_transform { ui_kit::icon::MAGIC } else { ui_kit::icon::PENCIL };
        let accent = ctx.global_style().visuals.selection.stroke.color;
        let title = p.title.clone();
        let res = ui_kit::Dialog::new("prompt", title).icon(ic, accent).width(400.0).show(
            ctx,
            |ui| {
                let mut submit = false;
                egui::Grid::new("prompt_grid").num_columns(2).spacing([14.0, 10.0]).min_col_width(110.0).show(ui, |ui| {
                    for (k, prm) in p.params.iter().enumerate() {
                        ui.label(RichText::new(prm.label).color(ui.visuals().weak_text_color()));
                        let v = p.values.entry(prm.key.to_string()).or_insert_with(|| prm.default.to_string());
                        match &prm.kind {
                            ParamKind::Bool => {
                                let mut b = matches!(v.as_str(), "true" | "1" | "yes");
                                if ui.checkbox(&mut b, "").changed() {
                                    *v = b.to_string();
                                }
                            }
                            ParamKind::Choice(opts) => {
                                egui::ComboBox::from_id_salt(("choice", k)).width(220.0).selected_text(v.clone()).show_ui(ui, |ui| {
                                    for o in opts {
                                        ui.selectable_value(v, o.to_string(), *o);
                                    }
                                });
                            }
                            _ => {
                                let r = ui.add(egui::TextEdit::singleline(v).desired_width(220.0));
                                if k == 0 && p.focus {
                                    r.request_focus();
                                }
                                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                                    submit = true;
                                }
                            }
                        }
                        ui.end_row();
                        if !prm.help.is_empty() {
                            ui.label("");
                            ui.label(RichText::new(prm.help).size(11.5).color(ui.visuals().weak_text_color()));
                            ui.end_row();
                        }
                    }
                });
                p.focus = false;
                (submit, false)
            },
            |ui, (submit, cancel)| {
                if ui_kit::primary(ui, if is_transform { "Apply" } else { "OK" }).clicked() {
                    *submit = true;
                }
                if ui_kit::secondary(ui, "Cancel").clicked() {
                    *cancel = true;
                }
            },
        );
        let (submit, cancel) = res.inner;
        if submit {
            let p = self.prompt.take().unwrap();
            self.submit_prompt(ctx, p);
        } else if cancel || res.dismissed {
            self.prompt = None;
        }
    }

    fn dialogs(&mut self, ctx: &Context) {
        use ui_kit::{danger, icon, primary, secondary, section, Dialog as Dlg};
        let Some(mut dlg) = self.dialog.take() else { return };
        let mut keep = true;
        let mut after: Option<Box<dyn FnOnce(&mut App, &Context)>> = None;
        let vis = ctx.global_style().visuals.clone();
        let accent = vis.selection.stroke.color;
        let warn = vis.warn_fg_color;
        let weak = vis.weak_text_color();
        let error = vis.error_fg_color;
        match &mut dlg {
            Dialog::ConfirmClose { tabs, quit } => {
                let dirty: Vec<String> = tabs.iter().filter_map(|&i| self.docs.get(i).filter(|d| d.dirty).map(|d| d.title())).collect();
                let title = match dirty.as_slice() {
                    [one] => format!("Save changes to “{one}”?"),
                    many => format!("Save changes to {} files?", many.len()),
                };
                let r = Dlg::new("confirm", title).icon(icon::WARNING, warn).show(
                    ctx,
                    |ui| {
                        if dirty.len() > 1 {
                            for n in &dirty {
                                ui.label(format!("•  {n}"));
                            }
                            ui.add_space(4.0);
                        }
                        ui.label(RichText::new("Your changes will be lost if you don't save them.").color(weak));
                        0u8
                    },
                    |ui, choice| {
                        if primary(ui, "Save").clicked() {
                            *choice = 1;
                        }
                        if secondary(ui, "Cancel").clicked() {
                            *choice = 3;
                        }
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            if danger(ui, "Don't Save").clicked() {
                                *choice = 2;
                            }
                        });
                        if ui.input(|i| i.key_pressed(Key::Enter)) {
                            *choice = 1;
                        }
                    },
                );
                let (tabs, quit) = (tabs.clone(), *quit);
                match r.inner {
                    1 => {
                        after = Some(Box::new(move |app, ctx| app.save_and_close(ctx, tabs, quit)));
                        keep = false;
                    }
                    2 => {
                        after = Some(Box::new(move |app, ctx| {
                            for &i in &tabs {
                                if let Some(d) = app.docs.get_mut(i) {
                                    d.dirty = false;
                                }
                            }
                            if quit {
                                app.allow_quit = true;
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            } else {
                                app.close_tabs(tabs);
                            }
                        }));
                        keep = false;
                    }
                    3 => keep = false,
                    _ => keep = !r.dismissed,
                }
            }
            Dialog::DeleteFile(i) => {
                let i = *i;
                let name = self.docs.get(i).map(|d| d.title()).unwrap_or_default();
                let r = Dlg::new("delete", format!("Delete “{name}”?")).icon(icon::TRASH, error).show(
                    ctx,
                    |ui| {
                        ui.label(RichText::new("The file will be permanently deleted from disk. This can't be undone.").color(weak));
                        0u8
                    },
                    |ui, c| {
                        if danger(ui, "Delete").clicked() {
                            *c = 1;
                        }
                        if secondary(ui, "Cancel").clicked() {
                            *c = 2;
                        }
                    },
                );
                if r.inner == 1 {
                    after = Some(Box::new(move |app, _| {
                        if i < app.docs.len() {
                            let d = app.docs.remove(i);
                            let p = d.path.clone();
                            drop(d);
                            if let Some(p) = p {
                                match std::fs::remove_file(&p) {
                                    Ok(()) => app.msg(format!("Deleted {}", p.display())),
                                    Err(e) => app.msg(format!("Delete failed: {e}")),
                                }
                            }
                            app.active = app.active.min(app.docs.len().saturating_sub(1));
                        }
                    }));
                }
                keep = r.inner == 0 && !r.dismissed;
            }
            Dialog::ExternalChanged(i) => {
                let i = *i;
                let name = self.docs.get(i).map(|d| d.title()).unwrap_or_default();
                let r = Dlg::new("ext", format!("“{name}” changed on disk")).icon(icon::RELOAD, accent).show(
                    ctx,
                    |ui| {
                        ui.label(RichText::new("Another program modified this file. Reload to see the latest version — unsaved changes in FI CSV will be lost.").color(weak));
                        0u8
                    },
                    |ui, c| {
                        if primary(ui, "Reload").clicked() {
                            *c = 1;
                        }
                        if secondary(ui, "Keep My Version").clicked() {
                            *c = 2;
                        }
                    },
                );
                if r.inner == 1 {
                    after = Some(Box::new(move |app, ctx| app.reload(ctx, i, None)));
                }
                keep = r.inner == 0 && !r.dismissed;
            }
            Dialog::About => {
                let logo = self.logo(ctx);
                let r = Dlg::new("about", "").width(340.0).show(
                    ctx,
                    |ui| {
                        ui.vertical_centered(|ui| {
                            ui.add(egui::Image::new(&logo).fit_to_exact_size(egui::vec2(72.0, 72.0)));
                            ui.add_space(6.0);
                            ui.label(RichText::new(APP_NAME).font(ui_kit::semibold(22.0)));
                            ui.label(RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION"))).color(weak));
                            ui.add_space(8.0);
                            ui.label("A fast CSV editor for very large files, with full right-to-left and Dhivehi support.");
                            ui.add_space(4.0);
                            ui.hyperlink_to("github.com/marcoazeem/ficsv", "https://github.com/marcoazeem/ficsv");
                        });
                        false
                    },
                    |ui, ok| {
                        if primary(ui, "OK").clicked() {
                            *ok = true;
                        }
                    },
                );
                keep = !r.inner && !r.dismissed;
            }
            Dialog::Import { path, preserve } => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let r = Dlg::new("import", format!("Import “{name}”")).icon(icon::FILE_TEXT, accent).show(
                    ctx,
                    |ui| {
                        ui.label(RichText::new("FI CSV finds the repeating records in the file and turns them into rows.").color(weak));
                        ui.add_space(8.0);
                        ui.checkbox(preserve, "Preserve original file structure");
                        ui.label(RichText::new("Lossless path/value rows that export back to exactly the same JSON or XML.").size(11.5).color(weak));
                        0u8
                    },
                    |ui, c| {
                        if primary(ui, "Import").clicked() {
                            *c = 1;
                        }
                        if secondary(ui, "Cancel").clicked() {
                            *c = 2;
                        }
                    },
                );
                if r.inner == 1 {
                    let (p, pres) = (path.clone(), *preserve);
                    after = Some(Box::new(move |app, _| match formats::import_file(&p, pres) {
                        Ok(t) => {
                            app.docs.push(Document::from_table(t.header, t.rows));
                            app.active = app.docs.len() - 1;
                            app.msg("Imported — use Save File As to write it as CSV");
                        }
                        Err(e) => app.msg(format!("Import failed: {e}")),
                    }));
                }
                keep = r.inner == 0 && !r.dismissed;
            }
            Dialog::Export { format, typed, visible_only } => {
                let r = Dlg::new("export", "Export to JSON or XML").icon(icon::FILE_TEXT, accent).show(
                    ctx,
                    |ui| {
                        section(ui, "Format");
                        ui.horizontal(|ui| {
                            ui.selectable_value(format, ExportFormat::Json, "  JSON  ");
                            ui.selectable_value(format, ExportFormat::Xml, "  XML  ");
                        });
                        section(ui, "Options");
                        ui.add_enabled(*format == ExportFormat::Json, egui::Checkbox::new(typed, "Typed values (numbers, true/false, null)"));
                        ui.checkbox(visible_only, "Only visible rows and columns");
                        0u8
                    },
                    |ui, c| {
                        if primary(ui, "Export…").clicked() {
                            *c = 1;
                        }
                        if secondary(ui, "Cancel").clicked() {
                            *c = 2;
                        }
                    },
                );
                if r.inner == 1 {
                    let (f, t, v) = (*format, *typed, *visible_only);
                    after = Some(Box::new(move |app, ctx| app.export_data(ctx, f, t, v)));
                }
                keep = r.inner == 0 && !r.dismissed;
            }
            Dialog::Pdf { opts, selection_only, print, title } => {
                let heading = if *print { "Print" } else { "Export to PDF" };
                let r = Dlg::new("pdf", heading).icon(icon::PRINTER, accent).width(440.0).show(
                    ctx,
                    |ui| {
                        section(ui, "Page");
                        ui.horizontal(|ui| {
                            ui.selectable_value(&mut opts.paper_a4, false, "Letter");
                            ui.selectable_value(&mut opts.paper_a4, true, "A4");
                            ui.add_space(12.0);
                            ui.selectable_value(&mut opts.landscape, false, "Portrait");
                            ui.selectable_value(&mut opts.landscape, true, "Landscape");
                        });
                        ui.add(egui::Slider::new(&mut opts.font_size, 5.0..=16.0).text("Text size"));
                        section(ui, "Range");
                        ui.checkbox(selection_only, "Selected cells only");
                        section(ui, "Layout");
                        egui::Grid::new("pdfopts").num_columns(2).spacing([24.0, 4.0]).show(ui, |ui| {
                            ui.checkbox(&mut opts.repeat_headers, "Header on every page");
                            ui.checkbox(&mut opts.labels, "Row & column labels");
                            ui.end_row();
                            ui.checkbox(&mut opts.gridlines, "Gridlines");
                            ui.checkbox(&mut opts.page_numbers, "Page numbers");
                            ui.end_row();
                            ui.checkbox(&mut opts.center_horizontally, "Center horizontally");
                            ui.checkbox(&mut opts.center_vertically, "Center vertically");
                            ui.end_row();
                            ui.checkbox(title, "File name as title");
                            ui.end_row();
                        });
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Page order").color(weak));
                            ui.selectable_value(&mut opts.row_wise_page_order, true, "Across, then down");
                            ui.selectable_value(&mut opts.row_wise_page_order, false, "Down, then across");
                        });
                        0u8
                    },
                    |ui, c| {
                        if primary(ui, if *print { "Print…" } else { "Export…" }).clicked() {
                            *c = 1;
                        }
                        if secondary(ui, "Cancel").clicked() {
                            *c = 2;
                        }
                    },
                );
                if r.inner == 1 {
                    let mut o = opts.clone();
                    let (sel, is_print, want_title) = (*selection_only, *print, *title);
                    after = Some(Box::new(move |app, ctx| app.export_pdf(ctx, &mut o, sel, is_print, want_title)));
                }
                keep = r.inner == 0 && !r.dismissed;
            }
            Dialog::Filters { col, text, focus } => {
                let Some(d) = self.docs.get_mut(self.active) else { return };
                let names: Vec<String> = { let s = d.snap(); (0..d.ncols()).map(|c| s.column_name(c)).collect() };
                let has_filters = !d.filters.is_empty();
                let r = Dlg::new("filters", "Filters").icon(icon::FUNNEL, accent).width(520.0).show(
                    ctx,
                    |ui| {
                        let mut err = None;
                        if d.filters.is_empty() {
                            ui.label(RichText::new("No filters yet. Rows are shown when they match every filter.").color(weak));
                        } else {
                            let mut remove = None;
                            for (k, f) in d.filters.iter().enumerate() {
                                let cname = f.col.and_then(|id| d.cols.iter().position(|&x| x == id)).map_or("Any column".to_string(), |c| names[c].clone());
                                egui::Frame::new().fill(ui.visuals().faint_bg_color).corner_radius(8).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new(crate::rtl::visual(&cname).into_owned()).font(ui_kit::semibold(13.0)));
                                        ui.label(RichText::new(&f.text).monospace().color(accent));
                                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                            if ui_kit::tool_button(ui, icon::X, "Remove filter", true).clicked() {
                                                remove = Some(k);
                                            }
                                        });
                                    });
                                });
                            }
                            if let Some(k) = remove {
                                d.filters.remove(k);
                                d.view_dirty = true;
                            }
                        }
                        section(ui, "Add filter");
                        ui.horizontal(|ui| {
                            egui::ComboBox::from_id_salt("fcol")
                                .width(150.0)
                                .selected_text(col.map_or("Any column".to_string(), |c| crate::rtl::visual(names.get(c).map(String::as_str).unwrap_or("")).into_owned()))
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(col, None, "Any column");
                                    for (c, n) in names.iter().enumerate() {
                                        ui.selectable_value(col, Some(c), crate::rtl::visual(n).into_owned());
                                    }
                                });
                            let rr = ui.add(egui::TextEdit::singleline(text).desired_width(230.0).hint_text(">100  |  =done  |  !draft  |  #^\\d+$"));
                            if *focus {
                                rr.request_focus();
                                *focus = false;
                            }
                            let enter = rr.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                            if (ui_kit::primary(ui, "Add").clicked() || enter) && !text.trim().is_empty() {
                                match Query::parse(text) {
                                    Ok(_) => {
                                        d.filters.push(Filter { col: col.map(|c| d.cols[c]), text: text.clone() });
                                        d.view_dirty = true;
                                        text.clear();
                                    }
                                    Err(e) => err = Some(e),
                                }
                            }
                        });
                        ui.collapsing(RichText::new("Filter syntax").color(weak), |ui| {
                            egui::Grid::new("syntax").num_columns(2).spacing([16.0, 3.0]).show(ui, |ui| {
                                for (a, b) in [
                                    ("foo", "contains foo"),
                                    ("=foo", "equals foo"),
                                    ("f*o", "wildcard"),
                                    ("!foo", "does not contain"),
                                    ("a | b    a & b", "or / and"),
                                    (">5  <=10  10-20", "numbers"),
                                    ("50+   50-", "at least / at most"),
                                    ("!", "not empty"),
                                    ("#regex", "regular expression"),
                                ] {
                                    ui.label(RichText::new(a).monospace().color(accent));
                                    ui.label(RichText::new(b).color(weak));
                                    ui.end_row();
                                }
                            });
                        });
                        (err, 0u8)
                    },
                    |ui, (_, c)| {
                        if primary(ui, "Done").clicked() {
                            *c = 1;
                        }
                        if has_filters {
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                if danger(ui, "Remove All").clicked() {
                                    *c = 2;
                                }
                            });
                        }
                    },
                );
                let (err, c) = r.inner;
                if let Some(e) = err {
                    after = Some(Box::new(move |app, _| app.msg(e)));
                }
                if c == 2 {
                    d.filters.clear();
                    d.view_dirty = true;
                }
                keep = c != 1 && !r.dismissed;
            }
            Dialog::Lookup(st) => {
                if st.src_doc >= self.docs.len() || self.docs.is_empty() {
                    return;
                }
                let doc_names: Vec<String> = self.docs.iter().map(|d| d.title()).collect();
                let src_names: Vec<String> = { let s = self.docs[st.src_doc].snap(); (0..s.ncols()).map(|c| crate::rtl::visual(&s.column_name(c)).into_owned()).collect() };
                let dst_title = self.docs[self.active].title();
                let dst_names: Vec<String> = { let s = self.docs[self.active].snap(); (0..s.ncols()).map(|c| crate::rtl::visual(&s.column_name(c)).into_owned()).collect() };
                let src_ncols: Vec<usize> = self.docs.iter().map(|d| d.ncols()).collect();
                let r = Dlg::new("lookup", "Column Lookup").icon(icon::TABLE, accent).width(500.0).show(
                    ctx,
                    |ui| {
                        ui.label(RichText::new("Copy values from a lookup file into this file by matching key columns, like VLOOKUP.").color(weak));
                        section(ui, "From");
                        egui::Grid::new("lk1").num_columns(2).spacing([12.0, 8.0]).min_col_width(120.0).show(ui, |ui| {
                            ui.label("Lookup file");
                            let before = st.src_doc;
                            egui::ComboBox::from_id_salt("lsrc").width(260.0).selected_text(&doc_names[st.src_doc]).show_ui(ui, |ui| {
                                for (i, n) in doc_names.iter().enumerate() {
                                    ui.selectable_value(&mut st.src_doc, i, n);
                                }
                            });
                            if before != st.src_doc {
                                st.src_vals = vec![false; src_ncols[st.src_doc]];
                                st.src_key = 0;
                            }
                            ui.end_row();
                            ui.label("Key column");
                            egui::ComboBox::from_id_salt("lkey").width(260.0).selected_text(src_names.get(st.src_key).cloned().unwrap_or_default()).show_ui(ui, |ui| {
                                for (c, n) in src_names.iter().enumerate() {
                                    ui.selectable_value(&mut st.src_key, c, n);
                                }
                            });
                            ui.end_row();
                            ui.label("Values to copy");
                            egui::ScrollArea::vertical().max_height(130.0).id_salt("lvals").show(ui, |ui| {
                                st.src_vals.resize(src_names.len(), false);
                                for (c, n) in src_names.iter().enumerate() {
                                    if c != st.src_key {
                                        ui.checkbox(&mut st.src_vals[c], n);
                                    }
                                }
                            });
                            ui.end_row();
                        });
                        section(ui, &format!("Into {dst_title}"));
                        egui::Grid::new("lk2").num_columns(2).spacing([12.0, 8.0]).min_col_width(120.0).show(ui, |ui| {
                            ui.label("Key column");
                            egui::ComboBox::from_id_salt("ldk").width(260.0).selected_text(dst_names.get(st.dst_key).cloned().unwrap_or_default()).show_ui(ui, |ui| {
                                for (c, n) in dst_names.iter().enumerate() {
                                    ui.selectable_value(&mut st.dst_key, c, n);
                                }
                            });
                            ui.end_row();
                            ui.label("Output");
                            ui.vertical(|ui| {
                                ui.radio_value(&mut st.insert, true, "Insert new columns after the key");
                                ui.radio_value(&mut st.insert, false, "Overwrite columns starting at:");
                                if !st.insert {
                                    egui::ComboBox::from_id_salt("lds").width(200.0).selected_text(dst_names.get(st.dst_start).cloned().unwrap_or("(new)".into())).show_ui(ui, |ui| {
                                        for (c, n) in dst_names.iter().enumerate() {
                                            ui.selectable_value(&mut st.dst_start, c, n);
                                        }
                                    });
                                }
                            });
                            ui.end_row();
                        });
                        0u8
                    },
                    |ui, c| {
                        if primary(ui, "Run Lookup").clicked() {
                            *c = 1;
                        }
                        if secondary(ui, "Cancel").clicked() {
                            *c = 2;
                        }
                    },
                );
                if r.inner == 1 {
                    let st2 = LookupState { src_vals: st.src_vals.clone(), ..*st };
                    after = Some(Box::new(move |app, _| app.run_lookup(&st2)));
                }
                keep = r.inner == 0 && !r.dismissed;
            }
            Dialog::Preferences => {
                let mut open = true;
                let mut apply_fonts = false;
                let s = &mut self.settings;
                egui::Window::new(RichText::new("Preferences").font(ui_kit::semibold(15.0)))
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(false)
                    .default_width(500.0)
                    .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                    .show(ctx, |ui| {
                        egui::ScrollArea::vertical().max_height(560.0).show(ui, |ui| {
                            section(ui, "Appearance");
                            ui.horizontal_wrapped(|ui| {
                                for t in Theme::ALL {
                                    let tv = t.visuals();
                                    let sel = s.theme == t;
                                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(88.0, 58.0), egui::Sense::click());
                                    let pnt = ui.painter();
                                    pnt.rect_filled(rect, 8, tv.panel_fill);
                                    pnt.rect_filled(egui::Rect::from_min_size(rect.min + egui::vec2(8.0, 8.0), egui::vec2(72.0, 22.0)), 4, tv.extreme_bg_color);
                                    pnt.circle_filled(rect.min + egui::vec2(18.0, 19.0), 5.0, tv.selection.stroke.color);
                                    pnt.text(rect.center_bottom() - egui::vec2(0.0, 12.0), egui::Align2::CENTER_CENTER, t.label(), egui::FontId::proportional(11.0), tv.text_color());
                                    let stroke = if sel { egui::Stroke::new(2.0, accent) } else { egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color) };
                                    pnt.rect_stroke(rect, 8, stroke, egui::StrokeKind::Inside);
                                    if resp.clicked() {
                                        s.theme = t;
                                    }
                                }
                            });
                            egui::Grid::new("prefs-a").num_columns(2).spacing([16.0, 8.0]).min_col_width(170.0).show(ui, |ui| {
                                ui.label("Text size");
                                ui.add(egui::Slider::new(&mut s.font_size, 10.0..=20.0).step_by(0.5));
                                ui.end_row();
                                ui.label("Row height");
                                ui.add(egui::Slider::new(&mut s.row_height, 18.0..=40.0).step_by(1.0));
                                ui.end_row();
                            });
                            section(ui, "Table");
                            egui::Grid::new("prefs-t").num_columns(2).spacing([24.0, 4.0]).show(ui, |ui| {
                                ui.checkbox(&mut s.shade_rows, "Shade alternate rows");
                                ui.checkbox(&mut s.shade_cols, "Shade alternate columns");
                                ui.end_row();
                                ui.checkbox(&mut s.highlight_cursor_cross, "Highlight row & column");
                                ui.checkbox(&mut s.col_letters, "Columns as letters");
                                ui.end_row();
                                ui.checkbox(&mut s.show_row_labels, "Row numbers");
                                ui.checkbox(&mut s.show_col_labels, "Column labels");
                                ui.end_row();
                            });
                            egui::Grid::new("prefs-t2").num_columns(2).spacing([16.0, 8.0]).min_col_width(170.0).show(ui, |ui| {
                                ui.label("First row number");
                                ui.add(egui::DragValue::new(&mut s.first_row_index));
                                ui.end_row();
                                ui.label("First column number");
                                ui.add(egui::DragValue::new(&mut s.first_col_index));
                                ui.end_row();
                                ui.label("Max auto-fit width");
                                ui.add(egui::Slider::new(&mut s.max_autofit_width, 120.0..=1600.0).suffix(" px"));
                                ui.end_row();
                            });
                            section(ui, "Files");
                            egui::Grid::new("prefs-f").num_columns(2).spacing([24.0, 4.0]).show(ui, |ui| {
                                ui.checkbox(&mut s.default_header, "First row is a header");
                                ui.checkbox(&mut s.auto_detect_delimiter, "Auto-detect delimiter");
                                ui.end_row();
                                ui.checkbox(&mut s.retain_filters_on_refresh, "Keep filters on reload");
                                ui.checkbox(&mut s.save_after_each_edit, "Save after each edit");
                                ui.end_row();
                                ui.checkbox(&mut s.show_toolbar, "Show toolbar");
                                ui.end_row();
                            });
                            egui::Grid::new("prefs-f2").num_columns(2).spacing([16.0, 8.0]).min_col_width(170.0).show(ui, |ui| {
                                ui.label("When a file changes on disk");
                                egui::ComboBox::from_id_salt("ext").selected_text(format!("{:?}", s.external_change)).show_ui(ui, |ui| {
                                    ui.selectable_value(&mut s.external_change, ExternalChange::Refresh, "Refresh");
                                    ui.selectable_value(&mut s.external_change, ExternalChange::Ask, "Ask");
                                    ui.selectable_value(&mut s.external_change, ExternalChange::Ignore, "Ignore");
                                });
                                ui.end_row();
                                ui.label("Open read-only above (MB)");
                                ui.add(egui::DragValue::new(&mut s.read_only_threshold_mb).suffix(" MB")).on_hover_text("0 = never");
                                ui.end_row();
                            });
                            section(ui, "Dhivehi");
                            ui.horizontal(|ui| {
                                let found = crate::rtl::find_faruma(&s.dhivehi_font);
                                let hint = found.as_ref().map_or("Using the bundled Faruma".to_string(), |p| format!("Auto: {}", p.display()));
                                ui.add(egui::TextEdit::singleline(&mut s.dhivehi_font).hint_text(hint).desired_width(300.0));
                                if ui.button("Browse…").clicked() {
                                    if let Some(p) = rfd::FileDialog::new().add_filter("Font", &["ttf", "otf"]).pick_file() {
                                        s.dhivehi_font = p.to_string_lossy().into_owned();
                                        apply_fonts = true;
                                    }
                                }
                            });
                            section(ui, "Integrations");
                            egui::Grid::new("prefs-i").num_columns(2).spacing([16.0, 8.0]).min_col_width(170.0).show(ui, |ui| {
                                ui.label("Search engine URL");
                                ui.add(egui::TextEdit::singleline(&mut s.search_engine).desired_width(280.0));
                                ui.end_row();
                                ui.label("Map search URL");
                                ui.add(egui::TextEdit::singleline(&mut s.map_engine).desired_width(280.0));
                                ui.end_row();
                                ui.label("Text editor command");
                                ui.add(egui::TextEdit::singleline(&mut s.text_editor).hint_text("System default").desired_width(280.0));
                                ui.end_row();
                            });
                            section(ui, "Delimiter by file extension");
                            let mut remove = None;
                            for (ext, del) in s.ext_delimiters.iter_mut() {
                                ui.horizontal(|ui| {
                                    ui.label(format!(".{ext}"));
                                    ui.add(egui::TextEdit::singleline(del).desired_width(40.0));
                                    if ui_kit::tool_button(ui, icon::X, "Remove", true).clicked() {
                                        remove = Some(ext.clone());
                                    }
                                });
                            }
                            if let Some(e) = remove {
                                s.ext_delimiters.remove(&e);
                            }
                            if ui.button((ui_kit::icon_text(icon::PLUS, 13.0), "Add extension")).clicked() {
                                s.ext_delimiters.insert(format!("ext{}", s.ext_delimiters.len() + 1), ";".into());
                            }
                            if let Some(dir) = config_dir() {
                                ui.add_space(8.0);
                                ui.label(RichText::new(format!("Settings are saved in {}", dir.join("settings.json").display())).size(11.0).color(weak));
                            }
                        });
                    });
                if apply_fonts {
                    let w = crate::rtl::install_fonts(ctx, &self.settings.dhivehi_font);
                    if let Some(w) = w {
                        after = Some(Box::new(move |app, _| app.msg(w)));
                    }
                }
                if ctx.input(|i| i.key_pressed(Key::Escape)) && !egui::Popup::is_any_open(ctx) {
                    open = false;
                }
                if !open {
                    keep = false;
                    self.settings.save();
                }
            }
        }
        if keep && self.dialog.is_none() {
            self.dialog = Some(dlg);
        }
        if let Some(f) = after {
            f(self, ctx);
        }
    }

    fn export_pdf(&mut self, ctx: &Context, opts: &mut PdfOptions, selection_only: bool, print: bool, want_title: bool) {
        let Some(d) = self.docs.get(self.active) else { return };
        if want_title {
            opts.title = Some(d.title());
        }
        let path = if print {
            std::env::temp_dir().join(format!("ficsv-print-{}.pdf", std::process::id()))
        } else {
            match rfd::FileDialog::new().add_filter("PDF", &["pdf"]).set_file_name(format!("{}.pdf", d.title())).save_file() {
                Some(p) => p,
                None => return,
            }
        };
        let table = SubTable { inner: d.snap(), rows: d.visible.clone(), cols: d.vcols.clone() };
        let (rows, cols) = if selection_only {
            let ((r0, c0), (r1, c1)) = d.sel_range();
            (Some((r0..=r1.min(d.vrows().saturating_sub(1))).collect::<Vec<_>>()), Some((c0..=c1).collect::<Vec<_>>()))
        } else {
            (None, None)
        };
        let o = opts.clone();
        self.start_aux(ctx, "Writing PDF", move |_| {
            match formats::export_pdf(&table, &path, &o, rows.as_deref(), cols.as_deref()) {
                Ok(pages) => {
                    if print {
                        let _ = open::that(&path);
                        JobResult::Done(format!("Opened {pages} pages in the system viewer — print from there"))
                    } else {
                        JobResult::Done(format!("Exported {pages} pages to {}", path.display()))
                    }
                }
                Err(e) => JobResult::Error(format!("PDF export failed: {e}")),
            }
        });
    }

    fn export_data(&mut self, ctx: &Context, format: ExportFormat, typed: bool, visible_only: bool) {
        let Some(d) = self.docs.get(self.active) else { return };
        let ext = if format == ExportFormat::Json { "json" } else { "xml" };
        let stem = d.path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().into_owned()).unwrap_or("export".into());
        let Some(path) = rfd::FileDialog::new().add_filter(ext, &[ext]).set_file_name(format!("{stem}.{ext}")).save_file() else { return };
        let table: Box<dyn TableRead> = if visible_only {
            Box::new(SubTable { inner: d.snap(), rows: d.visible.clone(), cols: d.vcols.clone() })
        } else {
            Box::new(d.snap())
        };
        self.start_aux(ctx, "Exporting", move |_| match formats::export_file(table.as_ref(), &path, format, typed, None, None) {
            Ok(()) => JobResult::Done(format!("Exported {}", path.display())),
            Err(e) => JobResult::Error(format!("Export failed: {e}")),
        });
    }

    fn context_menu(&mut self, ui: &mut Ui) -> Option<Cmd> {
        let mut out = None;
        let mut item = |ui: &mut Ui, c: Cmd| {
            if ui.button(c.name()).clicked() {
                out = Some(c);
                ui.close();
            }
        };
        for c in [Cmd::Copy, Cmd::CopyWithHeader, Cmd::Cut, Cmd::Paste, Cmd::Clear] {
            item(ui, c);
        }
        ui.separator();
        for c in [Cmd::InsertRowsBefore, Cmd::InsertRowsAfter, Cmd::InsertColsBefore, Cmd::InsertColsAfter, Cmd::RemoveRows, Cmd::RemoveCols] {
            item(ui, c);
        }
        ui.separator();
        for c in [Cmd::SortColAsc, Cmd::SortColDesc, Cmd::FilterSelContents, Cmd::FilterSelCol, Cmd::HideRows, Cmd::HideCols] {
            item(ui, c);
        }
        ui.separator();
        for c in [Cmd::UniqueValues, Cmd::StatsSelCols, Cmd::SetHeaderRow, Cmd::FollowLinks] {
            item(ui, c);
        }
        out
    }
}

/// Like native menu bars: while one top-level menu is open, hovering another title switches to it.
pub fn menu_hover_switch(ctx: &Context, tops: &[egui::Response]) {
    let ids: Vec<egui::Id> = tops.iter().map(egui::Popup::default_response_id).collect();
    if let Some(open) = ids.iter().position(|&id| egui::Popup::is_id_open(ctx, id)) {
        let pointer = ctx.pointer_hover_pos();
        if let Some(k) = tops.iter().position(|r| pointer.is_some_and(|p| r.rect.contains(p))) {
            if k != open {
                egui::Popup::close_id(ctx, ids[open]);
                egui::Popup::open_id(ctx, ids[k]);
                ctx.request_repaint();
            }
        }
    }
}

fn trim_f(x: f64) -> String {
    let s = format!("{x:.4}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn reveal(p: &Path) -> Result<(), String> {
    let r = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg("-R").arg(p).spawn()
    } else if cfg!(windows) {
        std::process::Command::new("explorer").arg(format!("/select,{}", p.display())).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(p.parent().unwrap_or(p)).spawn()
    };
    r.map(|_| ()).map_err(|e| e.to_string())
}

fn open_in_editor(p: &Path, editor: &str) -> Result<(), String> {
    let r = if !editor.trim().is_empty() {
        std::process::Command::new(editor.trim()).arg(p).spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg("-t").arg(p).spawn()
    } else if cfg!(windows) {
        std::process::Command::new("notepad").arg(p).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(p).spawn()
    };
    r.map(|_| ()).map_err(|e| e.to_string())
}

fn open_terminal(dir: &Path) -> Result<(), String> {
    let r = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg("-a").arg("Terminal").arg(dir).spawn()
    } else if cfg!(windows) {
        std::process::Command::new("cmd").args(["/c", "start", "cmd", "/K", "cd", "/d"]).arg(dir).spawn()
    } else {
        std::process::Command::new("x-terminal-emulator").current_dir(dir).spawn()
    };
    r.map(|_| ()).map_err(|e| e.to_string())
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.apply_theme(&ctx);
        let title = match self.docs.get(self.active) {
            Some(d) => format!("{}{} — {APP_NAME}", d.title(), if d.dirty { " •" } else { "" }),
            None => APP_NAME.to_string(),
        };
        if self.window_title != title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }

        // Quit confirmation for unsaved files.
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_quit {
            let dirty: Vec<usize> = (0..self.docs.len()).filter(|&i| self.docs[i].dirty).collect();
            if !dirty.is_empty() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.dialog = Some(Dialog::ConfirmClose { tabs: dirty, quit: true });
            }
        }
        // Drag and drop.
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        for p in dropped {
            if p.is_dir() {
                self.settings.folders.push(p.to_string_lossy().into_owned());
                self.settings.show_sidebar = true;
                self.rescan_folders();
            } else {
                self.open_path(&ctx, &p);
            }
        }

        self.poll_jobs(&ctx);
        self.update_views(&ctx);
        self.check_external_changes(&ctx);
        self.handle_shortcuts(&ctx);

        let v = ctx.global_style().visuals.clone();
        let bar = |m: egui::Margin| egui::Frame::new().fill(v.panel_fill).inner_margin(m);
        let line = egui::Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color);
        egui::Panel::top("menu").frame(bar(egui::Margin::symmetric(8, 2))).show(ui, |ui| self.menu_bar(ui));
        if self.settings.show_toolbar {
            egui::Panel::top("toolbar").frame(bar(egui::Margin::symmetric(8, 3))).show(ui, |ui| self.toolbar(ui));
        }
        if !self.docs.is_empty() {
            egui::Panel::top("tabs").frame(bar(egui::Margin { left: 8, right: 8, top: 2, bottom: 0 })).show(ui, |ui| self.tab_bar(ui));
            if self.settings.show_metadata {
                egui::Panel::top("meta").frame(bar(egui::Margin::symmetric(12, 6)).stroke(line)).show(ui, |ui| self.metadata_bar(ui));
            }
            if self.find_open {
                egui::Panel::top("find").frame(bar(egui::Margin::symmetric(10, 6)).stroke(line)).show(ui, |ui| self.find_bar(ui));
            }
        }
        egui::Panel::bottom("status").frame(bar(egui::Margin::symmetric(12, 4)).stroke(line)).show(ui, |ui| self.status_bar(ui));
        if self.settings.show_editor_pane && !self.docs.is_empty() {
            egui::Panel::bottom("editor").frame(bar(egui::Margin::same(10)).stroke(line)).resizable(true).default_size(140.0).show(ui, |ui| self.editor_pane(ui));
        }
        if self.settings.show_sidebar {
            egui::Panel::left("sidebar").frame(bar(egui::Margin::same(10)).stroke(line)).resizable(true).default_size(220.0).show(ui, |ui| self.sidebar(ui));
        }

        let find_re = if self.find_open && self.find.highlight { self.find_regex() } else { None };
        let mut ev = None;
        let mut ctx_cmd = None;
        if self.docs.is_empty() {
            egui::CentralPanel::default().frame(egui::Frame::new().fill(v.extreme_bg_color)).show(ui, |ui| self.welcome(ui));
        } else {
            egui::CentralPanel::no_frame().show(ui, |ui| {
                if let Some(d) = self.docs.get_mut(self.active) {
                    let out = grid::show(ui, d, &self.settings, find_re.as_ref());
                    ev = out.event;
                    out.response.context_menu(|ui| ctx_cmd = self.context_menu(ui));
                }
            });
        }
        match ev {
            Some(GridEvent::Sort(c)) => {
                let col_id = self.docs[self.active].cols[c];
                let asc = !matches!(self.docs[self.active].sort, Some((id, true)) if id == col_id);
                self.sort_by(&ctx, c, asc);
            }
            Some(GridEvent::Message(m)) => self.msg(m),
            Some(GridEvent::Edited) => self.after_edit(&ctx),
            None => {}
        }
        if let Some(c) = ctx_cmd {
            self.run(&ctx, c);
        }

        if let Some(AnalysisAction::FilterValues { col, values, keep }) = self.analysis.show(&ctx) {
            if let Some(d) = self.docs.get_mut(self.active) {
                let c = d.vcol_to_col(col);
                if c < d.ncols() {
                    let terms: Vec<String> = values.iter().map(|v| format!("{}={}", if keep { "" } else { "!" }, escape_value(v))).collect();
                    let text = terms.join(if keep { " | " } else { " & " });
                    d.filters.push(Filter { col: Some(d.cols[c]), text });
                    d.view_dirty = true;
                }
            }
        }
        self.palette_ui(&ctx);
        self.prompt_ui(&ctx);
        self.dialogs(&ctx);
        if let Some((m, t)) = &self.status {
            let low = m.to_lowercase();
            let warn = ["fail", "could not", "can't", "cannot", "error", "too large", "not found", "invalid"].iter().any(|w| low.contains(w));
            ui_kit::toast(&ctx, m, t.elapsed().as_secs_f32(), warn);
        }

        if self.docs.iter().any(|d| !d.ready() || d.view_job.is_some() || d.aux_job.is_some()) {
            ctx.request_repaint_after(Duration::from_millis(100));
        } else {
            ctx.request_repaint_after(Duration::from_secs(2));
        }
    }

    fn on_exit(&mut self) {
        self.settings.save();
    }
}
