//! MANIFEST presentation: VersionEdit element explanations, the typed LSM inventory and the
//! observed-levels table derived from it (one source: `inventory`).

use super::common::explained;
use crate::format::ikey::user_key;
use crate::manifest::{Levels, LiveFile, MAX_LIVE_FILES, ikey_preview};
use crate::model::*;
use crate::util::human_size;
use std::path::Path;

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

/// Where the observed levels come from (supplied by the file facade, which owns all I/O).
pub struct LevelsSource<'a> {
    /// "CURRENT -> MANIFEST-000005" or "MANIFEST-000005".
    pub via: String,
    /// Folder of the MANIFEST (referenced SSTs are looked up there).
    pub dir: Option<&'a Path>,
    /// Existence probe for referenced SSTs.
    pub exists: &'a dyn Fn(&Path) -> bool,
    /// WAL files in the folder (None: not looked at).
    pub wals: Option<usize>,
    /// Whether this MANIFEST is the one CURRENT names.
    pub active: Active,
}

/// What the CURRENT next to a MANIFEST says about it.
#[derive(Clone, Debug, PartialEq)]
pub enum Active {
    /// CURRENT names this MANIFEST.
    Yes,
    /// No CURRENT next to it.
    NoCurrent,
    /// CURRENT names another MANIFEST.
    Other,
    /// CURRENT exists but cannot be read or parsed (the reason).
    CurrentInvalid(String),
}

pub use crate::model::wal_words;

/// Distinct boundary keys of the given intervals in bytewise order (the rank scale).
fn boundary_keys<'a>(files: impl Iterator<Item = &'a LiveFile>) -> Vec<Vec<u8>> {
    let mut keys: Vec<Vec<u8>> = files.flat_map(|lf| [user_key(&lf.f.smallest).to_vec(), user_key(&lf.f.largest).to_vec()]).collect();
    keys.sort_unstable();
    keys.dedup();
    keys
}

/// Slots [start, end) of a file on the CF's rank scale in comparator order. Bytewise: rank of
/// the smallest .. rank of the largest + 1; reverse bytewise: the same ranks mirrored (larger
/// keys first). Inconsistent ranges (smallest after largest) are normalized.
fn rank_slots(keys: &[Vec<u8>], f: &LiveFile, reverse: bool) -> (u32, u32) {
    let n = keys.len() as u32;
    let rank = |k: &[u8]| keys.binary_search_by(|x| x.as_slice().cmp(k)).unwrap_or_else(|i| i) as u32;
    let (a, b) = (rank(user_key(&f.f.smallest)), rank(user_key(&f.f.largest)));
    let (a, b) = if reverse { (n.saturating_sub(1).saturating_sub(a), n.saturating_sub(1).saturating_sub(b)) } else { (a, b) };
    (a.min(b), a.max(b) + 1)
}

/// Greedy interval colouring: each file (sorted by start) goes to the first lane whose last end
/// is at or before its start. Returns the lane per file (input order) and the lane count.
/// O(n log n): a min-heap of (end, lane) of the busy lanes and a min-heap of free lanes, so the
/// result is the same lowest-free-lane colouring as a linear scan of lane ends.
pub fn assign_lanes(slots: &[(u32, u32)]) -> (Vec<u32>, u32) {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;
    let mut order: Vec<usize> = (0..slots.len()).collect();
    order.sort_by_key(|&i| (slots[i].0, slots[i].1));
    let mut busy: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();
    let mut free: BinaryHeap<Reverse<u32>> = BinaryHeap::new();
    let mut lanes = 0u32;
    let mut lane = vec![0u32; slots.len()];
    for i in order {
        let (s, e) = slots[i];
        while let Some(&Reverse((end, l))) = busy.peek() {
            if end > s {
                break;
            }
            busy.pop();
            free.push(Reverse(l));
        }
        let l = match free.pop() {
            Some(Reverse(l)) => l,
            None => {
                lanes += 1;
                lanes - 1
            }
        };
        lane[i] = l;
        busy.push(Reverse((e, l)));
    }
    (lane, lanes)
}

