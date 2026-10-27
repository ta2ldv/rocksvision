//! ~100 MB fixtures (task 0008 / handoff 0015): one GENERATOR (synthetic WAL) and the ASSERTION helpers
//! for the engine-made fixtures. All are #[ignore]d because the fixtures are not in the repo.
//!
//! Fixture dir: `ROCKSVISION_LARGE_DIR` (default /private/tmp/rocksvision-large). The engine-made files come from
//!   `bash tests/tools/large-fixtures.sh large-sst <dir> --mb 100` / `large-wal <dir> --mb 100`
//! Fixture shape (engine-made, local only): large-sst.sst 105522569 B (NoCompression, 102849 entries, 25713 data blocks; not byte-reproducible),
//! large-wal.log 105239773 B (10496 WriteBatch records / 82505 Puts; sha256 734c05b7...43944, reproducible). The tests/evidence-0015/*.md notes
//! are local only (the folder is git-ignored); every value they hold that a test relies on is quoted in this file.
//! One-shot run of everything: `bash tests/tools/large-verify.sh`.
//!
//! | test                               | kind       | needs                                              |
//! |------------------------------------|------------|----------------------------------------------------|
//! | gen_large_fixtures                 | GENERATOR  | writes <dir>/large-wal-synth.log, size ROCKSVISION_LARGE_MB (default 100) |
//! | assert_large_fixtures_bounded_dump | ASSERTION  | <dir>/large-wal.log + <dir>/large-sst.sst (>= 99 MB each) |
//! | assert_large_wal_search_cancel     | ASSERTION  | <dir>/large-wal.log                                |
mod common;
use common::*;
use rocksvision::dump::{dump, DumpOpts};
use rocksvision::jobs::{search_file, Response, Worker};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const DEFAULT_DIR: &str = "/private/tmp/rocksvision-large";
const BLOCK: u64 = 32768;
/// Dump bound: 3 x the worst measured wall time (0.10 s = WAL `--limit 400 --rows 400` on the 100 MB WAL, release build, warm cache; FREEZE C measured 0.03-0.08 s; local notes in the git-ignored tests/evidence-0015/).
const DUMP_BOUND: Duration = Duration::from_millis(300);
/// macOS `/usr/bin/time -l` max RSS bound: 3 x the worst measured (16.6 MiB, SST) ~ 50 MiB.
const RSS_BOUND: u64 = 50 << 20;
/// A bounded WAL scan must read far less than the file: measured 616 KB of 105 MB (0.6 %); bound 2 % (3 x).
const WAL_SCAN_PERCENT: u64 = 2;

fn large_dir() -> PathBuf {
    PathBuf::from(std::env::var("ROCKSVISION_LARGE_DIR").unwrap_or_else(|_| DEFAULT_DIR.into()))
}

fn fixture(name: &str) -> PathBuf {
    let p = large_dir().join(name);
    let len = std::fs::metadata(&p).unwrap_or_else(|e| panic!("{}: {e}. Generate with `bash tests/tools/large-fixtures.sh large-{{sst|wal}} <dir> --mb 100`", p.display())).len();
    assert!(len >= 99_000_000, "{} is {len} B, expected a ~100 MB fixture", p.display());
    p
}

// ------------------------------------------------------------------ GENERATOR (asserts only the file length)

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

/// GENERATOR. `ROCKSVISION_LARGE_DIR=<dir> ROCKSVISION_LARGE_MB=100 cargo test --release --test large_gen -- --ignored gen_large_fixtures`
/// writes `<dir>/large-wal-synth.log` (hand-built 32 KiB blocks, 5 FULL records each; NOT the engine WAL). Asserts only its length.
#[test]
#[ignore = "GENERATOR: ROCKSVISION_LARGE_DIR=<dir> [ROCKSVISION_LARGE_MB=100] cargo test --release --test large_gen -- --ignored gen_large_fixtures"]
fn gen_large_fixtures() {
    let mb: u64 = std::env::var("ROCKSVISION_LARGE_MB").ok().and_then(|v| v.parse().ok()).unwrap_or(100);
    let blocks = (mb << 20) / BLOCK;
    let out = large_dir();
    std::fs::create_dir_all(&out).unwrap();
    let path = out.join("large-wal-synth.log");
    let mut w = std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(&path).unwrap());
    let mut seq = 1u64;
    for _ in 0..blocks {
        let mut blk = vec![0u8; BLOCK as usize]; // zero tail = block padding
        let mut at = 0;
        for _ in 0..5 {
            let rec = full_record(&payload(seq));
            blk[at..at + rec.len()].copy_from_slice(&rec);
            at += rec.len();
            seq += 1;
        }
        w.write_all(&blk).unwrap();
    }
    w.flush().unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), blocks * BLOCK);
    eprintln!("wrote {} ({blocks} blocks x 5 records)", path.display());
}

