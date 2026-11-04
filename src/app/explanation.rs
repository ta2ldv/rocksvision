//! Explanation model (egui-free, 0020 §3): the one classifier of the current selection
//! (`subject`) and the cards the Explanation panel renders and the status-bar segment derived
//! from it, so the card and the status bar cannot drift apart. It only selects and composes
//! `provenance` (ranges, where, status) and `diagnosis` (typed findings); it never reads files.

use super::diagnosis::{self, Diagnosis, Finding, NodeFailure};
use super::provenance;
use super::selection::SelectionState;
use super::session::{DocumentSession, TreeNode};
use crate::model::{Field, SpaceInfo, SpaceMap, Span, Status, View, WhereLine};

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
    Some(StructureCard {
        status: view.status.or(tree.filter(|n| !n.unchecked()).map(|n| n.shown_status())),
        status_word: view.status_word.filter(|_| view.status.is_some()),
        subtitle: (title != view.title).then_some(view.title.as_str()),
        title,
        note: sel.raw_note(),
        // The location lines' first line is the structure's range (0020 §1); the shared range
        // service only for a selection that is not the whole structure.
        where_: if provenance::where_is_location(view, span) { WhereText::Location } else { where_text(doc, sel, span) },
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
