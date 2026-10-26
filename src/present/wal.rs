//! Log-format presentation (WAL and MANIFEST files): lazy tree children and views built from
//! `format::log` items, `wal::batch` operations and `manifest` edits.
//!
//! Tree: "Physical blocks" (blocks → fragments / padding in file order; the only place a
//! physical item is expanded) and the labeled views "Logical records (reassembled view)" and,
//! for a MANIFEST, "Observed levels (derived view)". A fragment's status is about its own bytes
//! only; whether a record is complete is the record's `Reassembly`.

use super::common::{bytes_dec_hex, error_view, explained, of_fields, range, size_row, st_cell, st_word_cell, Location};
use crate::format::codec::compression_name;
use crate::format::log::{self, FragRole, Logical, OpenTail, Phys, PhysKind, Reassembly, BLOCK_SIZE};
use crate::limits::{LIST_KEEP_COMPRESSED, LIST_KEEP_MANIFEST, LIST_KEEP_WAL, LOG_BLOCKS_PER_GROUP as BLOCKS_PER_GROUP, MAX_OPS_LISTED, MAX_RECORD_BYTES, OPEN_TAIL_BLOCKS, PATH_RECORD_PAGES, RECORDS_PER_PAGE, ROOT_RECORDS};
use crate::manifest;
use crate::model::*;
use crate::model::{DecodedValue as DV, FieldKind as K};
use crate::util::bytes_preview;
use crate::wal::batch::{self, tag_name};
use crate::wal::{meta_facts, stale, LogInspector, LogMode};

const CRC_GUESS: &str = " CRC MISMATCH: log_reader.cc drops the rest of this block here; RocksVision locates the following items by trusting this record's length field, which is a guess.";

/// Explanation of a WriteBatch element (db/write_batch.cc, dbformat.h) for an operation with
/// `tag`, selected by the element's `FieldKind` (never by its display label).
fn batch_explain(tag: Option<u8>, f: &Fact) -> String {
    let s = match f.kind {
        K::Sequence if tag.is_none() => "Sequence number of the first operation in this batch (WriteBatchInternal::Sequence).",
        K::Count if tag.is_none() => "Number of sequence-consuming operations in the batch (WriteBatchInternal::Count).",
        K::Tag => "Record tag (dbformat.h ValueType). Column-family variants carry a varint32 CF id.",
        K::Cf if f.span.is_some() => "Column family id (varint32) present because the tag is a ColumnFamily* variant.",
        K::Cf => "Tag without CF id: the operation targets column family 0 (write_batch.cc sets *column_family = 0).",
        K::DerivedSequence => "Not stored per operation: batch sequence + index of sequence-consuming ops (assumes seq_per_batch=false).",
        K::Payload if f.status == Status::NotPresent => "This marker tag has no payload.",
        K::Payload if f.status == Status::Unknown => "write_batch.cc returns Corruption(\"unknown WriteBatch tag\"); the rest of the batch cannot be framed.",
        K::WriteTime if f.status == Status::Present => "db/seqno_to_time_mapping.cc ParsePackedValueWithWriteTime: last 8 bytes.",
        K::EntityColumn if f.aux.is_some() => {
            return format!("Column name at record offset 0x{:x}; value size from the index section.", f.aux.map(|a| a.offset).unwrap_or(0));
        }
        _ if f.status == Status::Limited => "Bounded preview: the record is longer than the bytes kept for this view.",
        k if f.status == Status::Present => slice_explain(tag.unwrap_or(0xff), k),
        _ => "",
    };
    s.to_string()
}

/// Length-prefixed slices of WriteBatch records, by tag (write_batch.cc ReadRecordFromWriteBatch).
fn slice_explain(tag: u8, kind: K) -> &'static str {
    match (tag, kind) {
        (0x1 | 0x5 | 0x2 | 0x6 | 0x10 | 0x11, K::Key) => "User key bytes (no internal-key suffix in the WAL).",
        (0x1 | 0x5 | 0x2 | 0x6 | 0x10 | 0x11, K::Value | K::BlobIndex) => "Value bytes as written (opaque to RocksDB).",
        (0xE | 0xF, K::BeginKey) => "Inclusive start of the deleted range.",
        (0xE | 0xF, K::EndKey) => "Exclusive end of the deleted range.",
        (0x0 | 0x4 | 0x7 | 0x8, K::Key) => "Deleted user key.",
        (0x3, K::Blob) => "LogData: opaque application data, not applied to the DB.",
        (0xA, K::Xid) => "Transaction id (EndPrepare).",
        (0x15, K::CommitTimestamp) => "Commit timestamp bytes (opaque).",
        (0x15, K::Xid) => "Transaction id (Commit).",
        (0xB | 0xC, K::Xid) => "Transaction id.",
        (0x16 | 0x17, K::Key) => "Entity key.",
        (0x16 | 0x17, K::Entity) => "Serialized wide-column entity (db/wide/wide_column_serialization.cc).",
        (0x18 | 0x19, K::Key) => "User key.",
        (0x18 | 0x19, K::PackedValue) => "Value followed by fixed64 unix write time (ParsePackedValueWithWriteTime).",
        _ => "",
    }
}

/// WriteBatch facts as table fields; `tag` is None for the batch header.
pub fn batch_fields(tag: Option<u8>, facts: &[Fact]) -> Vec<Field> {
    facts.iter().map(|f| explained(f.clone(), batch_explain(tag, f))).collect()
}

/// Problems at the end of a log file: diagnostics, the physically cut item (only the last
/// block can end early) and the record the end of file left open.
struct EofInfo {
    diags: Vec<Diagnostic>,
    /// Offset of the first item whose own bytes the end of file cuts.
    cut: Option<u64>,
    /// Number of physically cut items.
    cut_items: usize,
    /// A fragmented record left open (no LAST, or a cut continuation).
    open: Option<OpenTail>,
}

/// Tree path of block `b` below `LogBlocks { first: 0, count: n }`: the nested groups (same
/// grouping as `children`), excluding the top node and the block itself. Pure.
pub fn block_group_path(n: u64, b: u64) -> Vec<NodeKind> {
    let mut out = Vec::new();
    let (mut first, mut count) = (0u64, n);
    while count > BLOCKS_PER_GROUP && b >= first && b < first + count {
        let mut step = BLOCKS_PER_GROUP;
        while step * BLOCKS_PER_GROUP < count {
            step *= BLOCKS_PER_GROUP;
        }
        let s = first + (b - first) / step * step;
        let c = step.min(first + count - s);
        out.push(NodeKind::LogBlocks { first: s, count: c });
        (first, count) = (s, c);
    }
    out
}

/// Label of a physical item: offset first, then what it is and its ranges.
fn phys_label(p: &Phys) -> String {
    match p.kind {
        PhysKind::Record => format!("0x{:x} {} · header {} · payload {}", p.offset, log::type_name(p.rtype), range(p.offset, p.payload_off), range(p.payload_off, p.payload_off + p.payload_avail)),
        PhysKind::BlockTrailer => format!("0x{:x} Padding {}", p.offset, range(p.offset, p.end)),
        PhysKind::ZeroFill => format!("0x{:x} Zero fill {}", p.offset, range(p.offset, p.end)),
        PhysKind::BadLength => format!("0x{:x} Bad length ({}) {}", p.offset, log::type_name(p.rtype), range(p.offset, p.end)),
        PhysKind::Truncated if p.header_len > 0 => format!("0x{:x} {} cut by end of file · header {} · payload {} of {} B", p.offset, log::type_name(p.rtype), range(p.offset, p.payload_off), range(p.payload_off, p.payload_off + p.payload_avail), p.len),
        PhysKind::Truncated => format!("0x{:x} Truncated header {}", p.offset, range(p.offset, p.end)),
    }
}

