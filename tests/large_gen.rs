//! Reproducible generator for the 64 MiB WAL used by the task 0006 responsiveness runs (NOT a normal test).
//! `ROCKSVISION_LARGE_OUT=<dir> CARGO_TARGET_DIR=target/tester cargo test --release --test large_gen -- --ignored gen_large_fixtures`
//! writes `<dir>/large-wal.log`. `<dir>/large.sst` comes from `tests/fixture-gen` (`fixture-gen large <dir>`, see tests/evidence-0011/large-fixtures.md).
mod common;
use common::*;
use std::io::Write;

const BLOCKS: u64 = 2048; // 2048 * 32 KiB = 64 MiB
const RECS_PER_BLOCK: u64 = 5;

/// Valid WriteBatch: seq, count 1, one Put (key "lw-<seq>", value ~40 B).
fn payload(seq: u64) -> Vec<u8> {
    let key = format!("lw-{seq:08}");
    let val = format!("v{seq:08}-{}", "0123456789abcdef".repeat(2));
    let mut p = Vec::new();
    p.extend_from_slice(&seq.to_le_bytes());
    p.extend_from_slice(&1u32.to_le_bytes());
    p.push(1); // Put (default CF)
    p.push(key.len() as u8);
    p.extend_from_slice(key.as_bytes());
    p.push(val.len() as u8);
    p.extend_from_slice(val.as_bytes());
    p
}

#[test]
#[ignore = "generator: set ROCKSVISION_LARGE_OUT=<dir>"]
fn gen_large_fixtures() {
    let out = std::path::PathBuf::from(std::env::var("ROCKSVISION_LARGE_OUT").expect("ROCKSVISION_LARGE_OUT not set"));
    std::fs::create_dir_all(&out).unwrap();
    let path = out.join("large-wal.log");
    let mut w = std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(&path).unwrap());
    let mut seq = 1u64;
    for _ in 0..BLOCKS {
        let mut blk = vec![0u8; 32768]; // zero tail = block padding
        let mut at = 0;
        for _ in 0..RECS_PER_BLOCK {
            let rec = full_record(&payload(seq));
            blk[at..at + rec.len()].copy_from_slice(&rec);
            at += rec.len();
            seq += 1;
        }
        w.write_all(&blk).unwrap();
    }
    w.flush().unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), BLOCKS * 32768);
    eprintln!("wrote {} ({} blocks x {RECS_PER_BLOCK} records)", path.display(), BLOCKS);
}
