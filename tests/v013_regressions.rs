//! 0.1.3 regressions (task 0010 / report 0019), public API only, fixtures read-only:
//! the "Engine form" Diagnosis line (present with a context modifier, absent without one) and the
//! byte-search state being forgotten when another file is opened.
mod common;
use common::fx;
use rocksvision::app::diagnosis::{ENGINE_FORM_LABEL, Finding, finding};
use rocksvision::app::{Action, AppState, CopyKind};
use rocksvision::jobs::{FsBackend, Worker};
use rocksvision::model::{BlockKind, NodeKind, Space, Span, Status};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The Diagnosis lines (label, text) of the first Invalid row of the trailer of the data block
/// at file offset `off` (block payload `size` bytes).
fn trailer_diagnosis(path: &Path, off: u64, size: u64) -> Vec<(String, String)> {
    let mut i = rocksvision::inspect::open(path).unwrap();
    let kind = NodeKind::SstTrailer { off, size, kind: BlockKind::Data };
    let v = i.view(&kind, &|| false);
    let row = v.rows.iter().find(|r| r.field.status == Status::Invalid).unwrap_or_else(|| panic!("no Invalid trailer row in {}", path.display()));
    let Some(Finding::Diagnosis(d)) = finding(row.field.status, row.field.diag.as_deref(), &row.field.name) else { panic!("no Diagnosis for {}", path.display()) };
    d.lines.iter().map(|l| (l.label.to_string(), l.text.clone())).collect()
}

/// (off, payload size) of the first data block listed through the index.
fn first_data_block(path: &Path) -> (u64, u64) {
    let mut i = rocksvision::inspect::open(path).unwrap();
    let blocks = i.children(&NodeKind::SstDataBlocks { skip: 0 }, &|| false);
    blocks
        .iter()
        .find_map(|c| match c.kind {
            NodeKind::SstBlock { off, size, kind: BlockKind::Data } => Some((off, size)),
            _ => None,
        })
        .expect("a data block")
}

fn line<'a>(d: &'a [(String, String)], label: &str) -> Option<&'a str> {
    d.iter().find(|(l, _)| l == label).map(|(_, t)| t.as_str())
}

/// A copy of `src` with one byte at `at` XOR 0xFF, in the OS temp dir (removed by the caller).
fn flipped_copy(src: &Path, at: usize, tag: &str) -> PathBuf {
    let mut b = std::fs::read(src).unwrap();
    b[at] ^= 0xFF;
    let p = std::env::temp_dir().join(format!("rocksvision-v013-{}-{tag}.sst", std::process::id()));
    std::fs::write(&p, b).unwrap();
    p
}

#[test]
fn engine_form_shown_for_a_format_version_6_checksum_failure() {
    let p = fx("corrupt/sst-bitflip-data-block.sst");
    let (off, size) = first_data_block(&p);
    let d = trailer_diagnosis(&p, off, size);
    let order: Vec<&str> = d.iter().map(|(l, _)| l.as_str()).collect();
    let o = order.iter().position(|l| *l == "Observed").expect("Observed line");
    assert_eq!(order.get(o + 1), Some(&ENGINE_FORM_LABEL), "Engine form directly below Observed: {order:?}");
    let t = line(&d, ENGINE_FORM_LABEL).unwrap();
    assert!(t.starts_with("stored 0x") && t.contains("(context modifier 0x") && t.contains(" removed) = "), "{t}");
    // Expected/Observed keep the trailer view (modifier included): they differ from the engine's numbers.
    assert_ne!(line(&d, "Expected"), line(&d, "Observed"));
}

/// The 0017 fixture (block #100 flipped, RocksDB's own message: stored = 3862634852, computed =
/// 2307332483). Lives in /private/tmp; skipped (printed) when absent.
#[test]
fn engine_form_matches_the_rocksdb_message_on_the_0017_block_100_fixture() {
    let p = Path::new("/private/tmp/rocksvision-0017/corrupt-sst-block100-flip.sst");
    if !p.exists() {
        eprintln!("SKIPPED: {} not present", p.display());
        return;
    }
    let d = trailer_diagnosis(p, 0x6390c, 4073);
    assert_eq!(line(&d, "Bytes"), Some("file [0x6390c,0x648fa) · 4078 B"));
    assert_eq!(line(&d, ENGINE_FORM_LABEL), Some("stored 0xe63b2164 · computed 0x89871983 (context modifier 0x04fc1904 removed) = 3862634852 / 2307332483"));
}