/// "complete" / "incomplete (no LAST)" … for the record table.
fn record_status_cell(l: &Logical) -> String {
    st_word_cell(l.status, l.status_word())
}

/// Health of one 32 KiB block from its own physical items: the worst problem status (Present
/// when none) and the text suffix ("· 1 truncated fragment"). One typed result for the tree
/// node, the block table row, the block view chip and the dump.
fn block_health(items: &[Phys]) -> (Status, String) {
    let rank = |s: Status| match s {
        Status::Invalid => 3,
        Status::Truncated => 2,
        Status::Limited => 1,
        _ => 0,
    };
    let mut worst = Status::Present;
    let (mut cut, mut bad) = (0usize, 0usize);
    for p in items {
        let s = p.status();
        if !s.is_problem() {
            continue;
        }
        match s {
            Status::Truncated => cut += 1,
            _ => bad += 1,
        }
        if rank(s) > rank(worst) {
            worst = s;
        }
    }
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    let mut parts = Vec::new();
    if bad > 0 {
        parts.push(format!("{bad} invalid item{}", plural(bad)));
    }
    if cut > 0 {
        parts.push(format!("{cut} truncated fragment{}", plural(cut)));
    }
    (worst, parts.iter().map(|p| format!(" · {p}")).collect())
}

impl LogInspector {
    fn eof_info(&self) -> EofInfo {
        let n = log::block_count(self.len());
        let mut info = EofInfo { diags: Vec::new(), cut: None, cut_items: 0, open: None };
        if n == 0 {
            return info;
        }
        let tail = log::open_tail(self.reader(), OPEN_TAIL_BLOCKS).ok().flatten();
        // A cut item that continues the open record is described by the record's diagnostic
        // (which carries the physical detail), not by a second, physical one.
        let in_tail = |p: &Phys| tail.as_ref().and_then(|t| t.cut.as_ref()).is_some_and(|c| c.offset == p.offset);
        if let Ok((items, _)) = self.scan_block(n - 1) {
            let cut: Vec<&Phys> = items.iter().filter(|p| p.kind == PhysKind::Truncated).collect();
            info.cut = cut.first().map(|p| p.offset);
            info.cut_items = cut.len();
            info.diags = cut.iter().filter(|p| !in_tail(p)).filter_map(|p| p.diagnostic()).collect();
        }
        if let Some(t) = tail {
            info.diags.push(t.diagnostic());
            info.open = Some(t);
        }
        info
    }

    /// Diagnostics of the end of the file (root view).
    fn eof_truncation(&self) -> Vec<Diagnostic> {
        self.eof_info().diags
    }

    pub fn root_children(&self) -> Vec<Child> {
        let n = log::block_count(self.len());
        if n == 0 {
            let mut v = vec![
                Child::new("Physical blocks (0)", NodeKind::LogBlocks { first: 0, count: 0 }).leaf().status(Status::NotPresent),
                Child::new("Logical records: none, file is empty", NodeKind::LogRecords { start: 0, page: 0 }).role(ChildRole::View).leaf().status(Status::NotPresent),
            ];
            if self.mode() == LogMode::Manifest {
                v.push(Child::new("Observed levels (derived view)", NodeKind::ManifestLevels).role(ChildRole::View).leaf());
            }
            return v;
        }
        let e = self.eof_info();
        // One glyph per node: a group shows the worst status inside it plus a text suffix.
        let phys_suffix = if e.cut_items > 0 { format!(" · {} truncated fragment{}", e.cut_items, if e.cut_items == 1 { "" } else { "s" }) } else { String::new() };
        let rec_suffix = if e.open.is_some() {
            " · 1 incomplete record"
        } else if e.cut.is_some() {
            " · 1 truncated record"
        } else {
            ""
        };
        let rec_mark = (e.open.is_some() || e.cut.is_some()).then_some(Status::Truncated);
        let mut v = vec![
            Child::new(format!("Physical blocks ({n} × 32 KiB){phys_suffix}"), NodeKind::LogBlocks { first: 0, count: n }).contains(e.cut.map(|_| Status::Truncated)),
            Child::new(format!("Logical records (reassembled view){rec_suffix}"), NodeKind::LogRecords { start: 0, page: 0 }).role(ChildRole::View).contains(rec_mark),
        ];
        if self.mode() == LogMode::Manifest {
            v.push(Child::new("Observed levels (derived view)", NodeKind::ManifestLevels).role(ChildRole::View).leaf());
        }
        v
    }

    pub fn children(&mut self, k: &NodeKind) -> Vec<Child> {
        match k {
            NodeKind::Root => self.root_children(),
            NodeKind::LogBlocks { first, count } => {
                // Only the block holding an item cut by the end of file (and its groups) carries
                // the marker; intact fragments of an incomplete record do not.
                let last = log::block_count(self.len()).saturating_sub(1);
                let cut_block = if first + count > last { self.eof_info().cut.map(|o| o / BLOCK_SIZE) } else { None };
                let mark = |s: u64, c: u64| cut_block.filter(|&b| b >= s && b < s + c).map(|_| Status::Truncated);
                if *count > BLOCKS_PER_GROUP {
                    let mut g = Vec::new();
                    let mut s = *first;
                    let mut step = BLOCKS_PER_GROUP;
                    while step * BLOCKS_PER_GROUP < *count {
                        step *= BLOCKS_PER_GROUP;
                    }
                    let eof = self.len();
                    while s < first + count {
                        let c = step.min(first + count - s);
                        let end = ((s + c) * BLOCK_SIZE).min(eof);
                        g.push(Child::new(format!("Blocks {}..{} {}", s, s + c - 1, range(s * BLOCK_SIZE, end)), NodeKind::LogBlocks { first: s, count: c }).contains(mark(s, c)));
                        s += c;
                    }
                    g
                } else {
                    let eof = self.len();
                    // ≤ BLOCKS_PER_GROUP block scans (the same reads as the block table).
                    (*first..first + count)
                        .map(|b| {
                            let (st, suffix) = match self.scan_block(b) {
                                Ok((items, _)) => block_health(&items),
                                Err(_) => (Status::Invalid, " · read error".to_string()),
                            };
                            Child::new(format!("Block {b} {}{suffix}", range(b * BLOCK_SIZE, ((b + 1) * BLOCK_SIZE).min(eof))), NodeKind::LogBlock(b)).status(st).contains(mark(b, 1))
                        })
                        .collect()
                }
            }
            NodeKind::LogBlock(b) => match self.scan_block(*b) {
                Ok((items, _)) => items.iter().map(|p| Child::new(phys_label(p), NodeKind::LogFragment(p.offset)).leaf().status(stale(p.status(), p.log_number, self.expected_log())).diag(p.diagnostic())).collect(),
                Err(e) => vec![Child::new(format!("read error: {e}"), NodeKind::LogBlock(*b)).leaf().status(Status::Invalid)],
            },
            NodeKind::LogRecords { start, page } => {
                if self.is_empty() {
                    return Vec::new();
                }
                let mut out = Vec::new();
                let mut off = *start;
                for _ in 0..RECORDS_PER_PAGE {
                    let Some(l) = self.read_logical(off, 16) else { break };
                    if l.end <= off {
                        break; // monotonic guard: record ends must advance
                    }
                    let mode = self.mode();
                    let kind = record_kind_word(mode, &l);
                    let leaf = mode == LogMode::Manifest || l.role == FragRole::Meta;
                    let word = if l.reassembly().is_incomplete() && l.frag_count > 1 || l.no_last { format!(" · {}", l.status_word()) } else { String::new() };
                    let mut c = Child::new(format!("0x{:x} {kind} ({} B, {} frag){word}", l.start, l.total_len, l.frag_count), NodeKind::LogRecord(l.start)).status(l.status).diag(l.diag.clone());
                    c.leaf = leaf;
                    out.push(c);
                    off = l.end;
                }
                if self.read_logical(off, 0).is_some() {
                    out.push(Child::new(format!("Next page from 0x{off:x} ›"), NodeKind::LogRecords { start: off, page: page + 1 }).role(ChildRole::View));
                }
                out
            }
            NodeKind::LogRecord(off) => {
                if self.mode() != LogMode::Wal {
                    return Vec::new();
                }
                let off = *off;
                let comp = self.wal_compression().unwrap_or(0);
                let Some(l) = self.record(off) else { return Vec::new() };
                if !matches!(l.role, FragRole::Full | FragRole::First) || (comp != 0 && l.decompressed_from.is_none()) {
                    return Vec::new();
                }
                let b = batch::decode(&l.data, l.total_len, Space::Record(off), MAX_OPS_LISTED);
                b.ops.iter().map(|o| Child::new(format!("op {} {} {}", o.idx, tag_name(o.tag), o.summary_key()), NodeKind::WalOp { rec: off, idx: o.idx }).leaf().status(o.status)).collect()
            }
            _ => Vec::new(),
        }
    }

