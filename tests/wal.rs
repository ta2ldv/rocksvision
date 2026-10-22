//! WAL integration tests against tests/fixtures/wal-*.
mod common;
use common::*;
use rocksvision::inspect;
use rocksvision::io::RangeReader;
use rocksvision::model::{NodeKind, Space, Status};
use rocksvision::wal::batch;
use rocksvision::wal::log::{self, FragRole, PhysKind};
use rocksvision::wal::{LogInspector, LogMode};

type ExpOp = (u64, u8, Option<u32>, &'static [u8], Option<&'static [u8]>);

fn flat_ops(r: &RangeReader) -> Vec<(u64, u8, Option<u32>, Vec<u8>, Option<Vec<u8>>, Option<u64>)> {
    let mut v = Vec::new();
    for l in all_logical(r) {
        assert_eq!(l.status, Status::Present, "record @{} {}", l.start, l.note);
        let b = batch::decode(&l.data, l.total_len, Space::Record(l.start), 1000);
        assert_eq!(b.status, Status::Present, "batch @{}: {}", l.start, b.note);
        for o in b.ops {
            assert_eq!(o.status, Status::Present);
            v.push((b.seq.unwrap(), o.tag, o.cf, o.key.clone().unwrap_or_default(), o.value.clone(), o.seq));
        }
    }
    v
}

#[test]
fn wal_basic_records_sequences_tags_cfs() {
    let r = rd("wal-basic/000004.log");
    let recs = all_logical(&r);
    assert_eq!(recs.len(), 19, "logical record count");
    assert!(recs.iter().all(|l| l.rtype == 1 && l.frag_count == 1 && l.role == FragRole::Full));
    // expected physical offsets from MANIFEST.md
    let offs: Vec<u64> = vec![0, 26, 59, 86, 120, 143, 169, 192, 217, 242, 269, 297, 328, 359, 385, 416, 443, 469, 550];
    assert_eq!(recs.iter().map(|l| l.start).collect::<Vec<_>>(), offs);

    // (derived op seq, tag, cf, key, value) in write order
    let exp: Vec<ExpOp> = vec![
        (1, 0x01, Some(0), b"k1", Some(b"v1")),
        (2, 0x01, Some(0), b"k2", Some(b"value-two")),
        (3, 0x01, Some(0), b"empty", Some(b"")),
        (4, 0x01, Some(0), b"bin\x00key", Some(b"\x00\x01\x02\xff\xfe")),
        (5, 0x00, Some(0), b"k1", None),
        (6, 0x01, Some(0), b"k3", Some(b"v3")),
        (7, 0x07, Some(0), b"k3", None),
        (8, 0x02, Some(0), b"m1", Some(b"a")),
        (9, 0x02, Some(0), b"m1", Some(b"b")),
        (10, 0x01, Some(0), b"r_b", Some(b"rb")),
        (11, 0x0f, Some(0), b"r_a", Some(b"r_z")),
        (12, 0x05, Some(1), b"c2k1", Some(b"c2v1")),
        (13, 0x05, Some(1), b"c2k2", Some(b"c2v2")),
        (14, 0x04, Some(1), b"c2k1", None),
        (15, 0x05, Some(2), b"c3k1", Some(b"c3v1")),
        (16, 0x06, Some(2), b"c3m", Some(b"x")),
        (17, 0x04, Some(2), b"c3k1", None),
        // ONE WriteBatch seq 18..23 (LogData consumes no seq)
        (18, 0x01, Some(0), b"b1", Some(b"bv1")),
        (19, 0x05, Some(1), b"b2", Some(b"bv2")),
        (20, 0x00, Some(0), b"k2", None),
        (21, 0x02, Some(0), b"m1", Some(b"c")),
        (0, 0x03, None, b"", Some(b"LOGDATA-blob-1")), // LogData: no seq
        (22, 0x0e, Some(2), b"c3a", Some(b"c3z")),
        (23, 0x05, Some(2), b"b3", Some(b"bv3")),
        // hand-encoded batch seq 24..29
        (24, 0x05, Some(1), b"hc-put1", Some(b"hc-val1")),
        (25, 0x05, Some(1), b"hc-sd", Some(b"x")),
        (26, 0x08, Some(1), b"hc-sd", None),
        (27, 0x05, Some(2), b"hc-put2", Some(b"hc-val2")),
        (28, 0x06, Some(2), b"c3m", Some(b"y")),
        (29, 0x04, Some(0), b"hc-gone", None),
    ];
    let got = flat_ops(&r);
    assert_eq!(got.len(), exp.len(), "op count");
    for (i, (g, e)) in got.iter().zip(exp.iter()).enumerate() {
        assert_eq!(g.1, e.1, "op {i} tag");
        assert_eq!(g.2, e.2, "op {i} cf");
        assert_eq!(g.3.as_slice(), e.3, "op {i} key");
        assert_eq!(g.4.as_deref(), e.4, "op {i} value");
        if e.1 == 0x03 {
            assert_eq!(g.5, None, "LogData must not consume a sequence");
        } else {
            assert_eq!(g.5, Some(e.0), "op {i} derived seq");
        }
    }
    // record-level sequences 1..29 and counts
    let heads: Vec<(u64, u32)> = recs
        .iter()
        .map(|l| {
            let b = batch::decode(&l.data, l.total_len, Space::Record(l.start), 100);
            (b.seq.unwrap(), b.count.unwrap())
        })
        .collect();
    assert_eq!(heads[0], (1, 1));
    assert_eq!(heads[16], (17, 1));
    assert_eq!(heads[17], (18, 6));
    assert_eq!(heads[18], (24, 6));
    let total: u32 = heads.iter().map(|h| h.1).sum();
    assert_eq!(total, 29);
    // known Put byte-exact: record 0 = k1=v1
    let b = batch::decode(&recs[0].data, recs[0].total_len, Space::Record(0), 10);
    assert_eq!(b.ops[0].key.as_deref(), Some(&b"k1"[..]));
    assert_eq!(b.ops[0].value.as_deref(), Some(&b"v1"[..]));
    assert_eq!(recs[0].total_len, 19);
}