// ------------------------------------------------------------------ ASSERTIONS

fn run_dump(p: &std::path::Path, o: &DumpOpts) -> (String, Duration) {
    let mut buf = Vec::new();
    let t = Instant::now();
    let ok = dump(p, o, &mut buf).expect("dump I/O");
    let wall = t.elapsed();
    assert!(ok, "dump reported failure for {}", p.display());
    (String::from_utf8(buf).expect("utf8"), wall)
}

/// Max RSS of the real CLI, macOS only (`/usr/bin/time -l`); other OSes: not measured.
#[cfg(target_os = "macos")]
fn cli_rss(p: &std::path::Path, extra: &[&str]) -> u64 {
    let out = std::process::Command::new("/usr/bin/time").arg("-l").arg(env!("CARGO_BIN_EXE_rocksvision")).arg("--dump").arg(p).args(extra).output().expect("spawn /usr/bin/time");
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stderr).lines().find(|l| l.contains("maximum resident set size")).and_then(|l| l.split_whitespace().next()).and_then(|v| v.parse().ok()).expect("RSS line")
}

/// ASSERTION (no generation). Opens both ~100 MB engine fixtures through `rocksvision::dump::dump` (the CLI `--dump` entry) and checks:
/// wall <= DUMP_BOUND for `--limit 50` and `--limit 400 --rows 400`; no Invalid/Truncated; WAL scan bounded (scanned < WAL_SCAN_PERCENT % of the file);
/// SST root lists all 25713 data blocks (declared == listed) with grouped "not checked" nodes; CLI max RSS <= RSS_BOUND (macOS).
#[test]
#[ignore = "ASSERTION on the 100 MB fixtures: bash tests/tools/large-fixtures.sh (generate), then cargo test --release --test large_gen -- --ignored --nocapture assert_large_fixtures_bounded_dump"]
fn assert_large_fixtures_bounded_dump() {
    let wal = fixture("large-wal.log");
    let sst = fixture("large-sst.sst");
    let wal_len = std::fs::metadata(&wal).unwrap().len();
    let big = DumpOpts { limit: 400, rows: 400 };
    let small = DumpOpts { limit: 50, ..DumpOpts::default() };
    for (name, p, o) in [("wal limit50", &wal, &small), ("wal limit400/rows400", &wal, &big), ("sst limit50", &sst, &small), ("sst limit400/rows400", &sst, &big)] {
        let (text, wall) = run_dump(p, o);
        eprintln!("LARGE-DUMP {name}: wall={wall:?} stdout={} B", text.len());
        assert!(wall <= DUMP_BOUND, "{name}: {wall:?} > bound {DUMP_BOUND:?}");
        assert!(!text.contains("Invalid") && !text.contains("Truncated"), "{name}: Invalid/Truncated in output");
        if p == &wal {
            // "note: Records 1–64 of ≥65 · scanned [0x0,0x9658f) of 105239773 B"
            let line = text.lines().find(|l| l.contains("scanned [0x")).unwrap_or_else(|| panic!("{name}: no 'scanned [' note"));
            let end = line.split("scanned [0x0,0x").nth(1).and_then(|s| s.split(')').next()).and_then(|h| u64::from_str_radix(h, 16).ok()).unwrap_or_else(|| panic!("unparsable scan range: {line}"));
            assert!(line.contains(&format!("of {wal_len} B")), "{name}: {line}");
            eprintln!("LARGE-DUMP {name}: scanned {end} of {wal_len} B ({:.2} %)", end as f64 * 100.0 / wal_len as f64);
            assert!(end * 100 < wal_len * WAL_SCAN_PERCENT, "{name}: scanned {end} of {wal_len} B is not bounded");
            assert!(text.contains("Large file: blocks are grouped; only expanded groups are scanned"), "{name}: grouped-blocks note missing");
        } else {
            assert!(text.contains("data blocks listed 25713 of 25713"), "{name}: block count");
            assert!(text.contains("data blocks (declared) | 0x") && text.contains("| 25713 |"), "{name}: declared count");
            assert!(text.contains("· not checked"), "{name}: grouped 'not checked' nodes missing");
        }
    }
    #[cfg(target_os = "macos")]
    for (p, extra) in [(&wal, vec!["--limit", "50"]), (&wal, vec!["--limit", "400", "--rows", "400"]), (&sst, vec!["--limit", "50"]), (&sst, vec!["--limit", "400", "--rows", "400"])] {
        let rss = cli_rss(p, &extra);
        eprintln!("LARGE-DUMP-CLI {} {extra:?}: max RSS {:.1} MiB", p.file_name().unwrap().to_string_lossy(), rss as f64 / 1048576.0);
        assert!(rss <= RSS_BOUND, "max RSS {rss} B > {RSS_BOUND} B");
    }
    #[cfg(not(target_os = "macos"))]
    eprintln!("max RSS NOT MEASURED on this OS");
}