    /// Ancestors of `target` below the root, in tree order (canonical path of a navigation).
    /// Bounded: block paths are arithmetic; record pages are scanned at most
    /// `PATH_RECORD_PAGES` pages. None when the target is not in this tree (or beyond the bound).
    pub fn path_to(&self, target: &NodeKind, cancel: &dyn Fn() -> bool) -> Option<Vec<NodeKind>> {
        let n = log::block_count(self.len());
        let top = NodeKind::LogBlocks { first: 0, count: n };
        let blocks = |b: u64| -> Option<Vec<NodeKind>> {
            if b >= n {
                return None;
            }
            let mut v = vec![top.clone()];
            v.extend(block_group_path(n, b));
            Some(v)
        };
        match target {
            NodeKind::Root => Some(Vec::new()),
            NodeKind::LogBlocks { first: 0, count } if *count == n => Some(Vec::new()),
            NodeKind::LogBlocks { first, .. } => {
                let mut v = blocks(*first)?;
                let i = v.iter().position(|k| k == target)?;
                v.truncate(i);
                Some(v)
            }
            NodeKind::LogBlock(b) => blocks(*b),
            NodeKind::LogFragment(o) => {
                let mut v = blocks(o / BLOCK_SIZE)?;
                v.push(NodeKind::LogBlock(o / BLOCK_SIZE));
                Some(v)
            }
            NodeKind::LogRecords { start: 0, page: 0 } | NodeKind::ManifestLevels => Some(Vec::new()),
            NodeKind::LogRecords { start, .. } => {
                let pages = self.record_pages(|s, _| s == *start, cancel)?;
                Some(pages[..pages.len() - 1].to_vec())
            }
            NodeKind::LogRecord(o) => self.record_pages(|s, e| *o >= s && *o < e, cancel),
            NodeKind::WalOp { rec, .. } => {
                let mut v = self.record_pages(|s, e| *rec >= s && *rec < e, cancel)?;
                v.push(NodeKind::LogRecord(*rec));
                Some(v)
            }
            _ => None,
        }
    }

    /// Record page nodes from page 0 up to the first page whose [start, end) satisfies `hit`.
    fn record_pages(&self, hit: impl Fn(u64, u64) -> bool, cancel: &dyn Fn() -> bool) -> Option<Vec<NodeKind>> {
        let mut out = Vec::new();
        let mut start = 0u64;
        for page in 0..PATH_RECORD_PAGES as u32 {
            if cancel() {
                return None;
            }
            out.push(NodeKind::LogRecords { start, page });
            let mut off = start;
            for _ in 0..RECORDS_PER_PAGE {
                let Some(l) = self.read_logical(off, 0) else { break };
                if l.end <= off {
                    break;
                }
                off = l.end;
            }
            if hit(start, off) {
                return Some(out);
            }
            if off == start {
                return None;
            }
            start = off;
        }
        None
    }

    /// The structure holding file byte `off` and what to select in its view: the record field
    /// for payload bytes of a record (by file offset), the fragment for header/padding bytes.
    pub fn locate(&self, off: u64) -> Option<(NodeKind, Option<(Space, u64)>, Option<Span>)> {
        let b = off / BLOCK_SIZE;
        let (items, _) = self.scan_block(b).ok()?;
        let p = items.iter().find(|p| off >= p.offset && off < p.end.max(p.offset + 1))?;
        let in_payload = p.kind == PhysKind::Record && off >= p.payload_off && off < p.payload_off + p.payload_avail;
        if in_payload || (p.kind == PhysKind::Truncated && p.header_len > 0 && off >= p.payload_off) {
            if let Ok(Some(start)) = log::record_start_for(self.reader(), p.offset, OPEN_TAIL_BLOCKS) {
                return Some((NodeKind::LogRecord(start), Some((Space::File, off)), None));
            }
        }
        let span = if matches!(p.kind, PhysKind::Record | PhysKind::Truncated) && p.header_len > 0 && off < p.payload_off { Span::file(p.offset, p.payload_off - p.offset) } else { Span::file(p.offset, p.end - p.offset) };
        Some((NodeKind::LogFragment(p.offset), Some((Space::File, off)), Some(span)))
    }

    /// `exists` probes referenced sibling files (supplied by the file facade).
    pub fn view(&mut self, k: &NodeKind, cancel: &dyn Fn() -> bool, exists: &dyn Fn(&std::path::Path) -> bool) -> View {
        match k {
            NodeKind::Root => self.root_view(cancel),
            NodeKind::LogBlocks { first, count } => self.blocks_view(*first, *count, cancel),
            NodeKind::LogBlock(b) => self.block_view(*b),
            NodeKind::LogFragment(o) => self.fragment_view(*o),
            NodeKind::LogRecords { start, .. } => self.records_view(*start, cancel),
            NodeKind::LogRecord(o) => self.record_view(*o),
            NodeKind::WalOp { rec, idx } => self.op_view(*rec, *idx),
            NodeKind::ManifestLevels => super::manifest::levels_view(&manifest::observed_levels(self.reader(), cancel), self.path().parent(), exists),
            _ => error_view("Unsupported node", Status::Unsupported, "node not handled by the log inspector"),
        }
    }

