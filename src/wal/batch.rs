//! WriteBatch decoder (logical WAL record payload), RocksDB 10.4.2.
//! Source: db/write_batch.cc (ReadRecordFromWriteBatch), db/dbformat.h (ValueType).
//! Layout: sequence fixed64 | count fixed32 | records...
//! Offsets are relative to the reassembled record (Space::Record).
//! Output is typed: operations with their decoded parts plus one `Fact` per encoded element
//! (explanations are added by `present::wal`).

pub use crate::format::ikey::value_type_name as tag_name;
use crate::limits::MAX_ENTITY_COLUMNS;
use crate::model::{CheckKind, DecodedValue as DV, Diagnostic, Fact, FieldKind as K, Space, Span, Status, Trust};
use crate::util::{bytes_preview, fixed32, fixed64, varint32};

pub const HEADER: usize = 12;

/// Tags that consume a sequence number (WriteBatchInternal counts these).
fn consumes_seq(t: u8) -> bool {
    matches!(t, 0x0 | 0x1 | 0x2 | 0x4..=0x8 | 0xE | 0xF | 0x10 | 0x11 | 0x16..=0x19)
}

#[derive(Clone, Debug)]
pub struct Op {
    pub idx: u32,
    pub offset: usize,
    pub end: usize,
    pub tag: u8,
    pub cf: Option<u32>,
    pub key: Option<Vec<u8>>,
    pub value: Option<Vec<u8>>,
    pub seq: Option<u64>,
    pub status: Status,
    /// Encoded elements of this operation, in byte order (plus implicit/derived ones).
    pub facts: Vec<Fact>,
}

impl Op {
    pub fn summary_key(&self) -> String {
        self.key.as_deref().map(|k| bytes_preview(k, 48)).unwrap_or_default()
    }
    pub fn summary_value(&self) -> String {
        self.value.as_deref().map(|k| bytes_preview(k, 64)).unwrap_or_default()
    }
}

#[derive(Clone, Debug)]
pub struct Batch {
    /// Sequence and count elements of the 12-byte header.
    pub header: Vec<Fact>,
    pub seq: Option<u64>,
    pub count: Option<u32>,
    pub ops: Vec<Op>,
    pub status: Status,
    pub note: String,
    /// The problem behind a non-Present `status` (its note equals `note`).
    pub diag: Option<Diagnostic>,
}

const RECOVER_STOP: &str = "decoding of this batch stops here (write_batch.cc fails with Corruption)";
const RECOVER_PREVIEW: &str = "the bounded preview is decoded; the rest of the record is not";

struct Cur<'a> {
    d: &'a [u8],
    pos: usize,
    space: Space,
    /// True when `d` is a bounded prefix of a longer record.
    clipped: bool,
    /// Set when a value was cut by the bounded prefix (partial preview returned).
    hit_limit: bool,
}

impl Cur<'_> {
    fn fail(&self) -> Status {
        if self.clipped && self.pos >= self.d.len().saturating_sub(10) {
            Status::Limited
        } else {
            Status::Truncated
        }
    }
    fn v32(&mut self, name: &str, kind: K, fields: &mut Vec<Fact>) -> Result<u32, Status> {
        let (v, n) = varint32(self.d, self.pos).ok_or_else(|| self.fail())?;
        fields.push(
            Fact::new(name, Some(Span::new(self.space, self.pos as u64, n as u64)), v.to_string())
                .kind(kind)
                .dv(DV::VarInt(u64::from(v)))
                .raw(&self.d[self.pos..self.pos + n])
                .enc("varint32"),
        );
        self.pos += n;
        Ok(v)
    }
    /// GetLengthPrefixedSlice: varint32 length + bytes.
    fn slice(&mut self, name: &str, kind: K, fields: &mut Vec<Fact>) -> Result<Vec<u8>, Status> {
        let len = self.v32(&format!("{name} length"), K::Length, fields)? as usize;
        let s = self.pos;
        if s + len > self.d.len() && self.clipped {
            let have = self.d.len().saturating_sub(s);
            let b = self.d[s.min(self.d.len())..].to_vec();
            let span = Span::new(self.space, s as u64, have as u64);
            fields.push(
                Fact::new(name, Some(span), format!("{} (first {have} of {len} bytes)", bytes_preview(&b, 64)))
                    .kind(kind)
                    .raw(&b)
                    .bytes(&b)
                    .status(Status::Limited)
                    .diag(Diagnostic::new(CheckKind::Limit, Some(span), Trust::Partial, RECOVER_PREVIEW, format!("first {have} of {len} bytes kept")).values(Some(DV::U64(len as u64)), Some(DV::U64(have as u64)))),
            );
            self.pos = self.d.len();
            self.hit_limit = true;
            return Ok(b);
        }
        if s + len > self.d.len() {
            let st = self.fail();
            let have = self.d.len().saturating_sub(s);
            let span = Span::new(self.space, s as u64, have as u64);
            let check = if st == Status::Limited { CheckKind::Limit } else { CheckKind::Truncation };
            fields.push(
                Fact::new(name, Some(span), format!("needs {len} bytes, {have} available"))
                    .kind(kind)
                    .status(st)
                    .diag(Diagnostic::new(check, Some(span), Trust::Partial, RECOVER_STOP, format!("needs {len} bytes, {have} available")).values(Some(DV::U64(len as u64)), Some(DV::U64(have as u64)))),
            );
            return Err(st);
        }
        let b = self.d[s..s + len].to_vec();
        fields.push(
            Fact::new(name, Some(Span::new(self.space, s as u64, len as u64)), bytes_preview(&b, 96))
                .kind(kind)
                .raw(&b)
                .bytes(&b)
                .enc("bytes (length-prefixed)"),
        );
        self.pos += len;
        Ok(b)
    }
}

