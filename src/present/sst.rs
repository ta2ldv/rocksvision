//! SST presentation: lazy tree children and views (tables, explanations, derived-space titles)
//! built from the typed state of `sst::SstInspector`.

use super::common::{error_view, explained, of_fields, st_cell, with_error_diag};
use crate::format::codec::compression_name;
use crate::format::ikey::{split_ikey, value_type_name as tag_name, MAX_SEQUENCE};
use crate::limits::{RESTARTS_LISTED, SST_DATA_PAGE as DATA_PAGE, SST_TREE_CHILDREN as TREE_CHILD_CAP};
use crate::model::*;
use crate::model::{DecodedValue as DV, FieldKind as K, HandleTarget as HT};
use crate::sst::block::{Block, Entry};
use crate::sst::footer::{self, Footer, Handle, BLOCK_TRAILER};
use crate::sst::{index_type_name, meta_kind, prop_value, MetaEntry, PropValue, SstInspector};
use crate::util::{bytes_preview, fixed32, hex_preview, human_size};

/// Display value and encoding of a table property.
fn prop_decode(name: &str, v: &[u8]) -> (String, &'static str) {
    match prop_value(name, v) {
        PropValue::U64(x) => (x.to_string(), "varint64"),
        PropValue::BadU64 => (format!("expected varint64: {}", hex_preview(v, 16)), "invalid"),
        PropValue::IndexType(Some(t)) => (format!("{t} {}", index_type_name(t)), "fixed32 LE"),
        PropValue::IndexType(None) => (hex_preview(v, 16), "bytes"),
        PropValue::Fixed64(x) => (x.map(|x| x.to_string()).unwrap_or_default(), "fixed64 LE"),
        PropValue::Fixed32(x) => (x.map(|x| x.to_string()).unwrap_or_default(), "fixed32 LE"),
        PropValue::Flag => (bytes_preview(v, 8), "string \"0\"/\"1\""),
        PropValue::Bytes => (bytes_preview(v, 160), "bytes/string"),
    }
}

/// Field of a table property with its typed decoded value (display text from `prop_decode`).
fn prop_field(label: &str, name: &str, v: &[u8]) -> Field {
    let f = Field::new(label, None, prop_decode(name, v).0);
    match prop_value(name, v) {
        PropValue::U64(x) => f.dv(DV::VarInt(x)),
        PropValue::IndexType(Some(t)) => f.dv(DV::Enum { name: index_type_name(t), code: u64::from(t) }),
        PropValue::Fixed64(Some(x)) => f.dv(DV::U64(x)),
        PropValue::Fixed32(Some(x)) => f.dv(DV::U32(x)),
        _ => f,
    }
}

fn space_info(b: &Block) -> Vec<SpaceInfo> {
    if b.compressed {
        vec![SpaceInfo {
            space: Space::Block(b.handle.offset),
            size: b.data.len() as u64,
            title: format!("Block-local offsets (decompressed {} block @0x{:x}: {} B stored -> {} B)", compression_name(b.ctype), b.handle.offset, b.handle.size, b.data.len()),
        }]
    } else {
        Vec::new()
    }
}

/// Footer facts with explanations and navigation (table/format.cc layouts).
pub fn footer_fields(ft: &Footer) -> Vec<Field> {
    let ct = ft.checksum_type;
    let metaindex = NodeKind::SstBlock { off: ft.metaindex.offset, size: ft.metaindex.size, kind: BlockKind::Metaindex };
    ft.fields
        .iter()
        .cloned()
        .map(|f| {
            let stored = f.span.is_some();
            // Selected by kind, never by the display label.
            match f.kind {
                K::Magic => explained(f, "kBlockBasedTableMagicNumber; the legacy value implies format_version 0 and CRC32c."),
                K::ChecksumType if stored => explained(f, "Checksum algorithm for every block trailer (and the footer in fv>=6)."),
                K::ExtendedMagic => explained(f, "kExtendedMagic; marks the format_version>=6 part2 layout."),
                K::Checksum(ChecksumRole::Stored) if stored => {
                    let computed = ft.footer_checksum.map(|x| x.1);
                    let ok = ct == 0 || ft.footer_checksum.map(|(s, c)| s == c).unwrap_or(false);
                    let e = format!(
                        "{} of the 53-byte footer with this field zeroed, plus ChecksumModifierForContext(base, footer offset {}). Computed: {}.",
                        footer::checksum_name(ct),
                        ft.offset,
                        computed.map(|c| format!("0x{c:08x} -> {}", if ok { "match" } else { "MISMATCH" })).unwrap_or("n/a".into())
                    );
                    explained(f, e)
                }
                K::BaseContext => explained(f, "Per-file random value; every block checksum adds base ^ (lo32(offset) + hi32(offset)) so misplaced blocks fail verification. RocksDB rejects 0 for format_version >= 6."),
                // The only stored length of the footer: the metaindex size (format_version >= 6).
                K::MetaindexSize => explained(f, "Metaindex block is assumed to end right before the footer (minus its 5-byte trailer)."),
                K::BlockHandle(HT::Metaindex) if !stored => explained(f, "Derived: footer offset − 5 − metaindex size. Not stored as a handle.").link(metaindex.clone()),
                K::BlockHandle(HT::Metaindex) if !ft.legacy => Field::from(f).link(metaindex.clone()),
                K::BlockHandle(HT::Index) if f.status == Status::NotPresent => explained(f, "format_version>=6 does not store the index handle in the footer (FormatVersionUsesIndexHandleInFooter)."),
                K::FormatVersion if stored => explained(f, "Block-based table format_version (footer version). 10.4.2 writes 6 by default and reads 0..7."),
                _ => Field::from(f),
            }
        })
        .collect()
}

