//! 0015 residual 2 (design A1–A9): unchecked blocks and layout rows never claim Present; the
//! three states (Not checked / Present / Invalid-Truncated) come from the check cache only; group
//! coverage aggregates worst-of from the cache; building the rows adds no read.

use crate::io::{MemReader, RangeRead};
use crate::model::*;
use crate::sst::footer::Handle;
use crate::sst::SstInspector;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// In-memory source that counts positional reads.
struct Counting {
    inner: MemReader,
    reads: Arc<AtomicUsize>,
}

impl RangeRead for Counting {
    fn len(&self) -> u64 {
        self.inner.len()
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.read_at(off, buf)
    }
    fn modified(&self) -> std::io::Result<std::time::SystemTime> {
        self.inner.modified()
    }
}

fn open(rel: &str) -> (SstInspector, Arc<AtomicUsize>) {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(rel);
    let reads = Arc::new(AtomicUsize::new(0));
    let r = Counting { inner: MemReader::new(std::fs::read(&p).unwrap()), reads: reads.clone() };
    (SstInspector::from_reader(Box::new(r), p), reads)
}

const NEVER: &dyn Fn() -> bool = &|| false;

fn status_cell(r: &Row) -> &str {
    r.cells.last().map(|s| s.as_str()).unwrap_or("")
}

fn group(first: u64, count: u64) -> NodeKind {
    NodeKind::SstLayoutGroup { first, count }
}

/// Unchecked data blocks are "Not checked" (Unknown + derivation), never ✔ Present; once a
/// block is opened (one bounded check) its row is ✔ Present and the group counts it.
#[test]
fn unchecked_rows_say_not_checked_and_checked_rows_present() {
    let (mut s, _) = open("sst-none/data.sst");
    let v = s.view(&group(0, 64), NEVER);
    assert_eq!(v.rows.len(), 64);
    for r in &v.rows {
        assert_eq!(status_cell(r), NOT_CHECKED, "{:?}", r.cells);
        assert_ne!(r.field.status, Status::Present);
        let d = r.field.not_checked.as_deref().expect("derivation for an unchecked row");
        assert!(d.starts_with("Not checked: the index handle places this block at [0x"), "{d}");
        assert!(d.ends_with("its checksum is verified only when the block is opened."), "{d}");
    }
    // Root summary: groups say "not checked" before the range; no row claims a block Present
    // that was not checked.
    let root = s.view(&NodeKind::Root, NEVER);
    let g = root.rows.iter().find(|r| r.field.name.starts_with("Data blocks #0–#63")).expect("group row");
    assert_eq!(g.field.name, "Data blocks #0–#63 · not checked · [0x0,0x3f780) · 64 blocks");
    assert!(g.field.not_checked.is_some() && g.field.status != Status::Present);
    // Open block #0 (the view performs the single bounded check): Present from then on.
    let b0 = s.view(&NodeKind::SstBlock { off: 0, size: 4057, kind: BlockKind::Data }, NEVER);
    assert_eq!(b0.status, None, "{:?}", b0.notes);
    let v = s.view(&group(0, 64), NEVER);
    assert_eq!(status_cell(&v.rows[0]), "✔ Present");
    assert!(v.rows[0].field.not_checked.is_none());
    assert_eq!(status_cell(&v.rows[1]), NOT_CHECKED);
    let root = s.view(&NodeKind::Root, NEVER);
    let g = root.rows.iter().find(|r| r.field.name.starts_with("Data blocks #0–#63")).unwrap();
    assert_eq!(g.field.name, "Data blocks #0–#63 · 1 of 64 checked · [0x0,0x3f780) · 64 blocks");
    assert_eq!(status_cell(g), "1 of 64 checked");
    assert!(g.field.not_checked.is_some(), "partly checked and clean: still no status claimed");
    // Layout table: every item row from the cache.
    let t = s.view(&NodeKind::SstLayout, NEVER);
    assert_eq!(t.rows.len(), 85);
    assert_eq!(status_cell(&t.rows[0]), "✔ Present");
    assert_eq!(status_cell(&t.rows[1]), NOT_CHECKED);
}

