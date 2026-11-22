//! End-to-end tests of the core table engine (no UI).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use crate::doc::{Document, LoadOpts, Quoting, Snap};
use crate::jobs::{self, SavePlan};
use crate::query::Query;
use crate::table::TableRead;

fn tmp(name: &str, content: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ficsv-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, content).unwrap();
    p
}

fn open(p: &PathBuf) -> Document {
    let mut d = Document::open(p, LoadOpts::default(), &egui::Context::default()).unwrap();
    let t = Instant::now();
    while !d.ready() {
        assert!(t.elapsed() < Duration::from_secs(60), "indexing timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
    d.measure_if_needed(&|t: &str| t.chars().count() as f32 * 7.0, 300.0);
    d
}

fn save_to_string(d: &Document, target: &PathBuf) -> String {
    let plan = SavePlan {
        opts: d.save_opts.clone(),
        raw_ok: d.cols_identity() && !d.src.transcoded && d.save_opts.delimiter == d.src.delimiter && d.save_opts.encoding == d.src.encoding,
        cols_identity: d.cols_identity(),
        subset: None,
    };
    let tmp = jobs::save(&d.snap(), &plan, target, &AtomicBool::new(false)).unwrap();
    let bytes = std::fs::read(&tmp).unwrap();
    std::fs::remove_file(tmp).unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn rows(s: &Snap) -> Vec<Vec<String>> {
    (0..s.nrows()).map(|r| (0..s.ncols()).map(|c| s.cell(r, c)).collect()).collect()
}

const CSV: &str = "name,age,note\r\nalice,30,\"likes \"\"tea\"\"\"\r\nbob,25,\"multi\r\nline\"\r\ncarol,41,plain\r\n";

#[test]
fn parse_quotes_newlines_crlf() {
    let p = tmp("a.csv", CSV.as_bytes());
    let d = open(&p);
    assert_eq!(d.nrows(), 3);
    assert_eq!(d.ncols(), 3);
    assert_eq!(d.cell(0, 2), "likes \"tea\"");
    assert_eq!(d.cell(1, 2), "multi\r\nline");
    assert_eq!(d.snap().column_name(1), "age");
    assert_eq!(d.src.line_ending.label(), "CRLF");
}

#[test]
fn unedited_save_is_byte_identical() {
    let p = tmp("b.csv", CSV.as_bytes());
    let d = open(&p);
    assert_eq!(save_to_string(&d, &p), CSV);
}

#[test]
fn edit_insert_remove_undo_and_save() {
    let p = tmp("c.csv", CSV.as_bytes());
    let mut d = open(&p);
    d.set_cells(vec![(0, 1, "31".into())]);
    d.insert_rows(1, 1);
    d.set_cells(vec![(1, 0, "new, row".into())]);
    d.insert_cols(1, 1);
    d.set_cells_t(vec![(0, 1, "extra".into())]);
    let r = rows(&d.snap());
    assert_eq!(r[0], vec!["alice", "", "31", "likes \"tea\""]);
    assert_eq!(r[1], vec!["new, row", "", "", ""]);
    let out = save_to_string(&d, &p);
    assert_eq!(
        out,
        "name,extra,age,note\r\nalice,,31,\"likes \"\"tea\"\"\"\r\n\"new, row\",,,\r\nbob,,25,\"multi\r\nline\"\r\ncarol,,41,plain\r\n"
    );
    // Undo everything
    while d.undo() {}
    assert_eq!(rows(&d.snap())[0], vec!["alice", "30", "likes \"tea\""]);
    assert_eq!(d.nrows(), 3);
    assert!(d.redo());
    assert_eq!(d.cell(0, 1), "31");
}

#[test]
fn remove_move_duplicate() {
    let p = tmp("d.csv", b"h\n1\n2\n3\n4\n");
    let mut d = open(&p);
    d.remove_rows(&HashSet::from([1]));
    assert_eq!(rows(&d.snap()).concat(), vec!["1", "3", "4"]);
    d.move_rows(2..3, 0);
    assert_eq!(rows(&d.snap()).concat(), vec!["4", "1", "3"]);
    d.duplicate_rows(0..1, 2);
    assert_eq!(rows(&d.snap()).concat(), vec!["4", "4", "4", "1", "3"]);
    d.set_cells(vec![(1, 0, "x".into())]);
    assert_eq!(d.cell(0, 0), "4", "copies must be independent");
}

#[test]
fn sort_filter_dedupe() {
    let p = tmp("e.csv", b"k,v\nb,10\na,9\nc,100\na,9\n,5\n");
    let d = open(&p);
    let s = d.snap();
    let c = AtomicBool::new(false);
    let order = jobs::sort_rows(&s, 1, true, &c);
    let vals: Vec<String> = order[1..].iter().map(|&id| s.value(id, 1)).collect();
    assert_eq!(vals, vec!["5", "9", "9", "10", "100"], "numeric sort");
    let order = jobs::sort_rows(&s, 0, true, &c);
    let keys: Vec<String> = order[1..].iter().map(|&id| s.value(id, 0)).collect();
    assert_eq!(keys, vec!["a", "a", "b", "c", ""], "text sort, empties last");
    let q = Query::parse(">=10").unwrap();
    let vis = jobs::compute_visible(&s, &[(q, Some(1))], &Default::default(), &c).unwrap();
    assert_eq!(vis, vec![0, 2]);
    assert_eq!(jobs::duplicate_rows(&s, &[], &c), vec![3]);
    assert_eq!(jobs::duplicate_rows(&s, &[0], &c), vec![3]);
}

#[test]
fn encodings_and_delimiters() {
    let p = tmp("f.tsv", b"a\tb\n1\t2\n");
    assert_eq!(open(&p).src.delimiter, b'\t');
    let p = tmp("g.csv", b"a;b;c\n1;2;3\n4;5;6\n");
    assert_eq!(open(&p).src.delimiter, b';');
    // Windows-1252 é
    let p = tmp("h.csv", b"name\ncaf\xe9\n");
    let d = open(&p);
    assert_eq!(d.cell(0, 0), "café");
    let saved = std::fs::read(&p).unwrap();
    assert_eq!(saved, b"name\ncaf\xe9\n");
    let _ = save_to_string(&d, &p);
}

#[test]
fn utf16_roundtrip() {
    let text = "a,b\nü,ß\n";
    let mut bytes = vec![0xFF, 0xFE];
    for u in text.encode_utf16() {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    let p = tmp("u16.csv", &bytes);
    let mut d = open(&p);
    assert_eq!(d.cell(0, 1), "ß");
    d.set_cells(vec![(0, 0, "Ü".into())]);
    let plan = SavePlan { opts: d.save_opts.clone(), raw_ok: false, cols_identity: true, subset: None };
    assert_eq!(d.save_opts.quoting, Quoting::AsNeeded);
    let tmpf = jobs::save(&d.snap(), &plan, &p, &AtomicBool::new(false)).unwrap();
    let out = std::fs::read(&tmpf).unwrap();
    std::fs::remove_file(tmpf).unwrap();
    assert_eq!(&out[..2], &[0xFF, 0xFE]);
    let decoded: Vec<u16> = out[2..].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    assert_eq!(String::from_utf16(&decoded).unwrap(), "a,b\nÜ,ß\n");
}

#[test]
#[ignore]
fn perf_big_file() {
    // cargo test --release perf_big_file -- --ignored --nocapture
    let p = std::env::temp_dir().join("ficsv-perf.csv");
    if !p.exists() {
        use std::io::Write;
        let mut w = std::io::BufWriter::new(std::fs::File::create(&p).unwrap());
        writeln!(w, "id,name,city,amount,note").unwrap();
        for i in 0..5_000_000u64 {
            writeln!(w, "{i},user{},city{},{}.{:02},\"note, with comma {}\"", i % 9973, i % 101, i % 100000, i % 100, i % 7).unwrap();
        }
    }
    let size = std::fs::metadata(&p).unwrap().len();
    let t = Instant::now();
    let d = open(&p);
    println!("indexed {} rows ({:.0} MB) in {:?}", d.nrows(), size as f64 / 1e6, t.elapsed());
    let s = d.snap();
    let c = AtomicBool::new(false);
    let t = Instant::now();
    let q = Query::parse("city42").unwrap();
    let v = jobs::compute_visible(&s, &[(q, Some(2))], &Default::default(), &c).unwrap();
    println!("filter -> {} rows in {:?}", v.len(), t.elapsed());
    let t = Instant::now();
    let o = jobs::sort_rows(&s, 3, false, &c);
    println!("numeric sort of {} rows in {:?}", o.len(), t.elapsed());
    let t = Instant::now();
    let _ = save_to_string(&d, &std::env::temp_dir().join("ficsv-perf-out.csv"));
    println!("save (raw copy) in {:?}", t.elapsed());
}

#[test]
fn autofit_fits_rtl_text() {
    // Real fonts (Faruma for Thaana, Arial for Arabic/Hebrew) and real text measurement.
    let ctx = egui::Context::default();
    crate::rtl::install_fonts(&ctx, "");
    ctx.begin_pass(egui::RawInput::default());
    let font = egui::FontId::proportional(13.0);
    let measure = |t: &str| -> f32 {
        let shown = crate::rtl::visual(t).into_owned();
        ctx.fonts_mut(|f| f.layout_no_wrap(shown, font.clone(), egui::Color32::WHITE).size().x)
    };
    let long_dv = "ދިވެހިރާއްޖޭގެ ޖުމްހޫރިއްޔާ - ރާއްޖޭގެ ވެރިރަށް މާލެ";
    let long_ar = "جمهورية مصر العربية - القاهرة عاصمة البلاد";
    let long_he = "מדינת ישראל - ירושלים היא הבירה";
    let csv = format!("dv,ar,he\nx,y,z\n{long_dv},{long_ar},{long_he}\n");
    let p = tmp("rtl-fit.csv", csv.as_bytes());
    let mut d = open(&p);
    for (c, text) in [(0, long_dv), (1, long_ar), (2, long_he)] {
        let need = measure(text);
        assert!(need > 150.0, "{text}: measured {need}px — glyphs missing?");
        d.autofit_col(c, &measure, 3000.0);
        let w = d.width_of(c);
        assert!(w >= need + 20.0, "column {c} is {w}px but text needs {need}px");
    }
    let _ = ctx.end_pass();
}

/// Drive the real grid widget headlessly with synthetic pointer events.
fn grid_frame(ctx: &egui::Context, d: &mut Document, s: &crate::settings::Settings, t: f64, events: Vec<egui::Event>) {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 700.0))),
        time: Some(t),
        events,
        ..Default::default()
    };
    let _ = ctx.run_ui(input, |ui| {
        egui::CentralPanel::no_frame().show(ui, |ui| {
            crate::grid::show(ui, d, s, None);
        });
    });
}