fn entry_explain(e: &Entry, b: &Block) -> String {
    format!(
        "Entry at {} 0x{:x}: header {} B (shared={}, non_shared={}{}), {} key bytes stored, key = {} shared + {} stored bytes; value {} B.{}",
        if b.compressed { "block-local" } else { "file" },
        if b.compressed { e.off as u64 } else { b.handle.offset + e.off as u64 },
        e.hdr_len,
        e.shared,
        e.non_shared,
        e.value_len_field.map(|v| format!(", value_length={v}")).unwrap_or_else(|| ", no value_length (delta index)".into()),
        e.non_shared,
        e.shared,
        e.non_shared,
        e.value_len,
        if e.restart { " Restart point." } else { "" }
    )
}

impl SstInspector {
    // ---------------- tree ----------------

    pub fn children(&mut self, k: &NodeKind, cancel: &dyn Fn() -> bool) -> Vec<Child> {
        match k {
            NodeKind::Root => self.root_children(),
            NodeKind::SstBlock { off, size, kind } => {
                let h = Handle { offset: *off, size: *size };
                let mut out = Vec::new();
                let structured = !matches!(kind, BlockKind::Filter | BlockKind::FilterPartition | BlockKind::CompressionDict | BlockKind::Meta);
                out.push(Child::new("Trailer (compression + checksum)", NodeKind::SstTrailer { off: *off, size: *size, kind: *kind }).leaf());
                if structured {
                    out.push(Child::new("Restart array / block footer", NodeKind::SstRestarts { off: *off, size: *size, kind: *kind }).leaf());
                }
                let target = match kind {
                    BlockKind::Index | BlockKind::IndexPartition => Some(BlockKind::Data),
                    BlockKind::TopIndex => Some(BlockKind::IndexPartition),
                    BlockKind::FilterIndex => Some(BlockKind::FilterPartition),
                    _ => None,
                };
                if *kind == BlockKind::Metaindex {
                    if let Ok(m) = self.meta() {
                        for e in m {
                            out.push(Child::new(format!("{} @{}", e.name, e.handle.offset), NodeKind::SstBlock { off: e.handle.offset, size: e.handle.size, kind: self.meta_kind_for(&e.name) }));
                        }
                    }
                }
                if let Some(t) = target {
                    let (_, res) = self.block_entries(h, *kind);
                    if let Ok((_, es, _)) = res {
                        let n = es.len();
                        for e in es.iter().take(TREE_CHILD_CAP) {
                            if let Some((th, _, _)) = &e.handle {
                                let (st, d) = self.block_check(*th);
                                out.push(Child::new(format!("{} @{} ({} B) ≤ {}", t.label(), th.offset, th.size, self.key_label(&e.key, true)), NodeKind::SstBlock { off: th.offset, size: th.size, kind: t }).status(st).diag(d));
                            }
                        }
                        if n > TREE_CHILD_CAP {
                            out.push(Child::new(format!("{} more — use \"Data blocks\" paging", n - TREE_CHILD_CAP), NodeKind::SstDataBlocks { skip: TREE_CHILD_CAP as u64 }).status(Status::Limited));
                        }
                    }
                }
                out
            }
            NodeKind::SstDataBlocks { skip } => {
                let (hs, more, _) = self.data_handles(*skip, DATA_PAGE, cancel);
                let mut out: Vec<Child> = Vec::with_capacity(hs.len() + 1);
                for (i, (h, k)) in hs.iter().enumerate() {
                    let (st, d) = self.block_check(*h);
                    out.push(Child::new(format!("#{} @{} ({} B) ≤ {}", *skip as usize + i, h.offset, h.size, self.key_label(k, true)), NodeKind::SstBlock { off: h.offset, size: h.size, kind: BlockKind::Data }).status(st).diag(d));
                }
                if more {
                    out.push(Child::new(format!("Next {} data blocks ›", DATA_PAGE), NodeKind::SstDataBlocks { skip: skip + hs.len() as u64 }));
                }
                out
            }
            _ => Vec::new(),
        }
    }

    fn root_children(&self) -> Vec<Child> {
        let mut v = vec![Child::new("Footer", NodeKind::SstFooter).leaf().status(if self.footer().is_ok() { Status::Present } else { Status::Invalid }).diag(self.footer_diagnostic().cloned())];
        let Ok(f) = self.footer() else { return v };
        v.push(Child::new(format!("Metaindex @{}", f.metaindex.offset), NodeKind::SstBlock { off: f.metaindex.offset, size: f.metaindex.size, kind: BlockKind::Metaindex }).status(if self.meta().is_ok() { Status::Present } else { Status::Invalid }));
        let unknown_meta = self.meta().is_err();
        let absent = |label: &str| Child::new(format!("{label}: {}", if unknown_meta { "unknown (metaindex unreadable)" } else { "not present" }), NodeKind::SstProperties).leaf().status(if unknown_meta { Status::Unknown } else { Status::NotPresent });
        match self.props_handle() {
            Some(h) => v.push(Child::new("Properties", NodeKind::SstBlock { off: h.offset, size: h.size, kind: BlockKind::Properties })),
            None => v.push(absent("Properties")),
        }
        match self.index_handle() {
            Some(h) => {
                let k = self.index_kind();
                v.push(Child::new(format!("{} ({})", k.label(), index_type_name(self.index_type())), NodeKind::SstBlock { off: h.offset, size: h.size, kind: k }));
                v.push(Child::new("Data blocks (via index)", NodeKind::SstDataBlocks { skip: 0 }));
            }
            None => v.push(absent("Index")),
        }
        let meta = self.meta().map(|m| m.to_vec()).unwrap_or_default();
        let filters: Vec<&MetaEntry> = meta.iter().filter(|m| matches!(meta_kind(&m.name), BlockKind::Filter | BlockKind::FilterIndex)).collect();
        let legacy_filter = meta.iter().any(|m| m.is_legacy_filter());
        if filters.is_empty() && !legacy_filter {
            v.push(absent("Filter"));
        }
        for m in filters {
            let k = meta_kind(&m.name);
            v.push(Child::new(format!("{} ({})", k.label(), m.name), NodeKind::SstBlock { off: m.handle.offset, size: m.handle.size, kind: k }));
        }
        for (name, label) in [("rocksdb.range_del", "Range deletions"), ("rocksdb.compression_dict", "Compression dictionary")] {
            match self.meta_handle(name) {
                Some(h) => v.push(Child::new(label, NodeKind::SstBlock { off: h.offset, size: h.size, kind: meta_kind(name) })),
                None => v.push(absent(label)),
            }
        }
        for m in meta.iter().filter(|m| meta_kind(&m.name) == BlockKind::Meta) {
            let (label, st) = if m.is_legacy_filter() {
                (format!("Legacy block-based filter {} (unsupported)", m.name), Status::Unsupported)
            } else {
                (format!("Meta block {}", m.name), Status::Unknown)
            };
            v.push(Child::new(label, NodeKind::SstBlock { off: m.handle.offset, size: m.handle.size, kind: BlockKind::Meta }).status(st));
        }
        v
    }

