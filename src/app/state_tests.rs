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
    // Window shrinks: the zoom is lowered with a notice (after two consecutive over-fit frames,
    // 0015 C3); data length never enters the fit.
    let small = ZoomFit { at_zoom: 1.5, avail_w: 600.0, avail_h: 600.0, need_w: 900.0, need_h: 350.0 };
    app.apply(Action::LayoutFit(small));
    assert!((app.prefs().zoom() - 1.5).abs() < 1e-4 && app.prefs().zoom_fit_pending(), "one transient frame does not lower the zoom");
    app.apply(Action::LayoutFit(small));
    assert!((app.prefs().zoom_ceiling() - 1.0).abs() < 1e-4 && (app.prefs().zoom() - 1.0).abs() < 1e-4, "{} {}", app.prefs().zoom_ceiling(), app.prefs().zoom());
    assert!(app.prefs().notice().contains("reduced to 100%"), "{}", app.prefs().notice());
    // Never below 1.0 and never above the absolute cap.
    app.apply(Action::LayoutFit(ZoomFit { at_zoom: 1.0, avail_w: 100.0, avail_h: 100.0, need_w: 900.0, need_h: 350.0 }));
    assert_eq!(app.prefs().zoom_ceiling(), 1.0);
    app.apply(Action::LayoutFit(ZoomFit { at_zoom: 1.0, avail_w: 9000.0, avail_h: 9000.0, need_w: 900.0, need_h: 350.0 }));
    assert!((app.prefs().zoom_ceiling() - 2.0).abs() < 1e-4);
}