fn press(pos: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }
}

#[test]
fn double_click_border_autofits_rtl_column() {
    let long = "ދިވެހިރާއްޖޭގެ ޖުމްހޫރިއްޔާ ".repeat(8);
    let p = tmp("dbl.csv", format!("name,b\n{long},x\n").as_bytes());
    let mut d = open(&p);
    d.widths_measured = false; // let the grid size columns with real fonts
    let s = crate::settings::Settings::default();
    let ctx = egui::Context::default();
    crate::rtl::install_fonts(&ctx, "");
    let mut t = 0.0;
    let mut frame = |d: &mut Document, ev: Vec<egui::Event>| {
        t += 0.05;
        grid_frame(&ctx, d, &s, t, ev);
    };
    frame(&mut d, vec![]);
    frame(&mut d, vec![]);
    let before = d.width_of(0);
    // Border of column A: body starts after the row-label gutter.
    let rn_w = 16.0 + 1.0 * (s.font_size * 0.6);
    let border = egui::pos2(rn_w + before, 10.0);
    frame(&mut d, vec![egui::Event::PointerMoved(border)]);
    frame(&mut d, vec![press(border, true)]);
    frame(&mut d, vec![press(border, false)]);
    frame(&mut d, vec![press(border, true)]);
    frame(&mut d, vec![press(border, false)]);
    frame(&mut d, vec![]);
    let after = d.width_of(0);
    assert!(after > before + 50.0, "double-click should widen the column: before {before}, after {after}");

    // Real double-click: the button stays held for a few frames after the second press.
    let id = d.cols[0];
    d.widths.insert(id, 120.0);
    frame(&mut d, vec![]);
    let border = egui::pos2(rn_w + 120.0, 10.0);
    frame(&mut d, vec![egui::Event::PointerMoved(border)]);
    frame(&mut d, vec![press(border, true)]);
    frame(&mut d, vec![press(border, false)]);
    frame(&mut d, vec![press(border, true)]);
    for _ in 0..5 {
        frame(&mut d, vec![]); // still holding the button
    }
    frame(&mut d, vec![press(border, false)]);
    frame(&mut d, vec![]);
    assert!(d.width_of(0) > 400.0, "held double-click should auto-fit, got {}", d.width_of(0));

    // Trackpad tap-to-click: both taps can arrive in the same frame.
    let id = d.cols[0];
    d.widths.insert(id, 120.0);
    frame(&mut d, vec![]);
    let border = egui::pos2(rn_w + 120.0, 10.0);
    frame(&mut d, vec![egui::Event::PointerMoved(border)]);
    frame(&mut d, vec![press(border, true), press(border, false), press(border, true), press(border, false)]);
    frame(&mut d, vec![]);
    assert!(d.width_of(0) > 400.0, "double-tap in one frame should auto-fit, got {}", d.width_of(0));
}

