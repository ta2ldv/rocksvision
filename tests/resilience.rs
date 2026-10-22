//! Malformed / truncated / corrupt input: no panics, honest statuses.
mod common;
use common::*;
use rocksvision::dump::{dump, DumpOpts};
use rocksvision::inspect;
use rocksvision::model::{BlockKind, NodeKind, Status};
use rocksvision::sst::SstInspector;
use std::path::Path;

fn dump_text(p: &Path) -> (bool, String) {
    let mut buf = Vec::new();
    let ok = dump(p, &DumpOpts { limit: 500, rows: 100 }, &mut buf).unwrap_or_else(|e| panic!("dump io error on {}: {e}", p.display()));
    (ok, String::from_utf8_lossy(&buf).into_owned())
}

const CORRUPT: [&str; 7] = [
    "corrupt/sst-bad-footer-magic.sst",
    "corrupt/sst-bitflip-data-block.sst",
    "corrupt/sst-snappy-bitflip-data-block.sst",
    "corrupt/sst-truncated-footer.sst",
    "corrupt/sst-truncated-mid-block.sst",
    "corrupt/wal-bitflip-payload.log",
    "corrupt/wal-truncated-mid-record.log",
];

#[test]
fn corrupt_files_open_and_dump_without_panic_and_report_problems() {
    for rel in CORRUPT {
        let p = fx(rel);
        assert!(inspect::open(&p).is_ok(), "{rel}: open");
        let (ok, text) = dump_text(&p);
        assert!(ok, "{rel}: dump returned Ok(false)");
        assert!(text.contains("Invalid") || text.contains("Truncated"), "{rel}: no Invalid/Truncated in dump");
    }
}

fn lines_with<'a>(text: &'a str, pat: &str) -> Vec<&'a str> {
    text.lines().filter(|l| l.contains(pat)).collect()
}

#[test]
fn wal_bitflip_record_never_present() {
    let (_, t) = dump_text(&fx("corrupt/wal-bitflip-payload.log"));
    // corrupted record is the one at 0x1a (payload byte at 46 flipped)
    for pat in ["+ 0x1a ", "== Physical item @0x1a [", "== Logical record @0x1a ["] {
        let ls = lines_with(&t, pat);
        assert!(!ls.is_empty(), "no line for {pat}");
        for l in ls {
            assert!(l.contains("Invalid") && !l.contains("Present"), "{l}");
        }
    }
    // table rows whose offset cell is 0x1a
    let rows: Vec<&str> = t.lines().filter(|l| l.split(" | ").any(|c| c == "0x1a")).collect();
    assert!(rows.len() >= 2);
    for l in rows {
        assert!(l.contains("Invalid") && !l.contains("Present"), "{l}");
    }
    // its neighbours stay Present
    assert!(lines_with(&t, "+ 0x3b FULL").iter().all(|l| l.contains("Present")));
    // Inspector level
    let mut i = inspect::open(&fx("corrupt/wal-bitflip-payload.log")).unwrap();
    let v = i.view(&NodeKind::LogFragment(0x1a), &never);
    assert_eq!(v.status, Some(Status::Invalid));
    let crc = v.rows.iter().find(|r| r.field.name.starts_with("checksum")).unwrap();
    assert_eq!(crc.field.status, Status::Invalid);
    let l = rocksvision::wal::log::read_logical(&rd("corrupt/wal-bitflip-payload.log"), 0x1a, 1 << 20).unwrap();
    assert_eq!(l.status, Status::Invalid);
    assert_eq!(l.frags[0].crc_ok, Some(false));
}

#[test]
fn wal_truncated_record_reported_truncated() {
    let (_, t) = dump_text(&fx("corrupt/wal-truncated-mid-record.log"));
    for pat in ["+ 0x3b ", "== Physical item @0x3b [", "== Logical record @0x3b ["] {
        let ls = lines_with(&t, pat);
        assert!(!ls.is_empty(), "no line for {pat}");
        for l in ls {
            assert!(l.contains("Truncated") && !l.contains("Present"), "{l}");
        }
    }
    let rows: Vec<&str> = t.lines().filter(|l| l.split(" | ").any(|c| c == "0x3b")).collect();
    assert!(!rows.is_empty());
    for l in rows {
        assert!(l.contains("Truncated") && !l.contains("Present"), "{l}");
    }
    let l = rocksvision::wal::log::read_logical(&rd("corrupt/wal-truncated-mid-record.log"), 59, 1 << 20).unwrap();
    assert_eq!(l.status, Status::Truncated);
    // earlier complete records unaffected
    let recs = all_logical(&rd("corrupt/wal-truncated-mid-record.log"));
    assert_eq!(recs.len(), 3);
    assert_eq!(recs[0].status, Status::Present);
    assert_eq!(recs[1].status, Status::Present);
    assert_eq!(recs[2].status, Status::Truncated);
}