    fn root_view(&self, cancel: &dyn Fn() -> bool) -> View {
        let len = self.len();
        let mut rows = vec![super::common::field_row(Field::new("file", None, self.path().display().to_string()))];
        rows.push(super::common::field_row(Field::new("format", None, format!("{} (log format, RocksDB 10.4.2 db/log_format.h)", self.what()))));
        rows.push(size_row(len));
        let mut f = vec![
            Field::new("block size", None, "32768").dv(DV::U64(BLOCK_SIZE)).explain("log::kBlockSize; records never span a block boundary without fragmentation."),
            Field::new("blocks", None, log::block_count(len).to_string()).kind(K::Count).dv(DV::U64(log::block_count(len))),
        ];
        match self.expected_log() {
            Some(n) => f.push(Field::new("log number (from file name)", None, n.to_string()).dv(DV::U64(n)).explain("Recyclable headers must carry this number, else the record is from a previous (recycled) use of the file.")),
            None => f.push(Field::new("log number (from file name)", None, "not derivable").status(Status::Unknown)),
        }
        if self.mode() == LogMode::Wal {
            let first_ok = matches!(self.first_record(), Some(r) if r.status == Status::Present);
            match self.wal_compression() {
                None if self.is_empty() => f.push(Field::new("WAL compression", None, "none: the file is empty").kind(K::CompressionType).status(Status::NotPresent).explain("An empty WAL has no SET_COMPRESSION_TYPE record (and no other record).")),
                None if !first_ok => f.push(
                    Field::new("WAL compression", None, "unknown: first record missing or unreadable")
                        .status(Status::Unknown)
                        .explain("A SET_COMPRESSION_TYPE record would be the first record; it cannot be read, so presence is unknown."),
                ),
                Some(0) | None => f.push(Field::new("WAL compression", None, "none").kind(K::CompressionType).status(if self.wal_compression().is_some() { Status::Present } else { Status::NotPresent }).explain("No leading SET_COMPRESSION_TYPE record (or it says kNoCompression): payloads are plain WriteBatches.")),
                Some(c) => f.push(
                    Field::new("WAL compression", Some(Span::file(self.first_record().map(|r| r.payload_off).unwrap_or(7), 4)), compression_name(c as u8))
                        .kind(K::CompressionType)
                        .dv(DV::U32(c))
                        .status(if c == 7 { Status::Present } else { Status::Unsupported })
                        .explain("Leading SET_COMPRESSION_TYPE record: each later logical record is one compressed stream (ZSTD supported, decoded on demand)."),
                ),
            }
        }
        if self.changed_since_open() {
            f.push(Field::new("file changed", None, "size or mtime differs from open time; reopen to refresh").status(Status::Unknown));
        }
        rows.extend(f.into_iter().map(super::common::field_row));
        let mut v = View { title: format!("{} file", self.what()), columns: super::common::fields_columns(), rows, ..Default::default() };
        if self.is_empty() {
            v.notes.push("Empty file (0 bytes): no records. This is not an error.".into());
            return v;
        }
        // First-use summary: the first records (bounded scan), each linking to its record view.
        let (recs, scanned, more) = self.record_summaries(cancel);
        let what = if self.mode() == LogMode::Wal { "WriteBatch" } else { "VersionEdit" };
        let total = if more { format!("≥{}", recs.len() + 1) } else { recs.len().to_string() };
        let coverage = if recs.is_empty() { format!("No records found · scanned {} of {len} B", range(0, scanned)) } else { format!("Records 1–{} of {total} · scanned {} of {len} B", recs.len(), range(0, scanned)) };
        v.notes.push(coverage.into());
        v.notes.push(format!("Rows below the file facts: one {what} (logical record) per row, linking to its reassembled view. Physical blocks lists the raw fragments; Logical records lists every record.").into());
        v.rows.extend(recs);
        if more {
            let fld = Field::new("more records", None, format!("records after 0x{scanned:x} are listed in Logical records ›")).kind(K::StructureRef).status(Status::Implicit).link(NodeKind::LogRecords { start: 0, page: 0 }).explain("The root lists a bounded first page; the Logical records view pages through all records.");
            v.rows.push(super::common::field_row(fld));
        }
        if self.mode() == LogMode::Manifest {
            let fld = Field::new("Observed levels ›", None, "live SST files per column family and level after replaying every VersionEdit").kind(K::StructureRef).status(Status::Implicit).link(NodeKind::ManifestLevels).explain("Derived view: the final observed LSM shape, not compaction history.");
            v.rows.push(super::common::field_row(fld));
        }
        // Not printed by the dump; the UI summarises it ("1 incomplete record") and offers the Diagnosis.
        v.diagnostics = self.eof_truncation();
        v
    }

    /// Root rows of the first `ROOT_RECORDS` records: (rows, file offset scanned to, more?).
    fn record_summaries(&self, cancel: &dyn Fn() -> bool) -> (Vec<Row>, u64, bool) {
        let wal = self.mode() == LogMode::Wal;
        let compressed = wal && self.wal_compression().unwrap_or(0) != 0;
        let keep = if compressed { LIST_KEEP_COMPRESSED } else if wal { LIST_KEEP_WAL } else { LIST_KEEP_MANIFEST };
        let mut rows = Vec::new();
        let mut off = 0u64;
        let mut n = 0usize;
        while n < ROOT_RECORDS {
            if cancel() {
                break;
            }
            // No further record: the scan examined the whole file.
            let Some(mut l) = self.read_logical(off, keep) else { return (rows, self.len(), false) };
            if l.end <= off {
                break;
            }
            if compressed {
                self.maybe_decompress(&mut l, LIST_KEEP_WAL);
            }
            self.mark_stale(&mut l);
            off = l.end;
            let (name, summary) = if wal {
                match l.role {
                    FragRole::Full | FragRole::First if l.decompressed_from.is_none() && compressed => (format!("WriteBatch #{n}"), "compressed stream (not decoded in this list)".to_string()),
                    FragRole::Full | FragRole::First if l.total_len < batch::HEADER as u64 => (format!("WriteBatch #{n}"), format!("{} B reassembled: shorter than the 12-byte WriteBatch header", l.total_len)),
                    FragRole::Full | FragRole::First => {
                        let b = batch::decode(&l.data, l.total_len, Space::Record(l.start), 3);
                        let ops: Vec<String> = b.ops.iter().take(3).map(|o| if o.value.is_some() { format!("{} {}={}", tag_name(o.tag), o.summary_key(), o.summary_value()) } else { format!("{} {}", tag_name(o.tag), o.summary_key()) }).collect();
                        let more_ops = b.count.map(|c| c as usize > ops.len()).unwrap_or(false);
                        (
                            format!("WriteBatch #{n}"),
                            format!("seq {} · {} ops · {}{}", b.seq.map(|s| s.to_string()).unwrap_or("?".into()), b.count.map(|c| c.to_string()).unwrap_or("?".into()), ops.join(", "), if more_ops { ", …" } else { "" }),
                        )
                    }
                    _ => (format!("record #{n} ({})", log::type_name(l.rtype)), meta_summary(&l)),
                }
            } else {
                match l.role {
                    FragRole::Full | FragRole::First if l.data_complete() => {
                        let e = manifest::decode_edit_summary(&l.data, Space::Record(l.start));
                        let mut parts = vec![format!("CF {}", e.cf)];
                        if let Some(x) = e.log_number {
                            parts.push(format!("log {x}"));
                        }
                        if let Some(x) = e.next_file {
                            parts.push(format!("next file {x}"));
                        }
                        if let Some(x) = e.last_seq {
                            parts.push(format!("last seq {x}"));
                        }
                        parts.push(format!("+{} files −{} files", e.adds.len(), e.deletes.len()));
                        if let Some(c) = &e.cf_add {
                            parts.push(format!("add CF {c:?}"));
                        }
                        if e.cf_drop {
                            parts.push("drop CF".into());
                        }
                        (format!("VersionEdit #{n}"), parts.join(" · "))
                    }
                    _ => (format!("record #{n} ({})", log::type_name(l.rtype)), meta_summary(&l)),
                }
            };
            let fld = Field::new(name, Some(Span::file(l.start, l.end - l.start)), summary).kind(K::StructureRef).status(l.status).link(NodeKind::LogRecord(l.start)).explain(if l.note.is_empty() { format!("Logical record {} reassembled from {} fragment(s); double-click to open it.", range(l.start, l.end), l.frag_count) } else { l.note.clone() }).diag(l.diag.clone());
            let cells = vec![fld.name.clone(), format!("0x{:x}", l.start), (l.end - l.start).to_string(), fld.value.clone(), header_variant(&l), String::new(), record_status_cell(&l)];
            rows.push(Row { cells, field: fld });
            n += 1;
        }
        let more = self.read_logical(off, 0).is_some();
        (rows, off, more)
    }

