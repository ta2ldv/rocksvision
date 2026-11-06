//! Task 0013 / handoff 0028: the shipped `examples/data/multilevel-advanced-db` read through
//! RocksVision's own reader (public API only, no engine): the observed LSM inventory, the WAL
//! inventory, the key ranges and gaps named in examples/README.md, the surviving versions and
//! tombstones in selected SSTs (via the same dump the CLI uses), and that reading leaves every
//! byte and modification time of the folder untouched.
use rocksvision::app::{Action, AppState};
use rocksvision::dump::{DumpOpts, dump};
use rocksvision::jobs::{FsBackend, Worker};
use rocksvision::model::{LsmCf, LsmFile, LsmInventory, Status};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

fn folder() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/data/multilevel-advanced-db")
}

/// Every file of the folder: name -> (bytes, mtime).
fn snapshot() -> BTreeMap<String, (Vec<u8>, SystemTime)> {
    let mut m = BTreeMap::new();
    for e in std::fs::read_dir(folder()).unwrap() {
        let e = e.unwrap();
        let md = e.metadata().unwrap();
        m.insert(e.file_name().to_string_lossy().into_owned(), (std::fs::read(e.path()).unwrap(), md.modified().unwrap()));
    }
    m
}

fn dump_text(name: &str) -> String {
    let mut buf = Vec::new();
    let ok = dump(&folder().join(name), &DumpOpts { limit: 5000, rows: 300 }, &mut buf).unwrap();
    assert!(ok, "dump failed for {name}");
    String::from_utf8_lossy(&buf).into_owned()
}

/// The data-row line(s) of `dump_text` for one user key: `"key" | seq | n Kind | ...`.
fn rows(text: &str, key: &str) -> Vec<String> {
    let pat = format!("\"{key}\" | ");
    text.lines().filter(|l| l.contains(&pat) && l.contains("✔ Present")).map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).collect()
}

/// Folder opened through CURRENT with the app's own worker; returns the inventories.
fn open() -> (LsmInventory, Vec<(String, u64)>) {
    let mut app = AppState::new(Worker::spawn_with(Arc::new(|| {}), Arc::new(FsBackend)));
    app.apply(Action::OpenPath(folder()));
    let t = Instant::now();
    loop {
        app.poll();
        let is_current = app.session().file().and_then(|p| p.file_name()).is_some_and(|n| n == "CURRENT");
        if is_current && app.session().view().is_some() && app.busy_label().is_none() {
            break;
        }
        assert!(t.elapsed() < Duration::from_secs(10), "timeout opening the folder");
        std::thread::sleep(Duration::from_millis(3));
    }
    // The LSM Map (levels view) is what carries the WAL inventory of the folder.
    app.apply(Action::ShowLsmMap);
    let t = Instant::now();
    while !(app.session().map_shown() && app.session().view().is_some() && app.busy_label().is_none() && app.session().wal_inventory().is_some()) {
        app.poll();
        assert!(t.elapsed() < Duration::from_secs(10), "timeout showing the map");
        std::thread::sleep(Duration::from_millis(3));
    }
    let wals = app.session().wal_inventory().expect("WAL inventory for a folder with logs");
    assert!(!wals.truncated);
    (app.session().view().unwrap().lsm.clone(), wals.files.iter().map(|w| (w.name.clone(), w.size)).collect())
}

fn level(c: &LsmCf, l: u32) -> &[LsmFile] {
    c.levels.iter().find(|x| x.level == l).map(|x| x.files.as_slice()).unwrap_or(&[])
}

fn ranges(c: &LsmCf, l: u32) -> Vec<(u64, String, String)> {
    let mut v: Vec<_> = level(c, l).iter().map(|f| (f.number, String::from_utf8_lossy(&f.smallest).into_owned(), String::from_utf8_lossy(&f.largest).into_owned())).collect();
    v.sort();
    v
}

fn r(n: u64, a: &str, b: &str) -> (u64, String, String) {
    (n, a.to_string(), b.to_string())
}