#[test]
fn wal_basic_physical_field_spans_and_bytes() {
    let mut i = inspect::open(&fx("wal-basic/000004.log")).expect("open");
    let v = i.view(&NodeKind::LogFragment(0), &never);
    let by = |n: &str| v.rows.iter().find(|r| r.field.name.starts_with(n)).unwrap_or_else(|| panic!("no field {n}"));
    let crc = by("checksum");
    let len = by("length");
    let ty = by("type");
    let sp = |r: &rocksvision::model::Row| r.field.span.expect("span");
    assert_eq!((sp(crc).space, sp(crc).offset, sp(crc).len), (Space::File, 0, 4));
    assert_eq!((sp(len).space, sp(len).offset, sp(len).len), (Space::File, 4, 2));
    assert_eq!((sp(ty).space, sp(ty).offset, sp(ty).len), (Space::File, 6, 1));
    // legacy header: no log-number bytes
    let ln = by("log number");
    assert_eq!(ln.field.span, None);
    assert_eq!(ln.field.status, Status::NotPresent);
    let pl = by("payload");
    assert_eq!((sp(pl).offset, sp(pl).len), (7, 19));
    // the spans point at the real bytes
    let hdr = i.space_bytes(Space::File, 0, 7).unwrap();
    assert_eq!(u16::from_le_bytes([hdr[4], hdr[5]]), 19);
    assert_eq!(hdr[6], 1);
    let stored = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
    let payload = i.space_bytes(Space::File, 7, 19).unwrap();
    let mut crc_in = vec![1u8];
    crc_in.extend_from_slice(&payload);
    assert_eq!(rocksvision::util::crc_unmask(stored), rocksvision::util::crc32c(&crc_in));
    // status of the fragment view is Present
    assert_eq!(v.status, Some(Status::Present));
}

