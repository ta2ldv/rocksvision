//! Block-based SST decoding state, RocksDB 10.4.2: footer, metaindex, properties, bounded block
//! cache, index walking. Views/tree children are built by `present::sst`, which reads this state
//! only through the accessor methods below (reader, path, properties and caches are private).
//! Sources: table/format.{h,cc}, table/meta_blocks.cc (meta block names),
//! table/block_based/block_based_table_reader.cc (index type/flags from properties),
//! table/table_properties.cc (property names), table/block_based/filter_policy.cc (filter metadata).

pub mod block;
pub mod compress;
pub mod footer;
pub mod layout;

use crate::io::{RangeRead, RangeReader, read_vec};
use crate::limits::{BLOCK_CACHE_BYTES, BLOCK_CACHE_ITEMS, KNOWN_BLOCK_HANDLES, MAX_DECOMPRESSED};
use crate::model::{BlockKind, CheckKind, DecodedValue as DV, Diagnostic, RowTally, Space, Span, SstManifestContext, Status, Trust};
use crate::util::{fixed32, fixed64, varint64};
use block::{Block, BlockCache, Entry, EntryError, IndexFmt, Layout, LayoutError};
use footer::{BLOCK_TRAILER, Footer, Handle};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct MetaEntry {
    pub name: String,
    pub handle: Handle,
}

impl MetaEntry {
    /// "filter.<policy>": obsolete block-based filter (pre-full-filter), not decoded.
    pub fn is_legacy_filter(&self) -> bool {
        is_legacy_filter_name(&self.name)
    }
}

/// Metaindex name of an obsolete block-based filter ("filter.<policy>", table/meta_blocks.cc).
pub fn is_legacy_filter_name(name: &str) -> bool {
    name.starts_with("filter.")
}

/// Entries of one block as decoded for a view: the block, its layout and entries, and the
/// problem that stopped iteration (typed, with a diagnostic).
pub struct BlockDecode {
    pub block: Arc<Block>,
    pub result: Result<(Layout, Vec<Entry>, Option<EntryError>), EntryError>,
}

/// `data_handles` result: ((handle, index key) per data block, more after this page, error).
pub type DataHandles = (Vec<(Handle, Vec<u8>)>, bool, Option<String>);

pub struct SstInspector {
    r: Box<dyn RangeRead>,
    /// Source path (display, sibling lookups); empty for in-memory sources.
    path: PathBuf,
    /// Public as a shim for tests; read it through `footer()`.
    pub footer: Result<Footer, String>,
    /// Footer problem as data (None when the footer decoded).
    footer_diag: Option<Diagnostic>,
    /// Public as a shim for tests; read it through `meta()`.
    pub meta: Result<Vec<MetaEntry>, String>,
    props: Option<BTreeMap<String, Vec<u8>>>,
    /// Where each property value is stored (properties block contents space).
    prop_spans: BTreeMap<String, Span>,
    /// Physical layout (built on first use; index reads only, bounded).
    layout: Option<Arc<layout::PhysLayout>>,
    cache: BlockCache,
    dict: Option<Option<Arc<Vec<u8>>>>,
    /// Handles of blocks decoded so far (bounded), so evicted blocks can be re-read for the hex view.
    known: std::collections::HashMap<u64, (Handle, BlockKind)>,
    /// Integrity status of blocks already checked (bounded): group nodes report coverage
    /// from it without reading any block.
    /// Value: (status, checksum actually compared, the check's diagnostic when it failed).
    checked: std::collections::HashMap<(u64, u64), CheckedBlock>,
    /// Row tallies of the two table views over the cached layout (`layout_tallies`).
    /// Boxed: keeps `Inspector::Sst` small (clippy::large_enum_variant).
    tallies: Option<Box<LayoutTallies>>,
    /// MANIFEST evidence of this file (0030 A7), looked up once by `inspect` (which owns the
    /// sibling-file access): None = not looked up yet; Some(None) = no evidence.
    manifest_context: Option<Box<SstManifestContext>>,
    /// Full tally scans so far (bounded-cost test).
    #[cfg(test)]
    tally_scans: u64,
}

/// A remembered block check: (status, checksum actually compared, diagnostic if any).
type CheckedBlock = (Status, bool, Option<Diagnostic>);

