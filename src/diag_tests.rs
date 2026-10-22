//! Diagnostics as data: CRC mismatch, footer magic and truncated block/record problems are
//! reported as typed `Diagnostic`s (fixtures are opened read-only).

use crate::io::RangeReader;
use crate::model::*;
use std::path::PathBuf;

fn fx(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(rel)
}

#[test]
fn wal_crc_mismatch_is_a_crc32c_diagnostic() {
    let r = RangeReader::open(&fx("corrupt/wal-bitflip-payload.log")).unwrap();
    let l = crate::format::log::read_logical(&r, 0x1a, 1 << 20).unwrap();
    assert_eq!(l.status, Status::Invalid);
    let d = l.diag.as_ref().expect("record diagnostic");
    assert_eq!(d.check, CheckKind::Crc32c);
    assert_eq!(d.trusted, Trust::Untrusted);
    assert_eq!(d.note, l.note, "the diagnostic carries the shown message");
    assert!(d.expected.is_some() && d.observed.is_some() && d.expected != d.observed);
    assert_eq!(d.span.map(|s| s.space), Some(Space::File));
    // The presenter attaches it to the record view and the tree node.
    let mut i = crate::inspect::open(&fx("corrupt/wal-bitflip-payload.log")).unwrap();
    let v = i.view(&NodeKind::LogRecord(0x1a), &|| false);
    assert!(v.diagnostics.iter().any(|d| d.check == CheckKind::Crc32c));
    let kids = i.children(&NodeKind::LogBlock(0), &|| false);
    assert!(kids.iter().any(|c| c.diag.as_ref().map(|d| d.check) == Some(CheckKind::Crc32c)));
}

#[test]
fn bad_footer_magic_is_a_magic_diagnostic() {
    let s = crate::sst::SstInspector::open(RangeReader::open(&fx("corrupt/sst-bad-footer-magic.sst")).unwrap());
    let d = s.footer_diagnostic().expect("footer diagnostic");
    assert_eq!(d.check, CheckKind::Magic);
    assert_eq!(d.expected, Some(DecodedValue::U64(crate::sst::footer::BLOCK_BASED_MAGIC)));
    assert!(matches!(d.observed, Some(DecodedValue::U64(m)) if m != crate::sst::footer::BLOCK_BASED_MAGIC));
    assert_eq!(d.span.map(|s| s.len), Some(8));
    let mut i = crate::inspect::open(&fx("corrupt/sst-bad-footer-magic.sst")).unwrap();
    let root = i.children(&NodeKind::Root, &|| false);
    assert_eq!(root[0].diag.as_ref().map(|d| d.check), Some(CheckKind::Magic));
    assert!(i.view(&NodeKind::SstFooter, &|| false).diagnostics.iter().any(|d| d.check == CheckKind::Magic));
}

#[test]
fn truncated_block_and_record_are_truncation_diagnostics() {
    // A block handle that runs past the end of the file (as in a truncated SST).
    let mut s = crate::sst::SstInspector::open(RangeReader::open(&fx("sst-none/data.sst")).unwrap());
    let h = crate::sst::footer::Handle { offset: s.len() - 10, size: 4096 };
    let b = s.block(h, BlockKind::Data);
    assert_eq!(b.error.as_ref().map(|e| e.0), Some(Status::Truncated));
    assert_eq!(b.diags.first().map(|d| d.check), Some(CheckKind::Truncation));
    let (st, d) = s.block_check(h);
    assert_eq!(st, Status::Truncated);
    assert_eq!(d.map(|d| d.check), Some(CheckKind::Truncation));
    let v = s.view(&NodeKind::SstBlock { off: h.offset, size: h.size, kind: BlockKind::Data }, &|| false);
    assert!(v.diagnostics.iter().any(|d| d.check == CheckKind::Truncation));
    // A WAL record cut by the end of the file.
    let r = RangeReader::open(&fx("corrupt/wal-truncated-mid-record.log")).unwrap();
    let l = crate::format::log::read_logical(&r, 59, 1 << 20).unwrap();
    assert_eq!(l.status, Status::Truncated);
    assert_eq!(l.diag.as_ref().map(|d| (d.check, d.trusted)), Some((CheckKind::Truncation, Trust::Partial)));
}

#[test]
fn block_checksum_mismatch_is_a_checksum_diagnostic() {
    let mut i = crate::inspect::open(&fx("corrupt/sst-bitflip-data-block.sst")).unwrap();
    let blocks = i.children(&NodeKind::SstDataBlocks { skip: 0 }, &|| false);
    let bad = blocks.iter().find(|c| c.status == Status::Invalid).expect("corrupt data block");
    let d = bad.diag.as_ref().expect("tree diagnostic");
    assert!(matches!(d.check, CheckKind::Crc32c | CheckKind::XxHash | CheckKind::XxHash64 | CheckKind::Xxh3), "{:?}", d.check);
    assert!(d.expected != d.observed);
}
