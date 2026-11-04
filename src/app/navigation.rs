//! Navigation (egui-free, 0020 §1): the history and the one view request in flight, with every
//! guard of its reply in one place — the request id, the selection epoch at the request (a
//! newer user choice overtakes it), what the request did to the history (undone when it is
//! overtaken) and the selection to apply when it arrives. `plan_arrival` is the pure policy of
//! that arrival selection. Owned by `DocumentSession` (reset with the generation); `AppState`
//! issues the decode request and executes the plan.

use super::jobs::{SelectHint, ViewTarget};
use super::provenance::{self, field_at};
use super::selection::{SelOrigin, SelSnapshot};
use crate::limits::LIMITS;
use crate::model::{NodeKind, Space, Span, View};

/// One history step: the view, its selection, the byte-view position (space + the offset
/// shown) and the Go-to text, restored together by Back.
#[derive(Clone, Debug, PartialEq)]
pub struct HistEntry {
    pub kind: NodeKind,
    pub sel: SelSnapshot,
    pub hex_space: Space,
    pub hex_at: u64,
    pub goto: String,
}

/// Where the user is (what Back restores): selection, byte-view space and offset, Go-to text.
#[derive(Clone, Debug, PartialEq)]
pub struct Here {
    pub sel: SelSnapshot,
    pub hex_space: Space,
    pub hex_at: u64,
    pub goto: String,
}

/// Selection to apply when the requested view arrives — only if the selection epoch is still
/// the one recorded at the request (a newer user choice always wins).
#[derive(Clone, Debug, PartialEq)]
pub struct PendingSel {
    pub epoch: u64,
    pub origin: SelOrigin,
    pub anchor: Option<Span>,
    pub hint: Option<SelectHint>,
    /// Origin of a navigation, for the structure card ("Referenced from").
    pub via: Option<String>,
    /// Back: the saved selection, byte-view position and Go-to text to restore as they were.
    pub restore: Option<Here>,
}

impl PendingSel {
    pub fn new(epoch: u64, origin: SelOrigin, anchor: Option<Span>, hint: Option<SelectHint>) -> Self {
        PendingSel { epoch, origin, anchor, hint, via: None, restore: None }
    }
}

/// What a view request does to the history.
#[derive(Clone, Debug, PartialEq)]
pub enum HistoryOp {
    None,
    /// Push the current view at this position (Navigate, tree click, located byte).
    Push(Here),
    /// Back popped this entry (put back if the request is overtaken).
    Popped(HistEntry),
}

/// What the request in flight did to the history (undone when it is overtaken).
#[derive(Clone, Debug, PartialEq)]
enum Applied {
    None,
    Pushed,
    Popped(Box<HistEntry>),
}

#[derive(Debug)]
struct InFlight {
    req: u64,
    target: ViewTarget,
    /// Selection epoch at the request, recorded only when a view is already shown (without one
    /// nothing newer can be overtaken: the view must arrive). A file change is covered by the
    /// generation.
    guard_epoch: Option<u64>,
    history: Applied,
    pending: Option<PendingSel>,
}

/// How a view reply is applied.
#[derive(Clone, Debug, PartialEq)]
pub enum ViewReply {
    /// Not the request in flight (older, superseded): ignored.
    NotOurs,
    /// A newer selection was made since the request: discarded entirely (no view, no
    /// selection, no reveal); the history is as it was before the request.
    Overtaken,
    /// The located byte has no structure: the raw selection stays.
    NothingLocated,
    /// Show the view; apply `pending` (already filtered by the selection epoch).
    Accepted { pending: Option<Box<PendingSel>> },
}

#[derive(Debug, Default)]
pub struct Navigation {
    history: Vec<HistEntry>,
    inflight: Option<InFlight>,
}

impl Navigation {
    /// The view requested and not answered yet.
    pub fn pending_target(&self) -> Option<&ViewTarget> {
        self.inflight.as_ref().map(|f| &f.target)
    }
    pub fn can_go_back(&self) -> bool {
        !self.history.is_empty()
    }
    #[cfg(test)]
    pub(super) fn history_len(&self) -> usize {
        self.history.len()
    }