/// Status counts of "Physical layout table" (`all`) and "Data blocks via index" (`data`) over
/// one layout (0019 §1). Built by one scan of the layout, then kept current by `block_check`
/// when a block's status changes, so attaching them to every view costs O(1), not O(blocks)
/// (0020 D1, P3-F1: a 400-node dump of a 100 MB SST scanned all blocks twice per view).
struct LayoutTallies {
    layout: Arc<layout::PhysLayout>,
    all: RowTally,
    data: RowTally,
}

/// table_properties.cc / meta_blocks.cc: properties written with PutVarint64.
const U64_PROPS: &[&str] = &[
    "rocksdb.original.file.number",
    "rocksdb.raw.key.size",
    "rocksdb.raw.value.size",
    "rocksdb.data.size",
    "rocksdb.index.size",
    "rocksdb.index.partitions",
    "rocksdb.top-level.index.size",
    "rocksdb.index.key.is.user.key",
    "rocksdb.index.value.is.delta.encoded",
    "rocksdb.num.entries",
    "rocksdb.num.filter_entries",
    "rocksdb.deleted.keys",
    "rocksdb.merge.operands",
    "rocksdb.num.range-deletions",
    "rocksdb.num.data.blocks",
    "rocksdb.filter.size",
    "rocksdb.format.version",
    "rocksdb.fixed.key.length",
    "rocksdb.column.family.id",
    "rocksdb.creation.time",
    "rocksdb.oldest.key.time",
    "rocksdb.newest.key.time",
    "rocksdb.file.creation.time",
    "rocksdb.sample_for_compression.fast.data.size",
    "rocksdb.sample_for_compression.slow.data.size",
    "rocksdb.tail.start.offset",
    "rocksdb.user.defined.timestamps.persisted",
    "rocksdb.key.largest.seqno",
];

/// Typed value of a table property, by the encoding its name implies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropValue {
    /// PutVarint64 property.
    U64(u64),
    /// Named as a varint64 property but not exactly one varint64.
    BadU64,
    /// rocksdb.block.based.table.index.type (fixed32), None if shorter than 4 bytes.
    IndexType(Option<u32>),
    Fixed64(Option<u64>),
    Fixed32(Option<u32>),
    /// "0"/"1" string flag.
    Flag,
    /// Opaque bytes / string.
    Bytes,
}

pub fn prop_value(name: &str, v: &[u8]) -> PropValue {
    if U64_PROPS.contains(&name) {
        return match varint64(v, 0) {
            Some((x, n)) if n == v.len() => PropValue::U64(x),
            _ => PropValue::BadU64,
        };
    }
    match name {
        "rocksdb.block.based.table.index.type" => PropValue::IndexType(fixed32(v, 0)),
        "rocksdb.external_sst_file.global_seqno" => PropValue::Fixed64(fixed64(v, 0)),
        "rocksdb.external_sst_file.version" => PropValue::Fixed32(fixed32(v, 0)),
        "rocksdb.block.based.table.whole.key.filtering" | "rocksdb.block.based.table.prefix.filtering" | "rocksdb.block.based.table.decoupled.partitioned.filters" => PropValue::Flag,
        _ => PropValue::Bytes,
    }
}

pub fn index_type_name(t: u32) -> &'static str {
    match t {
        0 => "kBinarySearch",
        1 => "kHashSearch",
        2 => "kTwoLevelIndexSearch (partitioned)",
        3 => "kBinarySearchWithFirstKey",
        _ => "unknown",
    }
}

pub fn meta_kind(name: &str) -> BlockKind {
    match name {
        "rocksdb.properties" | "rocksdb.stats" => BlockKind::Properties,
        "rocksdb.index" => BlockKind::Index,
        "rocksdb.range_del" => BlockKind::RangeDel,
        "rocksdb.compression_dict" => BlockKind::CompressionDict,
        n if n.starts_with("partitionedfilter.") => BlockKind::FilterIndex,
        n if n.starts_with("fullfilter.") => BlockKind::Filter,
        // "filter." = obsolete block-based filter (pre-full-filter); not decoded -> Meta (Unsupported).
        _ => BlockKind::Meta,
    }
}

