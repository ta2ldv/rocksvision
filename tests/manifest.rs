//! CURRENT / MANIFEST integration tests against tests/fixtures/db-levels and crafted files.
mod common;
use common::*;
use rocksvision::inspect;
use rocksvision::manifest;
use rocksvision::model::{NodeKind, Space, Status};
use std::collections::BTreeMap;

#[test]
fn current_names_manifest() {
    let r = rd("db-levels/CURRENT");
    let t = inspect::current_target(&r).expect("target");
    assert_eq!(t.file_name().unwrap(), "MANIFEST-000005");
    assert!(t.exists(), "{t:?}");
    assert_eq!(manifest::parse_current(b"MANIFEST-000005\n").unwrap(), "MANIFEST-000005");
    assert!(manifest::parse_current(b"MANIFEST-000005").is_err(), "missing newline");
    assert!(manifest::parse_current(b"../MANIFEST-000005\n").is_err());
    assert!(manifest::parse_current(b"\xff\xfe\n").is_err());
    assert!(manifest::parse_current(b"").is_err());
    // inspector view
    let mut i = inspect::open(&fx("db-levels/CURRENT")).unwrap();
    assert_eq!(i.kind_label(), "CURRENT");
    let v = i.view(&NodeKind::Root, &never);
    let row = v.rows.iter().find(|r| r.field.name == "referenced MANIFEST").unwrap();
    assert_eq!(row.field.value, "MANIFEST-000005");
    assert_eq!(row.field.status, Status::Present);
    assert!(row.field.open_path.as_ref().unwrap().ends_with("MANIFEST-000005"));
    // every fixture dir's CURRENT names an existing MANIFEST
    for d in ["wal-basic", "wal-fragmented", "wal-recycled", "wal-zstd", "db-levels"] {
        let t = inspect::current_target(&rd(&format!("{d}/CURRENT"))).unwrap();
        assert!(t.exists(), "{d}: {t:?}");
    }
}

#[test]
fn current_pointing_to_missing_manifest_is_not_present() {
    let d = scratch("cur-missing");
    std::fs::write(d.join("CURRENT"), b"MANIFEST-000099\n").unwrap();
    let mut i = inspect::open(&d.join("CURRENT")).unwrap();
    let v = i.view(&NodeKind::Root, &never);
    let row = v.rows.iter().find(|r| r.field.name == "referenced MANIFEST").unwrap();
    assert_eq!(row.field.status, Status::NotPresent);
    let lv = i.view(&NodeKind::ManifestLevels, &never);
    assert!(lv.status.is_none() || lv.status == Some(Status::NotPresent));
    assert_eq!(lv.rows[0].field.status, Status::NotPresent);
    let _ = std::fs::remove_dir_all(&d);
}

fn live_table() -> BTreeMap<(&'static str, u32), Vec<(u64, u64, &'static str, &'static str)>> {
    // (cf, level) -> [(file number, size, smallest user key, largest user key)]
    let mut m = BTreeMap::new();
    m.insert(("cf_events", 0), vec![(26, 1390, "e-00005", "e-00600")]);
    m.insert(("cf_events", 1), vec![(22, 4601, "e-00000", "e-00249"), (24, 4602, "e-00250", "e-00499")]);
    m.insert(("cf_users", 0), vec![(16, 2936, "u-00000", "u-00199"), (18, 2948, "u-00100", "u-00299"), (20, 2033, "u-00500", "u-00599")]);
    m.insert(("default", 0), vec![(14, 2751, "d-00400", "d-00499")]);
    m.insert(("default", 1), vec![(12, 7481, "d-00000", "d-00449")]);
    m
}

