//! 0.1.4 regressions (task 0011 / handoff 0024), public API only, committed fixtures read-only.
//! Task A: reproduction of the reported Back failure on the 0.1.3 behaviour.
//! Task B: tests/fixtures/db-checkpoint (a real RocksDB 10.4.2 CreateCheckpoint, see MANIFEST.md).
mod common;
use common::{fx, never};
use rocksvision::app::{Action, AppState};
use rocksvision::discover::{self, FileKind};
use rocksvision::inspect;
use rocksvision::jobs::{FsBackend, Worker};
use rocksvision::model::{NodeKind, Status};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

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

fn new_app() -> AppState {
    AppState::new(Worker::spawn_with(Arc::new(|| {}), Arc::new(FsBackend)))
}

// ------------------------------------------------------------------ Task A

/// History of the reported failure (reproduced on released 0.1.3): CURRENT -> Observed levels ->
/// follow an "on disk" row (the SST opens) -> the history was reset by opening the file, so Back
/// was not available although the upper-center button is drawn. 0.1.4 keeps the history across
/// the file switch; this test verifies the fix.
#[test]
fn back_after_opening_sst_from_levels_is_possible_fix_verified() {
    let mut app = new_app();
    app.apply(Action::OpenFile(fx("db-levels/CURRENT")));
    pump(&mut app, "CURRENT opened", opened);
    let target = NodeKind::ManifestLevels;
    app.apply(Action::Navigate { target: target.clone(), span: None });
    pump(&mut app, "levels view", |a| a.session().pending_view().is_none() && a.session().view_kind() == Some(&target));
    let v = app.session().view().unwrap();
    let row = v.rows.iter().position(|r| r.field.status == Status::Present && r.field.open_path.is_some()).expect("an on-disk row");
    let sst = v.rows[row].field.open_path.clone().unwrap();
    app.apply(Action::FollowRow(row));
    pump(&mut app, "sst opened", |a| a.session().file() == Some(sst.as_path()) && opened(a));
    // 0.1.3 asserted `!can_go_back()` here (the reproduction); fixed in 0.1.4
    assert!(app.navigation().can_go_back(), "0.1.4: Back is available after opening the SST from the levels table");
}

// ------------------------------------------------------------------ Task B

fn ckpt() -> PathBuf {
    fx("db-checkpoint")
}

/// (cf name, level) -> sorted file names, from the Observed levels table of `<dir>/CURRENT`.
type LevelFiles = BTreeMap<(String, String), Vec<String>>;

fn levels_of(dir: &Path) -> (LevelFiles, Vec<(Status, bool)>) {
    let mut i = inspect::open(&dir.join("CURRENT")).unwrap();
    let v = i.view(&NodeKind::ManifestLevels, &never);
    let mut m = LevelFiles::new();
    for r in &v.rows {
        // cells: "<id> <cf name>", "L<n>", "<file>"
        let cf = r.cells[0].split_once(' ').map(|x| x.1).unwrap_or(&r.cells[0]).to_string();
        m.entry((cf, r.cells[1].clone())).or_default().push(r.cells[2].clone());
    }
    for f in m.values_mut() {
        f.sort();
    }
    (m, v.rows.iter().map(|r| (r.field.status, r.field.open_path.is_some())).collect())
}

fn expected_levels() -> LevelFiles {
    // tests/fixtures/MANIFEST.md "## db-checkpoint/" (DB::live_files() of the source)
    let mut m = BTreeMap::new();
    for (cf, l, f) in [("default", "L0", &["000009"][..]), ("cf_a", "L0", &["000011", "000013"]), ("cf_b", "L1", &["000015", "000017"])] {
        m.insert((cf.to_string(), l.to_string()), f.iter().map(|s| s.to_string()).collect());
    }
    m
}

#[test]
fn checkpoint_dir_opens_via_current_and_lists_every_live_sst_by_cf_and_level() {
    let d = ckpt();
    let (entries, truncated) = discover::scan(&d).unwrap();
    assert!(!truncated);
    let kinds: Vec<FileKind> = entries.iter().map(|e| e.kind).collect();
    assert_eq!(kinds.iter().filter(|k| **k == FileKind::Current).count(), 1);
    assert_eq!(kinds.iter().filter(|k| **k == FileKind::Manifest).count(), 1);
    assert_eq!(kinds.iter().filter(|k| **k == FileKind::Sst).count(), 5);
    // CURRENT names the active MANIFEST (the same file discover found).
    let mut cur = inspect::open(&d.join("CURRENT")).unwrap();
    let root = cur.children(&NodeKind::Root, &never);
    assert!(root.iter().any(|c| c.kind == NodeKind::ManifestLevels));
    let (got, rows) = levels_of(&d);
    assert_eq!(got, expected_levels());
    assert_eq!(rows.len(), 5);
    assert!(rows.iter().all(|r| *r == (Status::Present, true)), "every referenced SST is on disk: {rows:?}");
    // The empty 000016.log is present and must not produce a problem note.
    let v = cur.view(&NodeKind::ManifestLevels, &never);
    assert!(v.notes.iter().all(|n| !n.text.starts_with("Problem:")), "{:?}", v.notes);
    assert!(v.status.is_none() || v.status == Some(Status::Present), "{:?}", v.status);
}

fn copy_dir(src: &Path, dst: &Path, skip: &[&str]) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap() {
        let e = e.unwrap();
        let n = e.file_name().to_string_lossy().to_string();
        if !skip.contains(&n.as_str()) {
            std::fs::copy(e.path(), dst.join(&n)).unwrap(); // plain copy, never a link
        }
    }
}

