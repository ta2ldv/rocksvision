//! Log-format file state for WAL (*.log) and MANIFEST-* files: log number from the file name,
//! WAL stream compression, bounded record cache. Framing lives in `format::log`; views and tree
//! children are built by `present::wal`, which reads this state only through the accessor
//! methods below (the reader, path and cache are private).

pub mod batch;

/// Shim: log framing moved to `format::log` (shared with MANIFEST decoding).
pub use crate::format::log;
pub use crate::limits::{MAX_OPS_LISTED, RECORDS_PER_PAGE};

use crate::format::codec::{compression_name, zstd_stream, ZSTD};
use crate::io::{RangeRead, RangeReader};
use crate::limits::MAX_RECORD_BYTES;
use crate::model::{CheckKind, DecodedValue as DV, Diagnostic, Fact, FieldKind as K, Space, Span, Status, Trust};
use crate::util::{bytes_preview, fixed16, fixed32, fixed64};
use log::{FragRole, Logical, Phys};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogMode {
    Wal,
    Manifest,
}

/// Leading record of a WAL: start offset, payload offset and status (judges compression presence).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FirstRecord {
    pub start: u64,
    pub payload_off: u64,
    pub status: Status,
}

pub struct LogInspector {
    r: Box<dyn RangeRead>,
    /// Source path (display, sibling lookups); empty for in-memory sources.
    path: PathBuf,
    mode: LogMode,
    /// Log number from the file name (for recyclable header checks).
    expected_log: Option<u64>,
    /// WAL compression announced by a leading SET_COMPRESSION_TYPE record. Public as a shim
    /// for tests; read it through `wal_compression()`.
    pub wal_compression: Option<u32>,
    first_record: Option<FirstRecord>,
    cache: Option<Logical>,
}

/// db/filename.cc: "<number>.log" and "MANIFEST-<number>".
pub fn log_number_from_name(p: &Path) -> Option<u64> {
    p.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".log").or_else(|| n.strip_prefix("MANIFEST-"))).and_then(|n| n.parse::<u64>().ok())
}

impl LogInspector {
    /// Shim for the former file-only constructor.
    pub fn new(r: RangeReader, mode: LogMode) -> Self {
        let path = r.path().to_path_buf();
        Self::from_reader(Box::new(r), path, mode)
    }

    pub fn from_reader(r: Box<dyn RangeRead>, path: PathBuf, mode: LogMode) -> Self {
        let expected_log = log_number_from_name(&path);
        let mut s = LogInspector { r, path, mode, expected_log, wal_compression: None, first_record: None, cache: None };
        if mode == LogMode::Wal {
            if let Some(l) = log::read_logical(&*s.r, 0, 64) {
                let payload = l.frags.first().map(|f| f.payload_off).unwrap_or(l.start);
                s.first_record = Some(FirstRecord { start: l.start, payload_off: payload, status: l.status });
                if l.rtype == 9 && l.data.len() >= 4 {
                    s.wal_compression = fixed32(&l.data, 0);
                }
            }
        }
        s
    }

    // ---------------- accessors (the presenter API) ----------------