#[test]
fn horizontal_swipe_does_not_move_rows() {
    let body: String = (0..500).map(|i| format!("{i},{}\n", "x".repeat(40))).collect();
    let p = tmp("swipe.csv", format!("a,b\n{body}").as_bytes());
    let mut d = open(&p);
    for c in 0..d.ncols() {
        let id = d.cols[c];
        d.widths.insert(id, 900.0);
    }
    let s = crate::settings::Settings::default();
    let ctx = egui::Context::default();
    let mid = egui::pos2(600.0, 300.0);
    grid_frame(&ctx, &mut d, &s, 0.0, vec![egui::Event::PointerMoved(mid)]);
    d.grid.top = 100;
    d.grid.scroll_x = 400.0;
    for k in 0..10 {
        // Mostly-horizontal trackpad swipe to the left with a little vertical jitter.
        let ev = egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(30.0, 4.0), modifiers: Default::default(), phase: egui::TouchPhase::Move };
        grid_frame(&ctx, &mut d, &s, 0.1 + k as f64 * 0.016, vec![ev]);
    }
    assert_eq!(d.grid.top, 100, "rows must not drift during a horizontal swipe");
    assert!(d.grid.scroll_x < 400.0, "should have scrolled left");
}

#[test]
fn menu_bar_switches_on_hover_when_open() {
    let ctx = egui::Context::default();
    let mut rects: Vec<egui::Rect> = Vec::new();
    let mut ids: Vec<egui::Id> = Vec::new();
    let frame = |t: f64, events: Vec<egui::Event>, rects: &mut Vec<egui::Rect>, ids: &mut Vec<egui::Id>| {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
            time: Some(t),
            events,
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |ui| {
            let mut tops = Vec::new();
            egui::MenuBar::new().ui(ui, |ui| {
                for name in ["File", "Edit", "View"] {
                    tops.push(ui.menu_button(name, |ui| { let _ = ui.button(format!("{name} item")); }).response);
                }
            });
            crate::app::menu_hover_switch(ui.ctx(), &tops);
            *rects = tops.iter().map(|r| r.rect).collect();
            *ids = tops.iter().map(egui::Popup::default_response_id).collect();
        });
    };
    frame(0.0, vec![], &mut rects, &mut ids);
    let file = rects[0].center();
    let edit = rects[1].center();
    let view = rects[2].center();
    frame(0.1, vec![egui::Event::PointerMoved(file), press(file, true)], &mut rects, &mut ids);
    frame(0.2, vec![press(file, false)], &mut rects, &mut ids);
    frame(0.3, vec![], &mut rects, &mut ids);
    assert!(egui::Popup::is_id_open(&ctx, ids[0]), "File should open on click");
    frame(0.4, vec![egui::Event::PointerMoved(edit)], &mut rects, &mut ids);
    frame(0.5, vec![], &mut rects, &mut ids);
    assert!(egui::Popup::is_id_open(&ctx, ids[1]), "hovering Edit should switch to Edit");
    assert!(!egui::Popup::is_id_open(&ctx, ids[0]));
    frame(0.6, vec![egui::Event::PointerMoved(view)], &mut rects, &mut ids);
    frame(0.7, vec![], &mut rects, &mut ids);
    assert!(egui::Popup::is_id_open(&ctx, ids[2]), "hovering View should switch to View");
}

