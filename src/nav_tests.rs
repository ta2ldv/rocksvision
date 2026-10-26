//! Task 0006 / handoff 0011 (Phase B/C): physical order, canonical paths, provenance and
//! first-use summaries on real fixtures (inspector level; the app-level selection and
//! navigation tests are in `app/state_tests.rs`).

use crate::app::provenance::{exact_file_ranges, field_at, where_lines};
use crate::inspect;
use crate::model::*;
use crate::present::common::SIZE_EXPLAIN;
use std::path::PathBuf;

fn fx(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}
fn never() -> bool {
    false
}

/// File offset of a physical node (None for views and statements).
fn start_of(k: &NodeKind) -> Option<u64> {
    match k {
        NodeKind::SstBlock { off, .. } => Some(*off),
        NodeKind::SstGap { start, .. } => Some(*start),
        _ => None,
    }
}

/// B: every SST structure appears once at its canonical place, in ascending file order, the
/// Footer last; index/metaindex entries are references, never expandable copies; every
/// canonical node's path (`path_to`) leads back to it.
#[test]
fn sst_physical_order_is_canonical() {
    for rel in ["tests/fixtures/sst-variants/partitioned.sst", "tests/fixtures/sst-zstd/data.sst", "tests/fixtures/sst-variants/rangedel.sst", "examples/files/sst-zstd/sst-zstd.sst"] {
        let mut i = inspect::open(&fx(rel)).unwrap();
        let root = i.children(&NodeKind::Root, &never);
        let phys: Vec<&Child> = root.iter().filter(|c| c.role == ChildRole::Structure).collect();
        assert_eq!(phys.last().map(|c| c.kind.clone()), Some(NodeKind::SstFooter), "{rel}: Footer last");
        // Walk the physical tree (groups expanded); collect canonical block nodes in order.
        let mut seen: Vec<NodeKind> = Vec::new();
        let mut stack: Vec<Child> = root.into_iter().rev().collect();
        while let Some(c) = stack.pop() {
            if c.role != ChildRole::Structure {
                continue;
            }
            if matches!(c.kind, NodeKind::SstBlock { .. } | NodeKind::SstGap { .. } | NodeKind::SstFooter) {
                assert!(!seen.contains(&c.kind), "{rel}: {:?} appears twice", c.kind);
                seen.push(c.kind.clone());
            }
            if let NodeKind::SstLayoutGroup { .. } = c.kind {
                let kids = i.children(&c.kind, &never);
                stack.extend(kids.into_iter().rev());
            } else if let NodeKind::SstBlock { .. } = c.kind {
                // Children of a block: references (links), restarts, trailer — no block copies.
                for k in i.children(&c.kind, &never) {
                    assert!(k.role == ChildRole::Reference || !matches!(k.kind, NodeKind::SstBlock { .. }), "{rel}: expandable copy {:?} under {:?}", k.kind, c.kind);
                }
            }
        }
        let offs: Vec<u64> = seen.iter().filter_map(start_of).collect();
        assert!(offs.windows(2).all(|w| w[0] < w[1]), "{rel}: not in ascending file order: {offs:?}");
        // Canonical path of each block ends where it is listed.
        for k in seen.iter().filter(|k| matches!(k, NodeKind::SstBlock { .. })) {
            let path = i.path_to(k, &never).unwrap_or_else(|| panic!("{rel}: no path to {k:?}"));
            let parent_kids = match path.last() {
                Some(p) => i.children(p, &never),
                None => i.children(&NodeKind::Root, &never),
            };
            assert!(parent_kids.iter().any(|c| &c.kind == k && c.role == ChildRole::Structure), "{rel}: path of {k:?} does not lead to it");
        }
    }
}

/// B: the WAL canonical path of a fragment is Physical blocks → block; a record path walks the
/// record pages; the root's view entries are labeled views.
#[test]
fn wal_canonical_paths() {
    let mut i = inspect::open(&fx("tests/fixtures/corrupt/wal-missing-last.log")).unwrap();
    let root = i.children(&NodeKind::Root, &never);
    assert_eq!((root[0].role, root[1].role), (ChildRole::Structure, ChildRole::View));
    assert!(root[1].label.starts_with("Logical records (reassembled view)"));
    let p = i.path_to(&NodeKind::LogFragment(0x10000), &never).unwrap();
    assert_eq!(p, vec![NodeKind::LogBlocks { first: 0, count: 3 }, NodeKind::LogBlock(2)]);
    let kids = i.children(&NodeKind::LogBlock(2), &never);
    assert!(kids.iter().any(|c| c.kind == NodeKind::LogFragment(0x10000)));
    // The open record is on the second record page.
    let p = i.path_to(&NodeKind::LogRecord(0x5eec), &never).unwrap();
    assert_eq!(p, vec![NodeKind::LogRecords { start: 0, page: 0 }, NodeKind::LogRecords { start: 0x3f48, page: 1 }]);
    assert!(i.children(&p[1], &never).iter().any(|c| c.kind == NodeKind::LogRecord(0x5eec)));
    // Fragment view links back to its record ("part 2 of 3").
    let v = i.view(&NodeKind::LogFragment(0x8000), &never);
    let rec = v.rows.iter().find(|r| r.field.link == Some(NodeKind::LogRecord(0x5eec))).expect("link back to the record");
    assert!(rec.field.value.starts_with("part 2 of 3 of record @0x5eec"), "{}", rec.field.value);
}

