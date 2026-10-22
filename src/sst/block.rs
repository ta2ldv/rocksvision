//! Block read (trailer, checksum, decompression) and block entry iteration, RocksDB 10.4.2.
//! Sources: table/block_based/reader_common.cc (VerifyBlockChecksum),
//! table/block_based/block.cc (DecodeEntry/DecodeKeyV4, restart array, NumRestarts),
//! table/block_based/data_block_footer.cc (MSB of num_restarts = hash index flag),
//! table/block_based/data_block_hash_index.h (HASH_IDX = buckets(u8) + NUM_BUCK(u16)),
//! table/format.cc (IndexValue::DecodeFrom: delta-encoded handles).

use super::footer::{builtin_checksum, checksum_check, context_modifier, Footer, Handle, BLOCK_TRAILER};
use crate::format::codec::{self as compress, DecompError};
use crate::io::{read_vec, RangeRead};
use crate::limits::MAX_RESTARTS_READ;
use crate::model::{CheckKind, DecodedValue as DV, Diagnostic, Space, Span, Status, Trust};
use crate::util::{fixed16, fixed32, varint32, varsigned64};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct Block {
    pub handle: Handle,
    pub ctype: u8,
    pub stored_checksum: u32,
    pub computed_checksum: Option<u32>,
    pub modifier: u32,
    pub checksum_status: Status,
    /// Contents: decompressed bytes, or the raw block bytes if uncompressed.
    pub data: Arc<Vec<u8>>,
    pub compressed: bool,
    pub size_prefix: Option<(u64, usize)>,
    /// Problem reading/decompressing (status + message). `data` may be empty then.
    pub error: Option<(Status, String)>,
    /// Problems as data: the `error` above and a checksum mismatch.
    pub diags: Vec<Diagnostic>,
    /// Index into `diags` of the diagnostic behind `error` (set by `fail`).
    pub(super) error_diag: Option<usize>,
}

const RECOVER_BLOCK: &str = "the block contents are not decoded";
const RECOVER_ENTRIES: &str = "entries before this point are listed; decoding of the block stops here";

impl Block {
    pub fn ok(&self) -> bool {
        self.error.is_none()
    }

    /// Records a read/decompression problem (status + message) and its diagnostic.
    pub(super) fn fail(&mut self, st: Status, d: Diagnostic) {
        self.error = Some((st, d.note.clone()));
        self.error_diag = Some(self.diags.len());
        self.diags.push(d);
    }

    /// The diagnostic of the read/decompression problem in `error` (typed, not matched by text).
    pub fn error_diagnostic(&self) -> Option<&Diagnostic> {
        self.error_diag.and_then(|i| self.diags.get(i))
    }

    /// Span of contents bytes `[off, off+len)`: block-local (derived space) when the block was
    /// decompressed, file offsets otherwise.
    pub fn span(&self, off: usize, len: usize) -> Span {
        if self.compressed {
            Span::new(Space::Block(self.handle.offset), off as u64, len as u64)
        } else {
            Span::file(self.handle.offset + off as u64, len as u64)
        }
    }
}