#[test]
fn wal_recycled_header_spans_include_log_number() {
    let mut i = inspect::open(&fx("wal-recycled/000011.log")).expect("open");
    let v = i.view(&NodeKind::LogFragment(0), &never);
    let by = |n: &str| v.rows.iter().find(|r| r.field.name.starts_with(n)).unwrap();
    let s = |n: &str| by(n).field.span.unwrap();
    assert_eq!((s("checksum").offset, s("checksum").len), (0, 4));
    assert_eq!((s("length").offset, s("length").len), (4, 2));
    assert_eq!((s("type").offset, s("type").len), (6, 1));
    assert_eq!((s("log number").offset, s("log number").len), (7, 4));
    assert_eq!(by("log number").field.value, "11");
    assert_eq!(by("log number").field.status, Status::Present);
    assert_eq!((s("payload").offset, s("payload").len), (11, 217));
    let hdr = i.space_bytes(Space::File, 0, 11).unwrap();
    assert_eq!(hdr[6], 5); // RECYCLABLE_FULL
    assert_eq!(u32::from_le_bytes([hdr[7], hdr[8], hdr[9], hdr[10]]), 11);
}

#[test]
fn wal_fragmented_reassembly_and_padding() {
    let r = rd("wal-fragmented/000004.log");
    // 100 KiB record at 0x5eec
    let l = log::read_logical(&r, 0x5eec, 1 << 26).expect("record");
    assert_eq!(l.start, 0x5eec);
    assert_eq!(l.role, FragRole::First);
    assert_eq!(l.rtype, 2);
    assert_eq!(l.frag_count, 4);
    assert_eq!(l.frags.iter().map(|f| f.rtype).collect::<Vec<_>>(), vec![2, 3, 3, 4]);
    assert_eq!(l.frags.iter().map(|f| f.len).collect::<Vec<_>>(), vec![8461, 32761, 32761, 28442]);
    assert_eq!(l.frags.iter().map(|f| f.offset).collect::<Vec<_>>(), vec![24300, 32768, 65536, 98304]);
    assert!(l.frags.iter().all(|f| f.crc_ok == Some(true) && f.header_len == 7));
    assert_eq!(l.status, Status::Present);
    assert!(l.data_complete());
    assert_eq!(l.total_len, 8461 + 32761 * 2 + 28442);
    let b = batch::decode(&l.data, l.total_len, Space::Record(l.start), 10);
    assert_eq!(b.status, Status::Present, "{}", b.note);
    assert_eq!(b.seq, Some(301));
    assert_eq!(b.ops.len(), 1);
    assert_eq!(b.ops[0].key.as_deref(), Some(&b"big-100k"[..]));
    let v = b.ops[0].value.as_ref().unwrap();
    assert_eq!(v.len(), 102_400);
    let exp = lcg_bytes(102_400);
    assert!(v == &exp, "100 KiB value is not byte-exact");
    // 40 KiB record
    let l2 = log::read_logical(&r, 139_730, 1 << 26).unwrap();
    assert_eq!(l2.frag_count, 2);
    let b2 = batch::decode(&l2.data, l2.total_len, Space::Record(l2.start), 10);
    assert_eq!(b2.ops[0].key.as_deref(), Some(&b"big-40k"[..]));
    let want: Vec<u8> = (0..40_960).map(|i| b'0' + (i % 10) as u8).collect();
    assert_eq!(b2.ops[0].value.as_deref(), Some(&want[..]));
    // all records: 1117 logical, seq contiguous 1..=1117, all Present
    let recs = all_logical(&r);
    assert_eq!(recs.len(), 1117);
    let mut next = 1u64;
    let mut pad_rec_end = None;
    for l in &recs {
        assert_eq!(l.status, Status::Present, "record @{}: {}", l.start, l.note);
        let b = batch::decode(&l.data, l.total_len, Space::Record(l.start), 5);
        assert_eq!(b.seq, Some(next));
        assert_eq!(b.status, Status::Present, "{}", b.note);
        next += u64::from(b.count.unwrap());
        if b.seq == Some(717) {
            pad_rec_end = Some(l.end);
            assert_eq!(b.ops[0].key.as_deref(), Some(&b"padkey-1"[..]));
            assert_eq!(b.ops[0].value.as_deref(), Some(&vec![b'p'; 99][..]));
        }
    }
    assert_eq!(next, 1118);
    assert_eq!(pad_rec_end, Some(196_605));
    // first record is plain FULL small value, a cf_two op exists
    let cf1 = recs.iter().flat_map(|l| batch::decode(&l.data, l.total_len, Space::Record(l.start), 5).ops).filter(|o| o.cf == Some(1)).count();
    assert_eq!(cf1, 10);
    // block-tail padding at 196605, 3 bytes, reported as BlockTrailer (Present, not a record)
    let (items, _) = log::scan_block(&r, 5).unwrap();
    let last = items.last().unwrap();
    assert_eq!((last.kind, last.offset, last.end), (PhysKind::BlockTrailer, 196_605, 196_608));
    assert_eq!(last.status(), Status::Present);
    // fragment view of the padding
    let mut i = inspect::open(&fx("wal-fragmented/000004.log")).unwrap();
    let v = i.view(&NodeKind::LogFragment(196_605), &never);
    let f = &v.rows[0].field;
    assert!(f.name.contains("padding"), "{}", f.name);
    let sp = f.span.unwrap();
    assert_eq!((sp.space, sp.offset, sp.len), (Space::File, 196_605, 3));
    // physical type counts: FIRST 3, MIDDLE 2, LAST 3
    let mut counts = std::collections::BTreeMap::new();
    for blk in 0..log::block_count(r.len()) {
        for p in log::scan_block(&r, blk).unwrap().0 {
            if p.kind == PhysKind::Record {
                *counts.entry(p.rtype).or_insert(0u32) += 1;
            }
        }
    }
    assert_eq!(counts.get(&1), Some(&1114));
    assert_eq!((counts.get(&2), counts.get(&3), counts.get(&4)), (Some(&3), Some(&2), Some(&3)));
}

