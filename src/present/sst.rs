//! SST presentation: lazy tree children and views (tables, explanations, derived-space titles)
//! built from the typed state of `sst::SstInspector`.

use super::common::{Location, error_view, explained, field_row, of_fields, range, range_len, size_row, st_cell, with_error_diag};
use crate::format::codec::compression_name;
use crate::format::ikey::{MAX_SEQUENCE, ikey_meaning, split_ikey, value_type_name as tag_name};
use crate::limits::{COMPRESSION_SAMPLE, LIST_CHECK_BYTES, RESTARTS_LISTED, SST_DATA_PAGE as DATA_PAGE, SST_TREE_CHILDREN as TREE_CHILD_CAP};
use crate::model::*;
use crate::model::{DecodedValue as DV, FieldKind as K, HandleTarget as HT};
use crate::sst::block::{Block, Entry as BlockEntry};
use crate::sst::footer::{self, BLOCK_TRAILER, Footer, Handle};
use crate::sst::layout::{Entry, Item, ItemKind, PhysLayout};
use crate::sst::{PropValue, SstInspector, index_type_name, is_legacy_filter_name, meta_kind, prop_value};
use crate::util::{bytes_preview, fixed32, hex_preview};
use std::collections::BTreeMap;

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

/// 0030 A7: the level / column family rows of an SST from its directory's MANIFEST (external
/// evidence, not stored in the SST; Implicit, no bytes), or the unknown level with why. The CF
/// row is compared with the SST's own `rocksdb.column.family.name` property (`cf_prop`).
pub fn manifest_context_fields(ctx: &SstManifestContext, cf_prop: Option<&[u8]>) -> Vec<Field> {
    let unknown = |why: String| vec![Field::new("level", None, format!("unknown — {why} (only a MANIFEST records levels)")).status(Status::Unknown)];
    match ctx {
        SstManifestContext::Standalone => unknown("a standalone SST has no MANIFEST context: no numbered name or no CURRENT next to it".into()),
        SstManifestContext::NotListed { manifest, number } => unknown(format!("{manifest} in this folder does not list {number:06}.sst as a live file")),
        SstManifestContext::Untrusted { manifest } => unknown(format!("{manifest} in this folder could not be read or replayed completely")),
        SstManifestContext::Listed(e) => {
            let ext = format!("External MANIFEST evidence ({}, live file {:06}), not stored in the SST.", e.manifest, e.number);
            let meaning = |what: &str| format!("The {what} that {} records for {:06}.sst. An SST does not store its level.\n{ext}", e.manifest, e.number);
            let read = e.read_at.map(|s| format!(" (read {})", crate::util::utc_time(std::time::UNIX_EPOCH + std::time::Duration::from_secs(s)))).unwrap_or_default();
            let source = format!("from the folder's MANIFEST{read}");
            let level = Field::new("level (MANIFEST)", None, format!("L{}", e.level)).status(Status::Implicit).explain(meaning("level"));
            let cf = Field::new("column family (MANIFEST)", None, format!("{} {}", e.cf_id, e.cf_name));
            let cf = match cf_prop.map(|p| String::from_utf8_lossy(p).into_owned()) {
                Some(p) if p == e.cf_name => cf.status(Status::Implicit).explain(format!("{}\nAgrees with the SST property rocksdb.column.family.name.", meaning("column family"))),
                Some(p) => cf.status(Status::Unknown).explain(format!("{}\nMANIFEST lists it under {} {}; the SST property says {p}. Not reconciled.", meaning("column family"), e.cf_id, e.cf_name)),
                None => cf.status(Status::Implicit).explain(meaning("column family")),
            };
            [level, cf]
                .into_iter()
                .map(|mut f| {
                    f.no_bytes_source = Some(source.clone());
                    f.kind = K::ManifestEvidence;
                    f
                })
                .collect()
        }
    }
}

fn space_info(b: &Block) -> Vec<SpaceInfo> {
    if b.compressed {
        vec![SpaceInfo {
            space: Space::Block(b.handle.offset),
            size: b.data.len() as u64,
            // "stored" = on-disk extent incl. trailer; "payload" = stored bytes before the
            // trailer; "decompressed" = decompressed length (one definition per word everywhere).
            title: format!("Block-local offsets ({} block @0x{:x}: {} B compressed payload -> {} B decompressed)", compression_name(b.ctype), b.handle.offset, b.handle.size, b.data.len()),
            map: SpaceMap::Compressed { file: Span::file(b.handle.offset, b.handle.size), codec: compression_name(b.ctype).to_string() },
        }]
    } else {
        Vec::new()
    }
}

/// Meaning line of a physical structure row (0020 §2), in the one range notation; the whole range
/// is on the Where line. A block: "payload file [0x6390c,0x648f5) · 4073 B · trailer
/// [0x648f5,0x648fa) · 5 B"; a structure without a block trailer (Footer): "53 B, no block
/// trailer".
fn structure_meaning(start: u64, end: u64, payload: u64, trailer: u64) -> String {
    if trailer > 0 {
        let p = start + payload;
        format!("payload {} · trailer {}", Span::file(start, payload).range_text(), range_len(p, end))
    } else {
        format!("{} B, no block trailer", end - start)
    }
}

/// Meaning line of an unaccounted gap row (0020 §2).
fn gap_meaning(start: u64, end: u64) -> String {
    format!("unaccounted {} · no known structure covers these bytes", Span::file(start, end - start).range_text())
}

/// "Physical layout: 7 structures in file order (Footer last) · 0 unaccounted gaps · data
/// blocks listed 1 of 1 (from index)" — coverage stated, never a bare count of a partial map.
fn layout_coverage(l: &PhysLayout) -> String {
    let gaps = l.with_all_gaps().iter().filter(|e| matches!(e, Entry::Gap { .. })).count();
    let bad = match l.invalid.len() {
        0 => String::new(),
        1 => " · 1 invalid handle".to_string(),
        n => format!(" · {n} invalid handles"),
    };
    format!(
        "Physical layout: {} structures in file order (Footer last) · {gaps} unaccounted gap(s){bad} · data blocks {} (from index){}",
        l.items.len(),
        l.listed_text(),
        l.note.as_ref().map(|n| format!(" · {n}")).unwrap_or_default()
    )
}

/// Status of a layout row from the check cache only (never reads): status, Status cell, the
/// "Not checked" derivation, the derivation of a checked Present block, and the diagnostic
/// the cached check recorded (0017 D3: the row's card shows it instead of "not recorded").
struct RowCheck {
    status: Status,
    cell: String,
    not_checked: Option<String>,
    note: Option<&'static str>,
    diag: Option<Diagnostic>,
}

/// The handle that places a block of this kind (for the "Not checked" derivation).
fn handle_source(kind: Option<BlockKind>) -> &'static str {
    match kind {
        Some(BlockKind::Data) => "index handle",
        Some(BlockKind::IndexPartition) => "top-level index handle",
        Some(BlockKind::FilterPartition) => "partitioned filter index handle",
        Some(BlockKind::Metaindex) => "footer handle",
        Some(BlockKind::Index | BlockKind::TopIndex) => "footer/metaindex index handle",
        _ => "metaindex handle",
    }
}

