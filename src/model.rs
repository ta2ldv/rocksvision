//! Data model shared by parsers, presenters, jobs, the headless dump and the UI.
//! Parsers produce typed structs and `Fact`s (format elements with byte spans); presenters
//! (`crate::present`) turn them into `View`s (rows of `Field`s) and lazy tree children (`Child`).
//! No pixel geometry lives here: columns carry a semantic `ColumnRole`, the UI maps roles to widths.

use std::fmt;

/// Offset space of a byte span. Physical file bytes and derived bytes are never mixed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Space {
    /// Physical bytes as stored in the file (file offsets).
    File,
    /// Decompressed contents of the SST block whose handle offset is the payload
    /// (block-local offsets; derived bytes, no file bytes of their own).
    Block(u64),
    /// Reassembled logical log record (WAL/MANIFEST) whose first fragment header
    /// starts at the given file offset (record-local offsets).
    Record(u64),
}

impl Space {
    pub fn label(&self) -> String {
        match self {
            Space::File => "file".into(),
            Space::Block(o) => format!("block@0x{o:x} (decompressed)"),
            Space::Record(o) => format!("record@0x{o:x} (reassembled)"),
        }
    }
    /// Short name of the offset column of a byte view in this space: FILE / BLOCK+ / RECORD+.
    pub fn offset_header(&self) -> &'static str {
        match self {
            Space::File => "FILE",
            Space::Block(_) => "BLOCK+",
            Space::Record(_) => "RECORD+",
        }
    }
    /// Name of the address space in range and position texts: "file", "block@0x5c9"
    /// (decompressed block contents), "record@0x0" (reassembled record).
    pub fn name(&self) -> String {
        match self {
            Space::File => "file".into(),
            Space::Block(o) => format!("block@0x{o:x}"),
            Space::Record(o) => format!("record@0x{o:x}"),
        }
    }
    /// One "Where" line of a span in this space: the shared range notation (`Span::range_text`).
    pub fn where_line(&self, offset: u64, len: u64) -> String {
        Span::new(*self, offset, len).range_text()
    }
}

/// The one byte-range notation (0019 §2): half-open `[a,b)` in lowercase hex. Derived spaces
/// mark offsets relative to their start with `+`: "[+0x6d,+0xd4)".
pub fn range_text(space: Space, a: u64, b: u64) -> String {
    match space {
        Space::File => format!("[0x{a:x},0x{b:x})"),
        _ => format!("[+0x{a:x},+0x{b:x})"),
    }
}

/// How the bytes of a derived offset space relate to file bytes (provenance). Reassembly maps
/// every record byte to exactly one file byte (fragment payloads, headers skipped);
/// decompression has no byte-to-byte mapping: a derived range is only *contained* in the
/// compressed stored range.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum SpaceMap {
    /// File space, or a derived space whose origin is not described.
    #[default]
    None,
    /// Reassembled record: (record offset, file offset, length) per fragment payload in record
    /// order, and the fragment header ranges between them (never part of the record).
    Fragments { segs: Vec<(u64, u64, u64)>, headers: Vec<Span> },
    /// Decompressed contents of the stored (compressed) file range `file`, codec name.
    Compressed { file: Span, codec: String },
}

/// Physical (file) location of a span of any offset space.
#[derive(Clone, Debug, PartialEq)]
pub enum FileRanges {
    /// Exact file ranges in order (two or more when a field crosses a fragment boundary) and the
    /// fragment headers skipped between them.
    Exact { ranges: Vec<Span>, skipped: Vec<Span> },
    /// No byte mapping exists: the span lies inside this compressed stored range.
    Contained { file: Span, codec: String },
    /// The origin of the space is not known to the view.
    Unknown,
}

impl SpaceMap {
    /// File location of `[off, off+len)` of the derived space this map describes. Pure; the
    /// one range service used by the Explanation, the status bar and the byte view.
    pub fn file_ranges(&self, off: u64, len: u64) -> FileRanges {
        match self {
            SpaceMap::None => FileRanges::Unknown,
            SpaceMap::Compressed { file, codec } => FileRanges::Contained { file: *file, codec: codec.clone() },
            SpaceMap::Fragments { segs, headers } => {
                let end = off.saturating_add(len.max(1));
                let mut ranges = Vec::new();
                for &(lo, fo, n) in segs {
                    let (a, b) = (off.max(lo), end.min(lo.saturating_add(n)));
                    if a < b {
                        ranges.push(Span::file(fo.saturating_add(a - lo), if len == 0 { 0 } else { b - a }));
                    }
                }
                if ranges.is_empty() {
                    return FileRanges::Unknown;
                }
                let (first, last) = (ranges[0].offset, ranges[ranges.len() - 1].offset);
                let skipped = headers.iter().filter(|h| h.offset > first && h.offset < last).copied().collect();
                FileRanges::Exact { ranges, skipped }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    pub space: Space,
    pub offset: u64,
    pub len: u64,
}

impl Span {
    pub fn file(offset: u64, len: u64) -> Self {
        Span { space: Space::File, offset, len }
    }
    pub fn new(space: Space, offset: u64, len: u64) -> Self {
        Span { space, offset, len }
    }
    /// `off` is a byte of this span (a zero-length span holds its start byte). Overflow-safe
    /// near `u64::MAX` (0020 §2).
    pub fn contains(&self, space: Space, off: u64) -> bool {
        self.space == space && off >= self.offset && off - self.offset < self.len.max(1)
    }
    /// End offset (exclusive).
    pub fn end(&self) -> u64 {
        self.offset.saturating_add(self.len)
    }
    /// The one range notation with its address space named (0019 §2): "file
    /// [0x6390c,0x648fa) · 4078 B", "block@0x5c9 [+0x6d,+0xd4) · 103 B", "record@0x0
    /// [+0x8,+0xc) · 4 B".
    pub fn range_text(&self) -> String {
        format!("{} {} · {} B", self.space.name(), range_text(self.space, self.offset, self.end()), self.len)
    }
    /// A single position: "file 0x6390c", "block@0x5c9 +0x6d", "record@0x0 +0x8".
    pub fn position_text(&self) -> String {
        match self.space {
            Space::File => format!("file 0x{:x}", self.offset),
            s => format!("{} +0x{:x}", s.name(), self.offset),
        }
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} 0x{:x}+{}", self.space.label(), self.offset, self.len)
    }
}

/// Presence/validity of a field or structure. An undecoded feature is never `NotPresent`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Status {
    Present,
    NotPresent,
    Implicit,
    Unsupported,
    Unknown,
    Invalid,
    Truncated,
    /// Decoding stopped at a configured resource limit (bounded allocation).
    Limited,
}

impl Status {
    pub fn label(&self) -> &'static str {
        match self {
            Status::Present => "Present",
            Status::NotPresent => "Not present",
            Status::Implicit => "Implicit",
            Status::Unsupported => "Unsupported",
            Status::Unknown => "Unknown",
            Status::Invalid => "Invalid",
            Status::Truncated => "Truncated",
            Status::Limited => "Resource limit",
        }
    }
    pub fn symbol(&self) -> &'static str {
        match self {
            Status::Present => "✔",
            Status::NotPresent => "○",
            Status::Implicit => "≈",
            Status::Unsupported => "⚠",
            Status::Unknown => "?",
            Status::Invalid => "✖",
            Status::Truncated => "✂",
            Status::Limited => "⛔",
        }
    }
    pub fn is_problem(&self) -> bool {
        matches!(self, Status::Invalid | Status::Truncated | Status::Limited)
    }
}

/// Kind of SST block, used to choose an entry decoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlockKind {
    Data,
    Index,
    /// Top level of a partitioned (two-level) index; values point to partitions.
    TopIndex,
    IndexPartition,
    Metaindex,
    Properties,
    Filter,
    /// Top-level index of a partitioned filter; values point to filter partitions.
    FilterIndex,
    FilterPartition,
    RangeDel,
    CompressionDict,
    /// Other named meta block (raw).
    Meta,
}

