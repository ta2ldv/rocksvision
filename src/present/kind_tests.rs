//! FieldKind-driven presentation: explanations and links depend on the fact's kind (and
//! status/span), never on its display label.

use crate::model::*;

/// One WAL record payload: WriteBatch seq 7, count 1, Put(cf 0) "k" -> "v".
fn put_batch() -> crate::wal::batch::Batch {
    let mut d = 7u64.to_le_bytes().to_vec();
    d.extend_from_slice(&1u32.to_le_bytes());
    d.extend_from_slice(&[0x1, 1, b'k', 1, b'v']);
    crate::wal::batch::decode(&d, d.len() as u64, Space::Record(0), 10)
}

fn renamed(facts: &[Fact]) -> Vec<Fact> {
    facts.iter().cloned().map(|mut f| {
        f.name = format!("relabelled {}", f.name.len());
        f
    }).collect()
}

fn explanations(fields: &[Field]) -> Vec<(String, Option<NodeKind>)> {
    fields.iter().map(|f| (f.explain.clone(), f.link.clone())).collect()
}

#[test]
fn writebatch_sequence_and_key_explained_by_kind() {
    let b = put_batch();
    let hdr = super::wal::batch_fields(None, &b.header);
    let seq = hdr.iter().find(|f| f.kind == FieldKind::Sequence).expect("sequence fact");
    assert!(seq.explain.contains("WriteBatchInternal::Sequence"), "{}", seq.explain);
    let op = super::wal::batch_fields(Some(b.ops[0].tag), &b.ops[0].facts);
    let key = op.iter().find(|f| f.kind == FieldKind::Key).expect("key fact");
    assert!(key.explain.contains("User key bytes"), "{}", key.explain);
    // Renaming every label changes nothing about interpretation.
    assert_eq!(explanations(&super::wal::batch_fields(None, &renamed(&b.header))), explanations(&hdr));
    assert_eq!(explanations(&super::wal::batch_fields(Some(b.ops[0].tag), &renamed(&b.ops[0].facts))), explanations(&op));
}

#[test]
fn footer_magic_and_handles_explained_by_kind() {
    let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sst-variants/format-v5.sst");
    let s = crate::sst::SstInspector::open(crate::io::RangeReader::open(&p).unwrap());
    let ft = s.footer().expect("footer").clone();
    let fields = super::sst::footer_fields(&ft);
    let magic = fields.iter().find(|f| f.kind == FieldKind::Magic).expect("magic");
    assert!(magic.explain.contains("kBlockBasedTableMagicNumber"), "{}", magic.explain);
    let mi = fields.iter().find(|f| f.kind == FieldKind::BlockHandle(HandleTarget::Metaindex)).expect("metaindex handle");
    assert!(matches!(mi.link, Some(NodeKind::SstBlock { kind: BlockKind::Metaindex, .. })));
    let mut relabelled = ft.clone();
    relabelled.fields = renamed(&ft.fields);
    assert_eq!(explanations(&super::sst::footer_fields(&relabelled)), explanations(&fields));
}

#[test]
fn version_edit_tag_explained_by_kind() {
    // VersionEdit: LogNumber (tag 2) = 9, then an ignorable unknown tag 8250 with 1 byte.
    let d = [2u8, 9, 0xBA, 0x40, 1, 0xAA];
    let e = crate::manifest::decode_edit(&d, Space::Record(0));
    let fields = super::manifest::edit_fields(&e.fields);
    let tag = fields.iter().find(|f| f.kind == FieldKind::Tag).expect("tag");
    assert!(tag.explain.contains("VersionEdit tag"), "{}", tag.explain);
    let ig = fields.iter().find(|f| f.kind == FieldKind::IgnorableField).expect("ignorable");
    assert!(ig.explain.contains("kTagSafeIgnoreMask"), "{}", ig.explain);
    assert_eq!(explanations(&super::manifest::edit_fields(&renamed(&e.fields))), explanations(&fields));
}