/// Decodes a WriteBatch from `data` (the first bytes of a logical record of
/// `total_len` bytes). At most `max_ops` operations are materialized.
pub fn decode(data: &[u8], total_len: u64, space: Space, max_ops: usize) -> Batch {
    let clipped = (data.len() as u64) < total_len;
    let mut b = Batch { header: Vec::new(), seq: None, count: None, ops: Vec::new(), status: Status::Present, note: String::new(), diag: None };
    let whole = Some(Span::new(space, 0, data.len() as u64));
    if data.len() < HEADER {
        b.status = if clipped { Status::Limited } else { Status::Invalid };
        b.note = format!("record has {} bytes, a WriteBatch header needs 12", total_len);
        let check = if clipped { CheckKind::Limit } else { CheckKind::Length };
        b.diag = Some(Diagnostic::new(check, whole, Trust::Untrusted, RECOVER_STOP, b.note.clone()).values(Some(DV::U64(HEADER as u64)), Some(DV::U64(total_len))));
        return b;
    }
    let seq = fixed64(data, 0).unwrap_or(0);
    let count = fixed32(data, 8).unwrap_or(0);
    b.seq = Some(seq);
    b.count = Some(count);
    b.header.push(
        Fact::new("sequence", Some(Span::new(space, 0, 8)), seq.to_string()).kind(K::Sequence).dv(DV::U64(seq)).raw(&data[0..8]).enc("fixed64 LE"),
    );
    b.header.push(
        Fact::new("count", Some(Span::new(space, 8, 4)), count.to_string()).kind(K::Count).dv(DV::U32(count)).raw(&data[8..12]).enc("fixed32 LE"),
    );
    let mut c = Cur { d: data, pos: HEADER, space, clipped, hit_limit: false };
    let mut seq_cursor = seq;
    let mut counted = 0u32;
    let mut idx = 0u32;
    while c.pos < data.len() {
        if b.ops.len() >= max_ops {
            b.note = format!("listing limited to {max_ops} operations");
            break;
        }
        let start = c.pos;
        let tag = data[start];
        let mut fields = vec![Fact::new("tag", Some(Span::new(space, start as u64, 1)), format!("0x{tag:02x} {}", tag_name(tag)))
            .kind(K::Tag)
            .dv(DV::Enum { name: tag_name(tag), code: u64::from(tag) })
            .raw(&data[start..start + 1])
            .enc("u8 ValueType")];
        c.pos += 1;
        let mut op = Op { idx, offset: start, end: start, tag, cf: None, key: None, value: None, seq: None, status: Status::Present, facts: Vec::new() };
        let res = decode_op(&mut c, tag, &mut op, &mut fields);
        if let Err(st) = res {
            op.status = st;
        } else if c.hit_limit {
            op.status = Status::Limited;
        }
        if op.cf.is_none() && matches!(tag, 0x0 | 0x1 | 0x2 | 0x7 | 0xF | 0x11 | 0x16 | 0x18) {
            // write_batch.cc sets *column_family = 0 for tags without a CF id.
            fields.push(Fact::new("column family", None, "0 (default)").kind(K::Cf).dv(DV::U32(0)).status(Status::Implicit));
            op.cf = Some(0);
        }
        if consumes_seq(tag) && matches!(op.status, Status::Present | Status::Limited) {
            op.seq = Some(seq_cursor);
            // Not stored: batch sequence + index of sequence-consuming ops (seq_per_batch=false).
            fields.push(Fact::new("derived sequence", None, seq_cursor.to_string()).kind(K::DerivedSequence).dv(DV::U64(seq_cursor)).status(Status::Implicit));
            seq_cursor += 1;
            counted += 1;
        }
        op.end = c.pos;
        let failed = op.status != Status::Present;
        let op_diag = fields.iter().rev().find_map(|f| f.diag.as_deref().cloned());
        op.facts = fields;
        b.ops.push(op);
        idx += 1;
        if failed {
            b.status = b.ops.last().map(|o| o.status).unwrap_or(Status::Invalid);
            b.note = if op_limited(&b) { format!("bounded preview: first {} of {} bytes kept", data.len(), total_len) } else { "decoding stopped at an unreadable operation".into() };
            let check = if b.status == Status::Limited { CheckKind::Limit } else { op_diag.as_ref().map(|d| d.check).unwrap_or(CheckKind::Truncation) };
            let span = op_diag.as_ref().and_then(|d| d.span).or(Some(Span::new(space, start as u64, (c.pos - start) as u64)));
            let recovery = if b.status == Status::Limited { RECOVER_PREVIEW } else { RECOVER_STOP };
            b.diag = Some(Diagnostic::new(check, span, Trust::Partial, recovery, b.note.clone()));
            break;
        }
    }
    if b.status == Status::Present && !clipped && b.note.is_empty() && counted != count {
        b.status = Status::Invalid;
        b.note = format!("header count {count} but {counted} sequence-consuming operations decoded");
        b.diag = Some(Diagnostic::new(CheckKind::Count, Some(Span::new(space, 8, 4)), Trust::Untrusted, "all operations are listed; the header count is not trusted", b.note.clone()).values(Some(DV::U32(count)), Some(DV::U32(counted))));
    }
    if clipped && b.status == Status::Present {
        b.status = Status::Limited;
        b.note = format!("decoded the first {} of {} bytes (bounded preview)", data.len(), total_len);
        b.diag = Some(Diagnostic::new(CheckKind::Limit, whole, Trust::Partial, RECOVER_PREVIEW, b.note.clone()).values(Some(DV::U64(total_len)), Some(DV::U64(data.len() as u64))));
    }
    b
}