/// ASSERTION (measures what is measurable). Cancellation of a long WAL search through the real `Worker`:
/// (a) `search_file` stops at the first chunk boundary after its cancel flag is set (bytes scanned and latency measured);
/// (b) a newer `Worker::search` supersedes a running full scan of the 100 MB WAL: the old generation answers
///     `SearchDone{cancelled:true}`, never a hit, and the new generation's hit arrives; cancel latency = new request -> old Done.
/// Not testable here: the UI's decision to ignore stale generations (state level, covered by in-crate state tests).
#[test]
#[ignore = "ASSERTION on the 100 MB fixture: cargo test --release --test large_gen -- --ignored --nocapture assert_large_wal_search_cancel"]
fn assert_large_wal_search_cancel() {
    let wal = fixture("large-wal.log");
    let len = std::fs::metadata(&wal).unwrap().len();
    let absent: &[u8] = b"\x00no-such-key-0015";
    // (a) cooperative cancel inside search_file
    let flag = std::sync::atomic::AtomicBool::new(false);
    let done = AtomicU64::new(0);
    let flipped: std::cell::Cell<Option<Instant>> = std::cell::Cell::new(None);
    let r = search_file(&wal, absent, 0, false, &|| flag.load(Ordering::Relaxed), &|d, _| {
        done.store(d, Ordering::Relaxed);
        if d >= 4 << 20 && !flag.load(Ordering::Relaxed) {
            flipped.set(Some(Instant::now()));
            flag.store(true, Ordering::Relaxed);
        }
    });
    let lat = flipped.get().expect("cancel flag was never set (scan ended before 4 MiB?)").elapsed();
    eprintln!("CANCEL-A search_file: result={r:?} scanned_at_return={} B of {len} B, flag->return {lat:?}", done.load(Ordering::Relaxed));
    assert_eq!(r, None, "cancelled scan must return None");
    assert!(done.load(Ordering::Relaxed) <= 5 << 20, "scan continued past the next chunk after cancel");
    assert!(lat < Duration::from_millis(50), "flag->return {lat:?}");
    // full-scan duration of the absent needle (so (b) can be judged)
    let t = Instant::now();
    assert_eq!(search_file(&wal, absent, 0, false, &|| false, &|_, _| {}), Some(None));
    let full = t.elapsed();
    eprintln!("CANCEL-A full absent-needle scan: {full:?}");
    // (b) Worker-level supersede
    let w = Worker::spawn(Arc::new(|| {}));
    let recv = |max: Duration| w.rx.recv_timeout(max).ok();
    w.search(1, wal.clone(), absent.to_vec(), 0, false);
    let mut first_progress = None;
    while first_progress.is_none() {
        match recv(Duration::from_secs(10)).expect("no progress from the first search") {
            t if t.generation == 1 && matches!(t.msg, Response::SearchProgress { .. }) => first_progress = Some(Instant::now()),
            t if t.generation == 1 && matches!(t.msg, Response::SearchDone { .. }) => panic!("first search already finished (full scan {full:?}); too fast to supersede"),
            _ => {}
        }
    }
    let sent = Instant::now();
    w.search(2, wal.clone(), b"wk-000000010".to_vec(), 0, false);
    let (mut old_done, mut new_hit, mut old_after_hit) = (None, None, false);
    let mut old_done_at = None;
    while old_done.is_none() || new_hit.is_none() {
        let t = recv(Duration::from_secs(10)).expect("timeout waiting for old Done / new hit");
        match (t.generation, t.msg) {
            (1, Response::SearchDone { found, cancelled }) => {
                old_done = Some((found, cancelled));
                old_done_at = Some(sent.elapsed());
            }
            (1, Response::SearchHit { .. }) => old_after_hit = true,
            (2, Response::SearchHit { off, .. }) => new_hit = Some((off, sent.elapsed())),
            _ => {}
        }
    }
    eprintln!("CANCEL-B Worker: old Done={old_done:?} after {:?}; new hit {new_hit:?}", old_done_at.unwrap());
    assert!(!old_after_hit, "superseded search produced a hit");
    assert_eq!(old_done, Some((false, true)), "old search must end cancelled");
    assert!(old_done_at.unwrap() < Duration::from_millis(100), "cancel latency {:?}", old_done_at);
    let (off, _) = new_hit.unwrap();
    let mut head = vec![0u8; 1 << 20];
    use std::io::Read;
    std::fs::File::open(&wal).unwrap().read_exact(&mut head).unwrap();
    assert_eq!(head[off as usize..off as usize + 12].to_vec(), b"wk-000000010".to_vec(), "new search hit offset");
    let _ = first_progress;
}
