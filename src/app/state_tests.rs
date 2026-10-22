//! UI-state regressions with a controllable delayed I/O backend: requests return immediately,
//! stale copies never reach the clipboard, stale hex pages are discarded after a file switch,
//! a delayed folder scan can be cancelled, and the hex page state never spins without a request.

use super::AppState as App;
use super::*;
use crate::app::hex::{HexState, PageState};
use crate::app::job_state::CopyReply;
use crate::app::prefs as theme;
use crate::jobs::{FsBackend, IoBackend, Outcome, Worker};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Latch: I/O on paths containing "slow" blocks until released (10 s safety timeout).
struct Gate {
    open: Mutex<bool>,
    cv: Condvar,
}

impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Gate { open: Mutex::new(false), cv: Condvar::new() })
    }
    fn wait(&self) {
        let g = self.open.lock().unwrap();
        let _ = self.cv.wait_timeout_while(g, Duration::from_secs(10), |open| !*open).unwrap();
    }
    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.cv.notify_all();
    }
}

struct SlowBackend(Arc<Gate>);

impl SlowBackend {
    fn maybe_wait(&self, p: &Path) {
        if p.to_string_lossy().contains("slow") {
            self.0.wait();
        }
    }
}

impl IoBackend for SlowBackend {
    fn is_dir(&self, p: &Path) -> std::io::Result<bool> {
        FsBackend.is_dir(p) // stat is never delayed; scans and reads are
    }
    fn scan(&self, dir: &Path) -> std::io::Result<(Vec<crate::discover::DirEntry>, bool)> {
        self.maybe_wait(dir);
        FsBackend.scan(dir)
    }
    fn read(&self, p: &Path, off: u64, len: u64) -> std::io::Result<(Vec<u8>, u64)> {
        self.maybe_wait(p);
        FsBackend.read(p, off, len)
    }
    fn stat(&self, p: &Path) -> std::io::Result<(u64, Option<std::time::SystemTime>)> {
        FsBackend.stat(p)
    }
}

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rocksvision-ui-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write(p: &Path, bytes: &[u8]) -> PathBuf {
    std::fs::write(p, bytes).unwrap();
    p.to_path_buf()
}

fn app_with(gate: &Arc<Gate>) -> App {
    App::new(Worker::spawn_with(Arc::new(|| {}), Arc::new(SlowBackend(gate.clone()))))
}

