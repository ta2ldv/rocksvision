//! SST footer and block checksums, RocksDB 10.4.2.
//! Source: table/format.h, table/format.cc (Footer::DecodeFrom, ComputeBuiltinChecksum,
//! ComputeBuiltinChecksumWithLastByte, ModifyChecksumForLastByte, ChecksumModifierForContext),
//! table/block_based/reader_common.cc (VerifyBlockChecksum).
//!
//! Footer layouts:
//!   format_version 0 (legacy magic): metaindex handle | index handle | pad to 40 | magic(8)  = 48 B
//!   format_version 1..5: checksum type(1) | metaindex handle | index handle | pad to 40 | version(4) | magic(8) = 53 B
//!   format_version >= 6: checksum type(1) | ext magic 3e 00 7a 00 | footer checksum(4) |
//!                        base_context_checksum(4) | metaindex size(4) | 24 B reserved | version(4) | magic(8) = 53 B
//!   (fv>=6 has no index handle in the footer; it is "rocksdb.index" in the metaindex.)

use crate::model::{ChecksumRole, CheckKind, DecodedValue as DV, Diagnostic, Fact, FieldKind as K, HandleTarget, Span, Status, Trust};
use crate::util::{crc32c, crc_mask, fixed32, fixed64, varint64};

pub const BLOCK_TRAILER: u64 = 5;
pub const LEGACY_FOOTER: u64 = 48;
pub const NEW_FOOTER: u64 = 53;
pub const BLOCK_BASED_MAGIC: u64 = 0x88e2_41b7_85f4_cff7;
pub const LEGACY_BLOCK_BASED_MAGIC: u64 = 0xdb47_7524_8b80_fb57;
pub const PLAIN_MAGIC: u64 = 0x8242_2296_63bf_9564;
pub const LEGACY_PLAIN_MAGIC: u64 = 0x4f34_18eb_7a8f_13b8;
pub const CUCKOO_MAGIC: u64 = 0x9267_89d0_c5f1_7873;
pub const EXT_MAGIC: [u8; 4] = [0x3e, 0x00, 0x7a, 0x00];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handle {
    pub offset: u64,
    pub size: u64,
}

impl Handle {
    /// BlockHandle::DecodeFrom: varint64 offset, varint64 size.
    pub fn decode(b: &[u8], pos: usize) -> Option<(Handle, usize)> {
        let (o, n1) = varint64(b, pos)?;
        let (s, n2) = varint64(b, pos + n1)?;
        Some((Handle { offset: o, size: s }, n1 + n2))
    }
}

pub fn checksum_name(t: u8) -> &'static str {
    match t {
        0 => "kNoChecksum",
        1 => "kCRC32c",
        2 => "kxxHash",
        3 => "kxxHash64",
        4 => "kXXH3",
        _ => "unknown",
    }
}

/// Verification kind of a block-based table checksum type.
pub fn checksum_check(t: u8) -> CheckKind {
    match t {
        2 => CheckKind::XxHash,
        3 => CheckKind::XxHash64,
        4 => CheckKind::Xxh3,
        _ => CheckKind::Crc32c,
    }
}

fn modify_for_last_byte(v: u32, last: u8) -> u32 {
    v ^ u32::from(last).wrapping_mul(0x6b90_83d9)
}

/// ComputeBuiltinChecksum(type, data). Returns None for unknown types.
pub(super) fn builtin_checksum(t: u8, data: &[u8]) -> Option<u32> {
    Some(match t {
        0 => 0,
        1 => crc_mask(crc32c(data)),
        2 => xxhash_rust::xxh32::xxh32(data, 0),
        3 => xxhash_rust::xxh64::xxh64(data, 0) as u32,
        4 => {
            if data.is_empty() {
                0
            } else {
                let v = xxhash_rust::xxh3::xxh3_64(&data[..data.len() - 1]) as u32;
                modify_for_last_byte(v, data[data.len() - 1])
            }
        }
        _ => return None,
    })
}

/// ChecksumModifierForContext (format.h): 0 when base == 0.
pub(super) fn context_modifier(base: u32, offset: u64) -> u32 {
    if base == 0 {
        0
    } else {
        base ^ ((offset as u32).wrapping_add((offset >> 32) as u32))
    }
}