/// The typed inventory (built once per view): per CF its comparator order, every level from L0
/// to the highest observed one (empty levels included), files with user-key bounds, previews,
/// overlap, on-disk status, table row, open path, rank slots and lanes.
pub fn inventory(lv: &Levels, src: &LevelsSource) -> LsmInventory {
    let mut cfs = Vec::new();
    let mut row = 0usize;
    for (cf, (name, levels)) in &lv.cfs {
        let comparator = lv.cf_comparators.get(cf).cloned();
        let order = KeyOrder::from_comparator(comparator.as_deref());
        let reverse = order == KeyOrder::ReverseBytewise;
        let keys = if order.ordered() { boundary_keys(levels.values().flatten()) } else { Vec::new() };
        // Empty levels are filled only up to the highest level ≤ MAX_LEVELS (`observed_levels`
        // never places a file above it; this bound holds for any `Levels`).
        let max = levels.keys().copied().filter(|l| *l <= crate::limits::MAX_LEVELS).max();
        let mut out_levels = Vec::new();
        for level in max.map(|m| 0..=m).into_iter().flatten() {
            let files: &[LiveFile] = levels.get(&level).map(|v| v.as_slice()).unwrap_or(&[]);
            let slots: Vec<(u32, u32)> = if order.ordered() { files.iter().map(|f| rank_slots(&keys, f, reverse)).collect() } else { (0..files.len() as u32).map(|i| (i, i + 1)).collect() };
            let (lanes, nlanes) = if order.ordered() { assign_lanes(&slots) } else { (vec![0; files.len()], u32::from(!files.is_empty())) };
            let mut out = LsmLevel {
                level,
                scale: if order.ordered() { keys.len() as u32 } else { files.len() as u32 },
                lanes: nlanes,
                overlaps: order.ordered() && level > 0 && nlanes > 1,
                ..Default::default()
            };
            for (i, lf) in files.iter().enumerate() {
                let f = &lf.f;
                let path = src.dir.map(|d| d.join(format!("{:06}.sst", f.number)));
                let on_disk = path.as_ref().is_some_and(|p| (src.exists)(p));
                out.bytes += f.size;
                out.on_disk += usize::from(on_disk);
                out.files.push(LsmFile {
                    number: f.number,
                    size: f.size,
                    smallest: user_key(&f.smallest).to_vec(),
                    largest: user_key(&f.largest).to_vec(),
                    smallest_preview: ikey_preview(&f.smallest),
                    largest_preview: ikey_preview(&f.largest),
                    seq: f.smallest_seq.zip(f.largest_seq),
                    overlap_same: lf.overlap_same_known.then_some(lf.overlap_same),
                    overlap_next: lf.overlap_next_known.then_some(lf.overlap_next),
                    status: if on_disk { Status::Present } else { Status::NotPresent },
                    row,
                    path: path.filter(|_| on_disk),
                    slots: slots[i],
                    lane: lanes[i],
                });
                row += 1;
            }
            out_levels.push(out);
        }
        cfs.push(LsmCf { id: *cf, name: name.clone(), levels: out_levels, comparator, order });
    }
    LsmInventory { cfs, truncated: lv.limited, log_numbers: log_numbers(lv, src) }
}

/// The replay's log numbers as WAL lifecycle evidence (trusted only for the active MANIFEST).
fn log_numbers(lv: &Levels, src: &LevelsSource) -> ManifestLogNumbers {
    ManifestLogNumbers {
        // Only live CFs (D5 nit 2): a kLogNumber for an id never added is not evidence.
        per_cf: lv.cf_log_numbers.iter().filter(|(c, _)| lv.live_cfs.contains(c)).map(|(c, l)| (*c, *l)).collect(),
        without: lv.live_cfs.iter().copied().filter(|c| !lv.cf_log_numbers.contains_key(c)).collect(),
        min_to_keep: lv.min_log_to_keep,
        prev: lv.prev_log,
        active: src.active == Active::Yes,
    }
}

