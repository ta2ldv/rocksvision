//! Shared map facts (0026 §5, egui-free): one builder per map object (SST box, WAL box, level,
//! older-WAL summary) feeds both its tooltip and its Explanation card, so they never drift.
//! Every fact is (label, value, source) with a typed source tag and a certainty; values are
//! composed from typed inventory/summary data only (nothing is parsed back from text).
//! Text rules (§2.6): keys appear only as the observed smallest/largest previews of files; no
//! predecessor/successor keys, no "missing keys"/"no data", widths are "key rank, not to scale".

use crate::model::{CfMembership, LsmCf, LsmFile, LsmInventory, LsmLevel, ManifestLogNumbers, PartialWhy, Status, WalFile, WalLifecycle, WalSummary, files_word};
use crate::util::{bytes_preview, human_size, utc_time};

/// Where a fact comes from (§5.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Manifest,
    /// The file name (WAL number, file number).
    Name,
    /// The folder listing / file system (size, mtime, on disk).
    Disk,
    /// The bounded background WAL scan.
    Scan,
    /// Computed from the above.
    Derived,
}

impl Source {
    pub fn tag(&self) -> &'static str {
        match self {
            Source::Manifest => "MANIFEST",
            Source::Name => "name",
            Source::Disk => "disk",
            Source::Scan => "scan",
            Source::Derived => "derived",
        }
    }
}

/// Known, from a partial scan (a lower bound), or unknown (the value says why).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Certainty {
    Known,
    Partial,
    Unknown,
}

/// How a value is set: prose, or a key preview (monospace).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueKind {
    Text,
    Key,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MapFact {
    pub label: &'static str,
    pub value: String,
    pub source: Source,
    pub certainty: Certainty,
    pub kind: ValueKind,
}

impl MapFact {
    fn new(label: &'static str, value: impl Into<String>, source: Source) -> Self {
        MapFact { label, value: value.into(), source, certainty: Certainty::Known, kind: ValueKind::Text }
    }
    fn key(mut self) -> Self {
        self.kind = ValueKind::Key;
        self
    }
    fn unknown(label: &'static str, reason: &str, source: Source) -> Self {
        MapFact { label, value: format!("unknown — {reason}"), source, certainty: Certainty::Unknown, kind: ValueKind::Text }
    }
    fn partial(label: &'static str, value: &str, records: u64, source: Source) -> Self {
        MapFact { label, value: format!("≈ {value} (partial: first {records} records)"), source, certainty: Certainty::Partial, kind: ValueKind::Text }
    }
    /// The tooltip's muted suffix: only for derived, partial or unknown facts (§5.1).
    pub fn tip_suffix(&self) -> Option<&'static str> {
        match self.certainty {
            Certainty::Partial => Some("(partial)"),
            Certainty::Unknown => Some("(unknown)"),
            Certainty::Known if self.source == Source::Derived => Some("(derived)"),
            Certainty::Known => None,
        }
    }
}

/// CF entries listed in one fact (D5 nit 3); the rest is counted (" +N more").
pub const LIST_CAP: usize = 16;

/// The first `LIST_CAP` of `items` joined with ", ", then " +N more" of `total`.
fn capped_list(items: impl Iterator<Item = String>, total: usize) -> String {
    let mut s = items.take(LIST_CAP).collect::<Vec<_>>().join(", ");
    if total > LIST_CAP {
        s.push_str(&format!(" +{} more", total - LIST_CAP));
    }
    s
}

/// Facts shown in a tooltip (the Explanation shows all of them).
pub const TIP_FACTS: usize = 9;
/// Names listed for the older-WAL summary and for overlapping files.
pub const SUMMARY_NAMES: usize = 50;
pub const OVERLAP_NAMES: usize = 3;

fn key_text(k: &[u8]) -> String {
    bytes_preview(k, 40)
}

// ---------------------------------------------------------------- SST

/// Files of `l` that overlap `f` in the CF's comparator order (same level, observed boundary
/// keys only), with the shared range [max of the smallest keys, min of the largest keys].
/// Only where the inventory computed the same-level overlap count (`overlap_same` is Some:
/// ordered keys, level within `OVERLAP_MAX_FILES`), so the names never contradict the count
/// (D3 S4). At most `OVERLAP_NAMES` (name, shared range) are built; the rest are only counted.
fn overlaps(c: &LsmCf, l: &LsmLevel, f: &LsmFile) -> (Vec<String>, usize) {
    let o = &c.order;
    let mut names = Vec::new();
    let mut total = 0usize;
    if f.overlap_same.is_none() {
        return (names, 0);
    }
    for g in l.files.iter().filter(|g| g.number != f.number) {
        let (Some(a), Some(b)) = (o.cmp_keys(&g.smallest, &f.largest), o.cmp_keys(&f.smallest, &g.largest)) else { return (Vec::new(), 0) };
        if a.is_le() && b.is_le() {
            total += 1;
            if names.len() < OVERLAP_NAMES {
                let lo = if o.cmp_keys(&g.smallest, &f.smallest).is_some_and(|x| x.is_gt()) { &g.smallest } else { &f.smallest };
                let hi = if o.cmp_keys(&g.largest, &f.largest).is_some_and(|x| x.is_lt()) { &g.largest } else { &f.largest };
                names.push(format!("{} on {} … {}", g.name().trim_end_matches(".sst"), key_text(lo), key_text(hi)));
            }
        }
    }
    (names, total)
}