#[test]
fn sst_footer_damage_reported_invalid() {
    for rel in ["corrupt/sst-bad-footer-magic.sst", "corrupt/sst-truncated-footer.sst", "corrupt/sst-truncated-mid-block.sst"] {
        let (_, t) = dump_text(&fx(rel));
        let f = lines_with(&t, "+ Footer");
        assert_eq!(f.len(), 1, "{rel}");
        assert!(f[0].contains("Invalid") && !f[0].contains("Present"), "{rel}: {}", f[0]);
        assert!(t.lines().any(|l| l.trim_start().starts_with("footer |") && l.contains("Invalid")), "{rel}: root footer row");
        // no block-level success claimed: nothing decoded as Present beyond the file size row
        assert!(!t.contains("Data block"), "{rel}: data blocks listed without a valid footer");
        let s = SstInspector::open(rd(rel));
        assert!(s.footer.is_err(), "{rel}");
        assert!(s.meta.is_err());
    }
}

/// The bit-flipped data block must be visible as Invalid wherever the dump shows it.
#[test]
fn sst_corrupt_block_checksum_invalid_in_trailer_and_notes() {
    for (rel, snappy) in [("corrupt/sst-bitflip-data-block.sst", false), ("corrupt/sst-snappy-bitflip-data-block.sst", true)] {
        let mut s = SstInspector::open(rd(rel));
        let (hs, _, _) = s.data_handles(0, 3, &never);
        let (h, _) = hs[0];
        assert_eq!(h.offset, 0);
        let b = s.block(h, BlockKind::Data);
        if snappy {
            assert!(b.error.is_some() || b.checksum_status == Status::Invalid);
        }
        assert_eq!(b.checksum_status, Status::Invalid, "{rel}: checksum_status");
        let t = s.view(&NodeKind::SstTrailer { off: h.offset, size: h.size, kind: BlockKind::Data }, &never);
        assert_eq!(t.status, Some(Status::Invalid), "{rel}");
        let ck = t.rows.iter().find(|r| r.field.name == "checksum").unwrap();
        assert_eq!(ck.field.status, Status::Invalid);
        // other blocks of the same file stay verified
        let (h2, _) = hs[1];
        assert_eq!(s.block(h2, BlockKind::Data).checksum_status, Status::Present);
        let (_, text) = dump_text(&fx(rel));
        assert!(text.lines().any(|l| l.contains("Trailer of Data block @0x0") && l.contains("Invalid")), "{rel}");
        assert!(text.contains("checksum Invalid") || snappy, "{rel}: note");
    }
}

/// Strict reading of "never claims Present for the corrupted block": the block's tree
/// node and its view status must not be Present when its checksum failed.
#[test]
fn sst_corrupt_block_node_and_view_not_present() {
    for rel in ["corrupt/sst-bitflip-data-block.sst", "corrupt/sst-snappy-bitflip-data-block.sst"] {
        let (_, text) = dump_text(&fx(rel));
        let node = lines_with(&text, "+ Data block @0 (");
        assert!(!node.is_empty(), "{rel}");
        for n in &node {
            assert!(!n.contains("Present"), "{rel}: tree node claims Present: {n}");
        }
        let mut s = SstInspector::open(rd(rel));
        let (hs, _, _) = s.data_handles(0, 1, &never);
        let (h, _) = hs[0];
        let v = s.view(&NodeKind::SstBlock { off: h.offset, size: h.size, kind: BlockKind::Data }, &never);
        assert_eq!(v.status, Some(Status::Invalid), "{rel}: block view status {:?} (entries shown Present)", v.status);
        // the dump section header of the block carries a status marker
        let hdr = lines_with(&text, "== Data block @0x0");
        assert!(hdr.iter().all(|l| l.contains("[Invalid]")), "{rel}: {hdr:?}");
    }
}

