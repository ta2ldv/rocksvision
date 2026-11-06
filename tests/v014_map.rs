//! 0.1.4 LSM Map, availability, cross-file Back and tooltip preference (task 0011 / handoff 0024,
//! phase 2a): public API only (`rocksvision::app`, `rocksvision::model`), committed fixtures
//! read-only, every modified layout is a scratch copy under the platform temp dir
//! (`std::env::temp_dir()`, `rocksvision-v014map-<pid>-<tag>`), removed on drop.
mod common;
use common::fx;
use rocksvision::app::explanation::{levels_summary, lsm_file_card, lsm_map_unavailable};
use rocksvision::app::lsm_map::{MapEntry, ZOOM_CAP, ZOOM_MIN};
use rocksvision::app::prefs::{KEY_TOOLTIPS, Prefs};
use rocksvision::app::state::NO_PREVIOUS_VIEW;
use rocksvision::app::{Action, AppState, MapMode, MapMove, MapZoom};
use rocksvision::jobs::{FsBackend, Worker};
use rocksvision::model::{KeyOrder, LsmCf, LsmFile, LsmInventory, LsmLevel, NodeKind, Status};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ------------------------------------------------------------------ harness

fn new_app() -> AppState {
    AppState::new(Worker::spawn_with(Arc::new(|| {}), Arc::new(FsBackend)))
}

fn idle(a: &AppState) -> bool {
    a.busy_label().is_none() && a.session().pending_view().is_none()
}

/// Polls until the app is idle and `done` holds on three consecutive polls.
fn settle(app: &mut AppState, what: &str, done: impl Fn(&AppState) -> bool) {
    let t = Instant::now();
    let mut ok = 0;
    loop {
        app.poll();
        ok = if idle(app) && done(app) { ok + 1 } else { 0 };
        if ok >= 3 {
            return;
        }
        assert!(t.elapsed() < Duration::from_secs(10), "timeout waiting for: {what} (status {:?})", app.session().status_msg());
        std::thread::sleep(Duration::from_millis(3));
    }
}

/// Lets queued work finish and proves nothing changed it (for "does nothing" assertions).
fn quiet(app: &mut AppState) {
    for _ in 0..60 {
        app.poll();
        std::thread::sleep(Duration::from_millis(3));
    }
}

fn file_name(a: &AppState) -> Option<String> {
    a.session().file().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned())
}

fn open_dir(app: &mut AppState, dir: &Path) {
    app.apply(Action::OpenPath(dir.to_path_buf()));
    settle(app, "folder opened", |a| a.session().dir().is_some() && !a.session().dir_entries().is_empty());
}

/// Folder opened and CURRENT auto-opened.
fn open_dir_current(app: &mut AppState, dir: &Path) {
    app.apply(Action::OpenPath(dir.to_path_buf()));
    settle(app, "CURRENT auto-opened", |a| file_name(a).as_deref() == Some("CURRENT") && a.session().view().is_some());
}

fn open_file(app: &mut AppState, p: &Path) {
    app.apply(Action::OpenFile(p.to_path_buf()));
    let want = p.to_path_buf();
    settle(app, "file opened", move |a| a.session().file() == Some(want.as_path()) && a.session().view().is_some());
    eprintln!("OBS after OpenFile {}: dir={:?} dir_entries={} dir_note={:?}", p.display(), app.session().dir(), app.session().dir_entries().len(), app.session().dir_note());
}

fn show_map(app: &mut AppState) {
    app.apply(Action::ShowLsmMap);
    settle(app, "map shown", |a| a.session().map_shown() && a.session().view().is_some());
}

fn to_levels(app: &mut AppState) {
    app.apply(Action::Navigate { target: NodeKind::ManifestLevels, span: None });
    settle(app, "levels view", |a| a.session().view_kind() == Some(&NodeKind::ManifestLevels) && a.session().view().is_some());
}

fn inv(a: &AppState) -> &LsmInventory {
    &a.session().view().expect("a view").lsm
}

fn unavailable(a: &AppState) -> String {
    a.session().lsm_map_availability().expect_err("the LSM Map must be unavailable").0
}