/// The SST box / levels row facts (§5.2), in tooltip order.
/// The one sentence relating an SST to its column family (0030 B-6/B-7; architect S2): the
/// short form is the LSM file card / map tooltip fact (UI final §5.2: one line), the full form
/// (which starts with it) the levels table's CF cell tooltip.
pub const CF_RELATION_SHORT: &str = "the SST belongs to this CF";
pub const CF_RELATION: &str = "the SST belongs to this CF; each CF has its own levels, key order and files";

pub fn sst_facts(c: &LsmCf, l: &LsmLevel, f: &LsmFile) -> Vec<MapFact> {
    use Source::*;
    let mut v = vec![
        MapFact::new("File", f.name(), Manifest),
        MapFact::new("Column family", format!("{} {} — {CF_RELATION_SHORT}", c.id, c.name), Manifest),
        MapFact::new("Level", format!("L{}", l.level), Manifest),
    ];
    if l.lanes > 1 {
        v.push(MapFact::new("Lane", format!("{} of {} in L{} (lanes stack overlapping files; empty lane space is outside this file's key range)", f.lane + 1, l.lanes, l.level), Derived));
    }
    v.push(MapFact::new("Size", human_size(f.size), Manifest));
    v.push(MapFact::new("Smallest", f.smallest_preview.clone(), Manifest).key());
    v.push(MapFact::new("Largest", f.largest_preview.clone(), Manifest).key());
    v.push(MapFact::new("Seq range", f.seq.map(|(a, b)| format!("{a}..{b}")).unwrap_or_else(|| "-".into()), Manifest));
    let mut overlap = LsmInventory::overlap_text(c, l, f);
    let (names, total) = overlaps(c, l, f);
    if !names.is_empty() {
        overlap.push_str(&format!(": {}", names.join("; ")));
        if total > names.len() {
            overlap.push_str(&format!(" +{} more", total - names.len()));
        }
    }
    v.push(MapFact::new("Overlap", overlap, Derived));
    v.push(MapFact::new("Key order", c.order.describe(), Manifest));
    v.push(MapFact::new("On disk", if f.on_disk() { "yes".into() } else { "missing from this directory".to_string() }, Disk));
    if let Some(p) = &f.path {
        v.push(MapFact::new("Path", p.display().to_string(), Disk));
    }
    v
}

/// How to open an SST, or why it cannot be opened.
pub fn sst_hint(f: &LsmFile) -> String {
    if f.on_disk() { "Enter / double-click opens it (Back returns here).".into() } else { format!("○ Missing: {} is not in this folder, so it cannot be opened.", f.name()) }
}

// ---------------------------------------------------------------- levels

/// The level card / title tooltip facts (§5.4).
pub fn level_facts(c: &LsmCf, l: &LsmLevel) -> Vec<MapFact> {
    use Source::*;
    let mut v = vec![MapFact::new("Level", format!("L{}", l.level), Manifest), MapFact::new("Column family", format!("{} {}", c.id, c.name), Manifest)];
    if l.files.is_empty() {
        v.push(MapFact::new("Files", "empty (0 files in MANIFEST)", Manifest));
    } else {
        v.push(MapFact::new("Files", files_word(l.files.len()), Manifest));
        v.push(MapFact::new("Bytes", human_size(l.bytes), Manifest));
        let missing = l.files.len() - l.on_disk;
        v.push(MapFact::new("On disk", if missing == 0 { format!("✔ {} on disk", l.on_disk) } else { format!("✔ {} · ○ {missing} missing", l.on_disk) }, Disk));
        if !c.order.ordered() {
            v.push(MapFact::unknown("Lanes", "key order unknown, so overlap is not computed", Derived));
        } else if l.level == 0 {
            v.push(MapFact::new("Lanes", format!("{} (overlapping L0 files stack in lanes; allowed in L0)", l.lanes.max(1)), Derived));
        } else if l.overlaps {
            v.push(MapFact::new("Overlap", format!("⚠ files of L{} overlap each other ({} lanes)", l.level, l.lanes), Derived));
        } else {
            v.push(MapFact::new("Overlap", "none (one lane)", Derived));
        }
        match c.order.span(l.files.iter()) {
            Some((a, b)) => v.push(MapFact::new("Key span", format!("{} … {} (first and last boundary key)", key_text(a), key_text(b)), Derived).key()),
            None => v.push(MapFact::unknown("Key span", "key order unknown", Derived)),
        }
    }
    v.push(MapFact::new("Key order", c.order.describe(), Manifest));
    v
}

