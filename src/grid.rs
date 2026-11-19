//! Custom virtualised table. Vertical scrolling is tracked as a row index (not pixels)
//! so files with hundreds of millions of rows scroll precisely.

use egui::{
    pos2, vec2, Align2, Color32, CursorIcon, Event, EventFilter, FontId, Id, Key, Rect, Response, Sense, Stroke,
    StrokeKind, TextEdit, Ui,
};
use regex::Regex;

use crate::doc::{CellEdit, Document, Drag};
use crate::rtl;
use crate::settings::Settings;
use crate::table::col_letter;

const LABEL_H: f32 = 20.0;
const SB: f32 = 12.0;
const MIN_COL_W: f32 = 24.0;
pub const MAX_COPY_CELLS: usize = 5_000_000;
pub const HEADER_ROW: usize = usize::MAX;

pub fn grid_id() -> Id {
    Id::new("ficsv-grid")
}

pub enum GridEvent {
    Sort(usize),
    Message(String),
    Edited,
}

pub struct GridOut {
    pub event: Option<GridEvent>,
    pub response: Response,
}

fn display_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(256));
    for (i, ch) in s.chars().enumerate() {
        if i >= 300 {
            out.push('…');
            break;
        }
        out.push(match ch {
            '\n' | '\r' => '⏎',
            '\t' => ' ',
            c => c,
        });
    }
    out
}

fn looks_numeric(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty() && t.len() < 32 && t.parse::<f64>().is_ok()
}

fn tsv_escape(v: &str, out: &mut String) {
    if v.contains(['\t', '\n', '"']) {
        out.push('"');
        out.push_str(&v.replace('"', "\"\""));
        out.push('"');
    } else {
        out.push_str(v);
    }
}

/// Copy the selection as TSV. Returns an error message if it is too large.
pub fn copy_selection(ctx: &egui::Context, doc: &Document, with_header: bool) -> Result<(), String> {
    let nrows = doc.vrows();
    if nrows == 0 || doc.vcols.is_empty() {
        return Ok(());
    }
    let ((r0, c0), (r1, c1)) = doc.sel_range();
    let r1 = r1.min(nrows - 1);
    let c1 = c1.min(doc.vcols.len() - 1);
    let cells = (r1 + 1 - r0) * (c1 + 1 - c0);
    if cells > MAX_COPY_CELLS {
        return Err(format!("Selection too large to copy ({cells} cells)"));
    }
    let snap = doc.snap();
    let cols: Vec<usize> = (c0..=c1).map(|vc| doc.vcols[vc]).collect();
    let mut out = String::new();
    if with_header && doc.has_header {
        let h = snap.header_names();
        for (i, &c) in cols.iter().enumerate() {
            if i > 0 {
                out.push('\t');
            }
            tsv_escape(h.get(c).map(String::as_str).unwrap_or(""), &mut out);
        }
        out.push('\n');
    }
    for vr in r0..=r1 {
        let vals = snap.row_values(doc.data_row_id(doc.vrow_to_data(vr)));
        for (i, &c) in cols.iter().enumerate() {
            if i > 0 {
                out.push('\t');
            }
            tsv_escape(vals.get(c).map(String::as_str).unwrap_or(""), &mut out);
        }
        out.push('\n');
    }
    ctx.copy_text(out);
    Ok(())
}

/// Parse clipboard text (TSV with optional quotes, or CSV if no tabs).
pub fn parse_clipboard(text: &str) -> Vec<Vec<String>> {
    let text = text.trim_end_matches(['\n', '\r']);
    let delim = if text.contains('\t') { b'\t' } else if text.lines().count() > 1 && text.contains(',') { b',' } else { b'\t' };
    let mut rows = Vec::new();
    // Re-use the CSV field parser on each logical line, respecting quoted newlines.
    let bytes = text.as_bytes();
    let mut start = 0;
    let mut in_q = false;
    for i in 0..=bytes.len() {
        let end = i == bytes.len();
        if !end && bytes[i] == b'"' {
            in_q = !in_q;
        }
        if end || (bytes[i] == b'\n' && !in_q) {
            let mut line = &bytes[start..i];
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            rows.push(crate::doc::Fields::new(line, delim, true).map(|f| String::from_utf8_lossy(&f).into_owned()).collect());
            start = i + 1;
        }
    }
    rows
}