    fn key_label(&self, k: &[u8], index: bool) -> String {
        if index && self.index_key_is_user() {
            bytes_preview(k, 32)
        } else {
            match split_ikey(k) {
                Some((u, s, t)) => format!("{} @{} {}", bytes_preview(u, 32), s, t),
                None => bytes_preview(k, 32),
            }
        }
    }

    // ---------------- views ----------------

    pub fn view(&mut self, k: &NodeKind, cancel: &dyn Fn() -> bool) -> View {
        match k {
            NodeKind::Root => self.root_view(),
            NodeKind::SstFooter => match self.footer() {
                Ok(f) => {
                    let mut v = of_fields(format!("Footer @0x{:x} ({} B, format_version {})", f.offset, f.len, f.format_version), footer_fields(f));
                    v.notes.push("Footer layout: table/format.cc (legacy 48 B, format_version>=1 53 B).".into());
                    v.diagnostics.extend(f.fields.iter().filter_map(|x| x.diag.as_deref().cloned()));
                    v
                }
                Err(e) => {
                    with_error_diag(error_view("Footer", Status::Invalid, e), self.footer_diagnostic().cloned())
                }
            },
            NodeKind::SstProperties => of_fields("Not present", vec![Field::new("structure", None, "This optional structure is not listed in the metaindex.").status(if self.meta().is_ok() { Status::NotPresent } else { Status::Unknown })]),
            NodeKind::SstBlock { off, size, kind } => self.block_view(Handle { offset: *off, size: *size }, *kind),
            NodeKind::SstTrailer { off, size, kind } => self.trailer_view(Handle { offset: *off, size: *size }, *kind),
            NodeKind::SstRestarts { off, size, kind } => self.restarts_view(Handle { offset: *off, size: *size }, *kind),
            NodeKind::SstEntry { off, size, kind, idx } => self.entry_view(Handle { offset: *off, size: *size }, *kind, *idx),
            NodeKind::SstDataBlocks { skip } => self.data_blocks_view(*skip, cancel),
            _ => error_view("Unsupported node", Status::Unsupported, "node not handled by the SST inspector"),
        }
    }

    fn root_view(&self) -> View {
        let len = self.len();
        let mut f = vec![Field::new("file", None, self.path().display().to_string()), Field::new("size", Some(Span::file(0, len)), human_size(len)).dv(DV::U64(len))];
        match self.footer() {
            Ok(ft) => {
                f.push(Field::new("table format", None, "BlockBasedTable"));
                let fv_span = if ft.legacy { None } else { Some(Span::file(ft.offset + ft.len - 12, 4)) };
                let fv_field = Field::new("format_version", fv_span, ft.format_version.to_string()).kind(K::FormatVersion).dv(DV::U32(ft.format_version)).link(NodeKind::SstFooter);
                f.push(if ft.legacy { fv_field.status(Status::Implicit).explain("Inferred from the legacy magic number; not stored.") } else { fv_field });
                f.push(Field::new("checksum type", None, footer::checksum_name(ft.checksum_type)).kind(K::ChecksumType).dv(DV::Enum { name: footer::checksum_name(ft.checksum_type), code: u64::from(ft.checksum_type) }).link(NodeKind::SstFooter));
            }
            Err(e) => f.push(Field::new("footer", None, e).status(Status::Invalid).diag(self.footer_diagnostic().cloned())),
        }
        f.push(Field::new("level", None, "unknown for a standalone SST (only a MANIFEST records levels)").status(Status::Unknown));
        if self.has_props() {
            for (k, label) in [
                ("rocksdb.creating.db.identity", "producer DB id"),
                ("rocksdb.column.family.name", "column family"),
                ("rocksdb.compression", "compression"),
                ("rocksdb.num.entries", "entries"),
                ("rocksdb.num.data.blocks", "data blocks"),
                ("rocksdb.filter.policy", "filter policy"),
                ("rocksdb.comparator", "comparator"),
                ("rocksdb.block.based.table.index.type", "index type"),
                ("rocksdb.index.value.is.delta.encoded", "index values delta-encoded"),
                ("rocksdb.index.key.is.user.key", "index keys are user keys"),
            ] {
                match self.prop(k) {
                    Some(v) => f.push(prop_field(label, k, v).explain(format!("From property {k}."))),
                    None => f.push(Field::new(label, None, "property not stored").status(Status::NotPresent)),
                }
            }
        } else if self.props_handle().is_some() {
            f.push(Field::new("properties", None, "properties block listed but could not be decoded; index flags unknown").status(Status::Invalid));
        } else {
            f.push(Field::new("properties", None, "not listed in the metaindex").status(if self.meta().is_ok() { Status::NotPresent } else { Status::Unknown }));
        }
        if self.changed_since_open() {
            f.push(Field::new("file changed", None, "size or mtime differs from open time; reopen").status(Status::Unknown));
        }
        let mut v = of_fields("SST file (block-based table)", f);
        v.notes.push("Compatibility target: RocksDB 10.4.2. Files from other versions decode only where layouts match.".into());
        v
    }