fn all_text(a: &AppState) -> Vec<String> {
    let mut t: Vec<String> = vec![a.session().dir_note().to_string()];
    if let Some(v) = a.session().view() {
        t.extend(v.notes.iter().map(|n| n.text.clone()));
    }
    t.extend(levels_summary(a.session()));
    t
}

fn no_provenance(a: &AppState) {
    for t in all_text(a) {
        let l = t.to_lowercase();
        assert!(!l.contains("checkpoint created") && !l.contains("created at") && !l.contains("creation time"), "provenance claim: {t}");
    }
}

/// Scratch directory removed on drop (also when an assertion fails).
struct Scratch(PathBuf);
impl Scratch {
    fn new(tag: &str) -> Scratch {
        // The platform temp dir (not a macOS-only /private/tmp root): writable on every CI runner.
        let p = std::env::temp_dir().join(format!("rocksvision-v014map-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Scratch(p)
    }
    /// Plain copy (never a link) of `src` files except those in `skip`.
    fn copy_from(&self, src: &Path, skip: &[&str]) -> &Scratch {
        for e in std::fs::read_dir(src).unwrap() {
            let e = e.unwrap();
            let n = e.file_name().to_string_lossy().to_string();
            if !skip.contains(&n.as_str()) {
                std::fs::copy(e.path(), self.0.join(&n)).unwrap();
            }
        }
        self
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn ckpt() -> PathBuf {
    fx("db-checkpoint")
}

/// cf name -> level -> [(file number, size)] with files only (tests/fixtures/MANIFEST.md
/// "## db-checkpoint/" live_files table).
type Layout = BTreeMap<String, BTreeMap<u32, Vec<(u64, u64)>>>;

fn layout_of(i: &LsmInventory) -> Layout {
    let mut m = Layout::new();
    for c in &i.cfs {
        for l in c.levels.iter().filter(|l| !l.files.is_empty()) {
            let mut v: Vec<(u64, u64)> = l.files.iter().map(|f| (f.number, f.size)).collect();
            v.sort();
            m.entry(c.name.clone()).or_default().insert(l.level, v);
        }
    }
    m
}

fn expected_checkpoint() -> Layout {
    let mut m = Layout::new();
    let mut put = |cf: &str, l: u32, f: &[(u64, u64)]| {
        m.entry(cf.to_string()).or_default().insert(l, f.to_vec());
    };
    put("default", 0, &[(9, 1506)]);
    put("cf_a", 0, &[(11, 1508), (13, 1503)]);
    put("cf_b", 1, &[(15, 1504), (17, 1507)]);
    m
}

// ------------------------------------------------------------------ a / b: checkpoint folder

#[test]
fn checkpoint_folder_opens_current_and_the_map_shows_the_live_layout() {
    let mut app = new_app();
    open_dir_current(&mut app, &ckpt());
    assert_eq!(app.session().lsm_map_availability(), Ok(MapEntry::InFile), "CURRENT root: the map opens via the file");
    show_map(&mut app);
    assert!(app.session().map_shown());
    assert_eq!(app.session().lsm_map_availability(), Ok(MapEntry::Shown));
    let i = inv(&app);
    assert!(!i.truncated);
    assert_eq!(i.cfs.len(), 3, "default, cf_a, cf_b");
    assert_eq!(layout_of(i), expected_checkpoint());
    for c in &i.cfs {
        assert_eq!(c.order, KeyOrder::Bytewise, "{}", c.name);
        for l in &c.levels {
            assert_eq!(l.on_disk, l.files.len(), "{} L{}", c.name, l.level);
            assert_eq!(l.bytes, l.files.iter().map(|f| f.size).sum::<u64>());
        }
        for f in c.levels.iter().flat_map(|l| &l.files) {
            assert_eq!(f.status, Status::Present, "{}", f.name());
            let p = f.path.as_ref().expect("an on-disk file has its path");
            assert_eq!(std::fs::metadata(p).unwrap().len(), f.size, "MANIFEST size == file size for {}", f.name());
        }
    }
    let by_name = |n: &str| i.cfs.iter().find(|c| c.name == n).unwrap();
    let l0 = |c: &LsmCf| c.levels.iter().find(|l| l.level == 0).unwrap().lanes;
    assert_eq!(l0(by_name("cf_a")), 2, "the two overlapping L0 files take two lanes");
    assert_eq!(l0(by_name("default")), 1);
    assert!(by_name("cf_a").levels[0].files.iter().all(|f| f.overlap_same == Some(1)), "each overlaps the other");
    assert_eq!(by_name("cf_b").levels.iter().find(|l| l.level == 1).unwrap().lanes, 1, "disjoint L1 files share a lane");
    // Statuses: the folder note states the layout and the WAL count; no provenance claim.
    let dn = app.session().dir_note();
    assert!(dn.contains("CURRENT present"), "{dn}");
    assert!(dn.contains("1 WAL"), "the empty live log is counted: {dn}");
    assert!(dn.contains("5 SST") && dn.contains("1 MANIFEST"), "{dn}");
    let v = app.session().view().unwrap();
    assert!(v.notes[0].text.contains("CURRENT -> MANIFEST-000005") && v.notes[0].text.contains("5 live SST") && v.notes[0].text.contains("all on disk"), "{:?}", v.notes[0]);
    assert!(v.notes.iter().all(|n| !n.text.starts_with("Problem:")), "{:?}", v.notes);
    no_provenance(&app);
}

#[test]
fn checkpoint_without_its_wal_says_so_and_is_not_degraded() {
    let s = Scratch::new("nowal");
    s.copy_from(&ckpt(), &["000016.log"]);
    let mut app = new_app();
    open_dir_current(&mut app, s.path());
    show_map(&mut app);
    assert_eq!(layout_of(inv(&app)), expected_checkpoint());
    let dn = app.session().dir_note();
    assert!(dn.contains("no WAL"), "{dn}");
    let v = app.session().view().unwrap();
    assert!(v.notes[0].text.contains("no WAL"), "{:?}", v.notes[0]);
    for n in &v.notes {
        assert!(!matches!(n.status, Some(Status::Invalid | Status::NotPresent)), "WAL absence flagged: {n:?}");
    }
    assert!(!matches!(v.status, Some(Status::Invalid | Status::NotPresent)), "{:?}", v.status);
    assert!(inv(&app).iter().all(|f| f.status == Status::Present));
    no_provenance(&app);
}

// ------------------------------------------------------------------ c: missing SST

#[test]
fn missing_sst_is_not_present_in_the_inventory_and_cannot_be_followed() {
    let s = Scratch::new("missing");
    s.copy_from(&ckpt(), &["000013.sst"]);
    let mut app = new_app();
    open_dir_current(&mut app, s.path());
    show_map(&mut app);
    let (row, cf_id, level_files, level_on_disk) = {
        let (c, l, f) = inv(&app).by_number(13).expect("the MANIFEST still lists 000013");
        assert_eq!((c.name.as_str(), l.level, f.status, f.path.is_none()), ("cf_a", 0, Status::NotPresent, true));
        (f.row, c.id, l.files.len(), l.on_disk)
    };
    assert_eq!((level_files, level_on_disk), (2, 1), "level header: 2 files, 1 on disk");
    let others = inv(&app).iter().filter(|f| f.number != 13).count();
    assert_eq!(others, 4);
    assert!(inv(&app).iter().filter(|f| f.number != 13).all(|f| f.on_disk()));
    let note = &app.session().view().unwrap().notes[0].text;
    assert!(note.contains("1 of 5 missing on disk"), "{note}");
    let _ = cf_id;

    let back = app.navigation().can_go_back();
    app.apply(Action::SelectRow(row));
    app.apply(Action::FollowRow(row));
    quiet(&mut app);
    assert_eq!(file_name(&app).as_deref(), Some("CURRENT"), "following a missing file opens nothing");
    assert!(app.session().map_shown(), "the map stays");
    assert_eq!(app.navigation().can_go_back(), back, "no history entry for a refused follow");
    assert_eq!(app.selection().row(), Some(row), "the missing row stays selected");
    let card = lsm_file_card(app.session(), row).expect("a card for the selected row");
    assert!(!card.on_disk);
    assert!(card.open.contains("○ Missing:") && card.open.contains("cannot be opened"), "{}", card.open);
    assert!(levels_summary(app.session()).iter().any(|l| l.contains("1 missing")), "{:?}", levels_summary(app.session()));
}

// ------------------------------------------------------------------ d: unavailable cases

#[test]
fn lone_sst_has_no_map_and_says_why() {
    let s = Scratch::new("lone");
    std::fs::copy(ckpt().join("000011.sst"), s.path().join("000011.sst")).unwrap();
    let sst = s.path().join("000011.sst");
    let mut app = new_app();
    open_file(&mut app, &sst);
    let r = unavailable(&app);
    assert_eq!(r, "000011.sst was opened without its database folder (no CURRENT next to it). Level and neighbours are unknown. Open the RocksDB folder to see the LSM map.");
    assert!(r.contains("was opened without its database folder"));
    let before = (file_name(&app), app.navigation().can_go_back());
    app.apply(Action::ShowLsmMap);
    quiet(&mut app);
    assert!(!app.session().map_shown(), "ShowLsmMap does nothing without a trustworthy MANIFEST");
    assert_eq!((file_name(&app), app.navigation().can_go_back()), before);
    assert_eq!(lsm_map_unavailable(app.session()), Some(format!("LSM Map: unavailable — {r}")));
    // The Open File… route (OpenPath, folder scanned) gives the same reason.
    let mut app2 = new_app();
    app2.apply(Action::OpenPath(sst.clone()));
    settle(&mut app2, "sst opened by path", |a| a.session().file() == Some(sst.as_path()) && a.session().view().is_some() && !a.session().dir_entries().is_empty());
    assert_eq!(unavailable(&app2), r);
}

/// Observation (not an assertion of fixed behaviour): an SST that HAS its CURRENT next to it,
/// opened by `OpenFile` (no folder scan: `dir` stays None) and by `OpenPath` (the Open File… route).
#[test]
fn sst_next_to_a_current_is_not_called_lone_when_opened_by_path() {
    let sst = ckpt().join("000011.sst");
    let mut app = new_app();
    app.apply(Action::OpenPath(sst.clone()));
    settle(&mut app, "sst opened by path", |a| a.session().file() == Some(sst.as_path()) && a.session().view().is_some() && !a.session().dir_entries().is_empty());
    match app.session().lsm_map_availability() {
        Ok(MapEntry::Via { path, focus, .. }) => {
            assert_eq!(path.file_name().unwrap(), "CURRENT");
            assert_eq!(focus, Some(11));
        }
        other => panic!("expected the map to be offered via CURRENT, got {other:?}"),
    }
    let mut app2 = new_app();
    open_file(&mut app2, &sst);
    eprintln!("OBS OpenFile(sst next to CURRENT) availability: {:?}", app2.session().lsm_map_availability());
}

#[test]
fn folder_without_current_or_manifest_has_no_map() {
    let s = Scratch::new("nocur");
    for n in ["000009.sst", "000011.sst"] {
        std::fs::copy(ckpt().join(n), s.path().join(n)).unwrap();
    }
    let mut app = new_app();
    open_dir(&mut app, s.path());
    let r = unavailable(&app);
    assert_eq!(r, "This folder has no CURRENT or MANIFEST, so its live SST files are unknown. Open the RocksDB data folder that contains CURRENT.");
    assert!(r.contains("no CURRENT or MANIFEST"));
    app.apply(Action::ShowLsmMap);
    quiet(&mut app);
    assert!(!app.session().map_shown());
    assert_eq!(lsm_map_unavailable(app.session()), Some(format!("LSM Map: unavailable — {r}")));
    assert!(app.session().dir_note().contains("no CURRENT or MANIFEST"), "{}", app.session().dir_note());
}

#[test]
fn current_naming_a_missing_manifest_has_no_map() {
    let s = Scratch::new("nomani");
    s.copy_from(&ckpt(), &["MANIFEST-000005"]);
    let mut app = new_app();
    open_dir_current(&mut app, s.path());
    let r = unavailable(&app);
    assert_eq!(r, "CURRENT names MANIFEST-000005, which is not in this folder. The live SST files cannot be observed.");
    app.apply(Action::ShowLsmMap);
    quiet(&mut app);
    assert!(!app.session().map_shown());
    assert_eq!(lsm_map_unavailable(app.session()), Some(format!("LSM Map: unavailable — {r}")));
    // The Observed levels view itself reports the same cause (levels_unavailable).
    to_levels(&mut app);
    let v = app.session().view().unwrap();
    assert!(v.lsm.is_empty());
    let t = &v.rows[0].field.value;
    eprintln!("OBS missing-MANIFEST levels view value: {t:?}; availability on that view: {:?}", app.session().lsm_map_availability());
    assert_eq!(t, &r, "levels view text must equal the shared availability sentence");
}

// ------------------------------------------------------------------ e: MANIFEST opened directly

#[test]
fn manifest_without_current_gets_a_map_with_an_unconfirmed_note() {
    let s = Scratch::new("manonly");
    std::fs::copy(fx("db-levels/MANIFEST-000005"), s.path().join("MANIFEST-000005")).unwrap();
    let mut app = new_app();
    open_dir(&mut app, s.path());
    assert!(app.session().dir_note().contains("not confirmed"), "{}", app.session().dir_note());
    open_file(&mut app, &s.path().join("MANIFEST-000005"));
    assert_eq!(app.session().lsm_map_availability(), Ok(MapEntry::InFile));
    show_map(&mut app);
    let v = app.session().view().unwrap();
    assert!(v.notes.iter().any(|n| n.text == "Not confirmed as the active MANIFEST (no CURRENT)."), "{:?}", v.notes);
    assert!(!v.lsm.is_empty());
    // Only the MANIFEST was copied: every referenced SST is reported missing, none invented.
    assert!(inv(&app).iter().all(|f| f.status == Status::NotPresent));
}

// ------------------------------------------------------------------ f: cross-file journey

#[test]
fn journey_levels_sst_map_then_back_three_times_without_duplicates() {
    let dir = fx("db-levels");
    let mut app = new_app();
    open_dir_current(&mut app, &dir);
    to_levels(&mut app);
    assert_eq!(app.session().map().mode(), MapMode::Table);
    let (row, number, cf_id, sst) = {
        let f = inv(&app).iter().find(|f| f.on_disk()).expect("an on-disk SST");
        let (c, _, _) = inv(&app).by_number(f.number).unwrap();
        (f.row, f.number, c.id, f.path.clone().unwrap())
    };
    app.apply(Action::SelectRow(row));
    assert_eq!(app.selection().row(), Some(row));
    app.apply(Action::FollowRow(row));
    {
        let want = sst.clone();
        settle(&mut app, "sst opened", move |a| a.session().file() == Some(want.as_path()) && a.session().view().is_some());
    }
    assert!(app.navigation().can_go_back(), "the SST was opened from the levels table: Back is available");
    assert!(app.session().view().unwrap().rows.len() > 1);
    app.apply(Action::SelectRow(1));
    let sel_name = app.selected_field().map(|f| f.name.clone()).expect("a selected field in the SST");

    // LSM Map from the SST: CURRENT's map, focus on that file, CF selected.
    show_map(&mut app);
    assert_eq!(file_name(&app).as_deref(), Some("CURRENT"));
    assert_eq!(app.session().map().focus(), Some(number));
    assert_eq!(app.session().map().shown_cf(inv(&app)), Some(cf_id));

    // Back 1: the SST with the same selection (the map entry pushed exactly one entry).
    app.apply(Action::Back);
    {
        let (want, n) = (sst.clone(), sel_name.clone());
        settle(&mut app, "back to the SST", move |a| a.session().file() == Some(want.as_path()) && a.selected_field().map(|f| f.name.as_str()) == Some(n.as_str()));
    }
    assert!(!app.session().map_shown());
    // Back 2: the levels Table of CURRENT with row `row` selected, map mode Table.
    app.apply(Action::Back);
    settle(&mut app, "back to the levels table", move |a| file_name(a).as_deref() == Some("CURRENT") && a.session().view_kind() == Some(&NodeKind::ManifestLevels) && a.selection().row() == Some(row));
    assert_eq!(app.session().map().mode(), MapMode::Table);
    assert!(!app.session().map_shown());
    assert!(app.navigation().can_go_back(), "the CURRENT root view is still behind");
    // Back 3: the CURRENT root; nothing earlier.
    app.apply(Action::Back);
    settle(&mut app, "back to the CURRENT root", |a| file_name(a).as_deref() == Some("CURRENT") && a.session().view_kind() == Some(&NodeKind::Root));
    assert!(!app.navigation().can_go_back(), "no duplicate entries left behind");
    app.apply(Action::Back);
    assert_eq!(app.session().status_msg(), NO_PREVIOUS_VIEW);
    assert_eq!(app.session().view_kind(), Some(&NodeKind::Root), "Back with an empty history strands nobody");
}

// ------------------------------------------------------------------ g: zoom / pan independence

fn db_levels_map() -> AppState {
    let mut app = new_app();
    open_dir_current(&mut app, &fx("db-levels"));
    to_levels(&mut app);
    app.apply(Action::SetMapMode(MapMode::Map));
    settle(&mut app, "map mode", |a| a.session().map_shown());
    app
}

#[test]
fn map_zoom_and_app_zoom_are_independent() {
    let mut app = db_levels_map();
    assert_eq!(app.session().map().zoom(), ZOOM_MIN);
    let app_zoom = app.prefs().zoom();
    app.apply(Action::MapZoom(MapZoom::In));
    let z = app.session().map().zoom();
    assert!(z > ZOOM_MIN && z <= ZOOM_CAP, "{z}");
    assert_eq!(app.prefs().zoom(), app_zoom, "map zoom never touches the app zoom");
    app.apply(Action::ZoomIn);
    assert_eq!(app.session().map().zoom(), z, "app zoom never touches the map zoom");
    app.apply(Action::MapZoom(MapZoom::Out));
    assert!(app.session().map().zoom() < z);
    app.apply(Action::MapView { pan: (0.2, 30.0), zoom: 3.0 });
    assert_eq!(app.session().map().zoom(), 3.0);
    app.apply(Action::MapZoom(MapZoom::Fit));
    assert_eq!((app.session().map().zoom(), app.session().map().pan()), (1.0, (0.0, 0.0)), "Fit: ×1, pan reset");
}

#[test]
fn map_view_is_clamped_to_the_documented_bounds() {
    let mut app = db_levels_map();
    app.apply(Action::MapView { pan: (5.0, -3.0), zoom: 1.0e9 });
    let (z, p) = (app.session().map().zoom(), app.session().map().pan());
    assert!((ZOOM_MIN..=ZOOM_CAP).contains(&z), "{z}");
    assert!(p.0 >= 0.0 && p.0 <= 1.0 - 1.0 / z + 1e-6 && p.1 >= 0.0, "{p:?} at ×{z}");
    app.apply(Action::MapView { pan: (-1.0, 4.0), zoom: 0.0 });
    let (z, p) = (app.session().map().zoom(), app.session().map().pan());
    assert_eq!((z, p), (ZOOM_MIN, (0.0, 4.0)), "zoom below ×1 is ×1; the left edge cannot go below 0");
    app.apply(Action::MapView { pan: (f32::NAN, f32::INFINITY), zoom: f32::NAN });
    let (z, p) = (app.session().map().zoom(), app.session().map().pan());
    assert!(z.is_finite() && p.0.is_finite() && p.1.is_finite(), "{z} {p:?}");
    app.apply(Action::MapView { pan: (0.3, 40.0), zoom: 3.0 });
    assert_eq!((app.session().map().zoom(), app.session().map().pan()), (3.0, (0.3, 40.0)), "an in-range view is kept exactly");
}

#[test]
fn back_after_map_zoom_and_pan_restores_the_same_view() {
    let mut app = db_levels_map();
    let row = inv(&app).iter().find(|f| f.on_disk()).unwrap().row;
    app.apply(Action::MapView { pan: (0.25, 12.0), zoom: 2.0 });
    let snap = app.session().map().snapshot();
    assert_eq!((snap.mode, snap.zoom, snap.pan), (MapMode::Map, 2.0, (0.25, 12.0)));
    app.apply(Action::FollowRow(row)); // double-click on a box
    settle(&mut app, "sst opened", |a| file_name(a).is_some_and(|n| n.ends_with(".sst")) && a.session().view().is_some());
    app.apply(Action::Back);
    settle(&mut app, "map restored", |a| file_name(a).as_deref() == Some("CURRENT") && a.session().map_shown());
    assert_eq!(app.session().map().snapshot(), snap, "mode, CF, pan, zoom and focus come back exactly");
}

#[test]
fn map_cf_selection_and_keyboard_move_focus_a_box() {
    let mut app = new_app();
    open_dir_current(&mut app, &ckpt());
    show_map(&mut app);
    let (id_b, first_b) = {
        let c = inv(&app).cfs.iter().find(|c| c.name == "cf_b").unwrap();
        (c.id, c.levels.iter().flat_map(|l| &l.files).map(|f| f.number).min().unwrap())
    };
    app.apply(Action::MapSelectCf(id_b));
    assert_eq!(app.session().map().shown_cf(inv(&app)), Some(id_b));
    app.apply(Action::MapMove(MapMove::Home));
    let n = app.session().map().focus().expect("Home focuses a box");
    assert_eq!(inv(&app).by_number(n).map(|(c, _, _)| c.id), Some(id_b), "focus stays in the shown CF");
    assert_eq!(app.selection().row(), inv(&app).by_number(n).map(|(_, _, f)| f.row), "the focused box's row is selected");
    assert_eq!(n, first_b, "Home = first box of the first level with files");
    app.apply(Action::MapMove(MapMove::Right));
    assert_ne!(app.session().map().focus(), Some(n), "Right moves to the next box of the lane");
}

// ------------------------------------------------------------------ h: tooltips and the file card

#[test]
fn tooltips_preference_is_persisted_and_defaults_on() {
    let mut app = new_app();
    assert!(app.prefs().tooltips());
    assert!(Prefs::load(|_| None).tooltips, "default on");
    app.apply(Action::SetTooltips(false));
    assert!(!app.prefs().tooltips());
    let p = app.prefs().persisted();
    assert!(!p.tooltips);
    let mut kv = BTreeMap::new();
    p.store(|k, v| {
        kv.insert(k.to_string(), v);
    });
    assert_eq!(KEY_TOOLTIPS, "tooltips");
    assert_eq!(kv.get(KEY_TOOLTIPS).map(String::as_str), Some("false"));
    // restart: a new session reading the stored values
    assert!(!Prefs::load(|k| kv.get(k).cloned()).tooltips);
    app.apply(Action::SetTooltips(true));
    let mut kv2 = BTreeMap::new();
    app.prefs().persisted().store(|k, v| {
        kv2.insert(k.to_string(), v);
    });
    assert_eq!(kv2.get(KEY_TOOLTIPS).map(String::as_str), Some("true"));
    assert!(Prefs::load(|k| kv2.get(k).cloned()).tooltips);
    assert!(Prefs::load(|k| (k == KEY_TOOLTIPS).then(|| "garbage".to_string())).tooltips, "anything but false = on");
}

#[test]
fn selected_box_card_lists_everything_without_tooltips() {
    let mut app = new_app();
    open_dir_current(&mut app, &ckpt());
    show_map(&mut app);
    app.apply(Action::SetTooltips(false));
    let (row, f, cfname, level, order) = {
        let (c, l, f) = inv(&app).by_number(11).unwrap();
        (f.row, f.clone(), c.name.clone(), l.level, c.order.clone())
    };
    app.apply(Action::SelectRow(row));
    let card = lsm_file_card(app.session(), row).expect("card");
    let facts: BTreeMap<&str, &str> = card.facts.iter().map(|(k, v)| (*k, v.as_str())).collect();
    assert_eq!(card.name, "000011.sst");
    assert_eq!(facts["File"], "000011.sst");
    assert!(facts["Column family"].contains(&cfname), "{facts:?}");
    assert_eq!(facts["Level"], format!("L{level}"));
    assert!(facts["Size"].contains(&rocksvision::util::human_size(f.size)), "{facts:?}");
    assert_eq!(facts["Smallest"], f.smallest_preview);
    assert_eq!(facts["Largest"], f.largest_preview);
    assert!(facts["Smallest"].contains("a-000") && facts["Largest"].contains("a-039"), "{facts:?}");
    assert!(facts["Seq range"].contains(".."), "{facts:?}");
    assert!(facts["Overlap"].starts_with("1 in L0 (allowed)"), "{}", facts["Overlap"]);
    assert_eq!(facts["On disk"], "yes");
    assert!(facts["Path"].ends_with("000011.sst"));
    assert_eq!(card.legend, order.legend());
    assert_eq!(card.legend, "x: user-key order (bytewise comparator), boxes placed by key rank, not to scale");
    assert!(card.open.contains("double-click"), "{}", card.open);
    assert!(card.on_disk);
}

// ------------------------------------------------------------------ i: Table <-> Map

#[test]
fn table_map_switch_keeps_the_selected_row_and_adds_no_history() {
    let mut app = new_app();
    open_dir_current(&mut app, &fx("db-levels"));
    to_levels(&mut app);
    let row = inv(&app).iter().nth(2).unwrap().row;
    app.apply(Action::SelectRow(row));
    app.apply(Action::SetMapMode(MapMode::Map));
    assert!(app.session().map_shown());
    assert_eq!(app.selection().row(), Some(row));
    app.apply(Action::SetMapMode(MapMode::Table));
    assert!(!app.session().map_shown());
    assert_eq!(app.session().map().mode(), MapMode::Table);
    assert_eq!(app.selection().row(), Some(row));
    // The toolbar action toggles the same way while the map is shown.
    app.apply(Action::ShowLsmMap);
    settle(&mut app, "map", |a| a.session().map_shown());
    app.apply(Action::ShowLsmMap);
    settle(&mut app, "table again", |a| !a.session().map_shown());
    assert_eq!(app.selection().row(), Some(row));
    // Exactly one history step is behind us (the CURRENT root); the switches added none.
    app.apply(Action::Back);
    settle(&mut app, "root", |a| a.session().view_kind() == Some(&NodeKind::Root));
    assert!(!app.navigation().can_go_back(), "mode switches left no history entries");
}

// ------------------------------------------------------------------ j: comparators

#[test]
fn unsupported_comparators_say_so_and_never_compute_overlap() {
    // No committed fixture records a non-bytewise comparator (grep "omparator" tests/fixtures/MANIFEST.md
    // is empty), so this is a unit-style test of the public model types.
    let rev = KeyOrder::from_comparator(Some("rocksdb.ReverseBytewiseComparator"));
    assert_eq!(rev, KeyOrder::ReverseBytewise);
    assert!(rev.ordered());
    assert_eq!(rev.legend(), "x: user-key order (reverse bytewise comparator: larger keys on the left), not to scale");
    let custom = KeyOrder::from_comparator(Some("custom"));
    assert_eq!(custom, KeyOrder::Unsupported("custom".into()));
    assert!(!custom.ordered());
    assert_eq!(custom.legend(), "x: file number order. Comparator 'custom' is not supported, so key order, overlap and gaps are not shown; boxes are equal in size.");
    let none = KeyOrder::from_comparator(None);
    assert_eq!(none, KeyOrder::Unknown);
    assert!(!none.ordered());
    assert!(none.legend().starts_with("x: file number order.") && none.legend().contains("not recorded"), "{}", none.legend());
    assert_eq!(KeyOrder::from_comparator(Some("leveldb.BytewiseComparator")), KeyOrder::Bytewise);

    let file = LsmFile {
        number: 7,
        size: 10,
        smallest: b"a".to_vec(),
        largest: b"z".to_vec(),
        smallest_preview: "a".into(),
        largest_preview: "z".into(),
        seq: None,
        overlap_same: None,
        overlap_next: None,
        status: Status::Present,
        row: 0,
        path: None,
        slots: (0, 1),
        lane: 0,
    };
    let level = LsmLevel { level: 1, files: vec![file.clone()], bytes: 10, on_disk: 1, scale: 1, lanes: 1, overlaps: false };
    let cf = LsmCf { id: 0, name: "default".into(), levels: vec![level.clone()], comparator: Some("custom".into()), order: custom };
    assert_eq!(LsmInventory::overlap_text(&cf, &level, &file), "not computed (comparator custom)");
    let cf0 = LsmCf { comparator: None, order: KeyOrder::Unknown, ..cf };
    assert_eq!(LsmInventory::overlap_text(&cf0, &level, &file), "not computed (comparator not recorded)");
}