/// Reads block `h` plus its 5-byte trailer and verifies/decompresses it.
pub(super) fn read_block(r: &dyn RangeRead, f: &Footer, h: Handle, cfv: u32, dict: Option<&[u8]>) -> Block {
    let mut b = Block {
        handle: h,
        ctype: 0,
        stored_checksum: 0,
        computed_checksum: None,
        modifier: 0,
        checksum_status: Status::Unknown,
        data: Arc::new(Vec::new()),
        compressed: false,
        size_prefix: None,
        error: None,
        diags: Vec::new(),
        error_diag: None,
    };
    let whole = Some(Span::file(h.offset, h.size.saturating_add(BLOCK_TRAILER)));
    let end = h.offset.checked_add(h.size).and_then(|x| x.checked_add(BLOCK_TRAILER));
    match end {
        Some(e) if e <= r.len() => {}
        _ => {
            b.fail(Status::Truncated, Diagnostic::new(CheckKind::Truncation, whole, Trust::Untrusted, RECOVER_BLOCK, format!("block handle offset {} size {} runs past file end {}", h.offset, h.size, r.len())).values(end.map(DV::U64), Some(DV::U64(r.len()))));
            return b;
        }
    }
    if h.size > compress::MAX_DECOMPRESSED {
        b.fail(Status::Limited, Diagnostic::new(CheckKind::Limit, whole, Trust::Partial, RECOVER_BLOCK, format!("block of {} bytes exceeds the {} byte cap", h.size, compress::MAX_DECOMPRESSED)));
        return b;
    }
    let buf = match read_vec(r, h.offset, h.size + BLOCK_TRAILER) {
        Ok(x) if x.len() as u64 == h.size + BLOCK_TRAILER => x,
        Ok(x) => {
            b.fail(Status::Truncated, Diagnostic::new(CheckKind::Truncation, whole, Trust::Untrusted, RECOVER_BLOCK, "short read").values(Some(DV::U64(h.size + BLOCK_TRAILER)), Some(DV::U64(x.len() as u64))));
            return b;
        }
        Err(e) => {
            b.fail(Status::Invalid, Diagnostic::new(CheckKind::Truncation, whole, Trust::Untrusted, RECOVER_BLOCK, format!("read error: {e}")));
            return b;
        }
    };
    let n = h.size as usize;
    b.ctype = buf[n];
    b.stored_checksum = fixed32(&buf, n + 1).unwrap_or(0);
    b.modifier = context_modifier(f.base_context, h.offset);
    b.computed_checksum = builtin_checksum(f.checksum_type, &buf[..=n]);
    b.checksum_status = match (f.checksum_type, b.computed_checksum) {
        (0, _) => Status::NotPresent,
        (_, Some(c)) if b.stored_checksum.wrapping_sub(b.modifier) == c => Status::Present,
        (_, Some(_)) => Status::Invalid,
        (_, None) => Status::Unknown,
    };
    if b.checksum_status == Status::Invalid {
        b.diags.push(
            Diagnostic::new(checksum_check(f.checksum_type), whole, Trust::Untrusted, "entries are still decoded for inspection but are not trusted", "Block checksum MISMATCH")
                .values(b.computed_checksum.map(|c| DV::U32(c.wrapping_add(b.modifier))), Some(DV::U32(b.stored_checksum))),
        );
    }
    let mut payload = buf;
    payload.truncate(n);
    if b.ctype == 0 {
        b.data = Arc::new(payload);
        return b;
    }
    b.compressed = true;
    match compress::decompress(b.ctype, &payload, cfv, dict) {
        Ok(d) => {
            b.size_prefix = d.size_prefix;
            b.data = Arc::new(d.data);
        }
        Err(e) => {
            let (st, check, trust) = match e {
                DecompError::Unsupported(_) => (Status::Unsupported, CheckKind::Unsupported, Trust::Partial),
                DecompError::Limit(_) => (Status::Limited, CheckKind::Limit, Trust::Partial),
                DecompError::Corrupt(_) => (Status::Invalid, CheckKind::Decompression, Trust::Untrusted),
            };
            b.fail(st, Diagnostic::new(check, Some(Span::file(h.offset, h.size)), trust, RECOVER_BLOCK, e.to_string()));
        }
    }
    b
}

/// Restart array / footer layout of a block.
#[derive(Clone, Debug)]
pub struct Layout {
    pub num_restarts: u32,
    pub footer_raw: u32,
    pub hash_index: bool,
    pub restarts_off: usize,
    /// (start offset of buckets, num_buckets)
    pub hash: Option<(usize, u16)>,
    pub entries_end: usize,
    pub footer_off: usize,
}

/// Why a block's restart array / footer cannot be laid out.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutError {
    pub check: CheckKind,
    pub msg: String,
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}

fn lerr(check: CheckKind, msg: impl Into<String>) -> LayoutError {
    LayoutError { check, msg: msg.into() }
}