impl BlockKind {
    pub fn label(&self) -> &'static str {
        match self {
            BlockKind::Data => "Data block",
            BlockKind::Index => "Index block",
            BlockKind::TopIndex => "Top-level index (partitioned)",
            BlockKind::IndexPartition => "Index partition",
            BlockKind::Metaindex => "Metaindex block",
            BlockKind::Properties => "Properties block",
            BlockKind::Filter => "Filter block",
            BlockKind::FilterIndex => "Partitioned filter index",
            BlockKind::FilterPartition => "Filter partition",
            BlockKind::RangeDel => "Range deletion block",
            BlockKind::CompressionDict => "Compression dictionary",
            BlockKind::Meta => "Meta block",
        }
    }
}

/// Result of locating a byte: (node that holds it, "select the narrowest field at this
/// offset", structure span when no field applies).
pub type Located = (NodeKind, Option<(Space, u64)>, Option<Span>);

/// Lazy tree node identity. Children/views are computed on demand from the file.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum NodeKind {
    Root,
    // ---- log format (WAL and MANIFEST) ----
    /// Physical blocks [first, first+count).
    LogBlocks {
        first: u64,
        count: u64,
    },
    LogBlock(u64),
    LogFragment(u64),
    /// Page of logical records starting at a file offset that begins a record.
    LogRecords {
        start: u64,
        page: u32,
    },
    LogRecord(u64),
    /// One WriteBatch operation inside a logical record.
    WalOp {
        rec: u64,
        idx: u32,
    },
    /// Observed LSM view built from MANIFEST VersionEdits.
    ManifestLevels,
    /// CURRENT pointing to a MANIFEST.
    CurrentFile,
    // ---- SST ----
    SstFooter,
    SstBlock {
        off: u64,
        size: u64,
        kind: BlockKind,
    },
    SstRestarts {
        off: u64,
        size: u64,
        kind: BlockKind,
    },
    SstTrailer {
        off: u64,
        size: u64,
        kind: BlockKind,
    },
    SstEntry {
        off: u64,
        size: u64,
        kind: BlockKind,
        idx: u32,
    },
    /// Page of data blocks enumerated through the index (`skip` handles skipped).
    SstDataBlocks {
        skip: u64,
    },
    SstProperties,
    /// Bytes between two known structures that no structure accounts for, [start, end).
    SstGap {
        start: u64,
        end: u64,
    },
    /// Run of `count` physical layout items starting at item `first` (data blocks in file
    /// order, grouped because there are many).
    SstLayoutGroup {
        first: u64,
        count: u64,
    },
    /// Table of every physical structure in file order (Start | End | Stored | Payload …).
    SstLayout,
    /// Diagnostic text file (LOG, OPTIONS): raw bounded preview only.
    TextPreview,
}

/// Exact decoded bytes of a value/key for copying (bounded by `LIMITS.copy_cap`; `total` is
/// the full length).
#[derive(Clone, Debug, PartialEq)]
pub struct ValueBytes {
    pub data: Vec<u8>,
    pub total: u64,
}

impl ValueBytes {
    pub fn of(b: &[u8]) -> Self {
        let cap = (crate::limits::LIMITS.copy_cap as usize).min(b.len());
        ValueBytes { data: b[..cap].to_vec(), total: b.len() as u64 }
    }
}

/// Typed identity of a format element. Presenters and the explanation builder select behaviour
/// by this kind (plus status/span), never by the display label: renaming a label changes only
/// what is shown. Kinds are interpreted in the context of the structure that produced them
/// (e.g. `Count` among block facts).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum FieldKind {
    /// No kind-specific behaviour.
    #[default]
    Other,
    /// Level / column family of an SST from its directory's MANIFEST (0030 A7; external).
    ManifestEvidence,
    Magic,
    /// format_version>=6 footer part-2 marker.
    ExtendedMagic,
    FormatVersion,
    ChecksumType,
    Checksum(ChecksumRole),
    /// Per-file base value of the block checksum context modifier.
    BaseContext,
    BlockHandle(HandleTarget),
    /// Stored length / size field (length prefix, payload length).
    Length,
    /// Footer metaindex size (format_version >= 6; the metaindex handle is derived from it).
    MetaindexSize,
    /// Stored offset (e.g. a restart point).
    Offset,
    Count,
    /// Stored sequence number.
    Sequence,
    /// Sequence number derived (not stored) for one operation.
    DerivedSequence,
    /// Record/operation/edit tag.
    Tag,
    /// Column family id (stored, or implicit default when the span is None).
    Cf,
    Key,
    Value,
    UserKey,
    InternalKey,
    CompressionType,
    RecordType,
    Payload,
    BeginKey,
    EndKey,
    BlobIndex,
    Blob,
    Xid,
    CommitTimestamp,
    Entity,
    /// One wide column value (its name is the fact's `aux` span).
    EntityColumn,
    /// Value with a trailing unix write time.
    PackedValue,
    WriteTime,
    /// Custom field whose tag RocksVision does not know.
    UnknownCustomField,
    /// Unknown tag with the safe-to-ignore bit.
    IgnorableField,
    /// Bytes that cannot be framed after an unknown element.
    Remainder,
    /// A list row standing for a whole structure shown elsewhere (layout, record, block and
    /// physical-item lists). A byte click never stops at such a row: it locates the field
    /// inside the structure.
    StructureRef,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChecksumRole {
    Stored,
    Computed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HandleTarget {
    Metaindex,
    Index,
    Block,
}

/// Typed decoded value of a fact (the display string stays in `value`).
#[derive(Clone, Debug, PartialEq)]
pub enum DecodedValue {
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    VarInt(u64),
    Bytes(ValueBytes),
    Text(String),
    Str(&'static str),
    Enum { name: &'static str, code: u64 },
    Handle { offset: u64, size: u64 },
    Bool(bool),
}

impl DecodedValue {
    /// Numeric value, if the decoded value is a number or an enum code.
    pub fn as_u64(&self) -> Option<u64> {
        match *self {
            DecodedValue::U8(x) => Some(u64::from(x)),
            DecodedValue::U16(x) => Some(u64::from(x)),
            DecodedValue::U32(x) => Some(u64::from(x)),
            DecodedValue::U64(x) | DecodedValue::VarInt(x) => Some(x),
            DecodedValue::Enum { code, .. } => Some(code),
            _ => None,
        }
    }
}

/// Which verification found a problem.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CheckKind {
    Crc32c,
    XxHash,
    XxHash64,
    Xxh3,
    Magic,
    Length,
    Varint,
    RestartCount,
    Truncation,
    Handle,
    Decompression,
    /// Decoding stopped at a configured resource limit.
    Limit,
    /// Header count differs from what was decoded.
    Count,
    /// Unknown type/tag value.
    UnknownType,
    /// Recyclable record header carries another log's number (stale record).
    LogNumber,
    /// Inconsistent structure (shared length, non-advancing entry, interrupted record).
    Structure,
    Unsupported,
}

/// How far the reader trusts the bytes after the check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Trust {
    Trusted,
    Untrusted,
    Partial,
}

/// A failed or incomplete verification as data: what was checked, where, expected vs observed,
/// how far the result is trusted and what the reader did next. The free-text `note` equals the
/// message the views show today.
#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub check: CheckKind,
    pub space: Space,
    pub span: Option<Span>,
    pub expected: Option<DecodedValue>,
    pub observed: Option<DecodedValue>,
    pub trusted: Trust,
    /// What RocksVision did next (its own recovery, never a claim about RocksDB).
    pub recovery: &'static str,
    pub note: String,
    /// What the RocksDB engine does in this situation, with its source citation (version, file,
    /// function, lines). Only set where the behaviour was verified in the cited source.
    pub engine: Option<&'static str>,
    /// SST block checksums (format_version ≥ 6): the context modifier added to the computed
    /// value, so `expected` = engine computed + modifier and `observed` = the raw trailer value
    /// (engine "stored" = observed − modifier). None when no modifier applies (or it is 0).
    pub context_modifier: Option<u32>,
}