impl SstInspector {
    /// Shim for the former file-only constructor.
    pub fn open(r: RangeReader) -> Self {
        let path = r.path().to_path_buf();
        Self::from_reader(Box::new(r), path)
    }

    pub fn set_manifest_context(&mut self, e: SstManifestContext) {
        self.manifest_context = Some(Box::new(e));
    }
    /// The MANIFEST context of this file (0030 A7); None until `inspect` looked it up.
    pub fn manifest_context(&self) -> Option<&SstManifestContext> {
        self.manifest_context.as_deref()
    }

    pub fn from_reader(r: Box<dyn RangeRead>, path: PathBuf) -> Self {
        let len = r.len();
        let tail_len = len.min(footer::NEW_FOOTER);
        let (footer, footer_diag) = match read_vec(&*r, len - tail_len, tail_len) {
            Ok(t) => match footer::decode(&t, len) {
                Ok(f) => (Ok(f), None),
                Err(e) => (Err(e.to_string()), Some(e.diagnostic(len))),
            },
            Err(e) => {
                let m = format!("read error: {e}");
                (Err(m.clone()), Some(Diagnostic::new(CheckKind::Truncation, Some(Span::file(len - tail_len, tail_len)), Trust::Untrusted, "the file is not decoded", m)))
            }
        };
        let mut s = SstInspector {
            r,
            path,
            footer,
            footer_diag,
            meta: Err("footer not decoded".into()),
            props: None,
            prop_spans: BTreeMap::new(),
            layout: None,
            cache: BlockCache::new(BLOCK_CACHE_ITEMS, BLOCK_CACHE_BYTES),
            dict: None,
            known: std::collections::HashMap::new(),
            checked: std::collections::HashMap::new(),
            tallies: None,
            manifest_context: None,
            #[cfg(test)]
            tally_scans: 0,
        };
        if let Ok(f) = s.footer.clone() {
            let b = s.block(f.metaindex, BlockKind::Metaindex);
            // Read (and so checksum-verified) at open: recorded as checked (cache hit, no read).
            s.block_check(f.metaindex);
            s.meta = match (&b.error, block::layout(&b.data)) {
                (Some((_, e)), _) => Err(e.clone()),
                (None, Err(e)) => Err(e.to_string()),
                (None, Ok(lay)) => {
                    let (es, err) = block::entries(&b.data, &lay, None);
                    let v: Vec<MetaEntry> =
                        es.iter().filter_map(|e| Handle::decode(&b.data, e.value_off).map(|(h, _)| MetaEntry { name: String::from_utf8_lossy(&e.key).into_owned(), handle: h })).collect();
                    match err {
                        Some(e) if v.is_empty() => Err(e.msg),
                        _ => Ok(v),
                    }
                }
            };
            if let Some(h) = s.props_handle() {
                let b = s.block(h, BlockKind::Properties);
                s.block_check(h);
                if let Ok(lay) = block::layout(&b.data) {
                    let (es, _) = block::entries(&b.data, &lay, None);
                    s.props = Some(es.iter().map(|e| (String::from_utf8_lossy(&e.key).into_owned(), b.data[e.value_off..e.value_off + e.value_len].to_vec())).collect());
                    s.prop_spans = es.iter().map(|e| (String::from_utf8_lossy(&e.key).into_owned(), b.span(e.value_off, e.value_len))).collect();
                }
            }
        }
        s
    }

    // ---------------- accessors (the presenter API) ----------------

