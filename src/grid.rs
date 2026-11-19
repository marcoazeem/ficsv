//! Custom virtualised table. Scrolling is tracked as a row index (not pixels)
//! so files with hundreds of millions of rows scroll precisely.

use egui::{
    pos2, vec2, Align2, Color32, CursorIcon, Event, EventFilter, FontId, Key, Rect, Sense, Stroke,
    StrokeKind, TextEdit, Ui,
};

use crate::doc::{CellEdit, Document, Drag};
use crate::query::contains_ci;

pub const ROW_H: f32 = 22.0;
const HEAD_H: f32 = 24.0;
const SB: f32 = 12.0;
const MIN_COL_W: f32 = 28.0;
const MAX_COPY_CELLS: usize = 2_000_000;

pub enum GridEvent {
    Sort(usize),
    Message(String),
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

fn sel_range(doc: &Document) -> ((usize, usize), (usize, usize)) {
    let (a, b) = (doc.grid.anchor, doc.grid.cur);
    ((a.0.min(b.0), a.1.min(b.1)), (a.0.max(b.0), a.1.max(b.1)))
}

fn copy_selection(ui: &Ui, doc: &Document) -> Option<String> {
    let nrows = doc.view_len();
    if nrows == 0 {
        return None;
    }
    let ((r0, c0), (r1, c1)) = sel_range(doc);
    let r1 = r1.min(nrows - 1);
    let cells = (r1 + 1 - r0) * (c1 + 1 - c0);
    if cells > MAX_COPY_CELLS {
        return Some(format!("Selection too large to copy ({cells} cells)"));
    }
    let mut out = String::new();
    for r in r0..=r1 {
        let f = doc.row_fields(doc.rec_of(r));
        for c in c0..=c1 {
            if c > c0 {
                out.push('\t');
            }
            let v = f.get(c).map(String::as_str).unwrap_or("");
            if v.contains(['\t', '\n', '"']) {
                out.push('"');
                out.push_str(&v.replace('"', "\"\""));
                out.push('"');
            } else {
                out.push_str(v);
            }
        }
        out.push('\n');
    }
    ui.ctx().copy_text(out);
    None
}

fn paste(doc: &mut Document, text: &str) {
    let nrows = doc.view_len();
    if nrows == 0 {
        return;
    }
    let (r0, c0) = sel_range(doc).0;
    let lines: Vec<&str> = text.trim_end_matches(['\n', '\r']).split('\n').collect();
    let mut items = Vec::new();
    let mut max_c = c0;
    for (i, line) in lines.iter().enumerate() {
        let r = r0 + i;
        if r >= nrows {
            break;
        }
        let rec = doc.rec_of(r);
        for (j, v) in line.trim_end_matches('\r').split('\t').enumerate() {
            items.push((rec, c0 + j, v.to_string()));
            max_c = max_c.max(c0 + j);
        }
    }
    let last_r = (r0 + lines.len()).min(nrows) - 1;
    doc.set_cells(items);
    doc.grid.anchor = (r0, c0);
    doc.grid.cur = (last_r, max_c);
}

fn clear_selection(doc: &mut Document) {
    let nrows = doc.view_len();
    if nrows == 0 {
        return;
    }
    let ((r0, c0), (r1, c1)) = sel_range(doc);
    let r1 = r1.min(nrows - 1);
    if (r1 + 1 - r0) * (c1 + 1 - c0) > MAX_COPY_CELLS {
        return;
    }
    let mut items = Vec::new();
    for r in r0..=r1 {
        let rec = doc.rec_of(r);
        for c in c0..=c1 {
            items.push((rec, c, String::new()));
        }
    }
    doc.set_cells(items);
}

fn start_edit(doc: &mut Document, text: Option<String>) {
    if doc.view_len() == 0 {
        return;
    }
    let (row, col) = doc.grid.cur;
    let text = text.unwrap_or_else(|| doc.cell(doc.rec_of(row), col));
    doc.grid.anchor = doc.grid.cur;
    doc.grid.edit = Some(CellEdit { row, col, text, needs_focus: true });
}

fn commit_edit(doc: &mut Document) {
    if let Some(e) = doc.grid.edit.take() {
        if e.row < doc.view_len() {
            let rec = doc.rec_of(e.row);
            doc.set_cells(vec![(rec, e.col, e.text)]);
        }
    }
}

fn jump_to_change(doc: &mut Document, at: Option<(u32, u32)>) {
    if let Some((rec, col)) = at {
        if let Some(row) = doc.row_of_rec(rec as usize) {
            doc.grid.cur = (row, col as usize);
            doc.grid.anchor = doc.grid.cur;
            doc.ensure_visible();
        }
    }
}

pub fn show(ui: &mut Ui, doc: &mut Document, find_lc: &str) -> Option<GridEvent> {
    let mut event = None;
    doc.measure_if_needed();

    let rect = ui.available_rect_before_wrap();
    let resp = ui.allocate_rect(rect, Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let vis = ui.visuals().clone();

    let nrows = doc.view_len();
    let digits = ((doc.record_count().max(1)) as f64).log10().floor() as usize + 1;
    let rn_w = 18.0 + digits as f32 * 7.5;

    let grid = Rect::from_min_max(rect.min, pos2(rect.right() - SB, rect.bottom() - SB));
    let body = Rect::from_min_max(pos2(grid.left() + rn_w, grid.top() + HEAD_H), grid.max);
    let vs_rect = Rect::from_min_max(pos2(grid.right(), rect.top()), pos2(rect.right(), grid.bottom()));
    let hs_rect = Rect::from_min_max(pos2(rect.left(), grid.bottom()), pos2(grid.right(), rect.bottom()));

    let vis_rows = ((body.height() / ROW_H).floor() as usize).max(1);
    doc.grid.vis_rows = vis_rows;
    doc.grid.body_w = body.width();
    let max_top = nrows.saturating_sub(vis_rows);

    let mut prefix = Vec::with_capacity(doc.ncols + 1);
    prefix.push(0.0f32);
    for w in &doc.col_widths {
        prefix.push(prefix.last().unwrap() + w);
    }
    let total_w = *prefix.last().unwrap();
    let max_sx = (total_w - body.width() + 40.0).max(0.0);

    // ---------- Mouse wheel ----------
    if resp.hovered() {
        let d = ui.input(|i| i.smooth_scroll_delta);
        if d.y != 0.0 {
            doc.grid.scroll_acc -= d.y / ROW_H;
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
        (i >= 1 && i - 1 < doc_ncols(&prefix)).then(|| i - 1)
    };
    fn doc_ncols(prefix: &[f32]) -> usize {
        prefix.len() - 1
    }

    // ---------- Pointer ----------
    let (pointer, pressed, down, mods) = ui.input(|i| {
        (i.pointer.interact_pos(), i.pointer.primary_pressed(), i.pointer.primary_down(), i.modifiers)
    });
    let sx = doc.grid.scroll_x;

    // Hover cursor for resize handles.
    if let Some(p) = pointer.filter(|_| resp.hovered()) {
        if p.y >= grid.top() && p.y < grid.top() + HEAD_H && p.x > body.left() {
            if (0..doc.ncols).any(|c| (body.left() + prefix[c + 1] - sx - p.x).abs() < 4.0) {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
            }
        }
    }

    if pressed && resp.hovered() {
        if let Some(p) = pointer {
            if doc.grid.edit.is_some() {
                commit_edit(doc);
            }
            resp.request_focus();
            if vs_rect.contains(p) {
                doc.grid.drag = Drag::VScroll;
            } else if hs_rect.contains(p) {
                doc.grid.drag = Drag::HScroll;
            } else if p.y < grid.top() + HEAD_H {
                // Header row
                let resize = (0..doc.ncols).find(|&c| (body.left() + prefix[c + 1] - sx - p.x).abs() < 4.0);
                if let Some(c) = resize.filter(|_| p.x > body.left()) {
                    doc.grid.drag = Drag::Resize(c, p.x - doc.col_widths[c]);
                } else if p.x < body.left() {
                    // Corner: select all
                    doc.grid.anchor = (0, 0);
                    doc.grid.cur = (nrows.saturating_sub(1), doc.ncols - 1);
                } else if let Some(c) = col_at(p.x, sx) {
                    // Select whole column (cursor stays on screen).
                    doc.grid.anchor = (nrows.saturating_sub(1), c);
                    doc.grid.cur = (doc.grid.top.min(nrows.saturating_sub(1)), c);
                }
            } else if p.x < body.left() {
                let r = doc.grid.top + ((p.y - body.top()) / ROW_H) as usize;
                if r < nrows {
                    doc.grid.anchor = (r, doc.ncols - 1);
                    doc.grid.cur = (r, 0);
                }
            } else {
                let r = doc.grid.top + ((p.y - body.top()) / ROW_H) as usize;
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
                Drag::Resize(c, base) => doc.col_widths[c] = (p.x - base).max(MIN_COL_W),
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
                    // Auto-scroll when dragging past the edges.
                    if p.y > body.bottom() && doc.grid.top < max_top {
                        doc.grid.top += 1;
                    } else if p.y < body.top() && doc.grid.top > 0 {
                        doc.grid.top -= 1;
                    }
                    let r = (doc.grid.top as i64 + ((p.y - body.top()) / ROW_H).floor() as i64)
                        .clamp(0, nrows.saturating_sub(1) as i64) as usize;
                    let c = col_at(p.x.clamp(body.left(), body.right() - 1.0), doc.grid.scroll_x)
                        .unwrap_or(doc.ncols - 1);
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
            if p.y < grid.top() + HEAD_H && p.x > body.left() {
                let on_border = (0..doc.ncols).any(|c| (body.left() + prefix[c + 1] - sx - p.x).abs() < 4.0);
                if !on_border {
                    if let Some(c) = col_at(p.x, sx) {
                        event = Some(GridEvent::Sort(c));
                    }
                }
            } else if body.contains(p) {
                start_edit(doc, None);
            }
        }
    }

    // ---------- Keyboard ----------
    let focused = ui.memory(|m| m.focused());
    if focused == Some(resp.id) {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                resp.id,
                EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true },
            )
        });
    }
    let has_kb = doc.grid.edit.is_none() && (focused.is_none() || focused == Some(resp.id));
    if has_kb && nrows > 0 {
        let events = ui.input(|i| i.events.clone());
        let last_r = nrows - 1;
        let last_c = doc.ncols.saturating_sub(1);
        for ev in events {
            match ev {
                Event::Copy => {
                    if let Some(m) = copy_selection(ui, doc) {
                        event = Some(GridEvent::Message(m));
                    }
                }
                Event::Cut => {
                    if let Some(m) = copy_selection(ui, doc) {
                        event = Some(GridEvent::Message(m));
                    } else {
                        clear_selection(doc);
                    }
                }
                Event::Paste(t) => paste(doc, &t),
                Event::Text(t) if !mods.command && !mods.ctrl => {
                    start_edit(doc, Some(t));
                    break;
                }
                Event::Key { key, pressed: true, modifiers: m, .. } => {
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
                            doc.grid.cur = (r, c);
                            doc.grid.anchor = (r, c);
                            doc.ensure_visible();
                            continue;
                        }
                        Key::Enter | Key::F2 => {
                            start_edit(doc, None);
                            break;
                        }
                        Key::Delete | Key::Backspace => {
                            clear_selection(doc);
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
                        Key::Z if m.command && m.shift => {
                            let at = doc.redo();
                            jump_to_change(doc, at);
                            moved = false;
                        }
                        Key::Z if m.command => {
                            let at = doc.undo();
                            jump_to_change(doc, at);
                            moved = false;
                        }
                        Key::Y if m.command => {
                            let at = doc.redo();
                            jump_to_change(doc, at);
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
    let head_bg = vis.widgets.inactive.bg_fill;
    let stripe = vis.faint_bg_color;
    let sel_fill = vis.selection.bg_fill.gamma_multiply(0.35);
    let cur_stroke = Stroke::new(2.0, vis.selection.bg_fill);
    let find_fill = Color32::from_rgba_unmultiplied(255, 190, 0, 70);
    let edited_col = Color32::from_rgb(230, 140, 30);
    let font = FontId::proportional(13.0);
    let mono = FontId::monospace(11.5);

    painter.rect_filled(rect, 0.0, vis.extreme_bg_color);

    let first_col = prefix.partition_point(|&p| p <= sx).saturating_sub(1);
    let mut last_col = first_col;
    while last_col < doc.ncols && prefix[last_col] - sx < body.width() {
        last_col += 1;
    }
    let ((sr0, sc0), (sr1, sc1)) = sel_range(doc);
    let cur = doc.grid.cur;
    let body_painter = painter.with_clip_rect(body);
    let row_end = (top + vis_rows + 1).min(nrows);

    let mut widest = doc.ncols;
    for r in top..row_end {
        let y = body.top() + (r - top) as f32 * ROW_H;
        let rec = doc.rec_of(r);
        let fields = doc.row_fields(rec);
        widest = widest.max(fields.len());
        let row_rect = Rect::from_min_size(pos2(body.left(), y), vec2(body.width(), ROW_H));
        if r % 2 == 1 {
            body_painter.rect_filled(row_rect, 0.0, stripe);
        }
        for c in first_col..last_col {
            let x = body.left() + prefix[c] - sx;
            let cell = Rect::from_min_size(pos2(x, y), vec2(doc.col_widths[c], ROW_H));
            if r >= sr0 && r <= sr1 && c >= sc0 && c <= sc1 && (sr0, sc0) != (sr1, sc1) {
                body_painter.rect_filled(cell, 0.0, sel_fill);
            }
            let Some(v) = fields.get(c) else { continue };
            if v.is_empty() {
                continue;
            }
            if !find_lc.is_empty() && contains_ci(v, find_lc) {
                body_painter.rect_filled(cell, 0.0, find_fill);
            }
            if doc.is_edited(rec, c) {
                body_painter.rect_filled(
                    Rect::from_min_size(cell.left_top() + vec2(1.0, 1.0), vec2(3.0, ROW_H - 2.0)),
                    0.0,
                    edited_col,
                );
            }
            let clip = cell.shrink2(vec2(5.0, 0.0)).intersect(body);
            let p = painter.with_clip_rect(clip);
            if looks_numeric(v) {
                p.text(pos2(clip.right(), cell.center().y), Align2::RIGHT_CENTER, v.trim(), font.clone(), text_col);
            } else {
                p.text(pos2(clip.left(), cell.center().y), Align2::LEFT_CENTER, display_text(v), font.clone(), text_col);
            }
        }
    }
    if widest > doc.ncols {
        doc.ensure_cols(widest);
    }

    // Grid lines
    for c in first_col..=last_col.min(doc.ncols) {
        let x = (body.left() + prefix[c] - sx).round() - 0.5;
        if x >= body.left() {
            painter.vline(x, grid.top()..=grid.bottom(), Stroke::new(1.0, line_col));
        }
    }
    for i in 0..=(row_end - top) {
        let y = (body.top() + i as f32 * ROW_H).round() - 0.5;
        painter.hline(grid.left()..=grid.right(), y, Stroke::new(1.0, line_col.gamma_multiply(0.5)));
    }

    // Cursor
    if cur.0 >= top && cur.0 < row_end && cur.1 < doc.ncols {
        let x = body.left() + prefix[cur.1] - sx;
        let y = body.top() + (cur.0 - top) as f32 * ROW_H;
        let cell = Rect::from_min_size(pos2(x, y), vec2(doc.col_widths[cur.1], ROW_H));
        body_painter.rect_stroke(cell.shrink(1.0), 0.0, cur_stroke, StrokeKind::Inside);
    }

    // Header row
    let head = Rect::from_min_max(pos2(body.left(), grid.top()), pos2(grid.right(), body.top()));
    painter.rect_filled(Rect::from_min_max(grid.left_top(), head.right_bottom()), 0.0, head_bg);
    let names = doc.column_names();
    let head_painter = painter.with_clip_rect(head);
    for c in first_col..last_col {
        let x = body.left() + prefix[c] - sx;
        let cell = Rect::from_min_size(pos2(x, grid.top()), vec2(doc.col_widths[c], HEAD_H));
        if c >= sc0 && c <= sc1 {
            head_painter.rect_filled(cell, 0.0, sel_fill);
        }
        let mut label = match names.get(c) {
            Some(n) if !n.is_empty() => display_text(n),
            _ => col_letter(c),
        };
        let mut col = text_col;
        if let Some((sc, asc)) = doc.sort {
            if sc == c {
                label.push_str(if asc { "  ▲" } else { "  ▼" });
                col = vis.selection.stroke.color;
            }
        }
        if doc.applied_filter.as_ref().is_some_and(|(_, fc)| *fc == Some(c)) {
            label.push_str("  ⏷");
        }
        let clip = cell.shrink2(vec2(6.0, 0.0)).intersect(head);
        painter.with_clip_rect(clip).text(
            pos2(clip.left(), cell.center().y),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(13.0),
            col,
        );
        head_painter.vline(cell.right().round() - 0.5, cell.y_range(), Stroke::new(1.0, line_col));
    }
    painter.hline(grid.left()..=grid.right(), body.top() - 0.5, Stroke::new(1.0, line_col));

    // Row numbers
    let rn = Rect::from_min_max(pos2(grid.left(), body.top()), pos2(body.left(), grid.bottom()));
    painter.rect_filled(rn, 0.0, head_bg);
    let first = doc.first_rec();
    for r in top..row_end {
        let y = body.top() + (r - top) as f32 * ROW_H;
        let cell = Rect::from_min_size(pos2(rn.left(), y), vec2(rn_w, ROW_H));
        if r >= sr0 && r <= sr1 {
            painter.rect_filled(cell, 0.0, sel_fill);
        }
        let label = (doc.rec_of(r) - first + 1).to_string();
        painter.text(pos2(cell.right() - 6.0, cell.center().y), Align2::RIGHT_CENTER, label, mono.clone(), weak_col);
    }
    painter.vline(body.left() - 0.5, grid.y_range(), Stroke::new(1.0, line_col));

    // Scrollbars
    let track = vis.extreme_bg_color;
    let thumb = vis.widgets.inactive.fg_stroke.color.gamma_multiply(0.45);
    painter.rect_filled(vs_rect, 0.0, track);
    painter.rect_filled(hs_rect, 0.0, track);
    painter.rect_filled(Rect::from_min_max(grid.right_bottom(), rect.right_bottom()), 0.0, head_bg);
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

    if nrows == 0 && doc.index.is_done() {
        painter.text(body.center(), Align2::CENTER_CENTER, "No rows", FontId::proportional(14.0), weak_col);
    }

    // ---------- In-place editor ----------
    let mut finish: Option<bool> = None; // Some(true)=commit, Some(false)=cancel
    if let Some(edit) = &mut doc.grid.edit {
        let visible = edit.row >= top && edit.row < top + vis_rows && edit.col < doc.ncols;
        if !visible {
            finish = Some(true);
        } else {
            let x = body.left() + prefix[edit.col] - sx;
            let y = body.top() + (edit.row - top) as f32 * ROW_H;
            let cell = Rect::from_min_size(pos2(x, y), vec2(doc.col_widths[edit.col].max(160.0), ROW_H));
            let te = ui.put(
                cell,
                TextEdit::singleline(&mut edit.text).font(font.clone()).margin(vec2(4.0, 2.0)),
            );
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
            commit_edit(doc);
            if nrows > 0 {
                if enter {
                    doc.grid.cur.0 = (doc.grid.cur.0 + 1).min(nrows - 1);
                } else if tab {
                    doc.grid.cur.1 = (doc.grid.cur.1 + 1).min(doc.ncols - 1);
                }
                doc.grid.anchor = doc.grid.cur;
                doc.ensure_visible();
            }
        }
        Some(false) => doc.grid.edit = None,
        None => {}
    }

    event
}