// ---------------------------------------------------------------- WAL

/// Lifecycle sentence of a WAL (§4.6), relative to the CF shown.
pub fn lifecycle_text(number: Option<u64>, ln: &ManifestLogNumbers, inv: &LsmInventory, shown_cf: Option<u32>) -> (String, bool) {
    let name = |id: u32| inv.cf(id).map(|c| c.name.clone()).unwrap_or_else(|| format!("CF {id}"));
    match WalLifecycle::of(number, ln) {
        WalLifecycle::FlushedPerManifest => {
            // N7 (2PC): a WAL at or above kMinLogNumberToKeep is still kept by RocksDB.
            let kept = match (number, ln.min_to_keep) {
                (Some(n), Some(k)) if n >= k => " (kept: ≥ min log to keep)",
                _ => "",
            };
            (format!("No unflushed data per MANIFEST (below every column family's log number){kept}."), true)
        }
        WalLifecycle::MayHoldUnflushed { cfs } => {
            let cf = shown_cf.filter(|c| ln.per_cf.contains_key(c));
            match cf {
                Some(c) if cfs.contains(&c) => (format!("May hold unflushed writes for {} (log number ≥ {:06} per MANIFEST).", name(c), ln.per_cf[&c]), true),
                Some(c) => {
                    let others = capped_list(cfs.iter().map(|o| name(*o)), cfs.len());
                    (format!("Flushed for {} per MANIFEST; may hold unflushed writes for {others} (per MANIFEST).", name(c)), true)
                }
                None => {
                    let all = capped_list(cfs.iter().map(|o| name(*o)), cfs.len());
                    (format!("May hold unflushed writes for {all} (per MANIFEST)."), true)
                }
            }
        }
        WalLifecycle::Unknown(why) => (format!("Unknown ({}).", why.text()), false),
    }
}

/// The WAL box's state line (§4.4): glyph + text and its status colour role.
#[derive(Clone, Debug, PartialEq)]
pub struct WalState {
    pub text: String,
    /// None: `text_muted`; Some: that status colour.
    pub status: Option<Status>,
    /// The state's category (summary counts, typed).
    pub kind: WalStateKind,
    /// The glyph alone (chips form).
    pub glyph: &'static str,
}

/// The category of a WAL state (typed; `word` for the older-WAL counts).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalStateKind {
    NotScanned,
    Scanning,
    Empty,
    Complete,
    TruncatedTail,
    Partial,
    StaleTail,
    Corrupt,
    Unknown,
}

/// Compares with the category word (kept so callers written against the D1 `&'static str`
/// kind, e.g. tests/v0012_map.rs, keep compiling; production code compares the enum).
impl PartialEq<&str> for WalStateKind {
    fn eq(&self, other: &&str) -> bool {
        self.word() == *other
    }
}

impl WalStateKind {
    pub fn word(&self) -> &'static str {
        match self {
            WalStateKind::NotScanned => "not scanned",
            WalStateKind::Scanning => "scanning",
            WalStateKind::Empty => "empty",
            WalStateKind::Complete => "complete",
            WalStateKind::TruncatedTail => "truncated tail",
            WalStateKind::Partial => "partial",
            WalStateKind::StaleTail => "old-record tail",
            WalStateKind::Corrupt => "corrupt",
            WalStateKind::Unknown => "unknown",
        }
    }
}

pub fn wal_state(f: &WalFile) -> WalState {
    let st = |text: String, status: Option<Status>, kind: WalStateKind, glyph: &'static str| WalState { text, status, kind, glyph };
    match &f.summary {
        WalSummary::NotScanned(_) => st("– not scanned".into(), None, WalStateKind::NotScanned, "–"),
        WalSummary::Scanning => st("… scanning".into(), None, WalStateKind::Scanning, "…"),
        WalSummary::Done(_) if f.size == 0 => st("· empty (0 B)".into(), None, WalStateKind::Empty, "·"),
        WalSummary::Done(s) => st(format!("✔ {} rec{}", s.records, s.seq.map(|(a, b)| format!(" · seq {a}..{b}")).unwrap_or_default()), Some(Status::Present), WalStateKind::Complete, "✔"),
        WalSummary::Partial { why: PartialWhy::TruncatedTail, .. } => st("⚠ truncated tail".into(), Some(Status::Truncated), WalStateKind::TruncatedTail, "⚠"),
        WalSummary::Partial { stats, .. } => st(format!("≈ partial: first {} rec", stats.records), Some(Status::Limited), WalStateKind::Partial, "≈"),
        WalSummary::StaleTail { .. } => st("⚠ old-record tail (recycled WAL)".into(), Some(Status::Unknown), WalStateKind::StaleTail, "⚠"),
        WalSummary::Corrupt { at, recycled: true, .. } => st(format!("✖ corrupt or recycled leftover at 0x{at:X}"), Some(Status::Invalid), WalStateKind::Corrupt, "✖"),
        WalSummary::Corrupt { at, .. } => st(format!("✖ corrupt at 0x{at:X}"), Some(Status::Invalid), WalStateKind::Corrupt, "✖"),
        WalSummary::Unreadable(e) => st(format!("⚠ unknown: {}", short(e, 40)), Some(Status::Unknown), WalStateKind::Unknown, "⚠"),
    }
}