/// The table row of one inventory file (cells derived from the typed values).
fn row_of(cf: &LsmCf, level: &LsmLevel, f: &LsmFile) -> Row {
    let on_disk = f.on_disk();
    // Meaning only (0030 UI final §5.2): key order, on-disk state and the open hint are the LSM
    // file card's facts / hint, so they are not repeated here.
    let mut fld = Field::new(f.name(), None, format!("CF {} L{} {}", cf.name, level.level, human_size(f.size))).status(f.status).explain("Live SST file per the MANIFEST (after replaying all edits).");
    fld.no_bytes_source = Some(NO_BYTES_MANIFEST.into());
    if let Some(p) = &f.path {
        fld = fld.open(p.clone());
    }
    Row {
        cells: vec![
            format!("{} {}", cf.id, cf.name),
            format!("L{}", level.level),
            format!("{:06}", f.number),
            f.size.to_string(),
            f.smallest_preview.clone(),
            f.largest_preview.clone(),
            f.seq.map(|(a, b)| format!("{a}..{b}")).unwrap_or_else(|| "-".into()),
            LsmInventory::overlap_text(cf, level, f),
            format!("{} {}", if on_disk { "✔" } else { "○" }, if on_disk { "on disk" } else { "missing" }),
        ],
        field: fld,
    }
}

/// "Layout: CURRENT -> MANIFEST-000005 · 7 live SST · all on disk · no WAL (…)" and whether a
/// referenced SST is missing.
pub fn layout_note(inv: &LsmInventory, src: &LevelsSource) -> Note {
    let total: usize = inv.cfs.iter().map(|c| c.files()).sum();
    let on_disk: usize = inv.cfs.iter().flat_map(|c| c.levels.iter()).map(|l| l.on_disk).sum();
    let missing = total - on_disk;
    let mut parts = vec![format!("Layout: {}", src.via), format!("{total} live SST")];
    if total > 0 {
        parts.push(if missing == 0 { "all on disk".into() } else { format!("{missing} of {total} missing on disk") });
    }
    if let Some(w) = src.wals {
        parts.push(wal_words(w));
    }
    let text = parts.join(" · ");
    if missing > 0 { Note::with_status(Status::NotPresent, text) } else { Note::from(text) }
}

/// The observed-levels view: notes, the table rows (from the inventory) and the inventory.
pub fn levels_view(lv: &Levels, src: &LevelsSource) -> View {
    let lsm = inventory(lv, src);
    let mut rows = Vec::new();
    for cf in &lsm.cfs {
        for level in &cf.levels {
            for f in &level.files {
                rows.push(row_of(cf, level, f));
            }
        }
    }
    let mut notes = vec![layout_note(&lsm, src)];
    match &src.active {
        Active::NoCurrent => notes.push(Note::with_status(Status::Unknown, "Not confirmed as the active MANIFEST (no CURRENT).")),
        Active::Other => notes.push(Note::with_status(Status::Unknown, "Not the active MANIFEST: CURRENT names another one.")),
        // CURRENT exists but cannot be read or parsed: say that, never "names another one".
        Active::CurrentInvalid(e) => notes.push(Note::with_status(Status::Invalid, format!("Not confirmed as the active MANIFEST: {}", current_invalid(e)))),
        Active::Yes => {}
    }
    notes.push(format!("Observed from MANIFEST: {} edits replayed, {} file additions, {} deletions. This is the final observed state, not compaction history.", lv.edits, lv.adds, lv.deletes).into());
    for line in lsm.comparator_lines() {
        notes.push(line.into());
    }
    if lv.limited {
        notes.push(Note::with_status(Status::Limited, format!("Resource limit: more than {} live files; listing truncated.", MAX_LIVE_FILES)));
    }
    for p in lv.problems.iter().take(10) {
        notes.push(format!("Problem: {p}").into());
    }
    let more = lv.problems.len().saturating_sub(10) as u64 + lv.problems_dropped;
    if more > 0 {
        notes.push(format!("… {more} more problems not listed").into());
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
        notes,
        lsm,
        diagnostics: lv.diagnostics.clone(),
        ..Default::default()
    }
}

/// The one sentence for a CURRENT that names a missing MANIFEST (UI advice §1.6): the levels
/// view, the CURRENT root view and the LSM Map's disabled reason share it.
pub fn manifest_missing(name: &str) -> String {
    format!("CURRENT names {name}, which is not in this folder. The live SST files cannot be observed.")
}
/// The one sentence for an unreadable / invalid CURRENT.
pub fn current_invalid(reason: &str) -> String {
    format!("CURRENT is invalid: {reason}. The active MANIFEST is unknown.")
}