#[test]
fn wal_recycled_headers_log_number_and_stale_tail() {
    // obsolete file: 6 records, clean
    let r11 = rd("wal-recycled/000011.log");
    let recs = all_logical(&r11);
    assert_eq!(recs.len(), 6);
    assert!(recs.iter().all(|l| l.status == Status::Present && l.log_number == Some(11) && l.frags[0].header_len == 11 && l.rtype == 5));
    // live file with stale tail
    let r = rd("wal-recycled/000014.log");
    let recs = all_logical(&r);
    let live: Vec<_> = recs.iter().filter(|l| l.start < 1858).collect();
    assert_eq!(live.len(), 9);
    assert!(live.iter().all(|l| l.status == Status::Present && l.log_number == Some(14) && l.frags[0].header_len == 11));
    assert_eq!(live.iter().map(|l| l.start).collect::<Vec<_>>(), vec![0, 228, 456, 684, 912, 1140, 1368, 1596, 1824]);
    // first record starting at/after 1858 is Invalid and ends where the older log resumes
    let bad = recs.iter().find(|l| l.start == 1858).expect("record at 1858");
    assert_eq!(bad.status, Status::Invalid, "{}", bad.note);
    assert_eq!(bad.end, 2052);
    // records parsed from 2052 on belong to an older log
    let tail: Vec<_> = recs.iter().filter(|l| l.start >= 2052).collect();
    assert!(!tail.is_empty());
    assert_eq!(tail[0].start, 2052);
    assert_eq!(tail[0].log_number, Some(8));

    // Inspector view: status per row (stale => not Present)
    let mut i = inspect::open(&fx("wal-recycled/000014.log")).unwrap();
    let kids = i.children(&NodeKind::Root, &never);
    let rec_node = kids.iter().find(|c| matches!(c.kind, NodeKind::LogRecords { .. })).expect("Logical records node");
    let v = i.view(&rec_node.kind, &never);
    let mut seen_1858 = false;
    for row in &v.rows {
        let off = row.field.span.unwrap().offset;
        if off < 1858 {
            assert_eq!(row.field.status, Status::Present, "row @{off}");
        } else if off == 1858 {
            seen_1858 = true;
            assert_eq!(row.field.status, Status::Invalid, "row @1858");
        } else {
            assert!(row.field.status != Status::Present, "stale row @{off} claims Present");
            assert_eq!(row.field.status, Status::Unknown, "stale row @{off}");
        }
    }
    assert!(seen_1858);
    assert!(v.rows.iter().any(|r| r.field.span.unwrap().offset == 2052));
    // block-level children (fragments) also not Present for stale ones
    let blk = i.children(&NodeKind::LogBlock(0), &never);
    for c in &blk {
        if let NodeKind::LogFragment(o) = c.kind {
            if o >= 2052 {
                assert_ne!(c.status, Status::Present, "fragment @{o}");
            }
            if o == 1858 {
                assert_eq!(c.status, Status::Invalid);
            }
        }
    }
    // file-level log number comes from file name
    let root = i.view(&NodeKind::Root, &never);
    assert!(root.rows.iter().any(|r| r.field.name.contains("log number") && r.field.value == "14"));
}