fn short(s: &str, n: usize) -> String {
    if s.chars().count() > n { format!("{}…", s.chars().take(n).collect::<String>()) } else { s.to_string() }
}

/// The membership glyph of a WAL card for the CF shown (0030 A5-1, the status vocabulary):
/// `✔` records of the CF seen, `○` none seen after a complete scan (the card is dimmed), `?`
/// unknown (not scanned, scanning, partial) — never drawn as absent.
pub fn wal_member_glyph(f: &WalFile, cf: u32) -> (&'static str, bool) {
    let (glyph, dim, _) = member_parts(f, cf);
    (glyph, dim)
}

/// The one match on a WAL's membership of a CF (architect N2): glyph, dimmed, word.
fn member_parts(f: &WalFile, cf: u32) -> (&'static str, bool, &'static str) {
    match f.summary.membership(cf) {
        CfMembership::Observed => ("✔", false, "✔ seen"),
        CfMembership::ObservedAbsent => ("○", true, "none seen"),
        CfMembership::Unknown => ("?", false, "unknown"),
    }
}

/// The state line of a narrow WAL card (0030 A5-2): (`short` for
/// 120–159 pt (`✔ 12 rec`), `tiny` for 72–119 pt (`✔ 12`)); the glyph comes first in each.
pub fn wal_state_short(f: &WalFile) -> (String, String) {
    let s = wal_state(f);
    let n = f.summary.stats().map(|x| x.records);
    let short = match (s.kind, n) {
        (WalStateKind::Complete, Some(n)) => format!("✔ {n} rec"),
        (WalStateKind::Partial, Some(n)) => format!("≈ {n} rec"),
        (WalStateKind::StaleTail, _) => "⚠ old tail".into(),
        (WalStateKind::Corrupt, _) => "✖ corrupt".into(),
        (WalStateKind::TruncatedTail, _) => "⚠ truncated".into(),
        (WalStateKind::Unknown, _) => "⚠ unknown".into(),
        (WalStateKind::Empty, _) => "· empty".into(),
        _ => s.text.clone(),
    };
    let tiny = match (s.kind, n) {
        (WalStateKind::Complete | WalStateKind::Partial, Some(n)) => format!("{} {n}", s.glyph),
        _ => short.clone(),
    };
    (short, tiny)
}

/// The membership line of a WAL box for the CF shown (§4.5): text and whether the box is dimmed.
pub fn wal_membership(f: &WalFile, cf_name: &str, cf: u32) -> (String, bool) {
    let (_, dim, word) = member_parts(f, cf);
    (format!("{cf_name}: {word}"), dim)
}

