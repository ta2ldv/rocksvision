//! Explanation model (egui-free, 0020 §3): the one classifier of the current selection
//! (`subject`) and the cards the Explanation panel renders and the status-bar segment derived
//! from it, so the card and the status bar cannot drift apart. It only selects and composes
//! `provenance` (ranges, where, status) and `diagnosis` (typed findings); it never reads files.

use super::diagnosis::{self, Diagnosis, Finding, NodeFailure};
use super::lsm_map::MapPick;
use super::map_facts::{self, MapFact};
use super::provenance;
use super::selection::SelectionState;
use super::session::{DocumentSession, TreeNode};
use crate::model::{ColumnRole, Field, SpaceInfo, SpaceMap, Span, Status, View, WhereLine, files_word};

/// Where of a value without bytes of its own (no enclosing structure shown).
pub const NO_BYTES: &str = "No bytes in the file (derived, inferred, default or metadata value)";

/// What the selection is, for the Explanation and the status bar.
#[derive(Clone, Debug)]
pub enum Subject<'a> {
    /// A field (table row) of the current view.
    Field { row: usize, field: &'a Field },
    /// The whole structure the view shows (no span, or the view's own extent; no clicked byte).
    WholeStructure,
    /// Structure bytes that are not one field (header, trailer, compressed stored bytes…).
    /// `clicked`: reached through a clicked byte (the hex view's anchor); the status bar then
    /// states the byte range, not "structure …".
    StructureRange { span: Span, clicked: bool },
    /// A raw byte range (Shift+click, go to offset, diagnostic bytes, byte-search hit).
    Raw(Span),
    /// No selection; a view is shown.
    Node,
    /// A whole structure was chosen and no view is shown (yet): the card is empty, the status
    /// bar names the structure.
    PendingStructure,
    /// Nothing at all.
    Empty,
}

/// The one classifier of the selection.
pub fn subject<'a>(doc: &'a DocumentSession, sel: &SelectionState) -> Subject<'a> {
    if let (Some(row), Some(field)) = (sel.row(), doc.row_field(sel.row())) {
        return Subject::Field { row, field };
    }
    let whole = sel.is_structure() && sel.anchor().is_none() && (sel.span().is_none() || sel.span() == doc.view().and_then(|v| v.extent));
    if whole && doc.view().is_some() {
        return Subject::WholeStructure;
    }
    match sel.span() {
        Some(span) if sel.is_structure() => Subject::StructureRange { span, clicked: sel.anchor().is_some() },
        Some(s) => Subject::Raw(s),
        None if doc.view().is_some() => Subject::Node,
        None if whole => Subject::PendingStructure,
        None => Subject::Empty,
    }
}

/// The "Where" text of a selection or field span.
#[derive(Clone, Debug, PartialEq)]
pub enum WhereText {
    /// One monospace line per address space, File first.
    Lines(Vec<String>),
    /// A sentence (byte-less value, table view node).
    Prose(String),
    /// The structure's own location lines say it (`StructureCard::location`).
    Location,
}

/// "Where" of `span` (a field's or the selection's); without a span: what the byte view shows
/// for a byte-less field, what a table view node covers, or `NO_BYTES`.
pub fn where_text(doc: &DocumentSession, sel: &SelectionState, span: Option<Span>) -> WhereText {
    match span {
        Some(s) => WhereText::Lines(provenance::where_lines(doc.view(), &s)),
        None => match sel.shown() {
            // The byte view shows the enclosing structure's first byte (design B4).
            Some(at) => WhereText::Prose(provenance::byteless_where(doc.view(), &at)),
            // A table view node lists structures across the file (0020 §5); only a value
            // without bytes says "No bytes".
            None => WhereText::Prose(doc.view().and_then(|v| provenance::view_where(doc.view_kind(), v, doc.file_len())).unwrap_or_else(|| NO_BYTES.to_string())),
        },
    }
}

/// Failures of a node or structure: none, one (its Diagnosis grid and the failing row to
/// select), or a summary list.
#[derive(Clone, Debug, PartialEq)]
pub enum Failures {
    None,
    One { diag: Diagnosis, select: Option<(usize, String)> },
    Many(Vec<NodeFailure>),
}

impl Failures {
    fn of(fails: Vec<NodeFailure>) -> Self {
        match fails.as_slice() {
            [] => Failures::None,
            [one] => Failures::One { diag: diagnosis::diagnosis(one.status, one.diag.as_ref()), select: one.row.map(|r| (r, one.name.clone())) },
            _ => Failures::Many(fails),
        }
    }
    pub fn is_none(&self) -> bool {
        matches!(self, Failures::None)
    }
}

