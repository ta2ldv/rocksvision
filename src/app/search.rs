//! Search state: query, hex mode, decoded-row matches of the current table, the byte-search
//! cursor (last hit) and progress, and the status line. Mutators are visible to `app` only; the
//! byte search itself runs as a job (`JobState`).

use crate::model::View;

#[derive(Debug, Default)]
pub struct SearchState {
    query: String,
    hex: bool,
    status: String,
    progress: Option<(u64, u64)>,
    /// File offset of the last byte-search hit (start of the next/previous search).
    hit: Option<u64>,
    row_matches: Vec<usize>,
    match_idx: usize,
}

impl SearchState {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub fn query(&self) -> &str {
        &self.query
    }
    pub fn hex_mode(&self) -> bool {
        self.hex
    }
    pub fn status(&self) -> &str {
        &self.status
    }
    pub fn progress(&self) -> Option<(u64, u64)> {
        self.progress
    }
    pub fn hit(&self) -> Option<u64> {
        self.hit
    }
    pub fn row_matches(&self) -> &[usize] {
        &self.row_matches
    }
    pub fn match_index(&self) -> usize {
        self.match_idx
    }
    /// Position (1-based) of `row` among the row matches, if any.
    pub fn match_position(&self, row: Option<usize>) -> Option<usize> {
        row.and_then(|r| self.row_matches.iter().position(|&m| m == r)).map(|i| i + 1)
    }

    /// New query text: forgets the byte-search cursor and recomputes the row matches.
    pub(super) fn set_query(&mut self, q: String, view: Option<&View>) {
        self.query = q;
        self.hit = None;
        self.update_matches(view);
    }
    pub(super) fn set_hex(&mut self, b: bool) {
        self.hex = b;
    }
    pub(super) fn set_status(&mut self, s: impl Into<String>) {
        self.status = s.into();
    }
    pub(super) fn set_progress(&mut self, p: Option<(u64, u64)>) {
        self.progress = p;
    }
    pub(super) fn set_hit(&mut self, off: Option<u64>) {
        self.hit = off;
    }
    pub(super) fn clear_matches(&mut self) {
        self.row_matches.clear();
    }

    /// Rows of `view` whose decoded cells contain the query (hex queries search file bytes only).
    pub(super) fn update_matches(&mut self, view: Option<&View>) {
        self.row_matches.clear();
        self.match_idx = 0;
        let q = self.query.to_lowercase();
        if q.is_empty() || self.hex {
            return;
        }
        if let Some(v) = view {
            for (i, r) in v.rows.iter().enumerate() {
                if r.cells.iter().any(|c| c.to_lowercase().contains(&q)) || r.field.value.to_lowercase().contains(&q) {
                    self.row_matches.push(i);
                }
            }
        }
    }

    /// Next/previous row match relative to the selected row; None when no row matches.
    pub(super) fn step_rows(&mut self, sel_row: Option<usize>, backward: bool) -> Option<usize> {
        if self.row_matches.is_empty() {
            return None;
        }
        let n = self.row_matches.len();
        let cur = sel_row.and_then(|r| self.row_matches.iter().position(|&m| m == r));
        let next = match cur {
            Some(i) if backward => (i + n - 1) % n,
            Some(i) => (i + 1) % n,
            None => 0,
        };
        self.match_idx = next;
        self.status = format!("Rows: {} / {} in the current table (decoded)", next + 1, n);
        Some(self.row_matches[next])
    }

    /// Clear search: query, row matches, byte-search hit, progress and status.
    pub(super) fn clear(&mut self) {
        self.query.clear();
        self.status.clear();
        self.hit = None;
        self.row_matches.clear();
        self.match_idx = 0;
        self.progress = None;
    }
}
