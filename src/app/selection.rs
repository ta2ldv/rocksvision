//! Selection: the single owner of what is selected — a field (table row), a structure or a raw
//! byte range — with its span(s), the anchor byte (the clicked byte, outlined in the byte view)
//! and the origin of the selection. This is the only place the selection epoch changes; every
//! change of target or range bumps it, so a copy result or an async view requested for an
//! older epoch is recognisably stale. Reveal/scroll requests are single-use and never change
//! the selection. Mutators are visible to `app` only.

use crate::model::Span;

/// What is selected.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum SelTarget {
    #[default]
    None,
    /// A field: table row of the current view.
    Field(usize),
    /// A structure without a single field (header, trailer, padding, unaccounted or compressed
    /// bytes, a whole block); the note says what it is.
    Structure,
    /// A raw byte range (Shift+click, go to offset, diagnostic bytes).
    Raw,
}

/// Which input made the selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SelOrigin {
    #[default]
    None,
    Row,
    Hex,
    Keyboard,
    Navigate,
    Search,
    Diagnosis,
    /// A tree click or a structure step (Shift+] / Shift+[).
    Tree,
}

#[derive(Debug, Default)]
pub struct SelectionState {
    target: SelTarget,
    /// Primary span (in its own space).
    span: Option<Span>,
    /// Further ranges to fill: the exact file ranges of a derived field (two when it crosses a
    /// fragment boundary).
    extra: Vec<Span>,
    /// The clicked byte of a byte-view selection (outlined; Shift+click extends from it).
    anchor: Option<Span>,
    origin: SelOrigin,
    epoch: u64,
    /// Explanation of a raw / structure selection.
    raw_note: Option<String>,
    /// Where a navigation came from ("entry 2 of Index block @0x…"), for the structure card's
    /// "Referenced from" back-link.
    via: Option<String>,
    /// One-shot request for the records table to scroll this row into view, and whether to
    /// center it (navigation, Back, search, byte click) or scroll minimally (row keys, clicks).
    scroll_row: Option<(usize, bool)>,
    /// A field without bytes of its own: the first byte of the enclosing structure the byte
    /// view shows (design B2). Never a selected range — nothing is filled or outlined.
    shown: Option<Span>,
}

/// Everything a selection is (no epoch, no one-shot requests): saved on the history so Back
/// restores the exact previous selection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SelSnapshot {
    pub target: SelTarget,
    pub span: Option<Span>,
    pub extra: Vec<Span>,
    pub anchor: Option<Span>,
    pub origin: SelOrigin,
    pub raw_note: Option<String>,
    pub via: Option<String>,
    pub shown: Option<Span>,
}