impl Diagnostic {
    pub fn new(check: CheckKind, span: Option<Span>, trusted: Trust, recovery: &'static str, note: impl Into<String>) -> Self {
        Diagnostic { check, space: span.map(|s| s.space).unwrap_or(Space::File), span, expected: None, observed: None, trusted, recovery, note: note.into(), engine: None, context_modifier: None }
    }
    /// Records the block checksum context modifier (kept only when non-zero).
    pub fn modifier(mut self, m: u32) -> Self {
        self.context_modifier = (m != 0).then_some(m);
        self
    }
    pub fn engine(mut self, e: &'static str) -> Self {
        self.engine = Some(e);
        self
    }
    pub fn in_space(mut self, space: Space) -> Self {
        self.space = space;
        self
    }
    pub fn values(mut self, expected: Option<DecodedValue>, observed: Option<DecodedValue>) -> Self {
        self.expected = expected;
        self.observed = observed;
        self
    }
}

/// A decoded format element as a parser reports it: the element name the format uses, its typed
/// identity (`kind`), byte span, the decoded value (display rendering + typed `decoded`), a
/// bounded raw-bytes preview, the encoding, a status and an optional diagnostic. Explanations,
/// navigation links and table layout are added by presenters.
#[derive(Clone, Debug)]
pub struct Fact {
    pub name: String,
    pub kind: FieldKind,
    /// None for inferred/default/derived values (no invented on-disk range).
    pub span: Option<Span>,
    pub value: String,
    pub decoded: Option<DecodedValue>,
    /// Short raw-bytes preview (hex), bounded.
    pub raw: String,
    pub encoding: String,
    pub status: Status,
    /// Secondary byte span the element refers to (e.g. the name of a wide column).
    pub aux: Option<Span>,
    /// Exact decoded bytes of byte-string elements (keys, values), for copying.
    pub bytes: Option<ValueBytes>,
    pub diag: Option<Box<Diagnostic>>,
}

impl Fact {
    pub fn new(name: impl Into<String>, span: Option<Span>, value: impl Into<String>) -> Self {
        Fact {
            name: name.into(),
            kind: FieldKind::Other,
            span,
            value: value.into(),
            decoded: None,
            raw: String::new(),
            encoding: String::new(),
            status: if span.is_some() { Status::Present } else { Status::Implicit },
            aux: None,
            bytes: None,
            diag: None,
        }
    }
    pub fn kind(mut self, k: FieldKind) -> Self {
        self.kind = k;
        self
    }
    pub fn dv(mut self, v: DecodedValue) -> Self {
        self.decoded = Some(v);
        self
    }
    pub fn diag(mut self, d: Diagnostic) -> Self {
        self.diag = Some(Box::new(d));
        self
    }
    pub fn diag_if(self, cond: bool, d: impl FnOnce() -> Diagnostic) -> Self {
        if cond { self.diag(d()) } else { self }
    }
    pub fn enc(mut self, e: impl Into<String>) -> Self {
        self.encoding = e.into();
        self
    }
    pub fn raw(mut self, bytes: &[u8]) -> Self {
        self.raw = crate::util::hex_preview(bytes, 24);
        self
    }
    pub fn status(mut self, s: Status) -> Self {
        self.status = s;
        self
    }
    pub fn aux(mut self, s: Span) -> Self {
        self.aux = Some(s);
        self
    }
    pub fn bytes(mut self, b: &[u8]) -> Self {
        self.bytes = Some(ValueBytes::of(b));
        self
    }
}

/// A presented field (table row payload) with byte-span provenance, explanation and navigation.
#[derive(Clone, Debug)]
pub struct Field {
    pub name: String,
    pub kind: FieldKind,
    /// None for inferred/default/derived values (no invented on-disk range).
    pub span: Option<Span>,
    pub value: String,
    pub decoded: Option<DecodedValue>,
    /// Problem found while decoding this element (rendered by the explanation panel).
    pub diag: Option<Box<Diagnostic>>,
    /// Short raw-bytes preview (hex), bounded.
    pub raw: String,
    pub encoding: String,
    pub status: Status,
    /// Decoded from bytes whose enclosing checksum failed (status is then Invalid; the UI says
    /// "Untrusted"). Not Unknown: Unknown means missing context (spec §12.6).
    pub untrusted: bool,
    /// Bytes in range but not checked (no status claimed; `status` is then Unknown): the
    /// Derivation sentence saying where the handle places them and when they are checked.
    pub not_checked: Option<String>,
    /// Derivation of a status that comes from a block check (`CHECKSUM_VERIFIED` only when a
    /// checksum was actually compared, else e.g. `NO_CHECKSUM`); None = the generic meaning.
    pub check_note: Option<&'static str>,
    pub explain: String,
    /// Navigation target (e.g. block handle → target block).
    pub link: Option<NodeKind>,
    /// Navigation to another file (e.g. MANIFEST file entry → SST file).
    pub open_path: Option<std::path::PathBuf>,
    /// Exact decoded value bytes ("Copy value"); None → the displayed value is copied.
    pub value_bytes: Option<ValueBytes>,
    /// Exact key bytes of an entry row ("Copy key").
    pub key_bytes: Option<ValueBytes>,
    /// Extra "Other byte interpretations" rows supplied by the presenter (label, value), e.g.
    /// the internal and user key bytes of an entry.
    pub interp: Vec<(&'static str, String)>,
    /// Where a value without bytes in this file comes from (0030 B-8 / A7), e.g. "from the
    /// folder's MANIFEST (read …)"; None: the generic byte-less sentence.
    pub no_bytes_source: Option<String>,
}

impl Field {
    pub fn new(name: impl Into<String>, span: Option<Span>, value: impl Into<String>) -> Self {
        Field {
            name: name.into(),
            kind: FieldKind::Other,
            span,
            value: value.into(),
            decoded: None,
            diag: None,
            raw: String::new(),
            encoding: String::new(),
            status: if span.is_some() { Status::Present } else { Status::Implicit },
            untrusted: false,
            not_checked: None,
            check_note: None,
            explain: String::new(),
            link: None,
            open_path: None,
            value_bytes: None,
            key_bytes: None,
            interp: Vec::new(),
            no_bytes_source: None,
        }
    }
    pub fn enc(mut self, e: impl Into<String>) -> Self {
        self.encoding = e.into();
        self
    }
    pub fn raw(mut self, bytes: &[u8]) -> Self {
        self.raw = crate::util::hex_preview(bytes, 24);
        self
    }
    pub fn status(mut self, s: Status) -> Self {
        self.status = s;
        self
    }
    pub fn value_bytes(mut self, b: &[u8]) -> Self {
        self.value_bytes = Some(ValueBytes::of(b));
        self
    }
    pub fn key_bytes(mut self, b: &[u8]) -> Self {
        self.key_bytes = Some(ValueBytes::of(b));
        self
    }
    pub fn explain(mut self, e: impl Into<String>) -> Self {
        self.explain = e.into();
        self
    }
    pub fn open(mut self, p: std::path::PathBuf) -> Self {
        self.open_path = Some(p);
        self
    }
    pub fn link(mut self, k: NodeKind) -> Self {
        self.link = Some(k);
        self
    }
    pub fn kind(mut self, k: FieldKind) -> Self {
        self.kind = k;
        self
    }
    pub fn dv(mut self, v: DecodedValue) -> Self {
        self.decoded = Some(v);
        self
    }
    pub fn diag(mut self, d: Option<Diagnostic>) -> Self {
        self.diag = d.map(Box::new);
        self
    }
    /// Marks the field as not checked (see `not_checked`): status Unknown, no status claimed.
    pub fn not_checked(mut self, derivation: Option<String>) -> Self {
        if derivation.is_some() {
            self.status = Status::Unknown;
        }
        self.not_checked = derivation;
        self
    }
    /// Derivation of a status from a block check (see `check_note`).
    pub fn check_note(mut self, note: Option<&'static str>) -> Self {
        self.check_note = note;
        self
    }
}

impl From<Fact> for Field {
    fn from(f: Fact) -> Self {
        Field {
            name: f.name,
            kind: f.kind,
            span: f.span,
            value: f.value,
            decoded: f.decoded,
            diag: f.diag,
            raw: f.raw,
            encoding: f.encoding,
            status: f.status,
            untrusted: false,
            not_checked: None,
            check_note: None,
            explain: String::new(),
            link: None,
            open_path: None,
            value_bytes: f.bytes,
            key_bytes: None,
            interp: Vec::new(),
            no_bytes_source: None,
        }
    }
}

/// A table row: display cells plus the field used for highlight/explanation.
#[derive(Clone, Debug)]
pub struct Row {
    pub cells: Vec<String>,
    pub field: Field,
}

/// Semantic role of a table column. The UI derives widths and fill behaviour from the role;
/// nothing matches on display titles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColumnRole {
    /// Row number / ordinal.
    Index,
    Status,
    Offset,
    /// Byte length or small byte count.
    Len,
    /// Element/field name of a fields table.
    Field,
    /// Decoded value of a fields table.
    Decoded,
    Encoding,
    RawHex,
    /// Record/fragment kind or type name.
    Kind,
    /// Framing/header variant.
    Header,
    Seq,
    /// Small counts (operations, fragments, shared/non-shared lengths).
    Count,
    /// Column family id.
    Cf,
    /// Column family id + name.
    CfName,
    Level,
    /// File number.
    FileNo,
    Size,
    /// Key preview (user/internal/separator key).
    Key,
    /// Value preview.
    Value,
    Crc,
    /// Block handle / navigation target.
    Handle,
    /// Internal-key value type.
    ValueType,
    /// Narrow marker (e.g. restart point "R").
    Flag,
    /// On-disk presence of a referenced file.
    Presence,
    /// Free-text summary (edit summary, type list, overlap description).
    Summary,
    /// Entry / property name.
    Name,
    /// Text line.
    Text,
}