/// 0015 C3 GUI-3: A+ enabled ⇔ Zoom In changes the zoom, at every step; a fit measured a
/// hair below the new zoom right after a step (rounding) does not snap it back; the ceiling
/// of a 900×700-like window (raw 1.205) is reached and kept.
#[test]
fn zoom_in_and_its_enabled_state_agree() {
    let gate = Gate::new();
    gate.release();
    let mut app = app_with(&gate);
    let fit = |z: f32, scale: f32| ZoomFit { at_zoom: z, avail_w: 900.0 / z, avail_h: 675.0 * scale / z, need_w: 742.6, need_h: 560.0 };
    app.apply(Action::LayoutFit(fit(1.0, 1.0)));
    for _ in 0..5 {
        let (can, before) = (app.can_zoom_in(), app.prefs().zoom());
        app.apply(Action::ZoomIn);
        assert_eq!(can, (app.prefs().zoom() - before).abs() > 1e-4, "A+ enabled {can} but Zoom In {before} → {}", app.prefs().zoom());
        let z = app.prefs().zoom();
        app.apply(Action::LayoutFit(fit(z, 1.0)));
        app.apply(Action::LayoutFit(fit(z, 1.0)));
    }
    assert!((app.prefs().zoom() - 1.2).abs() < 1e-4, "{}", app.prefs().zoom());
    assert!(!app.can_zoom_in());
    // Measured 0.5 % low at 120 % (rounding): the ceiling reads 110 %, the zoom stays.
    app.apply(Action::LayoutFit(fit(1.2, 0.995)));
    app.apply(Action::LayoutFit(fit(1.2, 0.995)));
    assert!((app.prefs().zoom() - 1.2).abs() < 1e-4, "snapped back to {}", app.prefs().zoom());
    assert!(!app.can_zoom_in());
    app.apply(Action::ZoomIn);
    assert!((app.prefs().zoom() - 1.2).abs() < 1e-4);
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
    app.doc.push_test_node(TreeNode {
        label: "n".into(),
        kind: NodeKind::Root,
        status: Status::Present,
        leaf: false,
        depth: 0,
        children: None,
        expanded: true,
        loading: true,
        diag: None,
        contains: None,
        role: ChildRole::Structure,
        checked: None,
    });
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
    app.apply_response(Tagged {
        generation: g,
        msg: Response::View { req: 1, target: crate::jobs::ViewTarget::Node(NodeKind::Root), kind: Some(NodeKind::Root), view: Box::default(), reveal: None, select: None },
    });
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

// ---------------- task 0006 / handoff 0011: navigation and synchronized selection ----------

fn fixture(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn open_ready(rel: &str) -> App {
    let gate = Gate::new();
    gate.release();
    let mut app = app_with(&gate);
    app.apply(Action::OpenFile(fixture(rel)));
    pump(&mut app, "opened", |a| a.session().view().is_some() && a.session().pending_view().is_none() && !a.session().roots().is_empty() && a.session().hex().page().is_some());
    app
}

fn view_is(app: &mut App, k: &NodeKind) {
    let k2 = k.clone();
    pump(app, "view", move |a| a.session().pending_view().is_none() && a.session().view_kind() == Some(&k2));
}

/// Navigate expands the canonical ancestors, selects and reveals the canonical node (never a
/// reference), shows its view; a reference node click navigates; Back returns.
#[test]
fn navigate_reveals_canonical_node_and_back_returns() {
    let mut app = open_ready("tests/fixtures/sst-variants/partitioned.sst");
    let part = NodeKind::SstBlock { off: 0x18073, size: 282, kind: BlockKind::IndexPartition };
    app.apply(Action::Navigate { target: part.clone(), span: None });
    view_is(&mut app, &part);
    let i = app.session().sel_node().expect("tree node selected");
    assert_eq!((&app.session().nodes()[i].kind, app.session().nodes()[i].role), (&part, ChildRole::Structure));
    assert_eq!(app.take_scroll_request().tree, Some(i), "single-use reveal request");
    assert_eq!(app.take_scroll_request().tree, None);
    assert!(app.session().hex().page_base() <= 0x18073);
    // A reference node (index entry → data block) navigates to the canonical block.
    let top = app.session().nodes().iter().position(|n| matches!(n.kind, NodeKind::SstBlock { kind: BlockKind::TopIndex, .. })).unwrap();
    app.apply(Action::ToggleNode(top));
    pump(&mut app, "top index children", move |a| a.session().nodes()[top].children.is_some());
    let r = app.session().nodes().iter().position(|n| n.role == ChildRole::Reference).expect("reference child");
    let target = app.session().nodes()[r].kind.clone();
    app.apply(Action::SelectNode(r));
    view_is(&mut app, &target);
    let j = app.session().sel_node().unwrap();
    assert_eq!((&app.session().nodes()[j].kind, app.session().nodes()[j].role), (&target, ChildRole::Structure), "canonical node, not the reference");
    // Back returns to the partition (and selects its node again).
    app.apply(Action::Back);
    view_is(&mut app, &part);
    assert_eq!(app.session().sel_node(), Some(i));
}

/// Row "count" of the first WriteBatch fills its file bytes [0xf,0x13) (exact provenance);
/// clicking byte 0x11 selects the same row with the byte outlined, in one step; a reveal or
/// repeated poll never changes the selection.
#[test]
fn hex_click_and_row_select_agree_without_loops() {
    let mut app = open_ready("examples/files/wal-plain/000004.log");
    app.apply(Action::Navigate { target: NodeKind::LogRecord(0), span: None });
    view_is(&mut app, &NodeKind::LogRecord(0));
    let count = app.session().view().unwrap().rows.iter().position(|r| r.field.kind == FieldKind::Count).unwrap();
    app.apply(Action::SelectRow(count));
    assert!(app.selection().spans().any(|s| s == Span::file(0xf, 4)), "{:?}", app.selection().spans().collect::<Vec<_>>());
    assert_eq!(app.session().hex().space(), Space::File, "file offset is the primary location");
    let e = app.selection().epoch();
    app.apply(Action::HexClick(0x11));
    assert_eq!((app.selection().row(), app.selection().anchor(), app.selection().epoch()), (Some(count), Some(Span::file(0x11, 1)), e + 1));
    assert!(app.session().pending_view().is_none(), "the current table answered: no locate request");
    for _ in 0..5 {
        app.poll();
        let _ = app.take_scroll_request();
    }
    assert_eq!(app.selection().epoch(), e + 1, "no feedback loop");
    // From the root view the byte is located inside its record (worker request), then selected.
    app.apply(Action::SelectRoot);
    view_is(&mut app, &NodeKind::Root);
    app.apply(Action::HexClick(0x11));
    view_is(&mut app, &NodeKind::LogRecord(0));
    let row = app.selection().row().expect("field selected after locate");
    assert_eq!(app.session().view().unwrap().rows[row].field.kind, FieldKind::Count);
    assert_eq!(app.selection().anchor(), Some(Span::file(0x11, 1)));
    // Shift+click: raw range from the anchor, row cleared, honest note.
    app.apply(Action::HexShiftClick(0x14));
    assert_eq!((app.selection().row(), app.selection().span()), (None, Some(Span::file(0x11, 4))));
    assert_eq!(app.selection().raw_note(), Some("Raw range [0x11,0x15): not a single field"));
}

/// A pending (async) view and its selection are applied only if nothing was selected since the
/// request (0015 residual 1: the stale view no longer replaces the table either).
#[test]
fn pending_selection_never_overrides_a_newer_choice() {
    let mut app = open_ready("examples/files/wal-plain/000004.log");
    app.apply(Action::HexClick(0x11)); // root view: locate request
    app.apply(Action::SelectSpan(Span::file(2, 1))); // newer user choice before the reply
    let e = app.selection().epoch();
    pump(&mut app, "locate answered", |a| a.session().pending_view().is_none());
    assert_eq!(app.session().view_kind(), Some(&NodeKind::Root), "the stale locate replaced the view");
    assert_eq!((app.selection().span(), app.selection().epoch()), (Some(Span::file(2, 1)), e), "the newer selection stays");
}

/// 0015 residual 1 (reviewer note 4 of 0011): a Locate / Navigate / Back result that arrives
/// after a newer selection, navigation or file change never replaces the newer view or
/// selection; a result nobody overtook still applies. Replies are applied only by `poll()`, so
/// an action issued before polling lands exactly between request and reply (controlled delay).
#[test]
fn stale_view_results_are_discarded() {
    // (a) Locate overtaken by a row click in the current table.
    let mut app = open_ready("examples/files/wal-plain/000004.log");
    app.apply(Action::HexClick(0x11));
    assert!(app.session().pending_view().is_some(), "locate request in flight");
    app.apply(Action::SelectRow(0));
    let snap = app.selection().snapshot();
    pump(&mut app, "locate answered", |a| a.session().pending_view().is_none());
    settle(&mut app);
    assert_eq!(app.session().view_kind(), Some(&NodeKind::Root), "stale locate replaced the view");
    assert_eq!(app.selection().snapshot(), snap, "stale locate replaced the selection");
    assert_eq!(app.busy_label(), None, "no spinner left for the discarded view");
    // Navigate and Back overtaken by a row click: discarded the same way.
    app.apply(Action::Navigate { target: NodeKind::LogRecord(0), span: None });
    app.apply(Action::SelectRow(1));
    pump(&mut app, "navigate answered", |a| a.session().pending_view().is_none());
    assert_eq!((app.session().view_kind(), app.selection().row()), (Some(&NodeKind::Root), Some(1)));
    // A tree click overtaken by a row click: the view stays and the tree follows it again.
    app.apply(Action::SelectNode(app.session().roots()[0]));
    assert!(app.session().sel_node().is_some());
    app.apply(Action::SelectRow(2));
    pump(&mut app, "tree view answered", |a| a.session().pending_view().is_none());
    assert_eq!((app.session().view_kind(), app.session().sel_node(), app.selection().row()), (Some(&NodeKind::Root), None, Some(2)));
    app.apply(Action::Navigate { target: NodeKind::LogRecord(0), span: None });
    view_is(&mut app, &NodeKind::LogRecord(0));
    app.apply(Action::Back);
    app.apply(Action::SelectRow(0));
    pump(&mut app, "back answered", |a| a.session().pending_view().is_none());
    assert_eq!((app.session().view_kind(), app.selection().row()), (Some(&NodeKind::LogRecord(0)), Some(0)));
    // (b) Locate overtaken by a file change: the old generation's reply is dropped.
    app.apply(Action::SelectRoot);
    view_is(&mut app, &NodeKind::Root);
    app.apply(Action::HexClick(0x11));
    let other = fixture("examples/files/wal-fragmented/000004.log");
    app.apply(Action::OpenFile(other.clone()));
    pump(&mut app, "other file", |a| a.session().view().is_some() && a.session().pending_view().is_none() && a.session().hex().page().is_some());
    settle(&mut app);
    assert_eq!((app.session().file(), app.session().view_kind()), (Some(other.as_path()), Some(&NodeKind::Root)));
    assert_eq!(app.selection().row(), None, "no located field of the previous file");
    // (c) A fresh result (nothing selected since the request) still applies.
    app.apply(Action::HexClick(0x11));
    view_is(&mut app, &NodeKind::LogRecord(0));
    assert!(app.selection().row().is_some(), "fresh locate selects its field");
    assert_eq!(app.selection().anchor(), Some(Span::file(0x11, 1)));
}

/// `]` / `[`: one field per press in span order; Shift: one sibling structure per press.
#[test]
fn keyboard_steps_are_single() {
    let mut app = open_ready("examples/files/wal-plain/000004.log");
    app.apply(Action::Navigate { target: NodeKind::LogRecord(0), span: None });
    view_is(&mut app, &NodeKind::LogRecord(0));
    let v = app.session().view().unwrap().clone();
    let mut order: Vec<(usize, Span)> = v.rows.iter().enumerate().filter_map(|(i, r)| r.field.span.map(|s| (i, s))).collect();
    order.sort_by_key(|(i, s)| (matches!(s.space, Space::File) as u8 ^ 1, s.offset, *i));
    let e = app.selection().epoch();
    app.apply(Action::StepField(true));
    assert_eq!((app.selection().row(), app.selection().epoch()), (Some(order[0].0), e + 1));
    app.apply(Action::StepField(true));
    assert_eq!(app.selection().row(), Some(order[1].0));
    app.apply(Action::StepField(false));
    assert_eq!((app.selection().row(), app.selection().epoch()), (Some(order[0].0), e + 3));
    // Structure steps: from the Physical blocks node to the next sibling (Logical records).
    app.apply(Action::SelectNode(app.session().roots()[0]));
    view_is(&mut app, &NodeKind::LogBlocks { first: 0, count: 1 });
    app.apply(Action::StepStructure(true));
    view_is(&mut app, &NodeKind::LogRecords { start: 0, page: 0 });
    assert_eq!(app.session().sel_node(), Some(app.session().roots()[1]));
}

/// 0011 fix G1/G2/G4: a reference link selects the whole target structure (never "Nothing
/// selected") with its file range and the origin for "Referenced from"; Back restores the exact
/// previous selection (row, span, anchor) and byte view; a tree click and Shift+] / Shift+[
/// take the same path (Structure selection, one sibling per press, tree reveal).
#[test]
fn navigate_selects_structure_and_back_restores_selection() {
    use crate::app::selection::SelTarget;
    let mut app = open_ready("examples/files/sst-zstd/sst-zstd.sst");
    let index = app.session().nodes().iter().find(|n| matches!(n.kind, NodeKind::SstBlock { kind: BlockKind::Index, .. })).map(|n| n.kind.clone()).unwrap();
    app.apply(Action::Navigate { target: index.clone(), span: None });
    view_is(&mut app, &index);
    // Tree-click / navigation path: the whole structure, its extent as the span.
    assert_eq!(app.selection().target(), &SelTarget::Structure);
    assert_eq!(app.selection().span(), app.session().view().unwrap().extent);
    // Select index entry 2 (a link row) with its byte, then follow the link.
    let row = app.session().view().unwrap().rows.iter().position(|r| r.field.name == "entry 2" && r.field.link.is_some()).expect("entry 2");
    app.apply(Action::SelectRow(row));
    let before = app.selection().snapshot();
    let hex_before = (app.session().hex().space(), app.session().hex().page_base());
    let target = app.session().view().unwrap().rows[row].field.link.clone().unwrap();
    app.apply(Action::FollowRow(row));
    view_is(&mut app, &target);
    assert_eq!(app.selection().target(), &SelTarget::Structure, "never Nothing selected after a navigate");
    let ext = app.session().view().unwrap().extent.expect("data block extent");
    assert_eq!(app.selection().span(), Some(ext));
    assert!(app.selection().via().is_some_and(|v| v.starts_with("entry 2 of Index block")), "{:?}", app.selection().via());
    assert!(app.session().hex().page_base() <= ext.offset, "byte view moved to the target");
    // Back: same selection as before the link, same byte view.
    app.apply(Action::Back);
    view_is(&mut app, &index);
    assert_eq!(app.selection().snapshot(), before);
    assert_eq!((app.session().hex().space(), app.session().hex().page_base()), hex_before);
    // Structure steps = tree click path: Structure selection, one sibling per press.
    let i = app.session().sel_node().unwrap();
    let next = app.session().sibling(i, true).unwrap();
    let k = app.session().nodes()[next].kind.clone();
    app.apply(Action::StepStructure(true));
    view_is(&mut app, &k);
    assert_eq!((app.session().sel_node(), app.selection().target()), (Some(next), &SelTarget::Structure));
    assert_eq!(app.take_scroll_request().tree, Some(next), "the tree reveals the stepped node");
    let tree_click = app.selection().snapshot();
    app.apply(Action::SelectNode(i));
    view_is(&mut app, &index);
    app.apply(Action::SelectNode(next));
    view_is(&mut app, &k);
    assert_eq!(app.selection().snapshot(), tree_click, "tree click and structure step select the same");
}

// ---------------- task 0008 / handoff 0015: byte-less navigation and check status -----------

fn row_named(app: &App, name: &str) -> usize {
    app.session().view().unwrap().rows.iter().position(|r| r.field.name == name).unwrap_or_else(|| panic!("row {name}"))
}

/// C2 (reviewer 1): SelectRoot replaces an in-flight Locate's pending selection, so the stale
/// click is not applied to the newer Root view.
#[test]
fn select_root_drops_pending_locate_selection() {
    let mut app = open_ready("examples/files/wal-plain/000004.log");
    app.apply(Action::Navigate { target: NodeKind::LogRecord(0), span: None });
    view_is(&mut app, &NodeKind::LogRecord(0));
    app.apply(Action::SelectRoot);
    view_is(&mut app, &NodeKind::Root);
    // Root view: a byte click starts a Locate; SelectRoot follows before any reply.
    app.apply(Action::HexClick(0x11));
    let e = app.selection().epoch();
    app.apply(Action::SelectRoot);
    pump(&mut app, "views answered", |a| a.session().pending_view().is_none());
    settle(&mut app);
    assert_eq!(app.session().view_kind(), Some(&NodeKind::Root));
    assert!(app.selection().anchor().is_none() && app.selection().epoch() > e, "stale click applied to Root: {:?}", app.selection().raw_note());
}

/// C2 (reviewer 2): a dropped (overtaken) Back reply puts its history entry back, and a dropped
/// Navigate reply removes the source entry it pushed — the next Back never skips a level.
#[test]
fn overtaken_back_or_navigate_keeps_history_intact() {
    let mut app = open_ready("examples/files/wal-plain/000004.log");
    app.apply(Action::Navigate { target: NodeKind::LogRecord(0), span: None });
    view_is(&mut app, &NodeKind::LogRecord(0));
    assert!(app.session().can_go_back());
    // Back overtaken by a row click: the entry for Root must still be there.
    app.apply(Action::Back);
    app.apply(Action::SelectRow(0));
    pump(&mut app, "back answered", |a| a.session().pending_view().is_none());
    assert_eq!(app.session().view_kind(), Some(&NodeKind::LogRecord(0)));
    assert!(app.session().can_go_back(), "the popped history entry was lost");
    app.apply(Action::Back);
    view_is(&mut app, &NodeKind::Root);
    assert!(!app.session().can_go_back());
    // Navigate overtaken by a row click: no stale source entry stays on the stack.
    app.apply(Action::Navigate { target: NodeKind::LogRecord(0), span: None });
    app.apply(Action::SelectRow(1));
    pump(&mut app, "navigate answered", |a| a.session().pending_view().is_none());
    assert_eq!(app.session().view_kind(), Some(&NodeKind::Root));
    assert!(!app.session().can_go_back(), "a dropped navigation left its source entry");
}

/// Residual 3 (design B1–B7): a link to a record whose selected row has no bytes moves the byte
/// view to the record's FIRST fragment header (file), the Go-to text follows, nothing is
/// selected or filled; Back restores selection, byte view and Go-to text without an extra
/// history entry; a file change resets Go-to.
#[test]
fn byteless_row_reveals_enclosing_structure() {
    let mut app = open_ready("tests/fixtures/wal-fragmented/000004.log");
    // The fragment's "record" row (Implicit) links to the logical record.
    app.apply(Action::Navigate { target: NodeKind::LogFragment(0x5eec), span: None });
    view_is(&mut app, &NodeKind::LogFragment(0x5eec));
    let link = row_named(&app, "record");
    app.apply(Action::SelectRow(link));
    assert_eq!(app.selection().shown(), Some(Span::file(0x5eec, 1)), "fragment's first byte shown");
    app.apply(Action::FollowRow(link));
    view_is(&mut app, &NodeKind::LogRecord(0x5eec));
    // Move the byte view elsewhere first, then select the Implicit "reassembly" row.
    app.apply(Action::SetGoto("0x10".into()));
    app.apply(Action::GotoOffset);
    assert_eq!(app.session().hex().scroll_to, Some(0x10));
    let re = row_named(&app, "reassembly");
    assert!(app.session().view().unwrap().rows[re].field.span.is_none());
    app.apply(Action::SelectRow(re));
    assert_eq!((app.session().hex().space(), app.session().hex().scroll_to), (Space::File, Some(0x5eec)), "FIRST header offset revealed in the file");
    assert_eq!(app.session().goto(), "0x5eec", "Go-to text follows");
    assert_eq!((app.selection().span(), app.selection().spans().count(), app.selection().anchor()), (None, 0, None), "no fabricated span");
    assert_eq!(app.selection().shown(), Some(Span::file(0x5eec, 1)));
    assert_eq!(provenance::byteless_status("reassembly", &app.selection().shown().unwrap()), "selection: reassembly · no bytes · showing file 0x5eec");
    assert!(provenance::byteless_where(app.session().view(), &Span::file(0x5eec, 1)).ends_with("enclosing Logical record @0x5eec: file 0x5eec."));
    // Back: everything restored; the reveal added no history entry.
    let before = (app.selection().snapshot(), app.session().hex().space(), app.session().goto().to_string());
    let f1 = app.session().view().unwrap().rows.iter().position(|r| r.field.name.starts_with("fragment 1")).unwrap();
    let target = app.session().view().unwrap().rows[f1].field.link.clone().unwrap();
    app.apply(Action::FollowRow(f1));
    view_is(&mut app, &target);
    assert_ne!(app.session().goto(), "0x5eec", "navigation moved the byte view and Go-to");
    app.apply(Action::Back);
    view_is(&mut app, &NodeKind::LogRecord(0x5eec));
    assert_eq!((app.selection().snapshot(), app.session().hex().space(), app.session().goto().to_string()), before, "Back restores selection, space and Go-to");
    app.apply(Action::Back);
    view_is(&mut app, &NodeKind::LogFragment(0x5eec));
    assert_eq!(app.selection().row(), Some(link), "the reveal itself added no history entry");
    // A file change resets Go-to to the placeholder.
    app.apply(Action::OpenFile(fixture("tests/fixtures/wal-basic/000004.log")));
    assert_eq!(app.session().goto(), "");
}

/// 0015 C3 UI-6: a parent group counts blocks, not sub-groups: after its sub-group #0–#63 is
/// listed (64 blocks checked) the parent reads "64 of 4096 checked", no glyph.
#[test]
fn parent_group_coverage_counts_blocks() {
    let mut app = App::idle_for_tests();
    let group = |first: u64, count: u64| NodeKind::SstLayoutGroup { first, count };
    app.doc.push_test_node(TreeNode {
        label: "Data blocks #0–#4095 · [0x0,0x3a0f00) · 4096 blocks".into(),
        kind: group(0, 4096),
        status: Status::Unknown,
        leaf: false,
        depth: 0,
        children: None,
        expanded: true,
        loading: true,
        diag: None,
        contains: None,
        role: ChildRole::Structure,
        checked: Some((0, 4096)),
    });
    let kids: Vec<Child> =
        (0..64u64)
            .map(|i| {
                Child::new(format!("Data blocks #{}–#{} · [..) · 64 blocks", i * 64, i * 64 + 63), group(i * 64, 64))
                    .status(if i == 0 { Status::Present } else { Status::Unknown })
                    .checked(if i == 0 { (64, 64) } else { (0, 64) })
            })
            .collect();
    app.doc.children_arrived(0, kids, None);
    let p = &app.session().nodes()[0];
    assert_eq!(p.checked, Some((64, 4096)));
    assert_eq!(p.full_label(), "Data blocks #0–#4095 · 64 of 4096 checked · [0x0,0x3a0f00) · 4096 blocks");
    assert!(p.unchecked(), "partly checked and clean: no glyph");
}

/// 0015 C3 (B3 drift): the Go-to text follows whatever the byte pane moves to — paging, a
/// space switch, a view that resets the pane to the file start.
#[test]
fn goto_text_follows_the_byte_pane() {
    let mut app = open_ready("tests/fixtures/wal-fragmented/000004.log");
    app.apply(Action::HexPage(0x10000));
    assert_eq!((app.session().hex().page_base(), app.session().goto()), (0x10000, "0x10000"));
    app.apply(Action::Navigate { target: NodeKind::LogRecord(0x5eec), span: None });
    view_is(&mut app, &NodeKind::LogRecord(0x5eec));
    assert_eq!(app.session().goto(), "0x5eec", "navigation shows the record start");
    app.apply(Action::HexShowSpace(Space::Record(0x5eec)));
    assert_eq!((app.session().hex().space(), app.session().goto()), (Space::Record(0x5eec), "0x0"));
    // Root does not describe the record space: the pane returns to file 0x0, Go-to follows.
    app.apply(Action::SelectRoot);
    view_is(&mut app, &NodeKind::Root);
    assert_eq!((app.session().hex().space(), app.session().goto()), (Space::File, "0x0"));
}

/// B2: in a derived space (BLK+) the byte view stays there and shows +0x0; without an honest
/// enclosing range (WAL root "file" row) the byte view and Go-to stay unchanged (B6).
#[test]
fn byteless_row_keeps_derived_space_or_stays() {
    let mut app = open_ready("examples/files/sst-zstd/sst-zstd.sst");
    let blk = app.session().nodes().iter().find_map(|n| match n.kind {
        NodeKind::SstBlock { off, size, kind: BlockKind::Data } => Some((off, size)),
        _ => None,
    });
    let (off, size) = blk.unwrap_or((0, 99));
    let entry = NodeKind::SstEntry { off, size, kind: BlockKind::Data, idx: 0 };
    app.apply(Action::Navigate { target: entry.clone(), span: None });
    view_is(&mut app, &entry);
    app.apply(Action::HexShowSpace(Space::Block(off)));
    pump(&mut app, "decompressed page", |a| a.session().hex().page().is_some());
    app.apply(Action::SetGoto("0x20".into()));
    app.apply(Action::GotoOffset);
    let key = row_named(&app, "key (reconstructed)");
    app.apply(Action::SelectRow(key));
    assert_eq!((app.session().hex().space(), app.session().hex().scroll_to, app.session().goto()), (Space::Block(off), Some(0), "0x0"), "BLK+ stays in BLK+ at +0x0");
    assert_eq!((app.selection().span(), app.selection().shown()), (None, Some(Span::new(Space::Block(off), 0, 1))));
    // WAL root "file" row: no enclosing range — nothing moves.
    let mut app = open_ready("examples/files/wal-plain/000004.log");
    app.apply(Action::SetGoto("0x10".into()));
    app.apply(Action::GotoOffset);
    let file = row_named(&app, "file");
    app.apply(Action::SelectRow(file));
    assert_eq!((app.session().hex().scroll_to, app.session().goto(), app.selection().shown()), (Some(0x10), "0x10", None));
}

/// F3: row keys scroll the table minimally; a jump (navigation, Back) centers the selected row.
#[test]
fn row_scroll_alignment_follows_the_kind_of_move() {
    let mut app = open_ready("examples/files/wal-plain/000004.log");
    app.apply(Action::Navigate { target: NodeKind::LogRecord(0), span: None });
    view_is(&mut app, &NodeKind::LogRecord(0));
    let _ = app.take_scroll_request();
    app.apply(Action::MoveRow(RowMove::Down));
    let r = app.selection().row().unwrap();
    assert_eq!(app.take_scroll_request().row, Some((r, false)), "keys: minimal scroll");
    app.apply(Action::StepField(true));
    assert!(matches!(app.take_scroll_request().row, Some((_, false))));
    let count = app.session().view().unwrap().rows.iter().position(|x| x.field.kind == FieldKind::Count).unwrap();
    app.apply(Action::SelectRow(count));
    app.apply(Action::Navigate { target: NodeKind::Root, span: None });
    view_is(&mut app, &NodeKind::Root);
    app.apply(Action::Back);
    view_is(&mut app, &NodeKind::LogRecord(0));
    assert_eq!(app.take_scroll_request().row, Some((count, true)), "Back: centered");
}

/// Residual 2 / A8: opening a Not checked layout row is its check; Back shows the updated row.
#[test]
fn opening_unchecked_row_checks_it_and_back_shows_it() {
    let mut app = open_ready("tests/fixtures/sst-none/data.sst");
    let layout = app.session().nodes().iter().position(|n| n.kind == NodeKind::SstLayout).expect("layout table node");
    app.apply(Action::SelectNode(layout));
    view_is(&mut app, &NodeKind::SstLayout);
    let cell = |a: &App| a.session().view().unwrap().rows[0].cells.last().cloned().unwrap();
    assert_eq!(cell(&app), NOT_CHECKED);
    app.apply(Action::FollowRow(0));
    let b0 = NodeKind::SstBlock { off: 0, size: 4057, kind: BlockKind::Data };
    view_is(&mut app, &b0);
    app.apply(Action::Back);
    view_is(&mut app, &NodeKind::SstLayout);
    assert_eq!(cell(&app), "✔ Present");
}
