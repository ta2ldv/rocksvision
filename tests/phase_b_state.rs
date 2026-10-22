//! Phase B regressions (task 0003 / handoff 0005) through the public API only: `AppState`,
//! `Action`, `Worker::spawn_with` + a custom `IoBackend`. Std + rocksvision, no GUI.
//! Fixtures are only read (never written).
mod common;
use common::fx;
use rocksvision::app::{Action, AppState, CopyKind, ZoomFit, ZoomPolicy, CENTER_SPLIT_DEFAULT};
use rocksvision::discover::DirEntry;
use rocksvision::jobs::{FsBackend, IoBackend, Worker};
use rocksvision::model::Span;
use rocksvision::ui::theme::theme_by_id;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Latch with a 10 s safety timeout so a failing test cannot hang.
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
        let _ = self.cv.wait_timeout_while(g, Duration::from_secs(10), |o| !*o).unwrap();
    }
    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.cv.notify_all();
    }
}

/// Backend that blocks (on `gate`) reads of paths containing `needle`. With `first_only`,
/// only the first read after `armed` is set blocks; later reads pass.
struct Hold {
    gate: Arc<Gate>,
    needle: &'static str,
    armed: AtomicBool,
    first_only: bool,
    entered: AtomicUsize,
}
impl Hold {
    fn new(gate: &Arc<Gate>, needle: &'static str, armed: bool, first_only: bool) -> Arc<Self> {
        Arc::new(Hold { gate: gate.clone(), needle, armed: AtomicBool::new(armed), first_only, entered: AtomicUsize::new(0) })
    }
}
struct HoldBackend(Arc<Hold>);
impl IoBackend for HoldBackend {
    fn is_dir(&self, p: &Path) -> std::io::Result<bool> {
        FsBackend.is_dir(p)
    }
    fn scan(&self, d: &Path) -> std::io::Result<(Vec<DirEntry>, bool)> {
        FsBackend.scan(d)
    }
    fn read(&self, p: &Path, o: u64, l: u64) -> std::io::Result<(Vec<u8>, u64)> {
        let h = &self.0;
        if h.armed.load(Ordering::SeqCst) && p.to_string_lossy().contains(h.needle) {
            let n = h.entered.fetch_add(1, Ordering::SeqCst);
            if !h.first_only || n == 0 {
                h.gate.wait();
            }
        }
        FsBackend.read(p, o, l)
    }
    fn stat(&self, p: &Path) -> std::io::Result<(u64, Option<std::time::SystemTime>)> {
        FsBackend.stat(p)
    }
}

fn app_with(h: &Arc<Hold>) -> AppState {
    AppState::new(Worker::spawn_with(Arc::new(|| {}), Arc::new(HoldBackend(h.clone()))))
}