/// The WAL card / tooltip facts (§5.3), in tooltip order (the first nine include the
/// lifecycle).
pub fn wal_facts(f: &WalFile, inv: &LsmInventory, shown_cf: Option<u32>) -> Vec<MapFact> {
    use Source::*;
    let mut v = vec![MapFact::new("File", f.name.clone(), Name)];
    v.push(match f.number {
        Some(n) => MapFact::new("Log number", n.to_string(), Name),
        None => MapFact::unknown("Log number", "not a numbered WAL name", Name),
    });
    v.push(MapFact::new("Size", human_size(f.size), Disk));
    let reason = |s: &WalSummary| -> String {
        match s {
            WalSummary::NotScanned(w) => format!("not scanned{}", w.text().map(|t| format!(" ({t})")).unwrap_or_default()),
            WalSummary::Scanning => "scanning…".into(),
            WalSummary::Unreadable(e) => format!("unreadable: {e}"),
            _ => String::new(),
        }
    };
    let state = match &f.summary {
        WalSummary::Done(_) => MapFact::new("Scan state", "whole file read", Scan),
        WalSummary::Partial { why, stats } => MapFact::partial("Scan state", &why.text(), stats.records, Scan),
        WalSummary::StaleTail { at, stats } => MapFact::new(
            "Scan state",
            format!("ends at an old-record tail at 0x{at:X} (recycled WAL; RocksDB's default recovery modes stop reading there); records before the stale tail: {}", stats.records),
            Scan,
        ),
        WalSummary::Corrupt { at, recycled, reason: r, stats } => {
            MapFact::partial("Scan state", &format!("{} at 0x{at:X}: {}", if *recycled { "corrupt or recycled leftover" } else { "corrupt" }, short(r, 120)), stats.records, Scan)
        }
        s => MapFact::unknown("Scan state", &reason(s), Scan),
    };
    v.push(state);
    let complete = f.summary.complete();
    match f.summary.stats() {
        Some(s) => {
            let rec = format!("{} ({} write batches)", s.records, s.batches);
            v.push(if complete { MapFact::new("Records", rec, Scan) } else { MapFact::partial("Records", &rec, s.records, Scan) });
            let seq = s.seq.map(|(a, b)| format!("{a}..{b}")).unwrap_or_else(|| "no write batch".into());
            v.push(if complete { MapFact::new("Seq range", seq, Scan) } else { MapFact::partial("Seq range", &seq, s.records, Scan) });
            let cfs =
                if s.cfs.is_empty() { "none".to_string() } else { capped_list(s.cfs.iter().map(|id| inv.cf(*id).map(|c| format!("{id} {}", c.name)).unwrap_or_else(|| id.to_string())), s.cfs.len()) };
            v.push(if complete { MapFact::new("CFs seen", cfs, Scan) } else { MapFact::partial("CFs seen", &format!("seen so far: {cfs}"), s.records, Scan) });
        }
        None => v.push(MapFact::unknown("CFs seen", &reason(&f.summary), Scan)),
    }
    if let Some(cf) = shown_cf {
        let name = inv.cf(cf).map(|c| c.name.clone()).unwrap_or_else(|| format!("CF {cf}"));
        let (text, _) = wal_membership(f, &name, cf);
        v.push(match f.summary.membership(cf) {
            CfMembership::Unknown => MapFact::unknown("Shown CF", &format!("{name}: not fully scanned"), Scan),
            CfMembership::ObservedAbsent => MapFact::new("Shown CF", format!("{text} ({})", f.summary.complete_scan_text().unwrap_or_default()), Scan),
            CfMembership::Observed => MapFact::new("Shown CF", text, Scan),
        });
    }
    let (mut life, known) = lifecycle_text(f.number, &inv.log_numbers, inv, shown_cf);
    // D6: the MANIFEST rule and a complete scan are two typed facts; when the complete logical
    // scan (to the end of the log or its stale tail, 0030 A6) found no record of the shown CF,
    // "may hold" would contradict it.
    if let Some(c) = shown_cf
        && f.summary.membership(c) == CfMembership::ObservedAbsent
        && matches!(WalLifecycle::of(f.number, &inv.log_numbers), WalLifecycle::MayHoldUnflushed { ref cfs } if cfs.contains(&c))
    {
        let name = inv.cf(c).map(|x| x.name.clone()).unwrap_or_else(|| format!("CF {c}"));
        life = format!("At or above {name}'s log number per MANIFEST; {} found no {name} records.", f.summary.complete_scan_clause().unwrap_or_else(|| "a complete scan".into()));
    }
    v.push(MapFact { certainty: if known { Certainty::Known } else { Certainty::Unknown }, ..MapFact::new("Lifecycle", life, Derived) });
    v.push(match f.mtime {
        Some(t) => MapFact::new("Modified", utc_time(t), Disk),
        None => MapFact::unknown("Modified", "not available", Disk),
    });
    let ln = &inv.log_numbers;
    if !ln.per_cf.is_empty() {
        let mut t = capped_list(ln.per_cf.iter().map(|(c, l)| format!("CF {c}: {l}")), ln.per_cf.len());
        if let Some(k) = ln.min_to_keep {
            t.push_str(&format!(" · min to keep {k}"));
        }
        v.push(MapFact::new("MANIFEST log numbers", t, Manifest));
    }
    v
}

pub const WAL_NOTE: &str = "A WAL is not an SST level; no WAL-to-SST link is recorded.";

pub fn wal_hint(f: &WalFile) -> String {
    if f.symlink { format!("{} is a symlink; it is not followed, so it cannot be opened here.", f.name) } else { "Enter / double-click opens it (Back returns here).".into() }
}

/// Title of a level card / tooltip (D3 N2: painter and Explanation share it).
pub fn level_title(c: &LsmCf, l: &LsmLevel) -> String {
    format!("L{} · {}", l.level, c.name)
}

/// Title of the older-WAL summary card / tooltip.
pub fn older_title(k: usize) -> String {
    format!("+{k} older WAL files")
}

/// The WALs folded into an older-WAL summary whose newest file is `newest`: the leading files
/// of the inventory (number order) up to and including it.
pub fn older_wals(files: &[WalFile], newest: u64) -> &[WalFile] {
    let k = files.iter().position(|f| f.number == Some(newest)).map(|i| i + 1).unwrap_or(0);
    &files[..k]
}