fn scratch(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("rocksvision-v014-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    p
}

#[test]
fn checkpoint_without_a_wal_is_not_an_error() {
    let s = scratch("nowal");
    copy_dir(&ckpt(), &s, &["000016.log"]);
    let r = std::panic::catch_unwind(|| {
        let (entries, _) = discover::scan(&s).unwrap();
        assert!(entries.iter().all(|e| e.kind != FileKind::Wal));
        let (got, rows) = levels_of(&s);
        assert_eq!(got, expected_levels());
        assert!(rows.iter().all(|r| *r == (Status::Present, true)), "{rows:?}");
    });
    let _ = std::fs::remove_dir_all(&s);
    r.unwrap();
}

fn snapshot(dir: &Path) -> BTreeMap<String, (Vec<u8>, SystemTime)> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            let m = e.metadata().unwrap();
            (e.file_name().to_string_lossy().to_string(), (std::fs::read(e.path()).unwrap(), m.modified().unwrap()))
        })
        .collect()
}

/// Inspection (CURRENT, MANIFEST, every SST and the WAL) leaves bytes and mtimes unchanged, and the
/// committed bytes match SHA256SUMS size-for-size (hashes are checked by `shasum -c` in the helper).
#[test]
fn inspecting_the_checkpoint_does_not_change_any_file() {
    let d = ckpt();
    let before = snapshot(&d);
    assert_eq!(before.len(), 9);
    for name in before.keys() {
        let p = d.join(name);
        if discover::classify(name).decodable() {
            let mut i = inspect::open(&p).unwrap_or_else(|e| panic!("{name}: {e:?}"));
            let _ = i.children(&NodeKind::Root, &never);
            let _ = i.view(&NodeKind::Root, &never);
        }
    }
    let mut cur = inspect::open(&d.join("CURRENT")).unwrap();
    let _ = cur.view(&NodeKind::ManifestLevels, &never);
    // the app path too (opens the dir, the CURRENT, follows a row)
    let mut app = new_app();
    app.apply(Action::OpenFile(d.join("CURRENT")));
    pump(&mut app, "CURRENT opened", opened);
    let after = snapshot(&d);
    assert_eq!(before.keys().collect::<Vec<_>>(), after.keys().collect::<Vec<_>>(), "no file created or removed");
    for (n, (b, t)) in &before {
        assert!(&after[n].0 == b, "{n}: bytes changed");
        assert_eq!(&after[n].1, t, "{n}: mtime changed");
    }
}

/// A scratch dir with only one SST: no CURRENT / MANIFEST, so there is no trustworthy source for
/// observed levels. Records what the current API returns.
#[test]
fn lone_sst_has_no_observed_levels() {
    let s = scratch("lone");
    std::fs::create_dir_all(&s).unwrap();
    let sst = s.join("000011.sst");
    std::fs::copy(ckpt().join("000011.sst"), &sst).unwrap();
    let r = std::panic::catch_unwind(|| {
        let (entries, _) = discover::scan(&s).unwrap();
        assert!(entries.iter().all(|e| e.kind != FileKind::Current && e.kind != FileKind::Manifest));
        // The SST inspector offers no Observed levels node ...
        let mut i = inspect::open(&sst).unwrap();
        let root = i.children(&NodeKind::Root, &never);
        assert!(root.iter().all(|c| c.kind != NodeKind::ManifestLevels), "no levels node for a lone SST");
        // ... and asking for the view anyway yields no level rows.
        let v = i.view(&NodeKind::ManifestLevels, &never);
        eprintln!("lone SST view(ManifestLevels): title={:?} rows={} status={:?} notes={:?}", v.title, v.rows.len(), v.status, v.notes);
        assert!(v.lsm.is_empty());
        // A directory with no CURRENT at all: CURRENT cannot be opened.
        assert!(inspect::open(&s.join("CURRENT")).is_err(), "no CURRENT file");
        // The app opening the lone SST: Observed levels unavailable (no map data), Back not offered.
        let mut app = new_app();
        app.apply(Action::OpenFile(sst.clone()));
        pump(&mut app, "sst opened", opened);
        assert!(app.session().view().unwrap().lsm.is_empty());
        assert!(!app.navigation().can_go_back());
    });
    let _ = std::fs::remove_dir_all(&s);
    r.unwrap();
}

/// A referenced SST removed from a scratch copy: exactly that file is NotPresent (no open path),
/// all others stay Present.
#[test]
fn missing_referenced_sst_is_not_present_and_the_others_stay_present() {
    let s = scratch("missing");
    copy_dir(&ckpt(), &s, &["000013.sst"]);
    let r = std::panic::catch_unwind(|| {
        let mut i = inspect::open(&s.join("CURRENT")).unwrap();
        let v = i.view(&NodeKind::ManifestLevels, &never);
        assert_eq!(v.rows.len(), 5, "the MANIFEST still lists the missing file");
        for row in &v.rows {
            let file = &row.cells[2];
            if file == "000013" {
                assert_eq!(row.field.status, Status::NotPresent);
                assert!(row.field.open_path.is_none(), "nothing to open for a missing file");
                assert!(row.cells.last().unwrap().contains("missing"), "{:?}", row.cells);
            } else {
                assert_eq!(row.field.status, Status::Present, "{file}");
                assert!(row.field.open_path.is_some());
            }
        }
        assert_eq!(v.lsm.iter().filter(|f| f.status == Status::NotPresent).count(), 1);
        assert_eq!(v.lsm.iter().filter(|f| f.status == Status::Present).count(), 4);
    });
    let _ = std::fs::remove_dir_all(&s);
    r.unwrap();
}