#[test]
fn manifest_observed_levels_match_live_files() {
    let r = rd("db-levels/MANIFEST-000005");
    let lv = manifest::observed_levels(&r, &never);
    assert!(lv.problems.is_empty(), "{:?}", lv.problems);
    assert!(!lv.limited);
    assert_eq!(lv.comparator.as_deref(), Some("leveldb.BytewiseComparator"));
    let mut got: BTreeMap<(String, u32), Vec<(u64, u64, String, String)>> = BTreeMap::new();
    for (name, levels) in lv.cfs.values() {
        for (level, files) in levels {
            for lf in files {
                let (su, _, _) = manifest::split_ikey(&lf.f.smallest).unwrap();
                let (lu, _, _) = manifest::split_ikey(&lf.f.largest).unwrap();
                got.entry((name.clone(), *level)).or_default().push((lf.f.number, lf.f.size, String::from_utf8_lossy(su).into(), String::from_utf8_lossy(lu).into()));
            }
        }
    }
    let exp = live_table();
    assert_eq!(got.len(), exp.len(), "{:?}", got.keys().collect::<Vec<_>>());
    for ((cf, level), files) in &exp {
        let mut g = got.get(&(cf.to_string(), *level)).unwrap_or_else(|| panic!("missing {cf} L{level}")).clone();
        g.sort();
        let mut e: Vec<_> = files.iter().map(|(n, s, a, b)| (*n, *s, a.to_string(), b.to_string())).collect();
        e.sort();
        assert_eq!(g, e, "{cf} L{level}");
    }
    let total: usize = lv.cfs.values().map(|(_, l)| l.values().map(|f| f.len()).sum::<usize>()).sum();
    assert_eq!(total, 8);
    // CF ids: default is 0, the others were created in order
    assert_eq!(lv.cfs[&0].0, "default");
    let names: Vec<&str> = lv.cfs.values().map(|c| c.0.as_str()).collect();
    assert!(names.contains(&"cf_users") && names.contains(&"cf_events"));
    // overlap facts: cf_users L0 16 & 18 overlap each other, 20 does not; default L0 overlaps its L1
    let users = lv.cfs.values().find(|c| c.0 == "cf_users").unwrap();
    let ov: BTreeMap<u64, u32> = users.1[&0].iter().map(|f| (f.f.number, f.overlap_same)).collect();
    assert_eq!((ov[&16], ov[&18], ov[&20]), (1, 1, 0));
    let def = &lv.cfs[&0].1;
    assert_eq!(def[&0][0].overlap_next, 1);
    assert_eq!(def[&1][0].overlap_same, 0);
}

#[test]
fn current_view_levels_rows_and_manifest_records() {
    let mut i = inspect::open(&fx("db-levels/CURRENT")).unwrap();
    let kids = i.children(&NodeKind::Root, &never);
    assert!(kids.iter().any(|c| c.kind == NodeKind::ManifestLevels));
    let v = i.view(&NodeKind::ManifestLevels, &never);
    assert_eq!(v.rows.len(), 8);
    let mut cells: Vec<(String, String, String)> = v.rows.iter().map(|r| (r.cells[0].clone(), r.cells[1].clone(), r.cells[2].clone())).collect();
    cells.sort();
    let want = vec![
        ("0 default", "L0", "000014"),
        ("0 default", "L1", "000012"),
    ];
    for (a, b, c) in want {
        assert!(cells.iter().any(|x| x.0 == a && x.1 == b && x.2 == c), "missing {a} {b} {c}: {cells:?}");
    }
    for (cf, level, files) in live_table().into_iter().map(|((cf, l), f)| (cf, l, f)) {
        for (n, ..) in files {
            assert!(cells.iter().any(|x| x.0.ends_with(cf) && x.1 == format!("L{level}") && x.2 == format!("{n:06}")), "{cf} L{level} {n}");
        }
    }
    // all referenced files exist in the fixture dir
    assert!(v.rows.iter().all(|r| r.field.status == Status::Present && r.field.open_path.is_some()));
    // MANIFEST opened directly: same levels, and its records decode with Present status
    let mut m = inspect::open(&fx("db-levels/MANIFEST-000005")).unwrap();
    assert_eq!(m.kind_label(), "MANIFEST");
    let lv2 = m.view(&NodeKind::ManifestLevels, &never);
    assert_eq!(lv2.rows.len(), 8);
    let kids = m.children(&NodeKind::Root, &never);
    let recs = kids.iter().find(|c| matches!(c.kind, NodeKind::LogRecords { .. })).unwrap();
    let rv = m.view(&recs.kind, &never);
    assert!(rv.rows.len() >= 5, "records {}", rv.rows.len());
    assert!(rv.rows.iter().all(|r| r.field.status == Status::Present), "{:?}", rv.rows.iter().map(|r| r.field.status).collect::<Vec<_>>());
    // first record's fields all carry record-space spans
    let first = rv.rows[0].field.span.unwrap().offset;
    let one = m.view(&NodeKind::LogRecord(first), &never);
    assert!(one.rows.iter().any(|r| r.field.name.starts_with("tag ")));
    assert!(one.rows.iter().filter(|r| r.field.name.starts_with("tag ")).all(|r| r.field.span.unwrap().space == Space::Record(first)));
}