/// The whole structure selected (tree click, structure step, navigation without a target row).
#[derive(Clone, Debug)]
pub struct StructureCard<'a> {
    /// The tree label with its coverage (`TreeNode::full_label`), else the view title.
    pub title: String,
    pub status: Option<Status>,
    /// The view's own word for its status chip (only with a view status).
    pub status_word: Option<&'static str>,
    /// The view title under Meaning when the title is the tree label.
    pub subtitle: Option<&'a str>,
    /// What the structure is, when there is a sentence for it (0030 B-9: the Observed levels
    /// card says what it lists instead of repeating its title).
    pub meaning: Option<&'static str>,
    pub note: Option<&'a str>,
    pub where_: WhereText,
    pub location: &'a [WhereLine],
    pub failures: Failures,
    pub legend: Vec<(Option<Status>, &'static str)>,
    /// "Referenced from" back-link of a navigation.
    pub via: Option<&'a str>,
    /// The decompressed space of compressed contents ("Open decompressed view").
    pub decompressed: Option<&'a SpaceInfo>,
}

/// Node summary when nothing is selected.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeCard<'a> {
    pub title: &'a str,
    pub status: Option<Status>,
    pub failures: Failures,
    /// Unsupported / Unknown / Limited statement of the node.
    pub other: Option<Finding>,
    pub legend: Vec<(Option<Status>, &'static str)>,
}

/// A structure sub-range or raw range (no single field).
#[derive(Clone, Debug)]
pub struct RangeCard<'a> {
    /// Structure bytes (else a raw range).
    pub structure: bool,
    pub note: Option<&'a str>,
    pub where_: WhereText,
    /// Compressed stored bytes: the decompressed space that holds the fields.
    pub decompressed: Option<&'a SpaceInfo>,
}

/// The tree node of the structure shown (the selected node when it is the view's node).
pub fn structure_node(doc: &DocumentSession) -> Option<&TreeNode> {
    doc.sel_node().and_then(|i| doc.nodes().get(i)).filter(|n| Some(&n.kind) == doc.view_kind())
}

fn legend(view: &View) -> Vec<(Option<Status>, &'static str)> {
    view.tally.map(|t| diagnosis::table_legend(&t)).unwrap_or_default()
}

pub fn structure_card<'a>(doc: &'a DocumentSession, sel: &'a SelectionState) -> Option<StructureCard<'a>> {
    let view = doc.view()?;
    let tree = structure_node(doc);
    let title = tree.map(|n| n.full_label()).unwrap_or_else(|| view.title.clone());
    let span = sel.span();
    let meaning = (doc.view_kind() == Some(&crate::model::NodeKind::ManifestLevels) && !view.lsm.is_empty()).then_some(LEVELS_MEANING);
    Some(StructureCard {
        status: view.status.or(tree.filter(|n| !n.unchecked()).map(|n| n.shown_status())),
        status_word: view.status_word.filter(|_| view.status.is_some()),
        subtitle: (title != view.title && meaning.is_none()).then_some(view.title.as_str()),
        meaning,
        title,
        note: sel.raw_note(),
        // The location lines' first line is the structure's range (0020 §1); the shared range
        // service only for a selection that is not the whole structure.
        where_: if meaning.is_some() {
            // The levels view has no bytes of its own: its source (0030 B-8, UI final §5.2).
            WhereText::Prose(format!("No bytes in this file: {}.", crate::model::NO_BYTES_MANIFEST))
        } else if provenance::where_is_location(view, span) {
            WhereText::Location
        } else {
            where_text(doc, sel, span)
        },
        location: &view.location,
        failures: Failures::of(diagnosis::node_failures(view, tree.map(|n| n.status), tree.and_then(|n| n.diag.as_deref()))),
        legend: legend(view),
        via: sel.via(),
        decompressed: view.spaces.iter().find(|sp| matches!(sp.map, SpaceMap::Compressed { .. })),
    })
}

/// Node-level card when no row is selected; None when there is nothing node-specific to say.
pub fn node_card(doc: &DocumentSession) -> Option<NodeCard<'_>> {
    let view = doc.view()?;
    let tree = doc.sel_node().and_then(|i| doc.nodes().get(i));
    let status = view.status.or(tree.map(|n| n.status));
    let fails = diagnosis::node_failures(view, tree.map(|n| n.status), tree.and_then(|n| n.diag.as_deref()));
    let other = status.filter(|s| matches!(s, Status::Unsupported | Status::Unknown | Status::Limited));
    if fails.is_empty() && other.is_none() && view.tally.is_none() {
        return None;
    }
    let other = other.and_then(|s| diagnosis::finding(s, diagnosis::statement_diag(s, tree.and_then(|n| n.diag.as_deref()).into_iter().chain(view.diagnostics.iter())), "this structure"));
    Some(NodeCard { title: &view.title, status, failures: Failures::of(fails), other, legend: legend(view) })
}

