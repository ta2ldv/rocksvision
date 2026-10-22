//! Bounded behaviour on a synthesized ~256 MiB WAL-like file.
mod common;
use common::*;
use rocksvision::jobs::search_file;
use std::io::Write;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const BLOCKS: u64 = 8192; // 8192 * 32 KiB = 256 MiB

struct Cleanup(std::path::PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One FULL record per block: a valid 20-byte WriteBatch (seq, count 1, Put key "ZZZ" = "ED" style bytes).
fn payload(i: u64, last: bool) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&(i + 1).to_le_bytes());
    p.extend_from_slice(&1u32.to_le_bytes());
    p.push(1); // Put
    p.push(3);
    if last {
        p.extend_from_slice(b"ZZZ");
        p.push(2);
        p.extend_from_slice(b"ED");
    } else {
        p.extend_from_slice(b"key");
        p.push(2);
        p.extend_from_slice(b"vv");
    }
    assert_eq!(p.len(), 20);
    p
}

fn build(path: &std::path::Path) {
    let f = std::fs::File::create(path).unwrap();
    let mut w = std::io::BufWriter::with_capacity(1 << 20, f);
    for b in 0..BLOCKS {
        let mut blk = vec![0u8; 32768];
        let rec = full_record(&payload(b, b == BLOCKS - 1));
        blk[..rec.len()].copy_from_slice(&rec);
        w.write_all(&blk).unwrap();
    }
    w.flush().unwrap();
}

#[test]
fn large_wal_dump_is_bounded_and_search_works() {
    let dir = scratch("large");
    let _g = Cleanup(dir.clone());
    let p = dir.join("000009.log");
    let t0 = Instant::now();
    build(&p);
    let len = std::fs::metadata(&p).unwrap().len();
    assert_eq!(len, BLOCKS * 32768);
    eprintln!("built {} MiB in {:?}", len >> 20, t0.elapsed());

    // --- dump under /usr/bin/time -l ---
    let bin = env!("CARGO_BIN_EXE_rocksvision");
    let t = Instant::now();
    let out = Command::new("/usr/bin/time").arg("-l").arg(bin).args(["--dump"]).arg(&p).args(["--limit", "50"]).output().expect("spawn /usr/bin/time");
    let wall = t.elapsed();
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "dump failed: {err}");
    let rss_bytes: u64 = err
        .lines()
        .find(|l| l.contains("maximum resident set size"))
        .and_then(|l| l.split_whitespace().next())
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("no RSS in time output: {err}"));
    eprintln!("LARGE-DUMP wall={wall:?} maxrss={} MiB ({} B) stdout={} B", rss_bytes >> 20, rss_bytes, stdout.len());
    assert!(wall < Duration::from_secs(60), "dump took {wall:?}");
    assert!(rss_bytes < 200 * 1000 * 1000, "max RSS {rss_bytes} B >= 200 MB");
    assert!(stdout.contains("node limit 50 reached") || stdout.lines().count() > 20, "dump output unexpectedly short");
    assert!(stdout.contains("8192"), "block count not reported");
    // first records decode as Present and are not flagged
    assert!(!stdout.contains("Invalid"), "valid blocks reported Invalid");

    // --- search ---
    let needle = b"ZZZ\x02ED";
    let want = (BLOCKS - 1) * 32768 + 7 + 12 + 2; // block start + header + batch header + tag + key len
    let t = Instant::now();
    let hit = search_file(&p, needle, 0, false, &never, &|_, _| {});
    eprintln!("SEARCH-FWD {:?} -> {hit:?}", t.elapsed());
    assert_eq!(hit, Some(Some(want)));
    let t = Instant::now();
    let hit = search_file(&p, needle, len, true, &never, &|_, _| {});
    eprintln!("SEARCH-BWD {:?}", t.elapsed());
    assert_eq!(hit, Some(Some(want)));
    // absent needle: full scan, Some(None)
    let t = Instant::now();
    assert_eq!(search_file(&p, b"no-such-needle-xyz", 0, false, &never, &|_, _| {}), Some(None));
    eprintln!("SEARCH-ABSENT {:?}", t.elapsed());
    // needle straddling a 1 MiB chunk boundary is found: first record of block 32 ends chunk edge test
    let boundary = 1u64 << 20; // chunk boundary; first bytes of block 32 are the record header
    let mut f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
    use std::io::{Seek, SeekFrom};
    f.seek(SeekFrom::Start(boundary - 3)).unwrap();
    f.write_all(b"BOUNDARYNEEDLE").unwrap();
    drop(f);
    assert_eq!(search_file(&p, b"BOUNDARYNEEDLE", 0, false, &never, &|_, _| {}), Some(Some(boundary - 3)));
    assert_eq!(search_file(&p, b"BOUNDARYNEEDLE", len, true, &never, &|_, _| {}), Some(Some(boundary - 3)));

    // --- cancellation ---
    let t = Instant::now();
    assert_eq!(search_file(&p, b"no-such-needle-xyz", 0, false, &|| true, &|_, _| {}), None);
    assert!(t.elapsed() < Duration::from_millis(500), "pre-cancelled search took {:?}", t.elapsed());
    // cancel after the first progress callback
    let calls = AtomicU32::new(0);
    let t = Instant::now();
    let r = search_file(&p, b"no-such-needle-xyz", 0, false, &|| calls.load(Ordering::SeqCst) >= 1, &|_, _| {
        calls.fetch_add(1, Ordering::SeqCst);
    });
    let el = t.elapsed();
    eprintln!("SEARCH-CANCEL-MID {el:?} progress_calls={}", calls.load(Ordering::SeqCst));
    assert_eq!(r, None);
    assert!(calls.load(Ordering::SeqCst) <= 2, "kept scanning after cancel");
    assert!(el < Duration::from_secs(2));
    // backward cancelled
    let calls = AtomicU32::new(0);
    let r = search_file(&p, b"no-such-needle-xyz", len, true, &|| calls.load(Ordering::SeqCst) >= 1, &|_, _| {
        calls.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(r, None);
    assert!(calls.load(Ordering::SeqCst) <= 2);
    // temp file removed by Cleanup guard
}

#[test]
fn parse_query_forms() {
    use rocksvision::jobs::parse_query;
    assert_eq!(parse_query("abc", false).unwrap(), b"abc");
    assert_eq!(parse_query("0x00ff", false).unwrap(), vec![0, 255]);
    assert_eq!(parse_query("hex:de ad", false).unwrap(), vec![0xde, 0xad]);
    assert_eq!(parse_query("de ad", true).unwrap(), vec![0xde, 0xad]);
    assert!(parse_query("0xabc", false).is_err());
    assert!(parse_query("zz", true).is_err());
    assert!(parse_query("", true).is_err());
}