fn decode_op(c: &mut Cur, tag: u8, op: &mut Op, f: &mut Vec<Fact>) -> Result<(), Status> {
    let cf_tag = matches!(tag, 0x4 | 0x5 | 0x6 | 0x8 | 0xE | 0x10 | 0x17 | 0x19);
    if cf_tag {
        op.cf = Some(c.v32("column family id", K::Cf, f)?);
    }
    match tag {
        // Put / Merge / BlobIndex / RangeDeletion: key + value
        0x1 | 0x5 | 0x2 | 0x6 | 0x10 | 0x11 => {
            op.key = Some(c.slice("key", K::Key, f)?);
            let (vname, vk) = if matches!(tag, 0x10 | 0x11) { ("blob index", K::BlobIndex) } else { ("value", K::Value) };
            op.value = Some(c.slice(vname, vk, f)?);
        }
        0xE | 0xF => {
            op.key = Some(c.slice("begin key", K::BeginKey, f)?);
            op.value = Some(c.slice("end key", K::EndKey, f)?);
        }
        0x0 | 0x4 | 0x7 | 0x8 => {
            op.key = Some(c.slice("key", K::Key, f)?);
        }
        0x3 => {
            op.value = Some(c.slice("blob", K::Blob, f)?);
        }
        0xD | 0x9 | 0x12 | 0x13 => {
            f.push(Fact::new("payload", None, "none").kind(K::Payload).status(Status::NotPresent));
        }
        0xA => {
            op.value = Some(c.slice("xid", K::Xid, f)?);
        }
        0x15 => {
            op.key = Some(c.slice("commit timestamp", K::CommitTimestamp, f)?);
            op.value = Some(c.slice("xid", K::Xid, f)?);
        }
        0xB | 0xC => {
            op.value = Some(c.slice("xid", K::Xid, f)?);
        }
        0x16 | 0x17 => {
            op.key = Some(c.slice("key", K::Key, f)?);
            let v = c.slice("entity", K::Entity, f)?;
            decode_entity(&v, c.space, (c.pos - v.len()) as u64, f);
            op.value = Some(v);
        }
        0x18 | 0x19 => {
            op.key = Some(c.slice("key", K::Key, f)?);
            let v = c.slice("packed value", K::PackedValue, f)?;
            let base = (c.pos - v.len()) as u64;
            if v.len() >= 8 {
                let t = fixed64(&v, v.len() - 8).unwrap_or(0);
                f.push(Fact::new("value", Some(Span::new(c.space, base, (v.len() - 8) as u64)), bytes_preview(&v[..v.len() - 8], 96)).kind(K::Value).raw(&v[..v.len() - 8]).bytes(&v[..v.len() - 8]).enc("bytes"));
                // db/seqno_to_time_mapping.cc ParsePackedValueWithWriteTime: last 8 bytes.
                f.push(Fact::new("write unix time", Some(Span::new(c.space, base + v.len() as u64 - 8, 8)), t.to_string()).kind(K::WriteTime).dv(DV::U64(t)).raw(&v[v.len() - 8..]).enc("fixed64 LE"));
                op.value = Some(v[..v.len() - 8].to_vec());
            } else {
                let span = Some(Span::new(c.space, base, v.len() as u64));
                f.push(
                    Fact::new("write unix time", None, "packed value shorter than 8 bytes")
                        .kind(K::WriteTime)
                        .status(Status::Invalid)
                        .diag(Diagnostic::new(CheckKind::Length, span, Trust::Untrusted, RECOVER_STOP, "packed value shorter than 8 bytes").values(Some(DV::U64(8)), Some(DV::U64(v.len() as u64)))),
                );
                return Err(Status::Invalid);
            }
        }
        _ => {
            // write_batch.cc returns Corruption("unknown WriteBatch tag"); the rest cannot be framed.
            let note = format!("unknown WriteBatch tag 0x{tag:02x}");
            let span = Some(Span::new(c.space, (c.pos - 1) as u64, 1));
            f.push(
                Fact::new("payload", None, note.clone())
                    .kind(K::Payload)
                    .status(Status::Unknown)
                    .diag(Diagnostic::new(CheckKind::UnknownType, span, Trust::Untrusted, RECOVER_STOP, note).values(None, Some(DV::U8(tag)))),
            );
            return Err(Status::Unknown);
        }
    }
    Ok(())
}