pub(super) fn layout(d: &[u8]) -> Result<Layout, LayoutError> {
    if d.len() < 4 {
        return Err(lerr(CheckKind::Length, "block shorter than its 4-byte footer"));
    }
    let footer_off = d.len() - 4;
    let raw = fixed32(d, footer_off).unwrap_or(0);
    // block.cc NumRestarts(): only blocks <= 64 KiB may use the MSB as hash-index flag.
    let (hash_flag, nr) = if d.len() > (1 << 16) { (false, raw) } else { (raw & 0x8000_0000 != 0, raw & 0x7fff_ffff) };
    let mut end = footer_off;
    let mut hash = None;
    if hash_flag {
        if end < 2 {
            return Err(lerr(CheckKind::Structure, "hash index flag set but no NUM_BUCK"));
        }
        let nb = fixed16(d, end - 2).unwrap_or(0);
        let start = (end - 2).checked_sub(nb as usize).ok_or_else(|| lerr(CheckKind::Structure, "hash index larger than block"))?;
        hash = Some((start, nb));
        end = start;
    }
    let ra = (nr as usize).checked_mul(4).ok_or_else(|| lerr(CheckKind::RestartCount, "restart count overflow"))?;
    let restarts_off = end.checked_sub(ra).ok_or_else(|| lerr(CheckKind::RestartCount, format!("{nr} restarts do not fit in a {} byte block", d.len())))?;
    Ok(Layout { num_restarts: nr, footer_raw: raw, hash_index: hash_flag, restarts_off, hash, entries_end: restarts_off, footer_off })
}

#[derive(Clone, Copy, Debug, Default)]
pub struct IndexFmt {
    /// Values are IndexValue (handle [+ first key]) without explicit value length.
    pub value_delta: bool,
    pub first_key: bool,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub idx: u32,
    pub off: usize,
    pub shared: u32,
    pub non_shared: u32,
    /// None when the format omits the value length (delta-encoded index values).
    pub value_len_field: Option<u32>,
    /// Byte length of the shared/non_shared/value_len varints.
    pub hdr_len: usize,
    /// Byte lengths of the shared, non_shared and value_length varints (0 = not stored).
    pub hdr_lens: [usize; 3],
    pub key_off: usize,
    pub key: Vec<u8>,
    pub value_off: usize,
    pub value_len: usize,
    pub restart: bool,
    /// Decoded index value (handle, delta?, first key) for index-format blocks.
    pub handle: Option<(Handle, bool, Option<Vec<u8>>)>,
    pub status: Status,
    pub note: String,
    /// Failed check of an entry that was still decoded (e.g. a restart point with shared ≠ 0).
    pub issue: Option<CheckKind>,
}

const RESTART_SHARED: &str = "restart point with non-zero shared length";

impl Entry {
    /// The entry's problem as data (span = the entry's header varints, in the block's space).
    pub fn diagnostic(&self, b: &Block) -> Option<Diagnostic> {
        let check = self.issue?;
        let shared = self.hdr_lens[0].max(1);
        Some(
            Diagnostic::new(check, Some(b.span(self.off, shared)), Trust::Partial, "the entry is listed with the key it encodes; decoding continues with the next entry", self.note.clone())
                .values(Some(DV::U32(0)), Some(DV::U32(self.shared))),
        )
    }
}

pub use crate::limits::MAX_BLOCK_ENTRIES as MAX_ENTRIES;

/// Why entry iteration stopped early: status, failed check, block-local position and message.
#[derive(Clone, Debug, PartialEq)]
pub struct EntryError {
    pub status: Status,
    pub check: CheckKind,
    pub at: usize,
    pub msg: String,
}

impl EntryError {
    /// (status, message) pair of the former API.
    pub fn pair(&self) -> (Status, String) {
        (self.status, self.msg.clone())
    }

    /// The problem as data, with a span in the block's offset space.
    pub fn diagnostic(&self, b: &Block) -> Diagnostic {
        let (trust, rec) = if self.status == Status::Limited { (Trust::Partial, "the first entries up to the cap are listed") } else { (Trust::Partial, RECOVER_ENTRIES) };
        Diagnostic::new(self.check, Some(b.span(self.at.min(b.data.len()), 1)), trust, rec, self.msg.clone())
    }
}

fn eerr(status: Status, check: CheckKind, at: usize, msg: String) -> Option<EntryError> {
    Some(EntryError { status, check, at, msg })
}