    fn block_view(&mut self, h: Handle, kind: BlockKind) -> View {
        match kind {
            BlockKind::Filter | BlockKind::FilterPartition => return self.filter_view(h, kind),
            BlockKind::CompressionDict | BlockKind::Meta => {
                let b = self.block(h, kind);
                let mut f = vec![Field::new("contents", Some(b.span(0, b.data.len())), format!("{} bytes: {}", b.data.len(), bytes_preview(&b.data, 48))).raw(&b.data)];
                if kind == BlockKind::Meta {
                    f.push(Field::new("structure", None, "meta block layout not decoded by RocksVision").status(Status::Unsupported));
                } else {
                    f.push(Field::new("usage", None, "Dictionary for LZ4/ZSTD (Zlib unsupported here) data-block decompression.").status(Status::Implicit));
                }
                if let Some((s, e)) = &b.error {
                    f.push(Field::new("error", None, e.clone()).status(*s).diag(b.error_diagnostic().cloned()));
                }
                let mut v = of_fields(format!("{} @0x{:x}", kind.label(), h.offset), f);
                v.spaces = space_info(&b);
                v.diagnostics = b.diags.clone();
                return v;
            }
            _ => {}
        }
        let dec = self.decode_entries(h, kind);
        let (b, res) = (dec.block, dec.result);
        let mut v = View { title: format!("{} @0x{:x} ({} B stored, {} B contents)", kind.label(), h.offset, h.size, b.data.len()), spaces: space_info(&b), diagnostics: b.diags.clone(), ..Default::default() };
        if b.checksum_status == Status::Invalid {
            // Entries are still listed for inspection, but the block as a whole is corrupt.
            v.status = Some(Status::Invalid);
            v.notes.push(Note::with_status(Status::Invalid, format!("{} Block checksum MISMATCH: decoded entries below come from corrupt bytes.", Status::Invalid.symbol())));
        }
        v.notes.push(format!("Compression {} · checksum {} · offsets {}", compression_name(b.ctype), b.checksum_status.label(), if b.compressed { "are block-local (decompressed)" } else { "are file offsets" }).into());
        let (lay, es, err) = match res {
            Ok(x) => x,
            Err(e) => {
                let d = if b.error.is_none() { Some(e.diagnostic(&b)) } else { b.error_diagnostic().cloned() };
                let mut ev = error_view(v.title.clone(), e.status, e.msg.clone());
                if let (Some(d), Some(r)) = (&d, ev.rows.first_mut()) {
                    r.field.diag = Some(Box::new(d.clone()));
                }
                ev.spaces = v.spaces;
                ev.diagnostics = v.diagnostics;
                if b.error.is_none() {
                    ev.diagnostics.extend(d);
                }
                return ev;
            }
        };
        let index_like = matches!(kind, BlockKind::Index | BlockKind::TopIndex | BlockKind::IndexPartition | BlockKind::FilterIndex);
        let props_missing = index_like && !self.has_props();
        let mut es = es;
        let mut err = err;
        if props_missing {
            // Without properties the index value/key encoding flags are guesses (full handles,
            // internal keys): decoding failures are Unknown, not proof of corruption.
            v.notes.push("Properties unavailable: index value delta-encoding and user-key flags are assumed (full handles, internal keys); results may be wrong.".into());
            for e in &mut es {
                if e.status == Status::Invalid {
                    e.status = Status::Unknown;
                }
            }
            if let Some(e) = err.as_mut() {
                if e.status == Status::Invalid || e.status == Status::Truncated {
                    e.status = Status::Unknown;
                }
            }
        }
        // Entries decoded from bytes that failed the block checksum are not trustworthy: their
        // field is Invalid + `untrusted` and carries the block's checksum diagnostic (spec §12.6:
        // Unknown means missing context, not corruption). The text cell keeps its pre-C2 word
        // "? Unknown" so the --dump output stays byte-identical; the UI shows "✖ Untrusted".
        let untrusted_diag = (b.checksum_status == Status::Invalid)
            .then(|| b.diags.iter().find(|d| matches!(d.check, CheckKind::Crc32c | CheckKind::XxHash | CheckKind::XxHash64 | CheckKind::Xxh3)).cloned())
            .flatten()
            .map(|mut d| {
                d.trusted = Trust::Untrusted;
                d.note = "Decoded from a block whose checksum failed; not trustworthy.".into();
                d
            });
        if let Some(e) = &err {
            let s = e.status;
            v.status = Some(s);
            v.notes.push(Note::with_status(s, format!("{} {}: {}", s.symbol(), s.label(), e.msg)));
            let mut d = e.diagnostic(&b);
            if props_missing {
                d.trusted = Trust::Untrusted;
            }
            v.diagnostics.push(d);
        }
        v.notes.push(format!("{} entries, {} restart points{}", es.len(), lay.num_restarts, if lay.hash_index { ", data-block hash index present" } else { "" }).into());
        let idx_user = self.index_key_is_user();
        let target = match kind {
            BlockKind::TopIndex => BlockKind::IndexPartition,
            BlockKind::FilterIndex => BlockKind::FilterPartition,
            _ => BlockKind::Data,
        };
        use ColumnRole as R;
        v.columns = match kind {
            BlockKind::Metaindex => vec![col(R::Index, "#"), col(R::Offset, "Offset"), col(R::Count, "Shared"), col(R::Count, "Non-shared"), col(R::Name, "Name"), col(R::Handle, "Handle"), col(R::Status, "Status")],
            BlockKind::Properties => vec![col(R::Index, "#"), col(R::Offset, "Offset"), col(R::Name, "Name"), col(R::Value, "Value"), col(R::Encoding, "Encoding"), col(R::Status, "Status")],
            _ if index_like => vec![
                col(R::Index, "#"),
                col(R::Offset, "Offset"),
                col(R::Flag, "R"),
                col(R::Count, "Shared"),
                col(R::Count, "Non-shared"),
                col(R::Key, "Separator key"),
                col(R::Seq, "Seq"),
                col(R::ValueType, "Type"),
                col(R::Handle, "Handle ›"),
                col(R::Key, "First key"),
                col(R::Status, "Status"),
            ],
            _ => vec![
                col(R::Index, "#"),
                col(R::Offset, "Offset"),
                col(R::Flag, "R"),
                col(R::Count, "Shared"),
                col(R::Count, "Non-shared"),
                col(R::Len, "Value len"),
                col(R::Key, "User key (reconstructed)"),
                col(R::Seq, "Seq"),
                col(R::ValueType, "Type"),
                col(R::Value, "Value preview"),
                col(R::Status, "Status"),
            ],
        };
        let prefix = if b.compressed { "blk+" } else { "" };
        for e in &es {
            let off_cell = if b.compressed { format!("{prefix}0x{:x}", e.off) } else { format!("0x{:x}", b.handle.offset + e.off as u64) };
            let val = &b.data[e.value_off..e.value_off + e.value_len];
            let entry_span = b.span(e.off, e.value_off + e.value_len - e.off);
            let link = NodeKind::SstEntry { off: h.offset, size: h.size, kind, idx: e.idx };
            let ediag = e.diagnostic(&b);
            v.diagnostics.extend(ediag.clone());
            let untrusted = e.status == Status::Present && b.checksum_status == Status::Invalid;
            let (fstatus, fdiag, cell_status) = if untrusted { (Status::Invalid, untrusted_diag.clone(), Status::Unknown) } else { (e.status, ediag, e.status) };
            let mut fld = Field::new(format!("entry {}", e.idx), Some(entry_span), bytes_preview(&e.key, 64)).kind(K::Key).status(fstatus).link(link).explain(entry_explain(e, &b)).diag(fdiag);
            fld.untrusted = untrusted;
            let cells = match kind {
                BlockKind::Metaindex => {
                    let hs = Handle::decode(val, 0).map(|(x, _)| format!("offset {} size {}", x.offset, x.size)).unwrap_or("invalid".into());
                    if let Some((x, _)) = Handle::decode(val, 0) {
                        let name = String::from_utf8_lossy(&e.key).into_owned();
                        fld = fld.link(NodeKind::SstBlock { off: x.offset, size: x.size, kind: self.meta_kind_for(&name) });
                    }
                    fld = fld.key_bytes(&e.key);
                    vec![e.idx.to_string(), off_cell, e.shared.to_string(), e.non_shared.to_string(), bytes_preview(&e.key, 80), hs, st_cell(cell_status)]
                }
                BlockKind::Properties => {
                    let name = String::from_utf8_lossy(&e.key).into_owned();
                    let (dv, enc) = prop_decode(&name, val);
                    fld.value = format!("{name} = {dv}");
                    vec![e.idx.to_string(), off_cell, name, dv, enc.into(), st_cell(cell_status)]
                }
                _ if index_like => {
                    fld = fld.key_bytes(&e.key);
                    let (uk, seq, ty) = self.split_key(&e.key, idx_user);
                    let (hs, fk) = match &e.handle {
                        Some((x, d, fk)) => {
                            fld = fld.link(NodeKind::SstBlock { off: x.offset, size: x.size, kind: target });
                            (format!("› off 0x{:x}, size {}{}", x.offset, x.size, if *d { " (Δ)" } else { "" }), fk.as_ref().map(|k| self.key_label(k, false)).unwrap_or_default())
                        }
                        None => ("invalid handle".into(), String::new()),
                    };
                    vec![e.idx.to_string(), off_cell, if e.restart { "R".into() } else { String::new() }, e.shared.to_string(), e.non_shared.to_string(), uk, seq, ty, hs, fk, st_cell(cell_status)]
                }
                _ => {
                    // Data / range-deletion entries: Copy value = entry value, Copy key = internal key.
                    fld = fld.value_bytes(val).key_bytes(&e.key);
                    let (uk, seq, ty) = self.split_key(&e.key, false);
                    let vp = if kind == BlockKind::RangeDel { format!("end: {}", bytes_preview(val, 64)) } else { bytes_preview(val, 64) };
                    vec![e.idx.to_string(), off_cell, if e.restart { "R".into() } else { String::new() }, e.shared.to_string(), e.non_shared.to_string(), e.value_len.to_string(), uk, seq, ty, vp, st_cell(cell_status)]
                }
            };
            v.rows.push(Row { cells, field: fld });
        }
        v
    }

