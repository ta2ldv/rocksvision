//! RocksDB log format framing shared by WAL *.log and MANIFEST-* files, RocksDB 10.4.2.
//! Source: db/log_format.h, db/log_reader.cc (ReadPhysicalRecord / ReadRecord).
//!
//! The file is a sequence of 32 KiB blocks. Each physical record:
//!   legacy:     crc32c(4, masked) | length(2, LE) | type(1) | payload
//!   recyclable: crc32c(4, masked) | length(2, LE) | type(1) | log_number(4, LE) | payload
//! CRC covers bytes [6 .. header_size+length) i.e. type (+log number) + payload.
//! A block tail shorter than a header is zero padding.

use crate::io::{read_vec, RangeRead};
pub use crate::limits::{MAX_FRAG_REFS, MAX_RECORD_BYTES};
use crate::limits::RESYNC_BUDGET;
use crate::model::{CheckKind, DecodedValue, Diagnostic, Space, Span, Status, Trust};
use crate::util::{crc32c, crc_unmask, fixed16, fixed32};
use std::io;

pub const BLOCK_SIZE: u64 = 32768;
pub const HEADER_SIZE: usize = 7;
pub const RECYCLABLE_HEADER_SIZE: usize = 11;

pub fn type_name(t: u8) -> &'static str {
    match t {
        0 => "ZERO",
        1 => "FULL",
        2 => "FIRST",
        3 => "MIDDLE",
        4 => "LAST",
        5 => "RECYCLABLE_FULL",
        6 => "RECYCLABLE_FIRST",
        7 => "RECYCLABLE_MIDDLE",
        8 => "RECYCLABLE_LAST",
        9 => "SET_COMPRESSION_TYPE",
        10 => "USER_DEFINED_TIMESTAMP_SIZE",
        11 => "RECYCLABLE_USER_DEFINED_TIMESTAMP_SIZE",
        130 => "PREDECESSOR_WAL_INFO",
        131 => "RECYCLE_PREDECESSOR_WAL_INFO",
        t if t & 0x80 != 0 => "UNKNOWN (safe-to-ignore bit set)",
        _ => "UNKNOWN",
    }
}