/// Levels unavailable: an error view whose message is the shared reason (`manifest_missing` /
/// `current_invalid`), typed in `View::lsm_blocked`; an OS error is only a secondary note.
pub fn levels_unavailable(status: Status, why: String, detail: Option<String>) -> View {
    let mut v = super::common::error_view("Observed levels", status, why.clone());
    if let Some(d) = detail {
        v.notes.push(Note::from(format!("Detail: {d}")));
    }
    v.lsm_blocked = Some(why);
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::NewFile;
    use std::collections::BTreeMap;

    fn ik(k: &str) -> Vec<u8> {
        let mut v = k.as_bytes().to_vec();
        v.extend_from_slice(&((7u64 << 8) | 1).to_le_bytes());
        v
    }
    fn lf(number: u64, level: u32, a: &str, b: &str) -> LiveFile {
        LiveFile {
            f: NewFile { cf: 0, level, number, size: 1000 * number, smallest: ik(a), largest: ik(b), smallest_seq: Some(1), largest_seq: Some(2) },
            overlap_same: 0,
            overlap_next: 0,
            overlap_same_known: true,
            overlap_next_known: true,
        }
    }
    fn levels(cmp: Option<&str>, files: Vec<LiveFile>) -> Levels {
        let mut by: BTreeMap<u32, Vec<LiveFile>> = BTreeMap::new();
        for f in files {
            by.entry(f.f.level).or_default().push(f);
        }
        let mut lv = Levels::default();
        lv.cfs.insert(0, ("default".into(), by));
        if let Some(c) = cmp {
            lv.cf_comparators.insert(0, c.into());
        }
        lv
    }
    fn src<'a>(exists: &'a dyn Fn(&Path) -> bool) -> LevelsSource<'a> {
        LevelsSource { via: "CURRENT -> MANIFEST-000005".into(), dir: Some(Path::new("/db")), exists, wals: Some(0), active: Active::Yes }
    }

    #[test]
    fn key_order_from_comparator_names() {
        assert_eq!(KeyOrder::from_comparator(Some("leveldb.BytewiseComparator")), KeyOrder::Bytewise);
        assert_eq!(KeyOrder::from_comparator(Some("rocksdb.ReverseBytewiseComparator")), KeyOrder::ReverseBytewise);
        assert_eq!(KeyOrder::from_comparator(Some("my.Comparator")), KeyOrder::Unsupported("my.Comparator".into()));
        assert_eq!(KeyOrder::from_comparator(None), KeyOrder::Unknown);
        assert!(KeyOrder::Bytewise.ordered() && KeyOrder::ReverseBytewise.ordered());
        assert!(!KeyOrder::Unknown.ordered() && !KeyOrder::Unsupported("x".into()).ordered());
        assert!(KeyOrder::Unsupported("my.C".into()).legend().contains("'my.C' is not supported, so key order, overlap and gaps are not shown"));
    }

    #[test]
    fn ranks_lanes_and_mirroring() {
        let all = |_: &Path| true;
        let files = vec![lf(1, 0, "a", "c"), lf(2, 0, "b", "d"), lf(3, 0, "e", "f"), lf(4, 1, "a", "b"), lf(5, 1, "c", "f")];
        let inv = inventory(&levels(Some("leveldb.BytewiseComparator"), files.clone()), &src(&all));
        let l0 = &inv.cfs[0].levels[0];
        // keys a b c d e f: 6 slots; file 1 [0,3), 2 [1,4), 3 [4,6)
        assert_eq!(l0.scale, 6);
        let slots: Vec<(u32, u32)> = l0.files.iter().map(|f| f.slots).collect();
        assert_eq!(slots, vec![(0, 3), (1, 4), (4, 6)]);
        assert_eq!((l0.lanes, l0.files.iter().map(|f| f.lane).collect::<Vec<_>>()), (2, vec![0, 1, 0]), "overlapping L0 files take two lanes");
        assert!(!inv.cfs[0].levels[1].overlaps && inv.cfs[0].levels[1].lanes == 1);
        // Reverse bytewise: mirrored (larger keys on the left).
        let rev: Vec<LiveFile> = vec![lf(1, 0, "c", "a"), lf(3, 0, "f", "e")];
        let inv = inventory(&levels(Some("rocksdb.ReverseBytewiseComparator"), rev), &src(&all));
        let s: Vec<(u32, u32)> = inv.cfs[0].levels[0].files.iter().map(|f| (f.number as u32, f.slots.0)).collect();
        // keys a c e f: f/e rank 0..1 on the left, c/a 2..3 right
        assert_eq!(s.iter().find(|x| x.0 == 3).unwrap().1, 0);
        assert_eq!(s.iter().find(|x| x.0 == 1).unwrap().1, 2);
        // Unsupported: number order, equal width, one lane, overlap not computed.
        let mut un = files;
        for f in &mut un {
            f.overlap_same_known = false;
            f.overlap_next_known = false;
        }
        let inv = inventory(&levels(Some("my.Comparator"), un), &src(&all));
        let l0 = &inv.cfs[0].levels[0];
        assert_eq!((l0.scale, l0.lanes, l0.overlaps), (3, 1, false));
        assert_eq!(l0.files.iter().map(|f| f.slots).collect::<Vec<_>>(), vec![(0, 1), (1, 2), (2, 3)]);
        let (c, l, f) = inv.by_row(0).unwrap();
        assert_eq!(LsmInventory::overlap_text(c, l, f), "not computed (comparator my.Comparator)");
    }

    #[test]
    fn levels_view_notes_and_missing_files() {
        let some = |p: &Path| !p.ends_with("000002.sst");
        let lv = levels(Some("leveldb.BytewiseComparator"), vec![lf(1, 0, "a", "c"), lf(2, 2, "b", "d")]);
        let v = levels_view(&lv, &src(&some));
        assert_eq!(v.notes[0].text, "Layout: CURRENT -> MANIFEST-000005 · 2 live SST · 1 of 2 missing on disk · no WAL (normal for a checkpoint or a flushed DB)");
        assert_eq!(v.notes[0].status, Some(Status::NotPresent));
        assert_eq!(v.rows.len(), 2);
        assert_eq!(v.lsm.cfs[0].levels.len(), 3, "L0..L2 incl. the empty L1");
        assert!(v.lsm.cfs[0].levels[1].files.is_empty());
        let (_, _, f) = v.lsm.by_row(1).unwrap();
        assert_eq!((f.number, f.on_disk(), f.path.is_none()), (2, false, true));
        assert!(v.rows[1].field.open_path.is_none() && v.rows[1].cells[8].contains("missing"));
        let all = |_: &Path| true;
        let v = levels_view(&lv, &LevelsSource { active: Active::NoCurrent, wals: None, ..src(&all) });
        assert!(v.notes[0].text.ends_with("2 live SST · all on disk"));
        assert_eq!(v.notes[1].text, "Not confirmed as the active MANIFEST (no CURRENT).");
        assert!(!v.lsm.log_numbers.active, "no lifecycle evidence from an unconfirmed MANIFEST");
        // D3 (reviewer nit 6): an unreadable / invalid CURRENT is said as such.
        let v = levels_view(&lv, &LevelsSource { active: Active::CurrentInvalid("CURRENT is not UTF-8 text".into()), ..src(&all) });
        assert_eq!(v.notes[1].text, "Not confirmed as the active MANIFEST: CURRENT is invalid: CURRENT is not UTF-8 text. The active MANIFEST is unknown.");
        assert_eq!(v.notes[1].status, Some(Status::Invalid));
    }

    /// 0026 D5 nit 2: a kLogNumber recorded for a CF id that was never added (not live) is not
    /// lifecycle evidence.
    #[test]
    fn log_numbers_only_for_live_cfs() {
        let all = |_: &Path| true;
        let mut lv = levels(Some("leveldb.BytewiseComparator"), vec![lf(1, 0, "a", "c")]);
        lv.live_cfs = [0].into_iter().collect();
        lv.cf_log_numbers = BTreeMap::from([(0, 5), (7, 9)]);
        let ln = inventory(&lv, &src(&all)).log_numbers;
        assert_eq!(ln.per_cf, BTreeMap::from([(0, 5)]));
        assert!(ln.without.is_empty());
    }

    /// 0026 D4: a CF with files in L0 and L2 but none in L1 (no GUI fixture has one): the
    /// inventory keeps the empty L1, the map lays it out as a title-only frame, and its title
    /// says `L1 · empty (0 files in MANIFEST)`; the other titles never say empty.
    #[test]
    fn empty_intermediate_level_is_a_labelled_title_only_frame() {
        use crate::app::lsm_map::{Geometry, layout, title_parts};
        let all = |_: &Path| true;
        let lv = levels(Some("leveldb.BytewiseComparator"), vec![lf(1, 0, "a", "c"), lf(2, 0, "b", "d"), lf(3, 2, "a", "d")]);
        let inv = inventory(&lv, &src(&all));
        assert_eq!(inv.cfs[0].levels.iter().map(|l| (l.level, l.files.len())).collect::<Vec<_>>(), vec![(0, 2), (1, 0), (2, 1)]);
        let g = Geometry { width: 400.0, lane_h: 20.0, lane_gap: 2.0, title_h: 10.0, pad: 5.0, row_gap: 4.0, min_w: 3.0, dense_lane_h: 12.0, visible: [0.0, 0.0, 400.0, 1000.0] };
        let map = layout(&inv, Some(0), (0.0, 0.0), 1.0, &g);
        let text = |i: usize| title_parts(&map.rows[i], true, 0).into_iter().map(|p| p.0).collect::<String>();
        assert_eq!(text(1), "L1 · empty (0 files in MANIFEST)");
        assert_eq!((map.rows[1].lane_rows, map.rows[1].y1 - map.rows[1].y0), (0, 20.0), "title bar only");
        assert!(!text(0).contains("empty") && !text(2).contains("empty"));
        assert!(map.boxes.iter().all(|b| b.level != 1), "no box in the empty level");
    }

    /// 0024 D3 (reviewer blocker): hostile levels in a MANIFEST: the view lists the valid files,
    /// says what was not placed, and the inventory/map materialise at most MAX_LEVELS + 1 levels.
    #[test]
    fn hostile_levels_view_and_map_stay_bounded() {
        let max = crate::limits::MAX_LEVELS;
        let bytes = crate::manifest::tests::manifest_bytes(&[(0, 9), (u32::MAX, 10), (max + 1, 11), (3, 12)]);
        let lv = crate::manifest::observed_levels(&crate::io::MemReader::new(bytes), &|| false);
        let all = |_: &Path| true;
        let v = levels_view(&lv, &src(&all));
        assert_eq!(v.rows.iter().map(|r| r.cells[2].clone()).collect::<Vec<_>>(), vec!["000009", "000012"]);
        assert_eq!(v.lsm.cfs[0].levels.len(), 4, "L0..L3 only");
        assert_eq!(v.notes.iter().filter(|n| n.text.starts_with("Problem:") && n.text.contains("not placed in the levels")).count(), 2);
        let map = crate::app::lsm_map::layout(
            &v.lsm,
            Some(0),
            (0.0, 0.0),
            1.0,
            &crate::app::lsm_map::Geometry { width: 400.0, lane_h: 20.0, lane_gap: 2.0, title_h: 10.0, pad: 5.0, row_gap: 4.0, min_w: 3.0, dense_lane_h: 12.0, visible: [0.0, 0.0, 400.0, 1000.0] },
        );
        assert_eq!((map.rows.len(), map.boxes.len()), (4, 2));
        // A hand-made Levels with a level above the bound never makes the inventory allocate it.
        let mut lv2 = lv.clone();
        if let Some((_, levels)) = lv2.cfs.get_mut(&0) {
            let f = levels[&0][0].clone();
            levels.insert(u32::MAX, vec![f]);
        }
        assert_eq!(inventory(&lv2, &src(&all)).cfs[0].levels.len(), 4);
    }

    /// 0024 D2 (architect should-fix 3): the heap lane assignment equals the lowest-free-lane
    /// linear scan on pseudo-random intervals, and lanes never overlap.
    #[test]
    fn heap_lanes_match_the_linear_scan() {
        let naive = |slots: &[(u32, u32)]| {
            let mut order: Vec<usize> = (0..slots.len()).collect();
            order.sort_by_key(|&i| (slots[i].0, slots[i].1));
            let mut ends: Vec<u32> = Vec::new();
            let mut lane = vec![0u32; slots.len()];
            for i in order {
                let (s, e) = slots[i];
                match ends.iter().position(|&end| end <= s) {
                    Some(l) => {
                        ends[l] = e;
                        lane[i] = l as u32;
                    }
                    None => {
                        lane[i] = ends.len() as u32;
                        ends.push(e);
                    }
                }
            }
            (lane, ends.len() as u32)
        };
        let mut x = 12345u32;
        let mut rnd = |m: u32| {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (x >> 8) % m
        };
        for n in [0usize, 1, 7, 50, 400] {
            let slots: Vec<(u32, u32)> = (0..n)
                .map(|_| {
                    let a = rnd(200);
                    (a, a + 1 + rnd(30))
                })
                .collect();
            assert_eq!(assign_lanes(&slots), naive(&slots), "n = {n}");
        }
    }

    /// 0024 D2 (architect: overlap counts agree with lanes): in the real fixtures (bytewise),
    /// a file in lane > 0 overlaps another file of its level (`overlap_same > 0`), and an L1+
    /// level with more than one lane is flagged `overlaps`.
    #[test]
    fn lanes_agree_with_overlap_counts_in_fixtures() {
        let all = |_: &Path| true;
        for m in ["tests/fixtures/db-levels/MANIFEST-000005", "tests/fixtures/db-checkpoint/MANIFEST-000005"] {
            let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(m);
            let r = crate::io::RangeReader::open(&p).unwrap();
            let lv = crate::manifest::observed_levels(&r, &|| false);
            let inv = inventory(&lv, &src(&all));
            // 0026 item 4: every live CF of the real fixtures recorded a log number.
            let ln = &inv.log_numbers;
            assert!(ln.active && !ln.per_cf.is_empty() && ln.without.is_empty(), "{m}: {ln:?}");
            let mut lanes_seen = 0;
            for c in &inv.cfs {
                assert!(c.order.ordered(), "{m}: {:?}", c.order);
                for l in &c.levels {
                    for f in &l.files {
                        if f.lane > 0 {
                            lanes_seen += 1;
                            assert!(f.overlap_same.is_some_and(|n| n > 0), "{m}: {} lane {} without overlap", f.name(), f.lane);
                        }
                    }
                    assert_eq!(l.overlaps, l.level > 0 && l.lanes > 1, "{m} L{}", l.level);
                }
            }
            if m.contains("db-levels") {
                assert!(lanes_seen > 0, "db-levels has overlapping L0 files");
            }
        }
    }

    /// 0024 D2 (tester observation 2): one sentence for a missing MANIFEST; the OS error is a
    /// secondary detail note only; the reason is typed in `lsm_blocked`.
    #[test]
    fn missing_manifest_wording_is_shared_and_typed() {
        let v = levels_unavailable(Status::NotPresent, manifest_missing("MANIFEST-000005"), Some("No such file or directory (os error 2)".into()));
        let want = "CURRENT names MANIFEST-000005, which is not in this folder. The live SST files cannot be observed.";
        assert_eq!(v.lsm_blocked.as_deref(), Some(want));
        assert_eq!(v.rows[0].field.value, want);
        assert!(!v.rows[0].field.value.contains("os error"));
        assert!(v.notes.iter().any(|n| n.text.starts_with("Detail: ") && n.text.contains("os error 2")));
        let c = super::super::common::current_view(b"MANIFEST-000005\n", Ok((std::path::PathBuf::from("/x/MANIFEST-000005"), false)));
        assert_eq!(c.lsm_blocked.as_deref(), Some(want), "CURRENT root and levels view say the same");
        assert_eq!(current_invalid("unexpected CURRENT content"), "CURRENT is invalid: unexpected CURRENT content. The active MANIFEST is unknown.");
    }
}