    fn split_key(&self, k: &[u8], user: bool) -> (String, String, String) {
        if user {
            return (bytes_preview(k, 64), "-".into(), "-".into());
        }
        match split_ikey(k) {
            // dbformat.h: separators/seek keys use kMaxSequenceNumber + kValueTypeForSeek.
            Some((u, s, t)) if s == MAX_SEQUENCE => (bytes_preview(u, 64), "max (2^56−1)".into(), format!("{t} kValueTypeForSeek")),
            Some((u, s, t)) => (bytes_preview(u, 64), s.to_string(), format!("{t} {}", tag_name(t))),
            None => (bytes_preview(k, 64), "?".into(), "short key".into()),
        }
    }

    fn entry_view(&mut self, h: Handle, kind: BlockKind, idx: u32) -> View {
        let (b, res) = self.block_entries(h, kind);
        let Ok((_, es, _)) = res else { return error_view("Entry", Status::Invalid, "block not decodable") };
        let Some(e) = es.iter().find(|e| e.idx == idx) else { return error_view("Entry", Status::Unknown, "entry not found") };
        let d = &b.data;
        let mut f = Vec::new();
        let mut p = e.off;
        let [n_shared, n_non_shared, n_value_len] = e.hdr_lens;
        let n = n_shared;
        let ediag = e.diagnostic(&b);
        let shared_status = if ediag.is_some() { e.status } else { Status::Present };
        f.push(Field::new("shared", Some(b.span(p, n)), e.shared.to_string()).kind(K::Length).dv(DV::VarInt(u64::from(e.shared))).raw(&d[p..p + n]).enc("varint32").status(shared_status).diag(ediag.clone()).explain("Bytes reused from the previous key (0 at restart points)."));
        p += n;
        let n = n_non_shared;
        f.push(Field::new("non_shared", Some(b.span(p, n)), e.non_shared.to_string()).kind(K::Length).dv(DV::VarInt(u64::from(e.non_shared))).raw(&d[p..p + n]).enc("varint32").explain("Bytes of key suffix stored in this entry."));
        p += n;
        match e.value_len_field {
            Some(vl) => {
                let n = n_value_len;
                f.push(Field::new("value_length", Some(b.span(p, n)), vl.to_string()).kind(K::Length).dv(DV::VarInt(u64::from(vl))).raw(&d[p..p + n]).enc("varint32"));
            }
            None => f.push(Field::new("value_length", None, "not stored (delta-encoded index value, format_version>=4)").status(Status::NotPresent)),
        }
        let suffix = &d[e.key_off..e.key_off + e.non_shared as usize];
        f.push(Field::new("key delta (stored)", Some(b.span(e.key_off, suffix.len())), bytes_preview(suffix, 96)).kind(K::Key).raw(suffix).value_bytes(suffix).enc("bytes").explain("Stored non-shared key suffix."));
        f.push(
            Field::new("key (reconstructed)", None, bytes_preview(&e.key, 96))
                .kind(K::InternalKey)
                .raw(&e.key)
                .value_bytes(&e.key)
                .status(Status::Implicit)
                .explain(format!("Derived: first {} bytes of the previous key + {} stored bytes. Not a contiguous on-disk range.", e.shared, e.non_shared)),
        );
        let user_index = matches!(kind, BlockKind::Index | BlockKind::TopIndex | BlockKind::IndexPartition | BlockKind::FilterIndex) && self.index_key_is_user();
        let internal = matches!(kind, BlockKind::Data | BlockKind::RangeDel | BlockKind::Index | BlockKind::TopIndex | BlockKind::IndexPartition | BlockKind::FilterIndex) && !user_index;
        if internal {
            if let Some((u, s, t)) = split_ikey(&e.key) {
                f.push(Field::new("user key", None, bytes_preview(u, 96)).kind(K::UserKey).raw(u).value_bytes(u).status(Status::Implicit).explain("Internal key minus its last 8 bytes."));
                // The 8-byte trailer is fully stored here only if non_shared >= 8.
                let tspan = if e.non_shared >= 8 { Some(b.span(e.key_off + e.non_shared as usize - 8, 8)) } else { None };
                let packed = (s << 8) | u64::from(t);
                f.push(Field::new("sequence", tspan, s.to_string()).kind(K::Sequence).dv(DV::U64(s)).enc("fixed64 LE packed >> 8").explain(format!("Packed trailer 0x{packed:016x} = seq << 8 | type (dbformat.h PackSequenceAndType).")));
                f.push(Field::new("value type", tspan.map(|sp| Span::new(sp.space, sp.offset, 1)), format!("{t} {}", tag_name(t))).kind(K::Tag).dv(DV::Enum { name: tag_name(t), code: u64::from(t) }).enc("low byte of packed trailer"));
            } else {
                f.push(Field::new("internal key", None, "shorter than 8 bytes").status(Status::Invalid));
            }
        }
        let val = &d[e.value_off..e.value_off + e.value_len];
        let vspan = Some(b.span(e.value_off, e.value_len));
        match &e.handle {
            Some((x, delta, fk)) => {
                let target = match kind {
                    BlockKind::TopIndex => BlockKind::IndexPartition,
                    BlockKind::FilterIndex => BlockKind::FilterPartition,
                    _ => BlockKind::Data,
                };
                let enc = if *delta { "varsigned64 size delta" } else { "varint64 offset + varint64 size" };
                let ex = if *delta {
                    "Delta-encoded: offset = previous offset + previous size + 5 (trailer); size = previous size + delta (IndexValue::DecodeFrom)."
                } else {
                    "Full BlockHandle."
                };
                f.push(Field::new("block handle", vspan, format!("offset {} size {}", x.offset, x.size)).kind(K::BlockHandle(HT::Block)).dv(DV::Handle { offset: x.offset, size: x.size }).raw(val).enc(enc).explain(ex).link(NodeKind::SstBlock { off: x.offset, size: x.size, kind: target }));
                if let Some(k) = fk {
                    f.push(Field::new("first key in block", None, self.key_label(k, false)).raw(k).value_bytes(k).status(Status::Present).explain("kBinarySearchWithFirstKey: length-prefixed first internal key follows the handle."));
                }
            }
            None => {
                let name = String::from_utf8_lossy(&e.key).into_owned();
                let (dv, enc) = if kind == BlockKind::Properties { prop_decode(&name, val) } else { (bytes_preview(val, 128), "bytes") };
                let mut vf = if kind == BlockKind::Properties { prop_field("value", &name, val) } else { Field::new("value", vspan, dv).kind(K::Value) };
                vf.span = vspan;
                vf = vf.raw(val).enc(enc);
                if kind != BlockKind::Properties && kind != BlockKind::Metaindex {
                    vf = vf.value_bytes(val);
                }
                if kind == BlockKind::Metaindex {
                    if let Some((x, _)) = Handle::decode(val, 0) {
                        vf = vf.link(NodeKind::SstBlock { off: x.offset, size: x.size, kind: self.meta_kind_for(&name) });
                        vf.value = format!("offset {} size {}", x.offset, x.size);
                        vf.encoding = "BlockHandle varint64 ×2".into();
                    }
                }
                f.push(vf);
            }
        }
        f.push(Field::new("restart point", None, if e.restart { "yes" } else { "no" }).status(Status::Implicit).explain("Whether this entry's offset is listed in the restart array."));
        let mut v = of_fields(format!("Entry {idx} of {} @0x{:x}", kind.label(), h.offset), f);
        v.status = Some(e.status);
        v.spaces = space_info(&b);
        v.diagnostics = b.diags.clone();
        v.diagnostics.extend(ediag);
        v
    }