#[test]
fn wal_zstd_decodes_same_op_list() {
    let r = rd("wal-zstd/000004.log");
    let recs = all_logical(&r);
    assert_eq!(recs.len(), 42);
    assert_eq!(recs[0].rtype, 9);
    assert_eq!(recs[0].role, FragRole::Meta);
    assert_eq!(recs[0].start, 0);
    assert_eq!(u32::from_le_bytes([recs[0].data[0], recs[0].data[1], recs[0].data[2], recs[0].data[3]]), 7); // ZSTD
    let mut li = LogInspector::new(rd("wal-zstd/000004.log"), LogMode::Wal);
    assert_eq!(li.wal_compression, Some(7));
    let mut seq = 1u64;
    let mut n = 0;
    for l in recs.iter().skip(1) {
        let rec = li.record(l.start).expect("record").clone();
        assert_eq!(rec.status, Status::Present, "@{}: {}", rec.start, rec.note);
        assert!(rec.decompressed_from.is_some(), "@{} not decompressed", rec.start);
        let b = batch::decode(&rec.data, rec.total_len, Space::Record(rec.start), 10);
        assert_eq!(b.status, Status::Present, "{}", b.note);
        assert_eq!(b.seq, Some(seq));
        assert_eq!(b.ops.len(), 1);
        let o = &b.ops[0];
        assert_eq!(o.tag, 1);
        assert_eq!(o.cf, Some(0));
        if seq <= 40 {
            let idx = seq - 1;
            assert_eq!(o.key.as_deref(), Some(format!("zkey-{idx:03}").as_bytes()));
            let want = format!("zvalue-{idx:03}-{}", "compressible ".repeat(20));
            assert_eq!(o.value.as_deref(), Some(want.as_bytes()));
        } else {
            assert_eq!(o.key.as_deref(), Some(&b"zsmall"[..]));
            assert_eq!(o.value.as_deref(), Some(&b"s"[..]));
        }
        seq += 1;
        n += 1;
    }
    assert_eq!(n, 41);
    assert_eq!(seq, 42);
    // compressed vs decompressed size differ for a compressible record
    let rec = li.record(recs[1].start).unwrap();
    assert!(rec.decompressed_from.unwrap() < rec.total_len, "expected compression to shrink record 1");
}

#[test]
fn wal_basic_tree_children_ops() {
    // Tree drilling: LogRecord(550) -> 7 ops (6 counted + LogData)
    let mut i = inspect::open(&fx("wal-basic/000004.log")).unwrap();
    let ops = i.children(&NodeKind::LogRecord(469), &never);
    assert_eq!(ops.len(), 7);
    assert!(ops.iter().all(|c| c.status == Status::Present));
    let v = i.view(&NodeKind::LogRecord(469), &never);
    assert_eq!(v.status, Some(Status::Present));
    assert!(v.spaces.iter().any(|s| s.space == Space::Record(469)));
    // op fields spans are record-local
    let ov = i.view(&NodeKind::WalOp { rec: 469, idx: 0 }, &never);
    assert!(ov.rows.iter().all(|r| r.field.span.map(|s| s.space == Space::Record(469)).unwrap_or(true)));
}
