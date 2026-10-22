//! Unit tests for scan/iteration loops: crafted inputs that could stall a non-monotonic loop
//! must finish quickly with a problem/limited status.

use crate::io::RangeReader;
use crate::model::{NodeKind, Space, Status};
use crate::format::log;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn tmp(name: &str, bytes: &[u8]) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rocksvision-unit-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

fn walk_logical(r: &RangeReader) -> usize {
    let mut off = 0;
    let mut n = 0;
    while let Some(l) = log::read_logical(r, off, 4096) {
        assert!(l.end > off, "logical cursor must advance");
        off = l.end;
        n += 1;
        assert!(n < 1_000_000);
    }
    n
}

#[test]
fn wal_all_zero_blocks_terminate() {
    let p = tmp("zeros.log", &vec![0u8; 3 * 32768 + 100]);
    let r = RangeReader::open(&p).unwrap();
    for b in 0..log::block_count(r.len()) {
        let (items, _) = log::scan_block(&r, b).unwrap();
        assert!(!items.is_empty() && items.len() < 10);
    }
    assert_eq!(walk_logical(&r), 0);
}

#[test]
fn wal_zero_length_bad_crc_headers_terminate() {
    // FULL type, length 0, wrong CRC, repeated: each item is 7 bytes and must advance.
    let mut v = Vec::new();
    while v.len() < 2 * 32768 {
        v.extend_from_slice(&[1, 2, 3, 4, 0, 0, 1]);
    }
    let p = tmp("zerolen.log", &v);
    let r = RangeReader::open(&p).unwrap();
    let (items, _) = log::scan_block(&r, 0).unwrap();
    assert!(items.iter().all(|i| i.end > i.offset));
    assert!(walk_logical(&r) > 0);
}

#[test]
fn wal_resync_garbage_is_bounded() {
    // Bytes that look like valid types with in-block lengths but never match a CRC.
    let mut v = vec![0u8; 8 * 32768];
    let mut x: u32 = 12345;
    for b in v.iter_mut() {
        x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
        *b = (x >> 16) as u8;
    }
    for i in (6..v.len()).step_by(9) {
        v[i] = 1; // FULL type everywhere
    }
    let p = tmp("garbage.log", &v);
    let r = RangeReader::open(&p).unwrap();
    let t = Instant::now();
    walk_logical(&r);
    assert!(t.elapsed() < Duration::from_secs(20), "resync too slow: {:?}", t.elapsed());
}

#[test]
fn manifest_repeated_two_byte_tags_capped() {
    // tag 202 (ColumnFamilyDrop) = varint 0xCA 0x01, no payload: 2 MiB of it.
    let d: Vec<u8> = std::iter::repeat([0xCAu8, 0x01]).take(1 << 20).flatten().collect();
    let t = Instant::now();
    let e = crate::manifest::decode_edit(&d, Space::Record(0));
    assert!(e.fields.len() <= crate::manifest::MAX_EDIT_FIELDS + 1);
    assert_eq!(e.status, Some(Status::Limited));
    let s = crate::manifest::decode_edit_summary(&d, Space::Record(0));
    assert!(s.fields.is_empty());
    assert!(t.elapsed() < Duration::from_secs(10));
}

#[test]
fn writebatch_zero_data_terminates() {
    let b = crate::wal::batch::decode(&vec![0u8; 1 << 16], 1 << 16, Space::Record(0), usize::MAX);
    assert!(b.ops.len() <= 1 << 16);
}

#[test]
fn inspector_views_on_zero_files_terminate() {
    for name in ["000001.log", "MANIFEST-000001", "000002.sst"] {
        let p = tmp(name, &vec![0u8; 5 * 32768]);
        let mut i = crate::inspect::open(&p).unwrap();
        let never = || false;
        let t = Instant::now();
        let roots = i.children(&NodeKind::Root, &never);
        i.view(&NodeKind::Root, &never);
        for c in roots {
            i.view(&c.kind, &never);
            i.children(&c.kind, &never);
        }
        assert!(t.elapsed() < Duration::from_secs(10), "{name}");
    }
}