impl ColumnRole {
    /// Roles whose column takes the remaining table width (first one in display order wins).
    pub fn fills(&self) -> bool {
        matches!(self, ColumnRole::Decoded | ColumnRole::Text | ColumnRole::Summary | ColumnRole::Name | ColumnRole::Value)
    }
}

#[derive(Clone, Debug)]
pub struct Column {
    pub role: ColumnRole,
    pub title: &'static str,
}

pub fn col(role: ColumnRole, title: &'static str) -> Column {
    Column { role, title }
}

/// Description of an offset space shown by the hex view.
#[derive(Clone, Debug)]
pub struct SpaceInfo {
    pub space: Space,
    pub size: u64,
    pub title: String,
    /// Where the bytes of this space come from in the file.
    pub map: SpaceMap,
}

/// A navigation target: a canonical tree node and optionally the bytes to select in its view.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub node: NodeKind,
    pub span: Option<Span>,
}

/// One line of a view's location header ("file [0x0,0x59) · stored 89 B (0x59)"), with an
/// optional navigation target ("next structure at 0x59 ›").
#[derive(Clone, Debug, PartialEq)]
pub struct WhereLine {
    pub text: String,
    pub link: Option<Target>,
}

/// A view note with an optional status (problem notes are drawn in their status colour).
#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub status: Option<Status>,
    pub text: String,
}

impl Note {
    pub fn with_status(status: Status, text: impl Into<String>) -> Self {
        Note { status: Some(status), text: text.into() }
    }
}

impl From<String> for Note {
    fn from(text: String) -> Self {
        Note { status: None, text }
    }
}

impl From<&str> for Note {
    fn from(text: &str) -> Self {
        Note { status: None, text: text.into() }
    }
}

impl fmt::Display for Note {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

#[derive(Clone, Debug, Default)]
pub struct View {
    pub title: String,
    pub columns: Vec<Column>,
    pub rows: Vec<Row>,
    pub notes: Vec<Note>,
    /// Status of the node as a whole.
    pub status: Option<Status>,
    /// Word shown with the status glyph instead of the status label (e.g. "Incomplete (no
    /// LAST)" for a Truncated record whose fragments are intact); the dump prints the label.
    pub status_word: Option<&'static str>,
    pub spaces: Vec<SpaceInfo>,
    /// Location header lines of the structure shown (file range, stored size, contents,
    /// trailer, next structure / gap), printed by the dump as "where:".
    pub location: Vec<WhereLine>,
    /// File range of the structure shown, when it is one physical structure.
    pub extent: Option<Span>,
    /// Observed-levels inventory (CF → level → live SST), the one typed source of the levels
    /// table rows and the LSM Map; empty for every other view.
    pub lsm: LsmInventory,
    /// Why the live SST files of this file cannot be observed (CURRENT names a missing MANIFEST,
    /// CURRENT invalid): the typed reason the LSM Map availability reads (never parsed back from
    /// cells or notes).
    pub lsm_blocked: Option<String>,
    /// Problems found while decoding the structure shown (not printed by the text dump).
    pub diagnostics: Vec<Diagnostic>,
    /// Check-status counts of every structure this table stands for (all pages; cache only),
    /// for table views whose rows are checked lazily (0019 §1). Not printed by the dump.
    pub tally: Option<RowTally>,
    /// Current tallies of the table view nodes of this file (node kind → counts, cache only),
    /// applied to the tree whenever any view is shown (review S2). Not printed by the dump.
    pub tallies: Vec<(NodeKind, RowTally)>,
    /// WAL files of the levels view's folder (0026 item 4; None: not looked at). Listed by the
    /// file facade with the same bounded folder listing as the WAL count; the session takes it
    /// over as its WAL inventory (summaries are added there). Not printed by the dump.
    pub wals: Option<WalInventory>,
}

// ---------------- WAL overview (0026 item 4) ----------------

/// The log numbers a MANIFEST replay recorded, the evidence of a WAL's lifecycle. Source:
/// RocksDB 10.4.2 db/version_edit_handler.cc 566–576 (kLogNumber applies to the edit's CF and
/// is kept only when it does not decrease), db/version_set.h 1424–1462
/// (`MinLogNumberWithUnflushedData` = min of the live CFs' log numbers).
/// Note (D5 nit 8): the replay applies every complete VersionEdit in file order, including the
/// edits of an incomplete trailing atomic group that RocksDB would discard (the same
/// pre-existing rule as for the live files).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ManifestLogNumbers {
    /// Log number per live (not dropped) CF.
    pub per_cf: std::collections::BTreeMap<u32, u64>,
    /// Live CFs whose edits recorded no log number.
    pub without: std::collections::BTreeSet<u32>,
    /// kMinLogNumberToKeep (largest seen; 2PC), when recorded.
    pub min_to_keep: Option<u64>,
    /// kPrevLogNumber of the last edit that carried it (legacy).
    pub prev: Option<u64>,
    /// The MANIFEST is the one CURRENT names (otherwise nothing is inferred from it).
    pub active: bool,
}

impl ManifestLogNumbers {
    /// `MinLogNumberWithUnflushedData` (None when no live CF recorded a log number).
    pub fn min_unflushed(&self) -> Option<u64> {
        self.per_cf.values().copied().min()
    }
}

/// A WAL relative to the MANIFEST's log numbers (never from the file name alone, never
/// "memtable", never WAL-to-SST ancestry).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WalLifecycle {
    /// The WAL number is below every live CF's log number: per the MANIFEST its writes of every
    /// CF are already in SST files (recovery skips them, write_batch.cc 2203–2213).
    FlushedPerManifest,
    /// At or above the log number of these CFs: the WAL may hold writes of them that are not in
    /// any SST (recovery would replay them).
    MayHoldUnflushed { cfs: Vec<u32> },
    /// Not decidable (why).
    Unknown(LifecycleUnknown),
}

/// Why a WAL's lifecycle is unknown (typed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleUnknown {
    NoNumber,
    NotActive,
    NoLogNumbers,
}

impl LifecycleUnknown {
    pub fn text(&self) -> &'static str {
        match self {
            LifecycleUnknown::NoNumber => "the file name carries no WAL number",
            LifecycleUnknown::NotActive => "this MANIFEST is not confirmed as the active one",
            LifecycleUnknown::NoLogNumbers => "no column family log numbers in the MANIFEST",
        }
    }
}

impl WalLifecycle {
    /// Lifecycle of WAL `number` from the MANIFEST's log numbers (db/version_set.h 1448–1462).
    pub fn of(number: Option<u64>, ln: &ManifestLogNumbers) -> WalLifecycle {
        let Some(n) = number else { return WalLifecycle::Unknown(LifecycleUnknown::NoNumber) };
        if !ln.active {
            return WalLifecycle::Unknown(LifecycleUnknown::NotActive);
        }
        if ln.per_cf.is_empty() || !ln.without.is_empty() {
            return WalLifecycle::Unknown(LifecycleUnknown::NoLogNumbers);
        }
        let cfs: Vec<u32> = ln.per_cf.iter().filter(|(_, l)| n >= **l).map(|(c, _)| *c).collect();
        if cfs.is_empty() { WalLifecycle::FlushedPerManifest } else { WalLifecycle::MayHoldUnflushed { cfs } }
    }
}

