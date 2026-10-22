#![allow(dead_code)]
//! Shared helpers for the RocksVision integration tests (std only).

use rocksvision::io::RangeReader;
use rocksvision::wal::log::{read_logical, Logical};
use std::path::{Path, PathBuf};

pub fn fx(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(rel)
}

pub fn rd(rel: &str) -> RangeReader {
    RangeReader::open(&fx(rel)).unwrap_or_else(|e| panic!("open {rel}: {e}"))
}

pub fn never() -> bool {
    false
}

/// All logical records of a log file, in order (full payload kept).
pub fn all_logical(r: &RangeReader) -> Vec<Logical> {
    let mut out = Vec::new();
    let mut off = 0u64;
    while let Some(l) = read_logical(r, off, 1 << 26) {
        assert!(l.end > off, "reader made no progress at {off}");
        off = l.end;
        out.push(l);
        assert!(out.len() < 1_000_000);
    }
    out
}

pub fn lcg_bytes(n: usize) -> Vec<u8> {
    (0..n).map(|i| b'a' + (i % 26) as u8).collect()
}

/// Fresh scratch directory under the system temp dir (removed by the caller).
pub fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rocksvision-test-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Frames `payload` as one legacy FULL physical record (masked CRC32C over type+payload).
pub fn full_record(payload: &[u8]) -> Vec<u8> {
    let mut crc_in = vec![1u8];
    crc_in.extend_from_slice(payload);
    let crc = rocksvision::util::crc_mask(rocksvision::util::crc32c(&crc_in));
    let mut v = Vec::new();
    v.extend_from_slice(&crc.to_le_bytes());
    v.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    v.push(1);
    v.extend_from_slice(payload);
    v
}

/// Writes records that all fit in block 0 (total < 32 KiB).
pub fn write_small_log(path: &Path, payloads: &[Vec<u8>]) {
    let mut out = Vec::new();
    for p in payloads {
        out.extend_from_slice(&full_record(p));
    }
    assert!(out.len() < 32768);
    std::fs::write(path, out).unwrap();
}

pub fn varint(mut v: u64, out: &mut Vec<u8>) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

pub fn lp(b: &[u8], out: &mut Vec<u8>) {
    varint(b.len() as u64, out);
    out.extend_from_slice(b);
}

pub fn ikey(user: &[u8], seq: u64, ty: u8) -> Vec<u8> {
    let mut k = user.to_vec();
    k.extend_from_slice(&((seq << 8) | ty as u64).to_le_bytes());
    k
}
