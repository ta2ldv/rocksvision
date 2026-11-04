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
    /// Observed-levels files for the LSM box diagram (row index into `rows` + typed cells).
    pub lsm: Vec<LsmFile>,
    /// Problems found while decoding the structure shown (not printed by the text dump).
    pub diagnostics: Vec<Diagnostic>,
    /// Check-status counts of every structure this table stands for (all pages; cache only),
    /// for table views whose rows are checked lazily (0019 §1). Not printed by the dump.
    pub tally: Option<RowTally>,
    /// Current tallies of the table view nodes of this file (node kind → counts, cache only),
    /// applied to the tree whenever any view is shown (review S2). Not printed by the dump.
    pub tallies: Vec<(NodeKind, RowTally)>,
}

/// One live SST file of the observed LSM shape, as shown in the box diagram.
#[derive(Clone, Debug)]
pub struct LsmFile {
    /// Index of the corresponding table row.
    pub row: usize,
    pub cf: String,
    pub level: String,
    pub file: String,
    pub size: String,
    pub smallest: String,
    pub largest: String,
    pub overlap: String,
    pub status: Status,
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