    pub fn reader(&self) -> &dyn RangeRead {
        &*self.r
    }
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
    /// File length observed at open time.
    pub fn len(&self) -> u64 {
        self.r.len()
    }
    pub fn is_empty(&self) -> bool {
        self.r.len() == 0
    }
    pub fn changed_since_open(&self) -> bool {
        self.r.changed_since_open()
    }
    pub fn footer(&self) -> Result<&Footer, &str> {
        self.footer.as_ref().map_err(|e| e.as_str())
    }
    pub fn footer_diagnostic(&self) -> Option<&Diagnostic> {
        self.footer_diag.as_ref()
    }
    pub fn meta(&self) -> Result<&[MetaEntry], &str> {
        self.meta.as_deref().map_err(|e| e.as_str())
    }
    /// True when the properties block was decoded.
    pub fn has_props(&self) -> bool {
        self.props.is_some()
    }
    /// Raw value of a table property (None if absent or the block was not decoded).
    pub fn prop(&self, name: &str) -> Option<&[u8]> {
        self.props.as_ref()?.get(name).map(|v| v.as_slice())
    }
    /// Bytes of a table property's value (file offsets, or the properties block's decompressed
    /// space when it was compressed).
    pub fn prop_span(&self, name: &str) -> Option<Span> {
        self.prop_spans.get(name).copied()
    }
    /// Physical layout of the table: every known structure by file offset (bounded index
    /// walk; cached once complete). See `layout::PhysLayout`.
    pub fn layout(&mut self, cancel: &dyn Fn() -> bool) -> Arc<layout::PhysLayout> {
        if let Some(l) = &self.layout {
            return l.clone();
        }
        let l = Arc::new(layout::build(self, cancel));
        if !l.cancelled {
            self.layout = Some(l.clone());
        }
        l
    }
    /// Compression type byte of block `h` (first trailer byte), if readable.
    pub fn block_ctype(&self, h: Handle) -> Option<u8> {
        read_vec(&*self.r, h.offset + h.size, 1).ok().and_then(|b| b.first().copied())
    }
    /// The 5 trailer bytes (compression type + checksum) after block `h`, bounded by the file.
    pub fn trailer_bytes(&self, h: Handle) -> Vec<u8> {
        read_vec(&*self.r, h.offset + h.size, BLOCK_TRAILER).unwrap_or_default()
    }
    /// Restart array / footer layout of a decoded block's contents.
    pub fn layout_of(b: &Block) -> Result<Layout, LayoutError> {
        block::layout(&b.data)
    }

    pub fn meta_handle(&self, name: &str) -> Option<Handle> {
        self.meta.as_ref().ok()?.iter().find(|m| m.name == name).map(|m| m.handle)
    }

    /// Properties block ("rocksdb.properties", or the older "rocksdb.stats").
    pub fn props_handle(&self) -> Option<Handle> {
        self.meta_handle("rocksdb.properties").or_else(|| self.meta_handle("rocksdb.stats"))
    }

    fn cfv(&self) -> u32 {
        match &self.footer {
            Ok(f) if f.format_version >= 2 => 2,
            _ => 1,
        }
    }

    pub fn prop_u64(&self, name: &str) -> Option<u64> {
        self.props.as_ref()?.get(name).and_then(|v| varint64(v, 0)).map(|x| x.0)
    }

    pub fn index_type(&self) -> u32 {
        self.props.as_ref().and_then(|p| p.get("rocksdb.block.based.table.index.type")).and_then(|v| fixed32(v, 0)).unwrap_or(0)
    }

    pub fn index_fmt(&self) -> IndexFmt {
        IndexFmt { value_delta: self.prop_u64("rocksdb.index.value.is.delta.encoded") == Some(1), first_key: self.index_type() == 3 }
    }

    /// block_based_table_reader.cc: index_key_includes_seq = (index_key_is_user_key == 0).
    pub fn index_key_is_user(&self) -> bool {
        self.prop_u64("rocksdb.index.key.is.user.key") == Some(1)
    }

    pub fn index_handle(&self) -> Option<Handle> {
        match &self.footer {
            Ok(f) => f.index.or_else(|| self.meta_handle("rocksdb.index")),
            Err(_) => None,
        }
    }

    pub fn meta_kind_for(&self, name: &str) -> BlockKind {
        if name == "rocksdb.index" { self.index_kind() } else { meta_kind(name) }
    }

    pub fn index_kind(&self) -> BlockKind {
        if self.index_type() == 2 { BlockKind::TopIndex } else { BlockKind::Index }
    }

    fn dict(&mut self) -> Option<Arc<Vec<u8>>> {
        if self.dict.is_none() {
            let d = self.meta_handle("rocksdb.compression_dict").map(|h| self.block(h, BlockKind::CompressionDict)).filter(|b| b.ok()).map(|b| b.data.clone());
            self.dict = Some(d);
        }
        self.dict.clone().flatten()
    }