/// Iterates all entries of a block (bounded by MAX_ENTRIES). `index` selects index-value decoding.
pub(super) fn entries(d: &[u8], lay: &Layout, index: Option<IndexFmt>) -> (Vec<Entry>, Option<EntryError>) {
    let restarts: Vec<usize> = (0..lay.num_restarts.min(MAX_RESTARTS_READ) as usize).filter_map(|i| fixed32(d, lay.restarts_off + 4 * i).map(|x| x as usize)).collect();
    let mut out = Vec::new();
    let mut pos = 0usize;
    let mut prev: Vec<u8> = Vec::new();
    let mut prev_handle: Option<Handle> = None;
    let limit = lay.entries_end;
    let mut ri = 0usize;
    while pos < limit {
        if out.len() >= MAX_ENTRIES {
            return (out, eerr(Status::Limited, CheckKind::Limit, pos, format!("listing capped at {MAX_ENTRIES} entries")));
        }
        let start = pos;
        let delta = index.map(|i| i.value_delta).unwrap_or(false);
        let bad_hdr = |pos: usize| eerr(Status::Truncated, CheckKind::Varint, pos, format!("bad entry header at 0x{pos:x}"));
        let (shared, non_shared, vlen, hl, lens) = if delta {
            // DecodeKeyV4: shared, non_shared only.
            let Some((s, a)) = varint32(d, pos) else { return (out, bad_hdr(pos)) };
            let Some((n, b)) = varint32(d, pos + a) else { return (out, bad_hdr(pos)) };
            (s, n, None, a + b, [a, b, 0])
        } else {
            let Some((s, a)) = varint32(d, pos) else { return (out, bad_hdr(pos)) };
            let Some((n, b)) = varint32(d, pos + a) else { return (out, bad_hdr(pos)) };
            let Some((v, c)) = varint32(d, pos + a + b) else { return (out, bad_hdr(pos)) };
            (s, n, Some(v), a + b + c, [a, b, c])
        };
        let key_off = start + hl;
        let key_end = key_off + non_shared as usize;
        if shared as usize > prev.len() || key_end > limit {
            return (out, eerr(Status::Invalid, CheckKind::Structure, start, format!("entry at 0x{start:x}: shared {shared} / non_shared {non_shared} inconsistent with block")));
        }
        let mut key = prev[..shared as usize].to_vec();
        key.extend_from_slice(&d[key_off..key_end]);
        while ri < restarts.len() && restarts[ri] < start {
            ri += 1;
        }
        let restart = ri < restarts.len() && restarts[ri] == start;
        let mut e = Entry {
            idx: out.len() as u32,
            off: start,
            shared,
            non_shared,
            value_len_field: vlen,
            hdr_len: hl,
            hdr_lens: lens,
            key_off,
            key: key.clone(),
            value_off: key_end,
            value_len: 0,
            restart,
            handle: None,
            status: Status::Present,
            note: String::new(),
            issue: None,
        };
        if restart && shared != 0 {
            // block_builder.cc: an entry at a restart point stores its full key (shared = 0).
            e.status = Status::Invalid;
            e.note = RESTART_SHARED.into();
            e.issue = Some(CheckKind::Structure);
        }
        match (vlen, index) {
            (Some(v), idx) => {
                if key_end + v as usize > limit {
                    return (out, eerr(Status::Invalid, CheckKind::Length, start, format!("entry at 0x{start:x}: value length {v} past entries end")));
                }
                e.value_len = v as usize;
                if let Some(ix) = idx {
                    if let Some((h, n)) = Handle::decode(d, key_end) {
                        let fk = if ix.first_key { first_key(d, key_end + n) } else { None };
                        e.handle = Some((h, false, fk));
                        prev_handle = Some(h);
                    }
                }
            }
            (None, Some(ix)) => {
                // IndexValue::DecodeFrom: delta-encoded size when shared != 0 (IndexBlockIter::DecodeCurrentValue).
                let mut p = key_end;
                let h = if shared != 0 && prev_handle.is_some() {
                    let ph = prev_handle.unwrap_or(Handle { offset: 0, size: 0 });
                    let Some((dl, n)) = varsigned64(d, p) else { return (out, eerr(Status::Truncated, CheckKind::Varint, p, format!("bad delta index value at 0x{p:x}"))) };
                    p += n;
                    // Checked: crafted deltas must not wrap (offset + size + 5, size + delta >= 0).
                    let off = ph.offset.checked_add(ph.size).and_then(|x| x.checked_add(BLOCK_TRAILER));
                    let size = i64::try_from(ph.size).ok().and_then(|s| s.checked_add(dl)).filter(|s| *s >= 0).map(|s| s as u64);
                    match (off, size) {
                        (Some(o), Some(sz)) => (Handle { offset: o, size: sz }, true),
                        _ => return (out, eerr(Status::Invalid, CheckKind::Handle, p, format!("delta-encoded index value at 0x{p:x} overflows"))),
                    }
                } else {
                    let Some((h, n)) = Handle::decode(d, p) else { return (out, eerr(Status::Truncated, CheckKind::Handle, p, format!("bad index handle at 0x{p:x}"))) };
                    p += n;
                    (h, false)
                };
                let fk = if ix.first_key {
                    let r = first_key(d, p);
                    if let Some(k) = &r {
                        let (_, n) = varint32(d, p).unwrap_or((0, 0));
                        p += n + k.len();
                    }
                    r
                } else {
                    None
                };
                e.value_len = p - key_end;
                e.handle = Some((h.0, h.1, fk));
                prev_handle = Some(h.0);
            }
            (None, None) => unreachable!(),
        }
        let next = e.value_off + e.value_len;
        if next <= start {
            return (out, eerr(Status::Invalid, CheckKind::Structure, start, format!("entry at 0x{start:x} does not advance")));
        }
        pos = next;
        prev = key;
        out.push(e);
    }
    (out, None)
}