/// log_reader.cc: recyclable header for types 5..8, 11, 131.
pub fn is_recyclable(t: u8) -> bool {
    (5..=8).contains(&t) || t == 11 || t == 131
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FragRole {
    Full,
    First,
    Middle,
    Last,
    /// Standalone metadata record (compression type, UDT sizes, predecessor WAL info).
    Meta,
    Unknown,
}

pub fn role(t: u8) -> FragRole {
    match t {
        1 | 5 => FragRole::Full,
        2 | 6 => FragRole::First,
        3 | 7 => FragRole::Middle,
        4 | 8 => FragRole::Last,
        9 | 10 | 11 | 130 | 131 => FragRole::Meta,
        _ => FragRole::Unknown,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhysKind {
    Record,
    /// Tail of a block shorter than a header (writer pads with zeros).
    BlockTrailer,
    /// Zero type with zero length (preallocated/zeroed space). log_reader treats it as bad record.
    ZeroFill,
    /// Length field points past the block end.
    BadLength,
    /// File ends inside the header or payload.
    Truncated,
}

#[derive(Clone, Debug)]
pub struct Phys {
    pub offset: u64,
    pub kind: PhysKind,
    pub rtype: u8,
    pub header_len: usize,
    pub len: u32,
    pub stored_crc: u32,
    /// Unmasked computed CRC32C over type(+log number)+payload; None if payload incomplete.
    pub computed_crc: Option<u32>,
    pub log_number: Option<u32>,
    pub payload_off: u64,
    /// Payload bytes actually present (may be < len when truncated).
    pub payload_avail: u64,
    /// Offset right after this item.
    pub end: u64,
}

impl Phys {
    pub fn crc_ok(&self) -> Option<bool> {
        self.computed_crc.map(|c| c == crc_unmask(self.stored_crc))
    }
    /// Declared payload length (header length field) as expected and the payload bytes present as
    /// observed, when the header was read far enough to know the length (a header cut short
    /// before its length field has neither).
    pub fn declared_vs_available(&self) -> (Option<DecodedValue>, Option<DecodedValue>) {
        if self.header_len > 0 || self.rtype != 0 {
            (Some(DecodedValue::U64(u64::from(self.len))), Some(DecodedValue::U64(self.payload_avail)))
        } else {
            (None, None)
        }
    }
    /// Problem of this physical item as data (None for a verified record / padding).
    pub fn diagnostic(&self) -> Option<Diagnostic> {
        let span = Some(Span::file(self.offset, self.end.saturating_sub(self.offset)));
        match self.kind {
            PhysKind::Record => match self.crc_ok() {
                Some(false) => Some(
                    Diagnostic::new(CheckKind::Crc32c, span, Trust::Untrusted, RECOVER_CRC, format!("CRC mismatch in fragment at 0x{:x}", self.offset))
                        .values(self.computed_crc.map(DecodedValue::U32), Some(DecodedValue::U32(crc_unmask(self.stored_crc)))),
                ),
                Some(true) => None,
                None => {
                    let (e, o) = self.declared_vs_available();
                    Some(Diagnostic::new(CheckKind::Truncation, span, Trust::Partial, RECOVER_EOF, "payload incomplete").values(e, o))
                }
            },
            PhysKind::BlockTrailer | PhysKind::ZeroFill => None,
            PhysKind::BadLength => Some(Diagnostic::new(CheckKind::Length, span, Trust::Untrusted, RECOVER_RESYNC, "Length field runs past the end of the block: corrupt (kBadRecordLen).")),
            PhysKind::Truncated => {
                let (e, o) = self.declared_vs_available();
                Some(Diagnostic::new(CheckKind::Truncation, span, Trust::Partial, RECOVER_EOF, "File ends inside this record.").values(e, o))
            }
        }
    }
    pub fn status(&self) -> Status {
        match self.kind {
            PhysKind::Record => match self.crc_ok() {
                Some(true) => Status::Present,
                Some(false) => Status::Invalid,
                None => Status::Truncated,
            },
            PhysKind::BlockTrailer | PhysKind::ZeroFill => Status::Present,
            PhysKind::BadLength => Status::Invalid,
            PhysKind::Truncated => Status::Truncated,
        }
    }
}

/// Recovery descriptions of log-format diagnostics (what the reader does next).
pub const RECOVER_CRC: &str = "log_reader.cc drops the rest of this block; RocksVision locates the following items by trusting this record's length field (a guess)";
pub const RECOVER_EOF: &str = "reading stops at the end of the file";
pub const RECOVER_RESYNC: &str = "the scanner resynchronizes at the next offset holding a known type with a matching CRC, else skips the rest of the block";
pub const RECOVER_RESTART: &str = "log_reader.cc reports the partial record as corruption and restarts at the interrupting fragment";
pub const RECOVER_SKIP: &str = "the record is skipped";
pub const RECOVER_CAP: &str = "the first bytes up to the cap are kept; the rest is not decoded";
/// RocksDB 10.4.2 db/log_reader.cc Reader::ReadRecord, `case kEof` with `in_fragmented_record`:
/// the partial record is cleared; with kAbsoluteConsistency or kPointInTimeRecovery (the
/// default `wal_recovery_mode`, include/rocksdb/options.h) it also calls ReportCorruption.
pub const RECOVER_OPEN_AT_EOF: &str = "RocksDB's log reader (10.4.2 log_reader.cc) discards a record left incomplete at end of file; with wal_recovery_mode kPointInTimeRecovery (default) or kAbsoluteConsistency it also reports \"error reading trailing data due to encountering EOF\". RocksVision shows the collected fragments";
/// A read of the file failed while a fragmented record was being collected (not end of file).
/// model::CheckKind has no I/O kind: the diagnostic uses `Structure`, status Unknown, and this
/// recovery text identifies it (`is_read_error`).
pub const RECOVER_READ_ERROR: &str = "reading stops at the read error; the fragments read before it are shown, nothing is assumed about the rest";

/// True for the diagnostic of a read error (not a format problem of the bytes).
pub fn is_read_error(d: &Diagnostic) -> bool {
    d.recovery == RECOVER_READ_ERROR
}

/// A continuation fragment physically cut off by the end of file (the file ends inside its
/// header or payload), as the last item of a record left open at EOF.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CutFrag {
    pub offset: u64,
    pub rtype: u8,
    /// The header was read far enough to know type and length.
    pub header_known: bool,
    /// Declared payload length (header length field).
    pub len: u64,
    /// Payload bytes present before EOF.
    pub avail: u64,
}

impl CutFrag {
    fn of(p: &Phys) -> Self {
        CutFrag { offset: p.offset, rtype: p.rtype, header_known: p.header_len > 0 || p.rtype != 0, len: u64::from(p.len), avail: p.payload_avail }
    }
}

/// A fragmented logical record left open at end of file: FIRST (+ MIDDLE …) fragments and then
/// EOF, no LAST; the last fragment may itself be cut off by EOF (`cut`). Found by `open_tail`
/// (bounded backward scan) or `read_logical`; both build the same diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenTail {
    /// Header offset of the FIRST fragment; None when it lies before the scanned blocks.
    pub start: Option<u64>,
    /// Earliest fragment header found (= `start` when known).
    pub from: u64,
    /// Complete MIDDLE fragments after FIRST (counted within the scan).
    pub middles: u64,
    /// Payload bytes of the fragments found (of a cut fragment: the bytes present).
    pub collected: u64,
    /// File length.
    pub eof: u64,
    /// The trailing continuation fragment the end of file cuts off, if any.
    pub cut: Option<CutFrag>,
}

impl OpenTail {
    /// "FIRST @0x5eec + 2 MIDDLE" (or "2 MIDDLE from 0x…, FIRST before the scanned blocks").
    pub fn shape(&self) -> String {
        match self.start {
            Some(s) => format!("FIRST @0x{s:x} + {} MIDDLE", self.middles),
            None => format!("{} MIDDLE from 0x{:x}, FIRST before the scanned blocks (not read)", self.middles, self.from),
        }
    }
    /// Truncation over the record's bytes (start..EOF). Untrusted: RocksDB discards the record.
    /// Expected a LAST fragment, observed the end of file; with a cut fragment, its declared vs
    /// present payload length (the physical detail: the file ends inside that fragment).
    pub fn diagnostic(&self) -> Diagnostic {
        let span = Some(Span::file(self.from, self.eof.saturating_sub(self.from)));
        let (note, e, o) = match &self.cut {
            None => (
                format!("The logical record has no LAST fragment before end of file ({}; {} payload bytes collected).", self.shape(), self.collected),
                Some(DecodedValue::Text("LAST fragment".into())),
                Some(DecodedValue::Text(format!("end of file at 0x{:x}", self.eof))),
            ),
            Some(c) => {
                let (what, bytes, e, o) = if c.header_known {
                    (format!("{} fragment", type_name(c.rtype)), format!(" ({} of {} payload bytes)", c.avail, c.len), Some(DecodedValue::U64(c.len)), Some(DecodedValue::U64(c.avail)))
                } else {
                    ("fragment header".to_string(), String::new(), None, None)
                };
                let note = format!(
                    "The {what} at 0x{:x} is cut off by the end of file{bytes}: the file ends inside this fragment, so the logical record ({}, then the cut fragment; {} payload bytes collected) has no LAST fragment.",
                    c.offset,
                    self.shape(),
                    self.collected
                );
                (note, e, o)
            }
        };
        Diagnostic::new(CheckKind::Truncation, span, Trust::Untrusted, RECOVER_OPEN_AT_EOF, note).values(e, o)
    }
}

/// The record left open at end of file, if the last record-bearing item is a complete FIRST or
/// MIDDLE fragment, or an item cut off by EOF preceded by one (a cut continuation; the same
/// rule `read_logical` applies). Bounded: reads at most `max_blocks` blocks backwards from the
/// end. None when the tail ends with FULL/LAST/metadata, a cut-off item without an open
/// record before it (reported by `Phys::diagnostic`), an inconsistent sequence (reported by
/// `read_logical`) or a read error.
pub fn open_tail(r: &dyn RangeRead, max_blocks: u64) -> io::Result<Option<OpenTail>> {
    let n = block_count(r.len());
    let mut t = OpenTail { start: None, from: 0, middles: 0, collected: 0, eof: r.len(), cut: None };
    let mut seen = false;
    for b in (n.saturating_sub(max_blocks)..n).rev() {
        let (items, _) = scan_block(r, b)?;
        for p in items.iter().rev() {
            if matches!(p.kind, PhysKind::BlockTrailer | PhysKind::ZeroFill) {
                continue;
            }
            if p.kind == PhysKind::Truncated && !seen && t.cut.is_none() {
                // Only the trailing item can be cut by EOF; the record it continues decides.
                t.cut = Some(CutFrag::of(p));
                t.collected += p.payload_avail;
                t.from = p.offset;
                continue;
            }
            if p.kind != PhysKind::Record {
                return Ok(None);
            }
            match role(p.rtype) {
                FragRole::Middle => {
                    t.middles += 1;
                    t.collected += p.payload_avail;
                    t.from = p.offset;
                    seen = true;
                }
                FragRole::First => {
                    t.collected += p.payload_avail;
                    (t.start, t.from) = (Some(p.offset), p.offset);
                    return Ok(Some(t));
                }
                _ => return Ok(None),
            }
        }
    }
    Ok(seen.then_some(t))
}

pub fn block_count(file_len: u64) -> u64 {
    file_len.div_ceil(BLOCK_SIZE)
}

/// Scans one physical block. Returns the items and the block bytes.
pub fn scan_block(r: &dyn RangeRead, block: u64) -> io::Result<(Vec<Phys>, Vec<u8>)> {
    let base = block * BLOCK_SIZE;
    let buf = read_vec(r, base, BLOCK_SIZE)?;
    let at_eof = base + buf.len() as u64 >= r.len();
    let blen = buf.len();
    let mut out = Vec::new();
    let mut pos = 0usize;
    // Shared CRC budget for all resync attempts in this block (bounded work per scan).
    let mut budget = RESYNC_BUDGET;
    let mut iters = 0usize;
    while pos < blen {
        // Hard cap: every iteration advances pos by >= 1, so more than blen iterations means a bug.
        iters += 1;
        if iters > blen {
            break;
        }
        let loop_start = pos;
        let remaining = blen - pos;
        let off = base + pos as u64;
        let mk = |kind, end: usize| Phys {
            offset: off,
            kind,
            rtype: 0,
            header_len: 0,
            len: 0,
            stored_crc: 0,
            computed_crc: None,
            log_number: None,
            payload_off: off,
            payload_avail: 0,
            end: base + end as u64,
        };
        if remaining < HEADER_SIZE {
            // Writer pads the block tail with zeros (log_writer.cc). At EOF a short
            // non-zero tail is a truncated header.
            let zero = buf[pos..].iter().all(|&b| b == 0);
            let kind = if zero || blen as u64 == BLOCK_SIZE { PhysKind::BlockTrailer } else { PhysKind::Truncated };
            out.push(mk(kind, blen));
            break;
        }
        let rtype = buf[pos + 6];
        let len = u32::from(fixed16(&buf, pos + 4).unwrap_or(0));
        if rtype == 0 && len == 0 {
            // Zero header: run of zero bytes (preallocated or zeroed tail).
            let run = buf[pos..].iter().take_while(|&&b| b == 0).count();
            let end = if pos + run >= blen { blen } else { pos + HEADER_SIZE };
            out.push(mk(PhysKind::ZeroFill, end));
            pos = end;
            continue;
        }
        let hl = if is_recyclable(rtype) { RECYCLABLE_HEADER_SIZE } else { HEADER_SIZE };
        if remaining < hl {
            let mut p = mk(PhysKind::Truncated, blen);
            p.rtype = rtype;
            p.len = len;
            out.push(p);
            break;
        }
        let stored_crc = fixed32(&buf, pos).unwrap_or(0);
        let log_number = if hl == RECYCLABLE_HEADER_SIZE { fixed32(&buf, pos + 7) } else { None };
        let need = hl + len as usize;
        let mut p = Phys {
            offset: off,
            kind: PhysKind::Record,
            rtype,
            header_len: hl,
            len,
            stored_crc,
            computed_crc: None,
            log_number,
            payload_off: off + hl as u64,
            payload_avail: 0,
            end: off + need as u64,
        };
        let known = role(rtype) != FragRole::Unknown;
        let crc_ok = need <= remaining && crc_unmask(stored_crc) == crc32c(&buf[pos + 6..pos + need]);
        if need > remaining || (!known && !crc_ok) {
            // Not a framable record. Resynchronize at the next offset in this block that
            // carries a known type, fits, and has a matching CRC (e.g. stale tail of a
            // recycled WAL). log_reader.cc itself stops/drops here.
            if let Some(q) = resync(&buf, pos + 1, &mut budget) {
                p.kind = PhysKind::BadLength;
                p.payload_avail = 0;
                p.end = base + q as u64;
                out.push(p);
                pos = q;
                continue;
            }
            p.kind = if need > remaining && at_eof && (blen as u64) < BLOCK_SIZE { PhysKind::Truncated } else { PhysKind::BadLength };
            p.payload_avail = (remaining.saturating_sub(hl)) as u64;
            p.end = base + blen as u64;
            out.push(p);
            break;
        }
        // log_reader.cc: crc32c::Value(header + 6, length + header_size - 6)
        p.computed_crc = Some(crc32c(&buf[pos + 6..pos + need]));
        p.payload_avail = u64::from(len);
        out.push(p);
        pos += need;
        if pos <= loop_start {
            break; // monotonic guard (need >= 7, so unreachable)
        }
    }
    Ok((out, buf))
}

/// Next offset >= `from` in `buf` holding a known-type record that fits and whose CRC matches.
/// Bounded: at most RESYNC_BUDGET bytes are checksummed per block scan (shared `budget`).
fn resync(buf: &[u8], from: usize, budget: &mut usize) -> Option<usize> {
    let mut q = from;
    while q + HEADER_SIZE <= buf.len() {
        let t = buf[q + 6];
        let len = usize::from(fixed16(buf, q + 4).unwrap_or(0));
        if role(t) != FragRole::Unknown && !(t == 0 && len == 0) {
            let hl = if is_recyclable(t) { RECYCLABLE_HEADER_SIZE } else { HEADER_SIZE };
            let need = hl + len;
            if q + need <= buf.len() {
                if need > *budget {
                    *budget = 0;
                    return None;
                }
                *budget -= need;
                let stored = fixed32(buf, q).unwrap_or(0);
                if crc_unmask(stored) == crc32c(&buf[q + 6..q + need]) {
                    return Some(q);
                }
            }
        }
        q += 1;
    }
    None
}

/// Sequential physical iterator starting at a file offset; keeps one block in memory.
pub struct PhysIter<'a> {
    r: &'a dyn RangeRead,
    block: u64,
    items: Vec<Phys>,
    buf: Vec<u8>,
    i: usize,
    start: u64,
    pub error: Option<io::Error>,
}

