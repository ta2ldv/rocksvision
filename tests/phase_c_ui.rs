//! Phase C (UI) contract tests through the PUBLIC API only (std + rocksvision, no GUI, fixtures are only read):
//! hex bytes-per-row policy, zoom ceiling vs panel visibility, Reset View contract, diagnostic data on corrupt fixtures.
//! Not covered here: search Enter one-dispatch - the pure key mapping (`ui::keys::map`) is a private module, so only
//! the in-crate unit tests and the GUI script tests/tools/scripts/search_enter.txt can check it.
mod common;
use common::fx;
use rocksvision::app::{Action, AppState, ZoomFit, ZoomPolicy, CENTER_SPLIT_DEFAULT};
use rocksvision::inspect;
use rocksvision::jobs::{FsBackend, Worker};
use rocksvision::model::{CheckKind, DecodedValue, Diagnostic, NodeKind, Span, Status};
use rocksvision::ui::layout::{choose_bytes_per_row, hex_row_chars, Measured, HexMetrics, LAYOUT};
use rocksvision::ui::theme::theme_by_id;
use std::sync::Arc;
use std::time::{Duration, Instant};

// ------------------------------------------------------------------ hex bytes per row

fn metrics(cw: f32, pad: f32, digits: u64) -> HexMetrics {
    HexMetrics { cw, pad, digits }
}

#[test]
fn bytes_per_row_never_grows_when_the_width_shrinks_at_fixed_zoom() {
    // Fixed zoom = fixed glyph advance. Several glyph sizes / offset-digit counts.
    for (cw, digits) in [(6.0f32, 8u64), (7.2, 8), (8.4, 9), (7.2, 12), (10.0, 16)] {
        let m = metrics(cw, 14.0, digits);
        // Stateless monotonicity from every starting state.
        for cur in [4u64, 8, 16] {
            let mut prev = u64::MAX;
            let mut w = 1600.0f32;
            while w >= 0.0 {
                let n = choose_bytes_per_row(cur, w, &m);
                assert!(matches!(n, 4 | 8 | 16), "cw {cw}: n={n} not in 4/8/16");
                assert!(n <= prev, "cw {cw} d {digits} cur {cur}: {n} bytes at {w} > {prev} at a wider width");
                prev = n;
                w -= 1.0;
            }
        }
        // Resizing narrower step by step with the previous result carried as state (what the UI does).
        let mut n = 16u64;
        let mut w = 1600.0f32;
        let mut seq = vec![];
        while w >= 0.0 {
            let next = choose_bytes_per_row(n, w, &m);
            assert!(next <= n, "cw {cw}: carried state grew {n} -> {next} at {w}");
            n = next;
            seq.push(n);
            w -= 3.0;
        }
        assert_eq!(*seq.last().unwrap(), 4, "floor is 4 bytes per row");
        // Rows really fit whenever the policy says n > 4.
        for &avail in &[300.0f32, 500.0, 700.0, 900.0, 1200.0] {
            let n = choose_bytes_per_row(16, avail, &m);
            assert!(n == 4 || m.width(n) <= avail, "cw {cw}: n={n} width {} > avail {avail}", m.width(n));
        }
    }
}

#[test]
fn bytes_per_row_floor_is_four_and_invalid_state_is_repaired() {
    let m = metrics(7.2, 14.0, 8);
    for avail in [-10.0f32, 0.0, 1.0, 50.0] {
        assert_eq!(choose_bytes_per_row(16, avail, &m), 4, "avail {avail}");
        assert_eq!(choose_bytes_per_row(4, avail, &m), 4);
    }
    // A state outside {4,8,16} is treated as 16 (then reduced to fit).
    assert_eq!(choose_bytes_per_row(5, 0.0, &m), 4);
    assert_eq!(choose_bytes_per_row(0, 100000.0, &m), 16);
    // Row width formula: strictly increasing in n, ASCII header keeps 4-byte rows >= 5 ascii glyphs wide.
    assert!(hex_row_chars(4, 8) < hex_row_chars(8, 8) && hex_row_chars(8, 8) < hex_row_chars(16, 8));
    assert_eq!(hex_row_chars(4, 8), 8 + 2 + 12 + 0 + 1 + 5);
}