    fn blocks_view(&self, first: u64, count: u64, cancel: &dyn Fn() -> bool) -> View {
        let mut rows = Vec::new();
        if count == 0 {
            let mut v = of_fields("Physical blocks (0)", Vec::new());
            v.notes.push("Empty file (0 bytes): no blocks. This is not an error.".into());
            return v;
        }
        let n = count.min(BLOCKS_PER_GROUP);
        if count > BLOCKS_PER_GROUP {
            let mut v = of_fields("Block groups", vec![Field::new("blocks", None, format!("{count} blocks; expand a group"))]);
            v.notes.push("Large file: blocks are grouped; only expanded groups are scanned.".into());
            let (a, z) = (first * BLOCK_SIZE, ((first + count) * BLOCK_SIZE).min(self.len()));
            v.location = vec![WhereLine { text: format!("file {}", range(a, z)), link: None }];
            v.extent = Some(Span::file(a, z.saturating_sub(a)));
            return v;
        }
        for b in first..first + n {
            if cancel() {
                break;
            }
            match self.scan_block(b) {
                Ok((items, _)) => {
                    let recs = items.iter().filter(|p| p.kind == PhysKind::Record).count();
                    let types: Vec<&str> = items.iter().filter(|p| p.kind == PhysKind::Record).map(|p| log::type_name(p.rtype)).take(4).collect();
                    let (st, _) = block_health(&items);
                    let blen = (self.len() - b * BLOCK_SIZE).min(BLOCK_SIZE);
                    let first_problem = items.iter().find(|p| p.status().is_problem()).and_then(|p| p.diagnostic());
                    let fld = Field::new(format!("block {b}"), Some(Span::file(b * BLOCK_SIZE, blen)), format!("{recs} records")).kind(K::StructureRef).dv(DV::U64(recs as u64)).status(st).link(NodeKind::LogBlock(b)).diag(first_problem);
                    rows.push(Row { cells: vec![b.to_string(), format!("0x{:x}", b * BLOCK_SIZE), blen.to_string(), recs.to_string(), types.join(", "), st_cell(st)], field: fld });
                }
                Err(e) => rows.push(Row { cells: vec![b.to_string(), String::new(), String::new(), String::new(), format!("{e}"), "✖ Invalid".into()], field: Field::new("block", None, e.to_string()).status(Status::Invalid) }),
            }
        }
        use ColumnRole as R;
        View {
            title: format!("Blocks {}..{}", first, first + n - 1),
            columns: vec![col(R::Index, "#"), col(R::Offset, "File offset"), col(R::Len, "Bytes"), col(R::Count, "Records"), col(R::Summary, "Types"), col(R::Status, "Status")],
            rows,
            location: vec![WhereLine { text: format!("file {}", range(first * BLOCK_SIZE, ((first + n) * BLOCK_SIZE).min(self.len()))), link: None }],
            extent: Some(Span::file(first * BLOCK_SIZE, ((first + n) * BLOCK_SIZE).min(self.len()) - first * BLOCK_SIZE)),
            ..Default::default()
        }
    }