pub fn range_card<'a>(doc: &'a DocumentSession, sel: &'a SelectionState, s: Span) -> RangeCard<'a> {
    RangeCard { structure: sel.is_structure(), note: sel.raw_note(), where_: where_text(doc, sel, Some(s)), decompressed: doc.view().and_then(|v| provenance::compressed_container(v, &s)) }
}

/// A selected levels row / map box (0024 §5–6, 0026 §5.2): the file's facts from the one
/// builder the map tooltip uses (`map_facts::sst_facts`), so nothing is lost with tooltips off.
#[derive(Clone, Debug, PartialEq)]
pub struct LsmFileCard {
    pub name: String,
    /// (label, value) of `typed` (kept for callers that read values by label).
    pub facts: Vec<(&'static str, String)>,
    /// The typed facts with their source tags.
    pub typed: Vec<MapFact>,
    /// The x-axis sentence of the file's CF.
    pub legend: String,
    /// How to open it, or why it cannot be opened.
    pub open: String,
    pub on_disk: bool,
    /// The Derivation line of a levels row (replaces the generic status legend: a missing
    /// SST is missing on disk, not an absent optional format part).
    pub derivation: String,
}

pub fn lsm_file_card(doc: &DocumentSession, row: usize) -> Option<LsmFileCard> {
    let (c, l, f) = doc.view()?.lsm.by_row(row)?;
    let typed = map_facts::sst_facts(c, l, f);
    Some(LsmFileCard {
        name: f.name(),
        facts: typed.iter().map(|x| (x.label, x.value.clone())).collect(),
        typed,
        legend: c.order.legend(),
        open: map_facts::sst_hint(f),
        on_disk: f.on_disk(),
        derivation: if f.on_disk() {
            format!("✔ Live in the MANIFEST (after replaying all edits) and present on disk as {}.", f.name())
        } else {
            format!("○ Missing on disk: the MANIFEST lists {} as live, but the file is not in this folder.", f.name())
        },
    })
}

/// A map object card that is not a levels row (0026 §5): a WAL, the older-WAL summary or a
/// level. Same facts as its tooltip.
#[derive(Clone, Debug, PartialEq)]
pub struct MapCard {
    pub title: String,
    /// One fixed sentence under the title (what the object is), if any.
    pub note: Option<&'static str>,
    pub facts: Vec<MapFact>,
    /// How to open it (WAL), or None.
    pub open: Option<String>,
    /// The WAL number the open button opens.
    pub opens: Option<u64>,
}

/// The card of the map's picked object (the levels view shown, a WAL / older-WAL summary /
/// level picked on the map, no row selected).
pub fn map_card(doc: &DocumentSession) -> Option<MapCard> {
    let v = doc.view().filter(|_| doc.view_kind() == Some(&crate::model::NodeKind::ManifestLevels))?;
    let inv = &v.lsm;
    let shown = doc.map().shown_cf(inv);
    match doc.map().pick()? {
        MapPick::Wal(n) => {
            let f = doc.wal_inventory()?.by_number(n)?;
            Some(MapCard { title: f.name.clone(), note: Some(map_facts::WAL_NOTE), facts: map_facts::wal_facts(f, inv, shown), open: Some(map_facts::wal_hint(f)), opens: (!f.symlink).then_some(n) })
        }
        MapPick::OlderWals(newest) => {
            let older = map_facts::older_wals(&doc.wal_inventory()?.files, newest);
            Some(MapCard { title: map_facts::older_title(older.len()), note: Some(map_facts::WAL_NOTE), facts: map_facts::older_wals_facts(older, inv), open: None, opens: None })
        }
        MapPick::Level(level) => {
            let c = inv.cf(shown?)?;
            let l = c.levels.iter().find(|l| l.level == level)?;
            Some(MapCard { title: map_facts::level_title(c, l), note: None, facts: map_facts::level_facts(c, l), open: None, opens: None })
        }
    }
}

/// The levels view summary (nothing or the whole view selected; 0030 B-9): one line per CF
/// (`CF 0 default · 9 files · 16.3 KiB · L0 3 · L1 3 · L2 3`, missing files counted), the
/// comparator once (per CF when they differ), the axis sentence once at the end, truncation.
pub fn levels_summary(doc: &DocumentSession) -> Vec<String> {
    let Some(v) = doc.view().filter(|v| !v.lsm.is_empty()) else { return Vec::new() };
    let mut out = Vec::new();
    for c in &v.lsm.cfs {
        let mut line = format!("CF {} {} · {} · {}", c.id, c.name, files_word(c.files()), super::lsm_map::short_size(c.bytes()));
        for l in &c.levels {
            line.push_str(&format!(" · L{} {}", l.level, l.files.len()));
        }
        let missing: usize = c.levels.iter().map(|l| l.files.len() - l.on_disk).sum();
        if missing > 0 {
            line.push_str(&format!(" · {missing} missing"));
        }
        out.push(line);
    }
    out.extend(v.lsm.comparator_lines());
    out.extend(v.lsm.legend_lines());
    if v.lsm.truncated {
        out.push(format!("Listing truncated at {} live files.", crate::limits::MAX_LIVE_FILES));
    }
    out
}

/// What the Observed levels view is (0030 B-9: the structure card's Meaning, instead of
/// repeating its title).
pub const LEVELS_MEANING: &str = "Live SST files of each column family and level, as the MANIFEST's edits leave them (final state, not compaction history).";

/// The status legend sentence of a status (Explanation, Help, chips and table tooltips).
pub fn status_meaning(s: Status) -> &'static str {
    match s {
        Status::Present => "Present: decoded from bytes in the file.",
        Status::NotPresent => "Not present: the format says this optional part is absent here.",
        Status::Implicit => "Implicit: derived or defaulted; no on-disk byte range.",
        Status::Unsupported => "Unsupported: exists but RocksVision does not decode it.",
        Status::Unknown => "Unknown: not identifiable (unknown tag/type, stale or unreadable context).",
        Status::Invalid => "Invalid: checksum mismatch or inconsistent structure.",
        Status::Truncated => "Truncated: the file ends before this structure does.",
        Status::Limited => "Resource limit: decoding stopped at a configured bound.",
    }
}

