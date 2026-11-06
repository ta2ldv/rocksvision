//! Bounded WAL summary for the LSM Map's WAL overview (0026 item 4): logical records, WriteBatch
//! count, column family ids and sequence range of one WAL, read with the same log reader
//! (`format::log::read_logical`, WAL stream decompression, stale-record check) and WriteBatch
//! decoder (`batch::decode`) as the WAL views. Runs on a background job only.
//! Bounds: `WAL_SUMMARY_BYTES` of file per WAL, `WAL_SUMMARY_RECORD_KEEP` payload bytes per
//! record, `MAX_OPS_LISTED` operations per batch, `WAL_SUMMARY_CFS` CF ids.

use super::batch;
use super::{LogInspector, LogMode};
use crate::format::log::{self, FragRole, is_read_error, is_recyclable};
use crate::io::RangeRead;
use crate::limits::{MAX_OPS_LISTED, WAL_REASON_CHARS, WAL_SUMMARY_CFS, WAL_SUMMARY_RECORD_KEEP};
use crate::model::{PartialWhy, Space, Status, WalStats, WalSummary};
use std::path::PathBuf;

fn bounded(s: String) -> String {
    if s.chars().count() > WAL_REASON_CHARS { format!("{}…", s.chars().take(WAL_REASON_CHARS).collect::<String>()) } else { s }
}

