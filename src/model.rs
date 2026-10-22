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
    pub fn contains(&self, space: Space, off: u64) -> bool {
        self.space == space && off >= self.offset && off < self.offset + self.len.max(1)
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

/// Lazy tree node identity. Children/views are computed on demand from the file.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum NodeKind {
    Root,
    // ---- log format (WAL and MANIFEST) ----
    /// Physical blocks [first, first+count).
    LogBlocks { first: u64, count: u64 },
    LogBlock(u64),
    LogFragment(u64),
    /// Page of logical records starting at a file offset that begins a record.
    LogRecords { start: u64, page: u32 },
    LogRecord(u64),
    /// One WriteBatch operation inside a logical record.
    WalOp { rec: u64, idx: u32 },
    /// Observed LSM view built from MANIFEST VersionEdits.
    ManifestLevels,
    /// CURRENT pointing to a MANIFEST.
    CurrentFile,
    // ---- SST ----
    SstFooter,
    SstBlock { off: u64, size: u64, kind: BlockKind },
    SstRestarts { off: u64, size: u64, kind: BlockKind },
    SstTrailer { off: u64, size: u64, kind: BlockKind },
    SstEntry { off: u64, size: u64, kind: BlockKind, idx: u32 },
    /// Page of data blocks enumerated through the index (`skip` handles skipped).
    SstDataBlocks { skip: u64 },
    SstProperties,
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
    /// What the reader did next.
    pub recovery: &'static str,
    pub note: String,
}

impl Diagnostic {
    pub fn new(check: CheckKind, span: Option<Span>, trusted: Trust, recovery: &'static str, note: impl Into<String>) -> Self {
        Diagnostic { check, space: span.map(|s| s.space).unwrap_or(Space::File), span, expected: None, observed: None, trusted, recovery, note: note.into() }
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
        if cond {
            self.diag(d())
        } else {
            self
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
    pub explain: String,
    /// Navigation target (e.g. block handle → target block).
    pub link: Option<NodeKind>,
    /// Navigation to another file (e.g. MANIFEST file entry → SST file).
    pub open_path: Option<std::path::PathBuf>,
    /// Exact decoded value bytes ("Copy value"); None → the displayed value is copied.
    pub value_bytes: Option<ValueBytes>,
    /// Exact key bytes of an entry row ("Copy key").
    pub key_bytes: Option<ValueBytes>,
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
            explain: String::new(),
            link: None,
            open_path: None,
            value_bytes: None,
            key_bytes: None,
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
            explain: String::new(),
            link: None,
            open_path: None,
            value_bytes: f.bytes,
            key_bytes: None,
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
    pub spaces: Vec<SpaceInfo>,
    /// Observed-levels files for the LSM box diagram (row index into `rows` + typed cells).
    pub lsm: Vec<LsmFile>,
    /// Problems found while decoding the structure shown (not printed by the text dump).
    pub diagnostics: Vec<Diagnostic>,
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
}

impl Child {
    pub fn new(label: impl Into<String>, kind: NodeKind) -> Self {
        Child { label: label.into(), kind, status: Status::Present, leaf: false, diag: None, contains: None }
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