/// C: the file length is metadata: no offset/length, "file system metadata", Implicit, no
/// bytes (so no byte interpretations), the stated explanation.
#[test]
fn size_row_is_file_system_metadata() {
    for rel in ["tests/fixtures/sst-zstd/data.sst", "tests/fixtures/wal-basic/000004.log", "tests/fixtures/db-levels/MANIFEST-000005"] {
        let mut i = inspect::open(&fx(rel)).unwrap();
        let v = i.view(&NodeKind::Root, &never);
        let r = v.rows.iter().find(|r| r.cells[0] == "size").unwrap_or_else(|| panic!("{rel}: size row"));
        assert_eq!((r.field.span, r.field.status, r.field.encoding.as_str(), r.field.explain.as_str()), (None, Status::Implicit, "file system metadata", SIZE_EXPLAIN), "{rel}");
        assert_eq!((r.cells[1].as_str(), r.cells[2].as_str()), ("—", "—"), "{rel}");
    }
}

/// C: an empty WAL says empty — leaves, no Truncated/Unknown, no Diagnosis.
#[test]
fn empty_wal_is_empty_not_an_error() {
    let p = fx("tests/fixtures/empty/000001.log");
    if !p.exists() {
        // The tester adds the fixture; until then an in-memory empty file is used.
        let li = crate::wal::LogInspector::from_reader(Box::new(crate::io::MemReader::new(Vec::new())), PathBuf::from("000001.log"), crate::wal::LogMode::Wal);
        let mut i = inspect::Inspector::Log(li);
        return check_empty(&mut i);
    }
    let mut i = inspect::open(&p).unwrap();
    check_empty(&mut i);
}

fn check_empty(i: &mut inspect::Inspector) {
    let root = i.children(&NodeKind::Root, &never);
    assert_eq!(root.iter().map(|c| (c.label.as_str(), c.leaf, c.status)).collect::<Vec<_>>(), vec![("Physical blocks (0)", true, Status::NotPresent), ("Logical records: none, file is empty", true, Status::NotPresent)]);
    let v = i.view(&NodeKind::Root, &never);
    assert!(v.notes.iter().any(|n| n.text == "Empty file (0 bytes): no records. This is not an error."), "{:?}", v.notes);
    assert!(v.diagnostics.is_empty());
    assert!(v.rows.iter().all(|r| !matches!(r.field.status, Status::Truncated | Status::Unknown | Status::Invalid)), "{:?}", v.rows.iter().map(|r| (&r.field.name, r.field.status)).collect::<Vec<_>>());
    for k in [NodeKind::LogBlocks { first: 0, count: 0 }, NodeKind::LogRecords { start: 0, page: 0 }] {
        let v = i.view(&k, &never);
        assert!(v.rows.is_empty() && v.status.is_none(), "{k:?}");
    }
}

/// C: a field crossing a fragment boundary has two exact file ranges with the fragment
/// header between them unfilled; a byte of either range selects it.
#[test]
fn cross_fragment_field_has_two_file_ranges() {
    let mut i = inspect::open(&fx("tests/fixtures/wal-fragmented/000004.log")).unwrap();
    // Records that start with a FIRST fragment (fragmented records), from the physical blocks.
    let mut firsts = Vec::new();
    for b in 0..7 {
        for c in i.children(&NodeKind::LogBlock(b), &never) {
            if let (NodeKind::LogFragment(o), true) = (&c.kind, c.label.contains(" FIRST ")) {
                firsts.push(*o);
            }
        }
    }
    assert!(!firsts.is_empty());
    let mut found = false;
    for o in firsts {
        let v = i.view(&NodeKind::LogRecord(o), &never);
        for (idx, row) in v.rows.iter().enumerate() {
            let Some(s) = row.field.span.filter(|s| s.space == Space::Record(o)) else { continue };
            let ranges = exact_file_ranges(Some(&v), &s);
            if ranges.len() < 2 {
                continue;
            }
            found = true;
            assert_eq!(ranges.iter().map(|x| x.len).sum::<u64>(), s.len, "every record byte maps to one file byte");
            let gap = ranges[1].offset - (ranges[0].offset + ranges[0].len);
            assert!(gap == 7 || gap == 11, "fragment header between the ranges: {ranges:?}");
            assert!(where_lines(Some(&v), &s)[0].contains(&format!("{} ranges, fragment header [0x", ranges.len())), "{:?}", where_lines(Some(&v), &s));
            // A byte of either range selects a field of this record (the narrowest; it may be a
            // part of `row`), never a row that starts in the skipped header.
            for b in [ranges[0].offset, ranges[1].offset] {
                let at = field_at(&v, Space::File, b).expect("a field holds the byte");
                let sp = v.rows[at].field.span.unwrap();
                assert!(sp.len <= s.len || at == idx, "narrowest wins: {:?} vs {:?}", sp, s);
            }
        }
    }
    assert!(found, "a field crossing a fragment boundary exists in wal-fragmented");
}

