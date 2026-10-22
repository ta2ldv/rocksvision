//! Selection identity: the selected table row, the selected byte range and the selection epoch.
//! This is the only place the epoch changes; every change of row or range bumps it, so a copy
//! result requested for an older epoch is recognisably stale. Mutators are visible to `app` only.

use crate::model::Span;

#[derive(Debug, Default)]
pub struct SelectionState {
    row: Option<usize>,
    span: Option<Span>,
    epoch: u64,
    /// Explanation of a raw (non-row) selection.
    raw_note: Option<String>,
    /// One-shot request for the records table to scroll this row into view.
    scroll_row: Option<usize>,
}

impl SelectionState {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub fn row(&self) -> Option<usize> {
        self.row
    }
    pub fn span(&self) -> Option<Span> {
        self.span
    }
    /// Bumped on every selection change; copy results carry the epoch they were requested for.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn raw_note(&self) -> Option<&str> {
        self.raw_note.as_deref()
    }

    /// Selects table row `row` with its byte span (and asks the table to scroll to it).
    pub(super) fn select_row(&mut self, row: usize, span: Option<Span>) {
        self.row = Some(row);
        self.scroll_row = Some(row);
        self.set_span(span);
    }

    /// Replaces the byte range, keeping the row (byte-search hit).
    pub(super) fn set_span(&mut self, span: Option<Span>) {
        self.span = span;
        self.epoch += 1;
        self.raw_note = None;
    }

    /// Raw byte range without a row (hex click/shift-click, go to offset).
    pub(super) fn select_raw(&mut self, span: Span, note: &str) {
        self.row = None;
        self.span = Some(span);
        self.epoch += 1;
        self.raw_note = Some(note.into());
    }

    /// No row and no range (new view, closed file).
    pub(super) fn clear(&mut self) {
        self.row = None;
        self.span = None;
        self.epoch += 1;
    }

    /// Scroll the selected row into view again (Reset View).
    pub(super) fn rescroll(&mut self) {
        self.scroll_row = self.row;
    }

    pub(super) fn take_scroll_row(&mut self) -> Option<usize> {
        self.scroll_row.take()
    }
}