fn crafted_manifest() -> Vec<Vec<u8>> {
    let mut r1 = Vec::new();
    varint(1, &mut r1);
    lp(b"leveldb.BytewiseComparator", &mut r1);
    varint(2, &mut r1);
    varint(0, &mut r1);
    varint(3, &mut r1);
    varint(10, &mut r1);
    varint(4, &mut r1);
    varint(5, &mut r1);
    // unknown tag WITH safe-ignore bit (8192 | 58), length-prefixed payload
    varint(8250, &mut r1);
    lp(b"ab", &mut r1);
    // NewFile2 (tag 100): L0, file 9, size 123, [a,b], seq 1..2
    let mut r2 = Vec::new();
    varint(100, &mut r2);
    varint(0, &mut r2);
    varint(9, &mut r2);
    varint(123, &mut r2);
    lp(&ikey(b"a", 1, 1), &mut r2);
    lp(&ikey(b"b", 2, 1), &mut r2);
    varint(1, &mut r2);
    varint(2, &mut r2);
    // record with an unknown tag WITHOUT the ignore bit (77) followed by opaque bytes
    let mut r3 = Vec::new();
    varint(77, &mut r3);
    r3.extend_from_slice(&[1, 2, 3, 4]);
    vec![r1, r2, r3]
}

#[test]
fn manifest_unknown_tags_are_unknown_not_absent() {
    let recs = crafted_manifest();
    // direct decode
    let e = manifest::decode_edit(&recs[0], Space::Record(0));
    assert_eq!(e.comparator.as_deref(), Some("leveldb.BytewiseComparator"));
    assert_eq!(e.status, None, "ignorable unknown tag must not poison the edit");
    let ig = e.fields.iter().find(|f| f.name == "ignorable field").expect("ignorable field row");
    assert_eq!(ig.status, Status::Unknown);
    assert!(e.fields.iter().all(|f| f.status != Status::NotPresent), "NotPresent on a field of a decodable edit with unknown tag");
    assert!(e.fields.iter().any(|f| f.name.starts_with("tag ") && f.value == "8250"));
    let e3 = manifest::decode_edit(&recs[2], Space::Record(0));
    assert_eq!(e3.status, Some(Status::Unknown));
    let rem = e3.fields.iter().find(|f| f.name == "undecodable remainder").unwrap();
    assert_eq!(rem.status, Status::Unknown);
    let sp = rem.span.unwrap();
    assert_eq!((sp.offset, sp.len), (1, 4));
    assert!(e3.fields.iter().all(|f| f.status != Status::NotPresent));
    // full pipeline through a MANIFEST file in a temp dir
    let d = scratch("manifest-unknown");
    let p = d.join("MANIFEST-000001");
    write_small_log(&p, &recs);
    let r = rocksvision::io::RangeReader::open(&p).unwrap();
    let lv = manifest::observed_levels(&r, &never);
    assert_eq!(lv.cfs[&0].1[&0].len(), 1);
    assert_eq!(lv.cfs[&0].1[&0][0].f.number, 9);
    let mut i = inspect::open(&p).unwrap();
    let kids = i.children(&NodeKind::Root, &never);
    let recs_node = kids.iter().find(|c| matches!(c.kind, NodeKind::LogRecords { .. })).unwrap();
    let rv = i.view(&recs_node.kind, &never);
    assert_eq!(rv.rows.len(), 3);
    for r in &rv.rows {
        assert_ne!(r.field.status, Status::NotPresent);
    }
    let v3 = i.view(&NodeKind::LogRecord(rv.rows[2].field.span.unwrap().offset), &never);
    assert!(v3.rows.iter().all(|r| r.field.status != Status::NotPresent));
    assert!(v3.rows.iter().any(|r| r.field.status == Status::Unknown));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn split_ikey_roundtrip() {
    let k = ikey(b"hello", 77, 2);
    assert_eq!(manifest::split_ikey(&k), Some((&b"hello"[..], 77, 2)));
    assert_eq!(manifest::split_ikey(&[1, 2, 3]), None);
    assert_eq!(manifest::split_ikey(&[0u8; 8]), Some((&b""[..], 0, 0)));
}