    fn trailer_view(&mut self, h: Handle, kind: BlockKind) -> View {
        let b = self.block(h, kind);
        let ct = self.footer().map(|f| f.checksum_type).unwrap_or(1);
        let base = self.footer().map(|f| f.base_context).unwrap_or(0);
        let mut f = vec![Field::new("block contents (stored)", Some(Span::file(h.offset, h.size)), format!("{} bytes{}", h.size, if b.compressed { " compressed" } else { "" })).enc("bytes").explain("Physical bytes as stored in the file, before the 5-byte trailer.")];
        if let Some((v, n)) = b.size_prefix {
            f.push(Field::new("uncompressed size prefix", Some(Span::file(h.offset, n as u64)), v.to_string()).kind(K::Length).dv(DV::VarInt(v)).enc("varint32/64").explain("compress_format_version 2: varint uncompressed size precedes the codec payload (all codecs except Snappy/XPRESS)."));
        }
        if h.offset + h.size + 5 <= self.len() {
            let raw = self.trailer_bytes(h);
            f.push(Field::new("compression type", Some(Span::file(h.offset + h.size, 1)), format!("{} {}", b.ctype, compression_name(b.ctype))).kind(K::CompressionType).dv(DV::U8(b.ctype)).raw(&raw[..1.min(raw.len())]).enc("u8").explain("include/rocksdb/compression_type.h code. Part of the checksummed bytes."));
            let comp = b.computed_checksum.map(|c| format!("0x{c:08x}")).unwrap_or("n/a".into());
            f.push(
                Field::new("checksum", Some(Span::file(h.offset + h.size + 1, 4)), format!("0x{:08x}", b.stored_checksum))
                    .kind(K::Checksum(ChecksumRole::Stored))
                    .dv(DV::U32(b.stored_checksum))
                    .diag(b.diags.iter().find(|d| d.check == footer::checksum_check(ct)).cloned())
                    .raw(raw.get(1..5).unwrap_or(&[]))
                    .enc("fixed32 LE")
                    .status(b.checksum_status)
                    .explain(format!(
                        "{} over file bytes [0x{:x}, 0x{:x}] (block + type byte). Computed {comp}. Context modifier = base 0x{base:08x} ^ (lo32(off)+hi32(off)) = 0x{:08x}; stored − modifier = 0x{:08x} -> {}.",
                        footer::checksum_name(ct),
                        h.offset,
                        h.offset + h.size,
                        b.modifier,
                        b.stored_checksum.wrapping_sub(b.modifier),
                        b.checksum_status.label()
                    )),
            );
        } else {
            f.push(Field::new("trailer", None, "past end of file").status(Status::Truncated).diag(b.error_diagnostic().cloned()));
        }
        if b.compressed {
            f.push(Field::new("decompressed contents", None, format!("{} bytes", b.data.len())).status(if b.ok() { Status::Implicit } else { Status::Invalid }).diag(b.error_diagnostic().cloned()).explain("Derived bytes with their own block-local offset space (blk+). No file bytes of their own."));
        }
        if let Some((s, e)) = &b.error {
            f.push(Field::new("error", None, e.clone()).status(*s).diag(b.error_diagnostic().cloned()));
        }
        let mut v = of_fields(format!("Trailer of {} @0x{:x}", kind.label(), h.offset), f);
        v.status = Some(if b.ok() { b.checksum_status } else { b.error.as_ref().map(|x| x.0).unwrap_or(Status::Invalid) });
        v.diagnostics = b.diags.clone();
        v
    }