// ------------------------------------------------------------------ zoom ceiling vs panel visibility

fn measured() -> Measured {
    Measured {
        nav_min: LAYOUT.left_min,
        explain_min: LAYOUT.right_min,
        strip: 22.0,
        hex: metrics(7.2, 14.0, 8),
        toolbar_groups: vec![180.0, 150.0, 120.0, 220.0, 170.0],
        toolbar_gap: 6.0,
        toolbar_row_h: 28.0,
        toolbar_pad_h: 9.0,
        status_h: 22.0,
        records_min_h: 90.0,
        hex_groups: [160.0, 140.0, 170.0],
        hex_gap: 6.0,
        hex_line_h: 22.0,
        hex_ruler_h: 16.0,
        hex_row_h: 16.0,
        hex_pad_h: 18.0,
    }
}

#[test]
fn zoom_ceiling_follows_the_panels_the_layout_actually_shows() {
    // Behaviour of the code at FREEZE B (ui/mod.rs: measured.need(show_left, show_right)): hiding a panel lowers the
    // minimum width the layout needs, so the ceiling for the SAME window can only rise; showing both panels is the
    // strictest case. (A rule "never compute the ceiling with a hidden panel" would instead pass (true, true) always;
    // this test documents the actual rule so a change in either direction is noticed.)
    let m = measured();
    let both = m.need(true, true);
    let no_nav = m.need(false, true);
    let no_expl = m.need(true, false);
    let none = m.need(false, false);
    assert!(both.0 > no_nav.0 && both.0 > no_expl.0, "{both:?} {no_nav:?} {no_expl:?}");
    assert!(no_nav.0 > none.0 && no_expl.0 > none.0);
    // (The height need is NOT monotone in the panels: a narrower layout wraps the toolbar/hex header into more rows.)
    let p = ZoomPolicy::DEFAULT;
    for (w, h) in [(900.0f32, 560.0f32), (1100.0, 800.0), (1400.0, 900.0), (2000.0, 1200.0)] {
        let c = |need: (f32, f32)| p.ceiling(ZoomFit { at_zoom: 1.0, avail_w: w, avail_h: h, need_w: need.0, need_h: need.1 });
        let (cb, cn, cx) = (c(both), c(no_nav), c(none));
        // No ordering between cb/cn/cx is asserted: with a hidden panel the layout is narrower, wraps into more toolbar
        // rows and needs MORE height, so the ceiling can be LOWER than with both panels (900x560: 1.4 / 1.4 / 1.2 with these
        // measurements). Toggling a panel can therefore change the allowed zoom in either direction.
        let _ = (cn, cx);
        assert!(cb >= 1.0 - 1e-6 && cb <= p.max + 1e-6, "{w}x{h}: ceiling {cb}");
        // With both panels visible the layout fits at the ceiling (acceptance rule of the policy).
        assert!(m.fits(w / cb, h / cb, true, true), "{w}x{h}: ceiling {cb} does not fit with both panels shown");
    }
    // Pure policy: an over-ceiling zoom is lowered by the next fit, so restoring a hidden panel cannot leave a clipped layout.
    let mut app = AppState::new(Worker::spawn_with(Arc::new(|| {}), Arc::new(FsBackend)));
    let fit_none = ZoomFit { at_zoom: 1.0, avail_w: 1100.0, avail_h: 800.0, need_w: none.0, need_h: none.1 };
    app.apply(Action::LayoutFit(fit_none));
    app.apply(Action::SetZoom(1.9));
    app.apply(Action::LayoutFit(ZoomFit { at_zoom: 1.9, ..fit_none }));
    let z_hidden = app.prefs().zoom();
    let fit_both = ZoomFit { at_zoom: z_hidden, avail_w: 1100.0 * z_hidden, avail_h: 800.0 * z_hidden, need_w: both.0, need_h: both.1 };
    app.apply(Action::LayoutFit(fit_both));
    assert!(app.prefs().zoom() <= z_hidden + 1e-6 && app.prefs().zoom() <= app.prefs().zoom_ceiling() + 1e-4);
}