/// Counts of a (partial) WAL scan. `cfs` are the column family ids of the WriteBatch
/// operations seen; `seq` the smallest batch sequence and the largest sequence consumed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WalStats {
    pub records: u64,
    pub batches: u64,
    pub cfs: std::collections::BTreeSet<u32>,
    pub seq: Option<(u64, u64)>,
    /// File bytes covered by the scan.
    pub bytes_scanned: u64,
}

/// Background summary of one WAL file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WalSummary {
    /// Not scanned (why).
    NotScanned(NotScannedWhy),
    Scanning,
    /// The whole file was read and every batch decoded: `cfs` is complete.
    Done(WalStats),
    /// Counts of the part read; `cfs` may miss ids.
    Partial {
        stats: WalStats,
        why: PartialWhy,
    },
    /// The log ends in a record of an older log (recyclable header with another log number):
    /// RocksDB 10.4.2 db/log_reader.cc 600–607 returns kOldRecord and 273–295 treats it as the
    /// end of the log, so the records before `at` are this WAL's whole contents.
    StaleTail {
        stats: WalStats,
        at: u64,
    },
    /// A record failed its checksum or framing at `at`; counts are of the records before it.
    /// `recycled`: the log uses recyclable records, so the bytes may be a previous use's
    /// leftover (log_reader.cc 324–328 treats a bad checksum there as the end only in
    /// kTolerateCorruptedTailRecords mode, so this is not called a normal end).
    Corrupt {
        stats: WalStats,
        at: u64,
        recycled: bool,
        reason: String,
    },
    Unreadable(String),
}

/// Why a WAL summary is partial (typed; the text is derived).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PartialWhy {
    /// The per-WAL byte bound was reached (bytes read of the file size).
    Bound { read: u64, size: u64 },
    /// The last record is incomplete at the end of the file.
    TruncatedTail,
    /// A record larger than the summary keeps (CF ids of it may be missing).
    LargeRecord { at: u64 },
    /// A batch with more operations than are listed.
    ManyOps { at: u64 },
    /// More CF ids than are collected.
    ManyCfs,
}

impl PartialWhy {
    pub fn text(&self) -> String {
        match self {
            PartialWhy::Bound { read, size } => format!("summary stops after {read} of {size} bytes (bound)"),
            PartialWhy::TruncatedTail => "the last record is incomplete at the end of the file".into(),
            PartialWhy::LargeRecord { at } => format!("record at 0x{at:x} is larger than the summary keeps; its column families may be incomplete"),
            PartialWhy::ManyOps { at } => format!("record at 0x{at:x} has more operations than are listed"),
            PartialWhy::ManyCfs => format!("more than {} column family ids", crate::limits::WAL_SUMMARY_CFS),
        }
    }
}

impl WalSummary {
    pub fn stats(&self) -> Option<&WalStats> {
        match self {
            WalSummary::Done(s) | WalSummary::Partial { stats: s, .. } | WalSummary::Corrupt { stats: s, .. } | WalSummary::StaleTail { stats: s, .. } => Some(s),
            _ => None,
        }
    }
    /// The CF set of the stats is the WAL's whole set (Done, or the records before a stale
    /// tail, which ends the log).
    pub fn complete(&self) -> bool {
        matches!(self, WalSummary::Done(_) | WalSummary::StaleTail { .. })
    }
    /// How far a complete scan read, for the "none seen" sentence (0030 A6; one source for the
    /// tooltip and the Explanation): Done reads to the end of the file; a stale tail ends the
    /// log (RocksDB stops there), so the scan is complete without reading the old bytes.
    pub fn complete_scan_text(&self) -> Option<String> {
        match self {
            WalSummary::Done(_) => Some("complete scan to the end of the file".into()),
            WalSummary::StaleTail { at, .. } => Some(format!("complete scan of this log's records, which end at the old-record tail at 0x{at:X}")),
            _ => None,
        }
    }
    /// The same extent as the subject of a lifecycle clause ("… found no cf_users records").
    pub fn complete_scan_clause(&self) -> Option<String> {
        match self {
            WalSummary::Done(_) => Some("a complete scan".into()),
            WalSummary::StaleTail { at, .. } => Some(format!("a complete scan of this log's records (ending at the old-record tail at 0x{at:X})")),
            _ => None,
        }
    }
    /// Membership of column family `cf` (unknown unless scanned; absent only after a complete
    /// scan).
    pub fn membership(&self, cf: u32) -> CfMembership {
        match self.stats() {
            Some(s) if s.cfs.contains(&cf) => CfMembership::Observed,
            Some(_) if self.complete() => CfMembership::ObservedAbsent,
            _ => CfMembership::Unknown,
        }
    }
    /// Not scanned yet, or left unscanned by a cancel / refused job: a new request may summarise
    /// it (never a Done / Partial / Corrupt one again).
    pub fn rescannable(&self) -> bool {
        matches!(self, WalSummary::NotScanned(NotScannedWhy::Pending | NotScannedWhy::Cancelled | NotScannedWhy::Failed(_)))
    }
}

/// Why a WAL has no summary (typed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotScannedWhy {
    /// Not requested yet.
    Pending,
    /// Cancel / Esc / a file switch stopped the job.
    Cancelled,
    /// Beyond `WAL_SUMMARY_FILES` (the highest numbers are summarised).
    BeyondCap,
    /// A symlink: listed, never followed.
    Symlink,
    /// The job could not run (bounded message, e.g. too many background reads).
    Failed(String),
}

impl NotScannedWhy {
    pub fn text(&self) -> Option<String> {
        match self {
            NotScannedWhy::Pending => None,
            NotScannedWhy::Cancelled => Some("summary cancelled".into()),
            NotScannedWhy::BeyondCap => Some(format!("only the {} highest-numbered WAL files are summarised", crate::limits::WAL_SUMMARY_FILES)),
            NotScannedWhy::Symlink => Some("symlink (not followed)".into()),
            NotScannedWhy::Failed(e) => Some(e.clone()),
        }
    }
}

/// Whether a WAL holds writes of one column family (the band dims `ObservedAbsent`, never hides).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CfMembership {
    Observed,
    ObservedAbsent,
    Unknown,
}

/// External MANIFEST evidence about one SST (0030 A7): the column family and level that the
/// active MANIFEST of the SST's own directory (named by its CURRENT) lists for this file
/// number. Not stored in the SST; absent when there is no such evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SstManifestEvidence {
    /// The MANIFEST file name (e.g. "MANIFEST-000005").
    pub manifest: String,
    pub number: u64,
    pub cf_id: u32,
    pub cf_name: String,
    pub level: u32,
    /// When the MANIFEST was read (UTC seconds since the epoch), for the Explanation.
    pub read_at: Option<u64>,
}

/// What the SST's own directory says about it (0030 A7): evidence, or why there is none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SstManifestContext {
    /// The active MANIFEST lists this file number as live.
    Listed(SstManifestEvidence),
    /// The active MANIFEST (named) was replayed completely and does not list this number.
    NotListed { manifest: String, number: u64 },
    /// The active MANIFEST (named) could not be read or replayed completely.
    Untrusted { manifest: String },
    /// No CURRENT / active MANIFEST next to the file, or no numbered `<n>.sst` name: a
    /// standalone SST.
    Standalone,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WalFile {
    /// From the file name ("NNNNNN.log"); None for another *.log name.
    pub number: Option<u64>,
    pub path: std::path::PathBuf,
    pub name: String,
    pub size: u64,
    pub mtime: Option<std::time::SystemTime>,
    /// A symlink: listed, never followed (not summarised, not opened).
    pub symlink: bool,
    pub summary: WalSummary,
}

/// WAL files of one folder, by number (unnumbered names last).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WalInventory {
    pub files: Vec<WalFile>,
    /// More than `MAX_WAL_LISTED` WAL files, or the folder listing itself was truncated.
    pub truncated: bool,
}