    /// Reads (cached, bounded) a block. Only data blocks use the compression dictionary.
    pub fn block(&mut self, h: Handle, kind: BlockKind) -> Arc<Block> {
        if let Some(b) = self.cache.get(h.offset)
            && b.handle == h
        {
            return b;
        }
        let Ok(f) = self.footer.clone() else {
            return Arc::new(Block {
                handle: h,
                ctype: 0,
                stored_checksum: 0,
                computed_checksum: None,
                modifier: 0,
                checksum_status: Status::Unknown,
                data: Arc::new(Vec::new()),
                compressed: false,
                size_prefix: None,
                error: Some((Status::Invalid, "no footer".into())),
                diags: Vec::new(),
                error_diag: None,
            });
        };
        if self.known.len() >= KNOWN_BLOCK_HANDLES {
            self.known.clear();
        }
        self.known.insert(h.offset, (h, kind));
        let dict = if kind == BlockKind::Data { self.dict() } else { None };
        let b = Arc::new(block::read_block(&*self.r, &f, h, self.cfv(), dict.as_deref().map(|v| v.as_slice())));
        self.cache.put(h.offset, b.clone());
        b
    }

    /// Cheap integrity status for a tree node: trailer checksum only (no decompression),
    /// reusing the decoded block when it is cached.
    pub fn block_status(&mut self, h: Handle) -> Status {
        self.block_check(h).0
    }

    /// `block_status` plus the problem as data (None when the block verified). The result is
    /// remembered (bounded) for group coverage (`checked_status`), with whether a checksum was
    /// actually compared (`checksum_verified`).
    pub fn block_check(&mut self, h: Handle) -> (Status, Option<Diagnostic>) {
        let (st, d, verified) = self.block_check_uncached(h);
        if self.checked.len() >= KNOWN_BLOCK_HANDLES {
            self.checked.clear();
            // Every block is "not checked" again: the next view rescans.
            self.tallies = None;
        }
        let prev = self.checked.insert((h.offset, h.size), (st, verified, d.clone())).map(|c| c.0);
        if prev != Some(st) {
            self.retally(h, prev, st);
        }
        (st, d)
    }

    /// Status counts of the two table views over layout `l` (cache only, never reads; 0019
    /// §1): (every layout item and invalid handle, only the data blocks). Gaps are not counted.
    /// Scanned once per layout, then updated per check (`retally`).
    pub fn layout_tallies(&mut self, l: &Arc<layout::PhysLayout>) -> (RowTally, RowTally) {
        if let Some(t) = &self.tallies
            && Arc::ptr_eq(&t.layout, l)
        {
            return (t.all, t.data);
        }
        let (all, data) = (self.scan_tally(l, false), self.scan_tally(l, true));
        #[cfg(test)]
        {
            self.tally_scans += 1;
        }
        self.tallies = Some(Box::new(LayoutTallies { layout: l.clone(), all, data }));
        (all, data)
    }

    fn scan_tally(&self, l: &layout::PhysLayout, data_only: bool) -> RowTally {
        let mut t = RowTally::default();
        let wanted = |it: &layout::Item| !data_only || it.block_kind() == Some(BlockKind::Data);
        for it in l.items.iter().filter(|it| wanted(it)) {
            t.add(match it.block_kind() {
                None => Some(if self.footer.is_ok() { Status::Present } else { Status::Invalid }),
                Some(_) => self.checked_status(it.handle()),
            });
        }
        for _ in l.invalid.iter().filter(|it| wanted(it)) {
            t.add(Some(Status::Invalid));
        }
        t
    }

    /// Moves block `h` from `prev` (None: not checked) to `now` in the kept tallies, when it
    /// is a block of their layout (the Footer and invalid handles never change).
    fn retally(&mut self, h: Handle, prev: Option<Status>, now: Status) {
        let Some(t) = &mut self.tallies else { return };
        let Some(it) = t.layout.item_at_start(h.offset).map(|i| &t.layout.items[i]) else { return };
        let Some(kind) = it.block_kind().filter(|_| it.handle() == h) else { return };
        t.all.remove(prev);
        t.all.add(Some(now));
        if kind == BlockKind::Data {
            t.data.remove(prev);
            t.data.add(Some(now));
        }
    }