/// Summarises the WAL `r` (named `path`, for the stale-record check) up to `budget` file bytes.
/// None when `cancel` became true.
pub fn summarize(r: Box<dyn RangeRead>, path: PathBuf, budget: u64, cancel: &dyn Fn() -> bool) -> Option<WalSummary> {
    let li = LogInspector::from_reader(r, path, LogMode::Wal);
    let len = li.len();
    let mut st = WalStats::default();
    let mut partial: Option<PartialWhy> = None;
    let mut recycled = false;
    let mut off = 0u64;
    loop {
        if cancel() {
            return None;
        }
        if off >= budget && off < len {
            partial = Some(PartialWhy::Bound { read: off, size: len });
            break;
        }
        // Bounded per fragment too (D5): one huge FIRST + MIDDLE… record stops at the bound
        // (≤ one block past it) and on cancel, instead of being read to its end.
        let Some(mut l) = log::read_logical_until(li.reader(), off, WAL_SUMMARY_RECORD_KEEP, budget, cancel) else { break };
        if cancel() {
            return None;
        }
        if l.status == Status::Limited && l.end > budget {
            partial = Some(PartialWhy::Bound { read: l.end, size: len });
            st.bytes_scanned = l.end;
            break;
        }
        if l.end <= off {
            return Some(WalSummary::Corrupt { stats: st, at: off, recycled, reason: format!("the reader did not advance at 0x{off:x}") });
        }
        off = l.end;
        recycled |= is_recyclable(l.rtype);
        li.mark_stale(&mut l);
        li.maybe_decompress(&mut l, WAL_SUMMARY_RECORD_KEEP);
        if l.diag.as_ref().is_some_and(is_read_error) {
            return Some(WalSummary::Unreadable(bounded(l.note)));
        }
        match l.status {
            Status::Present | Status::Limited => {}
            // An incomplete record at the end of the file: the records before it are counted.
            Status::Truncated if l.end >= len => {
                partial = Some(PartialWhy::TruncatedTail);
                break;
            }
            // A recycled log's stale record (kOldRecord, log_reader.cc 600–607) ends the log
            // (log_reader.cc 273–295).
            // Recognised by the engine diagnostic `mark_stale` records (D5 nit 5), not by proxy.
            _ if l.diag.as_ref().is_some_and(|d| d.engine == Some(log::ENGINE_OLD_RECORD)) => return Some(WalSummary::StaleTail { stats: st, at: l.start }),
            _ => return Some(WalSummary::Corrupt { stats: st, at: l.start, recycled, reason: bounded(format!("{} {}", l.status.label(), l.note)) }),
        }
        // Counted once the record is accepted (an incomplete tail or a stale record is not).
        st.records += 1;
        st.bytes_scanned = off;
        if !matches!(l.role, FragRole::Full | FragRole::First) {
            // Standalone metadata records (compression type, timestamp sizes, predecessor).
            continue;
        }
        let b = batch::decode(&l.data, l.total_len, Space::Record(l.start), MAX_OPS_LISTED);
        if let (Some(seq), Some(count)) = (b.seq, b.count) {
            st.batches += 1;
            let last = seq.saturating_add(u64::from(count).saturating_sub(1));
            st.seq = Some(match st.seq {
                Some((a, z)) => (a.min(seq), z.max(last)),
                None => (seq, last),
            });
        }
        for cf in b.ops.iter().filter_map(|o| o.cf) {
            if st.cfs.len() < WAL_SUMMARY_CFS || st.cfs.contains(&cf) {
                st.cfs.insert(cf);
            } else if partial.is_none() {
                partial = Some(PartialWhy::ManyCfs);
            }
        }
        match b.status {
            Status::Present => {}
            // Clipped by the bounded payload or the operation listing: CF ids may be missing.
            Status::Limited => {
                partial.get_or_insert(PartialWhy::LargeRecord { at: l.start });
            }
            _ => return Some(WalSummary::Corrupt { stats: st, at: l.start, recycled, reason: bounded(format!("WriteBatch {} {}", b.status.label(), b.note)) }),
        }
        if !b.note.is_empty() && b.status == Status::Present {
            // "listing limited to N operations"
            partial.get_or_insert(PartialWhy::ManyOps { at: l.start });
        }
    }
    Some(match partial {
        Some(why) => WalSummary::Partial { stats: st, why },
        None => WalSummary::Done(st),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::MemReader;
    use crate::model::CfMembership;
    use crate::util::{crc_mask, crc32c};

    /// One FULL legacy record.
    fn record(payload: &[u8]) -> Vec<u8> {
        let mut c = vec![1u8];
        c.extend_from_slice(payload);
        let mut r = crc_mask(crc32c(&c)).to_le_bytes().to_vec();
        r.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        r.push(1);
        r.extend_from_slice(payload);
        r
    }
    /// One RECYCLABLE_FULL record (type 5) with header log number `log`.
    fn recyclable(payload: &[u8], log: u32) -> Vec<u8> {
        let mut c = vec![5u8];
        c.extend_from_slice(&log.to_le_bytes());
        c.extend_from_slice(payload);
        let mut r = crc_mask(crc32c(&c)).to_le_bytes().to_vec();
        r.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        r.push(5);
        r.extend_from_slice(&log.to_le_bytes());
        r.extend_from_slice(payload);
        r
    }
    /// WriteBatch: seq, count, then Put ops in `cfs` (0 = default without a CF id).
    fn batch(seq: u64, cfs: &[u32]) -> Vec<u8> {
        let mut b = seq.to_le_bytes().to_vec();
        b.extend_from_slice(&(cfs.len() as u32).to_le_bytes());
        for &cf in cfs {
            if cf == 0 {
                b.push(0x1);
            } else {
                b.push(0x5);
                b.push(cf as u8);
            }
            b.extend_from_slice(&[1, b'k', 1, b'v']);
        }
        b
    }
    fn run(bytes: Vec<u8>, budget: u64) -> Option<WalSummary> {
        summarize(Box::new(MemReader::new(bytes)), PathBuf::from("000007.log"), budget, &|| false)
    }

    #[test]
    fn counts_batches_cfs_and_sequences() {
        let mut w = record(&batch(10, &[0, 2]));
        w.extend(record(&batch(12, &[3])));
        let WalSummary::Done(s) = run(w.clone(), u64::MAX).unwrap() else { panic!() };
        assert_eq!((s.records, s.batches, s.seq, s.bytes_scanned), (2, 2, Some((10, 12)), w.len() as u64));
        assert_eq!(s.cfs.into_iter().collect::<Vec<_>>(), vec![0, 2, 3]);
        // Empty file: done, nothing.
        assert_eq!(run(Vec::new(), u64::MAX), Some(WalSummary::Done(WalStats::default())));
    }

    #[test]
    fn budget_truncation_corruption_and_cancel() {
        let mut w = record(&batch(10, &[0]));
        let first = w.len() as u64;
        w.extend(record(&batch(11, &[1])));
        // Budget: only the first record.
        let s = run(w.clone(), first).unwrap();
        assert!(matches!(&s, WalSummary::Partial { stats, why: PartialWhy::Bound { read, .. } } if stats.records == 1 && *read == first), "{s:?}");
        assert_eq!(s.membership(1), CfMembership::Unknown, "not observed absent on a partial scan");
        assert_eq!(s.membership(0), CfMembership::Observed);
        // Incomplete last record: partial with the first record counted.
        let cut = w[..w.len() - 2].to_vec();
        assert!(matches!(run(cut, u64::MAX).unwrap(), WalSummary::Partial { stats, why: PartialWhy::TruncatedTail } if stats.records == 1));
        // A checksum mismatch in the middle: corrupt at its offset, not recycled.
        let mut bad = w.clone();
        bad[first as usize] ^= 0xff;
        bad.extend(record(&batch(12, &[0])));
        assert!(matches!(run(bad, u64::MAX).unwrap(), WalSummary::Corrupt { at, recycled: false, stats, .. } if at == first && stats.records == 1));
        // Cancelled: no summary.
        assert_eq!(summarize(Box::new(MemReader::new(w)), PathBuf::from("000007.log"), u64::MAX, &|| true), None);
        // Complete scan: CF 5 observed absent.
        let d = run(record(&batch(1, &[0])), u64::MAX).unwrap();
        assert_eq!(d.membership(5), CfMembership::ObservedAbsent);
    }

    /// 0026 D1: a recycled log's record with an older header log number ends the log
    /// (kOldRecord, log_reader.cc 273–295): StaleTail with the records before it, complete.
    #[test]
    fn stale_record_of_a_recycled_log_ends_it() {
        let mut w = recyclable(&batch(5, &[0]), 7);
        let first = w.len() as u64;
        w.extend(recyclable(&batch(1, &[2]), 3));
        let s = run(w, u64::MAX).unwrap();
        assert!(matches!(&s, WalSummary::StaleTail { stats, at } if stats.records == 1 && *at == first), "{s:?}");
        assert!(s.complete());
        assert_eq!(s.membership(2), CfMembership::ObservedAbsent, "the stale record is not this log's");
    }

    /// One legacy fragment of type `t` filling a whole 32 KiB block.
    fn block_fragment(t: u8, fill: u8) -> Vec<u8> {
        let payload = vec![fill; crate::format::log::BLOCK_SIZE as usize - 7];
        let mut c = vec![t];
        c.extend_from_slice(&payload);
        let mut r = crc_mask(crc32c(&c)).to_le_bytes().to_vec();
        r.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        r.push(t);
        r.extend_from_slice(&payload);
        r
    }

    /// D5 (reviewer should-fix 1): one huge record (FIRST + 8 MIDDLE, no LAST) is not read to
    /// its end: with a 2-block budget the summary stops inside it (≤ one block past the
    /// budget) as Partial(Bound), and a cancel stops it too.
    #[test]
    fn a_huge_fragmented_record_stops_at_the_byte_bound() {
        let bs = crate::format::log::BLOCK_SIZE;
        let mut w = block_fragment(2, 1);
        for _ in 0..8 {
            w.extend(block_fragment(3, 2));
        }
        let len = w.len() as u64;
        let budget = 2 * bs;
        let s = run(w.clone(), budget).unwrap();
        match &s {
            WalSummary::Partial { stats, why: PartialWhy::Bound { read, size } } => {
                assert!(stats.bytes_scanned <= budget + bs && *read <= budget + bs && *read > budget, "{s:?}");
                assert_eq!((*size, stats.records), (len, 0), "the cut record is not counted");
            }
            other => panic!("{other:?}"),
        }
        // Cancelled while inside the record: no summary.
        let n = std::cell::Cell::new(0);
        let cancel = || {
            n.set(n.get() + 1);
            n.get() > 3
        };
        assert_eq!(summarize(Box::new(MemReader::new(w)), PathBuf::from("000007.log"), u64::MAX, &cancel), None);
        // D6: the read stopped inside the record (per-fragment cancel), not after all 8 MIDDLEs.
        assert!(n.get() <= 5, "cancel checks: {}", n.get());
    }

    /// The real WAL fixtures: clean ones are Done with batches; the corrupt ones are not Done.
    #[test]
    fn fixture_wals_summarise() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let sum = |rel: &str| {
            let p = dir.join(rel);
            let r = crate::io::RangeReader::open(&p).unwrap();
            summarize(Box::new(r), p, crate::limits::WAL_SUMMARY_BYTES, &|| false).unwrap()
        };
        for rel in ["wal-basic/000004.log", "wal-fragmented/000004.log", "db-levels/000025.log", "db-checkpoint/000016.log"] {
            let s = sum(rel);
            assert!(matches!(&s, WalSummary::Done(st) if st.batches > 0 && st.seq.is_some() && st.cfs.contains(&0)) || matches!(&s, WalSummary::Done(st) if st.records == 0), "{rel}: {s:?}");
        }
        assert_eq!(sum("empty/000001.log"), WalSummary::Done(WalStats::default()));
        for rel in ["corrupt/wal-bitflip-payload.log", "corrupt/wal-truncated-mid-record.log", "corrupt/wal-missing-last.log"] {
            assert!(!matches!(sum(rel), WalSummary::Done(_)), "{rel}");
        }
        // The recycled WAL's leftover tail is a corrupt-or-leftover record, flagged recycled.
        assert!(matches!(sum("wal-recycled/000014.log"), WalSummary::Corrupt { recycled: true, at: 0x742, .. }));
    }
}