impl WalInventory {
    pub fn by_number(&self, n: u64) -> Option<&WalFile> {
        self.files.iter().find(|f| f.number == Some(n))
    }
    /// Summaries of `old` for the same files (path, size, mtime), so a re-shown levels view
    /// does not rescan (a Scanning file stays Scanning: its job of this file generation still
    /// answers by path). Indexed by path (D3 N4: no quadratic search at the 1000-file cap).
    pub fn keep_summaries(&mut self, old: &WalInventory) {
        let by_path: std::collections::HashMap<&std::path::Path, &WalFile> = old.files.iter().map(|o| (o.path.as_path(), o)).collect();
        for f in &mut self.files {
            if let Some(o) = by_path.get(f.path.as_path()).filter(|o| o.size == f.size && o.mtime == f.mtime) {
                f.summary = o.summary.clone();
            }
        }
    }
}

/// "1 file" / "N files" (CF selector, level header, Explanation summary, density bar).
pub fn files_word(n: usize) -> String {
    if n == 1 { "1 file".into() } else { format!("{n} files") }
}

/// Neutral wording of a folder's WAL count (levels note and folder note share it): absence is
/// normal (a checkpoint or a flushed DB).
pub fn wal_words(n: usize) -> String {
    match n {
        0 => "no WAL (normal for a checkpoint or a flushed DB)".into(),
        1 => "1 WAL".into(),
        n => format!("{n} WAL"),
    }
}

/// Key order of a column family's comparator, for range placement (MANIFEST kComparator,
/// db/version_edit.h). Only the two built-in byte orders are understood; any other comparator
/// is `Unsupported(name)`, and no recorded comparator is `Unknown`: neither is ever placed or
/// compared as bytes.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum KeyOrder {
    /// "leveldb.BytewiseComparator" (util/comparator.cc BytewiseComparatorImpl::Name).
    Bytewise,
    /// "rocksdb.ReverseBytewiseComparator" (util/comparator.cc ReverseBytewiseComparatorImpl).
    ReverseBytewise,
    /// The MANIFEST recorded no comparator for the CF (order unknown).
    #[default]
    Unknown,
    /// Any other comparator name (order unknown to RocksVision).
    Unsupported(String),
}

impl KeyOrder {
    pub fn from_comparator(name: Option<&str>) -> KeyOrder {
        match name {
            Some("leveldb.BytewiseComparator") => KeyOrder::Bytewise,
            Some("rocksdb.ReverseBytewiseComparator") => KeyOrder::ReverseBytewise,
            Some(n) => KeyOrder::Unsupported(n.to_string()),
            None => KeyOrder::Unknown,
        }
    }
    /// Key ranges of this order can be placed and compared (byte order, possibly reversed).
    pub fn ordered(&self) -> bool {
        matches!(self, KeyOrder::Bytewise | KeyOrder::ReverseBytewise)
    }
    /// Comparator sentence for notes, explanations and the map legend.
    pub fn describe(&self) -> String {
        match self {
            KeyOrder::Bytewise => "bytewise (leveldb.BytewiseComparator)".into(),
            KeyOrder::ReverseBytewise => "reverse bytewise (rocksdb.ReverseBytewiseComparator)".into(),
            KeyOrder::Unknown => "not recorded in the MANIFEST (order unknown)".into(),
            KeyOrder::Unsupported(n) => format!("{n} (not supported: key order unknown)"),
        }
    }
    /// The LSM Map's x-axis sentence (full text; strip, Explanation and tooltip).
    pub fn legend(&self) -> String {
        match self {
            KeyOrder::Bytewise => "x: user-key order (bytewise comparator), boxes placed by key rank, not to scale".into(),
            KeyOrder::ReverseBytewise => "x: user-key order (reverse bytewise comparator: larger keys on the left), not to scale".into(),
            KeyOrder::Unknown => "x: file number order. Comparator not recorded, so key order, overlap and gaps are not shown; boxes are equal in size.".into(),
            KeyOrder::Unsupported(n) => format!("x: file number order. Comparator '{n}' is not supported, so key order, overlap and gaps are not shown; boxes are equal in size."),
        }
    }
    /// Comparator order of two user keys (bytewise, or reversed); None for an unknown order.
    pub fn cmp_keys(&self, a: &[u8], b: &[u8]) -> Option<std::cmp::Ordering> {
        match self {
            KeyOrder::Bytewise => Some(a.cmp(b)),
            KeyOrder::ReverseBytewise => Some(b.cmp(a)),
            _ => None,
        }
    }
    /// First and last boundary key of these files in comparator order (observed smallest /
    /// largest keys only); None for an unknown order or no files.
    pub fn span<'a>(&self, files: impl Iterator<Item = &'a LsmFile>) -> Option<(&'a [u8], &'a [u8])> {
        let mut out: Option<(&[u8], &[u8])> = None;
        for f in files {
            for k in [f.smallest.as_slice(), f.largest.as_slice()] {
                out = Some(match out {
                    None => (k, k),
                    Some((lo, hi)) => {
                        let lo = if self.cmp_keys(k, lo)?.is_lt() { k } else { lo };
                        let hi = if self.cmp_keys(k, hi)?.is_gt() { k } else { hi };
                        (lo, hi)
                    }
                });
            }
        }
        self.cmp_keys(b"", b"")?;
        out
    }
    /// Short legend for a narrow strip.
    pub fn short_legend(&self) -> &'static str {
        match self {
            KeyOrder::Bytewise => "x: key order (bytewise)",
            KeyOrder::ReverseBytewise => "x: key order (reverse bytewise)",
            KeyOrder::Unknown | KeyOrder::Unsupported(_) => "x: file number order",
        }
    }
}

/// One live SST file observed from the MANIFEST (typed; the levels table row is derived from
/// the same values).
#[derive(Clone, Debug, PartialEq)]
pub struct LsmFile {
    pub number: u64,
    pub size: u64,
    /// Smallest / largest USER key (internal key minus its 8-byte trailer).
    pub smallest: Vec<u8>,
    pub largest: Vec<u8>,
    /// Escaped previews of the internal keys (`ikey_preview`: user key, seq, type).
    pub smallest_preview: String,
    pub largest_preview: String,
    pub seq: Option<(u64, u64)>,
    /// Same-level / next-level overlap counts; None when not computed (unsupported key order,
    /// level above `OVERLAP_MAX_FILES`).
    pub overlap_same: Option<u32>,
    pub overlap_next: Option<u32>,
    /// Present (on disk) or NotPresent (missing from the directory).
    pub status: Status,
    /// Index of the levels table row.
    pub row: usize,
    /// Path to open (on disk only).
    pub path: Option<std::path::PathBuf>,
    /// Horizontal slots [start, end) on its level's scale (`LsmLevel::scale`): ranks of the
    /// smallest/largest user key among the CF's distinct boundary keys in comparator order
    /// (end = rank(largest) + 1), or the file's index in number order for an unknown order.
    pub slots: (u32, u32),
    /// Overlap lane within the level (greedy interval colouring; 0 for an unknown order).
    pub lane: u32,
}

