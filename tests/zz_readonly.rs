//! Runs last (alphabetical): exercises every fixture through the dump path, then proves nothing changed.
mod common;
use common::*;
use rocksvision::dump::{dump, DumpOpts};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn all_files(root: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            if e.file_type().unwrap().is_dir() {
                stack.push(p);
            } else {
                v.push(p);
            }
        }
    }
    v.sort();
    v
}

/// Pure-Rust replacement for `shasum -a 256 -c SHA256SUMS` (see tests/common).
fn shasum_ok(dir: &Path) -> (bool, String) {
    check_sums(dir)
}

#[test]
fn sha256_known_answers() {
    assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    assert_eq!(
        sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
    // padding boundaries (55/56/64 bytes of 'a'); reference values from `shasum -a 256`
    assert_eq!(sha256_hex(&vec![b'a'; 55]), "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318");
    assert_eq!(sha256_hex(&vec![b'a'; 56]), "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a");
    assert_eq!(sha256_hex(&vec![b'a'; 64]), "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb");
    let m = vec![b'a'; 1_000_000];
    assert_eq!(sha256_hex(&m), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
}

fn stamps(files: &[PathBuf]) -> Vec<(u64, std::time::SystemTime)> {
    files
        .iter()
        .map(|p| {
            let m = std::fs::metadata(p).unwrap();
            (m.len(), m.modified().unwrap())
        })
        .collect()
}

#[test]
fn fixtures_unchanged_after_every_dump_path() {
    let root = fx("");
    let files = all_files(&root);
    assert!(files.len() >= 70, "{} files", files.len());
    let before = stamps(&files);
    let (ok, msg) = shasum_ok(&root);
    assert!(ok, "baseline shasum failed:\n{msg}");

    // exercise every file (dump walks the same tree/views as the UI)
    for p in &files {
        let mut buf = Vec::new();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dump(p, &DumpOpts { limit: 300, rows: 50 }, &mut buf)));
        assert!(r.is_ok(), "panic dumping {}", p.display());
    }
    for d in ["wal-basic", "wal-recycled", "db-levels", "corrupt"] {
        let mut buf = Vec::new();
        assert!(dump(&fx(d), &DumpOpts::default(), &mut buf).unwrap());
    }
    let _ = rocksvision::jobs::search_file(&fx("sst-none/data.sst"), b"key-001999", 0, false, &never, &|_, _| {});

    let (ok, msg) = shasum_ok(&root);
    assert!(ok, "fixtures changed after test paths:\n{msg}");
    assert_eq!(before, stamps(&files), "size/mtime of a fixture changed");
}

/// The API exposes no open-mode accessor, so "opened without write access" is proven behaviourally:
/// dumping a 0444 copy inside 0555 directories succeeds (any create/write/truncate would fail with EACCES),
/// creates no extra files and leaves bytes, sizes and mtimes intact.
#[cfg(unix)]
#[test]
fn works_on_read_only_files_and_dirs() {
    let src = fx("");
    let dst = scratch("ro-copy");
    let files = all_files(&src);
    for f in &files {
        let t = dst.join(f.strip_prefix(&src).unwrap());
        std::fs::create_dir_all(t.parent().unwrap()).unwrap();
        std::fs::copy(f, &t).unwrap();
        std::fs::set_permissions(&t, std::fs::Permissions::from_mode(0o444)).unwrap();
    }
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&dst).unwrap().map(|e| e.unwrap().path()).filter(|p| p.is_dir()).collect();
    dirs.push(dst.clone());
    for d in &dirs {
        std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o555)).unwrap();
    }
    let copies = all_files(&dst);
    let before = stamps(&copies);
    for p in &copies {
        let mut buf = Vec::new();
        dump(p, &DumpOpts { limit: 100, rows: 30 }, &mut buf).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        assert!(!buf.is_empty());
        assert!(rocksvision::io::RangeReader::open(p).is_ok());
    }
    assert_eq!(before, stamps(&copies));
    assert_eq!(all_files(&dst).len(), files.len(), "extra files created next to the inspected files");
    let (ok, msg) = shasum_ok(&dst);
    assert!(ok, "{msg}");
    for d in &dirs {
        std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let _ = std::fs::remove_dir_all(&dst);
}