// ------------------------------------------------------------------ Reset View contract

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

#[test]
fn reset_view_restores_layout_and_keeps_file_selection_search_and_theme() {
    let mut app = AppState::new(Worker::spawn_with(Arc::new(|| {}), Arc::new(FsBackend)));
    let cur = fx("db-levels/CURRENT");
    app.apply(Action::OpenFile(cur.clone()));
    pump(&mut app, "opened", |a| a.session().view().is_some() && a.session().pending_view().is_none() && a.session().hex().page().is_some());
    assert!(app.prefs().is_default_layout(), "fresh state is the default layout");
    app.apply(Action::LayoutFit(ZoomFit { at_zoom: 1.0, avail_w: 1600.0, avail_h: 1000.0, need_w: 900.0, need_h: 400.0 }));

    app.apply(Action::SetTheme(theme_by_id("matrix").id));
    app.apply(Action::SelectRow(0));
    app.apply(Action::SetSearch("MANIFEST".into()));
    app.apply(Action::SetCenterSplit(0.7));
    app.apply(Action::SetShowLeft(false));
    app.apply(Action::SetShowRight(false));
    app.apply(Action::SetShowHex(false));
    app.apply(Action::ZoomIn);
    app.apply(Action::ZoomIn);
    app.apply(Action::ZoomIn);
    assert!((app.prefs().zoom() - 1.3).abs() < 1e-4, "zoom {}", app.prefs().zoom());
    assert!(!app.prefs().is_default_layout());
    let file = app.session().file().map(|p| p.to_path_buf());
    let (sel, row, q, matches, rows) = (app.selection().span(), app.selection().row(), app.search().query().to_string(), app.search().row_matches().to_vec(), app.session().view().unwrap().rows.len());
    assert!(sel.is_some() && row == Some(0) && !matches.is_empty(), "precondition");
    let epoch = app.prefs().layout_epoch();

    app.apply(Action::ResetView);

    let p = app.prefs();
    assert_eq!(p.layout_epoch(), epoch.wrapping_add(1), "layout epoch bumps once (side-panel widths are reset by it)");
    assert_eq!(p.zoom(), 1.0);
    assert_eq!(p.center_split(), CENTER_SPLIT_DEFAULT);
    assert!(p.show_left() && p.show_right() && p.show_hex(), "all regions shown again");
    assert!(p.is_default_layout());
    assert!(p.notice().starts_with("View reset"), "notice: {}", p.notice());
    // Data state untouched.
    assert_eq!(app.session().file().map(|p| p.to_path_buf()), file);
    assert_eq!(app.selection().span(), sel);
    assert_eq!(app.selection().row(), row);
    assert_eq!(app.search().query(), q);
    assert_eq!(app.search().row_matches(), &matches[..]);
    assert_eq!(app.prefs().theme_id(), "matrix");
    assert_eq!(app.session().view().unwrap().rows.len(), rows);
}

// ------------------------------------------------------------------ diagnostic data on corrupt fixtures

