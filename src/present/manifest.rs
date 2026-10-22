//! MANIFEST presentation: VersionEdit element explanations and the observed-levels table.

use super::common::explained;
use crate::manifest::{ikey_preview, Levels, MAX_LIVE_FILES};
use crate::model::*;
use crate::util::human_size;

/// VersionEdit facts as table fields (db/version_edit.h, db/version_edit.cc DecodeFrom).
pub fn edit_fields(facts: &[Fact]) -> Vec<Field> {
    facts
        .iter()
        .cloned()
        .map(|f| {
            // Selected by kind, never by the display label.
            let e = match f.kind {
                FieldKind::Tag => "VersionEdit tag (db/version_edit.h enum Tag).",
                FieldKind::UnknownCustomField => "Unknown custom field (bit 6 set = must-understand).",
                FieldKind::IgnorableField if f.status == Status::Unknown => "Unknown tag with kTagSafeIgnoreMask set: RocksDB skips it.",
                FieldKind::Remainder => "Unknown tag without the safe-ignore bit: RocksDB fails with Corruption; framing of the rest is unknown.",
                _ => "",
            };
            explained(f, e)
        })
        .collect()
}

/// `exists` probes referenced SST files on disk (supplied by the file facade).
pub fn levels_view(lv: &Levels, dir: Option<&std::path::Path>, exists: &dyn Fn(&std::path::Path) -> bool) -> View {
    let mut rows = Vec::new();
    let mut lsm = Vec::new();
    for (cf, (name, levels)) in &lv.cfs {
        for (level, files) in levels {
            for lf in files {
                let f = &lf.f;
                let path = dir.map(|d| d.join(format!("{:06}.sst", f.number)));
                let exists = path.as_ref().map(|p| exists(p)).unwrap_or(false);
                let st = if exists { Status::Present } else { Status::NotPresent };
                let overlap = if *level == 0 {
                    format!("{} in L0 (allowed), {} in L1", lf.overlap_same, lf.overlap_next)
                } else {
                    format!("{} same level{}, {} in L{}", lf.overlap_same, if lf.overlap_same > 0 { " (unexpected)" } else { "" }, lf.overlap_next, level + 1)
                };
                let mut fld = Field::new(format!("{:06}.sst", f.number), None, format!("CF {name} L{level} {}", human_size(f.size))).status(st).explain(format!(
                    "Observed from MANIFEST: live after replaying all edits. Key range by user key (bytewise comparator assumed). File on disk: {}.",
                    if exists { "present (double-click to open)" } else { "not found in this directory" }
                ));
                if let (Some(p), true) = (path, exists) {
                    fld = fld.open(p);
                }
                lsm.push(LsmFile {
                    row: rows.len(),
                    cf: format!("{cf} {name}"),
                    level: format!("L{level}"),
                    file: format!("{:06}", f.number),
                    size: f.size.to_string(),
                    smallest: ikey_preview(&f.smallest),
                    largest: ikey_preview(&f.largest),
                    overlap: overlap.clone(),
                    status: st,
                });
                rows.push(Row {
                    cells: vec![
                        format!("{cf} {name}"),
                        format!("L{level}"),
                        format!("{:06}", f.number),
                        f.size.to_string(),
                        ikey_preview(&f.smallest),
                        ikey_preview(&f.largest),
                        match (f.smallest_seq, f.largest_seq) {
                            (Some(a), Some(b)) => format!("{a}..{b}"),
                            _ => "-".into(),
                        },
                        overlap,
                        format!("{} {}", if exists { "✔" } else { "○" }, if exists { "on disk" } else { "missing" }),
                    ],
                    field: fld,
                });
            }
        }
    }
    let mut notes = vec![format!("Observed from MANIFEST: {} edits replayed, {} file additions, {} deletions. This is the final observed state, not compaction history.", lv.edits, lv.adds, lv.deletes)];
    if let Some(c) = &lv.comparator {
        notes.push(format!("Comparator: {c}"));
    }
    if lv.limited {
        notes.push(format!("Resource limit: more than {} live files; listing truncated.", MAX_LIVE_FILES));
    }
    for p in lv.problems.iter().take(10) {
        notes.push(format!("Problem: {p}"));
    }
    let more = lv.problems.len().saturating_sub(10) as u64 + lv.problems_dropped;
    if more > 0 {
        notes.push(format!("… {more} more problems not listed"));
    }
    use ColumnRole as R;
    View {
        title: "Observed levels (from MANIFEST)".into(),
        columns: vec![
            col(R::CfName, "CF"),
            col(R::Level, "Level"),
            col(R::FileNo, "File"),
            col(R::Size, "Size"),
            col(R::Key, "Smallest"),
            col(R::Key, "Largest"),
            col(R::Seq, "Seq range"),
            col(R::Summary, "Overlap"),
            col(R::Presence, "Disk"),
        ],
        rows,
        notes: notes.into_iter().map(Note::from).collect(),
        lsm,
        diagnostics: lv.diagnostics.clone(),
        ..Default::default()
    }
}
