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

// ---- pure-Rust SHA-256 (FIPS 180-4) so tests need no shasum/sha256sum (Windows-portable) ----

pub fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    let mut out = [0u8; 32];
    for i in 0..8 {
        out[4 * i..4 * i + 4].copy_from_slice(&h[i].to_be_bytes());
    }
    out
}

pub fn sha256_hex(data: &[u8]) -> String {
    sha256(data).iter().map(|b| format!("{b:02x}")).collect()
}

/// Equivalent of `shasum -a 256 -c SHA256SUMS` run in `dir`: (all ok, report of failures / counts).
pub fn check_sums(dir: &Path) -> (bool, String) {
    let list = match std::fs::read_to_string(dir.join("SHA256SUMS")) {
        Ok(s) => s,
        Err(e) => return (false, format!("cannot read SHA256SUMS: {e}")),
    };
    let (mut n, mut bad) = (0usize, String::new());
    for line in list.lines().filter(|l| !l.trim().is_empty()) {
        let Some((hash, rel)) = line.split_once("  ") else {
            bad.push_str(&format!("malformed line: {line}\n"));
            continue;
        };
        n += 1;
        match std::fs::read(dir.join(rel.trim_start_matches('*'))) {
            Ok(b) if sha256_hex(&b) == hash.to_ascii_lowercase() => {}
            Ok(_) => bad.push_str(&format!("{rel}: FAILED\n")),
            Err(e) => bad.push_str(&format!("{rel}: cannot read ({e})\n")),
        }
    }
    (bad.is_empty() && n > 0, format!("{n} entries checked\n{bad}"))
}