impl<'a> PhysIter<'a> {
    pub fn new(r: &'a dyn RangeRead, start: u64) -> Self {
        let block = start / BLOCK_SIZE;
        let mut it = PhysIter { r, block, items: Vec::new(), buf: Vec::new(), i: 0, start, error: None };
        it.load();
        it
    }
    fn load(&mut self) {
        if self.block >= block_count(self.r.len()) {
            self.items.clear();
            return;
        }
        match scan_block(self.r, self.block) {
            Ok((items, buf)) => {
                self.items = items;
                self.buf = buf;
                self.i = 0;
            }
            Err(e) => {
                self.items.clear();
                self.error = Some(e);
            }
        }
    }
}

impl Iterator for PhysIter<'_> {
    /// Physical item and its in-block payload bytes.
    type Item = (Phys, Vec<u8>);
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.i >= self.items.len() {
                if self.items.is_empty() && self.error.is_some() {
                    return None;
                }
                if self.block + 1 >= block_count(self.r.len()) || self.items.is_empty() {
                    return None;
                }
                self.block += 1;
                self.load();
                continue;
            }
            let p = self.items[self.i].clone();
            self.i += 1;
            if p.end <= self.start {
                continue;
            }
            let base = self.block * BLOCK_SIZE;
            let s = (p.payload_off - base) as usize;
            let e = s + p.payload_avail as usize;
            let payload = self.buf.get(s..e).map(|b| b.to_vec()).unwrap_or_default();
            return Some((p, payload));
        }
    }
}