    /// Status of block `h` if it was already checked (never reads).
    pub fn checked_status(&self, h: Handle) -> Option<Status> {
        self.checked.get(&(h.offset, h.size)).map(|c| c.0)
    }

    /// Status and recorded diagnostic of block `h` if it was already checked (never reads).
    pub fn checked_result(&self, h: Handle) -> Option<(Status, Option<&Diagnostic>)> {
        self.checked.get(&(h.offset, h.size)).map(|c| (c.0, c.2.as_ref()))
    }

    /// The check of block `h` compared its stored checksum (and it matched when the status is
    /// Present). False for kNoChecksum files, an unknown checksum type or an unchecked block.
    pub fn checksum_verified(&self, h: Handle) -> bool {
        self.checked.get(&(h.offset, h.size)).is_some_and(|c| c.1)
    }

    /// (status, problem, checksum compared).
    fn block_check_uncached(&mut self, h: Handle) -> (Status, Option<Diagnostic>, bool) {
        if let Some(b) = self.cache.get(h.offset).filter(|b| b.handle == h) {
            let d = b.diags.first().cloned();
            return match &b.error {
                // The status comes from `error`, so does its diagnostic (a checksum mismatch
                // pushed earlier is not the problem behind this status).
                Some((st, _)) => (*st, b.error_diagnostic().cloned().or(d), false),
                None => match b.checksum_status {
                    Status::Invalid => (Status::Invalid, d, true),
                    Status::Present => (Status::Present, None, true),
                    // kNoChecksum: present, nothing compared.
                    Status::NotPresent => (Status::Present, None, false),
                    // Unknown checksum type: as the uncached path says.
                    s => (s, None, false),
                },
            };
        }
        let (st, d) = self.read_and_check(h);
        // Compared = a checksum type the file uses was computed and compared (match or mismatch).
        let ct = self.footer.as_ref().map(|f| f.checksum_type).unwrap_or(0);
        let compared = ct != 0 && (st == Status::Present || d.as_ref().is_some_and(|d| d.check == footer::checksum_check(ct)));
        (st, d, compared)
    }

    /// Reads block `h` with its trailer and compares the checksum (no decompression).
    fn read_and_check(&self, h: Handle) -> (Status, Option<Diagnostic>) {
        let Ok(f) = &self.footer else { return (Status::Unknown, None) };
        let (ct, base) = (f.checksum_type, f.base_context);
        let span = Some(Span::file(h.offset, h.size.saturating_add(BLOCK_TRAILER)));
        let rec = "the block contents are not decoded";
        let Some(end) = h.offset.checked_add(h.size).and_then(|x| x.checked_add(BLOCK_TRAILER)) else {
            return (Status::Invalid, Some(Diagnostic::new(CheckKind::Handle, None, Trust::Untrusted, rec, format!("block handle offset {} size {} overflows", h.offset, h.size))));
        };
        if end > self.r.len() {
            return (
                Status::Truncated,
                Some(
                    Diagnostic::new(CheckKind::Truncation, span, Trust::Untrusted, rec, format!("block handle offset {} size {} runs past file end {}", h.offset, h.size, self.r.len()))
                        .values(Some(DV::U64(end)), Some(DV::U64(self.r.len()))),
                ),
            );
        }
        if h.size > MAX_DECOMPRESSED {
            return (Status::Limited, Some(Diagnostic::new(CheckKind::Limit, span, Trust::Partial, rec, format!("block of {} bytes exceeds the {} byte cap", h.size, MAX_DECOMPRESSED))));
        }
        match read_vec(&*self.r, h.offset, h.size + BLOCK_TRAILER) {
            Ok(buf) if buf.len() as u64 == h.size + BLOCK_TRAILER => {
                if ct == 0 {
                    return (Status::Present, None);
                }
                let n = h.size as usize;
                let stored = fixed32(&buf, n + 1).unwrap_or(0);
                let modifier = footer::context_modifier(base, h.offset);
                match footer::builtin_checksum(ct, &buf[..=n]) {
                    Some(c) if stored.wrapping_sub(modifier) == c => (Status::Present, None),
                    Some(c) => (
                        Status::Invalid,
                        Some(
                            Diagnostic::new(footer::checksum_check(ct), span, Trust::Untrusted, "entries are still decoded for inspection but are not trusted", "Block checksum MISMATCH")
                                .values(Some(DV::U32(c.wrapping_add(modifier))), Some(DV::U32(stored)))
                                .modifier(modifier),
                        ),
                    ),
                    None => (Status::Unknown, None),
                }
            }
            Ok(_) => (Status::Truncated, Some(Diagnostic::new(CheckKind::Truncation, span, Trust::Untrusted, rec, "short read"))),
            Err(e) => (Status::Invalid, Some(Diagnostic::new(CheckKind::Truncation, span, Trust::Untrusted, rec, format!("read error: {e}")))),
        }
    }