pub fn paste(doc: &mut Document, text: &str) {
    if doc.vrows() == 0 || doc.vcols.is_empty() {
        return;
    }
    let ((r0, c0), (r1, c1)) = doc.sel_range();
    let block = parse_clipboard(text);
    if block.is_empty() {
        return;
    }
    let single = block.len() == 1 && block[0].len() == 1;
    let mut items = Vec::new();
    let mut max_r = r0;
    let mut max_c = c0;
    if single && (r0, c0) != (r1, c1) {
        // One value pasted into a multi-cell selection fills it.
        for vr in r0..=r1.min(doc.vrows() - 1) {
            for vc in c0..=c1.min(doc.vcols.len() - 1) {
                items.push((doc.vrow_to_data(vr), doc.vcols[vc], block[0][0].clone()));
            }
        }
        max_r = r1;
        max_c = c1;
    } else {
        // Grow the table to the right if needed.
        let need_cols = c0 + block.iter().map(|r| r.len()).max().unwrap_or(0);
        if need_cols > doc.vcols.len() && doc.ready() {
            let extra = need_cols - doc.vcols.len();
            doc.insert_cols(doc.ncols(), extra);
        }
        for (i, row) in block.iter().enumerate() {
            let vr = r0 + i;
            if vr >= doc.vrows() {
                break;
            }
            for (j, v) in row.iter().enumerate() {
                let vc = c0 + j;
                if vc >= doc.vcols.len() {
                    break;
                }
                items.push((doc.vrow_to_data(vr), doc.vcols[vc], v.clone()));
                max_r = max_r.max(vr);
                max_c = max_c.max(vc);
            }
        }
    }
    doc.set_cells(items);
    doc.grid.anchor = (r0, c0);
    doc.grid.cur = (max_r, max_c);
}

pub fn clear_selection(doc: &mut Document) {
    let n = doc.vrows();
    if n == 0 || doc.vcols.is_empty() {
        return;
    }
    let ((r0, c0), (r1, c1)) = doc.sel_range();
    let r1 = r1.min(n - 1);
    let c1 = c1.min(doc.vcols.len() - 1);
    if (r1 + 1 - r0) * (c1 + 1 - c0) > MAX_COPY_CELLS {
        return;
    }
    let mut items = Vec::new();
    for vr in r0..=r1 {
        let d = doc.vrow_to_data(vr);
        for vc in c0..=c1 {
            items.push((d, doc.vcols[vc], String::new()));
        }
    }
    doc.set_cells(items);
}

pub fn start_edit(doc: &mut Document, text: Option<String>) {
    if doc.read_only || doc.vrows() == 0 || doc.vcols.is_empty() {
        return;
    }
    let (row, col) = doc.grid.cur;
    let text = text.unwrap_or_else(|| doc.cell(doc.vrow_to_data(row), doc.vcols[col]));
    doc.grid.anchor = doc.grid.cur;
    doc.grid.edit = Some(CellEdit { row, col, text, needs_focus: true });
}

fn commit_edit(doc: &mut Document) -> bool {
    if let Some(e) = doc.grid.edit.take() {
        let Some(&c) = doc.vcols.get(e.col) else { return false };
        if e.row == HEADER_ROW {
            doc.set_cells_t(vec![(0, c, e.text)]);
            return true;
        }
        if e.row < doc.vrows() {
            let d = doc.vrow_to_data(e.row);
            doc.set_cells(vec![(d, c, e.text)]);
            return true;
        }
    }
    false
}