fn first_key(d: &[u8], p: usize) -> Option<Vec<u8>> {
    let (l, n) = varint32(d, p)?;
    d.get(p + n..p + n + l as usize).map(|x| x.to_vec())
}

/// Bounded LRU cache of decoded blocks (by handle offset).
pub(super) struct BlockCache {
    items: Vec<(u64, Arc<Block>)>,
    max_items: usize,
    max_bytes: usize,
}

impl BlockCache {
    pub(super) fn new(max_items: usize, max_bytes: usize) -> Self {
        BlockCache { items: Vec::new(), max_items, max_bytes }
    }
    pub(super) fn get(&mut self, off: u64) -> Option<Arc<Block>> {
        let i = self.items.iter().position(|(o, _)| *o == off)?;
        let it = self.items.remove(i);
        let b = it.1.clone();
        self.items.push(it);
        Some(b)
    }
    pub(super) fn put(&mut self, off: u64, b: Arc<Block>) {
        self.items.retain(|(o, _)| *o != off);
        self.items.push((off, b));
        let mut total: usize = self.items.iter().map(|(_, b)| b.data.len()).sum();
        while self.items.len() > 1 && (self.items.len() > self.max_items || total > self.max_bytes) {
            let (_, b) = self.items.remove(0);
            total -= b.data.len();
        }
    }
}

/// Moved from stall_tests (the block decoder internals are private to `sst`).
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_entries_zero_block_and_bad_restarts() {
        // All zeros: num_restarts 0, every entry header 0/0/0 (3 bytes) -> must advance.
        let z = vec![0u8; 4096];
        let lay = layout(&z).unwrap();
        let (es, _) = entries(&z, &lay, None);
        assert!(es.windows(2).all(|w| w[1].off > w[0].off));
        // Restart count larger than the block: layout error, no loop.
        let mut b = vec![0u8; 64];
        b[60..64].copy_from_slice(&0x7fff_ffffu32.to_le_bytes());
        assert!(layout(&b).is_err());
        // Delta-encoded index values with zero-length deltas still advance.
        let (es, _) = entries(&z, &lay, Some(IndexFmt { value_delta: true, first_key: false }));
        assert!(es.windows(2).all(|w| w[1].off > w[0].off));
    }
}