#[derive(Clone, Debug)]
pub struct FragRef {
    pub offset: u64,
    pub rtype: u8,
    pub header_len: usize,
    pub payload_off: u64,
    pub len: u64,
    pub crc_ok: Option<bool>,
    /// Offset of this fragment's payload inside the reassembled record.
    pub logical_off: u64,
}

#[derive(Clone, Debug)]
pub struct Logical {
    /// File offset of the first fragment header (identity of the record).
    pub start: u64,
    pub role: FragRole,
    pub rtype: u8,
    pub frags: Vec<FragRef>,
    pub frag_count: u64,
    pub total_len: u64,
    /// First `keep` bytes of the reassembled payload.
    pub data: Vec<u8>,
    pub status: Status,
    pub note: String,
    pub end: u64,
    pub log_number: Option<u32>,
    /// Set when `data` was produced by WAL stream decompression (compressed byte count).
    pub decompressed_from: Option<u64>,
    /// The problem behind a non-Present `status` (its `note` equals `note`).
    pub diag: Option<Diagnostic>,
    /// A fragmented record (FIRST …) that the end of file closed before its LAST fragment:
    /// either after a complete fragment or inside a cut-off one.
    pub no_last: bool,
}

impl Logical {
    pub fn data_complete(&self) -> bool {
        self.data.len() as u64 == self.total_len
    }

