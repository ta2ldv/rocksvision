//! Public examples dataset (examples/data, examples/files): every committed example opens, in the
//! way the acceptance criteria describe, with the expected kind and Present/Invalid/Truncated status.
//! Std-only plus the public API. The examples are read-only inputs: files/ entries are copied into
//! an otherwise EMPTY temp dir before opening (no CURRENT, MANIFEST or siblings).
mod common;
use common::*;
use rocksvision::dump::{dump, DumpOpts};
use rocksvision::inspect;
use rocksvision::model::{NodeKind, Status};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn ex(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("examples").join(rel)
}

fn dump_text(p: &Path) -> String {
    let mut buf = Vec::new();
    let ok = dump(p, &DumpOpts { limit: 5000, rows: 300 }, &mut buf).unwrap_or_else(|e| panic!("dump io error on {}: {e}", p.display()));
    assert!(ok, "dump returned Ok(false) for {}", p.display());
    String::from_utf8_lossy(&buf).into_owned()
}

/// Copies one example file into a fresh temp dir that contains nothing else and returns the new path.
fn isolate(tag: &str, src: &Path) -> (PathBuf, PathBuf) {
    let d = scratch(tag);
    let dst = d.join(src.file_name().unwrap());
    std::fs::copy(src, &dst).unwrap_or_else(|e| panic!("copy {}: {e}", src.display()));
    assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1, "isolated dir must hold ONLY the example file");
    (d, dst)
}

fn lines_with<'a>(text: &'a str, pat: &str) -> Vec<&'a str> {
    text.lines().filter(|l| l.contains(pat)).collect()
}

fn has_problem(text: &str) -> bool {
    text.contains(Status::Invalid.symbol()) || text.contains(Status::Truncated.symbol())
}

/// (examples/files directory, file name, expected kind label)
const FILES: [(&str, &str, &str); 18] = [
    ("sst-none", "sst-none.sst", "SST"),
    ("sst-snappy", "sst-snappy.sst", "SST"),
    ("sst-zlib", "sst-zlib.sst", "SST"),
    ("sst-bzip2", "sst-bzip2.sst", "SST"),
    ("sst-lz4", "sst-lz4.sst", "SST"),
    ("sst-lz4hc", "sst-lz4hc.sst", "SST"),
    ("sst-zstd", "sst-zstd.sst", "SST"),
    ("sst-bloom-full", "sst-bloom-full.sst", "SST"),
    ("wal-plain", "000004.log", "WAL"),
    ("wal-fragmented", "000004.log", "WAL"),
    ("wal-recycled", "000014.log", "WAL"),
    ("manifest-basic", "MANIFEST-000005", "MANIFEST"),
    ("current-basic", "CURRENT", "CURRENT"),
    ("corrupt-sst-checksum-flip", "corrupt-sst-checksum-flip.sst", "SST"),
    ("corrupt-sst-truncated", "corrupt-sst-truncated.sst", "SST"),
    ("corrupt-sst-bad-footer", "corrupt-sst-bad-footer.sst", "SST"),
    ("corrupt-wal-checksum-flip", "corrupt-wal-checksum-flip.log", "WAL"),
    ("corrupt-wal-truncated", "corrupt-wal-truncated.log", "WAL"),
];

#[test]
fn files_dir_matches_the_table() {
    let on_disk: BTreeSet<String> = std::fs::read_dir(ex("files")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).filter(|n| !n.starts_with('.')).collect();
    let listed: BTreeSet<String> = FILES.iter().map(|f| f.0.to_string()).collect();
    assert_eq!(on_disk, listed, "examples/files directories vs test table");
    for (d, f, _) in FILES.iter() {
        let names: Vec<_> = std::fs::read_dir(ex(&format!("files/{d}"))).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec![f.to_string()], "files/{d} must hold exactly one file");
    }
}

