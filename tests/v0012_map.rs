//! 0.1.4 task 0012 (handoff 0026): WAL band inventory/summaries, map texts, conditional
//! navigation on the model. Public API only. Scratch-folder tests are `#[ignore]` (they need
//! `bash tests/tools/mk-wal-fixtures-0026.sh`; run with `--ignored`) so a plain `cargo test`
//! never fails on a missing /private/tmp folder, and never silently skips either.
mod common;
use rocksvision::app::explanation::lsm_file_card;
use rocksvision::app::lsm_map::{MapMeasure, NavState, axis_ends, axis_middle, empty_lane_text, lane_caption};
use rocksvision::app::map_facts::{band_lifecycle, level_facts, sst_facts, wal_facts, wal_state};
use rocksvision::app::{Action, AppState, MapMode, MapZoom};
use rocksvision::jobs::{FsBackend, Worker};
use rocksvision::limits::{WAL_SUMMARY_BYTES, WAL_SUMMARY_FILES};
use rocksvision::model::{NodeKind, NotScannedWhy, WalLifecycle, WalSummary};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const SCRATCH: &str = "/private/tmp/rocksvision-wal0026";

fn new_app() -> AppState {
    AppState::new(Worker::spawn_with(Arc::new(|| {}), Arc::new(FsBackend)))
}