/// format_version 5 has no context modifier: Expected/Observed already are the engine values, so no
/// Engine form line (a byte flipped in the first data block of tests/fixtures/sst-variants/format-v5.sst).
#[test]
fn engine_form_omitted_without_a_context_modifier() {
    let src = fx("sst-variants/format-v5.sst");
    let (off, size) = first_data_block(&src);
    assert_eq!(off, 0, "first data block starts the file");
    let p = flipped_copy(&src, 100, "fv5");
    let r = std::panic::catch_unwind(|| {
        let d = trailer_diagnosis(&p, off, size);
        assert!(d.iter().any(|(l, t)| l == "Check" && t.starts_with("Checksum (")), "checksum failure expected: {d:?}");
        assert_eq!(line(&d, ENGINE_FORM_LABEL), None, "no Engine form line without a modifier: {d:?}");
        assert!(line(&d, "Expected").is_some() && line(&d, "Observed").is_some());
    });
    let _ = std::fs::remove_file(&p);
    r.unwrap();
}

fn pump(app: &mut AppState, what: &str, done: impl Fn(&AppState) -> bool) {
    let t = Instant::now();
    loop {
        app.poll();
        if done(app) {
            return;
        }
        assert!(t.elapsed() < Duration::from_secs(8), "timeout waiting for: {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn opened(a: &AppState) -> bool {
    a.session().view().is_some() && a.session().pending_view().is_none()
}

/// A byte-search hit (text query, uncompressed SST) must not survive opening another file.
#[test]
fn byte_search_hit_and_status_are_forgotten_when_another_file_opens() {
    let mut app = AppState::new(Worker::spawn_with(Arc::new(|| {}), Arc::new(FsBackend)));
    app.apply(Action::OpenFile(fx("sst-none/data.sst")));
    pump(&mut app, "sst opened", opened);
    app.apply(Action::SetSearch("value-0000".into()));
    // Stepping with no row match starts the byte search of this file.
    app.apply(Action::SearchNext);
    pump(&mut app, "byte search done", |a| a.search().progress().is_none() && !a.search().status().contains("searching"));
    let found = app.search().hit();
    assert!(found.is_some() && app.search().status().starts_with("Bytes: match at file 0x"), "hit expected in the uncompressed SST: {:?} {}", found, app.search().status());
    app.apply(Action::OpenFile(fx("wal-basic/000004.log")));
    pump(&mut app, "wal opened", opened);
    assert_eq!(app.search().hit(), None, "hit of the previous file forgotten");
    assert_eq!(app.search().progress(), None);
    assert!(!app.search().status().starts_with("Bytes:"), "no stale byte status: {:?}", app.search().status());
    assert_eq!(app.search().query(), "value-0000", "the query itself is kept");
    // Banner and status segment describe the new file only (one row-count format).
    let seg = app.search().status_segment(app.row_match_position());
    let banner = app.search().banner().unwrap_or_default();
    match app.search().row_matches().len() {
        0 => {
            assert_eq!(seg.as_deref(), Some("Rows 0 / 0 (current table)"));
            assert!(banner.starts_with("No row of this table matches"), "{banner}");
        }
        n => assert!(banner.starts_with(&format!("{n} row")), "{banner} / {seg:?}"),
    }
    assert!(app.search().can_step());
}

/// tests/tools/expected_clipboard.md documents the "Copy row (TSV)" result of row 0 of the
/// decompressed Data block @0x0 of sst-zstd/data.sst (Offset cell `block+0x0`). The real copy text
/// must equal that documented line, so a prefix rename cannot leave the GUI clipboard check stale.
#[test]
fn copy_row_tsv_equals_the_documented_clipboard_example() {
    let doc = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/tools/expected_clipboard.md")).unwrap();
    let line = doc.lines().find(|l| l.starts_with("| Copy row (TSV)") && l.contains("key-000000")).expect("documented Copy row (TSV) line");
    // The cells are the back-ticked items of that line, joined by TAB.
    let cells: Vec<&str> = line.split('`').skip(1).step_by(2).collect();
    assert_eq!(cells.len(), 11, "documented example has 11 cells: {cells:?}");
    let want = cells.join("\t");

    let p = fx("sst-zstd/data.sst");
    let (off, size) = first_data_block(&p);
    let mut app = AppState::new(Worker::spawn_with(Arc::new(|| {}), Arc::new(FsBackend)));
    app.apply(Action::OpenFile(p));
    pump(&mut app, "sst opened", opened);
    let target = NodeKind::SstBlock { off, size, kind: BlockKind::Data };
    app.apply(Action::Navigate { target: target.clone(), span: None });
    pump(&mut app, "block view", |a| a.session().pending_view().is_none() && a.session().view_kind() == Some(&target));
    app.apply(Action::SelectRow(0));
    app.do_copy(CopyKind::RowTsv);
    let t = Instant::now();
    let mut clip = Vec::new();
    while clip.is_empty() {
        clip.extend(app.poll());
        assert!(t.elapsed() < Duration::from_secs(8), "copy never answered");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(clip, vec![want]);
}

fn root(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn open_app(path: PathBuf) -> AppState {
    let mut app = AppState::new(Worker::spawn_with(Arc::new(|| {}), Arc::new(FsBackend)));
    app.apply(Action::OpenFile(path));
    pump(&mut app, "file opened", opened);
    app
}

fn show(app: &mut AppState, target: &NodeKind) {
    app.apply(Action::Navigate { target: target.clone(), span: None });
    pump(app, "view shown", |a| a.session().pending_view().is_none() && a.session().view_kind() == Some(target));
}

/// The clipboard text the real Copy Offset path produces for the current selection.
fn copy_offset(app: &mut AppState) -> String {
    app.do_copy(CopyKind::Offset);
    let t = Instant::now();
    let mut clip = Vec::new();
    while clip.is_empty() {
        clip.extend(app.poll());
        assert!(t.elapsed() < Duration::from_secs(8), "copy never answered");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(clip.len(), 1, "{clip:?}");
    clip.remove(0)
}

/// The single back-ticked string of the documented "Copy offset - <tag>" line of
/// tests/tools/expected_clipboard.md.
fn documented_copy_offset(doc: &str, tag: &str) -> String {
    let lines: Vec<&str> = doc.lines().filter(|l| l.starts_with("| Copy offset - ") && l.contains(tag)).collect();
    assert_eq!(lines.len(), 1, "exactly one documented `Copy offset - ` line tagged {tag:?}: {lines:?}");
    let ticks: Vec<&str> = lines[0].split('`').skip(1).step_by(2).collect();
    assert_eq!(ticks.len(), 1, "one back-ticked clipboard string expected in {:?}", lines[0]);
    ticks[0].to_string()
}

/// Every documented Copy Offset example of tests/tools/expected_clipboard.md (file space,
/// decompressed block, reassembled record with one file range and across a fragment boundary)
/// equals the text the real `do_copy(CopyKind::Offset)` produces, so the GUI clipboard check
/// cannot go stale when the notation changes.
#[test]
fn copy_offset_equals_the_documented_clipboard_examples() {
    let doc = std::fs::read_to_string(root("tests/tools/expected_clipboard.md")).unwrap();
    let mut got: Vec<(&str, String)> = Vec::new();

    // File space: raw selection 0x10+4 of the compressed SST.
    let mut app = open_app(root("examples/files/sst-zstd/sst-zstd.sst"));
    app.apply(Action::SelectSpan(Span::file(0x10, 4)));
    got.push(("[file]", copy_offset(&mut app)));

    // Compressed block 0, first field row of the decompressed view.
    let (off, size) = first_data_block(&root("examples/files/sst-zstd/sst-zstd.sst"));
    let blk = NodeKind::SstBlock { off, size, kind: BlockKind::Data };
    show(&mut app, &blk);
    let row = app.session().view().unwrap().rows.iter().position(|r| r.field.span.is_some_and(|s| s.space == Space::Block(off) && s.len > 0)).expect("a decompressed field row");
    assert_eq!(row, 0, "entry 0 is the first row");
    app.apply(Action::SelectRow(row));
    got.push(("[block]", copy_offset(&mut app)));

    // Reassembled record with one file range: count field of record 0 of wal-plain.
    let mut app = open_app(root("examples/files/wal-plain/000004.log"));
    show(&mut app, &NodeKind::LogRecord(0));
    let count = app.session().view().unwrap().rows.iter().position(|r| r.field.span == Some(Span::new(Space::Record(0), 8, 4))).expect("count field at record+0x8");
    app.apply(Action::SelectRow(count));
    got.push(("[record-one-range]", copy_offset(&mut app)));

    // Reassembled record across a fragment boundary: first field row of record@0x5eec that
    // needs more than one file range.
    let mut app = open_app(fx("wal-fragmented/000004.log"));
    let rec = NodeKind::LogRecord(0x5eec);
    show(&mut app, &rec);
    let v = app.session().view().unwrap();
    let i = v
        .rows
        .iter()
        .position(|r| r.field.span.is_some_and(|s| s.space == Space::Record(0x5eec) && rocksvision::app::provenance::exact_file_ranges(Some(v), &s).len() > 1))
        .expect("a field crossing a fragment boundary in record@0x5eec");
    app.apply(Action::SelectRow(i));
    got.push(("[record-across-fragments]", copy_offset(&mut app)));

    for (tag, real) in &got {
        assert_eq!(&documented_copy_offset(&doc, tag), real, "documented Copy offset example {tag}");
    }
    assert!(!got[3].1.contains("= file"), "no contiguous file range across a fragment boundary: {}", got[3].1);
    assert!(got[2].1.contains(" = file ["), "one file range names it: {}", got[2].1);
}

/// 0020 UI item 5 / phase 3: the Explanation "Where" and the tooltip of the three table view
/// nodes (file-order layout, data blocks via index, WAL logical records) are exact text. They had
/// no test; the view nodes have no range of their own, so no range is claimed for the two
/// scattered ones and the layout one names the whole file.
#[test]
fn table_view_nodes_have_exact_where_and_tooltip_text() {
    let sst = fx("sst-zstd/data.sst");
    let len = std::fs::metadata(&sst).unwrap().len();
    let mut app = open_app(sst);
    let cases: [(NodeKind, &str, String); 2] = [
        (NodeKind::SstLayout, "Every structure in file order, gaps included. Select a row to see its bytes.", format!("Whole file: file [0x0,0x{len:x}) · {len} B. Select a row for its byte range.")),
        (NodeKind::SstDataBlocks { skip: 0 }, "Data blocks in index (key) order, as the reader finds them. Select a row to see its bytes.", String::new()),
    ];
    for (kind, tip, wh) in cases {
        show(&mut app, &kind);
        assert_eq!(rocksvision::app::provenance::view_tooltip(&kind), Some(tip), "tooltip of {kind:?}");
        let w = rocksvision::app::provenance::view_where(app.session().view_kind(), app.session().view().unwrap(), app.session().file_len()).expect("a Where text");
        if wh.is_empty() {
            assert!(w.ends_with("data blocks spread across the file (not one range). Select a row for its byte range.") || w.starts_with("Data blocks spread across the file (not one range)."), "{w}");
            assert!(!w.contains("file ["), "no range claimed for a scattered view: {w}");
        } else {
            assert_eq!(w, wh, "Where of {kind:?}");
        }
    }

    let mut app = open_app(fx("wal-fragmented/000004.log"));
    let kind = NodeKind::LogRecords { start: 0, page: 0 };
    show(&mut app, &kind);
    assert_eq!(rocksvision::app::provenance::view_tooltip(&kind), Some("Logical records in file order, reassembled from their fragments. Select a row to see its bytes."));
    let w = rocksvision::app::provenance::view_where(app.session().view_kind(), app.session().view().unwrap(), app.session().file_len());
    assert_eq!(w.as_deref(), Some("Records reassembled from fragments across the file (not one range). Select a row for its byte range."));
    // A single record is not a table view node: no tooltip text of this kind.
    assert_eq!(rocksvision::app::provenance::view_tooltip(&NodeKind::LogRecord(0)), None);
}