/// The older-WAL summary box (§4.7): counts per scan state, CFs seen, the first 50 names.
pub fn older_wals_facts(files: &[WalFile], inv: &LsmInventory) -> Vec<MapFact> {
    use Source::*;
    let mut v = vec![MapFact::new("Files", files_word(files.len()), Disk)];
    if let (Some(a), Some(b)) = (files.first(), files.last()) {
        v.push(MapFact::new("Range", format!("{} – {}", a.name, b.name), Name));
    }
    let mut counts: Vec<(WalStateKind, usize)> = Vec::new();
    for f in files {
        let word = wal_state(f).kind;
        match counts.iter_mut().find(|(w, _)| *w == word) {
            Some((_, n)) => *n += 1,
            None => counts.push((word, 1)),
        }
    }
    v.push(MapFact::new("Scan states", counts.iter().map(|(w, n)| format!("{n} × {}", w.word())).collect::<Vec<_>>().join(", "), Scan));
    let seen: std::collections::BTreeSet<u32> = files.iter().filter_map(|f| f.summary.stats()).flat_map(|s| s.cfs.iter().copied()).collect();
    let n_seen = seen.len();
    let seen = capped_list(seen.iter().map(|id| inv.cf(*id).map(|c| format!("{id} {}", c.name)).unwrap_or_else(|| id.to_string())), n_seen);
    v.push(MapFact::new("CFs seen", if n_seen == 0 { "none observed".into() } else { seen }, Scan));
    let mut names = files.iter().take(SUMMARY_NAMES).map(|f| f.name.clone()).collect::<Vec<_>>().join(", ");
    if files.len() > SUMMARY_NAMES {
        names.push_str(&format!(" +{} more", files.len() - SUMMARY_NAMES));
    }
    v.push(MapFact::new("Names", names, Name));
    v
}