#[derive(Clone, Debug)]
pub struct Footer {
    pub offset: u64,
    pub len: u64,
    pub magic: u64,
    pub legacy: bool,
    pub format_version: u32,
    pub checksum_type: u8,
    pub metaindex: Handle,
    /// None for format_version >= 6 (found via metaindex "rocksdb.index").
    pub index: Option<Handle>,
    pub base_context: u32,
    pub footer_checksum: Option<(u32, u32)>,
    /// Encoded footer elements in file order (inferred ones without a span).
    pub fields: Vec<Fact>,
}

#[derive(Debug)]
pub enum FooterError {
    TooShort,
    OtherTable(&'static str),
    BadMagic(u64),
    Corrupt(String),
}

impl FooterError {
    /// The footer problem as data, over the last bytes of a file of `file_len` bytes.
    pub fn diagnostic(&self, file_len: u64) -> Diagnostic {
        let tail = |n: u64| Some(Span::file(file_len.saturating_sub(n), n.min(file_len)));
        let rec = "the file is not decoded as a block-based table (no metaindex, index or blocks)";
        match self {
            FooterError::TooShort => Diagnostic::new(CheckKind::Length, tail(file_len), Trust::Untrusted, rec, self.to_string()).values(Some(DV::U64(LEGACY_FOOTER)), Some(DV::U64(file_len))),
            FooterError::OtherTable(_) => Diagnostic::new(CheckKind::Unsupported, tail(8), Trust::Partial, rec, self.to_string()),
            FooterError::BadMagic(m) => Diagnostic::new(CheckKind::Magic, tail(8), Trust::Untrusted, rec, self.to_string()).values(Some(DV::U64(BLOCK_BASED_MAGIC)), Some(DV::U64(*m))),
            FooterError::Corrupt(_) => Diagnostic::new(CheckKind::Structure, tail(NEW_FOOTER.min(file_len)), Trust::Untrusted, rec, self.to_string()),
        }
    }
}

impl std::fmt::Display for FooterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FooterError::TooShort => write!(f, "file is shorter than an SST footer"),
            FooterError::OtherTable(t) => write!(f, "{t} table format is not supported by this inspector (block-based only)"),
            FooterError::BadMagic(m) => write!(f, "bad table magic number 0x{m:016x}"),
            FooterError::Corrupt(s) => write!(f, "{s}"),
        }
    }
}