/// Real-file check: FICSV_SAMPLE=/path/file.csv cargo test sample_file -- --ignored --nocapture
#[test]
#[ignore]
fn sample_file() {
    let Ok(path) = std::env::var("FICSV_SAMPLE") else { return };
    let mut d = open(&PathBuf::from(path));
    println!("rows {} cols {} delimiter {:?} encoding {}", d.nrows(), d.ncols(), d.src.delimiter as char, d.src.encoding.name());
    let s = crate::settings::Settings::default();
    let ctx = egui::Context::default();
    println!("font warning: {:?}", crate::rtl::install_fonts(&ctx, ""));
    // Characters no installed font can draw (would show as boxes).
    ctx.begin_pass(egui::RawInput::default());
    let snap = d.snap();
    let mut missing = std::collections::BTreeMap::new();
    for r in 0..d.nrows().min(2000) {
        for v in snap.row_values(snap.row_id(r + 1)) {
            for ch in v.chars() {
                if !ch.is_control() && !ctx.fonts_mut(|f| f.has_glyph(&egui::FontId::proportional(13.0), ch)) {
                    *missing.entry(ch).or_insert(0) += 1;
                }
            }
        }
    }
    let _ = ctx.end_pass();
    println!("chars without a glyph: {:?}", missing.iter().map(|(c, n)| format!("U+{:04X} {c:?} x{n}", *c as u32)).collect::<Vec<_>>());
    d.widths_measured = false;
    let mut t = 0.0;
    let mut frame = |d: &mut Document, ev: Vec<egui::Event>| {
        t += 0.05;
        grid_frame(&ctx, d, &s, t, ev);
    };
    frame(&mut d, vec![]);
    frame(&mut d, vec![]);
    let digits = ((d.nrows().max(1) as f64 + 1.0).log10().floor() as usize) + 1;
    let rn_w = 16.0 + digits as f32 * (s.font_size * 0.6);
    for c in 0..d.ncols() {
        let id = d.cols[c];
        d.widths.insert(id, 60.0);
    }
    for (c, two_in_one_frame) in [(1usize, false), (2, true), (3, false)] {
        frame(&mut d, vec![]);
        let left: f32 = (0..c).map(|k| d.width_of(k)).sum();
        let border = egui::pos2(rn_w + left + d.width_of(c), 10.0);
        let before = d.width_of(c);
        frame(&mut d, vec![egui::Event::PointerMoved(border)]);
        if two_in_one_frame {
            frame(&mut d, vec![press(border, true), press(border, false), press(border, true), press(border, false)]);
        } else {
            frame(&mut d, vec![press(border, true)]);
            frame(&mut d, vec![press(border, false)]);
            frame(&mut d, vec![press(border, true)]);
            frame(&mut d, vec![press(border, false)]);
        }
        frame(&mut d, vec![]);
        println!("double-click col {c} ({}): {before} -> {}", snap.column_name(c), d.width_of(c));
        assert!(d.width_of(c) > before, "column {c} did not auto-fit");
    }
}