    /// Sets status + note and records the matching diagnostic over the record's file bytes.
    pub fn fail(&mut self, status: Status, check: CheckKind, trusted: Trust, recovery: &'static str, note: String) {
        self.status = status;
        self.diag = Some(Diagnostic::new(check, Some(Span::file(self.start, self.end.saturating_sub(self.start))), trusted, recovery, note.clone()));
        self.note = note;
    }

    /// Records the truncated fragment's declared length (expected) and available payload bytes
    /// (observed) on the truncation diagnostic.
    fn truncated_by(&mut self, p: &Phys) {
        if let Some(d) = self.diag.as_mut() {
            (d.expected, d.observed) = p.declared_vs_available();
        }
    }

    /// Like `fail`, for problems of the reassembled (or decompressed) payload.
    pub fn fail_payload(&mut self, status: Status, check: CheckKind, trusted: Trust, recovery: &'static str, note: String) {
        self.fail(status, check, trusted, recovery, note);
        if let Some(d) = self.diag.as_mut() {
            d.space = Space::Record(self.start);
            d.span = None;
        }
    }
}

/// Reads the next logical record at or after `start`, keeping at most `keep`
/// payload bytes (bounded by MAX_RECORD_BYTES). Returns None at end of file.
pub fn read_logical(r: &dyn RangeRead, start: u64, keep: usize) -> Option<Logical> {
    let keep = keep.min(MAX_RECORD_BYTES);
    let mut it = PhysIter::new(r, start);
    let mut cur: Option<Logical> = None;
    loop {
        let Some((p, payload)) = it.next() else {
            // PhysIter also ends on a read error: that is not the end of the file.
            if let (Some(mut l), Some(e)) = (cur.take_if(|_| it.error.is_some()), it.error.as_ref()) {
                let at = it.block * BLOCK_SIZE; // the block whose read failed
                let note = format!("read error at 0x{at:x}: {e}; the record's remaining fragments could not be read ({} fragment(s), {} payload bytes read before it)", l.frag_count, l.total_len);
                l.fail(Status::Unknown, CheckKind::Structure, Trust::Untrusted, RECOVER_READ_ERROR, note);
                return Some(l);
            }
            if let Some(mut l) = cur {
                let t = OpenTail { start: Some(l.start), from: l.start, middles: l.frag_count.saturating_sub(1), collected: l.total_len, eof: r.len(), cut: None };
                let d = t.diagnostic();
                l.status = Status::Truncated;
                l.note = d.note.clone();
                l.diag = Some(d);
                l.no_last = true;
                return Some(l);
            }
            return None;
        };
        if matches!(p.kind, PhysKind::BlockTrailer | PhysKind::ZeroFill) {
            continue;
        }
        let ro = role(p.rtype);
        if let Some(l) = cur.as_mut() {
            if p.kind != PhysKind::Record {
                append(l, &p, &payload, keep);
                l.end = p.end;
                if p.kind == PhysKind::Truncated {
                    // The end of file cuts a continuation fragment: no LAST follows. Same
                    // diagnostic as `open_tail` (root) for this record.
                    // frag_count includes FIRST and the cut fragment.
                    let t = OpenTail { start: Some(l.start), from: l.start, middles: l.frag_count.saturating_sub(2), collected: l.total_len, eof: r.len(), cut: Some(CutFrag::of(&p)) };
                    let d = t.diagnostic();
                    l.status = Status::Truncated;
                    l.note = d.note.clone();
                    l.diag = Some(d);
                    l.no_last = true;
                } else {
                    let note = format!("fragment at 0x{:x}: {:?}", p.offset, p.kind);
                    l.fail(Status::Invalid, CheckKind::Length, Trust::Untrusted, RECOVER_RESYNC, note);
                }
                return cur;
            }
            match ro {
                FragRole::Middle | FragRole::Last => {
                    append(l, &p, &payload, keep);
                    l.end = p.end;
                    if ro == FragRole::Last {
                        if l.status == Status::Present && l.total_len > MAX_RECORD_BYTES as u64 {
                            let note = format!("record of {} bytes exceeds the {} byte reassembly cap", l.total_len, MAX_RECORD_BYTES);
                            l.fail(Status::Limited, CheckKind::Limit, Trust::Partial, RECOVER_CAP, note);
                        }
                        return cur;
                    }
                }
                _ => {
                    // log_reader.cc reports the partial record as corruption and restarts here.
                    let note = format!("interrupted by {} at 0x{:x} before LAST", type_name(p.rtype), p.offset);
                    l.fail(Status::Invalid, CheckKind::Structure, Trust::Untrusted, RECOVER_RESTART, note);
                    return cur;
                }
            }
            continue;
        }
        let mut l = Logical {
            start: p.offset,
            role: ro,
            rtype: p.rtype,
            frags: Vec::new(),
            frag_count: 0,
            total_len: 0,
            data: Vec::new(),
            status: Status::Present,
            note: String::new(),
            end: p.end,
            log_number: p.log_number,
            decompressed_from: None,
            diag: None,
            no_last: false,
        };
        append(&mut l, &p, &payload, keep);
        match p.kind {
            PhysKind::Record => {}
            PhysKind::Truncated => {
                l.fail(Status::Truncated, CheckKind::Truncation, Trust::Partial, RECOVER_EOF, "file ends inside this record".into());
                l.truncated_by(&p);
                return Some(l);
            }
            _ => {
                let note = format!("undecodable bytes up to 0x{:x} (bad length/type/CRC; scanner resynchronized after it)", p.end);
                l.fail(Status::Invalid, CheckKind::Length, Trust::Untrusted, RECOVER_RESYNC, note);
                return Some(l);
            }
        }
        match ro {
            FragRole::Full | FragRole::Meta => return Some(l),
            FragRole::First => cur = Some(l),
            FragRole::Middle | FragRole::Last => {
                let note = format!("{} fragment without preceding FIRST", type_name(p.rtype));
                l.fail(Status::Invalid, CheckKind::Structure, Trust::Untrusted, RECOVER_SKIP, note);
                return Some(l);
            }
            FragRole::Unknown => {
                let st = if p.rtype & 0x80 != 0 { Status::Unknown } else { Status::Invalid };
                let note = format!("record type {} not known to RocksDB 10.4.2", p.rtype);
                l.fail(st, CheckKind::UnknownType, Trust::Untrusted, RECOVER_SKIP, note);
                return Some(l);
            }
        }
    }
}