    /// Records view request `req` (replacing any request in flight). `epoch_now` is the
    /// selection epoch after any selection change the request itself made; `current_kind` is
    /// the view shown (the history entry of a push).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn begin(&mut self, req: u64, target: ViewTarget, hist: HistoryOp, pending: Option<PendingSel>, epoch_now: u64, view_shown: bool, current_kind: Option<&NodeKind>) {
        let history = match (hist, current_kind) {
            (HistoryOp::Push(Here { sel, hex_space, hex_at, goto }), Some(kind)) => {
                let e = HistEntry { kind: kind.clone(), sel, hex_space, hex_at, goto };
                let pushed = self.history.last() != Some(&e);
                if pushed {
                    self.history.push(e);
                }
                self.cap();
                if pushed { Applied::Pushed } else { Applied::None }
            }
            (HistoryOp::Popped(h), _) => Applied::Popped(Box::new(h)),
            _ => Applied::None,
        };
        self.inflight = Some(InFlight { req, target, guard_epoch: view_shown.then_some(epoch_now), history, pending });
    }

    fn cap(&mut self) {
        if self.history.len() > LIMITS.history {
            self.history.remove(0);
        }
    }

    /// Back: the previous view's entry.
    pub(super) fn back(&mut self) -> Option<HistEntry> {
        self.history.pop()
    }

    /// Every guard of a view reply (`located`: the reply names a view).
    pub(super) fn on_view_reply(&mut self, req: u64, located: bool, epoch_now: u64) -> ViewReply {
        if self.inflight.as_ref().map(|f| f.req) != Some(req) {
            return ViewReply::NotOurs;
        }
        let Some(f) = self.inflight.take() else { return ViewReply::NotOurs };
        if f.guard_epoch.is_some_and(|e| e != epoch_now) {
            match f.history {
                Applied::Pushed => {
                    self.history.pop();
                }
                Applied::Popped(h) => {
                    self.history.push(*h);
                    self.cap();
                }
                Applied::None => {}
            }
            return ViewReply::Overtaken;
        }
        if !located {
            return ViewReply::NothingLocated;
        }
        ViewReply::Accepted { pending: f.pending.filter(|p| p.epoch == epoch_now).map(Box::new) }
    }

    /// The file closed: no history, nothing in flight.
    pub(super) fn reset(&mut self) {
        *self = Navigation::default();
    }
}

/// The selection an arrived view gets (`plan_arrival`).
#[derive(Clone, Debug, PartialEq)]
pub enum Arrival {
    /// Back: the exact previous selection, byte-view position and Go-to text.
    Restore(Here),
    /// A field row.
    Field { row: usize, origin: SelOrigin, anchor: Option<Span> },
    /// Structure bytes that are not one field; `reveal`: move the byte view to them.
    Structure { span: Span, note: String, origin: SelOrigin, anchor: Option<Span>, reveal: bool },
    /// The whole structure (its first byte is shown).
    Whole { extent: Option<Span>, note: String, origin: SelOrigin, via: Option<String> },
}