#[test]
fn double_click_left_edge_of_rtl_column_fits_it() {
    // An RTL column's text is right-aligned, so its overflow is on the left: double-clicking its
    // left edge (the previous column's right border) must fit the RTL column too.
    let long = "ދިވެހިރާއްޖޭގެ ޖުމްހޫރިއްޔާ ".repeat(6);
    let p = tmp("rtl-left.csv", format!("id,title\n1,{long}\n2,{long}\n").as_bytes());
    let mut d = open(&p);
    let s = crate::settings::Settings::default();
    let ctx = egui::Context::default();
    crate::rtl::install_fonts(&ctx, "");
    for c in 0..d.ncols() {
        let id = d.cols[c];
        d.widths.insert(id, 100.0);
    }
    let mut t = 0.0;
    let mut frame = |d: &mut Document, ev: Vec<egui::Event>| {
        t += 0.05;
        grid_frame(&ctx, d, &s, t, ev);
    };
    frame(&mut d, vec![]);
    let rn_w = 16.0 + 1.0 * (s.font_size * 0.6);
    let border = egui::pos2(rn_w + 100.0, 10.0); // between "id" and the Dhivehi "title"
    frame(&mut d, vec![egui::Event::PointerMoved(border)]);
    frame(&mut d, vec![press(border, true)]);
    frame(&mut d, vec![press(border, false)]);
    frame(&mut d, vec![press(border, true)]);
    frame(&mut d, vec![]);
    frame(&mut d, vec![press(border, false)]);
    frame(&mut d, vec![]);
    assert!(d.width_of(1) > 400.0, "RTL column should auto-fit from its left edge, got {}", d.width_of(1));
    assert!(d.width_of(0) < 100.0, "the LTR id column fits its short content, got {}", d.width_of(0));
}