/// Polls until `done` holds; returns everything the app put on the clipboard meanwhile.
fn pump(app: &mut App, what: &str, done: impl Fn(&App) -> bool) -> Vec<String> {
    let t = Instant::now();
    let mut clip = Vec::new();
    loop {
        clip.extend(app.poll());
        if done(app) {
            return clip;
        }
        assert!(t.elapsed() < Duration::from_secs(8), "timeout waiting for: {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Waits until every I/O job has finished and its response was applied.
fn settle(app: &mut App) -> Vec<String> {
    let mut clip = pump(app, "io idle", |a| !a.jobs().io_busy());
    clip.extend(app.poll());
    clip
}

fn quick(what: &str, f: impl FnOnce()) {
    let t = Instant::now();
    f();
    assert!(t.elapsed() < Duration::from_millis(100), "{what} blocked the caller for {:?}", t.elapsed());
}

#[test]
fn request_paths_return_immediately_while_io_is_blocked() {
    let gate = Gate::new();
    let d = tmpdir("slow-immediate");
    let f = write(&d.join("slow-a.log"), &[7u8; 300]);
    let mut app = app_with(&gate);
    quick("open_path(folder)", || app.open_path(d.clone()));
    pump(&mut app, "folder resolved", |a| a.jobs().scanning());
    quick("open_file", || app.open_file(f.clone()));
    assert!(app.session().hex().is_loading());
    quick("load_hex_page", || app.load_hex_page(Space::File, 0));
    app.select_raw(Span::file(0, 4), "t");
    quick("copy hex", || app.do_copy(CopyKind::Hex));
    assert!(app.jobs().pending_copy().is_some());
    quick("cancel", || app.cancel_jobs());
    assert!(!app.session().hex().is_loading() && app.jobs().pending_copy().is_none() && !app.jobs().scanning());
    gate.release();
    settle(&mut app);
}

#[test]
fn stale_copy_result_is_discarded() {
    let gate = Gate::new();
    let d = tmpdir("copy");
    let bytes: Vec<u8> = (0u8..64).collect();
    let f = write(&d.join("slow-copy.log"), &bytes);
    let mut app = app_with(&gate);
    app.open_file(f);
    // The root view replaces the selection when it arrives; wait for it first.
    pump(&mut app, "root view", |a| a.session().pending_view().is_none());
    app.select_raw(Span::file(0, 4), "first");
    app.do_copy(CopyKind::Hex);
    // Selection changes while the delayed read is in flight.
    app.select_raw(Span::file(8, 4), "second");
    gate.release();
    let clip = pump(&mut app, "copy answered", |a| a.jobs().pending_copy().is_none());
    assert!(clip.is_empty(), "stale copy reached the clipboard: {clip:?}");
    assert!(app.jobs().copy_note().contains("stale"), "note: {}", app.jobs().copy_note());
    // Control: an unchanged selection is copied.
    app.do_copy(CopyKind::Hex);
    let clip = pump(&mut app, "copy answered", |a| a.jobs().pending_copy().is_none());
    assert_eq!(clip, vec![crate::util::hex(&bytes[8..12])]);
    // A copy result from a closed file is dropped as well.
    app.do_copy(CopyKind::Offset);
    app.close_file();
    assert!(settle(&mut app).is_empty());
}

#[test]
fn stale_hex_page_discarded_after_file_switch() {
    let gate = Gate::new();
    let d = tmpdir("switch");
    let slow = write(&d.join("slow-a.log"), &[0xAAu8; 200]);
    let fast = write(&d.join("fast-b.log"), &[0xBBu8; 100]);
    let mut app = app_with(&gate);
    app.open_file(slow);
    let PageState::Loading(r_slow) = app.session().hex().load.clone() else { panic!("expected Loading") };
    app.open_file(fast);
    assert!(matches!(app.session().hex().load, PageState::Loading(r) if r != r_slow));
    pump(&mut app, "fast page ready", |a| a.session().hex().page().is_some());
    gate.release();
    settle(&mut app);
    assert_eq!(app.session().hex().page().unwrap(), &vec![0xBBu8; 100], "stale page of the previous file was applied");
    assert_eq!(app.session().file_len(), 100);
}

#[test]
fn delayed_scan_cancel_and_open_other_file() {
    let gate = Gate::new();
    let slow_dir = tmpdir("slow-scan");
    write(&slow_dir.join("CURRENT"), b"MANIFEST-000001\n");
    let other = tmpdir("other");
    let fast = write(&other.join("fast-c.log"), &[1u8; 50]);
    let mut app = app_with(&gate);
    app.open_path(slow_dir.clone());
    pump(&mut app, "scan started", |a| a.jobs().scanning());
    assert_eq!(app.busy_label(), Some("Scanning folder…"));
    quick("cancel", || app.cancel_jobs());
    assert!(!app.jobs().scanning());
    assert_eq!(app.session().dir_note(), "Folder scan cancelled");
    assert_eq!(app.busy_label(), None, "spinner left after cancel");
    assert_eq!(app.session().hex().load, PageState::Idle);
    // Opening another file works while the cancelled scan thread is still blocked.
    app.open_path(fast.clone());
    pump(&mut app, "other file bytes", |a| a.session().hex().page().is_some() && !a.jobs().scanning());
    assert_eq!(app.session().dir(), Some(other.as_path()));
    assert_eq!(app.session().dir_entries().len(), 1);
    gate.release();
    settle(&mut app);
    assert_eq!(app.session().dir(), Some(other.as_path()), "stale scan replaced the folder");
    assert_eq!(app.session().dir_entries().len(), 1);
    assert_eq!(app.session().file(), Some(fast.as_path()));
}

#[test]
fn empty_failed_and_folder_only_never_spin() {
    let gate = Gate::new();
    gate.release();
    let d = tmpdir("states");
    let empty = write(&d.join("empty.log"), b"");
    let mut app = app_with(&gate);
    assert_eq!(app.session().hex().load, PageState::Idle);
    app.open_file(empty);
    pump(&mut app, "empty page", |a| !a.session().hex().is_loading() && a.busy_label().is_none());
    assert_eq!(app.session().hex().load, PageState::Ready(Vec::new()));
    assert_eq!(app.session().hex().size, 0);
    app.open_file(d.join("missing.log"));
    pump(&mut app, "open failure", |a| !a.session().hex().is_loading() && a.busy_label().is_none());
    assert!(matches!(app.session().hex().load, PageState::Error(_)), "{:?}", app.session().hex().load);
    // Folder without CURRENT/MANIFEST: no file is opened, nothing spins.
    app.open_path(d.clone());
    pump(&mut app, "folder scanned", |a| !a.jobs().resolving() && !a.jobs().scanning() && !a.session().dir_entries().is_empty());
    assert!(app.session().file().is_none());
    assert_eq!(app.session().hex().load, PageState::Idle);
    assert_eq!(app.busy_label(), None);
}

#[test]
fn hex_state_transitions() {
    let mut h = HexState::default();
    assert_eq!(h.load, PageState::Idle);
    h.begin(1, 0);
    assert!(h.is_loading());
    assert!(!h.complete(2, Space::File, Outcome::Done((vec![9], 1))), "other request accepted");
    assert_eq!(h.load, PageState::Loading(1));
    assert!(h.complete(1, Space::File, Outcome::Done((Vec::new(), 0))));
    assert_eq!(h.load, PageState::Ready(Vec::new()));
    // Cancel: the late answer does not resurrect the page.
    h.begin(3, 0);
    h.cancel();
    assert_eq!(h.load, PageState::Cancelled);
    assert!(!h.complete(3, Space::File, Outcome::Done((vec![1], 1))));
    assert_eq!(h.load, PageState::Cancelled);
    h.begin(4, 0);
    assert!(h.complete(4, Space::File, Outcome::Failed("boom".into())));
    assert_eq!(h.load, PageState::Error("boom".into()));
    // Wrong space (space switched while loading) is rejected.
    h.begin(5, 0);
    h.set_space(Space::Block(7), 10, "blk".into());
    assert_eq!(h.load, PageState::Idle);
    assert!(!h.complete(5, Space::File, Outcome::Done((vec![1], 1))));
    // File switch: a fresh state; the old file's completion arrives after the new request.
    let mut h = HexState::default();
    h.begin(6, 0);
    let mut h2 = HexState::default();
    h2.begin(7, 0);
    assert!(!h2.complete(6, Space::File, Outcome::Done((vec![0xAA], 1))));
    assert!(h2.complete(7, Space::File, Outcome::Done((vec![0xBB], 1))));
    assert_eq!(h2.page(), Some(&vec![0xBB]));
    h.cancel();
    assert_eq!(h.load, PageState::Cancelled);
}

#[test]
fn copy_gate_rules() {
    let p = Some(PendingCopy { req: 5, sel: 2 });
    assert!(copy_accepted(p, 5, 2, 2));
    assert!(!copy_accepted(p, 5, 2, 3), "selection changed");
    assert!(!copy_accepted(p, 4, 2, 2), "other request");
    assert!(!copy_accepted(None, 5, 2, 2), "no copy pending");
}

#[test]
fn reset_view_keeps_data_state() {
    let gate = Gate::new();
    gate.release();
    let mut app = app_with(&gate);
    for a in [Action::SetSearch("abc".into()), Action::SetZoom(1.3), Action::SetShowLeft(false), Action::SetShowHex(false), Action::SetCenterSplit(0.8)] {
        app.apply(a);
    }
    app.select_raw(Span::file(1, 2), "t");
    let e = app.prefs().layout_epoch();
    app.apply(Action::ResetView);
    let p = app.prefs();
    assert_ne!(p.layout_epoch(), e);
    assert!(p.show_left() && p.show_right() && p.show_hex());
    assert_eq!(app.search().query(), "abc");
    // Phase B (spec §3.1): Reset View restores 100 % zoom and the default center split.
    assert_eq!(p.zoom(), 1.0);
    assert_eq!(p.center_split(), crate::app::CENTER_SPLIT_DEFAULT);
    assert_eq!(app.selection().span(), Some(Span::file(1, 2)));
    assert_eq!(p.theme_id(), theme::DEFAULT_THEME);
}

#[test]
fn clear_search_keeps_selection() {
    let gate = Gate::new();
    gate.release();
    let mut app = app_with(&gate);
    app.apply(Action::SetSearch("abc".into()));
    app.select_raw(Span::file(4, 2), "t");
    app.apply(Action::ClearSearch);
    assert!(app.search().query().is_empty() && app.search().row_matches().is_empty() && app.search().hit().is_none());
    assert_eq!(app.selection().span(), Some(Span::file(4, 2)));
}

#[test]
fn zoom_ceiling_from_layout() {
    use crate::app::ZoomFit;
    let gate = Gate::new();
    gate.release();
    let mut app = app_with(&gate);
    // 1400 x 900 at 100 %, minimum layout 900 x 350 → ceiling floor(1.555) = 1.5.
    app.apply(Action::LayoutFit(ZoomFit { at_zoom: 1.0, avail_w: 1400.0, avail_h: 900.0, need_w: 900.0, need_h: 350.0 }));
    assert!((app.prefs().zoom_ceiling() - 1.5).abs() < 1e-4, "{}", app.prefs().zoom_ceiling());
    for _ in 0..10 {
        app.apply(Action::ZoomIn);
    }
    assert!((app.prefs().zoom() - 1.5).abs() < 1e-4, "zoom stops at the ceiling: {}", app.prefs().zoom());
    assert!(!app.can_zoom_in());
    assert!(app.prefs().notice().contains("layout limit (150%)"), "{}", app.prefs().notice());
    // Window shrinks: the zoom is lowered with a notice; data length never enters the fit.
    app.apply(Action::LayoutFit(ZoomFit { at_zoom: 1.5, avail_w: 600.0, avail_h: 600.0, need_w: 900.0, need_h: 350.0 }));
    assert!((app.prefs().zoom_ceiling() - 1.0).abs() < 1e-4 && (app.prefs().zoom() - 1.0).abs() < 1e-4, "{} {}", app.prefs().zoom_ceiling(), app.prefs().zoom());
    assert!(app.prefs().notice().contains("reduced to 100%"), "{}", app.prefs().notice());
    // Never below 1.0 and never above the absolute cap.
    app.apply(Action::LayoutFit(ZoomFit { at_zoom: 1.0, avail_w: 100.0, avail_h: 100.0, need_w: 900.0, need_h: 350.0 }));
    assert_eq!(app.prefs().zoom_ceiling(), 1.0);
    app.apply(Action::LayoutFit(ZoomFit { at_zoom: 1.0, avail_w: 9000.0, avail_h: 9000.0, need_w: 900.0, need_h: 350.0 }));
    assert!((app.prefs().zoom_ceiling() - 2.0).abs() < 1e-4);
}

#[test]
fn prefs_and_split_go_through_actions() {
    let gate = Gate::new();
    gate.release();
    let mut app = app_with(&gate);
    app.apply(Action::SetZoom(f32::NAN));
    assert_eq!(app.prefs().zoom(), 1.0);
    app.apply(Action::SetZoom(9.0));
    assert_eq!(app.prefs().zoom(), app.prefs().zoom_policy().max);
    app.apply(Action::SetCenterSplit(1.7));
    assert_eq!(app.prefs().center_split(), 1.0);
    app.apply(Action::ResetCenterSplit);
    assert_eq!(app.prefs().center_split(), crate::app::CENTER_SPLIT_DEFAULT);
}

#[test]
fn failed_children_mark_node_invalid() {
    use crate::jobs::{Response, Tagged};
    let gate = Gate::new();
    gate.release();
    let mut app = app_with(&gate);
    app.doc.push_test_node(TreeNode { label: "n".into(), kind: NodeKind::Root, status: Status::Present, leaf: false, depth: 0, children: None, expanded: true, loading: true, diag: None, contains: None });
    let msg = Response::Children { node: 0, children: Vec::new(), error: Some("boom".into()) };
    app.apply_response(Tagged { generation: app.session().generation(), msg });
    let nodes = app.session().nodes();
    assert_eq!(nodes[0].status, Status::Invalid);
    assert!(!nodes[0].loading);
    let kids = nodes[0].children.clone().unwrap();
    assert_eq!(kids.len(), 1);
    assert!(nodes[kids[0]].label.contains("boom"));
}

struct PanicBackend;

impl IoBackend for PanicBackend {
    fn is_dir(&self, _: &Path) -> std::io::Result<bool> {
        panic!("test panic")
    }
    fn scan(&self, _: &Path) -> std::io::Result<(Vec<crate::discover::DirEntry>, bool)> {
        panic!("test panic")
    }
    fn read(&self, _: &Path, _: u64, _: u64) -> std::io::Result<(Vec<u8>, u64)> {
        panic!("test panic")
    }
    fn stat(&self, _: &Path) -> std::io::Result<(u64, Option<std::time::SystemTime>)> {
        panic!("test panic")
    }
}

#[test]
fn io_panic_answers_with_same_kind_and_req() {
    use crate::jobs::{CopyJob, IoRequest, Response};
    let w = Worker::spawn_with(Arc::new(|| {}), Arc::new(PanicBackend));
    let p = PathBuf::from("/nonexistent/x");
    let reqs = [
        w.io(1, IoRequest::Resolve(p.clone())),
        w.io(1, IoRequest::Scan(p.clone())),
        w.io(1, IoRequest::Page { path: p.clone(), off: 0, len: 8 }),
        w.io(1, IoRequest::Copy { sel: 3, job: CopyJob::FileHex { path: p.clone(), off: 0, len: 4, requested: 4 } }),
        w.io(1, IoRequest::Stat(p.clone())),
    ];
    let mut seen = Vec::new();
    while seen.len() < reqs.len() {
        let t = w.rx.recv_timeout(Duration::from_secs(5)).expect("a response after each panic");
        let (kind, req, failed) = match t.msg {
            Response::Resolved { req, outcome, .. } => (0, req, matches!(outcome, Outcome::Failed(_))),
            Response::Scanned { req, outcome, .. } => (1, req, matches!(outcome, Outcome::Failed(_))),
            Response::Page { req, outcome, .. } => (2, req, matches!(outcome, Outcome::Failed(_))),
            Response::Copied { req, outcome, .. } => (3, req, matches!(outcome, Outcome::Failed(_))),
            Response::Stat { req, outcome } => (4, req, matches!(outcome, Outcome::Failed(_))),
            _ => panic!("unexpected response kind"),
        };
        assert_eq!(req, reqs[kind], "response kind does not match the request");
        assert!(failed);
        seen.push(kind);
    }
}

#[test]
fn file_switch_keeps_scan_of_same_folder() {
    let gate = Gate::new();
    let d = tmpdir("slow-keep");
    let f = write(&d.join("a.log"), &[3u8; 40]);
    let mut app = app_with(&gate);
    app.open_path(d.clone());
    pump(&mut app, "scan started", |a| a.jobs().scanning());
    app.open_file(f.clone()); // e.g. opened by path while the folder scan is still running
    assert!(app.jobs().scanning());
    gate.release();
    pump(&mut app, "scan done", |a| !a.jobs().scanning());
    assert_eq!(app.session().dir_entries().len(), 1, "note: {}", app.session().dir_note());
    assert_eq!(app.session().file(), Some(f.as_path()));
    settle(&mut app);
}

#[test]
fn refused_stat_does_not_flag_file_changed() {
    use crate::jobs::IoRequest;
    let gate = Gate::new();
    let d = tmpdir("slow-cap");
    let slow = write(&d.join("slow-x.log"), &[1u8; 8]);
    let f = write(&d.join("ok.log"), &[2u8; 8]);
    let mut app = app_with(&gate);
    let g = app.session().generation();
    for _ in 0..LIMITS.max_io_inflight {
        app.jobs.worker().io(g, IoRequest::Page { path: slow.clone(), off: 0, len: 8 });
    }
    app.doc.set_test_file(f.clone(), FsBackend.stat(&f).unwrap());
    let req = app.jobs.worker().io(g, IoRequest::Stat(f));
    app.jobs.set_stat_pending(req);
    pump(&mut app, "stat answered", |a| !a.jobs().stat_pending());
    assert!(!app.session().changed(), "a refused stat was reported as a change on disk");
    gate.release();
    settle(&mut app);
}

/// T-B1/T-B2: Copy value uses the exact decoded bytes, not the table preview.
#[test]
fn copy_value_uses_exact_bytes() {
    use crate::model::ValueBytes;
    let fx = |r: &str| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(r);
    let never = || false;
    // CURRENT contents: 16 bytes ending in a real LF.
    let mut i = crate::inspect::open(&fx("db-levels/CURRENT")).unwrap();
    let v = i.view(&NodeKind::Root, &never);
    let f = &v.rows.iter().find(|r| r.field.name == "contents").unwrap().field;
    let b = f.value_bytes.as_ref().expect("contents bytes");
    assert_eq!(b.data.len(), 16);
    assert_eq!(b.data.last(), Some(&0x0a));
    let (t, note) = bytes_copy_text(b, "value");
    assert_eq!(t, "MANIFEST-000005\n");
    assert!(note.contains("16 bytes") && !note.contains("escaped"), "{note}");
    // SST (zstd) data entry 0: the 148-byte value, key = internal key of key-000000.
    let mut s = crate::inspect::open(&fx("sst-zstd/data.sst")).unwrap();
    let blocks = s.children(&NodeKind::SstDataBlocks { skip: 0 }, &never);
    let v = s.view(&blocks[0].kind, &never);
    let row = &v.rows[0].field;
    let want = format!("value-000000:{}", "The quick brown fox jumps over the lazy dog. ".repeat(3));
    let val = row.value_bytes.as_ref().expect("entry value bytes");
    assert_eq!(val.data.len(), 148);
    assert_eq!(val.data, want.as_bytes());
    assert_eq!(bytes_copy_text(val, "value").0, want);
    assert_eq!(&row.key_bytes.as_ref().expect("entry key bytes").data[..10], b"key-000000");
    // Non-UTF-8: lossless escape with a note.
    let (t, note) = bytes_copy_text(&ValueBytes::of(&[b'a', 0xff, b'\\', b'\n']), "value");
    assert_eq!(t, "a\\xff\\\\\n");
    assert!(note.contains("escaped, 4 bytes"), "{note}");
}

/// Selection identity: only SelectionState changes the epoch. Non-selection actions leave it
/// alone; every selection change bumps it.
#[test]
fn only_selection_changes_the_epoch() {
    let gate = Gate::new();
    gate.release();
    let mut app = app_with(&gate);
    let e0 = app.selection().epoch();
    for a in [
        Action::SetZoom(1.2),
        Action::SetTheme("matrix"),
        Action::SetSearch("x".into()),
        Action::SetSearchHex(true),
        Action::ClearSearch,
        Action::SetShowLeft(false),
        Action::SetCenterSplit(0.3),
        Action::ResetView,
        Action::SetGoto("0x10".into()),
        Action::ShowShortcuts,
        Action::CloseShortcuts,
        Action::LayoutFit(ZoomFit { at_zoom: 1.0, avail_w: 1400.0, avail_h: 900.0, need_w: 900.0, need_h: 350.0 }),
    ] {
        app.apply(a.clone());
        assert_eq!(app.selection().epoch(), e0, "{a:?} changed the selection epoch");
    }
    app.select_raw(Span::file(0, 2), "a");
    assert_eq!(app.selection().epoch(), e0 + 1);
    app.apply(Action::HexClick(3)); // no view: raw selection
    assert_eq!(app.selection().epoch(), e0 + 2);
    assert_eq!(app.selection().span(), Some(Span::file(3, 1)));
    app.apply(Action::HexShiftClick(6));
    assert_eq!(app.selection().span(), Some(Span::file(3, 4)));
    assert_eq!(app.selection().epoch(), e0 + 3);
}

/// The copy gate (JobState) rejects a reply whose selection epoch is no longer current and
/// ignores replies that do not answer the copy in flight.
#[test]
fn copy_gate_rejects_stale_epoch() {
    use crate::app::job_state::JobState;
    use crate::jobs::CopyJob;
    let gate = Gate::new();
    gate.release();
    let mut j = JobState::new(Worker::spawn_with(Arc::new(|| {}), Arc::new(SlowBackend(gate.clone()))));
    let job = || CopyJob::Text { text: "t".into(), note: "n".into() };
    j.copy(1, 5, job());
    let p = j.pending_copy().unwrap();
    assert_eq!(p.sel, 5);
    assert_eq!(j.copy_reply(p.req + 100, 5, 5), CopyReply::Ignored, "other request");
    assert!(j.pending_copy().is_some());
    assert_eq!(j.copy_reply(p.req, 5, 6), CopyReply::Stale, "selection changed since the request");
    assert!(j.pending_copy().is_none());
    j.copy(1, 6, job());
    let p = j.pending_copy().unwrap();
    assert_eq!(j.copy_reply(p.req, 6, 6), CopyReply::Accepted);
}

/// Request identity: JobState issues increasing ids; replies with another id or an older
/// generation never change state.
#[test]
fn stale_replies_are_ignored_by_request_identity() {
    use crate::jobs::{Response, Tagged};
    let gate = Gate::new(); // scans/reads of "slow" paths stay blocked
    let d = tmpdir("slow-ids");
    let mut app = app_with(&gate);
    let g = app.session().generation();
    let a = app.jobs.page(g, d.join("x"), 0, 8);
    let b = app.jobs.page(g, d.join("y"), 0, 8);
    assert!(b > a, "ids are issued in increasing order");
    app.open_path(d.clone());
    assert!(app.jobs().resolving());
    // Reply to another resolve: ignored, the resolve stays pending, no folder is set.
    app.apply_response(Tagged { generation: g, msg: Response::Resolved { req: b + 1000, path: d.clone(), outcome: Outcome::Done(true) } });
    assert!(app.jobs().resolving() && app.session().dir().is_none());
    // Stat reply that does not answer the stat in flight: ignored.
    app.apply_response(Tagged { generation: g, msg: Response::Stat { req: b + 1001, outcome: Outcome::Done((1, None)) } });
    assert!(!app.session().changed());
    // A view of an older generation is dropped.
    app.close_file();
    app.apply_response(Tagged { generation: g, msg: Response::View { kind: NodeKind::Root, view: View::default() } });
    assert!(app.session().view().is_none());
    gate.release();
    settle(&mut app);
}

/// Phase B: panels change only through TogglePanel/SetShow* (never automatically), Reset View
/// shows its timed notice and reports defaults.
#[test]
fn panel_toggles_reset_notice_and_defaults() {
    use crate::app::view_prefs::{RESET_NOTICE, RESET_NOTICE_SECS};
    let gate = Gate::new();
    gate.release();
    let mut app = app_with(&gate);
    assert!(app.prefs().is_default_layout());
    for p in [Panel::Navigation, Panel::Explanation, Panel::Bytes] {
        app.apply(Action::TogglePanel(p));
        assert!(!app.prefs().shown(p) && !app.prefs().is_default_layout());
        app.apply(Action::TogglePanel(p));
        assert!(app.prefs().shown(p));
    }
    // Layout measurements never hide a panel (no narrow-window rule any more).
    app.apply(Action::LayoutFit(ZoomFit { at_zoom: 1.0, avail_w: 300.0, avail_h: 200.0, need_w: 900.0, need_h: 600.0 }));
    assert!(app.prefs().show_left() && app.prefs().show_right() && app.prefs().show_hex());
    app.apply(Action::TogglePanel(Panel::Explanation));
    app.apply(Action::SetZoom(1.3));
    app.apply(Action::ResetView);
    assert!(app.prefs().is_default_layout());
    assert_eq!(app.prefs().notice(), RESET_NOTICE);
    let left = app.prefs().notice_remaining().unwrap();
    assert!(left <= Duration::from_secs(RESET_NOTICE_SECS) && left > Duration::from_secs(RESET_NOTICE_SECS - 1));
    app.view_prefs.expire_notice(Instant::now() + Duration::from_secs(RESET_NOTICE_SECS - 1));
    assert_eq!(app.prefs().notice(), RESET_NOTICE, "still shown before 4 s");
    app.view_prefs.expire_notice(Instant::now() + Duration::from_secs(RESET_NOTICE_SECS) + Duration::from_millis(10));
    assert!(app.prefs().notice().is_empty(), "cleared after 4 s");
    // Zoom notices are not timed.
    app.apply(Action::LayoutFit(ZoomFit { at_zoom: 1.0, avail_w: 1000.0, avail_h: 700.0, need_w: 900.0, need_h: 600.0 }));
    app.apply(Action::ZoomIn);
    app.apply(Action::ZoomIn);
    assert!(app.prefs().notice().contains("layout limit") && app.prefs().notice_remaining().is_none());
}

/// SelectSpan (Diagnosis "Bytes" link) selects the span in its own space: a raw range when no
/// row has exactly that span; the hex view follows it.
#[test]
fn select_span_selects_in_its_space() {
    let gate = Gate::new();
    gate.release();
    let mut app = app_with(&gate);
    let e0 = app.selection().epoch();
    app.apply(Action::SelectSpan(Span::file(4, 3)));
    assert_eq!(app.selection().span(), Some(Span::file(4, 3)));
    assert_eq!(app.selection().row(), None);
    assert!(app.selection().epoch() > e0);
    assert_eq!(app.session().hex().space(), Space::File);
}