impl LsmFile {
    pub fn on_disk(&self) -> bool {
        self.status == Status::Present
    }
    pub fn name(&self) -> String {
        format!("{:06}.sst", self.number)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LsmLevel {
    pub level: u32,
    pub files: Vec<LsmFile>,
    pub bytes: u64,
    pub on_disk: usize,
    /// Slot count of the x scale (the CF's distinct boundary keys, or the level's file count
    /// for an unknown order).
    pub scale: u32,
    /// Overlap lanes used (1 for a non-overlapping level).
    pub lanes: u32,
    /// Unexpected overlap: an L1+ level whose files overlap each other (ordered keys only; L0
    /// overlap is normal and never sets it).
    pub overlaps: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LsmCf {
    pub id: u32,
    pub name: String,
    pub levels: Vec<LsmLevel>,
    /// The CF's comparator (MANIFEST kComparator) and its key order.
    pub comparator: Option<String>,
    pub order: KeyOrder,
}

impl LsmCf {
    pub fn files(&self) -> usize {
        self.levels.iter().map(|l| l.files.len()).sum()
    }
    pub fn bytes(&self) -> u64 {
        self.levels.iter().map(|l| l.bytes).sum()
    }
}

/// Observed live SSTs grouped by CF and level, with the comparator's key order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LsmInventory {
    pub cfs: Vec<LsmCf>,
    /// The MANIFEST stopped tracking files at `MAX_LIVE_FILES` (listing truncated).
    pub truncated: bool,
    /// Per-CF log numbers of the replay (the WAL lifecycle evidence).
    pub log_numbers: ManifestLogNumbers,
}

impl LsmInventory {
    pub fn is_empty(&self) -> bool {
        self.cfs.iter().all(|c| c.levels.is_empty())
    }
    /// The comparator sentence(s) (0030 A8-3): one line when every CF has the same key order
    /// (`Comparator (all 3 CFs): …`), else one line per CF. Levels notes and Explanation share it.
    pub fn comparator_lines(&self) -> Vec<String> {
        match self.cfs.as_slice() {
            [] => Vec::new(),
            [c] => vec![format!("Comparator of CF {} {}: {}", c.id, c.name, c.order.describe())],
            [first, rest @ ..] if rest.iter().all(|c| c.order == first.order) => vec![format!("Comparator (all {} CFs): {}", self.cfs.len(), first.order.describe())],
            cfs => cfs.iter().map(|c| format!("Comparator of CF {} {}: {}", c.id, c.name, c.order.describe())).collect(),
        }
    }
    /// The x-axis sentence(s): once when every CF has the same key order, else per CF.
    pub fn legend_lines(&self) -> Vec<String> {
        match self.cfs.as_slice() {
            [] => Vec::new(),
            [first, rest @ ..] if rest.iter().all(|c| c.order == first.order) => vec![first.order.legend()],
            cfs => cfs.iter().map(|c| format!("CF {} {}: {}", c.id, c.name, c.order.legend())).collect(),
        }
    }
    /// Every file, in table-row order.
    pub fn iter(&self) -> impl Iterator<Item = &LsmFile> {
        self.cfs.iter().flat_map(|c| c.levels.iter().flat_map(|l| l.files.iter()))
    }
    pub fn cf(&self, id: u32) -> Option<&LsmCf> {
        self.cfs.iter().find(|c| c.id == id)
    }
    /// (CF, level, file) of table row `row`.
    pub fn by_row(&self, row: usize) -> Option<(&LsmCf, &LsmLevel, &LsmFile)> {
        self.locate(|f| f.row == row)
    }
    /// (CF, level, file) of file number `n`.
    pub fn by_number(&self, n: u64) -> Option<(&LsmCf, &LsmLevel, &LsmFile)> {
        self.locate(|f| f.number == n)
    }
    fn locate(&self, pred: impl Fn(&LsmFile) -> bool) -> Option<(&LsmCf, &LsmLevel, &LsmFile)> {
        self.cfs.iter().find_map(|c| c.levels.iter().find_map(|l| l.files.iter().find(|f| pred(f)).map(|f| (c, l, f))))
    }
    /// The Overlap cell / fact of a file.
    pub fn overlap_text(cf: &LsmCf, level: &LsmLevel, f: &LsmFile) -> String {
        match (f.overlap_same, f.overlap_next) {
            (Some(s), n) if level.level == 0 => format!("{s} in L0 (allowed){}", n.map(|n| format!(", {n} in L1")).unwrap_or_default()),
            (Some(s), n) => format!("{s} same level{}{}", if s > 0 { " (unexpected)" } else { "" }, n.map(|n| format!(", {n} in L{}", level.level.saturating_add(1))).unwrap_or_default()),
            (None, _) if !cf.order.ordered() => format!("not computed (comparator {})", cf.comparator.as_deref().unwrap_or("not recorded")),
            (None, _) => "not computed (resource limit)".to_string(),
        }
    }
    /// Facts of one file (Explanation card and map tooltip share them): label → value.
    pub fn facts(cf: &LsmCf, level: &LsmLevel, f: &LsmFile) -> Vec<(&'static str, String)> {
        let overlap = Self::overlap_text(cf, level, f);
        vec![
            ("File", f.name()),
            ("Column family", format!("{} {}", cf.id, cf.name)),
            ("Level", format!("L{}", level.level)),
            ("Size", crate::util::human_size(f.size)),
            ("Smallest", f.smallest_preview.clone()),
            ("Largest", f.largest_preview.clone()),
            ("Seq range", f.seq.map(|(a, b)| format!("{a}..{b}")).unwrap_or_else(|| "-".into())),
            ("Overlap", overlap),
            ("Key order", cf.order.describe()),
            ("On disk", if f.on_disk() { "yes".into() } else { "missing from this directory".into() }),
        ]
    }
}

/// What a tree child stands for. Physical structures appear once, in file order; views and
/// references only point at them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ChildRole {
    /// A physical structure at its one canonical place (expandable when it has parts).
    #[default]
    Structure,
    /// A labeled derived/reassembled view (Logical records, Observed levels, Data blocks via
    /// index); listed under "Views".
    View,
    /// A link to a canonical structure elsewhere in the tree (never expanded in place).
    Reference,
    /// A non-expandable "not present" statement.
    Absent,
}

/// Lazily expandable tree child.
#[derive(Clone, Debug)]
pub struct Child {
    pub label: String,
    pub kind: NodeKind,
    pub status: Status,
    pub leaf: bool,
    pub diag: Option<Box<Diagnostic>>,
    /// A problem status found inside this node that its own `status` does not show (e.g. the
    /// truncated last record of a WAL under "Physical blocks"). UI marker only; the text dump
    /// prints `status`.
    pub contains: Option<Status>,
    pub role: ChildRole,
    /// A group whose status comes from its members: (members checked, members). `(0, n)` =
    /// nothing checked yet (no glyph, "· not checked"); otherwise `status` is the worst checked
    /// member (Present when the checked ones are clean; the glyph rule is `group_glyph`).
    pub checked: Option<(u64, u64)>,
    /// A table view node (Physical layout table, Data blocks via index): the status counts of
    /// the structures its table lists, from the check cache (0019 §1). Decides its glyph.
    pub tally: Option<RowTally>,
}

/// Check-status counts of the structures a table lists (cache only, nothing read): the one
/// source of a table view node's glyph, its tooltip and the table's summary line (0019 §1).
/// Rows with another status (e.g. an unaccounted gap) are not counted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RowTally {
    pub invalid: u64,
    pub truncated: u64,
    pub present: u64,
    pub not_checked: u64,
    /// Checked with another outcome (e.g. "? Unknown" for an unknown checksum type, "⛔
    /// Resource limit" for a block over the cap): neither a problem found nor ✔ (review S1).
    pub other: u64,
}