impl SelectionState {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub fn target(&self) -> &SelTarget {
        &self.target
    }
    pub fn row(&self) -> Option<usize> {
        match self.target {
            SelTarget::Field(r) => Some(r),
            _ => None,
        }
    }
    pub fn span(&self) -> Option<Span> {
        self.span
    }
    /// Every range to fill: the primary span and its exact file ranges.
    pub fn spans(&self) -> impl Iterator<Item = Span> + '_ {
        self.span.into_iter().chain(self.extra.iter().copied())
    }
    pub fn anchor(&self) -> Option<Span> {
        self.anchor
    }
    pub fn origin(&self) -> SelOrigin {
        self.origin
    }
    /// Bumped on every selection change; copy results carry the epoch they were requested for.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn raw_note(&self) -> Option<&str> {
        self.raw_note.as_deref()
    }
    pub fn is_structure(&self) -> bool {
        self.target == SelTarget::Structure
    }
    /// Origin of a navigation (structure card "Referenced from").
    pub fn via(&self) -> Option<&str> {
        self.via.as_deref()
    }
    /// For a field without bytes: the enclosing structure's first byte the byte view shows.
    pub fn shown(&self) -> Option<Span> {
        self.shown
    }

    /// The selection as data (for the history).
    pub fn snapshot(&self) -> SelSnapshot {
        SelSnapshot { target: self.target.clone(), span: self.span, extra: self.extra.clone(), anchor: self.anchor, origin: self.origin, raw_note: self.raw_note.clone(), via: self.via.clone(), shown: self.shown }
    }

    /// Restores a saved selection (Back): a new epoch, and the table scrolls to its row.
    pub(super) fn restore(&mut self, s: SelSnapshot) {
        self.set(s.target, s.span, s.extra, s.origin, s.raw_note);
        self.anchor = s.anchor;
        self.via = s.via;
        self.shown = s.shown;
        self.scroll_row = Some((self.row().unwrap_or(0), true));
    }

    fn set(&mut self, target: SelTarget, span: Option<Span>, extra: Vec<Span>, origin: SelOrigin, note: Option<String>) {
        self.target = target;
        self.span = span;
        self.extra = extra;
        self.origin = origin;
        self.raw_note = note;
        self.via = None;
        self.shown = None;
        self.epoch += 1;
    }

    /// Records where the byte view shows a byte-less field (part of the same selection: no
    /// new epoch, no span).
    pub(super) fn set_shown(&mut self, at: Option<Span>) {
        if self.span.is_none() {
            self.shown = at;
        }
    }

    /// Selects table row `row` with its span and exact file ranges (and asks the table to
    /// scroll to it). The anchor is kept only for a byte-view click.
    pub(super) fn select_field(&mut self, row: usize, span: Option<Span>, extra: Vec<Span>, origin: SelOrigin, anchor: Option<Span>) {
        self.set(SelTarget::Field(row), span, extra, origin, None);
        self.anchor = anchor;
        // Row keys and clicks scroll minimally; a jump (navigation, search, byte, diagnosis)
        // centers the row (design F3).
        self.scroll_row = Some((row, !matches!(origin, SelOrigin::Row | SelOrigin::Keyboard)));
    }

    /// Replaces the byte range, keeping the row (byte-search hit).
    pub(super) fn set_span(&mut self, span: Option<Span>) {
        let target = self.target.clone();
        self.set(target, span, Vec::new(), SelOrigin::Search, None);
    }

    /// A structure range without a single field (header, trailer, padding, compressed bytes…).
    pub(super) fn select_structure(&mut self, span: Span, note: String, origin: SelOrigin, anchor: Option<Span>) {
        self.set(SelTarget::Structure, Some(span), Vec::new(), origin, Some(note));
        self.anchor = anchor;
    }

    /// The whole structure shown (tree click, structure step, navigation without a target
    /// row): its file range when it has one, no row; the table scrolls to the top.
    pub(super) fn select_whole(&mut self, span: Option<Span>, note: String, origin: SelOrigin, via: Option<String>) {
        self.set(SelTarget::Structure, span, Vec::new(), origin, Some(note));
        self.anchor = None;
        self.via = via;
        self.scroll_row = Some((0, false));
    }

    /// Raw byte range without a row (Shift+click, go to offset).
    pub(super) fn select_raw(&mut self, span: Span, note: &str) {
        self.set(SelTarget::Raw, Some(span), Vec::new(), SelOrigin::Hex, Some(note.into()));
    }

    /// One clicked byte that no field or structure explains (anchor = the byte).
    pub(super) fn click_raw(&mut self, byte: Span, note: &str) {
        self.select_raw(byte, note);
        self.anchor = Some(byte);
    }

    /// Raw range from the anchor to `off` (Shift+click); the anchor stays.
    pub(super) fn extend_raw(&mut self, span: Span) {
        let note = format!("Raw range [0x{:x},0x{:x}): not a single field", span.offset, span.offset + span.len);
        self.set(SelTarget::Raw, Some(span), Vec::new(), SelOrigin::Hex, Some(note));
    }

    /// No row and no range (new view, closed file).
    pub(super) fn clear(&mut self) {
        self.set(SelTarget::None, None, Vec::new(), SelOrigin::None, None);
        self.anchor = None;
    }

    /// Scroll the selected row into view again (Reset View).
    pub(super) fn rescroll(&mut self) {
        self.scroll_row = self.row().map(|r| (r, false));
    }

    pub(super) fn take_scroll_row(&mut self) -> Option<(usize, bool)> {
        self.scroll_row.take()
    }
}