/// Wide-column entity, version 1: version varint32 | num_columns varint32 |
/// (name len-prefixed, value_size varint32)* | values concatenated.
/// Source: db/wide/wide_column_serialization.cc (Deserialize).
fn decode_entity(v: &[u8], space: Space, base: u64, f: &mut Vec<Fact>) {
    let Some((ver, n1)) = varint32(v, 0) else { return };
    f.push(Fact::new("  entity version", Some(Span::new(space, base, n1 as u64)), ver.to_string()).dv(DV::VarInt(u64::from(ver))).enc("varint32").raw(&v[..n1]));
    if ver != 1 {
        let note = format!("version {ver} layout not decoded");
        f.push(Fact::new("  entity columns", None, note.clone()).status(Status::Unsupported).diag(Diagnostic::new(CheckKind::Unsupported, Some(Span::new(space, base, n1 as u64)), Trust::Partial, "the entity is shown as opaque bytes", note)));
        return;
    }
    let Some((num, n2)) = varint32(v, n1) else { return };
    f.push(Fact::new("  column count", Some(Span::new(space, base + n1 as u64, n2 as u64)), num.to_string()).kind(K::Count).dv(DV::VarInt(u64::from(num))).enc("varint32"));
    let mut pos = n1 + n2;
    let mut cols = Vec::new();
    for _ in 0..num.min(MAX_ENTITY_COLUMNS) {
        let Some((nl, a)) = varint32(v, pos) else { return };
        let ns = pos + a;
        let Some(name) = v.get(ns..ns + nl as usize) else { return };
        let Some((vs, b)) = varint32(v, ns + nl as usize) else { return };
        cols.push((name.to_vec(), ns, vs));
        pos = ns + nl as usize + b;
    }
    for (name, ns, vs) in cols {
        let Some(val) = v.get(pos..pos + vs as usize) else {
            let span = Some(Span::new(space, base + pos as u64, (v.len() - pos.min(v.len())) as u64));
            f.push(Fact::new("  column value", None, "value runs past entity").kind(K::EntityColumn).status(Status::Truncated).diag(Diagnostic::new(CheckKind::Truncation, span, Trust::Partial, "remaining columns are not listed", "value runs past entity")));
            return;
        };
        f.push(
            Fact::new(format!("  column {}", bytes_preview(&name, 32)), Some(Span::new(space, base + pos as u64, vs as u64)), bytes_preview(val, 64))
                .raw(val)
                .bytes(val)
                .kind(K::EntityColumn)
                .enc("bytes")
                .aux(Span::new(space, base + ns as u64, name.len() as u64)),
        );
        pos += vs as usize;
    }
}

fn op_limited(b: &Batch) -> bool {
    b.ops.last().map(|o| o.status == Status::Limited).unwrap_or(false)
}