fn pump(app: &mut AppState, what: &str, done: impl Fn(&AppState) -> bool) -> Vec<String> {
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

/// Everything answered: root view shown and the first byte page loaded.
fn opened(a: &AppState) -> bool {
    a.session().view().is_some() && a.session().pending_view().is_none() && a.session().hex().page().is_some()
}

const CURRENT_BYTES: &[u8] = b"MANIFEST-000005\n";

// (a) -------------------------------------------------------------------------------------
#[test]
fn reset_view_restores_layout_and_keeps_data_state() {
    let h = Hold::new(&Gate::new(), "none", false, false);
    let mut app = app_with(&h);
    let cur = fx("db-levels/CURRENT");
    app.apply(Action::OpenFile(cur.clone()));
    pump(&mut app, "CURRENT opened", opened);
    assert_eq!(app.session().hex().page().unwrap(), &CURRENT_BYTES.to_vec(), "page bytes of the real fixture");

    app.apply(Action::SetTheme(theme_by_id("matrix").id));
    app.apply(Action::SetZoom(1.3));
    app.apply(Action::SelectRow(0));
    app.apply(Action::SetSearch("MANIFEST".into()));
    app.apply(Action::SetCenterSplit(0.75));
    app.apply(Action::SetShowLeft(false));
    app.apply(Action::SetShowRight(false));
    app.apply(Action::SetShowHex(false));
    assert!((app.prefs().zoom() - 1.3).abs() < 1e-5 && app.prefs().theme_id() == "matrix");
    let (file, sel, sel_row, search, matches, view_rows) = (app.session().file().map(|p| p.to_path_buf()), app.selection().span(), app.selection().row(), app.search().query().to_string(), app.search().row_matches().to_vec(), app.session().view().unwrap().rows.len());
    assert!(sel.is_some() && sel_row == Some(0) && !matches.is_empty(), "precondition: selection and search matches");
    let epoch = app.prefs().layout_epoch();

    app.apply(Action::ResetView);

    assert_eq!(app.prefs().layout_epoch(), epoch.wrapping_add(1), "layout_epoch incremented once");
    assert_eq!(app.prefs().zoom(), 1.0, "zoom back to 100 %");
    assert_eq!(app.prefs().center_split(), CENTER_SPLIT_DEFAULT);
    assert!(app.prefs().show_left() && app.prefs().show_right() && app.prefs().show_hex(), "panel visibility default");
    assert_eq!(app.session().file().map(|p| p.to_path_buf()), file, "file preserved");
    assert_eq!(app.selection().span(), sel, "selection preserved");
    assert_eq!(app.selection().row(), sel_row);
    assert_eq!(app.search().query(), search, "search preserved");
    assert_eq!(app.search().row_matches(), &matches[..]);
    assert_eq!(app.prefs().theme_id(), "matrix", "theme preserved");
    assert_eq!(app.session().view().unwrap().rows.len(), view_rows);
    assert_eq!(app.session().hex().page().unwrap(), &CURRENT_BYTES.to_vec());
}

// (b) -------------------------------------------------------------------------------------
#[test]
fn stale_delayed_copy_cannot_overwrite_newer_selection_clipboard() {
    let gate = Gate::new();
    // Only the FIRST read of CURRENT after arming blocks: the older copy.
    let h = Hold::new(&gate, "CURRENT", false, true);
    let mut app = app_with(&h);
    app.apply(Action::OpenFile(fx("db-levels/CURRENT")));
    pump(&mut app, "opened", opened);
    h.armed.store(true, Ordering::SeqCst);

    app.select_raw(Span::file(0, 4), "A");
    app.do_copy(CopyKind::Hex); // read blocks in the backend
    let t = Instant::now();
    while h.entered.load(Ordering::SeqCst) < 1 {
        assert!(t.elapsed() < Duration::from_secs(5), "older copy never reached the backend");
        std::thread::sleep(Duration::from_millis(2));
    }
    app.select_raw(Span::file(5, 3), "B"); // newer selection while A is still blocked
    app.do_copy(CopyKind::Hex);
    let clip = pump(&mut app, "newer copy answered", |a| a.jobs().pending_copy().is_none());
    assert_eq!(clip, vec!["45 53 54".to_string()], "newer selection's bytes (EST) only");

    gate.release(); // the OLDER copy now completes, after the newer one
    let t = Instant::now();
    let mut late = Vec::new();
    while t.elapsed() < Duration::from_millis(600) {
        late.extend(app.poll());
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(late.is_empty(), "stale older copy reached the clipboard after the newer one: {late:?}");
    assert!(app.jobs().pending_copy().is_none());
}

// (c) -------------------------------------------------------------------------------------
#[test]
fn late_completion_is_rejected_after_opening_another_file() {
    let gate = Gate::new();
    let h = Hold::new(&gate, "wal-fragmented", true, false);
    let mut app = app_with(&h);
    app.apply(Action::OpenFile(fx("wal-fragmented/000004.log")));
    let t = Instant::now();
    while h.entered.load(Ordering::SeqCst) < 1 {
        assert!(t.elapsed() < Duration::from_secs(5), "delayed page read never started");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(app.session().hex().is_loading(), "page of the slow file is pending");
    let g_old = app.session().generation();

    let cur = fx("db-levels/CURRENT");
    app.apply(Action::OpenFile(cur.clone()));
    assert!(app.session().generation() > g_old);
    pump(&mut app, "CURRENT opened while the old request is delayed", opened);
    app.apply(Action::SelectRow(0));
    let (file, len, sel, nodes, label, rows) = (app.session().file().map(|p| p.to_path_buf()), app.session().file_len(), app.selection().span(), app.session().nodes().len(), app.session().file_label().to_string(), app.session().view().unwrap().rows.len());
    assert_eq!(file.as_deref(), Some(cur.as_path()));
    assert_eq!(len, 16);

    gate.release(); // the old file's page (and decode answers) arrive late
    let t = Instant::now();
    let mut clip = Vec::new();
    while t.elapsed() < Duration::from_millis(800) {
        clip.extend(app.poll());
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(clip.is_empty());
    assert_eq!(app.session().file(), Some(cur.as_path()), "late completion changed the current file");
    assert_eq!(app.session().file_len(), len);
    assert_eq!(app.selection().span(), sel, "late completion changed the selection");
    assert_eq!(app.session().nodes().len(), nodes);
    assert_eq!(app.session().file_label(), label);
    assert_eq!(app.session().view().unwrap().rows.len(), rows);
    assert_eq!(app.session().hex().page().unwrap(), &CURRENT_BYTES.to_vec(), "old file's page replaced the current one");
}

// (d) -------------------------------------------------------------------------------------
#[test]
fn zoom_ceiling_below_cap_and_zoom_in_beyond_is_noop() {
    let p = ZoomPolicy::DEFAULT;
    let small = ZoomFit { at_zoom: 1.0, avail_w: 1200.0, avail_h: 700.0, need_w: 900.0, need_h: 350.0 };
    let c = p.ceiling(small);
    assert!(c < 2.0 && c >= 1.0, "ceiling {c}");
    assert!((c - 1.3).abs() < 1e-4, "floor_step(min(1.333, 2.0, 2.0)) = 1.3, got {c}");
    // Window so small the layout does not fit even at 100 %: never below 1.0.
    assert_eq!(p.ceiling(ZoomFit { avail_w: 300.0, avail_h: 200.0, ..small }), 1.0);

    let h = Hold::new(&Gate::new(), "none", false, false);
    let mut app = app_with(&h);
    app.apply(Action::LayoutFit(small));
    assert!((app.prefs().zoom_ceiling() - c).abs() < 1e-6);
    for _ in 0..12 {
        app.apply(Action::ZoomIn);
    }
    assert!((app.prefs().zoom() - c).abs() < 1e-4, "zoom stops at the ceiling: {}", app.prefs().zoom());
    assert!(!app.can_zoom_in());
    let (z, n) = (app.prefs().zoom(), app.prefs().notice().to_string());
    app.apply(Action::ZoomIn);
    assert_eq!(app.prefs().zoom(), z, "zoom-in beyond the ceiling is a no-op");
    assert!(n.contains("layout limit"), "notice: {n}");
    // Also through the persisted-zoom path: an over-ceiling zoom is lowered by the next fit.
    app.apply(Action::SetZoom(1.9));
    app.apply(Action::LayoutFit(ZoomFit { at_zoom: 1.9, ..small }));
    assert!(app.prefs().zoom() <= app.prefs().zoom_ceiling() + 1e-4, "zoom {} above ceiling {}", app.prefs().zoom(), app.prefs().zoom_ceiling());
}