/// What the field card shows besides the field's own text (0030 B-8, architect S3).
#[derive(Clone, Debug, PartialEq)]
pub struct FieldBody {
    /// The value line (skipped when empty or equal to the field name).
    pub show_value: bool,
    /// One muted line instead of a `Where` heading for a value without bytes in this file.
    pub no_bytes: Option<String>,
}

/// The field card's structure rules: no value line repeating the field name; without bytes, one
/// "No bytes in this file: <source>." line (the field's typed source, else the byte-less
/// sentence of `where_text`) instead of a `Where` heading.
pub fn field_body(doc: &DocumentSession, sel: &SelectionState, f: &Field) -> FieldBody {
    let no_bytes = match (&f.span, &f.no_bytes_source) {
        (Some(_), _) => None,
        (None, Some(src)) => Some(format!("No bytes in this file: {src}.")),
        (None, None) => match where_text(doc, sel, None) {
            WhereText::Prose(t) => Some(t),
            _ => None,
        },
    };
    FieldBody { show_value: !f.value.is_empty() && f.value != f.name, no_bytes }
}

/// The status sentence of a field (architect N1): Untrusted, else its Not-checked derivation,
/// else the status legend sentence (table tooltips and the Explanation chip share it).
pub fn field_status_meaning(f: &Field) -> &str {
    if f.untrusted {
        diagnosis::UNTRUSTED_MEANING
    } else if let Some(d) = &f.not_checked {
        d
    } else {
        status_meaning(f.status)
    }
}

/// Joins the full text of an elided cell / header and its meaning lines (blank line between).
fn tip_of(shown: &str, elided: bool, meaning: Vec<String>) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if elided && !shown.is_empty() {
        parts.push(crate::util::truncate_chars(shown, crate::limits::TIP_TEXT_CHARS));
    }
    if !meaning.is_empty() {
        parts.push(meaning.join("\n"));
    }
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// 0030 B-6: the tooltip of a table cell — `shown` is the text the cell displays, `elided`
/// whether it was cut. The full text only when elided, plus a meaning line only for: the CF
/// and File cells of the levels view, the Status column, a byte-less Offset/Len. None when the
/// cell is fully visible and self-explanatory (never the visible text alone; nothing invented:
/// the comparator line only when the MANIFEST recorded one).
pub fn cell_tip(view: &View, row: usize, col: usize, shown: &str, elided: bool) -> Option<String> {
    let r = view.rows.get(row)?;
    let role = view.columns.get(col)?.role;
    let f = &r.field;
    let mut meaning = Vec::new();
    match role {
        ColumnRole::Status => meaning.push(field_status_meaning(f).to_string()),
        ColumnRole::Offset | ColumnRole::Len if f.span.is_none() => meaning.push(NO_BYTES.to_string()),
        ColumnRole::CfName | ColumnRole::Cf => {
            if let Some((c, _, _)) = view.lsm.by_row(row) {
                meaning.push(format!("Column family {} \"{}\" (per MANIFEST).", c.id, c.name));
                // The one relation sentence (the LSM file card / map tooltip say it too).
                let rel = map_facts::CF_RELATION;
                meaning.push(format!("{}{}.", rel[..1].to_uppercase(), &rel[1..]));
                if c.comparator.is_some() {
                    meaning.push(format!("Comparator: {}", c.order.describe()));
                }
            }
        }
        ColumnRole::FileNo => {
            if let Some((_, _, file)) = view.lsm.by_row(row) {
                // The one open hint (map tooltip and LSM file card).
                meaning.push(if file.on_disk() { format!("{} · {}", file.name(), map_facts::sst_hint(file)) } else { map_facts::sst_hint(file) });
            }
        }
        _ => {}
    }
    tip_of(shown, elided, meaning)
}

