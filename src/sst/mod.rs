//! Block-based SST decoding state, RocksDB 10.4.2: footer, metaindex, properties, bounded block
//! cache, index walking. Views/tree children are built by `present::sst`, which reads this state
//! only through the accessor methods below (reader, path, properties and caches are private).
//! Sources: table/format.{h,cc}, table/meta_blocks.cc (meta block names),
//! table/block_based/block_based_table_reader.cc (index type/flags from properties),
//! table/table_properties.cc (property names), table/block_based/filter_policy.cc (filter metadata).

pub mod block;
pub mod compress;
pub mod footer;

use crate::io::{read_vec, RangeRead, RangeReader};
use crate::limits::{BLOCK_CACHE_BYTES, BLOCK_CACHE_ITEMS, KNOWN_BLOCK_HANDLES, MAX_DECOMPRESSED};
use crate::model::{BlockKind, CheckKind, DecodedValue as DV, Diagnostic, Space, Span, Status, Trust};
use crate::util::{fixed32, fixed64, varint64};
use block::{Block, BlockCache, Entry, EntryError, IndexFmt, Layout, LayoutError};
use footer::{Footer, Handle, BLOCK_TRAILER};
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
        self.name.starts_with("filter.")
    }
}

/// Entries of one block as decoded for a view: the block, its layout and entries, and the
/// problem that stopped iteration (typed, with a diagnostic).
pub struct BlockDecode {
    pub block: Arc<Block>,
    pub result: Result<(Layout, Vec<Entry>, Option<EntryError>), EntryError>,
}

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
    cache: BlockCache,
    dict: Option<Option<Arc<Vec<u8>>>>,
    /// Handles of blocks decoded so far (bounded), so evicted blocks can be re-read for the hex view.
    known: std::collections::HashMap<u64, (Handle, BlockKind)>,
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
        let mut s = SstInspector { r, path, footer, footer_diag, meta: Err("footer not decoded".into()), props: None, cache: BlockCache::new(BLOCK_CACHE_ITEMS, BLOCK_CACHE_BYTES), dict: None, known: std::collections::HashMap::new() };
        if let Ok(f) = s.footer.clone() {
            let b = s.block(f.metaindex, BlockKind::Metaindex);
            s.meta = match (&b.error, block::layout(&b.data)) {
                (Some((_, e)), _) => Err(e.clone()),
                (None, Err(e)) => Err(e.to_string()),
                (None, Ok(lay)) => {
                    let (es, err) = block::entries(&b.data, &lay, None);
                    let v: Vec<MetaEntry> = es
                        .iter()
                        .filter_map(|e| Handle::decode(&b.data, e.value_off).map(|(h, _)| MetaEntry { name: String::from_utf8_lossy(&e.key).into_owned(), handle: h }))
                        .collect();
                    match err {
                        Some(e) if v.is_empty() => Err(e.msg),
                        _ => Ok(v),
                    }
                }
            };
            if let Some(h) = s.props_handle() {
                let b = s.block(h, BlockKind::Properties);
                if let Ok(lay) = block::layout(&b.data) {
                    let (es, _) = block::entries(&b.data, &lay, None);
                    s.props = Some(es.iter().map(|e| (String::from_utf8_lossy(&e.key).into_owned(), b.data[e.value_off..e.value_off + e.value_len].to_vec())).collect());
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
        if name == "rocksdb.index" {
            self.index_kind()
        } else {
            meta_kind(name)
        }
    }

    pub fn index_kind(&self) -> BlockKind {
        if self.index_type() == 2 {
            BlockKind::TopIndex
        } else {
            BlockKind::Index
        }
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
        if let Some(b) = self.cache.get(h.offset) {
            if b.handle == h {
                return b;
            }
        }
        let Ok(f) = self.footer.clone() else {
            return Arc::new(Block { handle: h, ctype: 0, stored_checksum: 0, computed_checksum: None, modifier: 0, checksum_status: Status::Unknown, data: Arc::new(Vec::new()), compressed: false, size_prefix: None, error: Some((Status::Invalid, "no footer".into())), diags: Vec::new(), error_diag: None });
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

    /// `block_status` plus the problem as data (None when the block verified).
    pub fn block_check(&mut self, h: Handle) -> (Status, Option<Diagnostic>) {
        if let Some(b) = self.cache.get(h.offset).filter(|b| b.handle == h) {
            let d = b.diags.first().cloned();
            return match &b.error {
                Some((st, _)) => (*st, d),
                None if b.checksum_status == Status::Invalid => (Status::Invalid, d),
                None => (Status::Present, None),
            };
        }
        let Ok(f) = &self.footer else { return (Status::Unknown, None) };
        let (ct, base) = (f.checksum_type, f.base_context);
        let span = Some(Span::file(h.offset, h.size.saturating_add(BLOCK_TRAILER)));
        let rec = "the block contents are not decoded";
        let Some(end) = h.offset.checked_add(h.size).and_then(|x| x.checked_add(BLOCK_TRAILER)) else {
            return (Status::Invalid, Some(Diagnostic::new(CheckKind::Handle, None, Trust::Untrusted, rec, format!("block handle offset {} size {} overflows", h.offset, h.size))));
        };
        if end > self.r.len() {
            return (Status::Truncated, Some(Diagnostic::new(CheckKind::Truncation, span, Trust::Untrusted, rec, format!("block handle offset {} size {} runs past file end {}", h.offset, h.size, self.r.len())).values(Some(DV::U64(end)), Some(DV::U64(self.r.len())))));
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
                        Some(Diagnostic::new(footer::checksum_check(ct), span, Trust::Untrusted, "entries are still decoded for inspection but are not trusted", "Block checksum MISMATCH").values(Some(DV::U32(c.wrapping_add(modifier))), Some(DV::U32(stored)))),
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
    pub fn data_handles(&mut self, skip: u64, max: usize, cancel: &dyn Fn() -> bool) -> (Vec<(Handle, Vec<u8>)>, bool, Option<String>) {
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
}