/// The band title's lifecycle count for the CF shown (§4.1), when the MANIFEST gives evidence.
pub fn band_lifecycle(files: &[WalFile], inv: &LsmInventory, cf: Option<u32>) -> Option<String> {
    let ln = &inv.log_numbers;
    let c = cf?;
    // One lifecycle rule (D3 S1): `WalLifecycle::of`; nothing is said without evidence for
    // the CF shown.
    if !ln.active || !ln.per_cf.contains_key(&c) {
        return None;
    }
    let mut known = false;
    let mut n = 0usize;
    for f in files {
        match WalLifecycle::of(f.number, ln) {
            WalLifecycle::MayHoldUnflushed { cfs } => {
                known = true;
                // D6: a complete scan (Done or StaleTail) that found no record of this CF is not
                // counted.
                n += usize::from(cfs.contains(&c) && f.summary.membership(c) != CfMembership::ObservedAbsent);
            }
            WalLifecycle::FlushedPerManifest => known = true,
            WalLifecycle::Unknown(_) => {}
        }
    }
    // Nothing to say when no file is known, or none may hold writes of this CF (D6).
    if !known || n == 0 {
        return None;
    }
    let name = inv.cf(c).map(|x| x.name.clone()).unwrap_or_else(|| format!("CF {c}"));
    Some(format!("{n} may hold unflushed writes for {name} (per MANIFEST)"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{KeyOrder, ManifestLogNumbers, NotScannedWhy, Status as St, WalStats};
    use std::collections::{BTreeMap, BTreeSet};

    fn sst(number: u64, a: &str, b: &str, lane: u32) -> LsmFile {
        LsmFile {
            number,
            size: 4200,
            smallest: a.as_bytes().to_vec(),
            largest: b.as_bytes().to_vec(),
            smallest_preview: format!("\"{a}\" @1 t1"),
            largest_preview: format!("\"{b}\" @2 t1"),
            seq: Some((1, 2)),
            overlap_same: Some(1),
            overlap_next: Some(0),
            status: St::Present,
            row: number as usize,
            path: None,
            slots: (0, 1),
            lane,
        }
    }
    fn cf() -> LsmCf {
        let l0 = LsmLevel { level: 0, files: vec![sst(16, "u-000", "u-014", 0), sst(18, "u-010", "u-024", 1)], bytes: 8400, on_disk: 2, scale: 4, lanes: 2, overlaps: false };
        LsmCf { id: 1, name: "cf_users".into(), levels: vec![l0, LsmLevel { level: 1, ..Default::default() }], comparator: None, order: KeyOrder::Bytewise }
    }

    /// §2.7/§5.2: lane and overlap facts are derived, with the shared range from observed
    /// boundary keys only; §2.6 text rules hold for every map fact text.
    #[test]
    fn sst_and_level_facts_are_typed_and_truthful() {
        let c = cf();
        let f = sst_facts(&c, &c.levels[0], &c.levels[0].files[1]);
        let get = |l: &str| f.iter().find(|x| x.label == l).unwrap().clone();
        assert_eq!(get("Lane").value, "2 of 2 in L0 (lanes stack overlapping files; empty lane space is outside this file's key range)");
        assert_eq!(get("Lane").tip_suffix(), Some("(derived)"));
        assert_eq!(get("Overlap").value, "1 in L0 (allowed), 0 in L1: 000016 on \"u-010\" … \"u-014\"");
        assert_eq!((get("Overlap").source, get("Level").source, get("On disk").source), (Source::Derived, Source::Manifest, Source::Disk));
        assert_eq!(get("Smallest").kind, ValueKind::Key);
        let lv = level_facts(&c, &c.levels[0]);
        assert!(lv.iter().any(|x| x.label == "Key span" && x.value.starts_with("\"u-000\" … \"u-024\"")));
        let empty = level_facts(&c, &c.levels[1]);
        assert!(empty.iter().any(|x| x.value == "empty (0 files in MANIFEST)"));
        // Text rules: no fabricated keys, no "missing keys" / "no data" / "lacks", no share of
        // the key space.
        let all: Vec<String> = f.iter().chain(lv.iter()).chain(empty.iter()).map(|x| x.value.to_lowercase()).collect();
        for bad in ["u-009", "u-025", "missing keys", "no data", "lacks", "% of the key", "share of"] {
            assert!(!all.iter().any(|v| v.contains(bad)), "{bad}: {all:?}");
        }
    }

    /// D5 nit 3: CF lists in fact strings stop at 16 entries and count the rest.
    #[test]
    fn cf_lists_are_capped() {
        assert_eq!(capped_list((0..3).map(|i| i.to_string()), 3), "0, 1, 2");
        let s = capped_list((0..40).map(|i| format!("CF {i}")), 40);
        assert!(s.starts_with("CF 0, CF 1") && s.ends_with("CF 15 +24 more"), "{s}");
    }

    /// D3 S4: the overlap names follow the inventory's count (`overlap_same`) for every file of
    /// the real fixtures, bounded to three names; none where the count was not computed.
    #[test]
    fn overlap_facts_agree_with_inventory_counts() {
        let all = |_: &std::path::Path| true;
        let mut checked = 0;
        for m in ["tests/fixtures/db-levels/MANIFEST-000005", "tests/fixtures/db-checkpoint/MANIFEST-000005"] {
            let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(m);
            let lv = crate::manifest::observed_levels(&crate::io::RangeReader::open(&p).unwrap(), &|| false);
            let src = crate::present::manifest::LevelsSource { via: String::new(), dir: None, exists: &all, wals: None, active: crate::present::manifest::Active::Yes };
            let inv = crate::present::manifest::inventory(&lv, &src);
            for c in &inv.cfs {
                for l in &c.levels {
                    for f in &l.files {
                        let (names, total) = overlaps(c, l, f);
                        assert_eq!(total, f.overlap_same.unwrap_or(0) as usize, "{m} {}: names follow the inventory count", f.name());
                        assert_eq!(names.len(), total.min(OVERLAP_NAMES));
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 5);
        // Not computed (e.g. a level over OVERLAP_MAX_FILES): no names, whatever the keys say.
        let mut c = cf();
        for f in &mut c.levels[0].files {
            f.overlap_same = None;
        }
        assert_eq!(overlaps(&c, &c.levels[0], &c.levels[0].files[1]), (Vec::new(), 0));
        let o = sst_facts(&c, &c.levels[0], &c.levels[0].files[1]).into_iter().find(|x| x.label == "Overlap").unwrap();
        assert!(!o.value.contains("000016"), "{}", o.value);
    }

    /// §4.4–§4.6 / §5.3: WAL state lines, membership, lifecycle wording and fact certainty.
    #[test]
    fn wal_states_membership_lifecycle_and_facts() {
        let ln = ManifestLogNumbers { per_cf: BTreeMap::from([(0, 9), (1, 7)]), active: true, ..Default::default() };
        let mut c = cf();
        c.id = 1;
        let inv = LsmInventory { cfs: vec![LsmCf { id: 0, name: "default".into(), ..Default::default() }, c], log_numbers: ln, ..Default::default() };
        let mut f =
            WalFile { number: Some(8), path: "/db/000008.log".into(), name: "000008.log".into(), size: 100, mtime: None, symlink: false, summary: WalSummary::NotScanned(NotScannedWhy::Pending) };
        assert_eq!(wal_state(&f).text, "– not scanned");
        assert_eq!(wal_membership(&f, "cf_users", 1), ("cf_users: unknown".into(), false));
        let st = WalStats { records: 3, batches: 2, cfs: BTreeSet::from([0]), seq: Some((5, 9)), bytes_scanned: 60 };
        f.summary = WalSummary::Done(st.clone());
        assert_eq!((wal_state(&f).text.as_str(), wal_state(&f).status), ("✔ 3 rec · seq 5..9", Some(St::Present)));
        assert_eq!(wal_membership(&f, "cf_users", 1), ("cf_users: none seen".into(), true));
        f.summary = WalSummary::Partial { stats: st.clone(), why: PartialWhy::Bound { read: 60, size: 100 } };
        assert_eq!(wal_state(&f).text, "≈ partial: first 3 rec");
        assert_eq!(wal_membership(&f, "cf_users", 1).0, "cf_users: unknown");
        f.summary = WalSummary::Corrupt { stats: st.clone(), at: 0x1a2b, recycled: false, reason: "x".into() };
        assert_eq!(wal_state(&f).text, "✖ corrupt at 0x1A2B");
        f.summary = WalSummary::StaleTail { stats: st.clone(), at: 0x40 };
        assert_eq!(wal_state(&f).text, "⚠ old-record tail (recycled WAL)");
        f.size = 0;
        f.summary = WalSummary::Done(WalStats::default());
        assert_eq!(wal_state(&f).text, "· empty (0 B)");
        // Lifecycle (§4.6) relative to the CF shown.
        assert_eq!(lifecycle_text(Some(6), &inv.log_numbers, &inv, Some(1)).0, "No unflushed data per MANIFEST (below every column family's log number).");
        assert_eq!(lifecycle_text(Some(8), &inv.log_numbers, &inv, Some(1)).0, "May hold unflushed writes for cf_users (log number ≥ 000007 per MANIFEST).");
        assert_eq!(lifecycle_text(Some(8), &inv.log_numbers, &inv, Some(0)).0, "Flushed for default per MANIFEST; may hold unflushed writes for cf_users (per MANIFEST).");
        // N7 (2PC): at or above kMinLogNumberToKeep the WAL is kept.
        let kept = ManifestLogNumbers { min_to_keep: Some(5), ..inv.log_numbers.clone() };
        assert_eq!(lifecycle_text(Some(6), &kept, &inv, Some(1)).0, "No unflushed data per MANIFEST (below every column family's log number) (kept: ≥ min log to keep).");
        assert!(!lifecycle_text(Some(4), &kept, &inv, Some(1)).0.contains("kept"));
        let none = ManifestLogNumbers { active: true, ..Default::default() };
        assert_eq!(lifecycle_text(Some(8), &none, &inv, Some(1)).0, "Unknown (no column family log numbers in the MANIFEST).");
        // Facts: the first nine include the lifecycle; partial values are marked.
        f.size = 100;
        f.summary = WalSummary::Partial { stats: st, why: PartialWhy::Bound { read: 60, size: 100 } };
        let facts = wal_facts(&f, &inv, Some(1));
        assert!(facts.iter().take(TIP_FACTS).any(|x| x.label == "Lifecycle"));
        let rec = facts.iter().find(|x| x.label == "Records").unwrap();
        assert_eq!((rec.value.as_str(), rec.certainty), ("≈ 3 (2 write batches) (partial: first 3 records)", Certainty::Partial));
        assert_eq!(facts.iter().find(|x| x.label == "Modified").unwrap().value, "unknown — not available");
        for x in &facts {
            let v = x.value.to_lowercase();
            assert!(!v.contains("memtable") && !v.contains(".sst") && !(v.contains("unflushed") && !v.contains("per manifest")), "{v}");
        }
        assert_eq!(band_lifecycle(std::slice::from_ref(&f), &inv, Some(1)).as_deref(), Some("1 may hold unflushed writes for cf_users (per MANIFEST)"));
        let older = older_wals_facts(std::slice::from_ref(&f), &inv);
        assert!(older.iter().any(|x| x.label == "Names" && x.value == "000008.log"));
        // D6: a complete scan without a cf_users record: no "may hold" for it (band count and
        // lifecycle line); Unknown membership keeps the MANIFEST sentence.
        f.summary = WalSummary::Done(WalStats { records: 3, batches: 2, cfs: BTreeSet::from([0]), seq: Some((5, 9)), bytes_scanned: 100 });
        assert_eq!(band_lifecycle(std::slice::from_ref(&f), &inv, Some(1)).as_deref(), None, "no \"0 may hold\" note");
        let life = wal_facts(&f, &inv, Some(1)).into_iter().find(|x| x.label == "Lifecycle").unwrap();
        assert_eq!(life.value, "At or above cf_users's log number per MANIFEST; a complete scan found no cf_users records.");
        // 0030 A6: a stale tail ends the log — complete, but not the whole file.
        f.summary = WalSummary::StaleTail { stats: WalStats { records: 2, batches: 2, cfs: BTreeSet::from([0]), seq: Some((5, 6)), bytes_scanned: 60 }, at: 0x1F40 };
        let facts = wal_facts(&f, &inv, Some(1));
        let life = facts.iter().find(|x| x.label == "Lifecycle").unwrap();
        assert_eq!(life.value, "At or above cf_users's log number per MANIFEST; a complete scan of this log's records (ending at the old-record tail at 0x1F40) found no cf_users records.");
        let shown = facts.iter().find(|x| x.label == "Shown CF").unwrap();
        assert_eq!(shown.value, "cf_users: none seen (complete scan of this log's records, which end at the old-record tail at 0x1F40)");
        assert!(facts.iter().all(|x| !x.value.contains("whole")), "no whole-file wording for a stale tail: {facts:?}");
        f.summary = WalSummary::Done(WalStats { cfs: BTreeSet::from([1]), ..WalStats::default() });
        assert!(wal_facts(&f, &inv, Some(1)).iter().any(|x| x.label == "Lifecycle" && x.value.starts_with("May hold unflushed writes for cf_users")), "Observed keeps it");
    }
}
