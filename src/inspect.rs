//! File-level facade: opens a file by kind and routes tree/view/byte requests to the parser
//! state (`wal`, `sst`, `manifest`) and its presenter (`present`). All file-system access
//! (opening, sibling lookups such as the MANIFEST named by CURRENT, existence probes) happens
//! here; parsers and presenters only see `&dyn RangeRead` and plain values.

use crate::discover::{sniff, FileKind};
use crate::io::{read_vec, RangeRead, RangeReader, MAX_READ};
use crate::limits::CURRENT_READ;
use crate::manifest;
use crate::model::*;
use crate::present::common::{current_view, error_view, text_view};
use crate::present::manifest::levels_view;
use crate::sst::SstInspector;
use crate::wal::{LogInspector, LogMode};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub enum Inspector {
    Log(LogInspector),
    Sst(SstInspector),
    Current { r: Box<dyn RangeRead>, path: PathBuf },
    Text { r: Box<dyn RangeRead>, path: PathBuf, kind: FileKind },
}

/// Existence probe for referenced sibling files (CURRENT → MANIFEST, MANIFEST → SST).
fn exists(p: &Path) -> bool {
    p.exists()
}

/// Opens any supported file read-only. Unknown names are sniffed (SST magic / log header).
pub fn open(path: &Path) -> Result<Inspector, String> {
    let r = RangeReader::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    Ok(match sniff(path) {
        FileKind::Wal => Inspector::Log(LogInspector::new(r, LogMode::Wal)),
        FileKind::Manifest => Inspector::Log(LogInspector::new(r, LogMode::Manifest)),
        FileKind::Sst => Inspector::Sst(SstInspector::open(r)),
        FileKind::Current => Inspector::Current { r: Box::new(r), path: path.to_path_buf() },
        k => Inspector::Text { r: Box::new(r), path: path.to_path_buf(), kind: k },
    })
}

impl Inspector {
    pub fn reader(&self) -> &dyn RangeRead {
        match self {
            Inspector::Log(l) => l.reader(),
            Inspector::Sst(s) => s.reader(),
            Inspector::Current { r, .. } | Inspector::Text { r, .. } => &**r,
        }
    }

    pub fn path(&self) -> &Path {
        match self {
            Inspector::Log(l) => l.path(),
            Inspector::Sst(s) => s.path(),
            Inspector::Current { path, .. } | Inspector::Text { path, .. } => path,
        }
    }

    /// Size observed at open time.
    pub fn file_len(&self) -> u64 {
        self.reader().len()
    }

    /// Modification time observed at open time.
    pub fn mtime(&self) -> Option<SystemTime> {
        self.reader().modified().ok()
    }

    pub fn kind_label(&self) -> &'static str {
        match self {
            Inspector::Log(l) if l.mode() == LogMode::Wal => "WAL",
            Inspector::Log(_) => "MANIFEST",
            Inspector::Sst(_) => "SST",
            Inspector::Current { .. } => "CURRENT",
            Inspector::Text { kind, .. } => kind.label(),
        }
    }

    pub fn children(&mut self, k: &NodeKind, cancel: &dyn Fn() -> bool) -> Vec<Child> {
        match self {
            Inspector::Log(l) => l.children(k),
            Inspector::Sst(s) => s.children(k, cancel),
            Inspector::Current { .. } if *k == NodeKind::Root => vec![Child::new("Observed levels (via MANIFEST)", NodeKind::ManifestLevels).leaf()],
            Inspector::Text { .. } if *k == NodeKind::Root => vec![Child::new("Text preview", NodeKind::TextPreview).leaf()],
            _ => Vec::new(),
        }
    }

    pub fn view(&mut self, k: &NodeKind, cancel: &dyn Fn() -> bool) -> View {
        match self {
            Inspector::Log(l) => l.view(k, cancel, &exists),
            Inspector::Sst(s) => s.view(k, cancel),
            Inspector::Current { r, path } => {
                let target = current_target_at(&**r, path);
                if *k == NodeKind::ManifestLevels {
                    return match &target {
                        Ok(p) => match RangeReader::open(p) {
                            Ok(mr) => levels_view(&manifest::observed_levels(&mr, cancel), p.parent(), &exists),
                            Err(e) => error_view("Observed levels", Status::NotPresent, format!("referenced MANIFEST unavailable: {e}")),
                        },
                        Err(e) => error_view("Observed levels", Status::Invalid, e.clone()),
                    };
                }
                let raw = read_vec(&**r, 0, CURRENT_READ.min(r.len())).unwrap_or_default();
                current_view(&raw, target.map(|p| {
                    let ex = exists(&p);
                    (p, ex)
                }))
            }
            Inspector::Text { r, kind, .. } => text_view(&**r, *kind),
        }
    }

    /// Bytes of an offset space, bounded by `len`.
    pub fn space_bytes(&mut self, space: Space, off: u64, len: u64) -> Option<Vec<u8>> {
        let len = len.min(MAX_READ);
        match (self, space) {
            (Inspector::Sst(s), sp) => s.space_bytes(sp, off, len),
            (Inspector::Log(l), Space::Record(o)) => {
                let rec = l.record(o)?;
                let s = (off as usize).min(rec.data.len());
                let e = (off.saturating_add(len) as usize).min(rec.data.len());
                Some(rec.data[s..e].to_vec())
            }
            (me, Space::File) => read_vec(me.reader(), off, len).ok(),
            _ => None,
        }
    }

    pub fn space_size(&mut self, space: Space) -> u64 {
        match (self, space) {
            (me, Space::File) => me.reader().len(),
            (Inspector::Log(l), Space::Record(o)) => l.record(o).map(|r| r.data.len() as u64).unwrap_or(0),
            (Inspector::Sst(s), Space::Block(o)) => s.space_bytes(Space::Block(o), 0, u64::MAX >> 1).map(|b| b.len() as u64).unwrap_or(0),
            _ => 0,
        }
    }
}

/// Shim for the former file-only signature.
pub fn current_target(r: &RangeReader) -> Result<PathBuf, String> {
    current_target_at(r, r.path())
}

/// MANIFEST path named by a CURRENT file at `path` (sibling in the same directory).
pub fn current_target_at(r: &dyn RangeRead, path: &Path) -> Result<PathBuf, String> {
    let b = read_vec(r, 0, CURRENT_READ.min(r.len())).map_err(|e| e.to_string())?;
    let name = manifest::parse_current(&b)?;
    Ok(path.parent().map(|d| d.join(&name)).unwrap_or_else(|| PathBuf::from(name)))
}
