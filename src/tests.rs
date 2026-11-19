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
    d.measure_if_needed(7.0, 300.0);
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