    pub fn reader(&self) -> &dyn RangeRead {
        &*self.r
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn mode(&self) -> LogMode {
        self.mode
    }
    pub fn expected_log(&self) -> Option<u64> {
        self.expected_log
    }
    pub fn wal_compression(&self) -> Option<u32> {
        self.wal_compression
    }
    pub fn first_record(&self) -> Option<FirstRecord> {
        self.first_record
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
    /// Physical items of one 32 KiB block and the block bytes.
    pub fn scan_block(&self, block: u64) -> std::io::Result<(Vec<Phys>, Vec<u8>)> {
        log::scan_block(&*self.r, block)
    }
    /// Next logical record at or after `start`, keeping at most `keep` payload bytes (uncached).
    pub fn read_logical(&self, start: u64, keep: usize) -> Option<Logical> {
        log::read_logical(&*self.r, start, keep)
    }

    pub fn what(&self) -> &'static str {
        match self.mode {
            LogMode::Wal => "WAL",
            LogMode::Manifest => "MANIFEST",
        }
    }

    /// Fully reassembled record (bounded), cached for repeated hex/field access.
    /// For a ZSTD-compressed WAL the record payload is replaced by its decompressed bytes.
    pub fn record(&mut self, off: u64) -> Option<&Logical> {
        if self.cache.as_ref().map(|l| l.start) != Some(off) {
            let mut l = log::read_logical(&*self.r, off, MAX_RECORD_BYTES);
            if let Some(x) = l.as_mut() {
                self.maybe_decompress(x, MAX_RECORD_BYTES);
            }
            self.cache = l;
        }
        self.cache.as_ref().filter(|l| l.start == off)
    }

    /// db/log_writer.cc resets the streaming compressor per AddRecord and db/log_reader.cc
    /// resets the decompressor per record: the concatenated fragment payloads of one logical
    /// record form one ZSTD stream (ZSTDStreamingUncompress, util/compression.cc).
    pub fn maybe_decompress(&self, l: &mut Logical, cap: usize) {
        let c = self.wal_compression.unwrap_or(0);
        if self.mode != LogMode::Wal || c == 0 || !matches!(l.role, FragRole::Full | FragRole::First) {
            return;
        }
        if c != u32::from(ZSTD) {
            let note = format!("WAL stream compression {} is not decodable here (RocksDB only allows ZSTD)", compression_name(c as u8));
            l.fail_payload(Status::Unsupported, CheckKind::Unsupported, Trust::Partial, "the compressed stream is shown undecoded", note);
            return;
        }
        if !l.data_complete() {
            l.fail_payload(Status::Limited, CheckKind::Limit, Trust::Partial, "the compressed stream is shown undecoded", "compressed record exceeds the reassembly cap; not decompressed".into());
            return;
        }
        let compressed = l.data.len() as u64;
        match zstd_stream(&l.data, cap) {
            Ok((out, total, complete)) => {
                l.total_len = total;
                l.data = out;
                l.decompressed_from = Some(compressed);
                if !complete && l.status == Status::Present {
                    let note = format!("decompressed output exceeds {} bytes (reassembly cap)", MAX_RECORD_BYTES);
                    l.fail_payload(Status::Limited, CheckKind::Limit, Trust::Partial, log::RECOVER_CAP, note);
                }
            }
            Err(e) => {
                l.fail_payload(Status::Invalid, CheckKind::Decompression, Trust::Untrusted, "the compressed stream is shown undecoded", format!("zstd stream decode failed: {e}"));
            }
        }
    }
}

impl LogInspector {
    /// Marks a recyclable record whose header log number differs from the file's number as
    /// stale (status Unknown + diagnostic; log_reader.cc treats it as kOldRecord).
    pub fn mark_stale(&self, l: &mut Logical) {
        if let (Some(ln), Some(e)) = (l.log_number, self.expected_log) {
            if u64::from(ln) != e & 0xffff_ffff && l.status == Status::Present {
                let note = format!("stale record from an older log (header log number {ln}, file {e}); log_reader.cc treats it as kOldRecord");
                l.fail(Status::Unknown, CheckKind::LogNumber, Trust::Untrusted, "log_reader.cc skips the record (kOldRecord)", note);
                if let Some(d) = l.diag.as_mut() {
                    d.expected = Some(DV::U64(e & 0xffff_ffff));
                    d.observed = Some(DV::U32(ln));
                }
            }
        }
    }
}

/// Facts of a standalone metadata record. Sources: util/compression.h CompressionTypeRecord,
/// util/udt_util.h UserDefinedTimestampSizeRecord, db/dbformat.h PredecessorWALInfo.
pub fn meta_facts(l: &Logical) -> Vec<Fact> {
    let d = &l.data;
    let sp = Space::Record(l.start);
    let short = |need: u64| {
        let note = format!("payload shorter than {need} bytes");
        Diagnostic::new(CheckKind::Length, Some(Span::new(sp, 0, d.len() as u64)), Trust::Partial, "the field is not decoded", note).values(Some(DV::U64(need)), Some(DV::U64(d.len() as u64)))
    };
    let mut f = Vec::new();
    match l.rtype {
        9 => match fixed32(d, 0) {
            Some(c) => f.push(Fact::new("compression type", Some(Span::new(sp, 0, 4)), format!("{c} {}", compression_name(c as u8))).kind(K::CompressionType).dv(DV::U32(c)).raw(&d[..4]).enc("fixed32 LE")),
            None => f.push(Fact::new("compression type", None, "payload shorter than 4 bytes").kind(K::CompressionType).status(Status::Truncated).diag(short(4))),
        },
        10 | 11 => {
            if d.len() % 6 != 0 {
                let note = format!("{} bytes is not a multiple of 6", d.len());
                f.push(Fact::new("timestamp sizes", None, note.clone()).status(Status::Invalid).diag(Diagnostic::new(CheckKind::Length, Some(Span::new(sp, 0, d.len() as u64)), Trust::Partial, "complete 6-byte entries are listed", note)));
            }
            for (i, ch) in d.chunks_exact(6).enumerate() {
                let cf = fixed32(ch, 0).unwrap_or(0);
                let ts = fixed16(ch, 4).unwrap_or(0);
                f.push(Fact::new(format!("cf {cf} timestamp size"), Some(Span::new(sp, (i * 6) as u64, 6)), ts.to_string()).dv(DV::U16(ts)).raw(ch).enc("fixed32 cf + fixed16 size"));
            }
        }
        130 | 131 => {
            let names = ["predecessor log number", "predecessor size bytes", "predecessor last seqno"];
            for (i, n) in names.iter().enumerate() {
                match fixed64(d, i * 8) {
                    Some(v) => f.push(Fact::new(*n, Some(Span::new(sp, (i * 8) as u64, 8)), v.to_string()).dv(DV::U64(v)).raw(&d[i * 8..i * 8 + 8]).enc("fixed64 LE")),
                    None => f.push(Fact::new(*n, None, "missing").status(Status::Truncated).diag(short((i as u64 + 1) * 8))),
                }
            }
        }
        _ => f.push(Fact::new("payload", Some(Span::new(sp, 0, d.len() as u64)), bytes_preview(d, 48)).kind(K::Payload).status(l.status)),
    }
    f
}

/// Recyclable record whose header log number differs from the file's number: stale (kOldRecord).
pub fn stale(st: Status, log_number: Option<u32>, expected: Option<u64>) -> Status {
    match (log_number, expected) {
        (Some(n), Some(e)) if st == Status::Present && u64::from(n) != e & 0xffff_ffff => Status::Unknown,
        _ => st,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::MemReader;

    /// Parser test without a file: one FULL legacy record holding an empty WriteBatch header.
    #[test]
    fn in_memory_wal_record() {
        let payload = [7u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut crc_in = vec![1u8];
        crc_in.extend_from_slice(&payload);
        let crc = crate::util::crc_mask(crate::util::crc32c(&crc_in));
        let mut rec = crc.to_le_bytes().to_vec();
        rec.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        rec.push(1);
        rec.extend_from_slice(&payload);
        let mut li = LogInspector::from_reader(Box::new(MemReader::new(rec)), PathBuf::from("000042.log"), LogMode::Wal);
        assert_eq!(li.expected_log(), Some(42));
        let l = li.record(0).expect("record").clone();
        assert_eq!(l.status, Status::Present);
        let b = batch::decode(&l.data, l.total_len, crate::model::Space::Record(0), 10);
        assert_eq!(b.seq, Some(7));
        assert_eq!(b.count, Some(0));
    }
}