pub fn show(ui: &mut Ui, doc: &mut Document, s: &Settings, find_re: Option<&Regex>) -> GridOut {
    let mut event = None;
    let row_h = s.row_height.max(s.font_size + 6.0);
    let measure_font = FontId::proportional(s.font_size);
    let painter_for_measure = ui.painter().clone();
    let measure = move |t: &str| -> f32 {
        let shown = rtl::visual(&display_text(t)).into_owned();
        painter_for_measure.layout_no_wrap(shown, measure_font.clone(), Color32::WHITE).size().x
    };
    doc.measure_if_needed(&measure, s.max_autofit_width.max(240.0));
    // Explicit auto-fit (double-click / command) fits the content, up to a generous cap.
    for c in std::mem::take(&mut doc.grid.autofit) {
        if c < doc.ncols() {
            doc.autofit_col(c, &measure, 3000.0);
        }
    }

    let rect = ui.available_rect_before_wrap();
    let resp = ui.interact(rect, grid_id(), Sense::click_and_drag());
    ui.allocate_rect(rect, Sense::hover());
    let painter = ui.painter_at(rect);
    let vis = ui.visuals().clone();

    let nrows = doc.vrows();
    let ncols_v = doc.vcols.len();
    let digits = ((doc.nrows().max(1) as f64 + s.first_row_index.max(0) as f64).log10().floor() as usize) + 1;
    let rn_w = if s.show_row_labels { 16.0 + digits as f32 * (s.font_size * 0.6) } else { 0.0 };
    let label_h = if s.show_col_labels || !doc.has_header { LABEL_H } else { 0.0 };
    let head_h = if doc.has_header { row_h + 2.0 } else { 0.0 };

    let grid = Rect::from_min_max(rect.min, pos2(rect.right() - SB, rect.bottom() - SB));
    let body = Rect::from_min_max(pos2(grid.left() + rn_w, grid.top() + label_h + head_h), grid.max);
    let vs_rect = Rect::from_min_max(pos2(grid.right(), rect.top()), pos2(rect.right(), grid.bottom()));
    let hs_rect = Rect::from_min_max(pos2(rect.left(), grid.bottom()), pos2(grid.right(), rect.bottom()));

    let vis_rows = ((body.height() / row_h).floor() as usize).max(1);
    doc.grid.vis_rows = vis_rows;
    doc.grid.body_w = body.width();
    let max_top = nrows.saturating_sub(vis_rows);

    let widths = doc.col_widths_visible();
    let mut prefix = Vec::with_capacity(widths.len() + 1);
    prefix.push(0.0f32);
    for w in &widths {
        prefix.push(prefix.last().unwrap() + w);
    }
    let total_w = *prefix.last().unwrap();
    let max_sx = (total_w - body.width() + 40.0).max(0.0);

    // ---------- Mouse wheel ----------
    if resp.hovered() || resp.contains_pointer() {
        let d = ui.input(|i| i.smooth_scroll_delta);
        if d.y != 0.0 {
            doc.grid.scroll_acc -= d.y / row_h;
            let whole = doc.grid.scroll_acc.trunc();
            doc.grid.scroll_acc -= whole;
            let top = doc.grid.top as i64 + whole as i64;
            doc.grid.top = top.clamp(0, max_top as i64) as usize;
        }
        if d.x != 0.0 {
            doc.grid.scroll_x -= d.x;
        }
    }
    doc.grid.top = doc.grid.top.min(max_top);
    doc.grid.scroll_x = doc.grid.scroll_x.clamp(0.0, max_sx);

    let col_at = |x: f32, sx: f32| -> Option<usize> {
        let x = x - body.left() + sx;
        if x < 0.0 {
            return None;
        }
        let i = prefix.partition_point(|&p| p <= x);
        (i >= 1 && i - 1 < ncols_v).then(|| i - 1)
    };
    let border_at = |x: f32, sx: f32| -> Option<usize> {
        if x <= body.left() {
            return None;
        }
        (0..ncols_v).find(|&c| (body.left() + prefix[c + 1] - sx - x).abs() < 6.0)
    };

    // ---------- Pointer ----------
    let (pointer, pressed, down, mods) =
        ui.input(|i| (i.pointer.interact_pos(), i.pointer.primary_pressed(), i.pointer.primary_down(), i.modifiers));
    let sx = doc.grid.scroll_x;
    let top_strip = grid.top() + label_h + head_h;

    if let Some(p) = pointer.filter(|_| resp.hovered()) {
        if p.y >= grid.top() && p.y < top_strip && border_at(p.x, sx).is_some() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        }
    }

    if pressed && resp.hovered() {
        if let Some(p) = pointer {
            if doc.grid.edit.is_some() && commit_edit(doc) {
                event = Some(GridEvent::Edited);
            }
            resp.request_focus();
            if vs_rect.contains(p) {
                doc.grid.drag = Drag::VScroll;
            } else if hs_rect.contains(p) {
                doc.grid.drag = Drag::HScroll;
            } else if p.y < top_strip {
                if let Some(c) = border_at(p.x, sx) {
                    let id = doc.cols[doc.vcols[c]];
                    // Second press on the same border within 0.45 s = double-click: auto-fit.
                    let now = ui.input(|i| i.time);
                    if doc.grid.last_border_press.is_some_and(|(t, lc)| lc == c && now - t < 0.45) {
                        doc.grid.autofit.push(doc.vcols[c]);
                        doc.grid.last_border_press = None;
                        ui.ctx().request_repaint();
                    } else {
                        doc.grid.last_border_press = Some((now, c));
                    }
                    // Remember the column's left edge in content coordinates.
                    doc.grid.drag = Drag::Resize(id, prefix[c]);
                } else if p.x < body.left() {
                    doc.grid.anchor = (0, 0);
                    doc.grid.cur = (nrows.saturating_sub(1), ncols_v.saturating_sub(1));
                } else if let Some(c) = col_at(p.x, sx) {
                    if mods.shift {
                        doc.grid.cur.1 = c;
                    } else {
                        doc.grid.anchor = (nrows.saturating_sub(1), c);
                        doc.grid.cur = (doc.grid.top.min(nrows.saturating_sub(1)), c);
                    }
                }
            } else if p.x < body.left() {
                let r = doc.grid.top + ((p.y - body.top()) / row_h) as usize;
                if r < nrows {
                    if mods.shift {
                        doc.grid.cur.0 = r;
                    } else {
                        doc.grid.anchor = (r, ncols_v.saturating_sub(1));
                        doc.grid.cur = (r, 0);
                    }
                }
            } else {
                let r = doc.grid.top + ((p.y - body.top()) / row_h) as usize;
                if let (true, Some(c)) = (r < nrows, col_at(p.x, sx)) {
                    doc.grid.cur = (r, c);
                    if !mods.shift {
                        doc.grid.anchor = (r, c);
                    }
                    doc.grid.drag = Drag::Select;
                }
            }
        }
    }

    if down {
        if let Some(p) = pointer {
            match doc.grid.drag {
                Drag::Resize(id, left) => {
                    // Auto-scroll when dragging past the right edge so the column can keep growing.
                    let edge = body.right() - 12.0;
                    if p.x > edge {
                        doc.grid.scroll_x += ((p.x - edge) * 0.6).clamp(6.0, 60.0);
                        ui.ctx().request_repaint();
                    } else if p.x < body.left() + 12.0 && doc.grid.scroll_x > 0.0 {
                        doc.grid.scroll_x = (doc.grid.scroll_x - 20.0).max(0.0);
                        ui.ctx().request_repaint();
                    }
                    let content_x = p.x.min(body.right()) - body.left() + doc.grid.scroll_x;
                    doc.widths.insert(id, (content_x - left).max(MIN_COL_W));
                }
                Drag::VScroll => {
                    let thumb_h = (vs_rect.height() * vis_rows as f32 / nrows.max(1) as f32).clamp(24.0, vs_rect.height());
                    let t = ((p.y - vs_rect.top() - thumb_h / 2.0) / (vs_rect.height() - thumb_h).max(1.0)).clamp(0.0, 1.0);
                    doc.grid.top = (t as f64 * max_top as f64).round() as usize;
                }
                Drag::HScroll => {
                    let thumb_w = (hs_rect.width() * body.width() / total_w.max(1.0)).clamp(24.0, hs_rect.width());
                    let t = ((p.x - hs_rect.left() - thumb_w / 2.0) / (hs_rect.width() - thumb_w).max(1.0)).clamp(0.0, 1.0);
                    doc.grid.scroll_x = t * max_sx;
                }
                Drag::Select => {
                    if p.y > body.bottom() && doc.grid.top < max_top {
                        doc.grid.top += 1;
                    } else if p.y < body.top() && doc.grid.top > 0 {
                        doc.grid.top -= 1;
                    }
                    if p.x > body.right() {
                        doc.grid.scroll_x = (doc.grid.scroll_x + 20.0).min(max_sx);
                    } else if p.x < body.left() {
                        doc.grid.scroll_x = (doc.grid.scroll_x - 20.0).max(0.0);
                    }
                    let r = (doc.grid.top as i64 + ((p.y - body.top()) / row_h).floor() as i64)
                        .clamp(0, nrows.saturating_sub(1) as i64) as usize;
                    let c = col_at(p.x.clamp(body.left(), body.right() - 1.0), doc.grid.scroll_x)
                        .unwrap_or(ncols_v.saturating_sub(1));
                    doc.grid.cur = (r, c);
                    ui.ctx().request_repaint();
                }
                Drag::None => {}
            }
        }
    } else {
        doc.grid.drag = Drag::None;
    }

    if resp.double_clicked() {
        if let Some(p) = pointer {
            if p.y < top_strip {
                if border_at(p.x, sx).is_some() {
                    // Handled by the border double-press detection above.
                } else if let Some(c) = col_at(p.x, sx) {
                    let on_header_row = doc.has_header && p.y >= grid.top() + label_h;
                    if on_header_row && label_h > 0.0 && !doc.read_only {
                        let text = doc.cell_t(0, doc.vcols[c]);
                        doc.grid.edit = Some(CellEdit { row: HEADER_ROW, col: c, text, needs_focus: true });
                    } else {
                        event = Some(GridEvent::Sort(doc.vcols[c]));
                    }
                }
            } else if body.contains(p) {
                start_edit(doc, None);
            }
        }
    }

    // ---------- Keyboard (navigation + editing; other shortcuts are app commands) ----------
    let focused = ui.memory(|m| m.focused());
    if focused == Some(resp.id) {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(resp.id, EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true })
        });
    }
    let has_kb = doc.grid.edit.is_none() && (focused.is_none() || focused == Some(resp.id));
    if has_kb && nrows > 0 && ncols_v > 0 {
        let events = ui.input(|i| i.events.clone());
        let last_r = nrows - 1;
        let last_c = ncols_v - 1;
        for ev in events {
            match ev {
                Event::Copy => {
                    if let Err(m) = copy_selection(ui.ctx(), doc, false) {
                        event = Some(GridEvent::Message(m));
                    }
                }
                Event::Cut => match copy_selection(ui.ctx(), doc, false) {
                    Err(m) => event = Some(GridEvent::Message(m)),
                    Ok(()) if !doc.read_only => {
                        clear_selection(doc);
                        event = Some(GridEvent::Edited);
                    }
                    Ok(()) => {}
                },
                Event::Paste(t) if !doc.read_only => {
                    paste(doc, &t);
                    event = Some(GridEvent::Edited);
                }
                Event::Text(t) if !mods.command && !mods.ctrl && !doc.read_only => {
                    start_edit(doc, Some(t));
                    break;
                }
                Event::Key { key, pressed: true, modifiers: m, .. } if !m.alt => {
                    let (mut r, mut c) = doc.grid.cur;
                    let mut moved = true;
                    match key {
                        Key::ArrowUp => r = if m.command { 0 } else { r.saturating_sub(1) },
                        Key::ArrowDown => r = if m.command { last_r } else { (r + 1).min(last_r) },
                        Key::ArrowLeft => c = if m.command { 0 } else { c.saturating_sub(1) },
                        Key::ArrowRight => c = if m.command { last_c } else { (c + 1).min(last_c) },
                        Key::PageUp => {
                            r = r.saturating_sub(vis_rows);
                            doc.grid.top = doc.grid.top.saturating_sub(vis_rows);
                        }
                        Key::PageDown => {
                            r = (r + vis_rows).min(last_r);
                            doc.grid.top = (doc.grid.top + vis_rows).min(max_top);
                        }
                        Key::Home => {
                            c = 0;
                            if m.command {
                                r = 0;
                            }
                        }
                        Key::End => {
                            c = last_c;
                            if m.command {
                                r = last_r;
                            }
                        }
                        Key::Tab => {
                            c = if m.shift { c.saturating_sub(1) } else { (c + 1).min(last_c) };
                            doc.select((r, c), (r, c));
                            continue;
                        }
                        Key::Enter | Key::F2 if !m.command => {
                            start_edit(doc, None);
                            break;
                        }
                        Key::E if m.command && !m.shift => {
                            start_edit(doc, None);
                            break;
                        }
                        Key::Delete | Key::Backspace if !m.command && !doc.read_only => {
                            clear_selection(doc);
                            event = Some(GridEvent::Edited);
                            moved = false;
                        }
                        Key::Escape => {
                            doc.grid.anchor = doc.grid.cur;
                            moved = false;
                        }
                        Key::A if m.command => {
                            doc.grid.anchor = (0, 0);
                            doc.grid.cur = (last_r, last_c);
                            moved = false;
                        }
                        _ => moved = false,
                    }
                    if moved {
                        doc.grid.cur = (r, c);
                        if !m.shift {
                            doc.grid.anchor = (r, c);
                        }
                        doc.ensure_visible();
                    }
                }
                _ => {}
            }
        }
    }
    let sx = doc.grid.scroll_x;
    let top = doc.grid.top;

    // ---------- Paint ----------
    let text_col = vis.text_color();
    let weak_col = vis.weak_text_color();
    let line_col = vis.widgets.noninteractive.bg_stroke.color;
    let head_bg = vis.widgets.inactive.weak_bg_fill;
    let label_bg = vis.panel_fill;
    let stripe = vis.faint_bg_color;
    let accent = vis.selection.stroke.color;
    let sel_fill = vis.selection.bg_fill.gamma_multiply(0.45);
    let cross_fill = vis.selection.bg_fill.gamma_multiply(0.15);
    let cur_stroke = Stroke::new(2.0, accent);
    let find_fill = Color32::from_rgba_unmultiplied(255, 190, 0, 80);
    let edited_col = Color32::from_rgb(230, 140, 30);
    let font = FontId::proportional(s.font_size);
    let bold = FontId::new(s.font_size, egui::FontFamily::Name("bold".into()));
    let bold = if ui.fonts(|f| f.families().iter().any(|fam| *fam == bold.family)) { bold } else { font.clone() };
    let small = FontId::proportional((s.font_size - 2.0).max(9.0));
    let mono = FontId::monospace((s.font_size - 1.5).max(9.0));

    painter.rect_filled(rect, 0.0, vis.extreme_bg_color);

    let first_col = prefix.partition_point(|&p| p <= sx).saturating_sub(1);
    let mut last_col = first_col;
    while last_col < ncols_v && prefix[last_col] - sx < body.width() {
        last_col += 1;
    }
    let ((sr0, sc0), (sr1, sc1)) = doc.sel_range();
    let multi = (sr0, sc0) != (sr1, sc1);
    let cur = doc.grid.cur;
    let body_painter = painter.with_clip_rect(body);
    let row_end = (top + vis_rows + 1).min(nrows);
    let snap = doc.snap();
    let edits_any = !doc.edits.is_empty() || doc.rows.is_some();

    let mut widest_disk = 0usize;
    for vr in top..row_end {
        let y = body.top() + (vr - top) as f32 * row_h;
        let d = doc.vrow_to_data(vr);
        if d == usize::MAX || d >= doc.nrows() {
            continue;
        }
        let id = doc.data_row_id(d);
        if doc.rows.is_none() {
            widest_disk = widest_disk.max(snap.disk_fields(id).len());
        }
        let vals = snap.row_values(id);
        let row_rect = Rect::from_min_size(pos2(body.left(), y), vec2(body.width(), row_h));
        if s.shade_rows && vr % 2 == 1 {
            body_painter.rect_filled(row_rect, 0.0, stripe);
        }
        if s.highlight_cursor_cross && !multi && vr == cur.0 {
            body_painter.rect_filled(row_rect, 0.0, cross_fill);
        }
        for vc in first_col..last_col {
            let x = body.left() + prefix[vc] - sx;
            let cell = Rect::from_min_size(pos2(x, y), vec2(widths[vc], row_h));
            if s.shade_cols && vc % 2 == 1 {
                body_painter.rect_filled(cell, 0.0, stripe);
            }
            if s.highlight_cursor_cross && !multi && vc == cur.1 && vr != cur.0 {
                body_painter.rect_filled(cell, 0.0, cross_fill);
            }
            if multi && vr >= sr0 && vr <= sr1 && vc >= sc0 && vc <= sc1 {
                body_painter.rect_filled(cell, 0.0, sel_fill);
            }
            let c = doc.vcols[vc];
            let Some(v) = vals.get(c) else { continue };
            if edits_any && doc.is_edited(id, doc.cols[c]) {
                body_painter.rect_filled(
                    Rect::from_min_size(cell.left_top() + vec2(1.0, 2.0), vec2(2.5, row_h - 4.0)),
                    0.0,
                    edited_col,
                );
            }
            if v.is_empty() {
                continue;
            }
            if let Some(re) = find_re {
                if re.is_match(v) {
                    body_painter.rect_filled(cell, 0.0, find_fill);
                }
            }
            let clip = cell.shrink2(vec2(5.0, 0.0)).intersect(body);
            let p = painter.with_clip_rect(clip);
            if looks_numeric(v) {
                p.text(pos2(clip.right(), cell.center().y), Align2::RIGHT_CENTER, v.trim(), font.clone(), text_col);
            } else if rtl::has_rtl(v) {
                // Shaped + bidi-reordered for display; right-aligned when the text starts RTL.
                let shown = rtl::visual(&display_text(v)).into_owned();
                if rtl::is_rtl_paragraph(v) {
                    p.text(pos2(clip.right(), cell.center().y), Align2::RIGHT_CENTER, shown, font.clone(), text_col);
                } else {
                    p.text(pos2(clip.left(), cell.center().y), Align2::LEFT_CENTER, shown, font.clone(), text_col);
                }
            } else {
                p.text(pos2(clip.left(), cell.center().y), Align2::LEFT_CENTER, display_text(v), font.clone(), text_col);
            }
        }
    }
    if widest_disk > doc.disk_cols as usize {
        doc.ensure_disk_cols(widest_disk);
    }

    // Grid lines
    let line = Stroke::new(1.0, line_col);
    for vc in first_col..=last_col.min(ncols_v) {
        let x = (body.left() + prefix[vc] - sx).round() - 0.5;
        if x >= body.left() {
            painter.vline(x, grid.top()..=grid.bottom(), line);
        }
    }
    for i in 0..=(row_end.saturating_sub(top)) {
        let y = (body.top() + i as f32 * row_h).round() - 0.5;
        painter.hline(grid.left()..=grid.right(), y, Stroke::new(1.0, line_col.gamma_multiply(0.5)));
    }

    // Cursor
    if cur.0 >= top && cur.0 < row_end && cur.1 < ncols_v {
        let x = body.left() + prefix[cur.1] - sx;
        let y = body.top() + (cur.0 - top) as f32 * row_h;
        let cell = Rect::from_min_size(pos2(x, y), vec2(widths[cur.1], row_h));
        body_painter.rect_stroke(cell.shrink(1.0), 0.0, cur_stroke, StrokeKind::Inside);
    }

    // Column label strip + header row
    let strip = Rect::from_min_max(pos2(body.left(), grid.top()), pos2(grid.right(), body.top()));
    painter.rect_filled(Rect::from_min_max(grid.left_top(), strip.right_bottom()), 0.0, label_bg);
    let names = if doc.has_header { snap.header_names() } else { Vec::new() };
    let strip_painter = painter.with_clip_rect(strip);
    for vc in first_col..last_col {
        let x = body.left() + prefix[vc] - sx;
        let c = doc.vcols[vc];
        let col_id = doc.cols[c];
        let sorted = doc.sort.filter(|(sc, _)| *sc == col_id).map(|(_, asc)| if asc { " ▲" } else { " ▼" }).unwrap_or("");
        let filtered = if doc.filters.iter().any(|f| f.col == Some(col_id)) { " ⏷" } else { "" };
        let selected = vc >= sc0 && vc <= sc1;
        if label_h > 0.0 {
            let cell = Rect::from_min_size(pos2(x, grid.top()), vec2(widths[vc], label_h));
            if selected {
                strip_painter.rect_filled(cell, 0.0, sel_fill);
            }
            let mut label = if s.col_letters { col_letter(c) } else { (c as i64 + s.first_col_index).to_string() };
            if !doc.has_header {
                label.push_str(sorted);
                label.push_str(filtered);
            }
            painter.with_clip_rect(cell.intersect(strip)).text(cell.center(), Align2::CENTER_CENTER, label, small.clone(), weak_col);
            strip_painter.vline(cell.right().round() - 0.5, cell.y_range(), line);
        }
        if doc.has_header {
            let cell = Rect::from_min_size(pos2(x, grid.top() + label_h), vec2(widths[vc], head_h));
            strip_painter.rect_filled(cell, 0.0, head_bg);
            if selected {
                strip_painter.rect_filled(cell, 0.0, sel_fill);
            }
            let raw_name = names.get(c).map(String::as_str).unwrap_or("");
            let mut label = rtl::visual(&display_text(raw_name)).into_owned();
            label.push_str(sorted);
            label.push_str(filtered);
            let clip = cell.shrink2(vec2(6.0, 0.0)).intersect(strip);
            let color = if sorted.is_empty() { text_col } else { accent };
            painter.with_clip_rect(clip).text(pos2(clip.left(), cell.center().y), Align2::LEFT_CENTER, label, bold.clone(), color);
            strip_painter.vline(cell.right().round() - 0.5, cell.y_range(), line);
        }
    }
    if label_h > 0.0 && head_h > 0.0 {
        painter.hline(grid.left()..=grid.right(), grid.top() + label_h - 0.5, line);
    }
    painter.hline(grid.left()..=grid.right(), body.top() - 0.5, Stroke::new(1.0, accent.gamma_multiply(0.5)));

    // Row labels
    if rn_w > 0.0 {
        let rn = Rect::from_min_max(pos2(grid.left(), body.top()), pos2(body.left(), grid.bottom()));
        painter.rect_filled(rn, 0.0, label_bg);
        for vr in top..row_end {
            let y = body.top() + (vr - top) as f32 * row_h;
            let cell = Rect::from_min_size(pos2(rn.left(), y), vec2(rn_w, row_h));
            if vr >= sr0 && vr <= sr1 {
                painter.rect_filled(cell, 0.0, sel_fill);
            }
            let d = doc.vrow_to_data(vr);
            let label = (d as i64 + s.first_row_index).to_string();
            painter.text(pos2(cell.right() - 6.0, cell.center().y), Align2::RIGHT_CENTER, label, mono.clone(), weak_col);
        }
        painter.vline(body.left() - 0.5, grid.y_range(), line);
    }

    // Scrollbars
    let thumb = vis.widgets.inactive.fg_stroke.color.gamma_multiply(0.45);
    painter.rect_filled(vs_rect, 0.0, vis.extreme_bg_color);
    painter.rect_filled(hs_rect, 0.0, vis.extreme_bg_color);
    painter.rect_filled(Rect::from_min_max(grid.right_bottom(), rect.right_bottom()), 0.0, label_bg);
    if max_top > 0 {
        let h = (vs_rect.height() * vis_rows as f32 / nrows as f32).clamp(24.0, vs_rect.height());
        let t = top as f64 / max_top as f64;
        let y = vs_rect.top() + (vs_rect.height() - h) * t as f32;
        painter.rect_filled(Rect::from_min_size(pos2(vs_rect.left() + 2.0, y), vec2(SB - 4.0, h)), 4.0, thumb);
    }
    if max_sx > 0.0 {
        let w = (hs_rect.width() * body.width() / total_w.max(1.0)).clamp(24.0, hs_rect.width());
        let x = hs_rect.left() + (hs_rect.width() - w) * (sx / max_sx);
        painter.rect_filled(Rect::from_min_size(pos2(x, hs_rect.top() + 2.0), vec2(w, SB - 4.0)), 4.0, thumb);
    }

    if nrows == 0 && doc.ready() {
        let msg = if doc.has_view_filter() { "No rows match the filters" } else { "No rows" };
        painter.text(body.center(), Align2::CENTER_CENTER, msg, FontId::proportional(14.0), weak_col);
    }

    // ---------- In-place editor ----------
    let mut finish: Option<bool> = None;
    if let Some(edit) = &mut doc.grid.edit {
        let in_view = edit.col < ncols_v && (edit.row == HEADER_ROW || (edit.row >= top && edit.row < top + vis_rows));
        if !in_view {
            finish = Some(true);
        } else {
            let x = body.left() + prefix[edit.col] - sx;
            let y = if edit.row == HEADER_ROW { grid.top() + label_h } else { body.top() + (edit.row - top) as f32 * row_h };
            let cell = Rect::from_min_size(pos2(x, y), vec2(widths[edit.col].max(180.0), row_h));
            let te = ui.put(cell, TextEdit::singleline(&mut edit.text).font(font.clone()).margin(vec2(4.0, 2.0)));
            if edit.needs_focus {
                te.request_focus();
                edit.needs_focus = false;
            } else if te.lost_focus() {
                let esc = ui.input(|i| i.key_pressed(Key::Escape));
                finish = Some(!esc);
                resp.request_focus();
            }
        }
    }
    match finish {
        Some(true) => {
            let tab = ui.input(|i| i.key_pressed(Key::Tab));
            let enter = ui.input(|i| i.key_pressed(Key::Enter));
            let was_header = doc.grid.edit.as_ref().is_some_and(|e| e.row == HEADER_ROW);
            if commit_edit(doc) {
                event = Some(GridEvent::Edited);
            }
            if nrows > 0 && !was_header {
                if enter {
                    doc.grid.cur.0 = (doc.grid.cur.0 + 1).min(nrows - 1);
                } else if tab {
                    doc.grid.cur.1 = (doc.grid.cur.1 + 1).min(ncols_v.saturating_sub(1));
                }
                doc.grid.anchor = doc.grid.cur;
                doc.ensure_visible();
            }
        }
        Some(false) => doc.grid.edit = None,
        None => {}
    }

    GridOut { event, response: resp }
}