fn append(l: &mut Logical, p: &Phys, payload: &[u8], keep: usize) {
    if l.frags.len() < MAX_FRAG_REFS {
        l.frags.push(FragRef {
            offset: p.offset,
            rtype: p.rtype,
            header_len: p.header_len,
            payload_off: p.payload_off,
            len: p.payload_avail,
            crc_ok: p.crc_ok(),
            logical_off: l.total_len,
        });
    }
    l.frag_count += 1;
    if p.crc_ok() == Some(false) && l.status == Status::Present {
        l.status = Status::Invalid;
        l.note = format!("CRC mismatch in fragment at 0x{:x}", p.offset);
        l.diag = p.diagnostic();
    }
    let room = keep.saturating_sub(l.data.len());
    l.data.extend_from_slice(&payload[..payload.len().min(room)]);
    l.total_len += payload.len() as u64;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::MemReader;
    use crate::util::crc_mask;
    use std::time::SystemTime;

    /// One legacy fragment filling a whole block (7 B header + 32761 B payload).
    fn block_fragment(rtype: u8) -> Vec<u8> {
        let mut b = vec![0u8; BLOCK_SIZE as usize];
        let len = BLOCK_SIZE as usize - HEADER_SIZE;
        b[4..6].copy_from_slice(&(len as u16).to_le_bytes());
        b[6] = rtype;
        for (i, x) in b[HEADER_SIZE..].iter_mut().enumerate() {
            *x = i as u8;
        }
        let crc = crc_mask(crc32c(&b[6..]));
        b[0..4].copy_from_slice(&crc.to_le_bytes());
        b
    }

    /// Reads fail from `fail_at` on (an I/O error, not end of data).
    struct FailingReader {
        inner: MemReader,
        fail_at: u64,
    }

    impl RangeRead for FailingReader {
        fn len(&self) -> u64 {
            self.inner.len()
        }
        fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
            if off >= self.fail_at {
                return Err(io::Error::other("injected read failure"));
            }
            self.inner.read_at(off, buf)
        }
        fn modified(&self) -> io::Result<SystemTime> {
            self.inner.modified()
        }
    }

    /// A read error while collecting a fragmented record is reported as a read error (status
    /// Unknown, the failing offset and the error), not as a record missing its LAST fragment.
    #[test]
    fn read_error_is_not_end_of_file() {
        let mut data = block_fragment(2); // FIRST
        data.extend(block_fragment(3)); // MIDDLE
        data.extend(block_fragment(4)); // LAST
        let ok = MemReader::new(data.clone());
        let l = read_logical(&ok, 0, 16).unwrap();
        assert_eq!((l.status, l.frag_count, l.no_last), (Status::Present, 3, false), "control: {}", l.note);

        let r = FailingReader { inner: MemReader::new(data), fail_at: BLOCK_SIZE };
        let l = read_logical(&r, 0, 16).unwrap();
        assert_eq!((l.status, l.no_last, l.frag_count), (Status::Unknown, false, 1), "{}", l.note);
        let d = l.diag.as_ref().unwrap();
        assert!(is_read_error(d) && d.trusted == Trust::Untrusted, "{d:?}");
        assert!(d.note.starts_with("read error at 0x8000: injected read failure"), "{}", d.note);
        assert!(!d.note.contains("LAST"), "{}", d.note);
        // The open-tail scan reports nothing on a read error (no false EOF claim).
        assert!(open_tail(&r, 64).is_err());
    }

    /// A trailing MIDDLE cut by EOF continues the open record: `open_tail` and `read_logical`
    /// build the same diagnostic over FIRST..EOF.
    #[test]
    fn cut_continuation_belongs_to_the_open_record() {
        let mut data = block_fragment(2);
        data.extend(block_fragment(3));
        let cut = block_fragment(3);
        data.extend_from_slice(&cut[..HEADER_SIZE + 100]);
        let r = MemReader::new(data);
        let t = open_tail(&r, 64).unwrap().expect("open tail");
        assert_eq!((t.start, t.from, t.middles, t.cut.as_ref().map(|c| (c.offset, c.len, c.avail))), (Some(0), 0, 1, Some((0x10000, 32761, 100))));
        let l = read_logical(&r, 0, 16).unwrap();
        assert_eq!((l.status, l.no_last), (Status::Truncated, true));
        assert_eq!(l.diag, Some(t.diagnostic()));
        // A cut item after a complete FULL record is not an open record.
        let mut full = block_fragment(1);
        full.extend_from_slice(&block_fragment(1)[..HEADER_SIZE + 100]);
        assert_eq!(open_tail(&MemReader::new(full), 64).unwrap(), None);
    }
}