#[test]
fn files_open_standalone_with_expected_kind_and_status() {
    for (d, f, kind) in FILES.iter() {
        let (tmp, p) = isolate(&format!("ex-{d}"), &ex(&format!("files/{d}/{f}")));
        let mut i = inspect::open(&p).unwrap_or_else(|e| panic!("{d}/{f}: open {e}"));
        assert_eq!(i.kind_label(), *kind, "{d}/{f}: kind");
        let _root = i.view(&NodeKind::Root, &never);
        let kids = i.children(&NodeKind::Root, &never);
        assert!(!kids.is_empty(), "{d}/{f}: root has children");
        let text = dump_text(&p);
        let corrupt = d.starts_with("corrupt-");
        if corrupt {
            assert!(has_problem(&text), "{d}: expected Invalid/Truncated in dump");
        } else if *d == "wal-recycled" {
            // a recycled WAL ends in the previous owner's stale data: valid prefix Present, tail flagged
            assert!(has_problem(&text) || text.contains("stale recycled record"), "{d}: stale tail must be visible");
        } else {
            assert!(!has_problem(&text), "{d}/{f}: valid example must show no Invalid/Truncated:\n{}", lines_with(&text, Status::Invalid.symbol()).join("\n"));
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

#[test]
fn corrupt_examples_report_the_documented_detection() {
    let run = |d: &str, f: &str| {
        let (tmp, p) = isolate(&format!("exc-{d}"), &ex(&format!("files/{d}/{f}")));
        let t = dump_text(&p);
        let _ = std::fs::remove_dir_all(&tmp);
        t
    };
    // bad footer magic: only the footer is Invalid, no tree below it
    for (d, f) in [("corrupt-sst-bad-footer", "corrupt-sst-bad-footer.sst"), ("corrupt-sst-truncated", "corrupt-sst-truncated.sst")] {
        let t = run(d, f);
        let foot = lines_with(&t, "+ Footer");
        assert!(!foot.is_empty() && foot.iter().all(|l| l.contains("Invalid")), "{d}: {foot:?}");
        assert!(t.contains("bad table magic number"), "{d}: magic text");
    }
    // checksum flip: footer fine, exactly data block 0 Invalid (trailer checksum), neighbours Present
    let t = run("corrupt-sst-checksum-flip", "corrupt-sst-checksum-flip.sst");
    assert!(lines_with(&t, "+ Footer").iter().all(|l| l.contains("Present")));
    let bad = lines_with(&t, "+ Data block @").into_iter().filter(|l| l.contains("Invalid")).collect::<Vec<_>>();
    assert!(!bad.is_empty() && bad.iter().all(|l| l.contains("+ Data block @0 ")), "{bad:?}");
    assert!(lines_with(&t, "+ Data block @").iter().any(|l| l.contains("Present")), "other data blocks stay Present");
    assert!(lines_with(&t, "checksum |").iter().any(|l| l.contains("Invalid")), "trailer checksum row Invalid");
    // WAL checksum flip: record at 0x1e Invalid, record 0 Present
    let t = run("corrupt-wal-checksum-flip", "corrupt-wal-checksum-flip.log");
    assert!(lines_with(&t, "+ 0x1e ").iter().all(|l| l.contains("Invalid")) && !lines_with(&t, "+ 0x1e ").is_empty());
    assert!(lines_with(&t, "+ 0x0 ").iter().all(|l| l.contains("Present")) && !lines_with(&t, "+ 0x0 ").is_empty());
    // WAL truncated mid record: record at 0x40 Truncated
    let t = run("corrupt-wal-truncated", "corrupt-wal-truncated.log");
    let l = lines_with(&t, "+ 0x40 ");
    assert!(!l.is_empty() && l.iter().all(|x| x.contains("Truncated")), "{l:?}");
}

#[test]
fn codec_examples_store_the_documented_compression_type() {
    // (dir, on-disk compression type id, name shown). LZ4HC has its OWN id (5), distinct from LZ4 (4).
    for (d, id, name) in [("none", 0, "NoCompression"), ("snappy", 1, "Snappy"), ("zlib", 2, "Zlib"), ("bzip2", 3, "BZip2"), ("lz4", 4, "LZ4"), ("lz4hc", 5, "LZ4HC"), ("zstd", 7, "ZSTD")] {
        let (tmp, p) = isolate(&format!("exk-{d}"), &ex(&format!("files/sst-{d}/sst-{d}.sst")));
        let t = dump_text(&p);
        assert!(t.contains(&format!("\"{name}\"")), "{d}: properties compression {name}");
        assert!(t.contains(&format!("Compression {name} ")), "{d}: data block note");
        let tr: Vec<_> = lines_with(&t, "compression type |").into_iter().filter(|l| l.contains(&format!("| {id} {name} |"))).collect();
        assert!(tr.len() >= 16, "{d}: >=16 data block trailers with id {id}, got {}", tr.len());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

#[test]
fn bloom_and_index_scenarios_show_their_structure() {
    let sst_of = |scn: &str| -> PathBuf {
        let v: Vec<_> = std::fs::read_dir(ex(&format!("data/{scn}"))).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().map(|x| x == "sst").unwrap_or(false)).collect();
        assert_eq!(v.len(), 1, "{scn}: one SST");
        v[0].clone()
    };
    let t = dump_text(&sst_of("bloom-full"));
    assert!(t.contains("Filter block (fullfilter.rocksdb.BuiltinBloomFilter)") && !t.contains("Filter: not present"));
    let t = dump_text(&sst_of("bloom-absent"));
    assert!(t.contains("Filter: not present") && !t.contains("Filter block ("));
    assert!(t.contains("Index block (kBinarySearch)"), "the index is mandatory and still present without Bloom");
    for scn in ["bloom-partitioned", "index-partitioned"] {
        let t = dump_text(&sst_of(scn));
        assert!(t.contains("Top-level index (partitioned)") && t.contains("Partitioned filter index"), "{scn}");
        assert!(t.contains("Index partition") && t.contains("Filter partition"), "{scn}");
    }
    let t = dump_text(&sst_of("index-binary"));
    assert!(t.contains("Index block (kBinarySearch)") && !t.contains("hash index present"));
    let t = dump_text(&sst_of("index-hash"));
    assert!(t.contains("data-block hash index present"));
}

const DATA: [&str; 18] = [
    "basic-db", "bloom-full", "bloom-partitioned", "bloom-absent", "index-binary", "index-partitioned", "index-hash", "codec-none", "codec-snappy",
    "codec-zlib", "codec-bzip2", "codec-lz4", "codec-lz4hc", "codec-zstd", "multilevel-db", "wal-basic", "wal-fragmented", "wal-recycled",
];

#[test]
fn data_scenarios_current_resolves_manifest_and_every_file_dumps() {
    let on_disk: BTreeSet<String> = std::fs::read_dir(ex("data")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).filter(|n| !n.starts_with('.')).collect();
    assert_eq!(on_disk, DATA.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>());
    for scn in DATA {
        let dir = ex(&format!("data/{scn}"));
        let cur = dir.join("CURRENT");
        let target = inspect::current_target(&rocksvision::io::RangeReader::open(&cur).unwrap()).unwrap_or_else(|e| panic!("{scn}: {e}"));
        assert!(target.exists(), "{scn}: {target:?}");
        assert_eq!(target.parent().unwrap(), dir.as_path(), "{scn}");
        let mut m = inspect::open(&target).unwrap();
        assert_eq!(m.kind_label(), "MANIFEST", "{scn}");
        let t = dump_text(&target);
        assert!(!has_problem(&t), "{scn}: MANIFEST must be clean");
        // CURRENT view: referenced MANIFEST Present; observed levels list only files that exist
        let mut c = inspect::open(&cur).unwrap();
        let v = c.view(&NodeKind::Root, &never);
        let row = v.rows.iter().find(|r| r.field.name == "referenced MANIFEST").unwrap();
        assert_eq!(row.field.status, Status::Present, "{scn}");
        let lv = c.view(&NodeKind::ManifestLevels, &never);
        assert!(lv.rows.iter().all(|r| r.field.status == Status::Present), "{scn}: SST listed by MANIFEST missing on disk");
        let lv2 = m.view(&NodeKind::ManifestLevels, &never);
        assert_eq!(lv.rows.len(), lv2.rows.len(), "{scn}: CURRENT and MANIFEST levels agree");
        // every file of the directory opens and dumps (also the empty WAL); no problem status except the documented stale tail
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            assert!(inspect::open(&p).is_ok(), "{scn}: open {p:?}");
            let t = dump_text(&p);
            let ext = p.extension().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default();
            if (ext == "sst" || ext == "log") && !(scn == "wal-recycled" && ext == "log") {
                assert!(!has_problem(&t), "{scn}: {p:?} must be clean");
            }
        }
        let sst_rows = lv.rows.len();
        match scn {
            "wal-basic" | "wal-fragmented" => assert_eq!(sst_rows, 0, "{scn}: WAL-only"),
            "multilevel-db" => assert_eq!(sst_rows, 7),
            "wal-recycled" => assert_eq!(sst_rows, 6),
            _ => assert_eq!(sst_rows, 1, "{scn}"),
        }
    }
}

#[test]
fn multilevel_db_observed_levels_contain_l0_and_l1_from_the_manifest() {
    let dir = ex("data/multilevel-db");
    let mut c = inspect::open(&dir.join("CURRENT")).unwrap();
    assert!(c.children(&NodeKind::Root, &never).iter().any(|k| k.kind == NodeKind::ManifestLevels));
    let v = c.view(&NodeKind::ManifestLevels, &never);
    let mut got: Vec<(String, String)> = v.rows.iter().map(|r| (r.cells[0].clone(), r.cells[1].clone())).collect();
    got.sort();
    let want = [("0 default", "L0"), ("0 default", "L1"), ("1 cf_users", "L0"), ("1 cf_users", "L0"), ("2 cf_events", "L0"), ("2 cf_events", "L1"), ("2 cf_events", "L1")];
    let mut want: Vec<(String, String)> = want.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    want.sort();
    assert_eq!(got, want, "levels per CF as recorded by the MANIFEST (engine live_files() at generation time)");
    assert!(got.iter().any(|g| g.1 == "L0") && got.iter().any(|g| g.1 == "L1"));
    // the same through the MANIFEST file itself
    let t = dump_text(&dir.join("MANIFEST-000005"));
    assert!(t.contains("Observed levels (from MANIFEST)") && t.contains("| L1 |") && t.contains("| L0 |"));
}

#[test]
fn sha256sums_lists_every_example_file_once() {
    let sums = std::fs::read_to_string(ex("SHA256SUMS")).unwrap();
    let mut listed = BTreeSet::new();
    for l in sums.lines() {
        let (h, p) = l.split_once("  ").unwrap_or_else(|| panic!("bad line {l}"));
        assert_eq!(h.len(), 64, "{l}");
        assert!(ex(p).is_file(), "listed file missing: {p}");
        assert!(listed.insert(p.to_string()), "duplicate {p}");
    }
    fn walk(d: &Path, base: &Path, out: &mut BTreeSet<String>) {
        for e in std::fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, base, out);
            } else if !p.file_name().unwrap().to_string_lossy().starts_with('.') {
                out.insert(p.strip_prefix(base).unwrap().to_string_lossy().into_owned());
            }
        }
    }
    let mut disk = BTreeSet::new();
    walk(&ex("data"), &ex(""), &mut disk);
    walk(&ex("files"), &ex(""), &mut disk);
    assert_eq!(listed, disk, "SHA256SUMS vs files on disk");
}
