//! Runs last (alphabetical): exercises every fixture through the dump path, then proves nothing changed.
mod common;
use common::*;
use rocksvision::dump::{dump, DumpOpts};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

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

fn shasum_ok(dir: &Path) -> (bool, String) {
    let out = Command::new("shasum").args(["-a", "256", "-c", "SHA256SUMS"]).current_dir(dir).output().expect("shasum");
    (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
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