    fn block_view(&self, b: u64) -> View {
        let (items, _) = match self.scan_block(b) {
            Ok(x) => x,
            Err(e) => return error_view("Block", Status::Invalid, format!("read error: {e}")),
        };
        let rows = items
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let st = stale(p.status(), p.log_number, self.expected_log());
                // The Type column names the record type (a cut fragment keeps its type, e.g.
                // "3 MIDDLE"; the Status column alone says Truncated).
                let (ty, hdr) = match p.kind {
                    PhysKind::Record => (format!("{} {}", p.rtype, log::type_name(p.rtype)), if p.header_len == 11 { "recyclable 11 B" } else { "legacy 7 B" }.to_string()),
                    PhysKind::Truncated if p.header_len > 0 => (format!("{} {}", p.rtype, log::type_name(p.rtype)), if p.header_len == 11 { "recyclable 11 B" } else { "legacy 7 B" }.to_string()),
                    PhysKind::Truncated => ("header (cut)".to_string(), "-".into()),
                    k => (format!("{k:?}"), "-".into()),
                };
                let crc = match p.computed_crc {
                    Some(c) => format!("{:08x} / {:08x}", crate::util::crc_unmask(p.stored_crc), c),
                    None => "-".into(),
                };
                let fld = Field::new(format!("physical item {i}"), Some(Span::file(p.offset, p.end - p.offset)), ty.clone()).kind(K::StructureRef).status(st).link(NodeKind::LogFragment(p.offset)).explain(phys_explain(p)).diag(p.diagnostic());
                Row { cells: vec![i.to_string(), format!("0x{:x}", p.offset), ty, hdr, p.len.to_string(), p.log_number.map(|n| n.to_string()).unwrap_or("-".into()), crc, st_cell(st)], field: fld }
            })
            .collect();
        let start = b * BLOCK_SIZE;
        let end = (start + BLOCK_SIZE).min(self.len());
        let next = (end < self.len()).then(|| (end, NodeKind::LogBlock(b + 1)));
        let (health, suffix) = block_health(&items);
        use ColumnRole as R;
        let mut v = View {
            title: format!("Block {b} @0x{:x}{suffix}", b * BLOCK_SIZE),
            status: health.is_problem().then_some(health),
            columns: vec![col(R::Index, "#"), col(R::Offset, "File offset"), col(R::Kind, "Type"), col(R::Header, "Header"), col(R::Len, "Len"), col(R::Seq, "Log #"), col(R::Crc, "CRC stored / computed"), col(R::Status, "Status")],
            rows,
            notes: vec!["CRC values are unmasked (crc32c::Unmask) for comparison.".into()],
            ..Default::default()
        };
        Location { start, end, contents: None, trailer: None, header: None, decompressed: None, next, gap: None, eof: self.len() }.apply(&mut v);
        v
    }

    fn fragment_view(&self, off: u64) -> View {
        let b = off / BLOCK_SIZE;
        let Ok((items, buf)) = self.scan_block(b) else {
            return error_view("Fragment", Status::Invalid, "read error");
        };
        let Some(i) = items.iter().position(|p| p.offset == off) else {
            return error_view("Fragment", Status::Unknown, "no physical item at this offset");
        };
        let p = &items[i];
        let base = b * BLOCK_SIZE;
        let s = (off - base) as usize;
        let mut f = Vec::new();
        match p.kind {
            PhysKind::BlockTrailer => {
                f.push(Field::new("block trailer padding", Some(Span::file(off, p.end - off)), format!("{} bytes", p.end - off)).raw(&buf[s..]).enc("zero bytes").explain("Fewer than 7 bytes remained in the 32 KiB block, so the writer padded with zeros (log_writer.cc)."));
            }
            PhysKind::ZeroFill => {
                f.push(Field::new("zero region", Some(Span::file(off, p.end - off)), format!("{} bytes", p.end - off)).enc("zero bytes").explain("Type 0 with length 0: preallocated or zeroed space. log_reader.cc reports kBadRecord / treats as end of data."));
            }
            _ => {
                let h = &buf[s..];
                f.push(
                    Field::new("checksum (masked crc32c)", Some(Span::file(off, 4)), format!("0x{:08x}", p.stored_crc))
                        .kind(K::Checksum(crate::model::ChecksumRole::Stored))
                        .dv(DV::U32(p.stored_crc))
                        .diag(p.diagnostic())
                        .raw(&h[..4.min(h.len())])
                        .enc("fixed32 LE")
                        .explain(
                            format!(
                                "Stored masked CRC32C. Unmasked: 0x{:08x}. Computed over bytes [offset+6, offset+{}) = type{} + payload: {}.",
                                crate::util::crc_unmask(p.stored_crc),
                                p.header_len + p.len as usize,
                                if p.header_len == 11 { " + log number" } else { "" },
                                match p.computed_crc {
                                    Some(c) => format!("0x{c:08x} -> {}", if p.crc_ok() == Some(true) { "match" } else { "MISMATCH" }),
                                    None => "not computable (payload incomplete)".into(),
                                }
                            ) + if p.crc_ok() == Some(false) { CRC_GUESS } else { "" },
                        )
                        .status(match p.crc_ok() {
                            Some(true) => Status::Present,
                            Some(false) => Status::Invalid,
                            None => Status::Truncated,
                        }),
                );
                if h.len() >= 6 {
                    f.push(Field::new("length", Some(Span::file(off + 4, 2)), p.len.to_string()).kind(K::Length).dv(DV::U16(p.len as u16)).raw(&h[4..6]).enc("fixed16 LE").explain("Payload length of this fragment (excludes header)."));
                }
                if h.len() >= 7 {
                    f.push(Field::new("type", Some(Span::file(off + 6, 1)), format!("{} {}", p.rtype, log::type_name(p.rtype))).kind(K::RecordType).dv(DV::Enum { name: log::type_name(p.rtype), code: u64::from(p.rtype) }).raw(&h[6..7]).enc("u8").explain("Record type (db/log_format.h). 1-4 legacy fragments, 5-8 recyclable, 9 compression, 10/11 timestamp sizes, 130/131 predecessor WAL info."));
                }
                if p.header_len == 11 {
                    let ln = p.log_number.unwrap_or(0);
                    let st = match self.expected_log() {
                        Some(e) if e as u32 == ln => Status::Present,
                        Some(_) => Status::Unknown,
                        None => Status::Present,
                    };
                    f.push(Field::new("log number", Some(Span::file(off + 7, 4)), ln.to_string()).dv(DV::U32(ln)).raw(h.get(7..11).unwrap_or(&[])).enc("fixed32 LE").status(st).explain("Recyclable header: low 32 bits of the WAL number. A mismatch means a stale record from a recycled file (kOldRecord)."));
                } else {
                    f.push(Field::new("log number", None, "absent in legacy 7-byte header").status(Status::NotPresent));
                }
                let pl = p.payload_avail;
                let mut pf = Field::new("payload", Some(Span::file(p.payload_off, pl)), bytes_preview(&buf[(p.payload_off - base) as usize..(p.payload_off - base + pl) as usize], 64)).kind(K::Payload).enc("bytes").status(if pl < p.len as u64 { Status::Truncated } else { Status::Present });
                if matches!(log::role(p.rtype), FragRole::Full | FragRole::First | FragRole::Meta) {
                    pf = pf.link(NodeKind::LogRecord(off)).explain("Starts a logical record; follow the link to the reassembled view.");
                } else {
                    pf = pf.explain("Continuation fragment: its payload is appended to a record started by an earlier FIRST fragment.");
                }
                f.push(pf);
                // Link back to the record this fragment belongs to (bounded backward scan).
                if p.kind == PhysKind::Record || p.header_len > 0 {
                    if let Ok(Some(start)) = log::record_start_for(self.reader(), off, OPEN_TAIL_BLOCKS) {
                        if let Some(l) = self.read_logical(start, 0) {
                            if let Some(pos) = l.frags.iter().position(|x| x.offset == off) {
                                f.push(Field::new("record", None, format!("part {} of {} of record @0x{start:x} ›", pos + 1, l.frag_count)).kind(K::StructureRef).status(Status::Implicit).link(NodeKind::LogRecord(start)).explain(format!("This fragment's payload is reassembled offsets [0x{:x}, 0x{:x}) of the logical record {}.", l.frags[pos].logical_off, l.frags[pos].logical_off + l.frags[pos].len, range(l.start, l.end))));
                            }
                        }
                    }
                }
            }
        }
        let mut v = of_fields(format!("Physical item @0x{off:x}"), f);
        v.status = Some(p.status());
        v.diagnostics.extend(p.diagnostic());
        let next = items.get(i + 1).map(|q| (q.offset, NodeKind::LogFragment(q.offset))).or_else(|| (p.end < self.len()).then(|| (p.end, NodeKind::LogBlock(b + 1))));
        let hdr = (p.header_len > 0 && matches!(p.kind, PhysKind::Record | PhysKind::Truncated)).then_some((p.offset, p.payload_off));
        let contents = hdr.map(|_| (p.payload_off, p.payload_off + p.payload_avail));
        Location { start: p.offset, end: p.end, contents, trailer: None, header: hdr, decompressed: None, next, gap: None, eof: self.len() }.apply(&mut v);
        v
    }

    fn records_view(&mut self, start: u64, cancel: &dyn Fn() -> bool) -> View {
        let mut rows = Vec::new();
        let mut off = start;
        let wal = self.mode() == LogMode::Wal;
        for i in 0..RECORDS_PER_PAGE {
            if cancel() || self.is_empty() {
                break;
            }
            let compressed = wal && self.wal_compression().unwrap_or(0) != 0;
            let keep = if compressed { LIST_KEEP_COMPRESSED } else if wal { LIST_KEEP_WAL } else { LIST_KEEP_MANIFEST };
            let Some(mut l) = self.read_logical(off, keep) else { break };
            if l.end <= off {
                break; // monotonic guard: record ends must advance
            }
            if compressed {
                self.maybe_decompress(&mut l, LIST_KEEP_WAL);
            }
            self.mark_stale(&mut l);
            off = l.end;
            let hdr = header_variant(&l);
            let crc = if l.frags.iter().all(|f| f.crc_ok == Some(true)) {
                "all match"
            } else if l.frags.iter().any(|f| f.crc_ok == Some(false)) {
                "mismatch"
            } else {
                "not computable (cut)"
            };
            let re = l.reassembly();
            let mut fld = Field::new(format!("record {i}"), Some(Span::file(l.start, l.end - l.start)), format!("{} B logical", l.total_len)).kind(K::StructureRef).dv(DV::U64(l.total_len)).status(l.status).link(NodeKind::LogRecord(l.start)).explain(l.note.clone()).diag(l.diag.clone());
            let cells = if wal {
                let (kind, seq, cnt, cf, key, val) = if matches!(l.role, FragRole::Full | FragRole::First) {
                    if l.decompressed_from.is_none() && self.wal_compression().unwrap_or(0) != 0 {
                        ("WriteBatch (compressed)".to_string(), String::new(), String::new(), String::new(), String::new(), String::new())
                    } else {
                        let b = batch::decode(&l.data, l.total_len, Space::Record(l.start), 1);
                        let op = b.ops.first();
                        (
                            format!("WriteBatch{}", op.map(|o| format!(" · {}", tag_name(o.tag))).unwrap_or_default()),
                            b.seq.map(|s| s.to_string()).unwrap_or_default(),
                            b.count.map(|s| s.to_string()).unwrap_or_default(),
                            op.and_then(|o| o.cf).map(|c| c.to_string()).unwrap_or_default(),
                            op.map(|o| o.summary_key()).unwrap_or_default(),
                            op.map(|o| o.summary_value()).unwrap_or_default(),
                        )
                    }
                } else {
                    (log::type_name(l.rtype).to_string(), String::new(), String::new(), String::new(), String::new(), meta_summary(&l))
                };
                fld.value = format!("{kind} seq {seq} count {cnt}");
                vec![i.to_string(), format!("0x{:x}", l.start), kind, hdr, seq, cnt, cf, l.total_len.to_string(), crc.into(), re.text().into(), key, val, record_status_cell(&l)]
            } else {
                let summary = if l.data_complete() && matches!(l.role, FragRole::Full | FragRole::First) {
                    let e = manifest::decode_edit_summary(&l.data, Space::Record(l.start));
                    let mut s = e.tags.join(", ");
                    if !e.adds.is_empty() || !e.deletes.is_empty() {
                        s = format!("+{} files −{} files · {s}", e.adds.len(), e.deletes.len());
                    }
                    s
                } else {
                    meta_summary(&l)
                };
                fld.value = summary.clone();
                vec![i.to_string(), format!("0x{:x}", l.start), hdr, l.total_len.to_string(), l.frag_count.to_string(), crc.into(), re.text().into(), summary, record_status_cell(&l)]
            };
            rows.push(Row { cells, field: fld });
        }
        use ColumnRole as R;
        let columns = if wal {
            vec![
                col(R::Index, "#"),
                col(R::Offset, "Offset"),
                col(R::Kind, "Kind"),
                col(R::Header, "Header"),
                col(R::Seq, "Seq"),
                col(R::Count, "Count"),
                col(R::Cf, "CF"),
                col(R::Len, "Len"),
                col(R::Crc, "CRC"),
                col(R::Header, "Reassembly"),
                col(R::Key, "Key preview"),
                col(R::Value, "Value preview"),
                col(R::Status, "Status"),
            ]
        } else {
            vec![col(R::Index, "#"), col(R::Offset, "Offset"), col(R::Header, "Header"), col(R::Len, "Len"), col(R::Count, "Frags"), col(R::Crc, "CRC"), col(R::Header, "Reassembly"), col(R::Summary, "VersionEdit summary"), col(R::Status, "Status")]
        };
        let mut v = View { title: format!("Logical records from 0x{start:x}"), columns, rows, notes: vec![format!("Page of up to {RECORDS_PER_PAGE} records; expand \"Next page\" in the tree to continue.").into()], ..Default::default() };
        if self.is_empty() {
            v.notes = vec!["Empty file (0 bytes): no records. This is not an error.".into()];
        }
        v
    }

    fn record_view(&mut self, off: u64) -> View {
        let mode = self.mode();
        let comp = self.wal_compression().unwrap_or(0);
        let expected = self.expected_log();
        let eof = self.len();
        let Some(l) = self.record(off) else {
            return error_view("Record", Status::Unknown, "no record at this offset");
        };
        let sp = Space::Record(off);
        let mut batch_diag = None;
        let mut f = vec![
            Field::new("record type", Some(Span::file(off + 6, 1)), format!("{} {}", l.rtype, log::type_name(l.rtype))).kind(K::RecordType).dv(DV::Enum { name: log::type_name(l.rtype), code: u64::from(l.rtype) }).diag(l.diag.clone()).enc("u8").status(l.status).explain(if l.note.is_empty() { "Type of the first fragment.".to_string() } else { l.note.clone() }),
            Field::new("reassembled length", None, format!("{} bytes in {} fragment(s)", l.total_len, l.frag_count)).kind(K::Length).dv(DV::U64(l.total_len)).explain("Sum of fragment payload lengths. Logical offsets below are relative to the reassembled record (rec+), not the file."),
            Field::new("reassembly", None, l.reassembly().text()).status(if l.reassembly() == Reassembly::Complete { Status::Implicit } else { l.status }).diag(l.diag.clone()).explain("Whether every fragment from FIRST to LAST was found (reassembly is not decompression)."),
        ];
        if let (Some(ln), Some(e)) = (l.log_number, expected) {
            if u64::from(ln) != e & 0xffff_ffff {
                f.push(Field::new("stale recycled record", None, format!("log number {ln} ≠ {e}")).status(Status::Unknown).explain("log_reader.cc returns kOldRecord for this; data belongs to an older WAL that reused the file."));
            }
        }
        // Source fragments: each fragment's file range links to its canonical physical item.
        for (i, fr) in l.frags.iter().enumerate() {
            f.push(
                Field::new(format!("fragment {i} {}", log::type_name(fr.rtype)), Some(Span::file(fr.payload_off, fr.len)), format!("file 0x{:x}+{} -> rec+0x{:x}", fr.payload_off, fr.len, fr.logical_off))
                    .enc("payload bytes")
                    .status(match fr.crc_ok {
                        Some(true) => Status::Present,
                        Some(false) => Status::Invalid,
                        None => Status::Truncated,
                    })
                    .kind(K::Payload)
                    .link(NodeKind::LogFragment(fr.offset))
                    .explain(format!("Source fragment: physical header at file 0x{:x} ({} B header). This payload occupies reassembled offsets [0x{:x}, 0x{:x}).", fr.offset, fr.header_len, fr.logical_off, fr.logical_off + fr.len)),
            );
        }
        if (l.frags.len() as u64) < l.frag_count {
            f.push(Field::new("more fragments", None, format!("{} not listed", l.frag_count - l.frags.len() as u64)).status(Status::Limited));
        }
        if l.no_last {
            // Stated explicitly: the end of file closed this record before its LAST fragment.
            let after = match l.frags.last() {
                Some(fr) if fr.crc_ok.is_none() => format!("{} at 0x{:x} cut off by end of file", if fr.header_len > 0 { log::type_name(fr.rtype) } else { "fragment header" }, fr.offset),
                _ => format!("end of file at 0x{eof:x} after FIRST + {} MIDDLE", l.frag_count.saturating_sub(1)),
            };
            f.push(Field::new("LAST fragment", None, format!("missing: {after}")).kind(K::RecordType).status(Status::Truncated).diag(l.diag.clone()).explain(l.note.clone()));
        }
        let data = &l.data;
        if !l.data_complete() {
            f.push(Field::new("reassembly cap", None, format!("kept {} of {} bytes (cap {})", data.len(), l.total_len, MAX_RECORD_BYTES)).status(Status::Limited));
        }
        match l.role {
            FragRole::Meta => f.extend(meta_fields(l)),
            FragRole::Full | FragRole::First if mode == LogMode::Wal => {
                if let Some(cn) = l.decompressed_from {
                    f.push(Field::new("stream decompression", None, format!("{} -> {} bytes ({})", cn, l.total_len, compression_name(comp as u8))).status(Status::Implicit).explain("Fragment payloads are one compressed stream per record. The fragment rows give the FILE ranges of the compressed payload bytes (their \"-> rec+\" value is the position in the compressed stream). The WriteBatch fields below use rec+ offsets of the DECOMPRESSED WriteBatch, which have no byte-to-byte mapping to file offsets."));
                }
                if comp != 0 && l.decompressed_from.is_none() {
                    f.push(Field::new("WriteBatch", Some(Span::new(sp, 0, data.len() as u64)), format!("{}-compressed stream", compression_name(comp as u8))).status(l.status).explain(l.note.clone()));
                } else {
                    let b = batch::decode(data, l.total_len, sp, MAX_OPS_LISTED);
                    f.extend(batch_fields(None, &b.header));
                    batch_diag = b.diag.clone();
                    for o in &b.ops {
                        let mut val = format!("{} cf {} {}", tag_name(o.tag), o.cf.map(|c| c.to_string()).unwrap_or("-".into()), o.summary_key());
                        if o.value.is_some() {
                            val.push_str(&format!(" = {}", o.summary_value()));
                        }
                        f.push(
                            Field::new(format!("op {}", o.idx), Some(Span::new(sp, o.offset as u64, (o.end - o.offset) as u64)), val)
                                .enc("WriteBatch record")
                                .status(o.status)
                                .kind(K::Tag)
                                .dv(DV::Enum { name: tag_name(o.tag), code: u64::from(o.tag) })
                                .link(NodeKind::WalOp { rec: off, idx: o.idx })
                                .explain(format!("Operation {} (tag 0x{:02x}); derived sequence {}.", o.idx, o.tag, o.seq.map(|s| s.to_string()).unwrap_or("n/a".into()))),
                        );
                    }
                    if b.status != Status::Present || !b.note.is_empty() {
                        f.push(Field::new("batch status", None, b.note.clone()).status(b.status).diag(b.diag.clone()));
                    }
                }
            }
            FragRole::Full | FragRole::First => {
                let e = manifest::decode_edit(data, sp);
                f.extend(super::manifest::edit_fields(&e.fields));
                batch_diag = e.diag.clone();
                if let Some(s) = e.status {
                    f.push(Field::new("edit status", None, e.note).status(s).diag(e.diag));
                }
            }
            _ => {
                f.push(Field::new("payload", Some(Span::new(sp, 0, data.len() as u64)), bytes_preview(data, 64)).status(l.status).explain("Cannot be interpreted as a complete record."));
            }
        }
        let mut v = of_fields(format!("Logical record @0x{off:x}"), f);
        v.status = Some(l.status);
        if l.status_word() != l.status.label() {
            v.status_word = Some(l.status_word());
        }
        v.diagnostics.extend(l.diag.clone());
        v.diagnostics.extend(batch_diag);
        v.spaces.push(SpaceInfo {
            space: sp,
            size: data.len() as u64,
            title: if l.decompressed_from.is_some() { format!("Decompressed record offsets (derived from compressed record @0x{off:x}, {} B)", l.total_len) } else { format!("Reassembled record offsets (logical, first fragment @0x{off:x}, {} B)", l.total_len) },
            map: record_map(l),
        });
        let first = l.frags.first().map(|fr| NodeKind::LogFragment(fr.offset));
        let mut loc = vec![
            WhereLine { text: format!("file {} · stored {} in {} fragment(s)", range(l.start, l.end), bytes_dec_hex(l.end - l.start), l.frag_count), link: None },
            WhereLine { text: format!("reassembled contents {}{}", bytes_dec_hex(l.total_len), if l.decompressed_from.is_some() { " (decompressed stream; no byte mapping to file offsets)" } else { " (record offsets map to fragment payload bytes)" }), link: None },
        ];
        if let Some(k) = first {
            loc.push(WhereLine { text: format!("first fragment at 0x{:x} ›", l.start), link: Some(Target { node: k, span: None }) });
        }
        v.location = loc;
        v.extent = Some(Span::file(l.start, l.end - l.start));
        v
    }

    fn op_view(&mut self, rec: u64, idx: u32) -> View {
        let Some(l) = self.record(rec) else { return error_view("Operation", Status::Unknown, "record not found") };
        let b = batch::decode(&l.data, l.total_len, Space::Record(rec), idx as usize + 1);
        let Some(o) = b.ops.into_iter().find(|o| o.idx == idx) else {
            return error_view("Operation", Status::Unknown, "operation index not found");
        };
        let size = l.data.len() as u64;
        let map = record_map(l);
        let mut v = of_fields(format!("Operation {idx} in record @0x{rec:x}"), batch_fields(Some(o.tag), &o.facts));
        v.status = Some(o.status);
        v.diagnostics.extend(o.facts.iter().filter_map(|f| f.diag.as_deref().cloned()));
        v.spaces.push(SpaceInfo { space: Space::Record(rec), size, title: format!("Reassembled record offsets (logical, first fragment @0x{rec:x})"), map });
        v
    }
}