#[test]
fn advanced_example_inventory_wal_and_key_layout() {
    let before = snapshot();
    assert_eq!(before.len(), 35, "35 shipped files");
    let (inv, wals) = open();

    assert!(!inv.truncated);
    let names: Vec<&str> = inv.cfs.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["default", "cf_users", "cf_orders"]);
    let mut live = 0;
    for c in &inv.cfs {
        for l in [0u32, 1, 2] {
            let lv = c.levels.iter().find(|x| x.level == l).unwrap_or_else(|| panic!("{} has no L{l}", c.name));
            assert_eq!((lv.files.len(), lv.on_disk), (3, 3), "{} L{l}: 3 live files, all on disk", c.name);
            for f in &lv.files {
                assert_eq!(f.status, Status::Present, "{}", f.name());
                assert_eq!(std::fs::metadata(f.path.as_ref().unwrap()).unwrap().len(), f.size, "{}", f.name());
            }
            live += lv.files.len();
        }
        assert_eq!(c.files(), 9, "{}: nothing below L2", c.name);
    }
    assert_eq!(live, 27);

    // WAL inventory: exactly these three, with these sizes.
    assert_eq!(wals, [("000146.log".to_string(), 427), ("000148.log".to_string(), 540), ("000150.log".to_string(), 647)]);

    // Ranges per CF: L2 separated with gaps, L1 covering the L2 gaps, L0 mutually overlapping.
    let by = |n: &str| inv.cfs.iter().find(|c| c.name == n).unwrap();
    // (cf, key prefix, L0 files, L1 files, L2 files)
    type CfFiles = (&'static str, &'static str, [u64; 3], [u64; 3], [u64; 3]);
    let want: [CfFiles; 3] = [
        ("default", "item", [135, 137, 147], [58, 67, 76], [44, 45, 46]),
        ("cf_users", "user", [139, 141, 149], [85, 94, 103], [47, 48, 49]),
        ("cf_orders", "order", [143, 145, 151], [112, 121, 130], [50, 51, 52]),
    ];
    for (cf, p, l0, l1, l2) in want {
        let c = by(cf);
        let k = |s: &str| format!("{p}:{s}");
        assert_eq!(ranges(c, 2), [r(l2[0], &k("000"), &k("049")), r(l2[1], &k("100"), &k("149")), r(l2[2], &k("200"), &k("249"))], "{cf} L2");
        assert_eq!(ranges(c, 1), [r(l1[0], &k("030"), &k("079")), r(l1[1], &k("140"), &k("169")), r(l1[2], &k("240"), &k("289"))], "{cf} L1");
        assert_eq!(ranges(c, 0), [r(l0[0], &k("020"), &k("120")), r(l0[1], &k("060"), &k("180")), r(l0[2], &k("030"), &k("260"))], "{cf} L0");
        // The inventory's own previews carry the same boundary keys.
        for f in level(c, 2) {
            assert!(f.smallest_preview.contains(&format!("{p}:")) && f.largest_preview.contains(&format!("{p}:")), "{}", f.name());
            assert_eq!(f.seq, Some((0, 0)), "L2 sequence numbers are zeroed: {cf} {}", f.name());
        }
        // Gaps (README): no L1 or L2 file covers 080..099, 170..199, 290..299; all L0 files
        // overlap the other two; each L1 file overlaps exactly one L2 file.
        let sst = |l| level(c, l).iter().map(|f| (f.smallest.clone(), f.largest.clone())).collect::<Vec<_>>();
        for gap in [80, 90, 99, 170, 185, 199, 290, 299] {
            let key = format!("{p}:{gap:03}").into_bytes();
            for (s, l) in sst(1).into_iter().chain(sst(2)) {
                assert!(!(s <= key && key <= l), "{cf}: {gap:03} is inside an L1/L2 file");
            }
        }
        assert!(level(c, 0).iter().all(|f| f.overlap_same == Some(2)), "{cf} L0 overlap");
        assert!(level(c, 1).iter().all(|f| f.overlap_next == Some(1)), "{cf} L1 overlaps exactly one L2 file");
    }

    assert_eq!(snapshot(), before, "opening and reading changed a file (bytes or mtime)");
}

#[test]
fn advanced_example_ssts_show_versions_and_tombstones() {
    let before = snapshot();
    // item:030 has four versions in four files (oldest at L2 seq 0 .. newest in L0).
    for (file, want) in [
        ("000044.sst", "\"item:030\" | 0 | 1 Value (Put) | \"a2-030\""),
        ("000058.sst", "\"item:030\" | 568 | 1 Value (Put) | \"b1-030\""),
        ("000135.sst", "\"item:030\" | 845 | 1 Value (Put) | \"c1-030\""),
        ("000147.sst", "\"item:030\" | 918 | 1 Value (Put) | \"c3-030\""),
    ] {
        let got = rows(&dump_text(file), "item:030");
        assert_eq!(got.len(), 1, "{file}: one version of item:030 per file, got {got:?}");
        assert!(got[0].contains(want), "{file}: {} lacks {want}", got[0]);
    }
    // L1 000058: point tombstones for 031 and 039 and the range tombstone [034, 038).
    let t58 = dump_text("000058.sst");
    for (key, want) in [("item:031", "\"item:031\" | 594 | 0 Deletion"), ("item:039", "\"item:039\" | 595 | 0 Deletion"), ("item:034", "\"item:034\" | 596 | 15 RangeDeletion | end: \"item:038\"")] {
        let got = rows(&t58, key);
        assert_eq!(got.len(), 1, "{key}: {got:?}");
        assert!(got[0].contains(want), "000058.sst {key}: {}", got[0]);
    }
    // The deleted L2 keys still exist as values in 000044 (why the tombstones survive).
    assert!(rows(&dump_text("000044.sst"), "item:031")[0].contains("1 Value (Put)"));
    // L0 tombstones: Delete item:045 (000135), range delete [100, 106) (000137).
    assert!(rows(&dump_text("000135.sst"), "item:045")[0].contains("\"item:045\" | 855 | 0 Deletion"));
    assert!(rows(&dump_text("000137.sst"), "item:100")[0].contains("\"item:100\" | 865 | 15 RangeDeletion | end: \"item:106\""));
    // Round 1 dropped item:003 (a2 Delete) entirely: no row in any L2 file of default.
    assert!(rows(&dump_text("000044.sst"), "item:003").is_empty());
    assert_eq!(snapshot(), before, "dumping changed a file");
}