    fn fmt_for(&self, kind: BlockKind) -> Option<IndexFmt> {
        match kind {
            BlockKind::Index | BlockKind::TopIndex | BlockKind::IndexPartition => Some(self.index_fmt()),
            BlockKind::FilterIndex => Some(IndexFmt { value_delta: self.index_fmt().value_delta, first_key: false }),
            _ => None,
        }
    }

    /// Decoded block entries (bounded), with typed errors.
    pub fn decode_entries(&mut self, h: Handle, kind: BlockKind) -> BlockDecode {
        let b = self.block(h, kind);
        if let Some((st, msg)) = &b.error {
            let check = b.diags.first().map(|d| d.check).unwrap_or(CheckKind::Truncation);
            let e = EntryError { status: *st, check, at: 0, msg: msg.clone() };
            return BlockDecode { block: b, result: Err(e) };
        }
        let result = match block::layout(&b.data) {
            Ok(lay) => {
                let (es, err) = block::entries(&b.data, &lay, self.fmt_for(kind));
                Ok((lay, es, err))
            }
            Err(e) => Err(EntryError { status: Status::Invalid, check: e.check, at: b.data.len().saturating_sub(4), msg: e.msg }),
        };
        BlockDecode { block: b, result }
    }

    /// Decoded block entries (bounded). Shim of `decode_entries` with (status, message) errors.
    #[allow(clippy::type_complexity)]
    pub fn block_entries(&mut self, h: Handle, kind: BlockKind) -> (Arc<Block>, Result<(Layout, Vec<Entry>, Option<(Status, String)>), (Status, String)>) {
        let d = self.decode_entries(h, kind);
        let res = match d.result {
            Ok((lay, es, err)) => Ok((lay, es, err.map(|e| e.pair()))),
            Err(e) => Err(e.pair()),
        };
        (d.block, res)
    }

    /// Data block handles in index order, starting at `skip`, at most `max`. Returns (handles, more).
    pub fn data_handles(&mut self, skip: u64, max: usize, cancel: &dyn Fn() -> bool) -> DataHandles {
        let Some(ih) = self.index_handle() else { return (Vec::new(), false, Some("no index handle".into())) };
        let mut out = Vec::new();
        let mut seen = 0u64;
        let top_kind = self.index_kind();
        let (_, res) = self.block_entries(ih, top_kind);
        let top = match res {
            Ok((_, es, _)) => es,
            Err((_, e)) => return (out, false, Some(e)),
        };
        let parts: Vec<(Handle, Vec<u8>)> = top.iter().filter_map(|e| e.handle.as_ref().map(|h| (h.0, e.key.clone()))).collect();
        let groups: Vec<Option<Handle>> = if top_kind == BlockKind::TopIndex { parts.iter().map(|p| Some(p.0)).collect() } else { vec![None] };
        for g in groups {
            if cancel() {
                return (out, true, Some("cancelled".into()));
            }
            let list: Vec<(Handle, Vec<u8>)> = match g {
                None => parts.clone(),
                Some(ph) => {
                    // Skip whole partitions by their entry count only after reading them (bounded per page).
                    let (_, r) = self.block_entries(ph, BlockKind::IndexPartition);
                    match r {
                        Ok((_, es, _)) => es.iter().filter_map(|e| e.handle.as_ref().map(|h| (h.0, e.key.clone()))).collect(),
                        Err((_, e)) => return (out, false, Some(format!("partition at {}: {e}", ph.offset))),
                    }
                }
            };
            for it in list {
                if seen >= skip {
                    if out.len() >= max {
                        return (out, true, None);
                    }
                    out.push(it);
                }
                seen += 1;
            }
        }
        (out, false, None)
    }

