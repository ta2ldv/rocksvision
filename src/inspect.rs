//! File-level facade: opens a file by kind and routes tree/view/byte requests to the parser
//! state (`wal`, `sst`, `manifest`) and its presenter (`present`). All file-system access
//! (opening, sibling lookups such as the MANIFEST named by CURRENT, existence probes) happens
//! here; parsers and presenters only see `&dyn RangeRead` and plain values.

use crate::discover::{FileKind, sniff};
use crate::io::{MAX_READ, RangeRead, RangeReader, read_vec};
use crate::limits::{CURRENT_READ, MAX_WAL_LISTED};
use crate::manifest;
use crate::model::*;
use crate::present::common::{current_view, text_view};
use crate::present::manifest::{Active, LevelsSource, current_invalid, levels_unavailable, levels_view, manifest_missing};
use crate::sst::SstInspector;
use crate::wal::{LogInspector, LogMode, log_number_from_name};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub enum Inspector {
    Log(LogInspector),
    Sst(SstInspector),
    Current { r: Box<dyn RangeRead>, path: PathBuf },
    Text { r: Box<dyn RangeRead>, path: PathBuf, kind: FileKind },
}

/// Existence probe for referenced sibling files (CURRENT -> MANIFEST, MANIFEST → SST).
fn exists(p: &Path) -> bool {
    p.exists()
}

/// WAL files directly in `dir` (the bounded, non-recursive folder listing; empty if it cannot be
/// listed): number from the file name (db/filename.cc "<number>.log"), size and modification
/// time of the listing, nothing scanned yet. At most `MAX_WAL_LISTED` files: the newest.
/// Returns the inventory and the number of WAL files listed (before the cap; the layout note's
/// count). A symlinked WAL is listed but never followed, so it is not scanned.
pub fn wal_inventory(dir: &Path) -> (WalInventory, usize) {
    let Ok((entries, trunc)) = crate::discover::scan(dir) else { return (WalInventory::default(), 0) };
    let wals: Vec<_> = entries.into_iter().filter(|e| e.kind == FileKind::Wal).collect();
    let count = wals.len();
    let (wals, cut) = keep_newest(wals, MAX_WAL_LISTED);
    let files = wals
        .into_iter()
        .map(|e| WalFile {
            number: log_number_from_name(&e.path),
            summary: WalSummary::NotScanned(if e.is_symlink { NotScannedWhy::Symlink } else { NotScannedWhy::Pending }),
            symlink: e.is_symlink,
            path: e.path,
            name: e.name,
            size: e.size,
            mtime: e.mtime,
        })
        .collect();
    (WalInventory { files, truncated: trunc || cut }, count)
}

/// The WALs to list (D3 S2): the `cap` NEWEST numbered files (they may hold unflushed data),
/// then other `*.log` names only while room remains; display order: numbered ascending, then
/// the other names. Returns them and whether any were left out.
fn keep_newest(mut wals: Vec<crate::discover::DirEntry>, cap: usize) -> (Vec<crate::discover::DirEntry>, bool) {
    let numbered = |e: &crate::discover::DirEntry| log_number_from_name(&e.path);
    wals.sort_by(|a, b| (numbered(a).is_none(), numbered(a), &a.name).cmp(&(numbered(b).is_none(), numbered(b), &b.name)));
    let total = wals.len();
    let n_numbered = wals.iter().filter(|e| numbered(e).is_some()).count();
    let drop_numbered = n_numbered.saturating_sub(cap);
    wals.drain(..drop_numbered);
    wals.truncate(cap);
    let cut = wals.len() < total;
    (wals, cut)
}

/// The levels view of a MANIFEST in `dir`, with the folder's WAL inventory (one listing for the
/// layout note's count and the inventory).
fn levels_with_wals(lv: &manifest::Levels, via: String, dir: Option<&Path>, active: Active) -> View {
    let (wals, count) = match dir.map(wal_inventory) {
        Some((w, n)) => (Some(w), Some(n)),
        None => (None, None),
    };
    let src = LevelsSource { via, dir, exists: &exists, wals: count, active };
    let mut v = levels_view(lv, &src);
    v.wals = wals;
    v
}

/// db/filename.cc "<number>.sst" (MakeTableFileName).
fn sst_number_from_name(p: &Path) -> Option<u64> {
    p.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".sst")).and_then(|n| n.parse::<u64>().ok())
}