/// Applies the selection of an arrived view (navigation or located byte): the narrowest field
/// at the byte, else the row with exactly the span, else the structure range. A navigation
/// without a span selects the whole structure (`extent`).
pub fn plan_arrival(p: PendingSel, select: Option<SelectHint>, view: &View, extent: Option<Span>) -> Arrival {
    if let Some(here) = p.restore {
        return Arrival::Restore(here);
    }
    let hint = select.or(p.hint).unwrap_or(SelectHint { at: None, span: None });
    if let Some((space, off)) = hint.at
        && let Some(row) = field_at(view, space, off)
    {
        return Arrival::Field { row, origin: p.origin, anchor: p.anchor };
    }
    if let Some(s) = hint.span {
        if let Some(row) = view.rows.iter().position(|r| r.field.span == Some(s)) {
            return Arrival::Field { row, origin: p.origin, anchor: p.anchor };
        }
        // Navigation hint spans are file ranges (`Inspector::locate`, location links), so the
        // one compressed-container predicate applies (0020 §2).
        let note = match provenance::compressed_container(view, &s) {
            Some(sp) => format!(
                "Compressed contents of {} {}: stored bytes have no byte-to-byte mapping to decompressed fields. Open the decompressed view to select a field ({}).",
                view.title,
                s.range_text(),
                sp.space.label()
            ),
            None => format!("{} {}: structure bytes, not a single field.", view.title, s.range_text()),
        };
        return Arrival::Structure { span: s, note, origin: p.origin, anchor: p.anchor, reveal: p.origin != SelOrigin::Hex };
    }
    match p.anchor {
        // Located, but no field of that view holds the byte: the structure is the answer.
        Some(a) => Arrival::Structure {
            span: extent.unwrap_or(a),
            note: format!("{}: the clicked byte is part of this structure, not of a single field.", view.title),
            origin: p.origin,
            anchor: Some(a),
            reveal: false,
        },
        // Navigation without a target row, tree click, structure step: the whole structure
        // (never "Nothing selected"); the byte view shows its first byte.
        None => Arrival::Whole { extent, note: "The whole structure is selected (no single field). Select a row or a byte for a field.".into(), origin: p.origin, via: p.via },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Field, Row, SpaceInfo, SpaceMap};

    fn here(at: u64) -> Here {
        Here { sel: SelSnapshot::default(), hex_space: Space::File, hex_at: at, goto: format!("0x{at:x}") }
    }
    fn node() -> ViewTarget {
        ViewTarget::Node(NodeKind::SstFooter)
    }

    /// 0020 §1: every guard of a view reply, without a worker.
    #[test]
    fn on_view_reply_table() {
        let root = NodeKind::Root;
        // NotOurs: an older request.
        let mut n = Navigation::default();
        n.begin(5, node(), HistoryOp::None, None, 1, true, Some(&root));
        assert_eq!(n.on_view_reply(4, true, 1), ViewReply::NotOurs);
        assert_eq!(n.pending_target(), Some(&node()), "still in flight");
        // Overtaken + Push: the pushed entry is popped again.
        let mut n = Navigation::default();
        n.begin(1, node(), HistoryOp::Push(here(1)), None, 1, true, Some(&root));
        assert_eq!(n.history_len(), 1);
        n.begin(2, node(), HistoryOp::Push(here(2)), None, 1, true, Some(&root));
        assert_eq!(n.history_len(), 2);
        assert_eq!(n.on_view_reply(2, true, 2), ViewReply::Overtaken);
        assert_eq!((n.history_len(), n.pending_target()), (1, None), "history length restored");
        // Overtaken + Popped: the entry goes back on top.
        let e = n.back().unwrap();
        n.begin(3, node(), HistoryOp::Popped(e.clone()), None, 3, true, Some(&root));
        assert_eq!(n.on_view_reply(3, true, 4), ViewReply::Overtaken);
        assert_eq!(n.back(), Some(e), "entry back on top");
        // Accepted with a stale pending epoch: no pending selection.
        let mut n = Navigation::default();
        let p = PendingSel::new(6, SelOrigin::Navigate, None, None);
        n.begin(7, node(), HistoryOp::None, Some(p.clone()), 7, true, Some(&root));
        assert_eq!(n.on_view_reply(7, true, 7), ViewReply::Accepted { pending: None }, "pending epoch 6 ≠ 7");
        n.begin(8, node(), HistoryOp::None, Some(PendingSel { epoch: 7, ..p }), 7, true, Some(&root));
        assert!(matches!(n.on_view_reply(8, true, 7), ViewReply::Accepted { pending: Some(_) }));
        // No view yet (guard None): never Overtaken, whatever the epoch; no history push.
        let mut n = Navigation::default();
        n.begin(9, node(), HistoryOp::Push(here(0)), None, 1, false, None);
        assert_eq!((n.history_len(), n.on_view_reply(9, true, 99)), (0, ViewReply::Accepted { pending: None }));
        // Locate without a structure.
        let mut n = Navigation::default();
        n.begin(10, ViewTarget::Locate { space: Space::File, off: 3 }, HistoryOp::None, None, 1, true, Some(&root));
        assert_eq!(n.on_view_reply(10, false, 1), ViewReply::NothingLocated);
        // Push de-dup and the history cap.
        let mut n = Navigation::default();
        for i in 0..LIMITS.history + 5 {
            n.begin(i as u64, node(), HistoryOp::Push(here(i as u64)), None, 1, true, Some(&root));
            n.begin(i as u64, node(), HistoryOp::Push(here(i as u64)), None, 1, true, Some(&root));
        }
        assert_eq!(n.history_len(), LIMITS.history);
        n.reset();
        assert!(!n.can_go_back() && n.pending_target().is_none());
    }

    /// 0020 §1: the arrival policy as a pure function.
    #[test]
    fn plan_arrival_table() {
        let row = |name: &str, s: Span| Row { cells: vec![name.into()], field: Field::new(name, Some(s), "") };
        let view = View {
            title: "Data block @0x100".into(),
            rows: vec![row("key", Span::file(0x104, 4)), row("entry", Span::file(0x100, 0x10))],
            spaces: vec![SpaceInfo { space: Space::Block(0x200), size: 9, title: String::new(), map: SpaceMap::Compressed { file: Span::file(0x200, 0x20), codec: "ZSTD".into() } }],
            ..Default::default()
        };
        let p = |origin, anchor, hint| PendingSel::new(1, origin, anchor, hint);
        let ext = Some(Span::file(0x100, 0x40));
        // hint.at a field: the narrowest field.
        let at = Some(SelectHint { at: Some((Space::File, 0x105)), span: None });
        assert_eq!(plan_arrival(p(SelOrigin::Hex, Some(Span::file(0x105, 1)), None), at, &view, ext), Arrival::Field { row: 0, origin: SelOrigin::Hex, anchor: Some(Span::file(0x105, 1)) });
        // Exact-span row.
        let span = |s| Some(SelectHint { at: None, span: Some(s) });
        assert_eq!(plan_arrival(p(SelOrigin::Navigate, None, span(Span::file(0x100, 0x10))), None, &view, ext), Arrival::Field { row: 1, origin: SelOrigin::Navigate, anchor: None });
        // A span in a compressed container: the decompressed-view note.
        let a = plan_arrival(p(SelOrigin::Navigate, None, span(Span::file(0x200, 0x20))), None, &view, ext);
        assert_eq!(
            a,
            Arrival::Structure {
                span: Span::file(0x200, 0x20),
                note: "Compressed contents of Data block @0x100 file [0x200,0x220) · 32 B: stored bytes have no byte-to-byte mapping to decompressed fields. Open the decompressed view to select a field (block@0x200 (decompressed)).".into(),
                origin: SelOrigin::Navigate,
                anchor: None,
                reveal: true,
            }
        );
        // Other structure bytes, from a byte click: not revealed (the byte view stays).
        let a = plan_arrival(p(SelOrigin::Hex, None, span(Span::file(0x130, 5))), None, &view, ext);
        assert!(matches!(a, Arrival::Structure { reveal: false, ref note, .. } if note == "Data block @0x100 file [0x130,0x135) · 5 B: structure bytes, not a single field."), "{a:?}");
        // Anchor only: the structure (extent), clicked byte kept.
        let a = plan_arrival(p(SelOrigin::Hex, Some(Span::file(0x13f, 1)), None), None, &view, ext);
        assert!(matches!(a, Arrival::Structure { span, anchor: Some(_), reveal: false, .. } if Some(span) == ext), "{a:?}");
        // Nothing: the whole structure, with the navigation origin.
        let mut q = p(SelOrigin::Tree, None, None);
        q.via = Some("entry 2".into());
        assert_eq!(
            plan_arrival(q, None, &view, ext),
            Arrival::Whole { extent: ext, note: "The whole structure is selected (no single field). Select a row or a byte for a field.".into(), origin: SelOrigin::Tree, via: Some("entry 2".into()) }
        );
        // Back: restore wins over every hint.
        let mut r = p(SelOrigin::Row, None, span(Span::file(0x100, 0x10)));
        r.restore = Some(here(0x104));
        assert_eq!(plan_arrival(r, at, &view, ext), Arrival::Restore(here(0x104)));
    }
}