/// "Data blocks #0–#63" / "Index partitions @0x…–@0x…": the head of a group's label (what and
/// first/last member), never truncated in the tree (design A4/A5).
fn group_head(l: &PhysLayout, first: usize, count: usize) -> String {
    let (a, z) = (&l.items[first], &l.items[first + count - 1]);
    let what = match a.block_kind() {
        Some(BlockKind::Data) => "Data blocks",
        Some(BlockKind::IndexPartition) => "Index partitions",
        _ => "Filter partitions",
    };
    let ord = |it: &Item| match &it.kind {
        ItemKind::Block { ordinal: Some(n), .. } => format!("#{n}"),
        _ => format!("@0x{:x}", it.start),
    };
    format!("{what} {}–{}", ord(a), ord(z))
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
                K::BaseContext => explained(
                    f,
                    "Per-file random value; every block checksum adds base ^ (lo32(offset) + hi32(offset)) so misplaced blocks fail verification. RocksDB rejects 0 for format_version >= 6.",
                ),
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

fn entry_explain(e: &BlockEntry, b: &Block) -> String {
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
    // ---------------- physical layout (tree order) ----------------

    /// Tree label of a layout item: what it is, its offset (decimal, then hex) and stored size.
    fn item_label(&self, it: &Item) -> String {
        let pos = format!("@{} (0x{:x}, {} B)", it.start, it.start, it.size + it.trailer);
        match &it.kind {
            ItemKind::Footer => format!("Footer {pos}"),
            ItemKind::Block { kind, name, ordinal } => match kind {
                BlockKind::Data => format!("Data block {pos}{}", ordinal.map(|n| format!(" #{n}")).unwrap_or_default()),
                BlockKind::Index | BlockKind::TopIndex => format!("{} ({}) {pos}", kind.label(), index_type_name(self.index_type())),
                BlockKind::Filter | BlockKind::FilterIndex | BlockKind::Properties => format!("{} ({name}) {pos}", kind.label()),
                BlockKind::RangeDel => format!("Range deletions {pos}"),
                BlockKind::CompressionDict => format!("Compression dictionary {pos}"),
                BlockKind::Meta if is_legacy_filter_name(name) => format!("Legacy block-based filter {name} (unsupported) {pos}"),
                BlockKind::Meta => format!("Meta block {name} {pos}"),
                k => format!("{} {pos}", k.label()),
            },
        }
    }

    /// Node of a layout item at its canonical place.
    fn item_node(it: &Item) -> NodeKind {
        match &it.kind {
            ItemKind::Footer => NodeKind::SstFooter,
            ItemKind::Block { kind, .. } => NodeKind::SstBlock { off: it.start, size: it.size, kind: *kind },
        }
    }

    fn item_child(&mut self, it: &Item, budget: &mut u64) -> Child {
        let node = Self::item_node(it);
        let label = self.item_label(it);
        match &it.kind {
            ItemKind::Footer => Child::new(label, node).leaf().status(if self.footer().is_ok() { Status::Present } else { Status::Invalid }).diag(self.footer_diagnostic().cloned()),
            ItemKind::Block { kind: BlockKind::Metaindex, .. } => Child::new(label, node).status(if self.meta().is_ok() { Status::Present } else { Status::Invalid }),
            ItemKind::Block { kind: BlockKind::Meta, name, .. } => Child::new(label, node).status(if is_legacy_filter_name(name) { Status::Unsupported } else { Status::Unknown }),
            ItemKind::Block { .. } => self.checked_child(Child::new(label, node), it.handle(), budget),
        }
    }

    /// A listed block's child: listing checks it (one bounded read on the decode worker)
    /// while the listing's byte budget lasts; beyond it the block is listed "not checked"
    /// (no glyph, coverage 0 of 1) until opened (0015 C3 GUI-5). A block already in the check
    /// cache is answered from it (status and diagnostic, no read) and costs no budget (0017 D3).
    fn checked_child(&mut self, c: Child, h: Handle, budget: &mut u64) -> Child {
        if let Some((st, d)) = self.checked_result(h) {
            let d = d.cloned();
            return c.status(st).diag(d);
        }
        let cost = h.size.saturating_add(BLOCK_TRAILER);
        if cost > *budget {
            return c.status(Status::Unknown).checked((0, 1));
        }
        *budget = budget.saturating_sub(cost);
        let (st, d) = self.block_check(h);
        c.status(st).diag(d)
    }

    fn entry_child(&mut self, l: &PhysLayout, e: Entry, budget: &mut u64) -> Child {
        match e {
            Entry::Item(i) => self.item_child(&l.items[i], budget),
            Entry::Group { first, count } => {
                let (a, z) = (&l.items[first], &l.items[first + count - 1]);
                let (st, c) = self.group_check(l, first, count);
                // The coverage goes after the head at display time (`with_coverage`).
                Child::new(format!("{} · {} · {count} blocks", group_head(l, first, count), range(a.start, z.end())), NodeKind::SstLayoutGroup { first: first as u64, count: count as u64 })
                    .status(st)
                    .checked(c)
            }
            Entry::Gap { start, end } => Child::new(format!("Unaccounted bytes {} · {} B", range(start, end), end - start), NodeKind::SstGap { start, end }).leaf().status(Status::Unknown),
            Entry::Invalid(i) => {
                let it = &l.invalid[i];
                Child::new(format!("{} · handle beyond footer/file", self.item_label(it)), Self::item_node(it)).leaf().status(Status::Invalid).diag(Some(self.beyond_diag(l, it)))
            }
        }
    }

    /// Coverage of a group of layout items from blocks already checked (cache only, no read):
    /// the worst checked status (Present when the checked ones are clean; Unknown when none was
    /// checked) and (checked, members). Which glyph that allows is `group_glyph`.
    fn group_check(&self, l: &PhysLayout, first: usize, count: usize) -> (Status, (u64, u64)) {
        let mut checked = 0u64;
        let mut worst = Status::Present;
        for it in l.items.iter().skip(first).take(count) {
            if let Some(s) = self.checked_status(it.handle()) {
                checked += 1;
                if rank(s) > rank(worst) {
                    worst = s;
                }
            }
        }
        (if checked == 0 { Status::Unknown } else { worst }, (checked, count as u64))
    }

    /// Status of a layout item from the check cache only (never reads): the Footer is Present
    /// when it decoded at open; a block is its checked status, or Unknown with the "Not
    /// checked" derivation (design A2/A6/A9).
    fn item_check(&self, it: &Item) -> RowCheck {
        if it.block_kind().is_none() {
            let st = if self.footer().is_ok() { Status::Present } else { Status::Invalid };
            return RowCheck { status: st, cell: st_cell(st), not_checked: None, note: None, diag: None };
        }
        match self.checked_result(it.handle()) {
            Some((s, d)) => RowCheck { status: s, cell: st_cell(s), not_checked: None, note: self.check_note(it.handle()), diag: d.cloned() },
            None => RowCheck {
                status: Status::Unknown,
                cell: NOT_CHECKED.into(),
                not_checked: Some(format!(
                    "{NOT_CHECKED}: the {} places this block at {}; its checksum is verified only when the block is opened.",
                    handle_source(it.block_kind()),
                    range(it.start, it.end())
                )),
                note: None,
                diag: None,
            },
        }
    }

    /// Derivation of a checked Present block (cache only): "checksum verified" only when a
    /// checksum was actually compared; kNoChecksum / unknown types say so instead.
    fn check_note(&self, h: Handle) -> Option<&'static str> {
        (self.checked_status(h) == Some(Status::Present)).then(|| self.present_note(self.checksum_verified(h)))
    }

    fn present_note(&self, compared: bool) -> &'static str {
        match (compared, self.footer().map(|f| f.checksum_type)) {
            (true, _) => CHECKSUM_VERIFIED,
            (false, Ok(0)) => NO_CHECKSUM,
            _ => CHECKSUM_NOT_COMPARED,
        }
    }

    /// Status of a group row (layout table, root summary) from the cache, and its coverage;
    /// the "Not checked" derivation when the group shows no glyph (design A3).
    fn group_row(&self, l: &PhysLayout, first: usize, count: usize) -> (RowCheck, (u64, u64)) {
        let (st, c) = self.group_check(l, first, count);
        let row = match group_glyph(st, Some(c)) {
            Some(g) => {
                let all_compared = l.items.iter().skip(first).take(count).all(|it| self.checksum_verified(it.handle()));
                RowCheck { status: g, cell: st_cell(g), not_checked: None, note: (g == Status::Present).then(|| self.present_note(all_compared)), diag: None }
            }
            None if c.0 == 0 => RowCheck {
                status: Status::Unknown,
                cell: NOT_CHECKED.into(),
                not_checked: Some(format!("{NOT_CHECKED}: none of the {} blocks of this group was checked yet; a block's checksum is verified when it is opened or listed.", c.1)),
                note: None,
                diag: None,
            },
            None => RowCheck {
                status: Status::Unknown,
                cell: format!("{} of {} checked", c.0, c.1),
                not_checked: Some(format!(
                    "{NOT_CHECKED} as a whole: {} of {} blocks of this group were checked (blocks opened or listed so far), none with a problem; the others are verified when opened.",
                    c.0, c.1
                )),
                note: None,
                diag: None,
            },
        };
        (row, c)
    }

    /// Diagnostic of a handle that does not address bytes before the footer.
    fn beyond_diag(&self, l: &PhysLayout, it: &Item) -> Diagnostic {
        let span = (it.start < l.file_len).then(|| Span::file(it.start, it.end().min(l.file_len) - it.start));
        Diagnostic::new(
            CheckKind::Handle,
            span,
            Trust::Untrusted,
            "the block is not read; it is listed before the Footer, not in file order",
            format!("block handle offset {} size {} (+{} B trailer) ends at {}, beyond the footer at {} (file length {})", it.start, it.size, it.trailer, it.end(), l.footer_off, l.file_len),
        )
        .values(Some(DV::U64(l.footer_off)), Some(DV::U64(it.end())))
    }

    /// Location lines of the structure starting at `off` (`end` exclusive) with its contents
    /// and trailer, and the next structure / gap after it from the layout.
    fn location(&mut self, off: u64, end: u64, contents: Option<(u64, u64)>, trailer: Option<(u64, u64)>, decompressed: Option<u64>, cancel: &dyn Fn() -> bool) -> Location {
        let l = self.layout(cancel);
        let next = l.items.iter().find(|it| it.start >= end).map(|it| (it.start, Self::item_node(it)));
        let gap = next.as_ref().filter(|(at, _)| *at > end).map(|(at, _)| NodeKind::SstGap { start: end, end: *at });
        Location { start: off, end, contents, trailer, header: None, decompressed, next, gap, eof: self.len() }
    }

    /// Ancestors of `target` below the root (canonical path of a navigation): the layout groups
    /// holding a structure, plus the block for its parts. None if the target is not a node of
    /// the physical tree or the views.
    pub fn path_to(&mut self, target: &NodeKind, cancel: &dyn Fn() -> bool) -> Option<Vec<NodeKind>> {
        let l = self.layout(cancel);
        let groups = |off: u64| l.group_path(off).into_iter().map(|(f, c)| NodeKind::SstLayoutGroup { first: f as u64, count: c as u64 }).collect::<Vec<_>>();
        match target {
            NodeKind::Root | NodeKind::SstFooter | NodeKind::SstLayout | NodeKind::SstProperties | NodeKind::SstDataBlocks { skip: 0 } => Some(Vec::new()),
            NodeKind::SstBlock { off, size, .. } if l.item_at_start(*off).is_none() => l.invalid_at(*off, *size).map(|_| Vec::new()),
            NodeKind::SstBlock { off, .. } => Some(groups(*off)),
            NodeKind::SstGap { start, .. } => Some(groups(*start)),
            NodeKind::SstLayoutGroup { first, .. } => {
                let off = l.items.get(*first as usize)?.start;
                let mut g = groups(off);
                let i = g.iter().position(|k| k == target)?;
                g.truncate(i);
                Some(g)
            }
            NodeKind::SstTrailer { off, size, kind } | NodeKind::SstRestarts { off, size, kind } | NodeKind::SstEntry { off, size, kind, .. } => {
                l.item_at_start(*off)?;
                let mut g = groups(*off);
                g.push(NodeKind::SstBlock { off: *off, size: *size, kind: *kind });
                Some(g)
            }
            _ => None,
        }
    }

    /// The structure holding file byte `off` and what to select in its view: the field under
    /// the byte for uncompressed bytes; the whole block for compressed contents (no byte maps
    /// to a decompressed field); the gap for unaccounted bytes.
    pub fn locate(&mut self, off: u64, cancel: &dyn Fn() -> bool) -> Option<Located> {
        let l = self.layout(cancel);
        let at = Some((Space::File, off));
        let Some(i) = l.item_containing(off) else {
            let (a, b) = l.gap_containing(off)?;
            return Some((NodeKind::SstGap { start: a, end: b }, None, Some(Span::file(a, b - a))));
        };
        let it = l.items[i].clone();
        let Some(kind) = it.block_kind() else { return Some((NodeKind::SstFooter, at, None)) };
        let (o, s) = (it.start, it.size);
        if off >= o + s {
            return Some((NodeKind::SstTrailer { off: o, size: s, kind }, at, None));
        }
        if self.block_ctype(it.handle()).unwrap_or(0) != 0 {
            return Some((NodeKind::SstBlock { off: o, size: s, kind }, None, Some(Span::file(o, s))));
        }
        if !matches!(kind, BlockKind::Filter | BlockKind::FilterPartition | BlockKind::CompressionDict | BlockKind::Meta) {
            let b = self.block(it.handle(), kind);
            if let Ok(lay) = SstInspector::layout_of(&b)
                && off >= o + lay.restarts_off as u64
            {
                return Some((NodeKind::SstRestarts { off: o, size: s, kind }, at, None));
            }
        }
        Some((NodeKind::SstBlock { off: o, size: s, kind }, at, None))
    }

    // ---------------- tree ----------------

    pub fn children(&mut self, k: &NodeKind, cancel: &dyn Fn() -> bool) -> Vec<Child> {
        match k {
            NodeKind::Root => self.root_children(cancel),
            NodeKind::SstLayoutGroup { first, count } => {
                let l = self.layout(cancel);
                let mut budget = LIST_CHECK_BYTES;
                l.group(*first as usize, *count as usize).into_iter().map(|e| self.entry_child(&l, e, &mut budget)).collect()
            }
            NodeKind::SstBlock { off, size, kind } => {
                let h = Handle { offset: *off, size: *size };
                let mut out = Vec::new();
                let structured = !matches!(kind, BlockKind::Filter | BlockKind::FilterPartition | BlockKind::CompressionDict | BlockKind::Meta);
                let target = match kind {
                    BlockKind::Index | BlockKind::IndexPartition => Some(BlockKind::Data),
                    BlockKind::TopIndex => Some(BlockKind::IndexPartition),
                    BlockKind::FilterIndex => Some(BlockKind::FilterPartition),
                    _ => None,
                };
                // Entries first (physical order: entries → restart array → trailer). Entries that
                // name another block are references to its canonical node, never copies.
                if *kind == BlockKind::Metaindex {
                    let meta = self.meta().map(|m| m.to_vec()).unwrap_or_default();
                    for e in meta {
                        let it = Item {
                            start: e.handle.offset,
                            size: e.handle.size,
                            trailer: BLOCK_TRAILER,
                            kind: ItemKind::Block { kind: self.meta_kind_for(&e.name), name: e.name.clone(), ordinal: None },
                        };
                        out.push(Child::new(format!("{} › {}", e.name, self.item_label(&it)), Self::item_node(&it)).role(ChildRole::Reference));
                    }
                }
                if let Some(t) = target {
                    let (_, res) = self.block_entries(h, *kind);
                    if let Ok((_, es, _)) = res {
                        let n = es.len();
                        let mut budget = LIST_CHECK_BYTES;
                        for e in es.iter().take(TREE_CHILD_CAP) {
                            if let Some((th, _, _)) = &e.handle {
                                let it = Item { start: th.offset, size: th.size, trailer: BLOCK_TRAILER, kind: ItemKind::Block { kind: t, name: String::new(), ordinal: None } };
                                let c = Child::new(format!("{} ≤ {} ›", self.item_label(&it), self.key_label(&e.key, true)), NodeKind::SstBlock { off: th.offset, size: th.size, kind: t });
                                let c = self.checked_child(c, *th, &mut budget);
                                out.push(c.role(ChildRole::Reference));
                            }
                        }
                        if n > TREE_CHILD_CAP {
                            out.push(
                                Child::new(format!("{} more — use \"Data blocks via index\" paging", n - TREE_CHILD_CAP), NodeKind::SstDataBlocks { skip: TREE_CHILD_CAP as u64 })
                                    .status(Status::Limited)
                                    .role(ChildRole::Reference),
                            );
                        }
                    }
                }
                if structured {
                    out.push(Child::new("Restart array / block footer", NodeKind::SstRestarts { off: *off, size: *size, kind: *kind }).leaf());
                }
                out.push(
                    Child::new(format!("Trailer (compression + checksum) {}", range(off + size, off + size + BLOCK_TRAILER)), NodeKind::SstTrailer { off: *off, size: *size, kind: *kind }).leaf(),
                );
                out
            }
            NodeKind::SstDataBlocks { skip } => {
                let (hs, more, _) = self.data_handles(*skip, DATA_PAGE, cancel);
                let mut out: Vec<Child> = Vec::with_capacity(hs.len() + 1);
                let mut budget = LIST_CHECK_BYTES;
                for (i, (h, k)) in hs.iter().enumerate() {
                    let it = Item { start: h.offset, size: h.size, trailer: BLOCK_TRAILER, kind: ItemKind::Block { kind: BlockKind::Data, name: String::new(), ordinal: Some(*skip + i as u64) } };
                    let c = Child::new(format!("{} ≤ {} ›", self.item_label(&it), self.key_label(k, true)), NodeKind::SstBlock { off: h.offset, size: h.size, kind: BlockKind::Data });
                    let c = self.checked_child(c, *h, &mut budget);
                    out.push(c.role(ChildRole::Reference));
                }
                if more {
                    out.push(Child::new(format!("Next {} data blocks ›", DATA_PAGE), NodeKind::SstDataBlocks { skip: skip + hs.len() as u64 }).role(ChildRole::View));
                }
                out
            }
            _ => Vec::new(),
        }
    }

    fn root_children(&mut self, cancel: &dyn Fn() -> bool) -> Vec<Child> {
        let Ok(_) = self.footer() else {
            return vec![Child::new("Footer", NodeKind::SstFooter).leaf().status(Status::Invalid).diag(self.footer_diagnostic().cloned())];
        };
        // Physical layout, file order (Footer last); then the views.
        let l = self.layout(cancel);
        let mut budget = LIST_CHECK_BYTES;
        let mut v: Vec<Child> = l.top().into_iter().map(|e| self.entry_child(&l, e, &mut budget)).collect();
        if !l.data_complete {
            v.push(
                Child::new(format!("Data blocks: {} ({})", l.listed_text(), l.note.clone().unwrap_or_default()), NodeKind::SstDataBlocks { skip: 0 }).role(ChildRole::Absent).status(Status::Limited),
            );
        }
        // View nodes claim the worst status of their rows, none while a row is not checked
        // (0019 §1; updated from the view when it is shown).
        let (all, data) = self.layout_tallies(&l);
        v.push(Child::new("Physical layout table (file order)", NodeKind::SstLayout).role(ChildRole::View).leaf().tally(all));
        if self.index_handle().is_some() {
            // The listed count when the index walk is complete; otherwise a lower bound only.
            let n = if l.data_complete { l.data_listed.to_string() } else { format!("≥{}", l.data_declared.unwrap_or(0).max(l.data_listed)) };
            // Counts the listed blocks (an incomplete index walk has its own Limited node).
            v.push(Child::new(format!("Data blocks via index ({n})"), NodeKind::SstDataBlocks { skip: 0 }).role(ChildRole::View).tally(data));
        }
        let unknown_meta = self.meta().is_err();
        let absent =
            |label: &str| {
                Child::new(format!("{label}: {}", if unknown_meta { "unknown (metaindex unreadable)" } else { "not present" }), NodeKind::SstProperties)
                    .role(ChildRole::Absent)
                    .status(if unknown_meta { Status::Unknown } else { Status::NotPresent })
            };
        if self.props_handle().is_none() {
            v.push(absent("Properties"));
        }
        if self.index_handle().is_none() {
            v.push(absent("Index"));
        }
        let meta = self.meta().map(|m| m.to_vec()).unwrap_or_default();
        let filters = meta.iter().any(|m| matches!(meta_kind(&m.name), BlockKind::Filter | BlockKind::FilterIndex) || m.is_legacy_filter());
        if !filters {
            v.push(absent("Filter"));
        }
        for (name, label) in [("rocksdb.range_del", "Range deletions"), ("rocksdb.compression_dict", "Compression dictionary")] {
            if self.meta_handle(name).is_none() {
                v.push(absent(label));
            }
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

    /// The view of node `k`, carrying the current row tallies of both table view nodes (review
    /// S2): a block checked by this view (opened, Back) updates "Physical layout table" and
    /// "Data blocks via index" in the tree at once. Cache only; the layout and its tallies are
    /// kept (`layout_tallies`), so this costs O(1) per view.
    pub fn view(&mut self, k: &NodeKind, cancel: &dyn Fn() -> bool) -> View {
        let mut v = self.view_of(k, cancel);
        if self.footer().is_ok() {
            let l = self.layout(cancel);
            let (all, data) = self.layout_tallies(&l);
            v.tallies = vec![(NodeKind::SstLayout, all), (NodeKind::SstDataBlocks { skip: 0 }, data)];
        }
        v
    }

    fn view_of(&mut self, k: &NodeKind, cancel: &dyn Fn() -> bool) -> View {
        match k {
            NodeKind::Root => self.root_view(cancel),
            NodeKind::SstFooter => match self.footer().cloned() {
                Ok(f) => {
                    let mut v = of_fields(format!("Footer @0x{:x} ({} B, format_version {})", f.offset, f.len, f.format_version), footer_fields(&f));
                    v.notes.push("Footer layout: table/format.cc (legacy 48 B, format_version>=1 53 B).".into());
                    v.diagnostics.extend(f.fields.iter().filter_map(|x| x.diag.as_deref().cloned()));
                    self.location(f.offset, f.offset + f.len, None, None, None, cancel).apply(&mut v);
                    v
                }
                Err(e) => with_error_diag(error_view("Footer", Status::Invalid, e), self.footer_diagnostic().cloned()),
            },
            NodeKind::SstProperties => of_fields(
                "Not present",
                vec![Field::new("structure", None, "This optional structure is not listed in the metaindex.").status(if self.meta().is_ok() { Status::NotPresent } else { Status::Unknown })],
            ),
            // Opening a block (or a part of it) is its single bounded check (design A8): the
            // block was just read for the view, so `block_check` answers from the cache.
            NodeKind::SstBlock { off, size, kind } => {
                let mut v = self.block_view(Handle { offset: *off, size: *size }, *kind);
                self.block_check(Handle { offset: *off, size: *size });
                let dec = v.spaces.first().map(|s| s.size);
                let (o, s) = (*off, *size);
                self.location(o, o + s + BLOCK_TRAILER, Some((o, o + s)), Some((o + s, o + s + BLOCK_TRAILER)), dec, cancel).apply(&mut v);
                v
            }
            NodeKind::SstTrailer { off, size, kind } => {
                let mut v = self.trailer_view(Handle { offset: *off, size: *size }, *kind);
                self.block_check(Handle { offset: *off, size: *size });
                let t = off + size;
                self.location(t, t + BLOCK_TRAILER, None, None, None, cancel).apply(&mut v);
                v
            }
            NodeKind::SstRestarts { off, size, kind } => {
                let v = self.restarts_view(Handle { offset: *off, size: *size }, *kind);
                self.block_check(Handle { offset: *off, size: *size });
                v
            }
            NodeKind::SstEntry { off, size, kind, idx } => {
                let v = self.entry_view(Handle { offset: *off, size: *size }, *kind, *idx);
                self.block_check(Handle { offset: *off, size: *size });
                v
            }
            NodeKind::SstDataBlocks { skip } => {
                let mut v = self.data_blocks_view(*skip, cancel);
                if *skip == 0 {
                    // All listed data blocks, not only this page (the tree node stands for all).
                    let l = self.layout(cancel);
                    v.tally = Some(self.layout_tallies(&l).1);
                }
                v
            }
            NodeKind::SstLayout => {
                let l = self.layout(cancel);
                let entries: Vec<Entry> = l.with_all_gaps();
                let mut v = self.layout_table("Physical layout (file order)", &l, &entries, 0);
                v.notes.insert(0, layout_coverage(&l).into());
                v.tally = Some(self.layout_tallies(&l).0);
                v
            }
            NodeKind::SstLayoutGroup { first, count } => {
                let l = self.layout(cancel);
                let entries = l.group(*first as usize, *count as usize);
                let mut v = self.layout_table(&format!("Layout group: items {}..{} (file order)", first, first + count - 1), &l, &entries, l.end_before(*first as usize));
                if let (Some(a), Some(z)) = (l.items.get(*first as usize), l.items.get((first + count - 1) as usize)) {
                    v.location = vec![WhereLine { text: format!("{} · {count} structures", Span::file(a.start, z.end() - a.start).range_text()), link: None }];
                    v.extent = Some(Span::file(a.start, z.end() - a.start));
                }
                v
            }
            NodeKind::SstGap { start, end } => {
                let (a, b) = (*start, *end);
                let f = Field::new("unaccounted bytes", Some(Span::file(a, b - a)), format!("{} B", b - a))
                    .status(Status::Unknown)
                    .enc("bytes")
                    .explain("No handle in the footer, metaindex or index names these bytes. RocksVision does not guess what they are.");
                let mut v = of_fields(format!("Unaccounted bytes {}", range(a, b)), vec![f]);
                self.location(a, b, None, None, None, cancel).apply(&mut v);
                v
            }
            _ => error_view("Unsupported node", Status::Unsupported, "node not handled by the SST inspector"),
        }
    }

    /// A table of layout entries: Start | End (excl.) | Stored | Contents | Trailer | Gap before |
    /// Codec | Structure; each row links to the canonical node. `from`: end of what precedes
    /// the first entry in the file (0 for the whole file; a group page's previous items).
    fn layout_table(&mut self, title: &str, l: &PhysLayout, entries: &[Entry], from: u64) -> View {
        use ColumnRole as R;
        let mut rows = Vec::new();
        let gaps = l.gaps_before(entries, from);
        let mut codecs_read = 0usize;
        for (n, e) in entries.iter().enumerate() {
            // Status from the check cache only: never Present for a block that was not checked.
            let plain = |s: Status| RowCheck { status: s, cell: st_cell(s), not_checked: None, note: None, diag: None };
            let (start, end, contents, trailer, structure, node, chk) = match *e {
                Entry::Item(i) => {
                    let it = &l.items[i];
                    (it.start, it.end(), it.size, it.trailer, self.item_label(it), Self::item_node(it), self.item_check(it))
                }
                Entry::Group { first, count } => {
                    let (a, z) = (&l.items[first], &l.items[first + count - 1]);
                    let (chk, c) = self.group_row(l, first, count);
                    (
                        a.start,
                        z.end(),
                        z.end() - a.start,
                        0,
                        with_coverage(&format!("{} · {count} blocks", group_head(l, first, count)), Some(c)),
                        NodeKind::SstLayoutGroup { first: first as u64, count: count as u64 },
                        chk,
                    )
                }
                Entry::Gap { start, end } => (start, end, end - start, 0, format!("Unaccounted bytes {}", range(start, end)), NodeKind::SstGap { start, end }, plain(Status::Unknown)),
                Entry::Invalid(i) => {
                    let it = &l.invalid[i];
                    (it.start, it.end(), it.size, it.trailer, format!("{} · handle beyond footer/file", self.item_label(it)), Self::item_node(it), plain(Status::Invalid))
                }
            };
            let invalid = matches!(e, Entry::Invalid(_));
            let gap = gaps[n].map_or_else(|| "-".to_string(), |g| g.to_string());
            let codec = match *e {
                Entry::Item(i) if l.items[i].trailer > 0 && codecs_read < COMPRESSION_SAMPLE => {
                    codecs_read += 1;
                    self.block_ctype(l.items[i].handle()).map(|c| compression_name(c).to_string()).unwrap_or_else(|| "unreadable".into())
                }
                Entry::Item(i) if l.items[i].trailer > 0 => "not read".into(),
                _ => "-".into(),
            };
            let trailer_cell = if trailer > 0 { range(start + contents, end) } else { "-".into() };
            let meaning = match *e {
                Entry::Gap { start, end } => gap_meaning(start, end),
                Entry::Group { count, .. } => format!("{count} structures in {}", Span::file(start, end - start).range_text()),
                _ => structure_meaning(start, end, contents, trailer),
            };
            let fld = Field::new(structure.clone(), (!invalid).then(|| Span::file(start, end - start)), meaning)
                .kind(K::StructureRef)
                .status(chk.status)
                .not_checked(chk.not_checked)
                .check_note(chk.note)
                .diag(chk.diag)
                .link(node)
                .explain("Physical structure in file order; double-click (or Enter) to go to it.");
            rows.push(Row {
                cells: vec![n.to_string(), format!("0x{start:x}"), format!("0x{end:x}"), (end - start).to_string(), contents.to_string(), trailer_cell, gap, codec, structure, chk.cell],
                field: fld,
            });
        }
        View {
            title: title.to_string(),
            columns: vec![
                col(R::Index, "#"),
                col(R::Offset, "Start"),
                col(R::Offset, "End (excl.)"),
                col(R::Len, "Stored"),
                col(R::Len, "Payload"),
                col(R::Size, "Trailer"),
                col(R::Len, "Gap before"),
                col(R::Kind, "Codec"),
                col(R::Summary, "Structure"),
                col(R::Status, "Status"),
            ],
            rows,
            ..Default::default()
        }
    }

    fn root_view(&mut self, cancel: &dyn Fn() -> bool) -> View {
        let len = self.len();
        let mut rows = vec![field_row(Field::new("file", None, self.path().display().to_string())), size_row(len)];
        let mut f = Vec::new();
        let footer_ok = self.footer().is_ok();
        match self.footer().cloned() {
            Ok(ft) => {
                f.push(Field::new("table format", None, "BlockBasedTable"));
                let fv_span = if ft.legacy { None } else { Some(Span::file(ft.offset + ft.len - 12, 4)) };
                let fv_field = Field::new("format_version", fv_span, ft.format_version.to_string()).kind(K::FormatVersion).dv(DV::U32(ft.format_version)).link(NodeKind::SstFooter);
                f.push(if ft.legacy { fv_field.status(Status::Implicit).explain("Inferred from the legacy magic number; not stored.") } else { fv_field });
                let ct_span = if ft.legacy { None } else { Some(Span::file(ft.offset, 1)) };
                let ct = Field::new("checksum type", ct_span, footer::checksum_name(ft.checksum_type))
                    .kind(K::ChecksumType)
                    .dv(DV::Enum { name: footer::checksum_name(ft.checksum_type), code: u64::from(ft.checksum_type) })
                    .link(NodeKind::SstFooter);
                f.push(if ft.legacy { ct.explain("Implied by the legacy magic number (kCRC32c); not stored.") } else { ct.enc("u8 (first footer byte)") });
            }
            Err(e) => f.push(Field::new("footer", None, e).status(Status::Invalid).diag(self.footer_diagnostic().cloned())),
        }
        // 0030 A7: an SST does not store its column family id or level; the MANIFEST of its
        // own directory does. Shown only for this directory + file number (`inspect`).
        f.extend(manifest_context_fields(self.manifest_context().unwrap_or(&SstManifestContext::Standalone), self.prop("rocksdb.column.family.name")));
        if self.has_props() {
            for (k, label) in [
                ("rocksdb.creating.db.identity", "producer DB id"),
                ("rocksdb.column.family.name", "column family"),
                ("rocksdb.num.entries", "entries (declared)"),
                ("rocksdb.num.data.blocks", "data blocks (declared)"),
                ("rocksdb.filter.policy", "filter policy"),
                ("rocksdb.comparator", "comparator"),
                ("rocksdb.block.based.table.index.type", "index type"),
                ("rocksdb.index.value.is.delta.encoded", "index values delta-encoded"),
                ("rocksdb.index.key.is.user.key", "index keys are user keys"),
                ("rocksdb.key.largest.seqno", "largest seqno (declared)"),
                ("rocksdb.compression", "Declared compression policy (properties: rocksdb.compression)"),
            ] {
                match self.prop(k) {
                    Some(v) => {
                        let mut fld = prop_field(label, k, v).explain(format!("From property {k}."));
                        if let Some(sp) = self.prop_span(k) {
                            fld.span = Some(sp);
                            fld.status = Status::Present;
                            fld.raw = hex_preview(v, 24);
                            fld.encoding = prop_decode(k, v).1.to_string();
                        }
                        f.push(fld);
                    }
                    None if k == "rocksdb.key.largest.seqno" => {}
                    None => f.push(Field::new(label, None, "property not stored").status(Status::NotPresent)),
                }
            }
        } else if self.props_handle().is_some() {
            f.push(Field::new("properties", None, "properties block listed but could not be decoded; index flags unknown").status(Status::Invalid));
        } else {
            f.push(Field::new("properties", None, "not listed in the metaindex").status(if self.meta().is_ok() { Status::NotPresent } else { Status::Unknown }));
        }
        let l = footer_ok.then(|| self.layout(cancel));
        if let Some(l) = &l {
            f.push(self.actual_compression(l));
            if let Some(kr) = self.key_range(l) {
                f.push(kr);
            }
        }
        if self.changed_since_open() {
            f.push(Field::new("file changed", None, "size or mtime differs from open time; reopen").status(Status::Unknown));
        }
        rows.extend(f.into_iter().map(field_row));
        let mut v = View { title: "SST file (block-based table)".into(), columns: super::common::fields_columns(), rows, ..Default::default() };
        v.notes.push("Compatibility target: RocksDB 10.4.2. Files from other versions decode only where layouts match.".into());
        if let Some(l) = l {
            // First-use clarity: the physical layout in file order, each row linking to its node.
            v.notes.push(layout_coverage(&l).into());
            for e in l.top() {
                // Status from the check cache only (design A6); a group's cell names its coverage.
                let plain = |s: Status| RowCheck { status: s, cell: st_cell(s), not_checked: None, note: None, diag: None };
                let (name, span, value, node, chk, group) = match e {
                    Entry::Item(i) => {
                        let it = &l.items[i];
                        let codec = if it.trailer > 0 { self.block_ctype(it.handle()).map(|c| format!(" · {}", compression_name(c))).unwrap_or_default() } else { String::new() };
                        (
                            self.item_label(it),
                            Some(Span::file(it.start, it.end() - it.start)),
                            format!("{}{codec}", structure_meaning(it.start, it.end(), it.size, it.trailer)),
                            Self::item_node(it),
                            self.item_check(it),
                            false,
                        )
                    }
                    Entry::Group { first, count } => {
                        // Same label as the tree child, from the one cache-only group check.
                        let (chk, cov) = self.group_row(&l, first, count);
                        let (a, z) = (l.items[first].start, l.items[first + count - 1].end());
                        let label = format!("{} · {} · {count} blocks", group_head(&l, first, count), range(a, z));
                        (
                            with_coverage(&label, Some(cov)),
                            Some(Span::file(a, z - a)),
                            format!("{count} structures in {}", Span::file(a, z - a).range_text()),
                            NodeKind::SstLayoutGroup { first: first as u64, count: count as u64 },
                            chk,
                            true,
                        )
                    }
                    Entry::Gap { start, end } => (
                        format!("Unaccounted bytes {}", range(start, end)),
                        Some(Span::file(start, end - start)),
                        gap_meaning(start, end),
                        NodeKind::SstGap { start, end },
                        plain(Status::Unknown),
                        false,
                    ),
                    Entry::Invalid(i) => {
                        let it = &l.invalid[i];
                        (
                            format!("{} · handle beyond footer/file", self.item_label(it)),
                            None,
                            format!("{} · beyond the footer at file 0x{:x}", Span::file(it.start, it.end() - it.start).range_text(), l.footer_off),
                            Self::item_node(it),
                            plain(Status::Invalid),
                            false,
                        )
                    }
                };
                let fld = Field::new(name, span, value)
                    .kind(K::StructureRef)
                    .status(chk.status)
                    .not_checked(chk.not_checked)
                    .check_note(chk.note)
                    .diag(chk.diag)
                    .enc("physical structure")
                    .link(node)
                    .explain("Physical layout in file order; double-click (or Enter) to go to the structure.");
                let mut row = field_row(fld);
                // A group's Status cell names its coverage ("3 of 64 checked").
                if let (true, Some(last)) = (group, row.cells.last_mut()) {
                    *last = chk.cell;
                }
                v.rows.push(row);
            }
        }
        v
    }

    /// "Actual block compression": the trailer type byte of the data blocks (bounded sample),
    /// distinct from the declared policy property.
    fn actual_compression(&self, l: &PhysLayout) -> Field {
        let data: Vec<&Item> = l.items.iter().filter(|it| it.block_kind() == Some(BlockKind::Data)).collect();
        let mut counts: BTreeMap<String, u64> = BTreeMap::new();
        let mut read = 0u64;
        for it in data.iter().take(COMPRESSION_SAMPLE) {
            if let Some(c) = self.block_ctype(it.handle()) {
                *counts.entry(compression_name(c)).or_default() += 1;
                read += 1;
            }
        }
        let total = if l.data_complete { data.len().to_string() } else { format!("≥{}", data.len()) };
        let parts: Vec<String> = counts.iter().map(|(k, n)| format!("{n} {k}")).collect();
        // Reading the type byte is a read, never a check (design A1/A7).
        let value = format!("{} (type byte read for {read} of {total} data blocks)", if parts.is_empty() { "no data block read".to_string() } else { parts.join(", ") });
        Field::new("Actual block compression", None, value)
            .kind(K::CompressionType)
            .explain("Compression type byte of each data block's trailer (what the blocks actually use; meta blocks are usually stored uncompressed). Only the type byte is read here; block checksums are verified when a block is opened. The declared policy is the rocksdb.compression property.")
    }

    /// Smallest and largest internal key (first entry of the first data block, last entry of
    /// the last data block in index order): two block decodes.
    fn key_range(&mut self, l: &PhysLayout) -> Option<Field> {
        let mut data = l.items.iter().filter(|it| it.block_kind() == Some(BlockKind::Data));
        let first = data.clone().min_by_key(|it| match &it.kind {
            ItemKind::Block { ordinal, .. } => ordinal.unwrap_or(u64::MAX),
            _ => u64::MAX,
        })?;
        let last = data.by_ref().max_by_key(|it| match &it.kind {
            ItemKind::Block { ordinal, .. } => ordinal.unwrap_or(0),
            _ => 0,
        })?;
        let (fh, lh) = (first.handle(), last.handle());
        let (_, a) = self.block_entries(fh, BlockKind::Data);
        let lo = a.ok().and_then(|(_, es, _)| es.first().map(|e| e.key.clone()))?;
        let (_, b) = self.block_entries(lh, BlockKind::Data);
        let hi = b.ok().and_then(|(_, es, _)| es.last().map(|e| e.key.clone()))?;
        let value = format!("{} … {}", self.key_label(&lo, false), self.key_label(&hi, false));
        let complete = if l.data_complete { "" } else { " (index not fully listed: the last listed block)" };
        Some(
            Field::new("key range", None, value)
                .kind(K::InternalKey)
                .explain(format!("Derived: first key of data block #0 and last key of the last data block{complete}. Internal keys: user key @ sequence, type.")),
        )
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
        // "stored" = on-disk extent incl. the 5-byte trailer (same N as the tree label and
        // `where:`); "decompressed" only for compressed blocks.
        let decompressed = if b.compressed { format!(", decompressed {} B", b.data.len()) } else { String::new() };
        let mut v = View {
            title: format!("{} @0x{:x} (stored {} B{decompressed})", kind.label(), h.offset, h.size + BLOCK_TRAILER),
            spaces: space_info(&b),
            diagnostics: b.diags.clone(),
            ..Default::default()
        };
        if b.checksum_status == Status::Invalid {
            // Entries are still listed for inspection, but the block as a whole is corrupt.
            v.status = Some(Status::Invalid);
            v.notes.push(Note::with_status(Status::Invalid, format!("{} Block checksum MISMATCH: decoded entries below come from corrupt bytes.", Status::Invalid.symbol())));
        }
        v.notes.push(
            format!(
                "Compression {} · checksum {} · offsets {}",
                compression_name(b.ctype),
                b.checksum_status.label(),
                if b.compressed { "are block-local (decompressed)" } else { "are file offsets" }
            )
            .into(),
        );
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
            if let Some(e) = err.as_mut()
                && (e.status == Status::Invalid || e.status == Status::Truncated)
            {
                e.status = Status::Unknown;
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
            BlockKind::Metaindex => {
                vec![col(R::Index, "#"), col(R::Offset, "Offset"), col(R::Count, "Shared"), col(R::Count, "Non-shared"), col(R::Name, "Name"), col(R::Handle, "Handle"), col(R::Status, "Status")]
            }
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
        let prefix = if b.compressed { "block+" } else { "" };
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
                    // Meaning: the readable key (design D1); the raw key bytes go under "Other
                    // byte interpretations" (D4). Cells (and so the dump) are unchanged.
                    fld.value = ikey_meaning(&e.key);
                    let n = e.key.len().min(64);
                    let more = if e.key.len() > n { format!("… · {} B", e.key.len()) } else { String::new() };
                    fld.interp.push(("internal key", format!("0x{}{more}", crate::util::hex(&e.key[..n]).replace(' ', ""))));
                    let uk = split_ikey(&e.key).map(|x| x.0).unwrap_or(&e.key);
                    fld.interp.push(("user key", hex_preview(uk, 64)));
                    let (uk, seq, ty) = self.split_key(&e.key, false);
                    let vp = if kind == BlockKind::RangeDel { format!("end: {}", bytes_preview(val, 64)) } else { bytes_preview(val, 64) };
                    vec![
                        e.idx.to_string(),
                        off_cell,
                        if e.restart { "R".into() } else { String::new() },
                        e.shared.to_string(),
                        e.non_shared.to_string(),
                        e.value_len.to_string(),
                        uk,
                        seq,
                        ty,
                        vp,
                        st_cell(cell_status),
                    ]
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
        f.push(
            Field::new("shared", Some(b.span(p, n)), e.shared.to_string())
                .kind(K::Length)
                .dv(DV::VarInt(u64::from(e.shared)))
                .raw(&d[p..p + n])
                .enc("varint32")
                .status(shared_status)
                .diag(ediag.clone())
                .explain("Bytes reused from the previous key (0 at restart points)."),
        );
        p += n;
        let n = n_non_shared;
        f.push(
            Field::new("non_shared", Some(b.span(p, n)), e.non_shared.to_string())
                .kind(K::Length)
                .dv(DV::VarInt(u64::from(e.non_shared)))
                .raw(&d[p..p + n])
                .enc("varint32")
                .explain("Bytes of key suffix stored in this entry."),
        );
        p += n;
        match e.value_len_field {
            Some(vl) => {
                let n = n_value_len;
                f.push(Field::new("value_length", Some(b.span(p, n)), vl.to_string()).kind(K::Length).dv(DV::VarInt(u64::from(vl))).raw(&d[p..p + n]).enc("varint32"));
            }
            None => f.push(Field::new("value_length", None, "not stored (delta-encoded index value, format_version>=4)").status(Status::NotPresent)),
        }
        let suffix = &d[e.key_off..e.key_off + e.non_shared as usize];
        f.push(
            Field::new("key delta (stored)", Some(b.span(e.key_off, suffix.len())), bytes_preview(suffix, 96))
                .kind(K::Key)
                .raw(suffix)
                .value_bytes(suffix)
                .enc("bytes")
                .explain("Stored non-shared key suffix."),
        );
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
                f.push(
                    Field::new("sequence", tspan, s.to_string())
                        .kind(K::Sequence)
                        .dv(DV::U64(s))
                        .enc("fixed64 LE packed >> 8")
                        .explain(format!("Packed trailer 0x{packed:016x} = seq << 8 | type (dbformat.h PackSequenceAndType).")),
                );
                f.push(
                    Field::new("value type", tspan.map(|sp| Span::new(sp.space, sp.offset, 1)), format!("{t} {}", tag_name(t)))
                        .kind(K::Tag)
                        .dv(DV::Enum { name: tag_name(t), code: u64::from(t) })
                        .enc("low byte of packed trailer"),
                );
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
                let ex = if *delta { "Delta-encoded: offset = previous offset + previous size + 5 (trailer); size = previous size + delta (IndexValue::DecodeFrom)." } else { "Full BlockHandle." };
                f.push(
                    Field::new("block handle", vspan, format!("offset {} size {}", x.offset, x.size))
                        .kind(K::BlockHandle(HT::Block))
                        .dv(DV::Handle { offset: x.offset, size: x.size })
                        .raw(val)
                        .enc(enc)
                        .explain(ex)
                        .link(NodeKind::SstBlock { off: x.offset, size: x.size, kind: target }),
                );
                if let Some(k) = fk {
                    f.push(
                        Field::new("first key in block", None, self.key_label(k, false))
                            .raw(k)
                            .value_bytes(k)
                            .status(Status::Present)
                            .explain("kBinarySearchWithFirstKey: length-prefixed first internal key follows the handle."),
                    );
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
                if kind == BlockKind::Metaindex
                    && let Some((x, _)) = Handle::decode(val, 0)
                {
                    vf = vf.link(NodeKind::SstBlock { off: x.offset, size: x.size, kind: self.meta_kind_for(&name) });
                    vf.value = format!("offset {} size {}", x.offset, x.size);
                    vf.encoding = "BlockHandle varint64 ×2".into();
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
        let mut f = vec![
            Field::new("block contents", Some(Span::file(h.offset, h.size)), format!("{} bytes{}", h.size, if b.compressed { " compressed" } else { "" }))
                .enc("bytes")
                .explain("Physical bytes as stored in the file, before the 5-byte trailer."),
        ];
        if let Some((v, n)) = b.size_prefix {
            f.push(
                Field::new("uncompressed size prefix", Some(Span::file(h.offset, n as u64)), v.to_string())
                    .kind(K::Length)
                    .dv(DV::VarInt(v))
                    .enc("varint32/64")
                    .explain("compress_format_version 2: varint uncompressed size precedes the codec payload (all codecs except Snappy/XPRESS)."),
            );
        }
        if h.offset + h.size + 5 <= self.len() {
            let raw = self.trailer_bytes(h);
            f.push(Field::new("compression type", Some(Span::file(h.offset + h.size, 1)), format!("{} {}", b.ctype, compression_name(b.ctype))).kind(K::CompressionType).dv(DV::U8(b.ctype)).raw(&raw[..1.min(raw.len())]).enc("u8").explain("Actual block compression of this block (include/rocksdb/compression_type.h code), independent of the declared policy property rocksdb.compression. Part of the checksummed bytes."));
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
                        "{} over {} (payload + type byte). Computed {comp}. Context modifier = base 0x{base:08x} ^ (lo32(off)+hi32(off)) = 0x{:08x}; stored − modifier = 0x{:08x} -> {}.",
                        footer::checksum_name(ct),
                        Span::file(h.offset, h.size + 1).range_text(),
                        b.modifier,
                        b.stored_checksum.wrapping_sub(b.modifier),
                        b.checksum_status.label()
                    )),
            );
        } else {
            f.push(Field::new("trailer", None, "past end of file").status(Status::Truncated).diag(b.error_diagnostic().cloned()));
        }
        if b.compressed {
            f.push(
                Field::new("decompressed", None, format!("{} bytes", b.data.len()))
                    .status(if b.ok() { Status::Implicit } else { Status::Invalid })
                    .diag(b.error_diagnostic().cloned())
                    .explain("Derived bytes with their own block-local offset space (block+, BLOCK+ in the byte view). No file bytes of their own."),
            );
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
            f.push(
                Field::new(format!("restart[{i}]"), Some(b.span(p, 4)), format!("0x{v:x}"))
                    .kind(K::Offset)
                    .dv(DV::U32(v))
                    .raw(&d[p..p + 4])
                    .enc("fixed32 LE")
                    .explain("Block-local offset of an entry whose key is stored in full (shared = 0)."),
            );
        }
        if lay.num_restarts > RESTARTS_LISTED as u32 {
            f.push(Field::new("more restarts", None, format!("{} not listed", lay.num_restarts - RESTARTS_LISTED as u32)).status(Status::Limited));
        }
        match lay.hash {
            Some((s, nb)) => {
                f.push(
                    Field::new("hash index buckets", Some(b.span(s, nb as usize)), format!("{nb} buckets"))
                        .raw(&d[s..s + nb as usize])
                        .enc("u8 restart index each (255 empty, 254 collision)")
                        .explain("Data-block hash index (data_block_hash_index.h)."),
                );
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
                    f.push(
                        Field::new("metadata", meta_span, "new Bloom marker (-1)")
                            .raw(&d[m..])
                            .enc("5 bytes")
                            .explain("filter_policy.cc: byte0 = -1 newer Bloom, byte1 = sub-implementation, byte2 = block/probes."),
                    );
                    f.push(Field::new("sub-implementation", Some(b.span(m + 1, 1)), if sub == 0 { "0 FastLocalBloom".into() } else { format!("{sub} unknown") }).status(if sub == 0 {
                        Status::Present
                    } else {
                        Status::Unknown
                    }));
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
                // A link row (the block is shown at its canonical place in file order); its
                // status from the check cache only.
                let nc = self.checked_status(*h).is_none().then(|| {
                    format!("{NOT_CHECKED}: the index handle places this block at {}; its checksum is verified only when the block is opened.", range(h.offset, h.offset + h.size + BLOCK_TRAILER))
                });
                let st = self.checked_status(*h).unwrap_or(Status::Unknown);
                let cell = if nc.is_some() { NOT_CHECKED.to_string() } else { st_cell(st) };
                let fld = Field::new(format!("data block {}", skip as usize + i), Some(Span::file(h.offset, h.size + BLOCK_TRAILER)), format!("offset {} size {}", h.offset, h.size))
                    .kind(K::StructureRef)
                    .check_note(self.check_note(*h))
                    .status(st)
                    .not_checked(nc)
                    .dv(DV::Handle { offset: h.offset, size: h.size })
                    .link(NodeKind::SstBlock { off: h.offset, size: h.size, kind: BlockKind::Data })
                    .explain("Handle from the index; the separator key is ≥ every key in the block and < every key in the next.");
                Row { cells: vec![(skip as usize + i).to_string(), format!("0x{:x}", h.offset), h.size.to_string(), self.key_label(k, true), cell], field: fld }
            })
            .collect();
        use ColumnRole as R;
        let mut v = View {
            title: format!("Data blocks from #{skip}"),
            columns: vec![col(R::Index, "#"), col(R::Offset, "Offset"), col(R::Size, "Size"), col(R::Key, "Index separator key"), col(R::Status, "Status")],
            rows,
            ..Default::default()
        };
        if more {
            v.notes.push(format!("Showing {DATA_PAGE}; expand \"Next\" in the tree for more.").into());
        }
        if let Some(e) = err {
            v.notes.push(e.into());
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 0030 A7: MANIFEST rows (Implicit, no bytes, external-evidence explanation, CF agreement
    /// or disagreement with the SST property) and the unknown level with its reason.
    #[test]
    fn manifest_context_rows_and_unknown_reasons() {
        let e = SstManifestEvidence { manifest: "MANIFEST-000005".into(), number: 85, cf_id: 1, cf_name: "cf_users".into(), level: 1, read_at: Some(0) };
        let rows = manifest_context_fields(&SstManifestContext::Listed(e.clone()), Some(b"cf_users"));
        assert_eq!(
            rows.iter().map(|f| (&*f.name, &*f.value, f.status)).collect::<Vec<_>>(),
            [("level (MANIFEST)", "L1", Status::Implicit), ("column family (MANIFEST)", "1 cf_users", Status::Implicit)]
        );
        assert!(rows.iter().all(|f| f.span.is_none() && f.kind == K::ManifestEvidence && f.explain.contains("External MANIFEST evidence (MANIFEST-000005, live file 000085), not stored in the SST.")));
        assert_eq!(rows[0].explain.lines().next(), Some("The level that MANIFEST-000005 records for 000085.sst. An SST does not store its level."));
        assert!(rows[1].explain.ends_with("Agrees with the SST property rocksdb.column.family.name."));
        assert_eq!(rows[0].no_bytes_source.as_deref(), Some("from the folder's MANIFEST (read 1970-01-01 00:00:00 UTC)"));
        let differ = manifest_context_fields(&SstManifestContext::Listed(e), Some(b"cf_orders"));
        assert_eq!(differ[1].status, Status::Unknown);
        assert!(differ[1].explain.ends_with("MANIFEST lists it under 1 cf_users; the SST property says cf_orders. Not reconciled."));
        let one = |c: SstManifestContext| manifest_context_fields(&c, None).into_iter().map(|f| (f.name, f.value, f.status)).collect::<Vec<_>>();
        assert_eq!(
            one(SstManifestContext::Standalone),
            [("level".to_string(), "unknown — a standalone SST has no MANIFEST context: no numbered name or no CURRENT next to it (only a MANIFEST records levels)".to_string(), Status::Unknown)]
        );
        assert!(one(SstManifestContext::NotListed { manifest: "MANIFEST-000005".into(), number: 99 })[0].1.contains("does not list 000099.sst as a live file"));
        assert!(one(SstManifestContext::Untrusted { manifest: "MANIFEST-000005".into() })[0].1.contains("could not be read or replayed completely"));
    }

    /// 0015 C3 GUI-5: a tree listing reads and checks blocks only while its byte budget lasts;
    /// beyond it a block is listed "not checked" (coverage 0 of 1) and nothing is read.
    #[test]
    fn listing_checks_stop_at_the_byte_budget() {
        let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sst-none/data.sst");
        let mut s = SstInspector::open(crate::io::RangeReader::open(&p).unwrap());
        let l = s.layout(&|| false);
        let (a, b) = (l.items[0].handle(), l.items[1].handle());
        let mut budget = a.size + BLOCK_TRAILER;
        let c0 = s.checked_child(Child::new("a", NodeKind::Root), a, &mut budget);
        let c1 = s.checked_child(Child::new("b", NodeKind::Root), b, &mut budget);
        assert_eq!((c0.status, c0.checked, budget), (Status::Present, None, 0));
        assert_eq!((c1.status, c1.checked), (Status::Unknown, Some((0, 1))));
        assert_eq!(s.checked_status(b), None, "nothing read beyond the budget");
    }

    /// 0017 D3/F3: blocks already in the check cache cost no listing budget, so listing the
    /// same page again (first blocks cached) checks the next blocks instead of stopping early.
    #[test]
    fn cached_blocks_do_not_consume_the_listing_budget() {
        let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sst-none/data.sst");
        let mut s = SstInspector::open(crate::io::RangeReader::open(&p).unwrap());
        let l = s.layout(&|| false);
        let hs: Vec<Handle> = l.items[..3].iter().map(|it| it.handle()).collect();
        let cost = |h: Handle| h.size + BLOCK_TRAILER;
        let budget0 = cost(hs[0]) + cost(hs[1]);
        assert!(cost(hs[2]) <= budget0, "fixture: block 2 fits the budget alone");
        let list = |s: &mut SstInspector| {
            let mut budget = budget0;
            hs.iter().map(|&h| s.checked_child(Child::new("x", NodeKind::Root), h, &mut budget)).map(|c| (c.status, c.checked)).collect::<Vec<_>>()
        };
        let first = list(&mut s);
        assert_eq!(first, [(Status::Present, None), (Status::Present, None), (Status::Unknown, Some((0, 1)))]);
        let second = list(&mut s);
        assert_eq!(second, [(Status::Present, None); 3], "cached blocks are free; block 2 is checked on the second listing");
    }

    /// 0017 D3 GUI: a corrupt block's layout row (and its listed tree child) carry the cached
    /// diagnostic of the check (checksum kind, expected/observed), not "not recorded".
    #[test]
    fn layout_row_of_a_checked_corrupt_block_carries_the_cached_diagnostic() {
        let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/corrupt/sst-bitflip-data-block.sst");
        let mut s = SstInspector::open(crate::io::RangeReader::open(&p).unwrap());
        let ck = footer::checksum_check(s.footer().as_ref().unwrap().checksum_type);
        let l = s.layout(&|| false);
        let bad = l.items.iter().position(|it| it.block_kind().is_some() && s.block_check(it.handle()).0 == Status::Invalid).expect("fixture has an invalid block");
        let h = l.items[bad].handle();
        let v = s.view(&NodeKind::SstLayout, &|| false);
        let row = v.rows.iter().find(|r| r.field.span == Some(Span::file(h.offset, h.size + BLOCK_TRAILER))).expect("layout row of the bad block");
        assert_eq!(row.field.status, Status::Invalid);
        let d = row.field.diag.as_deref().expect("cached diagnostic on the layout row");
        assert_eq!(d.check, ck);
        assert!(d.expected.is_some() && d.observed.is_some(), "{d:?}");
        let mut budget = 0;
        let c = s.checked_child(Child::new("x", NodeKind::Root), h, &mut budget);
        assert_eq!((c.status, c.diag.as_deref().map(|d| d.check)), (Status::Invalid, Some(ck)), "tree child from the cache");
    }
}