fn repo(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn scratch(name: &str) -> PathBuf {
    let p = Path::new(SCRATCH).join(name);
    assert!(p.is_dir(), "missing {} (run bash tests/tools/mk-wal-fixtures-0026.sh)", p.display());
    p
}

fn pump(app: &mut AppState, what: &str, done: impl Fn(&AppState) -> bool) {
    let t = Instant::now();
    let mut ok = 0;
    loop {
        app.poll();
        let idle = app.busy_label().is_none() && app.session().pending_view().is_none();
        ok = if idle && done(app) { ok + 1 } else { 0 };
        if ok >= 3 {
            return;
        }
        assert!(t.elapsed() < Duration::from_secs(60), "timeout waiting for: {what} (status {:?})", app.session().status_msg());
        std::thread::sleep(Duration::from_millis(3));
    }
}

/// Folder opened at CURRENT, the levels view shown, every WAL summary finished.
fn levels_with_wals(dir: &Path) -> AppState {
    let mut app = new_app();
    app.apply(Action::OpenPath(dir.to_path_buf()));
    pump(&mut app, "CURRENT opened", |a| a.session().file().and_then(|p| p.file_name()).is_some_and(|n| n == "CURRENT") && a.session().view().is_some());
    app.apply(Action::Navigate { target: NodeKind::ManifestLevels, span: None });
    pump(&mut app, "levels", |a| a.session().view_kind() == Some(&NodeKind::ManifestLevels) && a.session().view().is_some());
    pump(&mut app, "wal summaries", |a| !a.session().wal_scan_pending() && a.session().wal_inventory().is_some_and(|w| w.files.iter().all(|f| f.summary != WalSummary::Scanning)));
    app
}

// ---------------------------------------------------------------- WAL inventory states

#[test]
#[ignore = "needs bash tests/tools/mk-wal-fixtures-0026.sh (multi-cf-done)"]
fn multi_cf_wal_is_done_with_three_cfs_and_may_hold_unflushed_for_them() {
    let app = levels_with_wals(&scratch("multi-cf-done"));
    let w = app.session().wal_inventory().expect("inventory");
    assert_eq!(w.files.len(), 1);
    let f = &w.files[0];
    let WalSummary::Done(s) = &f.summary else { panic!("expected Done, got {:?}", f.summary) };
    assert_eq!(s.cfs.iter().copied().collect::<Vec<_>>(), vec![0, 1, 2]);
    let ln = &app.session().view().unwrap().lsm.log_numbers;
    match WalLifecycle::of(f.number, ln) {
        WalLifecycle::MayHoldUnflushed { cfs } => assert_eq!(cfs, vec![0, 1, 2]),
        other => panic!("lifecycle {other:?} per_cf {:?} active {}", ln.per_cf, ln.active),
    }
}

#[test]
#[ignore = "needs bash tests/tools/mk-wal-fixtures-0026.sh (no-wal)"]
fn folder_without_wal_has_an_empty_inventory_not_an_absent_one() {
    let app = levels_with_wals(&scratch("no-wal"));
    // The painter shows "No WAL files found in this folder." for Some(empty); None means "not listed".
    let w = app.session().wal_inventory().expect("folder was scanned: Some(empty), not None");
    assert!(w.files.is_empty() && !w.truncated);
}

#[test]
#[ignore = "needs bash tests/tools/mk-wal-fixtures-0026.sh (corrupt-wal)"]
fn corrupt_wal_is_corrupt_with_the_offset_in_the_state_line() {
    let app = levels_with_wals(&scratch("corrupt-wal"));
    let f = &app.session().wal_inventory().unwrap().files[0];
    let WalSummary::Corrupt { at, .. } = &f.summary else { panic!("expected Corrupt, got {:?}", f.summary) };
    assert!(*at <= 54, "corruption at byte 54 is reported at or before it, got {at}");
    let st = wal_state(f);
    assert_eq!(st.kind, rocksvision::app::map_facts::WalStateKind::Corrupt);
    assert!(st.text.starts_with("✖ corrupt") && st.text.contains(&format!("0x{at:X}")), "{}", st.text);
}

#[test]
#[ignore = "needs bash tests/tools/mk-wal-fixtures-0026.sh (big-wal)"]
fn wal_over_the_byte_limit_is_partial_and_membership_is_unknown() {
    let dir = scratch("big-wal");
    assert!(std::fs::metadata(dir.join("000023.log")).unwrap().len() > WAL_SUMMARY_BYTES);
    let app = levels_with_wals(&dir);
    let f = &app.session().wal_inventory().unwrap().files[0];
    assert!(matches!(&f.summary, WalSummary::Partial { .. }), "{:?}", f.summary);
    let st = wal_state(f);
    assert!(st.text.starts_with("≈ partial: first "), "{}", st.text);
    use rocksvision::model::CfMembership;
    assert_eq!(f.summary.membership(1), CfMembership::Unknown, "an unseen CF in a partial scan is unknown, never 'none seen'");
}

#[test]
#[ignore = "needs bash tests/tools/mk-wal-fixtures-0026.sh (many-wal)"]
fn many_wals_keep_every_file_listed_and_scan_only_the_summary_cap() {
    let app = levels_with_wals(&scratch("many-wal"));
    let w = app.session().wal_inventory().unwrap();
    assert_eq!(w.files.len(), 41, "no WAL dropped from the inventory");
    let scanned = w.files.iter().filter(|f| !matches!(f.summary, WalSummary::NotScanned(_))).count();
    assert_eq!(scanned, WAL_SUMMARY_FILES, "exactly the cap is scanned");
    let beyond: Vec<_> = w.files.iter().filter(|f| matches!(f.summary, WalSummary::NotScanned(NotScannedWhy::BeyondCap))).collect();
    assert_eq!(beyond.len(), 41 - WAL_SUMMARY_FILES);
    let max_beyond = beyond.iter().filter_map(|f| f.number).max().unwrap();
    let min_scanned = w.files.iter().filter(|f| !matches!(f.summary, WalSummary::NotScanned(_))).filter_map(|f| f.number).min().unwrap();
    assert!(max_beyond < min_scanned, "the highest numbers are scanned first");
}

#[test]
fn recycled_wal_folder_lists_both_logs_and_ends_corrupt_at_the_recycled_tail() {
    let app = levels_with_wals(&repo("examples/data/wal-recycled"));
    let w = app.session().wal_inventory().unwrap();
    assert_eq!(w.files.iter().map(|f| f.number).collect::<Vec<_>>(), vec![Some(11), Some(14)]);
    assert!(matches!(&w.files[1].summary, WalSummary::Corrupt { at: 0x56E, recycled: true, .. }), "{:?}", w.files[1].summary);
    assert_eq!(wal_state(&w.files[1]).text, "✖ corrupt or recycled leftover at 0x56E");
}

// ---------------------------------------------------------------- forbidden text rule

const FORBIDDEN: [&str; 6] = ["memtable", "missing keys", "no data", "lacks", "u-009", "obsolete"];

fn assert_clean(what: &str, text: &str) {
    let low = text.to_lowercase();
    for bad in FORBIDDEN {
        assert!(!low.contains(bad), "{what}: forbidden {bad:?} in {text:?}");
    }
    if low.contains("unflushed") {
        assert!(low.contains("may") && low.contains("per manifest") || low.contains("no unflushed data per manifest"), "{what}: unflushed without may/per MANIFEST: {text:?}");
    }
}

#[test]
fn multilevel_db_cf_users_texts_obey_the_text_rules() {
    let app = levels_with_wals(&repo("examples/data/multilevel-db"));
    let inv = &app.session().view().unwrap().lsm;
    let cf = inv.cfs.iter().find(|c| c.name == "cf_users").expect("cf_users");
    let mut n = 0;
    for (a, b) in axis_ends(cf).into_iter() {
        assert_eq!((a.as_str(), b.as_str()), ("\"u-000\"", "\"u-024\""));
    }
    assert_eq!(axis_middle(&cf.order), "key rank, not to scale ->");
    assert!(lane_caption(cf).is_some_and(|c| c.starts_with("Stacked lanes: overlapping L0 files.")));
    for (li, l) in cf.levels.iter().enumerate() {
        for f in &l.files {
            for fact in sst_facts(cf, l, f) {
                assert_clean(&format!("sst {} {}", f.number, fact.label), &fact.value);
                n += 1;
            }
        }
        for fact in level_facts(cf, l) {
            assert_clean(&format!("level L{} {}", l.level, fact.label), &fact.value);
            n += 1;
        }
        for lane in 0..l.lanes {
            for step in 0..=20 {
                if let Some(t) = empty_lane_text(cf, li, lane, step as f32 / 20.0) {
                    assert_clean(&format!("empty L{} lane {lane} x{step}", l.level), &t);
                    n += 1;
                }
            }
        }
    }
    for row in 0..inv.iter().count() {
        let card = lsm_file_card(app.session(), row).expect("card");
        assert_clean("legend", &card.legend);
        assert_clean("open", &card.open);
        for (l, v) in &card.facts {
            assert_clean(l, v);
            n += 1;
        }
    }
    let w = app.session().wal_inventory().unwrap();
    for f in &w.files {
        for fact in wal_facts(f, inv, Some(cf.id)) {
            assert_clean(&format!("wal {}", fact.label), &fact.value);
            n += 1;
        }
        assert_clean("wal state", &wal_state(f).text);
    }
    if let Some(t) = band_lifecycle(&w.files, inv, Some(cf.id)) {
        assert_clean("band", &t);
    }
    assert!(n > 40, "the walk really covered texts ({n})");
}

#[test]
fn the_gap_left_of_000018_names_000016_and_makes_no_claim_about_keys() {
    let app = levels_with_wals(&repo("examples/data/multilevel-db"));
    let inv = &app.session().view().unwrap().lsm;
    let cf = inv.cfs.iter().find(|c| c.name == "cf_users").unwrap();
    let l0 = cf.levels.iter().position(|l| l.level == 0).expect("L0");
    assert_eq!(cf.levels[l0].lanes, 2);
    let f18 = cf.levels[l0].files.iter().find(|f| f.number == 18).expect("000018");
    let s = cf.levels[l0].scale.max(1) as f32;
    // A point left of 000018's range, in the lane 000018 is NOT in.
    let x = (f18.slots.0 as f32 / s) * 0.5;
    let other_lane = 1 - f18.lane;
    let t = empty_lane_text(cf, l0, other_lane, x).expect("text for an ordered comparator");
    assert!(t.contains(&format!("L0 lane {}: no file in this lane here.", other_lane + 1)), "{t}");
    assert!(t.contains("000016") && t.contains("Lanes only stack overlapping L0 files."), "{t}");
    assert!(!t.contains("u-"), "no key at all in the gap text: {t}");
}

// ---------------------------------------------------------------- Center vs Fit, hysteresis

fn measure(scene_h: f32, view_h: f32, narrowest: f32) -> MapMeasure {
    MapMeasure { scene_h, view_h, view_w: 800.0, narrowest, readable_w: 56.0, lane_h: 20.0, merged: false, cf: Some(1), files: 4, app_zoom: 1.0 }
}

#[test]
fn nav_state_hysteresis_needs_a_margin_to_leave_navigation() {
    let tight_h = measure(190.0, 200.0, 100.0); // 10 pt of spare height < one lane (20)
    assert_eq!(tight_h.nav(NavState::Overflow), NavState::Overflow, "needs one lane of spare height to leave");
    assert_eq!(tight_h.nav(NavState::Fits), NavState::Fits, "but it stays Fits while it fits");
    let tight_w = measure(100.0, 200.0, 56.0 + 10.0); // 10 pt of spare width < 24
    assert_eq!(tight_w.nav(NavState::Overflow), NavState::Overflow);
    assert_eq!(tight_w.nav(NavState::Fits), NavState::Fits);
    let roomy = measure(100.0, 200.0, 100.0);
    assert_eq!(roomy.nav(NavState::Overflow), NavState::Fits);
    let over = measure(210.0, 200.0, 100.0);
    assert_eq!(over.nav(NavState::Fits), NavState::Overflow);
    let unreadable = measure(100.0, 200.0, 40.0);
    assert_eq!(unreadable.nav(NavState::Fits), NavState::Overflow);
    // Decisions only after more than 2 pt, or when the CF changes.
    let mut a = roomy;
    a.view_h += 1.5;
    assert!(!a.decides_after(&roomy), "1.5 pt is inside the dead band");
    a.view_h = roomy.view_h + 3.0;
    assert!(a.decides_after(&roomy));
    let mut c = roomy;
    c.cf = Some(2);
    assert!(c.decides_after(&roomy));
}

#[test]
fn center_keeps_zoom_fit_resets_and_both_are_ignored_while_the_map_fits() {
    let mut app = levels_with_wals(&repo("examples/data/multilevel-db"));
    app.apply(Action::SetMapMode(MapMode::Map));
    // Overflow: 3x zoom, pan elsewhere, then Center.
    app.apply(Action::MapMeasure(measure(300.0, 200.0, 100.0)));
    assert_eq!(app.session().map().nav(), NavState::Overflow);
    app.apply(Action::MapView { pan: (0.0, 0.0), zoom: 3.0 });
    app.apply(Action::MapZoom(MapZoom::Center));
    let (z, p) = (app.session().map().zoom(), app.session().map().pan());
    assert!((z - 3.0).abs() < 1e-6, "Center keeps the zoom, got {z}");
    assert!((p.0 - (0.5 - 0.5 / 3.0)).abs() < 1e-4, "pan.x centred: {p:?}");
    app.apply(Action::MapZoom(MapZoom::Fit));
    assert_eq!((app.session().map().zoom(), app.session().map().pan()), (1.0, (0.0, 0.0)));
    // A user Fit never switches the mode.
    assert_eq!(app.session().map().nav(), NavState::Overflow);
    // The scene now fits with margin: stationary at x1.
    app.apply(Action::MapMeasure(measure(100.0, 400.0, 100.0)));
    assert_eq!(app.session().map().nav(), NavState::Fits);
    for z in [MapZoom::In, MapZoom::Out, MapZoom::Center] {
        app.apply(Action::MapZoom(z));
        assert_eq!((app.session().map().zoom(), app.session().map().pan()), (1.0, (0.0, 0.0)), "{z:?} is ignored in Fits");
    }
}