    fn restarts_view(&mut self, h: Handle, kind: BlockKind) -> View {
        let b = self.block(h, kind);
        if let Some((s, e)) = &b.error {
            let mut v = error_view("Restart array", *s, e.clone());
            if let (Some(d), Some(r)) = (b.error_diagnostic(), v.rows.first_mut()) {
                r.field.diag = Some(Box::new(d.clone()));
            }
            v.diagnostics = b.diags.clone();
            return v;
        }
        let d = &b.data;
        let lay = match SstInspector::layout_of(&b) {
            Ok(l) => l,
            Err(e) => {
                let diag = Diagnostic::new(e.check, Some(b.span(d.len().saturating_sub(4), 4.min(d.len()))), Trust::Untrusted, "the entries of this block are not decoded", e.msg.clone());
                return with_error_diag(error_view("Restart array", Status::Invalid, e.to_string()), Some(diag));
            }
        };
        let mut f = Vec::new();
        for i in 0..lay.num_restarts.min(RESTARTS_LISTED as u32) as usize {
            let p = lay.restarts_off + 4 * i;
            let v = fixed32(d, p).unwrap_or(0);
            f.push(Field::new(format!("restart[{i}]"), Some(b.span(p, 4)), format!("0x{v:x}")).kind(K::Offset).dv(DV::U32(v)).raw(&d[p..p + 4]).enc("fixed32 LE").explain("Block-local offset of an entry whose key is stored in full (shared = 0)."));
        }
        if lay.num_restarts > RESTARTS_LISTED as u32 {
            f.push(Field::new("more restarts", None, format!("{} not listed", lay.num_restarts - RESTARTS_LISTED as u32)).status(Status::Limited));
        }
        match lay.hash {
            Some((s, nb)) => {
                f.push(Field::new("hash index buckets", Some(b.span(s, nb as usize)), format!("{nb} buckets")).raw(&d[s..s + nb as usize]).enc("u8 restart index each (255 empty, 254 collision)").explain("Data-block hash index (data_block_hash_index.h)."));
                f.push(Field::new("NUM_BUCK", Some(b.span(s + nb as usize, 2)), nb.to_string()).enc("fixed16 LE"));
            }
            None => f.push(Field::new("data-block hash index", None, "not present (footer MSB clear)").status(Status::NotPresent)),
        }
        f.push(
            Field::new("block footer", Some(b.span(lay.footer_off, 4)), format!("0x{:08x}: num_restarts {}{}", lay.footer_raw, lay.num_restarts, if lay.hash_index { ", MSB=hash index" } else { "" }))
                .raw(&d[lay.footer_off..])
                .enc("fixed32 LE")
                .explain("data_block_footer.cc: low 31 bits = number of restarts; MSB = kDataBlockBinaryAndHash (only for blocks <= 64 KiB)."),
        );
        let mut v = of_fields(format!("Restart array of {} @0x{:x}", kind.label(), h.offset), f);
        v.spaces = space_info(&b);
        v
    }