/// Decodes the footer from the last bytes of the file (`tail` ends at `file_len`).
pub fn decode(tail: &[u8], file_len: u64) -> Result<Footer, FooterError> {
    if (tail.len() as u64) < LEGACY_FOOTER {
        return Err(FooterError::TooShort);
    }
    let n = tail.len();
    let magic = fixed64(tail, n - 8).unwrap_or(0);
    let mut fields = Vec::new();
    let span = |rel: usize, len: u64| Some(Span::file(file_len - (n - rel) as u64, len));
    match magic {
        PLAIN_MAGIC | LEGACY_PLAIN_MAGIC => return Err(FooterError::OtherTable("PlainTable")),
        CUCKOO_MAGIC => return Err(FooterError::OtherTable("CuckooTable")),
        BLOCK_BASED_MAGIC | LEGACY_BLOCK_BASED_MAGIC => {}
        m => return Err(FooterError::BadMagic(m)),
    }
    let legacy = magic == LEGACY_BLOCK_BASED_MAGIC;
    // kBlockBasedTableMagicNumber; the legacy value implies format_version 0 and CRC32c.
    fields.push(Fact::new("magic number", span(n - 8, 8), format!("0x{magic:016x}{}", if legacy { " (legacy block-based)" } else { " (block-based)" })).kind(K::Magic).dv(DV::U64(magic)).raw(&tail[n - 8..]).enc("fixed64 LE"));
    if legacy {
        let s = n - LEGACY_FOOTER as usize;
        let (mi, a) = Handle::decode(tail, s).ok_or_else(|| FooterError::Corrupt("bad metaindex handle".into()))?;
        let (ix, b) = Handle::decode(tail, s + a).ok_or_else(|| FooterError::Corrupt("bad index handle".into()))?;
        fields.insert(0, Fact::new("format version", None, "0 (inferred from legacy magic)").kind(K::FormatVersion).dv(DV::U32(0)).status(Status::Implicit));
        fields.insert(1, Fact::new("checksum type", None, "kCRC32c (implied)").kind(K::ChecksumType).dv(DV::Enum { name: checksum_name(1), code: 1 }).status(Status::Implicit));
        fields.insert(2, Fact::new("metaindex handle", span(s, a as u64), format!("offset {} size {}", mi.offset, mi.size)).kind(K::BlockHandle(HandleTarget::Metaindex)).dv(DV::Handle { offset: mi.offset, size: mi.size }).enc("varint64 ×2").raw(&tail[s..s + a]));
        fields.insert(3, Fact::new("index handle", span(s + a, b as u64), format!("offset {} size {}", ix.offset, ix.size)).kind(K::BlockHandle(HandleTarget::Index)).dv(DV::Handle { offset: ix.offset, size: ix.size }).enc("varint64 ×2").raw(&tail[s + a..s + a + b]));
        fields.insert(4, Fact::new("padding", span(s + a + b, (40 - a - b) as u64), "ignored").enc("zero bytes"));
        return Ok(Footer { offset: file_len - LEGACY_FOOTER, len: LEGACY_FOOTER, magic, legacy, format_version: 0, checksum_type: 1, metaindex: mi, index: Some(ix), base_context: 0, footer_checksum: None, fields });
    }
    if (n as u64) < NEW_FOOTER {
        return Err(FooterError::TooShort);
    }
    let s = n - NEW_FOOTER as usize;
    let footer_off = file_len - NEW_FOOTER;
    let fv = fixed32(tail, n - 12).unwrap_or(0);
    let ct = tail[s];
    let mut ctf = Fact::new("checksum type", span(s, 1), format!("{ct} {}", checksum_name(ct))).kind(K::ChecksumType).dv(DV::Enum { name: checksum_name(ct), code: u64::from(ct) }).raw(&tail[s..s + 1]).enc("u8").status(if ct <= 4 { Status::Present } else { Status::Invalid });
    if ct > 4 {
        ctf = ctf.diag(Diagnostic::new(CheckKind::UnknownType, span(s, 1), Trust::Untrusted, "block checksums cannot be verified", format!("unknown checksum type {ct}")).values(None, Some(DV::U8(ct))));
    }
    let mut f = vec![ctf];
    if fv > 7 {
        return Err(FooterError::Corrupt(format!("unsupported format_version {fv} (10.4.2 supports 0..7)")));
    }
    let mut out = Footer { offset: footer_off, len: NEW_FOOTER, magic, legacy, format_version: fv, checksum_type: ct, metaindex: Handle { offset: 0, size: 0 }, index: None, base_context: 0, footer_checksum: None, fields: Vec::new() };
    if fv >= 6 {
        let p = s + 1;
        let ext_ok = tail[p..p + 4] == EXT_MAGIC;
        let mut em = Fact::new("extended magic", span(p, 4), if ext_ok { "3e 00 7a 00 (ok)" } else { "mismatch" }).kind(K::ExtendedMagic).raw(&tail[p..p + 4]).enc("4 bytes").status(if ext_ok { Status::Present } else { Status::Invalid });
        if !ext_ok {
            em = em.diag(Diagnostic::new(CheckKind::Magic, span(p, 4), Trust::Untrusted, "the footer is rejected (bad extended magic number)", "bad extended magic number").values(Some(DV::U32(u32::from_le_bytes(EXT_MAGIC))), fixed32(tail, p).map(DV::U32)));
        }
        f.push(em);
        let stored = fixed32(tail, p + 4).unwrap_or(0);
        let base = fixed32(tail, p + 8).unwrap_or(0);
        let mis = fixed32(tail, p + 12).unwrap_or(0);
        let mut copy = tail[s..].to_vec();
        copy[5..9].copy_from_slice(&[0, 0, 0, 0]);
        let computed = builtin_checksum(ct, &copy).map(|c| c.wrapping_add(context_modifier(base, footer_off)));
        let ok = ct == 0 || computed == Some(stored);
        let mut fc = Fact::new("footer checksum", span(p + 4, 4), format!("0x{stored:08x}")).kind(K::Checksum(ChecksumRole::Stored)).dv(DV::U32(stored)).raw(&tail[p + 4..p + 8]).enc("fixed32 LE").status(if ok { Status::Present } else { Status::Invalid });
        if !ok {
            let d = Diagnostic::new(checksum_check(ct), Some(Span::file(footer_off, NEW_FOOTER)), Trust::Untrusted, "the footer fields are shown but not trusted", "footer checksum MISMATCH").values(computed.map(DV::U32), Some(DV::U32(stored)));
            fc = fc.diag(d);
        }
        f.push(fc);
        out.footer_checksum = computed.map(|c| (stored, c));
        // format.cc DecodeFrom: ChecksumModifierForContext(base, 0) == 0 (i.e. base == 0) is Corruption.
        f.push(
            Fact::new("base context checksum", span(p + 8, 4), format!("0x{base:08x}{}", if base == 0 { " (invalid: must be non-zero)" } else { "" }))
                .kind(K::BaseContext)
                .dv(DV::U32(base))
                .raw(&tail[p + 8..p + 12])
                .enc("fixed32 LE")
                .status(if base == 0 { Status::Invalid } else { Status::Present })
                .diag_if(base == 0, || Diagnostic::new(CheckKind::Structure, span(p + 8, 4), Trust::Untrusted, "RocksDB rejects the file (format.cc DecodeFrom: Corruption)", "base context checksum must be non-zero for format_version >= 6")),
        );
        let mi = Handle { offset: footer_off.saturating_sub(BLOCK_TRAILER).saturating_sub(u64::from(mis)), size: u64::from(mis) };
        f.push(Fact::new("metaindex size", span(p + 12, 4), mis.to_string()).kind(K::MetaindexSize).dv(DV::U32(mis)).raw(&tail[p + 12..p + 16]).enc("fixed32 LE"));
        // Derived: footer offset − 5 − metaindex size (not stored as a handle).
        f.push(Fact::new("metaindex handle (derived)", None, format!("offset {} size {}", mi.offset, mi.size)).kind(K::BlockHandle(HandleTarget::Metaindex)).dv(DV::Handle { offset: mi.offset, size: mi.size }).status(Status::Implicit));
        let res = &tail[p + 16..p + 40];
        let reserved_ok = res[16..].iter().all(|&b| b == 0);
        f.push(Fact::new("reserved", span(p + 16, 24), if reserved_ok { "zero" } else { "non-zero checked bytes" }).raw(res).enc("16 unchecked + 8 checked").status(if reserved_ok { Status::Present } else { Status::Unsupported }));
        // FormatVersionUsesIndexHandleInFooter is false for format_version >= 6.
        f.push(Fact::new("index handle", None, "in metaindex as \"rocksdb.index\"").kind(K::BlockHandle(HandleTarget::Index)).status(Status::NotPresent));
        out.metaindex = mi;
        out.base_context = base;
        if !ext_ok {
            return Err(FooterError::Corrupt("bad extended magic number".into()));
        }
    } else {
        let p = s + 1;
        let (mi, a) = Handle::decode(tail, p).ok_or_else(|| FooterError::Corrupt("bad metaindex handle".into()))?;
        let (ix, b) = Handle::decode(tail, p + a).ok_or_else(|| FooterError::Corrupt("bad index handle".into()))?;
        f.push(Fact::new("metaindex handle", span(p, a as u64), format!("offset {} size {}", mi.offset, mi.size)).kind(K::BlockHandle(HandleTarget::Metaindex)).dv(DV::Handle { offset: mi.offset, size: mi.size }).raw(&tail[p..p + a]).enc("varint64 offset + varint64 size"));
        f.push(Fact::new("index handle", span(p + a, b as u64), format!("offset {} size {}", ix.offset, ix.size)).kind(K::BlockHandle(HandleTarget::Index)).dv(DV::Handle { offset: ix.offset, size: ix.size }).raw(&tail[p + a..p + a + b]).enc("varint64 offset + varint64 size"));
        f.push(Fact::new("padding", span(p + a + b, (40 - a - b) as u64), "unchecked").enc("zero bytes"));
        f.push(Fact::new("footer checksum", None, "not used before format_version 6").kind(K::Checksum(ChecksumRole::Stored)).status(Status::NotPresent));
        out.metaindex = mi;
        out.index = Some(ix);
    }
    f.push(Fact::new("format version", span(n - 12, 4), fv.to_string()).kind(K::FormatVersion).dv(DV::U32(fv)).raw(&tail[n - 12..n - 8]).enc("fixed32 LE"));
    f.extend(fields);
    out.fields = f;
    Ok(out)
}