    pub fn space_bytes(&mut self, space: Space, off: u64, len: u64) -> Option<Vec<u8>> {
        match space {
            Space::File => read_vec(&*self.r, off, len).ok(),
            Space::Block(o) => {
                let b = match self.cache.get(o) {
                    Some(b) => b,
                    None => {
                        let (h, k) = *self.known.get(&o)?;
                        self.block(h, k) // evicted from the LRU: re-read and re-decompress
                    }
                };
                let s = (off as usize).min(b.data.len());
                let e = (off.saturating_add(len) as usize).min(b.data.len());
                Some(b.data[s..e].to_vec())
            }
            Space::Record(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::MemReader;

    /// Parser test without a file: a too-short in-memory source yields a footer error.
    #[test]
    fn in_memory_short_source_reports_footer_error() {
        let s = SstInspector::from_reader(Box::new(MemReader::new(vec![0u8; 20])), PathBuf::new());
        assert!(s.footer.is_err());
        assert!(s.meta.is_err());
    }

    /// 0017 D4: a cached block that both mismatches its checksum and fails decompression pairs
    /// the error's status with the error's diagnostic (not the checksum one pushed first).
    #[test]
    fn cached_failed_block_pairs_status_with_the_error_diagnostic() {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sst-zstd/data.sst");
        let orig = std::fs::read(&p).unwrap();
        let (hs, _, _) = SstInspector::open(RangeReader::open(&p).unwrap()).data_handles(0, 1, &|| false);
        let h = hs[0].0;
        // Flip one contents byte at a time until the block fails both checks (deterministic).
        let found = (8..h.size).find_map(|k| {
            let mut bytes = orig.clone();
            bytes[(h.offset + k) as usize] ^= 0xFF;
            let mut s = SstInspector::from_reader(Box::new(MemReader::new(bytes)), p.clone());
            let b = s.block(h, BlockKind::Data);
            (b.error.is_some() && b.checksum_status == Status::Invalid).then_some((s, b))
        });
        let (mut s, b) = found.expect("a ZSTD contents flip that fails checksum and decompression");
        let err = b.error_diagnostic().cloned().expect("error diagnostic");
        assert_ne!(b.diags.first(), Some(&err), "fixture: the checksum diagnostic comes first");
        let (st, d) = s.block_check(h);
        assert_eq!(st, b.error.as_ref().unwrap().0);
        assert_eq!(d.as_ref(), Some(&err), "status and diagnostic come from the same problem");
        assert_eq!(s.checked_result(h).and_then(|r| r.1), Some(&err));
    }

    /// 0020 D1 (P3-F1): the table-view tallies attached to every view are scanned once per
    /// layout and then follow each block check, so N views cost N lookups, not N × blocks; the
    /// kept counts always equal a fresh scan (including a block turning Invalid).
    #[test]
    fn layout_tallies_are_scanned_once_and_follow_every_check() {
        use crate::model::NodeKind;
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/corrupt/sst-bitflip-data-block.sst");
        let mut s = SstInspector::open(RangeReader::open(&p).unwrap());
        let never = || false;
        let l = s.layout(&never);
        let fresh = |s: &SstInspector| (s.scan_tally(&l, false), s.scan_tally(&l, true));
        s.view(&NodeKind::Root, &never);
        assert_eq!(s.tally_scans, 1);
        let blocks: Vec<_> = l.items.iter().filter_map(|it| Some((it.handle(), it.block_kind()?))).take(40).collect();
        assert!(blocks.len() > 10, "fixture has many blocks: {}", blocks.len());
        for &(h, kind) in &blocks {
            let v = s.view(&NodeKind::SstBlock { off: h.offset, size: h.size, kind }, &never);
            let (all, data) = fresh(&s);
            assert_eq!(v.tallies, vec![(NodeKind::SstLayout, all), (NodeKind::SstDataBlocks { skip: 0 }, data)], "block @{:#x}", h.offset);
        }
        let (all, data) = s.layout_tallies(&l);
        assert_eq!((all.invalid, data.invalid), (1, 1), "the flipped data block is counted once it is checked");
        assert_eq!(s.tally_scans, 1, "{} views, one scan", blocks.len() + 1);
    }
}