/// 0030 A7: what the active MANIFEST of the SST's own directory says about its file number —
/// CURRENT names the MANIFEST, whose VersionEdits are replayed with the same bounded replay as
/// the levels view (no directory walk; scoped to this directory and number). Evidence only
/// from a complete replay without problems; otherwise why there is none.
pub fn sst_manifest_context(sst: &Path, cancel: &dyn Fn() -> bool) -> SstManifestContext {
    let (Some(number), Some(dir)) = (sst_number_from_name(sst), sst.parent()) else { return SstManifestContext::Standalone };
    let current = dir.join("CURRENT");
    if !exists(&current) {
        return SstManifestContext::Standalone;
    }
    let Some(mpath) = RangeReader::open(&current).ok().and_then(|cr| current_target_at(&cr, &current).ok()) else {
        return SstManifestContext::Untrusted { manifest: "the MANIFEST named by CURRENT".into() };
    };
    let manifest = mpath.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let Ok(mr) = RangeReader::open(&mpath) else { return SstManifestContext::Untrusted { manifest } };
    let lv = manifest::observed_levels(&mr, cancel);
    if lv.limited || !lv.problems.is_empty() || lv.problems_dropped > 0 {
        return SstManifestContext::Untrusted { manifest };
    }
    let read_at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs());
    let hit = lv.cfs.iter().find_map(|(cf, (name, levels))| levels.iter().find_map(|(level, files)| files.iter().any(|f| f.f.number == number).then(|| (*cf, name.clone(), *level))));
    match hit {
        Some((cf_id, cf_name, level)) => SstManifestContext::Listed(SstManifestEvidence { manifest, number, cf_id, cf_name, level, read_at }),
        None => SstManifestContext::NotListed { manifest, number },
    }
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
            Inspector::Current { .. } if *k == NodeKind::Root => vec![Child::new("Observed levels (via MANIFEST)", NodeKind::ManifestLevels).role(ChildRole::View).leaf()],
            Inspector::Text { .. } if *k == NodeKind::Root => vec![Child::new("Text preview", NodeKind::TextPreview).leaf()],
            _ => Vec::new(),
        }
    }

    pub fn view(&mut self, k: &NodeKind, cancel: &dyn Fn() -> bool) -> View {
        match self {
            Inspector::Log(l) if *k == NodeKind::ManifestLevels && l.mode() == LogMode::Manifest => {
                // A MANIFEST opened directly: active only if a sibling CURRENT names it.
                let path = l.path().to_path_buf();
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let cur = path.parent().map(|d| d.join("CURRENT"));
                let active = match cur.filter(|c| exists(c)) {
                    None => Active::NoCurrent,
                    Some(c) => match RangeReader::open(&c).map_err(|e| format!("cannot read CURRENT: {e}")).and_then(|r| current_target_at(&r, &c)) {
                        Ok(t) if t == path => Active::Yes,
                        Ok(_) => Active::Other,
                        Err(e) => Active::CurrentInvalid(e),
                    },
                };
                levels_with_wals(&manifest::observed_levels(l.reader(), cancel), name, path.parent(), active)
            }
            Inspector::Log(l) => l.view(k, cancel, &exists),
            Inspector::Sst(s) => {
                // 0030 A7: looked up once, in this background job, for the file's own directory
                // and number; a cancelled lookup is not remembered.
                if *k == NodeKind::Root && s.manifest_context().is_none() {
                    let e = sst_manifest_context(s.path(), cancel);
                    if !cancel() {
                        s.set_manifest_context(e);
                    }
                }
                s.view(k, cancel)
            }
            Inspector::Current { r, path } => {
                let target = current_target_at(&**r, path);
                if *k == NodeKind::ManifestLevels {
                    return match &target {
                        Ok(p) => {
                            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            match RangeReader::open(p) {
                                Ok(mr) => levels_with_wals(&manifest::observed_levels(&mr, cancel), format!("CURRENT -> {name}"), p.parent(), Active::Yes),
                                Err(e) => levels_unavailable(Status::NotPresent, manifest_missing(&name), Some(format!("referenced MANIFEST unavailable: {e}"))),
                            }
                        }
                        Err(e) => levels_unavailable(Status::Invalid, current_invalid(e), None),
                    };
                }
                let raw = read_vec(&**r, 0, CURRENT_READ.min(r.len())).unwrap_or_default();
                current_view(
                    &raw,
                    target.map(|p| {
                        let ex = exists(&p);
                        (p, ex)
                    }),
                )
            }
            Inspector::Text { r, kind, .. } => text_view(&**r, *kind),
        }
    }

    /// Canonical tree path of `target`: the ancestors below the root, outermost first (the
    /// target itself excluded). None when the target has no node in this file's tree (or the
    /// bounded search did not find it).
    pub fn path_to(&mut self, target: &NodeKind, cancel: &dyn Fn() -> bool) -> Option<Vec<NodeKind>> {
        match self {
            Inspector::Log(l) => l.path_to(target, cancel),
            Inspector::Sst(s) => s.path_to(target, cancel),
            _ if matches!(target, NodeKind::Root | NodeKind::ManifestLevels | NodeKind::TextPreview) => Some(Vec::new()),
            _ => None,
        }
    }

    /// The structure that holds byte `off` of `space` and what to select in its view: (node,
    /// "select the narrowest field at this offset", structure span when no field applies).
    /// File bytes only; derived spaces are resolved by the view that shows them.
    pub fn locate(&mut self, space: Space, off: u64, cancel: &dyn Fn() -> bool) -> Option<Located> {
        match (self, space) {
            (Inspector::Log(l), Space::File) => l.locate(off),
            (Inspector::Sst(s), Space::File) => s.locate(off, cancel),
            (Inspector::Log(_), Space::Record(o)) => Some((NodeKind::LogRecord(o), Some((space, off)), None)),
            _ => None,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// D3 S2: over the cap, the newest numbered WALs are kept (in ascending order), unnumbered
    /// names only while room remains, and the listing says it was truncated.
    #[test]
    fn wal_listing_keeps_newest_when_truncated() {
        let e = |name: &str| crate::discover::DirEntry { path: PathBuf::from("/db").join(name), name: name.into(), kind: FileKind::Wal, size: 1, mtime: None, is_symlink: false };
        let mut v: Vec<_> = (1..=8u64).rev().map(|n| e(&format!("{n:06}.log"))).collect();
        v.push(e("odd.log"));
        let (kept, cut) = keep_newest(v.clone(), 5);
        assert!(cut);
        assert_eq!(kept.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), vec!["000004.log", "000005.log", "000006.log", "000007.log", "000008.log"]);
        let (kept, cut) = keep_newest(v.clone(), 20);
        assert!(!cut);
        assert_eq!((kept.len(), kept.last().map(|x| x.name.as_str())), (9, Some("odd.log")), "unnumbered names last, within the cap");
        let (kept, cut) = keep_newest(v, 8);
        assert!(cut && kept.first().map(|x| x.name.as_str()) == Some("000001.log") && kept.len() == 8, "room for every numbered file: the odd name is the one left out");
    }
}