/// Provenance of a record space: fragment payloads (exact), or the compressed record range.
fn record_map(l: &Logical) -> SpaceMap {
    if l.decompressed_from.is_some() {
        return SpaceMap::Compressed { file: Span::file(l.start, l.end - l.start), codec: "ZSTD stream (WAL record)".into() };
    }
    SpaceMap::Fragments {
        segs: l.frags.iter().map(|f| (f.logical_off, f.payload_off, f.len)).collect(),
        headers: l.frags.iter().skip(1).map(|f| Span::file(f.offset, f.payload_off - f.offset)).collect(),
    }
}

fn record_kind_word(mode: LogMode, l: &Logical) -> &'static str {
    if mode == LogMode::Wal && matches!(l.role, FragRole::Full | FragRole::First) {
        "WriteBatch"
    } else if mode == LogMode::Manifest && matches!(l.role, FragRole::Full | FragRole::First) {
        "VersionEdit"
    } else {
        log::type_name(l.rtype)
    }
}

fn header_variant(l: &Logical) -> String {
    let rec = l.frags.first().map(|f| f.header_len == 11).unwrap_or(false);
    let shape = if l.no_last && l.frag_count <= 1 {
        "FIRST, no LAST".to_string()
    } else if l.no_last {
        format!("FIRST..MIDDLE ×{}, no LAST", l.frag_count)
    } else if l.frag_count <= 1 {
        log::type_name(l.rtype).to_string()
    } else {
        format!("FIRST..LAST ×{}", l.frag_count)
    };
    format!("{shape} ({})", if rec { "recyclable 11 B" } else { "legacy 7 B" })
}

