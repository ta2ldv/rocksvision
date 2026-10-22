//! Data-directory discovery: non-recursive scan, never follows symlinks.
//! File naming per RocksDB 10.4.2 db/filename.cc.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FileKind {
    Current,
    Manifest,
    Wal,
    Sst,
    Options,
    /// Diagnostic text log (LOG, LOG.old.*) — not a WAL.
    InfoLog,
    Lock,
    Identity,
    Blob,
    Other,
}

impl FileKind {
    pub fn label(&self) -> &'static str {
        match self {
            FileKind::Current => "CURRENT",
            FileKind::Manifest => "MANIFEST",
            FileKind::Wal => "WAL",
            FileKind::Sst => "SST",
            FileKind::Options => "OPTIONS (text)",
            FileKind::InfoLog => "info LOG (text, not WAL)",
            FileKind::Lock => "LOCK",
            FileKind::Identity => "IDENTITY (text)",
            FileKind::Blob => "blob file (not decoded)",
            FileKind::Other => "other",
        }
    }
    pub fn decodable(&self) -> bool {
        matches!(self, FileKind::Current | FileKind::Manifest | FileKind::Wal | FileKind::Sst)
    }
}

pub fn classify(name: &str) -> FileKind {
    if name == "CURRENT" {
        FileKind::Current
    } else if name.starts_with("MANIFEST-") {
        FileKind::Manifest
    } else if name == "LOG" || name.starts_with("LOG.old.") {
        FileKind::InfoLog
    } else if name.ends_with(".log") {
        FileKind::Wal
    } else if name.ends_with(".sst") || name.ends_with(".ldb") {
        FileKind::Sst
    } else if name.starts_with("OPTIONS-") {
        FileKind::Options
    } else if name == "LOCK" {
        FileKind::Lock
    } else if name == "IDENTITY" {
        FileKind::Identity
    } else if name.ends_with(".blob") {
        FileKind::Blob
    } else {
        FileKind::Other
    }
}

#[derive(Clone, Debug)]
pub struct DirEntry {
    pub path: PathBuf,
    pub name: String,
    pub kind: FileKind,
    pub size: u64,
    pub is_symlink: bool,
}

pub use crate::limits::MAX_DIR_ENTRIES;

/// Lists regular files directly inside `dir` (no recursion, symlinks reported but not followed).
pub fn scan(dir: &Path) -> std::io::Result<(Vec<DirEntry>, bool)> {
    let mut out = Vec::new();
    let mut truncated = false;
    for e in std::fs::read_dir(dir)? {
        let Ok(e) = e else { continue };
        if out.len() >= MAX_DIR_ENTRIES {
            truncated = true;
            break;
        }
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        let size = if ft.is_symlink() { 0 } else { e.metadata().map(|m| m.len()).unwrap_or(0) };
        out.push(DirEntry { path: e.path(), kind: classify(&name), name, size, is_symlink: ft.is_symlink() });
    }
    out.sort_by(|a, b| a.kind.cmp(&b.kind).then(a.name.cmp(&b.name)));
    Ok((out, truncated))
}

/// Sniffs a file without a recognizable name: SST magic at the end, else log-format guess.
pub fn sniff(path: &Path) -> FileKind {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let k = classify(&name);
    if k != FileKind::Other {
        return k;
    }
    let Ok(r) = crate::io::RangeReader::open(path) else { return FileKind::Other };
    let len = r.len();
    if len >= 48 {
        if let Ok(t) = r.read_at(len - 8, 8) {
            let m = crate::util::fixed64(&t, 0).unwrap_or(0);
            if m == crate::sst::footer::BLOCK_BASED_MAGIC || m == crate::sst::footer::LEGACY_BLOCK_BASED_MAGIC {
                return FileKind::Sst;
            }
        }
    }
    if len >= 7 {
        if let Ok(h) = r.read_at(0, 7) {
            let t = h[6];
            if (1..=11).contains(&t) || t == 130 || t == 131 {
                return FileKind::Wal;
            }
        }
    }
    FileKind::Other
}