/// Runs `f` once per simulated 60 fps frame.
#[cfg(test)]
fn motion_frames(ctx: &egui::Context, t0: f64, n: usize, mut f: impl FnMut(&egui::Context)) -> f64 {
    let mut time = t0;
    for _ in 0..n {
        time += 1.0 / 60.0;
        let input = egui::RawInput { time: Some(time), predicted_dt: 1.0 / 60.0, ..Default::default() };
        let _ = ctx.run_ui(input, |ui| f(ui.ctx()));
    }
    time
}

#[test]
fn motion_glide_follows_then_settles_and_snaps_far_jumps() {
    use crate::ui_kit::motion;
    let ctx = egui::Context::default();
    let id = egui::Id::new("g");
    let mut v = 0.0;
    let t = motion_frames(&ctx, 0.0, 3, |c| v = motion::glide(c, id, 0.0, 500.0));
    assert_eq!(v, 0.0);
    let t = motion_frames(&ctx, t, 1, |c| v = motion::glide(c, id, 100.0, 500.0));
    assert!(v > 5.0 && v < 60.0, "first frame should move part of the way, got {v}");
    let t = motion_frames(&ctx, t, 30, |c| v = motion::glide(c, id, 100.0, 500.0));
    assert_eq!(v, 100.0, "settles exactly on the target within half a second");
    motion_frames(&ctx, t, 1, |c| v = motion::glide(c, id, 5000.0, 500.0));
    assert_eq!(v, 5000.0, "jumps beyond the snap distance are not animated");
}

#[test]
fn motion_appear_restarts_after_being_hidden_and_respects_animations_off() {
    use crate::ui_kit::motion;
    let ctx = egui::Context::default();
    let id = egui::Id::new("a");
    let mut v = 0.0;
    let t = motion_frames(&ctx, 0.0, 1, |c| v = motion::appear(c, id, 0.25));
    assert_eq!(v, 0.0);
    let t = motion_frames(&ctx, t, 30, |c| v = motion::appear(c, id, 0.25));
    assert_eq!(v, 1.0);
    let t = motion_frames(&ctx, t, 3, |_| {}); // hidden for a few frames
    let t = motion_frames(&ctx, t, 1, |c| v = motion::appear(c, id, 0.25));
    assert_eq!(v, 0.0, "re-showing starts the entrance again");
    ctx.all_styles_mut(|s| s.animation_time = 0.0);
    motion_frames(&ctx, t, 1, |c| v = motion::appear(c, id, 0.25));
    assert_eq!(v, 1.0);
    let mut g = 0.0;
    motion_frames(&ctx, t, 1, |c| g = motion::glide(c, egui::Id::new("off"), 1.0, 500.0));
    motion_frames(&ctx, t, 1, |c| g = motion::glide(c, egui::Id::new("off"), 90.0, 500.0));
    assert_eq!(g, 90.0);
}
