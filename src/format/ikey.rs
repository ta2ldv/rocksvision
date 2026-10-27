//! Internal keys and value types shared by WAL, SST and MANIFEST decoding, RocksDB 10.4.2.
//! Source: db/dbformat.h (ValueType, PackSequenceAndType, kMaxSequenceNumber).

use crate::util::{bytes_preview, escape_key, fixed64};

/// kMaxSequenceNumber: separators/seek keys carry it with kValueTypeForSeek.
pub const MAX_SEQUENCE: u64 = (1u64 << 56) - 1;

/// Splits an internal key into (user key, seq, type). dbformat.h: packed = seq << 8 | type.
pub fn split_ikey(k: &[u8]) -> Option<(&[u8], u64, u8)> {
    if k.len() < 8 {
        return None;
    }
    let p = fixed64(k, k.len() - 8)?;
    Some((&k[..k.len() - 8], p >> 8, (p & 0xff) as u8))
}

/// Canonical one-line rendering of an internal key (user key preview, seq, type code).
pub fn ikey_preview(k: &[u8]) -> String {
    match split_ikey(k) {
        Some((u, s, t)) => format!("{} @{} t{}", bytes_preview(u, 40), s, t),
        None => format!("invalid internal key {}", bytes_preview(k, 24)),
    }
}

/// Readable meaning of an internal key (0015 design D1/D3): `"key-006" · seq 7 · Value (Put)`;
/// the type name is the same `value_type_name` the Type column uses.
pub fn ikey_meaning(k: &[u8]) -> String {
    match split_ikey(k) {
        Some((u, s, t)) => {
            let seq = if s == MAX_SEQUENCE { "max (2^56−1)".to_string() } else { s.to_string() };
            let ty = match value_type_name(t) {
                "Unknown" => format!("type 0x{t:x} (unknown)"),
                n => n.to_string(),
            };
            format!("{} · seq {seq} · {ty}", escape_key(u, 64))
        }
        None => format!("{} · internal key < 8 B (no seq/type)", escape_key(k, 64)),
    }
}

/// User key of an internal key (the whole key if it is shorter than the 8-byte trailer).
pub fn user_key(k: &[u8]) -> &[u8] {
    split_ikey(k).map(|x| x.0).unwrap_or(k)
}

/// dbformat.h ValueType names (WAL WriteBatch tags share this enum).
pub fn value_type_name(t: u8) -> &'static str {
    match t {
        0x0 => "Deletion",
        0x1 => "Value (Put)",
        0x2 => "Merge",
        0x3 => "LogData",
        0x4 => "ColumnFamilyDeletion",
        0x5 => "ColumnFamilyValue (Put)",
        0x6 => "ColumnFamilyMerge",
        0x7 => "SingleDeletion",
        0x8 => "ColumnFamilySingleDeletion",
        0x9 => "BeginPrepareXID",
        0xA => "EndPrepareXID",
        0xB => "CommitXID",
        0xC => "RollbackXID",
        0xD => "Noop",
        0xE => "ColumnFamilyRangeDeletion",
        0xF => "RangeDeletion",
        0x10 => "ColumnFamilyBlobIndex",
        0x11 => "BlobIndex",
        0x12 => "BeginPersistedPrepareXID",
        0x13 => "BeginUnprepareXID",
        0x14 => "DeletionWithTimestamp",
        0x15 => "CommitXIDAndTimestamp",
        0x16 => "WideColumnEntity",
        0x17 => "ColumnFamilyWideColumnEntity",
        0x18 => "ValuePreferredSeqno (TimedPut)",
        0x19 => "ColumnFamilyValuePreferredSeqno (TimedPut)",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Design D1/D3: readable key meaning of a data-block entry.
    #[test]
    fn ikey_meaning_is_readable() {
        let mut k = b"key-006".to_vec();
        k.extend_from_slice(&((7u64 << 8) | 1).to_le_bytes());
        assert_eq!(ikey_meaning(&k), "\"key-006\" · seq 7 · Value (Put)");
        let mut u = vec![0xffu8];
        u.extend_from_slice(&((3u64 << 8) | 0x7f).to_le_bytes());
        assert_eq!(ikey_meaning(&u), "\"\\xff\" (not UTF-8) · seq 3 · type 0x7f (unknown)");
        assert_eq!(ikey_meaning(b"abc"), "\"abc\" · internal key < 8 B (no seq/type)");
    }
}