impl RowTally {
    /// Number of structures counted.
    pub fn total(&self) -> u64 {
        self.invalid + self.truncated + self.present + self.not_checked + self.other
    }
    /// Adds one structure: `None` = not checked.
    pub fn add(&mut self, s: Option<Status>) {
        match s {
            None => self.not_checked += 1,
            Some(Status::Invalid) => self.invalid += 1,
            Some(Status::Truncated) => self.truncated += 1,
            Some(Status::Present) => self.present += 1,
            Some(_) => self.other += 1,
        }
    }
    /// Takes back one structure counted by `add(s)` (its status changed; 0020 D1).
    pub fn remove(&mut self, s: Option<Status>) {
        let n = match s {
            None => &mut self.not_checked,
            Some(Status::Invalid) => &mut self.invalid,
            Some(Status::Truncated) => &mut self.truncated,
            Some(Status::Present) => &mut self.present,
            Some(_) => &mut self.other,
        };
        *n = n.saturating_sub(1);
    }
    /// The glyph (worst of the rows, same "no claim" rule as groups, design A3): ✖ when any
    /// row is Invalid, else ✂ when any is Truncated, else none while a row is not checked or
    /// has another outcome (Unknown, Limited …), else ✔.
    pub fn glyph(&self) -> Option<Status> {
        if self.invalid > 0 {
            Some(Status::Invalid)
        } else if self.truncated > 0 {
            Some(Status::Truncated)
        } else if self.not_checked + self.other > 0 {
            None
        } else {
            Some(Status::Present)
        }
    }
    /// Summary next to the table title: "✖ 1 invalid · ✔ 515 present · 4 not checked" (fixed
    /// order, zero terms left out; "not checked" has no glyph). None when nothing is counted.
    pub fn summary(&self) -> Option<String> {
        let parts: Vec<String> = self.parts().into_iter().map(|(_, t)| t).collect();
        (!parts.is_empty()).then(|| parts.join(" · "))
    }
    /// The summary's terms with their status (None: "not checked", shown without colour).
    pub fn parts(&self) -> Vec<(Option<Status>, String)> {
        [
            (self.invalid, Some(Status::Invalid), "invalid"),
            (self.truncated, Some(Status::Truncated), "truncated"),
            (self.present, Some(Status::Present), "present"),
            (self.not_checked, None, "not checked"),
            (self.other, None, "other"),
        ]
        .into_iter()
        .filter(|(n, _, _)| *n > 0)
        .map(|(n, s, w)| (s, s.map_or_else(|| format!("{n} {w}"), |s| format!("{} {n} {w}", s.symbol()))))
        .collect()
    }
    /// Tree tooltip of the view node.
    pub fn tooltip(&self) -> String {
        let other = if self.other > 0 { format!(" ({} other: unknown or limited, see their rows)", self.other) } else { String::new() };
        match self.glyph() {
            Some(Status::Invalid | Status::Truncated) => {
                let tr = if self.truncated > 0 { format!(", {} truncated", self.truncated) } else { String::new() };
                let ot = if self.other > 0 { format!(", {} other", self.other) } else { String::new() };
                format!("Contains {} invalid{tr}, {} present{ot} and {} not-checked structures.", self.invalid, self.present, self.not_checked)
            }
            None if self.not_checked > 0 => {
                format!("Not checked as a whole: {} of {} rows checked, none with a problem{other}.", self.present + self.other, self.present + self.other + self.not_checked)
            }
            None => format!("No status as a whole: {} present and {} other structures (unknown or limited, see their rows).", self.present, self.other),
            _ => format!("All {} structures checked; none with a problem.", self.present),
        }
    }
}

/// Where the levels view's values come from when they have no bytes in the open file (0030
/// B-8: "No bytes in this file: from the MANIFEST.").
pub const NO_BYTES_MANIFEST: &str = "from the MANIFEST";

/// Status word of bytes that are in range but were not checked (no glyph, no status claimed).
/// "Checked" means only "checksum and trailer verified" (design A1).
pub const NOT_CHECKED: &str = "Not checked";
/// Derivation of a Present block whose stored checksum was compared and matched (design A2).
pub const CHECKSUM_VERIFIED: &str = "Present: checksum verified.";
/// Derivation of a Present block of a kNoChecksum file: read, nothing to compare.
pub const NO_CHECKSUM: &str = "Present: block read; file has no checksum (kNoChecksum).";
/// Derivation of a Present block whose checksum was not compared (unknown checksum type).
pub const CHECKSUM_NOT_COMPARED: &str = "Present: block read; checksum not compared.";

/// A group's check coverage: "not checked" / "3 of 64 checked" (None: not a group).
pub fn coverage_text(c: Option<(u64, u64)>) -> Option<String> {
    match c? {
        (0, _) => Some("not checked".into()),
        (x, y) => Some(format!("{x} of {y} checked")),
    }
}

/// Group label with its coverage after the head (`{what} #a–#z · {coverage} · {range} · {n}
/// blocks`), so the coverage is never behind a long range. Labels without " · " get it appended.
pub fn with_coverage(label: &str, c: Option<(u64, u64)>) -> String {
    match (coverage_text(c), label.split_once(" · ")) {
        (None, _) => label.to_string(),
        (Some(cov), Some((head, tail))) => format!("{head} · {cov} · {tail}"),
        (Some(cov), None) => format!("{label} · {cov}"),
    }
}

/// Severity order of statuses (higher is worse): the one ranking for worst-of aggregation (tree
/// groups, cache coverage) and the single tree glyph.
pub fn rank(s: Status) -> u8 {
    match s {
        Status::Present | Status::Implicit | Status::NotPresent => 0,
        Status::Unknown | Status::Unsupported | Status::Limited => 1,
        Status::Truncated => 2,
        Status::Invalid => 3,
    }
}

/// The glyph a group shows (design A3): a checked member's problem (Invalid / Truncated /
/// resource limit) always; otherwise only when every member was checked (✔ "y of y checked");
/// none while members are unchecked. Non-groups always show their status.
pub fn group_glyph(status: Status, c: Option<(u64, u64)>) -> Option<Status> {
    match c {
        None => Some(status),
        _ if status.is_problem() => Some(status),
        Some((x, y)) if x > 0 && x >= y => Some(status),
        Some(_) => None,
    }
}

impl Child {
    pub fn new(label: impl Into<String>, kind: NodeKind) -> Self {
        Child { label: label.into(), kind, status: Status::Present, leaf: false, diag: None, contains: None, role: ChildRole::Structure, checked: None, tally: None }
    }
    /// A table view node's row tally (see `tally`); its status is the tally's glyph (Unknown
    /// while it claims none).
    pub fn tally(mut self, t: RowTally) -> Self {
        self.status = t.glyph().unwrap_or(Status::Unknown);
        self.tally = Some(t);
        self
    }
    /// The glyph this child shows: the tally's, a group's (`group_glyph`) or its status.
    pub fn glyph(&self) -> Option<Status> {
        match &self.tally {
            Some(t) => t.glyph(),
            None => group_glyph(self.status, self.checked),
        }
    }
    /// A group's check coverage (see `checked`).
    pub fn checked(mut self, c: (u64, u64)) -> Self {
        self.checked = Some(c);
        self
    }
    pub fn role(mut self, r: ChildRole) -> Self {
        self.role = r;
        if r == ChildRole::Reference || r == ChildRole::Absent {
            self.leaf = true;
        }
        self
    }
    pub fn contains(mut self, s: Option<Status>) -> Self {
        self.contains = s;
        self
    }
    pub fn diag(mut self, d: Option<Diagnostic>) -> Self {
        self.diag = d.map(Box::new);
        self
    }
    pub fn leaf(mut self) -> Self {
        self.leaf = true;
        self
    }
    pub fn status(mut self, s: Status) -> Self {
        self.status = s;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 0020 §2: spans near `u64::MAX` never panic; ends and mapped ranges saturate.
    #[test]
    fn span_bounds_near_u64_max() {
        const M: u64 = u64::MAX;
        // (span, probe offset, contains?)
        for (s, off, want) in [
            (Span::file(M - 1, 1), M - 1, true),
            (Span::file(M - 1, 1), M, false),
            (Span::file(M - 1, 5), M, true),
            (Span::file(M, 0), M, true),
            (Span::file(M, 0), M - 1, false),
            (Span::file(M - 4, M), M, true),
            (Span::file(0, M), M - 1, true),
            (Span::file(0, M), M, false),
            (Span::new(Space::Block(7), M, 1), M, true),
            (Span::new(Space::Block(7), M, 1), 0, false),
        ] {
            assert_eq!(s.contains(s.space, off), want, "{s:?} contains {off:#x}");
            assert!(!s.contains(Space::Record(9), off), "other space");
        }
        assert_eq!(Span::file(M - 1, 5).end(), M);
        assert_eq!(Span::file(M - 1, 5).range_text(), format!("file [0x{:x},0x{M:x}) · 5 B", M - 1));
        assert_eq!(Span::new(Space::Record(M), M - 2, 9).range_text(), format!("record@0x{M:x} [+0x{:x},+0x{M:x}) · 9 B", M - 2));
        // Fragment maps whose record or file offsets reach the end of u64.
        let map = SpaceMap::Fragments { segs: vec![(0, M - 4, 4), (4, M - 2, M)], headers: vec![] };
        assert_eq!(map.file_ranges(2, 4), FileRanges::Exact { ranges: vec![Span::file(M - 2, 2), Span::file(M - 2, 2)], skipped: vec![] });
        assert_eq!(map.file_ranges(M - 1, 1), FileRanges::Exact { ranges: vec![Span::file(M, 1)], skipped: vec![] });
        assert_eq!(map.file_ranges(M, 0), FileRanges::Unknown, "the end of the record space holds no byte");
    }
}