/// C2 (reviewer 3): "checksum verified" only when a checksum was compared. A kNoChecksum file's
/// opened block is Present with "block read; file has no checksum"; a checksummed file's is
/// "checksum verified".
#[test]
fn checksum_verified_only_when_compared() {
    for (rel, note, compared) in [("sst-variants/checksum-nochecksum.sst", NO_CHECKSUM, false), ("sst-none/data.sst", CHECKSUM_VERIFIED, true)] {
        let (mut s, _) = open(rel);
        let t = s.view(&NodeKind::SstLayout, NEVER);
        let first = t.rows.iter().find(|r| matches!(r.field.link, Some(NodeKind::SstBlock { kind: BlockKind::Data, .. }))).expect("data block row");
        let Some(NodeKind::SstBlock { off, size, kind }) = first.field.link.clone() else { unreachable!() };
        let _ = s.view(&NodeKind::SstBlock { off, size, kind }, NEVER);
        let h = Handle { offset: off, size };
        assert_eq!((s.checked_status(h), s.checksum_verified(h)), (Some(Status::Present), compared), "{rel}");
        let t = s.view(&NodeKind::SstLayout, NEVER);
        let row = t.rows.iter().find(|r| r.field.link == Some(NodeKind::SstBlock { off, size, kind })).unwrap();
        assert_eq!((row.field.status, row.field.check_note), (Status::Present, Some(note)), "{rel}");
        assert_eq!(row.cells.last().map(|c| c.as_str()), Some("✔ Present"), "dump cell unchanged: presence");
        let d = s.view(&NodeKind::SstDataBlocks { skip: 0 }, NEVER);
        assert_eq!(d.rows[0].field.check_note, Some(note), "{rel}: data blocks via index");
    }
}

/// Worst-of aggregation from the cache: a checked bad member gives the group its glyph; all
/// members checked clean gives ✔ "y of y checked".
#[test]
fn group_aggregation_is_worst_of_checked_members() {
    let (mut s, _) = open("corrupt/sst-bitflip-data-block.sst");
    let _ = s.view(&NodeKind::SstBlock { off: 0, size: 4057, kind: BlockKind::Data }, NEVER);
    let top = s.children(&NodeKind::Root, NEVER);
    let g = top.iter().find(|c| c.kind == group(0, 64)).expect("group child");
    assert_eq!((g.status, g.checked), (Status::Invalid, Some((1, 64))));
    assert_eq!(with_coverage(&g.label, g.checked), "Data blocks #0–#63 · 1 of 64 checked · [0x0,0x3f780) · 64 blocks");
    assert_eq!(group_glyph(g.status, g.checked), Some(Status::Invalid));
    let t = s.view(&NodeKind::SstLayout, NEVER);
    assert_eq!(status_cell(&t.rows[0]), "✖ Invalid");
    // The second group: list (check) all 16 members, then it is ✔ 16 of 16.
    let _ = s.children(&group(64, 16), NEVER);
    let top = s.children(&NodeKind::Root, NEVER);
    let g = top.iter().find(|c| c.kind == group(64, 16)).unwrap();
    assert_eq!((g.status, g.checked, group_glyph(g.status, g.checked)), (Status::Present, Some((16, 16)), Some(Status::Present)));
    let root = s.view(&NodeKind::Root, NEVER);
    let r = root.rows.iter().find(|r| r.field.name.starts_with("Data blocks #64–#79")).unwrap();
    assert_eq!((r.field.status, r.field.not_checked.is_none()), (Status::Present, true));
    // Partly checked and clean: no glyph.
    assert_eq!(group_glyph(Status::Present, Some((3, 64))), None);
    assert_eq!(group_glyph(Status::Unknown, Some((0, 64))), None);
    assert_eq!(group_glyph(Status::Truncated, Some((3, 64))), Some(Status::Truncated));
}

/// Status cells add no read: the layout table reads only its bounded Codec sample (one type
/// byte per Codec cell) and the same number again when rebuilt; the root compression line says
/// "type byte read", never "checked".
#[test]
fn status_cells_read_nothing() {
    let (mut s, reads) = open("sst-none/data.sst");
    let _ = s.layout(NEVER);
    let before = reads.load(Ordering::SeqCst);
    let t = s.view(&NodeKind::SstLayout, NEVER);
    let codec_reads = t.rows.iter().filter(|r| r.cells[7] != "-" && r.cells[7] != "not read").count();
    assert_eq!(reads.load(Ordering::SeqCst) - before, codec_reads, "layout table read more than its Codec sample");
    let before = reads.load(Ordering::SeqCst);
    let _ = s.view(&NodeKind::SstLayout, NEVER);
    assert_eq!(reads.load(Ordering::SeqCst) - before, codec_reads);
    let before = reads.load(Ordering::SeqCst);
    let _ = s.view(&group(0, 64), NEVER);
    assert_eq!(reads.load(Ordering::SeqCst) - before, 64, "one Codec type byte per member, nothing for the status");
    assert_eq!(s.checked_status(Handle { offset: 0, size: 4057 }), None, "building rows checked nothing");
    let root = s.view(&NodeKind::Root, NEVER);
    let c = root.rows.iter().find(|r| r.field.name == "Actual block compression").unwrap();
    assert_eq!(c.field.value, "80 NoCompression (type byte read for 80 of 80 data blocks)");
}