    fn filter_view(&mut self, h: Handle, kind: BlockKind) -> View {
        let b = self.block(h, kind);
        if let Some((s, e)) = &b.error {
            let mut v = error_view(kind.label(), *s, e.clone());
            if let (Some(d), Some(r)) = (b.error_diagnostic(), v.rows.first_mut()) {
                r.field.diag = Some(Box::new(d.clone()));
            }
            v.diagnostics = b.diags.clone();
            return v;
        }
        let d = &b.data;
        let n = d.len();
        let mut f = vec![Field::new("filter contents", Some(b.span(0, n)), format!("{n} bytes")).enc("bytes")];
        if n <= 5 {
            f.push(Field::new("filter", None, "≤ 5 bytes: AlwaysFalse (empty) filter").status(Status::Present));
        } else {
            let m = n - 5;
            let raw = d[m] as i8;
            let meta_span = Some(b.span(m, 5));
            match raw {
                -1 => {
                    let sub = d[m + 1];
                    let bp = d[m + 2];
                    f.push(Field::new("bit array", Some(b.span(0, m)), format!("{} bytes = {} bits", m, m as u64 * 8)).enc("bits"));
                    f.push(Field::new("metadata", meta_span, "new Bloom marker (-1)").raw(&d[m..]).enc("5 bytes").explain("filter_policy.cc: byte0 = -1 newer Bloom, byte1 = sub-implementation, byte2 = block/probes."));
                    f.push(Field::new("sub-implementation", Some(b.span(m + 1, 1)), if sub == 0 { "0 FastLocalBloom".into() } else { format!("{sub} unknown") }).status(if sub == 0 { Status::Present } else { Status::Unknown }));
                    f.push(Field::new("num_probes", Some(b.span(m + 2, 1)), (bp & 31).to_string()).enc("low 5 bits"));
                    f.push(Field::new("cache-line block size", Some(b.span(m + 2, 1)), format!("{} bytes", 1u32 << (((bp >> 5) & 7) + 6))).enc("high 3 bits: log2 − 6"));
                }
                -2 => {
                    let seed = d[m + 1];
                    let nb = u32::from(d[m + 2]) | u32::from(d[m + 3]) << 8 | u32::from(d[m + 4]) << 16;
                    f.push(Field::new("solution bytes", Some(b.span(0, m)), format!("{m} bytes")).enc("Ribbon solution"));
                    f.push(Field::new("metadata", meta_span, "Standard128Ribbon marker (-2)").raw(&d[m..]).enc("5 bytes"));
                    f.push(Field::new("seed", Some(b.span(m + 1, 1)), seed.to_string()).enc("u8"));
                    f.push(Field::new("num_blocks", Some(b.span(m + 2, 3)), nb.to_string()).enc("24-bit LE"));
                }
                0 => f.push(Field::new("metadata", meta_span, "0 = AlwaysTrue filter").raw(&d[m..])),
                p if p > 0 => {
                    let lines = fixed32(d, n - 4).unwrap_or(0);
                    f.push(Field::new("bit array", Some(b.span(0, m)), format!("{} bytes (legacy Bloom)", m)));
                    f.push(Field::new("num_probes", Some(b.span(m, 1)), p.to_string()).enc("i8"));
                    f.push(Field::new("num_lines", Some(b.span(n - 4, 4)), lines.to_string()).enc("fixed32 LE"));
                }
                _ => f.push(Field::new("metadata", meta_span, format!("marker {raw}: unknown -> treated as AlwaysTrue")).status(Status::Unknown)),
            }
        }
        let mut v = of_fields(format!("{} @0x{:x}", kind.label(), h.offset), f);
        v.notes.push("Filter bit contents are summarized, not queried.".into());
        v.spaces = space_info(&b);
        v
    }

    fn data_blocks_view(&mut self, skip: u64, cancel: &dyn Fn() -> bool) -> View {
        let (hs, more, err) = self.data_handles(skip, DATA_PAGE, cancel);
        let rows = hs
            .iter()
            .enumerate()
            .map(|(i, (h, k))| {
                let fld = Field::new(format!("data block {}", skip as usize + i), Some(Span::file(h.offset, h.size + BLOCK_TRAILER)), format!("offset {} size {}", h.offset, h.size))
                    .kind(K::BlockHandle(HT::Block))
                    .dv(DV::Handle { offset: h.offset, size: h.size })
                    .link(NodeKind::SstBlock { off: h.offset, size: h.size, kind: BlockKind::Data })
                    .explain("Handle from the index; the separator key is ≥ every key in the block and < every key in the next.");
                Row { cells: vec![(skip as usize + i).to_string(), format!("0x{:x}", h.offset), h.size.to_string(), self.key_label(k, true), st_cell(Status::Present)], field: fld }
            })
            .collect();
        use ColumnRole as R;
        let mut v = View { title: format!("Data blocks from #{skip}"), columns: vec![col(R::Index, "#"), col(R::Offset, "Offset"), col(R::Size, "Size"), col(R::Key, "Index separator key"), col(R::Status, "Status")], rows, ..Default::default() };
        if more {
            v.notes.push(format!("Showing {DATA_PAGE}; expand \"Next\" in the tree for more.").into());
        }
        if let Some(e) = err {
            v.notes.push(e.into());
        }
        v
    }
}