/// 0030 B-6: the tooltip of a column header: its full title when elided, plus the meaning of
/// the levels view's key and sequence columns.
pub fn header_tip(view: &View, col: usize, elided: bool) -> Option<String> {
    let c = view.columns.get(col)?;
    let mut meaning = Vec::new();
    if !view.lsm.is_empty() {
        match c.role {
            ColumnRole::Key => meaning.push("Smallest / largest internal key per MANIFEST: \"user key\" @sequence t<value type>".to_string()),
            ColumnRole::Seq => meaning.push("Sequence number range of the file's entries (per MANIFEST)".to_string()),
            _ => {}
        }
    }
    tip_of(c.title, elided, meaning)
}

/// "LSM Map: unavailable — <reason>" for the root/node card (the disabled button's reason,
/// visible without tooltips); None when the map is available or nothing is open.
pub fn lsm_map_unavailable(doc: &DocumentSession) -> Option<String> {
    if doc.file().is_none() && doc.dir().is_none() {
        return None;
    }
    doc.lsm_map_availability().err().map(|r| format!("LSM Map: unavailable — {}", r.0))
}

/// The status-bar selection segment, from the same `subject` (mirror of the Explanation
/// "Where": file range(s) first, then the derived offset).
pub fn selection_status(doc: &DocumentSession, sel: &SelectionState) -> String {
    let structure = |span: Option<Span>| {
        // The whole structure: "selection: structure Physical blocks · file [0x0,0x37d00)".
        let name = structure_node(doc).map(|n| n.label.clone()).or_else(|| doc.view().map(|v| v.title.clone())).unwrap_or_default();
        match span {
            Some(s) if s.space == crate::model::Space::File => format!("selection: structure {name} · {}", s.range_text()),
            Some(s) => format!("selection: structure {name} · {}", provenance::status_text(doc.view(), &s)),
            None => format!("selection: structure {name}"),
        }
    };
    let range = || sel.span().map(|s| format!("selection: {}", provenance::status_text(doc.view(), &s))).unwrap_or_default();
    match subject(doc, sel) {
        // A field without bytes: what the byte view shows instead (design B5).
        Subject::Field { field, .. } => match sel.shown() {
            Some(at) => provenance::byteless_status(&field.name, &at),
            None => range(),
        },
        Subject::WholeStructure | Subject::PendingStructure => structure(sel.span()),
        Subject::StructureRange { span, clicked: false } => structure(Some(span)),
        Subject::StructureRange { clicked: true, .. } | Subject::Raw(_) | Subject::Node | Subject::Empty => range(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::selection::SelOrigin;
    use crate::model::{ChildRole, NodeKind, Row, Space};

    /// A view of an example file (decoded as the worker does).
    fn example_view(rel: &str, k: NodeKind) -> View {
        let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
        crate::inspect::open(&p).unwrap().view(&k, &|| false)
    }

    /// Architect §5.4: a fully visible cell never gets a tooltip that only repeats it — in the
    /// advanced example's levels view, its 000150.log root view and its record list.
    #[test]
    fn cell_tip_never_repeats_the_visible_text() {
        let views = [
            example_view("examples/data/multilevel-advanced-db/CURRENT", NodeKind::ManifestLevels),
            example_view("examples/data/multilevel-advanced-db/000150.log", NodeKind::Root),
            example_view("examples/data/multilevel-advanced-db/000150.log", NodeKind::LogRecords { start: 0, page: 0 }),
        ];
        let mut checked = 0;
        for v in &views {
            assert!(!v.rows.is_empty());
            for (ri, r) in v.rows.iter().enumerate() {
                for (ci, cell) in r.cells.iter().enumerate() {
                    if let Some(t) = cell_tip(v, ri, ci, cell, false) {
                        assert_ne!(&t, cell, "{}: row {ri} col {ci}", v.title);
                        assert!(t.lines().all(|l| l != cell), "{}: row {ri} col {ci}: {t:?}", v.title);
                    }
                    checked += 1;
                }
            }
        }
        assert!(checked > 100, "{checked}");
    }

    /// Architect §5.5: the CF cell tooltip and the LSM file card share the one relation
    /// sentence; the File cell tooltip is the map's open hint.
    #[test]
    fn cf_cell_tip_and_sst_card_share_the_relation() {
        let v = example_view("examples/data/multilevel-advanced-db/CURRENT", NodeKind::ManifestLevels);
        let col = |role: ColumnRole| v.columns.iter().position(|c| c.role == role).unwrap();
        let (c, l, f) = v.lsm.by_row(0).unwrap();
        let rel = &map_facts::CF_RELATION[1..];
        let cf_tip = cell_tip(&v, 0, col(ColumnRole::CfName), &v.rows[0].cells[col(ColumnRole::CfName)], false).unwrap();
        assert!(cf_tip.contains(rel), "{cf_tip}");
        let card = map_facts::sst_facts(c, l, f);
        assert!(card.iter().any(|x| x.label == "Column family" && x.value.ends_with(map_facts::CF_RELATION_SHORT)), "{card:?}");
        assert!(map_facts::CF_RELATION.starts_with(map_facts::CF_RELATION_SHORT), "one sentence, two lengths");
        // UI final §5.2: the levels row's card says what it is once, with its source.
        let f0 = &v.rows[0].field;
        assert_eq!(f0.explain, "Live SST file per the MANIFEST (after replaying all edits).");
        let b = field_body(&doc(NodeKind::Root), &SelectionState::default(), f0);
        assert_eq!(b.no_bytes.as_deref(), Some("No bytes in this file: from the MANIFEST."));
        let file_tip = cell_tip(&v, 0, col(ColumnRole::FileNo), &v.rows[0].cells[col(ColumnRole::FileNo)], false).unwrap();
        assert!(file_tip.contains(&map_facts::sst_hint(f)), "{file_tip}");
    }

    /// Architect S3: the field card's structure rules (app-side): the MANIFEST-evidence row says
    /// where its value comes from; a value equal to the field name is not repeated.
    #[test]
    fn field_body_rules() {
        let s = doc(NodeKind::Root);
        let sel = SelectionState::default();
        let mut f = Field::new("level (MANIFEST)", None, "L1");
        f.no_bytes_source = Some("from the folder's MANIFEST (read 2026-11-06 12:00:00 UTC)".into());
        let b = field_body(&s, &sel, &f);
        assert_eq!(b.no_bytes.as_deref(), Some("No bytes in this file: from the folder's MANIFEST (read 2026-11-06 12:00:00 UTC)."));
        assert!(b.show_value);
        let same = Field::new("default", None, "default");
        assert!(!field_body(&s, &sel, &same).show_value);
        let with_bytes = Field::new("x", Some(Span::file(0, 1)), "1");
        assert_eq!(field_body(&s, &sel, &with_bytes).no_bytes, None, "bytes: a Where heading instead");
    }

    /// A view of a compressed block @0x100 (stored [0x100,0x140), 0x40 B incl. trailer) with a
    /// file field, a byte-less field and a decompressed field.
    fn doc(kind: NodeKind) -> DocumentSession {
        let blk = Space::Block(0x100);
        let row = |name: &str, s: Option<Span>| Row { cells: vec![name.into()], field: Field::new(name, s, "v") };
        let view = View {
            title: "Data block @0x100".into(),
            rows: vec![row("type", Some(Span::file(0x13b, 1))), row("reassembly", None), row("entry 0", Some(Span::new(blk, 0, 9)))],
            extent: Some(Span::file(0x100, 0x40)),
            location: vec![WhereLine { text: "file [0x100,0x140) · 64 B stored -> block@0x100 [+0x0,+0x90) · 144 B decompressed".into(), link: None }],
            spaces: vec![SpaceInfo { space: blk, size: 0x90, title: "Decompressed".into(), map: SpaceMap::Compressed { file: Span::file(0x100, 0x3b), codec: "ZSTD".into() } }],
            ..Default::default()
        };
        let mut d = DocumentSession::new();
        d.push_test_node(TreeNode {
            label: "Data block @0x100".into(),
            kind: kind.clone(),
            status: Status::Present,
            leaf: false,
            depth: 0,
            children: None,
            expanded: false,
            loading: false,
            diag: None,
            contains: None,
            role: ChildRole::Structure,
            checked: None,
            tally: None,
        });
        d.select_node(Some(0));
        d.set_test_view(kind, view);
        d
    }

    /// 0026 items 4–5: WAL lifecycle from the MANIFEST's per-CF log numbers only (RocksDB 10.4.2
    /// version_set.h 1448–1462), and every fact marked observed / derived / unknown / partial.
    #[test]
    fn wal_facts_lifecycle_and_basis() {
        use crate::model::{LsmCf, LsmInventory, ManifestLogNumbers, NotScannedWhy, WalFile, WalLifecycle, WalStats, WalSummary};
        use std::collections::{BTreeMap, BTreeSet};
        let ln = |active: bool, per: &[(u32, u64)], without: &[u32]| ManifestLogNumbers {
            per_cf: per.iter().copied().collect::<BTreeMap<_, _>>(),
            without: without.iter().copied().collect::<BTreeSet<_>>(),
            active,
            ..Default::default()
        };
        let both = ln(true, &[(0, 9), (1, 7)], &[]);
        assert_eq!(WalLifecycle::of(Some(6), &both), WalLifecycle::FlushedPerManifest);
        assert_eq!(WalLifecycle::of(Some(7), &both), WalLifecycle::MayHoldUnflushed { cfs: vec![1] });
        assert_eq!(WalLifecycle::of(Some(9), &both), WalLifecycle::MayHoldUnflushed { cfs: vec![0, 1] });
        assert!(matches!(WalLifecycle::of(None, &both), WalLifecycle::Unknown(_)), "no number: never from a guess");
        assert!(matches!(WalLifecycle::of(Some(1), &ln(false, &[(0, 9)], &[])), WalLifecycle::Unknown(_)), "MANIFEST not active");
        assert!(matches!(WalLifecycle::of(Some(1), &ln(true, &[], &[])), WalLifecycle::Unknown(_)), "no log numbers");
        assert!(matches!(WalLifecycle::of(Some(1), &ln(true, &[(0, 9)], &[3])), WalLifecycle::Unknown(_)), "a live CF without a log number");
        assert_eq!(both.min_unflushed(), Some(7));
        let inv = LsmInventory { cfs: vec![LsmCf { id: 0, name: "default".into(), ..Default::default() }], log_numbers: both, ..Default::default() };
        let mut f =
            WalFile { number: Some(8), path: "/db/000008.log".into(), name: "000008.log".into(), size: 100, mtime: None, symlink: false, summary: WalSummary::NotScanned(NotScannedWhy::Pending) };
        let fact = |f: &WalFile, l: &str| map_facts::wal_facts(f, &inv, Some(0)).into_iter().find(|x| x.label == l).unwrap();
        assert_eq!(fact(&f, "Lifecycle").source, map_facts::Source::Derived);
        assert!(fact(&f, "Lifecycle").value.contains("may hold unflushed writes for CF 1 (per MANIFEST)"), "{:?}", fact(&f, "Lifecycle"));
        assert_eq!(fact(&f, "Shown CF").certainty, map_facts::Certainty::Unknown, "unscanned: membership unknown");
        let st = WalStats { records: 3, batches: 2, cfs: BTreeSet::from([0]), seq: Some((5, 9)), bytes_scanned: 60 };
        f.summary = WalSummary::Done(WalStats { cfs: BTreeSet::from([2]), ..st });
        assert_eq!(fact(&f, "Shown CF").value, "default: none seen (complete scan to the end of the file)");
        // 0030 A6: a stale tail ends the log; the scan is complete but not "whole file".
        let done = f.summary.clone();
        f.summary = WalSummary::StaleTail { stats: WalStats { records: 3, batches: 2, cfs: BTreeSet::from([2]), seq: Some((5, 9)), bytes_scanned: 60 }, at: 0x40 };
        assert_eq!(fact(&f, "Shown CF").value, "default: none seen (complete scan of this log's records, which end at the old-record tail at 0x40)");
        f.summary = done;
        assert_eq!(fact(&f, "Seq range").value, "5..9");
    }

    /// 0020 §3: one table over the selections; subject, where and the status segment are
    /// asserted together so the card and the status bar cannot drift apart.
    #[test]
    fn subject_where_and_status_agree() {
        let block = NodeKind::SstBlock { off: 0x100, size: 0x3b, kind: crate::model::BlockKind::Data };
        let d = doc(block.clone());
        let lines = |v: &[&str]| WhereText::Lines(v.iter().map(|s| s.to_string()).collect());
        type Make = fn(&mut SelectionState);
        let cases: Vec<(&str, Make, &str, WhereText, &str)> = vec![
            ("field", |s| s.select_field(0, Some(Span::file(0x13b, 1)), vec![], SelOrigin::Row, None), "Field", lines(&["file [0x13b,0x13c) · 1 B"]), "selection: file [0x13b,0x13c) · 1 B"),
            (
                "byte-less field (shown)",
                |s| {
                    s.select_field(1, None, vec![], SelOrigin::Row, None);
                    s.set_shown(Some(Span::file(0x100, 1)));
                },
                "Field",
                WhereText::Prose("No bytes of its own (derived, inferred, default or metadata value). The hex view shows the start of the enclosing Data block @0x100: file 0x100.".into()),
                "selection: reassembly · no bytes · showing file 0x100",
            ),
            ("whole, no span", |s| s.select_whole(None, "n".into(), SelOrigin::Tree, None), "WholeStructure", WhereText::Location, "selection: structure Data block @0x100"),
            (
                "whole = extent",
                |s| s.select_whole(Some(Span::file(0x100, 0x40)), "n".into(), SelOrigin::Tree, None),
                "WholeStructure",
                WhereText::Location,
                "selection: structure Data block @0x100 · file [0x100,0x140) · 64 B",
            ),
            (
                "structure sub-range",
                |s| s.select_structure(Span::file(0x13b, 5), "trailer".into(), SelOrigin::Navigate, None),
                "StructureRange",
                lines(&["file [0x13b,0x140) · 5 B"]),
                "selection: structure Data block @0x100 · file [0x13b,0x140) · 5 B",
            ),
            ("raw", |s| s.select_raw(Span::file(0x104, 4), "r"), "Raw", lines(&["file [0x104,0x108) · 4 B"]), "selection: file [0x104,0x108) · 4 B"),
            (
                "compressed file range, clicked byte",
                |s| s.select_structure(Span::file(0x100, 0x3b), "c".into(), SelOrigin::Hex, Some(Span::file(0x104, 1))),
                "StructureRange",
                lines(&["file [0x100,0x13b) · 59 B"]),
                "selection: file [0x100,0x13b) · 59 B",
            ),
            (
                "decompressed field",
                |s| s.select_field(2, Some(Span::new(Space::Block(0x100), 0, 9)), vec![], SelOrigin::Row, None),
                "Field",
                lines(&["Contained in compressed block: file [0x100,0x13b) · 59 B (ZSTD); no byte-to-byte mapping to file offsets", "block@0x100 [+0x0,+0x9) · 9 B"]),
                "selection: in file [0x100,0x13b) (ZSTD, no byte mapping) · block@0x100 [+0x0,+0x9) · 9 B",
            ),
            ("nothing selected", |s| s.clear(), "Node", WhereText::Prose(NO_BYTES.into()), ""),
        ];
        for (what, make, subj, want_where, want_status) in cases {
            let mut sel = SelectionState::new();
            make(&mut sel);
            let s = subject(&d, &sel);
            assert_eq!(format!("{s:?}").split([' ', '(']).next(), Some(subj), "{what}: {s:?}");
            let w = match &s {
                Subject::Field { field, .. } => where_text(&d, &sel, field.span),
                Subject::WholeStructure => structure_card(&d, &sel).unwrap().where_,
                Subject::StructureRange { span: sp, .. } | Subject::Raw(sp) => range_card(&d, &sel, *sp).where_,
                _ => where_text(&d, &sel, None),
            };
            assert_eq!(w, want_where, "{what}");
            assert_eq!(selection_status(&d, &sel), want_status, "{what}");
        }
        // The compressed stored range offers the decompressed view; a byte after it does not.
        let mut sel = SelectionState::new();
        sel.select_raw(Span::file(0x13a, 1), "r");
        assert_eq!(range_card(&d, &sel, Span::file(0x13a, 1)).decompressed.map(|s| s.space), Some(Space::Block(0x100)));
        assert!(range_card(&d, &sel, Span::file(0x13b, 1)).decompressed.is_none());
        // Whole-structure card: tree title, location lines, the decompressed space.
        sel.select_whole(None, "n".into(), SelOrigin::Tree, Some("entry 2 of Index block @0x0".into()));
        let c = structure_card(&d, &sel).unwrap();
        assert_eq!((c.title.as_str(), c.subtitle, c.location.len(), c.via, c.failures.is_none()), ("Data block @0x100", None, 1, Some("entry 2 of Index block @0x0"), true));
        assert_eq!(c.decompressed.map(|s| s.space), Some(Space::Block(0x100)));
        // A table view node: its own Where sentence, and nothing at all without a view.
        let layout = doc(NodeKind::SstLayout);
        assert_eq!(where_text(&layout, &SelectionState::new(), None), WhereText::Prose("Whole file: file [0x0,0x0) · 0 B. Select a row for its byte range.".into()));
        let empty = DocumentSession::new();
        assert!(matches!(subject(&empty, &SelectionState::new()), Subject::Empty));
        assert_eq!(selection_status(&empty, &SelectionState::new()), "");
        // D1 (architect leak 1): the status bar's split comes from `subject` only — a clicked
        // byte in a structure range is a range, an unclicked one a structure; a whole structure
        // without a view names it.
        let mut sel = SelectionState::new();
        sel.select_structure(Span::file(0x100, 0x3b), "c".into(), SelOrigin::Hex, Some(Span::file(0x104, 1)));
        assert!(matches!(subject(&d, &sel), Subject::StructureRange { clicked: true, .. }));
        sel.select_structure(Span::file(0x13b, 5), "t".into(), SelOrigin::Navigate, None);
        assert!(matches!(subject(&d, &sel), Subject::StructureRange { clicked: false, .. }));
        sel.select_whole(None, "n".into(), SelOrigin::Tree, None);
        assert!(matches!(subject(&empty, &sel), Subject::PendingStructure));
        assert_eq!(selection_status(&empty, &sel), "selection: structure ");
        assert!(node_card(&empty).is_none());
    }
}