fn meta_summary(l: &Logical) -> String {
    meta_fields(l).iter().map(|f| format!("{}={}", f.name, f.value)).collect::<Vec<_>>().join(" ")
}

/// Standalone metadata records (decoded by `wal::meta_facts`) with explanations by kind.
fn meta_fields(l: &Logical) -> Vec<Field> {
    meta_facts(l)
        .into_iter()
        .map(|f| {
            let e = match f.kind {
                K::CompressionType if f.status == Status::Present => "Following WAL fragments are stream-compressed with this codec.",
                _ => "",
            };
            explained(f, e)
        })
        .collect()
}

fn phys_explain(p: &Phys) -> String {
    let base = phys_explain_base(p);
    if p.crc_ok() == Some(false) {
        format!("{base}{CRC_GUESS}")
    } else {
        base
    }
}

fn phys_explain_base(p: &Phys) -> String {
    match p.kind {
        PhysKind::Record => format!(
            "{} header ({} B) + {} B payload. CRC32C over type{} + payload.",
            if p.header_len == 11 { "Recyclable" } else { "Legacy" },
            p.header_len,
            p.len,
            if p.header_len == 11 { " + log number" } else { "" }
        ),
        PhysKind::BlockTrailer => "Zero padding at the end of a 32 KiB block (fewer than 7 bytes left).".into(),
        PhysKind::ZeroFill => "Zeroed/preallocated region; not a record.".into(),
        PhysKind::BadLength => "Length field runs past the end of the block: corrupt (kBadRecordLen).".into(),
        PhysKind::Truncated => "File ends inside this record.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical path of a block follows the same grouping as the tree children.
    #[test]
    fn block_group_path_matches_children_grouping() {
        assert!(block_group_path(10, 3).is_empty());
        let g = BLOCKS_PER_GROUP;
        assert_eq!(block_group_path(g * 3, g + 5), vec![NodeKind::LogBlocks { first: g, count: g }]);
        let n = g * g * 2 + 7;
        let p = block_group_path(n, g * g * 2 + 3);
        assert_eq!(p, vec![NodeKind::LogBlocks { first: g * g * 2, count: 7 }]);
        let p = block_group_path(n, g * g + 2 * g + 1);
        assert_eq!(p, vec![NodeKind::LogBlocks { first: g * g, count: g * g }, NodeKind::LogBlocks { first: g * g + 2 * g, count: g }]);
    }
}