/// C: compressed SST block contents: decompressed offset + containing compressed range, no
/// byte mapping; a click on compressed file bytes locates the block (structure), not a field.
#[test]
fn compressed_sst_provenance() {
    let mut i = inspect::open(&fx("examples/files/sst-zstd/sst-zstd.sst")).unwrap();
    let blocks = i.children(&NodeKind::SstDataBlocks { skip: 0 }, &never);
    let NodeKind::SstBlock { off, size, kind } = blocks[0].kind.clone() else { panic!() };
    let v = i.view(&NodeKind::SstBlock { off, size, kind }, &never);
    assert_eq!(v.spaces[0].map, SpaceMap::Compressed { file: Span::file(off, size), codec: "ZSTD".into() });
    let row = v.rows.iter().find(|r| r.field.span.is_some_and(|s| s.space == Space::Block(off))).expect("entry in block space");
    let w = where_lines(Some(&v), &row.field.span.unwrap());
    assert!(w[0].starts_with(&format!("Contained in compressed block: file [0x{off:x},0x{:x})", off + size)) && w[0].ends_with("no byte-to-byte mapping to file offsets"), "{w:?}");
    assert!(w[1].starts_with(&format!("Decompressed block offset: blk@0x{off:x} +0x")), "{w:?}");
    assert!(field_at(&v, Space::File, off + 1).is_none(), "no file byte maps into the decompressed block");
    let (node, at, span) = i.locate(Space::File, off + 1, &never).unwrap();
    assert_eq!((node, at, span), (NodeKind::SstBlock { off, size, kind }, None, Some(Span::file(off, size))));
    // Location header keeps stored and decompressed lengths apart.
    assert!(v.location.iter().any(|l| l.text.starts_with("decompressed ")) && v.location[0].text.starts_with(&format!("file [0x{off:x},")), "{:?}", v.location);
    // One definition per word (0011 fix C / addendum 1): "stored" = extent incl. the trailer,
    // the same N on the title, the tree label and `where:`; "contents" = stored payload;
    // "decompressed" = decompressed length; never "N B contents" for a decompressed length.
    let stored = size + 5;
    let dec = v.spaces[0].size;
    assert_eq!(v.title, format!("Data block @0x{off:x} (stored {stored} B, decompressed {dec} B)"));
    assert!(v.location[0].text.contains(&format!("stored {stored} B")), "{:?}", v.location);
    assert!(v.location[1].text.starts_with(&format!("contents [0x{off:x},0x{:x}) = {size} B", off + size)), "{:?}", v.location);
    assert!(v.spaces[0].title.contains(&format!("{size} B compressed contents -> {dec} B decompressed")), "{}", v.spaces[0].title);
    let root = i.children(&NodeKind::Root, &never);
    let label = &root.iter().find(|c| c.kind == NodeKind::SstBlock { off, size, kind }).unwrap().label;
    assert!(label.contains(&format!("0x{off:x}, {stored} B)")), "{label}");
    let all = [v.title.clone(), v.spaces[0].title.clone()].into_iter().chain(v.location.iter().map(|l| l.text.clone())).collect::<Vec<_>>().join("\n");
    assert!(!all.contains(&format!("{dec} B contents")) && !all.contains(&format!("{dec} B (0x{dec:x}) contents")), "{all}");
    // Uncompressed: one number only.
    let mut u = inspect::open(&fx("tests/fixtures/sst-none/data.sst")).unwrap();
    let b = u.children(&NodeKind::SstDataBlocks { skip: 0 }, &never);
    let NodeKind::SstBlock { off, size, kind } = b[0].kind.clone() else { panic!() };
    assert_eq!(u.view(&NodeKind::SstBlock { off, size, kind }, &never).title, format!("Data block @0x{off:x} (stored {} B)", size + 5));
}

/// C: declared compression policy and actual block compression are separate root rows; SST
/// property rows keep their spans (Present, not Implicit).
#[test]
fn declared_vs_actual_compression_and_property_spans() {
    let mut i = inspect::open(&fx("examples/files/sst-zstd/sst-zstd.sst")).unwrap();
    let v = i.view(&NodeKind::Root, &never);
    let declared = v.rows.iter().find(|r| r.field.name.starts_with("Declared compression policy")).unwrap();
    assert!(declared.field.span.is_some() && declared.field.status == Status::Present && declared.field.value.contains("ZSTD"));
    let actual = v.rows.iter().find(|r| r.field.name == "Actual block compression").unwrap();
    assert!(actual.field.value.contains(" ZSTD") && actual.field.value.contains("data blocks checked"), "{}", actual.field.value);
    let entries = v.rows.iter().find(|r| r.field.name == "entries (declared)").unwrap();
    assert_eq!(entries.field.status, Status::Present);
}