/// All diagnostics reachable in the lazy tree (node diags, view diagnostics, field diags), bounded.
fn diagnostics(rel: &str) -> Vec<Diagnostic> {
    let mut i = inspect::open(&fx(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
    let never = || false;
    let mut out = Vec::new();
    let mut stack: Vec<NodeKind> = vec![NodeKind::Root];
    let mut visited = 0;
    while let Some(k) = stack.pop() {
        visited += 1;
        if visited > 600 {
            break;
        }
        let v = i.view(&k, &never);
        out.extend(v.diagnostics.iter().cloned());
        for r in &v.rows {
            if let Some(d) = &r.field.diag {
                out.push((**d).clone());
            }
        }
        for c in i.children(&k, &never) {
            if let Some(d) = &c.diag {
                out.push((**d).clone());
            }
            if !c.leaf {
                stack.push(c.kind);
            }
        }
    }
    out
}

fn typed(d: &Diagnostic) -> bool {
    d.span.is_some() && d.expected.is_some() && d.observed.is_some()
}

#[test]
fn sst_checksum_flip_carries_check_span_expected_and_observed() {
    let ds = diagnostics("corrupt/sst-bitflip-data-block.sst");
    assert!(!ds.is_empty(), "no diagnostics reachable");
    if std::env::var_os("SHOW_DIAG").is_some() {
        for d in &ds {
            eprintln!("DIAG {:?} span={:?} exp={:?} obs={:?} trust={:?} note={}", d.check, d.span, d.expected, d.observed, d.trusted, d.note);
        }
    }
    let d = ds.iter().find(|d| matches!(d.check, CheckKind::Crc32c | CheckKind::XxHash | CheckKind::XxHash64 | CheckKind::Xxh3)).unwrap_or_else(|| panic!("no checksum diagnostic: {:?}", ds.iter().map(|d| d.check).collect::<Vec<_>>()));
    assert!(typed(d), "checksum diagnostic lacks span/expected/observed: {d:?}");
    // The span is the checksummed byte range (data block 0: offset 0, contents + compression-type byte), in file space.
    let s: Span = d.span.unwrap();
    assert_eq!((s.space, s.offset), (rocksvision::model::Space::File, 0), "{s}");
    assert!(s.len > 8, "span covers the block, not just the checksum field: {s}");
    assert_ne!(d.expected, d.observed, "stored and computed checksums must differ");
    assert!(matches!(d.expected, Some(DecodedValue::U32(_))) && matches!(d.observed, Some(DecodedValue::U32(_))), "{:?} {:?}", d.expected, d.observed);
    assert_eq!(d.trusted, rocksvision::model::Trust::Untrusted, "content of a checksum-failed block is untrusted");
}

#[test]
fn sst_bad_footer_magic_carries_expected_and_observed_magic() {
    let ds = diagnostics("corrupt/sst-bad-footer-magic.sst");
    let d = ds.iter().find(|d| d.check == CheckKind::Magic).unwrap_or_else(|| panic!("no Magic diagnostic: {:?}", ds.iter().map(|d| d.check).collect::<Vec<_>>()));
    assert!(typed(d), "{d:?}");
    assert_ne!(d.expected, d.observed);
    assert_eq!(d.span.unwrap().len, 8, "magic is 8 bytes");
}

#[test]
fn wal_truncated_record_carries_truncation_span() {
    let ds = diagnostics("corrupt/wal-truncated-mid-record.log");
    let d = ds.iter().find(|d| d.check == CheckKind::Truncation).unwrap_or_else(|| panic!("no Truncation diagnostic: {:?}", ds.iter().map(|d| d.check).collect::<Vec<_>>()));
    let s = d.span.expect("truncation span");
    assert_eq!((s.offset, s.len), (59, 17), "truncated record starts at 59 (tests/fixtures/MANIFEST.md, record index 2), 17 bytes present: {s}");
    assert_eq!(d.trusted, rocksvision::model::Trust::Partial);
    assert!(!d.note.is_empty() && !d.recovery.is_empty());
}

/// The truncation diagnostic states the declared vs available length (gap closed at FREEZE C).
#[test]
fn wal_truncated_record_states_declared_vs_available_length() {
    let ds = diagnostics("corrupt/wal-truncated-mid-record.log");
    let d = ds.iter().find(|d| d.check == CheckKind::Truncation).unwrap();
    assert!(d.expected.is_some() && d.observed.is_some(), "{d:?}");
}

#[test]
fn valid_fixtures_have_no_error_diagnostics() {
    for rel in ["sst-none/data.sst", "wal-basic/000004.log", "db-levels/MANIFEST-000005"] {
        let ds = diagnostics(rel);
        assert!(ds.is_empty(), "{rel}: unexpected diagnostics {ds:?}");
    }
    let _ = Status::Present;
}
