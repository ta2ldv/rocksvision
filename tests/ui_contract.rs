//! Public-API checks for task 0002 that complement the in-crate UI state tests:
//! theme registry/contrast with an independent WCAG implementation, and the I/O job contract
//! (non-blocking requests, echoed req ids, cancel, in-flight cap, panic containment).
use eframe::egui;
use rocksvision::discover::DirEntry;
use rocksvision::jobs::{FsBackend, IoBackend, IoRequest, Outcome, Response, Worker};
use rocksvision::ui::theme::{theme_by_id, Decoration, ThemeDefinition, DEFAULT_THEME, THEMES};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

// ------------------------------------------------------------------ theme

fn lin(v: u8) -> f64 {
    let s = v as f64 / 255.0;
    if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
}
fn lum(c: egui::Color32) -> f64 {
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}
fn cr(a: egui::Color32, b: egui::Color32) -> f64 {
    let (x, y) = (lum(a), lum(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

#[test]
fn registry_invariants_and_fallback() {
    assert_eq!(THEMES.len(), 6);
    let mut ids = std::collections::HashSet::new();
    for t in THEMES {
        assert!(!t.id.is_empty() && !t.name.is_empty());
        assert!(t.id.chars().all(|c| c.is_ascii_lowercase() || c == '-'), "id {:?} not a stable slug", t.id);
        assert!(ids.insert(t.id), "duplicate id {}", t.id);
        assert_eq!(theme_by_id(t.id).id, t.id, "id does not round-trip");
        assert!(t.typography.body > 0.0 && t.typography.mono > 0.0 && t.typography.small > 0.0);
    }
    assert_eq!(THEMES.iter().filter(|t| t.decoration == Decoration::WoodGrain).count(), 1);
    assert_eq!(THEMES[0].id, DEFAULT_THEME);
    for bad in ["", " ", "AUTISTIC", " matrix", "matrix\0", "wooden ", "../x", "unknown-theme", &"x".repeat(10_000)] {
        assert_eq!(theme_by_id(bad).id, DEFAULT_THEME, "{bad:.20?} did not fall back");
    }
}

fn check(t: &ThemeDefinition, fails: &mut Vec<String>, what: &str, fg: egui::Color32, bg: egui::Color32, min: f64) {
    let r = cr(fg, bg);
    if r < min {
        fails.push(format!("{}: {what} {fg:?} on {bg:?} = {r:.2} < {min}", t.id));
    }
}

/// Independent re-computation of the required pairs (does not call theme::contrast).
#[test]
fn contrast_required_pairs_all_six_themes() {
    let mut fails = Vec::new();
    for t in THEMES {
        let p = &t.palette;
        for (n, bg) in [("canvas", p.canvas), ("panel", p.panel), ("explain", p.explain), ("toolbar", p.toolbar), ("input", p.input), ("menu", p.menu), ("data", p.data), ("stripe_a", p.stripe_a), ("stripe_b", p.stripe_b)] {
            check(t, &mut fails, &format!("text/{n}"), p.text, bg, 4.5);
        }
        for (n, bg) in [("panel", p.panel), ("explain", p.explain), ("data", p.data), ("stripe_a", p.stripe_a), ("stripe_b", p.stripe_b)] {
            check(t, &mut fails, &format!("accent_text/{n}"), p.accent_text, bg, 4.5);
            for (i, s) in p.status.all().into_iter().enumerate() {
                check(t, &mut fails, &format!("status{i}/{n}"), s, bg, 4.5);
            }
        }
        check(t, &mut fails, "header", p.header_fg, p.header_bg, 4.5);
        check(t, &mut fails, "sel", p.sel_fg, p.sel_bg, 4.5);
        check(t, &mut fails, "match", p.match_fg, p.match_bg, 4.5);
        check(t, &mut fails, "active", p.active_fg, p.active_bg, 4.5);
        for (n, bg) in [("canvas", p.canvas), ("panel", p.panel), ("data", p.data)] {
            check(t, &mut fails, &format!("border_control/{n}"), p.border_control, bg, 3.0);
        }
    }
    assert!(fails.is_empty(), "{}", fails.join("\n"));
}

/// Pairs not covered by the in-crate test: kept separate so a failure is attributable.
#[test]
fn contrast_extra_pairs() {
    let mut fails = Vec::new();
    for t in THEMES {
        let p = &t.palette;
        // hover_bg is excluded: muted/hover_bg is 3.79 (workbench) and 3.76 (wooden) <4.5; reported as an
        // observation (only matters if muted labels are drawn over a hovered widget fill).
        for (n, bg) in [("input", p.input), ("lsm_bg", p.lsm_bg)] {
            check(t, &mut fails, &format!("muted/{n}"), p.text_muted, bg, 4.5);
        }
        for (n, bg) in [("canvas", p.canvas), ("panel", p.panel), ("data", p.data), ("input", p.input)] {
            check(t, &mut fails, &format!("focus/{n}"), p.focus, bg, 3.0);
        }
        check(t, &mut fails, "sel_edge/data", p.sel_edge, p.data, 3.0);
        check(t, &mut fails, "accent/data", p.accent, p.data, 3.0);
        // wood grain varies the toolbar by up to +-8 per channel: worst-case text contrast
        if t.decoration == Decoration::WoodGrain {
            for d in [-8i16, 8] {
                let f = |v: u8| (v as i16 + d).clamp(0, 255) as u8;
                let bg = egui::Color32::from_rgb(f(p.toolbar.r()), f(p.toolbar.g()), f(p.toolbar.b()));
                check(t, &mut fails, &format!("text/toolbar grain {d:+}"), p.text, bg, 4.5);
            }
        }
    }
    assert!(fails.is_empty(), "{}", fails.join("\n"));
}

// ------------------------------------------------------------------ jobs

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

enum Mode {
    Block(Arc<Gate>),
    Panic,
}
struct Test(Mode);
impl Test {
    fn hit(&self) {
        match &self.0 {
            Mode::Block(g) => g.wait(),
            Mode::Panic => panic!("injected backend panic"),
        }
    }
}
impl IoBackend for Test {
    fn is_dir(&self, p: &Path) -> std::io::Result<bool> {
        self.hit();
        FsBackend.is_dir(p)
    }
    fn scan(&self, d: &Path) -> std::io::Result<(Vec<DirEntry>, bool)> {
        self.hit();
        FsBackend.scan(d)
    }
    fn read(&self, p: &Path, o: u64, l: u64) -> std::io::Result<(Vec<u8>, u64)> {
        self.hit();
        FsBackend.read(p, o, l)
    }
    fn stat(&self, p: &Path) -> std::io::Result<(u64, Option<SystemTime>)> {
        self.hit();
        FsBackend.stat(p)
    }
}

fn tmpfile(name: &str, bytes: &[u8]) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rocksvision-uic-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

fn recv(w: &Worker, secs: u64) -> Option<Response> {
    w.rx.recv_timeout(Duration::from_secs(secs)).ok().map(|t| t.msg)
}

#[test]
fn io_requests_return_immediately_and_echo_req_when_blocked() {
    let gate = Gate::new();
    let w = Worker::spawn_with(Arc::new(|| {}), Arc::new(Test(Mode::Block(gate.clone()))));
    let p = tmpfile("a.bin", &[1, 2, 3, 4]);
    let t = Instant::now();
    let r1 = w.io(1, IoRequest::Page { path: p.clone(), off: 0, len: 4 });
    let r2 = w.io(1, IoRequest::Scan(p.parent().unwrap().to_path_buf()));
    let r3 = w.io(1, IoRequest::Resolve(p.clone()));
    assert!(t.elapsed() < Duration::from_millis(100), "io() blocked for {:?}", t.elapsed());
    assert!(r1 < r2 && r2 < r3, "job ids must increase");
    assert!(w.io_busy());
    gate.release();
    let mut seen = Vec::new();
    for _ in 0..3 {
        match recv(&w, 5).expect("response") {
            Response::Page { req, outcome: Outcome::Done((d, len)), .. } => {
                assert_eq!((d, len), (vec![1, 2, 3, 4], 4));
                seen.push(req)
            }
            Response::Scanned { req, outcome: Outcome::Done(_), .. } => seen.push(req),
            Response::Resolved { req, outcome: Outcome::Done(false), .. } => seen.push(req),
            _ => panic!("unexpected response"),
        }
    }
    seen.sort();
    assert_eq!(seen, vec![r1, r2, r3]);
}

#[test]
fn cancel_all_marks_blocked_jobs_cancelled_and_later_jobs_run() {
    let gate = Gate::new();
    let w = Worker::spawn_with(Arc::new(|| {}), Arc::new(Test(Mode::Block(gate.clone()))));
    let p = tmpfile("b.bin", &[9u8; 16]);
    let old = w.io(1, IoRequest::Page { path: p.clone(), off: 0, len: 16 });
    w.cancel_all();
    let new = w.io(1, IoRequest::Page { path: p.clone(), off: 0, len: 16 });
    gate.release();
    let mut got = std::collections::HashMap::new();
    for _ in 0..2 {
        if let Response::Page { req, outcome, .. } = recv(&w, 5).expect("response") {
            got.insert(req, outcome);
        }
    }
    assert_eq!(got[&old], Outcome::Cancelled);
    assert!(matches!(got[&new], Outcome::Done(_)), "request after cancel_all was cancelled too");
}

#[test]
fn inflight_cap_fails_fast_and_recovers_after_release() {
    let gate = Gate::new();
    let w = Worker::spawn_with(Arc::new(|| {}), Arc::new(Test(Mode::Block(gate.clone()))));
    let p = tmpfile("c.bin", &[5u8; 8]);
    let cap = rocksvision::ui::theme::LIMITS.max_io_inflight;
    for _ in 0..cap {
        w.io(1, IoRequest::Page { path: p.clone(), off: 0, len: 8 });
    }
    let extra = w.io(1, IoRequest::Page { path: p.clone(), off: 0, len: 8 });
    match recv(&w, 2).expect("refusal is immediate") {
        Response::Page { req, outcome: Outcome::Failed(m), .. } => {
            assert_eq!(req, extra);
            assert!(m.contains("try again"), "{m}");
        }
        _ => panic!("expected an immediate Failed page for request over the cap"),
    }
    gate.release();
    for _ in 0..cap {
        assert!(matches!(recv(&w, 5), Some(Response::Page { outcome: Outcome::Done(_), .. })));
    }
    let t = Instant::now();
    while w.io_busy() {
        assert!(t.elapsed() < Duration::from_secs(5), "io_inflight did not return to 0");
        std::thread::sleep(Duration::from_millis(2));
    }
    let again = w.io(1, IoRequest::Page { path: p, off: 0, len: 8 });
    assert!(matches!(recv(&w, 5), Some(Response::Page { req, outcome: Outcome::Done(_), .. }) if req == again));
}

/// A panic inside a backend call must still answer the SAME response kind with the request's id,
/// otherwise the UI (which matches on req) keeps its Loading/Scanning state forever.
#[test]
fn backend_panic_answers_with_matching_response_kind() {
    let w = Worker::spawn_with(Arc::new(|| {}), Arc::new(Test(Mode::Panic)));
    let p = tmpfile("d.bin", &[1u8; 8]);
    let req = w.io(1, IoRequest::Page { path: p, off: 0, len: 8 });
    match recv(&w, 5).expect("a response must be sent after a panic") {
        Response::Page { req: r, outcome: Outcome::Failed(_), .. } => assert_eq!(r, req),
        Response::Stat { req: r, .. } => panic!("panic in a Page job was reported as Response::Stat (req {r}); the hex page would stay Loading"),
        _ => panic!("unexpected response kind"),
    }
}

// ------------------------------------------------------------------ manual responsiveness helpers
// RV_WAL=<path> cargo test --release --test ui_contract -- --ignored rv_gen_wal   (512 MiB WAL, needle ZZZ at the end)
mod common;

#[test]
#[ignore = "manual helper: generates a 512 MiB synthetic WAL at $RV_WAL"]
fn rv_gen_wal() {
    use std::io::Write;
    let p = std::env::var("RV_WAL").expect("RV_WAL");
    let blocks = 16384u64;
    let mut w = std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(&p).unwrap());
    for b in 0..blocks {
        let mut pl = Vec::new();
        pl.extend_from_slice(&(b + 1).to_le_bytes());
        pl.extend_from_slice(&1u32.to_le_bytes());
        pl.extend_from_slice(&[1, 3]);
        pl.extend_from_slice(if b == blocks - 1 { b"ZZZ" } else { b"key" });
        pl.push(2);
        pl.extend_from_slice(b"vv");
        let rec = common::full_record(&pl);
        let mut blk = vec![0u8; 32768];
        blk[..rec.len()].copy_from_slice(&rec);
        w.write_all(&blk).unwrap();
    }
}

#[test]
#[ignore = "manual helper: times search_file on $RV_WAL"]
fn rv_time_search() {
    let p = PathBuf::from(std::env::var("RV_WAL").expect("RV_WAL"));
    let t = Instant::now();
    let hit = rocksvision::jobs::search_file(&p, b"ZZZ\x02vv", 0, false, &|| false, &|_, _| {});
    eprintln!("RV-SEARCH-END-NEEDLE {:?} -> {hit:?}", t.elapsed());
    assert!(matches!(hit, Some(Some(_))));
}