#[test]
fn nonexistent_and_text_files_error_without_panic() {
    let nope = Path::new("/nonexistent-rocksvision-dir/x.sst");
    assert!(inspect::open(nope).is_err());
    let (ok, t) = dump_text(nope);
    assert!(!ok);
    assert!(t.starts_with("ERROR"), "{t}");
    for n in ["x.log", "MANIFEST-000001", "CURRENT"] {
        assert!(inspect::open(&Path::new("/nonexistent-rocksvision-dir").join(n)).is_err());
    }
    assert!(rocksvision::io::RangeReader::open(nope).is_err());
    assert!(rocksvision::io::RangeReader::open(Path::new("/")).is_err(), "a directory is not a file");
    // MANIFEST.md (a text file) is not misidentified as a binary format
    let md = fx("MANIFEST.md");
    let i = inspect::open(&md).expect("text opens as bounded preview");
    assert!(!["WAL", "SST", "MANIFEST", "CURRENT"].contains(&i.kind_label()), "{}", i.kind_label());
    let (ok, t) = dump_text(&md);
    assert!(ok && t.contains("text preview"));
    // text content forced into each binary reader via file name: errors/problem statuses, no panic
    let d = scratch("textfiles");
    let txt = std::fs::read(&md).unwrap();
    for name in ["fake.sst", "fake.log", "MANIFEST-000007", "CURRENT"] {
        let p = d.join(name);
        std::fs::write(&p, &txt).unwrap();
        let (_, t) = dump_text(&p);
        assert!(t.contains("Invalid") || t.contains("Truncated") || t.contains("Unknown"), "{name}: text file shown without any problem status");
    }
    // the SST reader marks it Invalid
    let s = SstInspector::open(rd_abs(&d.join("fake.sst")));
    assert!(s.footer.is_err());
    let w = rocksvision::wal::log::read_logical(&rd_abs(&d.join("fake.log")), 0, 1 << 20).unwrap();
    assert_ne!(w.status, Status::Present, "text read as a Present WAL record");
    // empty files and tiny files
    for (name, bytes) in [("empty.sst", vec![]), ("empty.log", vec![]), ("one.sst", vec![1u8]), ("tiny.log", vec![0u8; 5]), ("zeros.log", vec![0u8; 40000]), ("zeros.sst", vec![0u8; 100]), ("MANIFEST-000001", vec![]), ("CURRENT", vec![])] {
        let p = d.join(name);
        std::fs::write(&p, bytes).unwrap();
        let (_, _t) = dump_text(&p);
    }
    let _ = std::fs::remove_dir_all(&d);
}

fn rd_abs(p: &Path) -> rocksvision::io::RangeReader {
    rocksvision::io::RangeReader::open(p).unwrap()
}

/// Every prefix and every single-byte flip of small fixtures: never panic.
#[test]
fn truncation_and_bitflip_sweeps_never_panic() {
    let d = scratch("sweep");
    for (src, name, step) in [("sst-variants/mixed-kinds.sst", "m.sst", 1usize), ("wal-basic/000004.log", "w.log", 1), ("wal-recycled/000011.log", "r.log", 3), ("db-levels/MANIFEST-000005", "MANIFEST-000005", 7), ("sst-variants/rangedel.sst", "rd.sst", 1)] {
        let data = std::fs::read(fx(src)).unwrap();
        let p = d.join(name);
        let mut n = 0;
        // prefixes
        for len in (0..=data.len()).step_by(step) {
            std::fs::write(&p, &data[..len]).unwrap();
            let r = std::panic::catch_unwind(|| dump_text(&p));
            assert!(r.is_ok(), "{src}: panic on prefix len {len}");
            n += 1;
        }
        // byte flips
        for pos in (0..data.len()).step_by(step.max(1)) {
            let mut c = data.clone();
            c[pos] ^= 0xff;
            std::fs::write(&p, &c).unwrap();
            let r = std::panic::catch_unwind(|| dump_text(&p));
            assert!(r.is_ok(), "{src}: panic on flip at {pos}");
            n += 1;
        }
        assert!(n > 10);
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// Oversized logical record: 100 MiB announced by FIRST/MIDDLE fragments is not allocated.
#[test]
fn oversized_logical_record_is_resource_limited() {
    use rocksvision::wal::log::{self, MAX_RECORD_BYTES};
    assert_eq!(MAX_RECORD_BYTES, 64 * 1024 * 1024);
    let d = scratch("bigrec");
    let p = d.join("000001.log");
    // 70 MiB record: FIRST + MIDDLE... + LAST with valid CRCs, payload zeros.
    let total = 70 * 1024 * 1024usize;
    let mut f = Vec::with_capacity(total + total / 1000);
    let mut left = total;
    let mut first = true;
    while left > 0 {
        let room = 32768 - (f.len() % 32768);
        assert!(room >= 7);
        let n = left.min(room - 7);
        let ty: u8 = if first { if n == left { 1 } else { 2 } } else if n == left { 4 } else { 3 };
        let mut crc_in = vec![ty];
        crc_in.extend(std::iter::repeat(0u8).take(n));
        let crc = rocksvision::util::crc_mask(rocksvision::util::crc32c(&crc_in));
        f.extend_from_slice(&crc.to_le_bytes());
        f.extend_from_slice(&(n as u16).to_le_bytes());
        f.push(ty);
        f.resize(f.len() + n, 0);
        left -= n;
        first = false;
        if room - 7 - n < 7 && left > 0 {
            let pad = 32768 - (f.len() % 32768);
            if pad < 32768 {
                f.resize(f.len() + pad, 0);
            }
        }
    }
    std::fs::write(&p, &f).unwrap();
    drop(f);
    let r = rd_abs(&p);
    let l = log::read_logical(&r, 0, usize::MAX).expect("record");
    assert_eq!(l.total_len, total as u64);
    assert!(l.data.len() <= MAX_RECORD_BYTES, "kept {} bytes", l.data.len());
    assert_eq!(l.status, Status::Limited, "{}", l.note);
    // dump terminates
    let (ok, _t) = dump_text(&p);
    assert!(ok);
    let _ = std::fs::remove_dir_all(&d);
}
