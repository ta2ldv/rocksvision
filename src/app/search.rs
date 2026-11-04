//! Search state: query, hex mode, decoded-row matches of the current table and the typed state
//! of the byte search of this file (`ByteSearch`, guarded by the request id of the search in
//! flight) with its cursor (last hit). Mutators are visible to `app` only; the byte search
//! itself runs as a job (`JobState`).
//!
//! 0019 §4: the banner above the table, the status-bar segment and Next/Prev are derived from
//! this one state (`banner`, `status_segment`, `can_step`), so they always agree. Row counts
//! have one format, "Rows k / n (current table)"; the byte-search state describes the file and
//! is forgotten when the file changes.
//!
//! 0020 §5: byte-search replies carry the search's request id (`Tagged::job`); progress, hit
//! and done of any other request (an older query, a search cancelled by Clear/Cancel/a new
//! query or a newer Next) are ignored.

use crate::model::{Span, View};

/// Status of a byte search that found nothing (from the start, or after an earlier hit).
pub const BYTES_NO_MATCH: &str = "Bytes: no match in this file's stored bytes (compressed contents are not searched)";
pub const BYTES_NO_FURTHER: &str = "Bytes: no further match in this file's stored bytes (compressed contents are not searched)";
pub const BYTES_SEARCHING: &str = "Bytes: searching this file's stored bytes…";
pub const BYTES_CANCELLED: &str = "Byte search cancelled";

/// State of the byte search of this file.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum ByteSearch {
    #[default]
    Idle,
    /// Search `req` in flight; `progress` = (bytes done, total).
    Running {
        req: u64,
        progress: (u64, u64),
    },
    /// The last search found the needle at file offset `off`.
    Hit {
        off: u64,
    },
    /// The last search ended without a match (`after_hit`: after an earlier hit).
    Exhausted {
        after_hit: bool,
    },
    Cancelled,
    /// The query could not be turned into bytes.
    Invalid(String),
}

/// One byte-search request: needle, start offset and direction.
#[derive(Clone, Debug, PartialEq)]
pub struct ByteQuery {
    pub needle: Vec<u8>,
    pub from: u64,
    pub backward: bool,
}

#[derive(Debug, Default)]
pub struct SearchState {
    query: String,
    hex: bool,
    bytes: ByteSearch,
    /// File offset of the last byte-search hit (start of the next/previous search); kept while
    /// later searches run, end or are cancelled.
    cursor: Option<u64>,
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
    pub fn byte_search(&self) -> &ByteSearch {
        &self.bytes
    }
    /// The byte-search status text ("" when idle).
    pub fn status(&self) -> String {
        match &self.bytes {
            ByteSearch::Idle => String::new(),
            ByteSearch::Running { .. } => BYTES_SEARCHING.into(),
            ByteSearch::Hit { off } => format!("Bytes: match at file 0x{off:x} (stored bytes)"),
            ByteSearch::Exhausted { after_hit } => if *after_hit { BYTES_NO_FURTHER } else { BYTES_NO_MATCH }.into(),
            ByteSearch::Cancelled => BYTES_CANCELLED.into(),
            ByteSearch::Invalid(e) => e.clone(),
        }
    }
    pub fn progress(&self) -> Option<(u64, u64)> {
        match self.bytes {
            ByteSearch::Running { progress, .. } => Some(progress),
            _ => None,
        }
    }
    /// File offset of the last byte-search hit.
    pub fn hit(&self) -> Option<u64> {
        self.cursor
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

    /// The query as file bytes: Ok for text and valid hex, Err for an invalid hex query.
    fn needle(&self) -> Result<Vec<u8>, String> {
        super::jobs::parse_query(&self.query, self.hex)
    }

    /// Next/Prev do something: a non-empty query that is not an invalid hex sequence (with no
    /// row match they start the byte search of this file).
    pub fn can_step(&self) -> bool {
        !self.query.is_empty() && (!self.row_matches.is_empty() || self.needle().is_ok())
    }

    /// The search line above the table, or None.
    pub fn banner(&self) -> Option<String> {
        if self.query.is_empty() {
            return None;
        }
        let q = &self.query;
        if self.row_matches.is_empty() && self.needle().is_err() {
            return Some("Not a hex byte sequence. Use byte pairs such as 4d 41 4e.".into());
        }
        if self.hex {
            // 0020 UI ruling F2: the banner stays while a valid hex query is active.
            let q = q.trim();
            return Some(match self.bytes {
                ByteSearch::Hit { off } => format!("Hex query {q}: match at file 0x{off:x} (stored bytes). Next/Prev to step."),
                ByteSearch::Exhausted { after_hit: false } => format!("Hex query {q}: no match in this file's stored bytes (compressed contents are not searched)."),
                ByteSearch::Exhausted { after_hit: true } => format!("Hex query {q}: no further match in this file's stored bytes (compressed contents are not searched)."),
                _ => format!("Hex query {q}: Next/Prev search this file's stored bytes (not decompressed)."),
            });
        }
        let running = matches!(self.bytes, ByteSearch::Running { .. });
        match self.row_matches.len() {
            0 if self.bytes == (ByteSearch::Exhausted { after_hit: false }) => Some(format!("No row of this table matches \"{q}\"; no match in this file's stored bytes either.")),
            0 if self.cursor.is_none() && !running => Some(format!("No row of this table matches \"{q}\". Next/Prev search this file's stored bytes (not decompressed).")),
            0 => None,
            1 => Some(format!("1 row of this table matches \"{q}\" (decoded cells). Next/Prev to step.")),
            n => Some(format!("{n} rows of this table match \"{q}\" (decoded cells). Next/Prev to step.")),
        }
    }

    /// The status-bar search segment, or None. `cur`: position of the selected row among the
    /// row matches.
    pub fn status_segment(&self, cur: Option<usize>) -> Option<String> {
        let status = self.status();
        if self.query.is_empty() {
            return (!status.is_empty()).then_some(status);
        }
        if !self.hex && !self.row_matches.is_empty() {
            let k = cur.map(|i| i.to_string()).unwrap_or("–".into());
            return Some(format!("Rows {k} / {} (current table)", self.row_matches.len()));
        }
        if !status.is_empty() {
            return Some(status);
        }
        (!self.hex && self.needle().is_ok()).then(|| "Rows 0 / 0 (current table)".to_string())
    }

    /// New query text: forgets the byte-search cursor and result and recomputes the row matches.
    pub(super) fn set_query(&mut self, q: String, view: Option<&View>) {
        self.query = q;
        self.forget_bytes();
        self.update_matches(view);
    }
    pub(super) fn set_hex(&mut self, b: bool, view: Option<&View>) {
        self.hex = b;
        self.forget_bytes();
        self.update_matches(view);
    }

    /// The next byte search (Next/Prev without a row match): the needle and where it starts —
    /// after the last hit, else at the selected file offset (forward) or the end (backward).
    pub(super) fn next_byte_search(&self, sel_file_off: Option<u64>, backward: bool) -> Result<ByteQuery, String> {
        let needle = self.needle()?;
        let from = match (self.cursor, backward) {
            (Some(h), false) => h + 1,
            (Some(h), true) => h,
            (None, false) => sel_file_off.unwrap_or(0),
            (None, true) => u64::MAX,
        };
        Ok(ByteQuery { needle, from, backward })
    }
    /// Byte search `req` was issued.
    pub(super) fn started(&mut self, req: u64) {
        self.bytes = ByteSearch::Running { req, progress: (0, 1) };
    }
    /// The query could not be searched (message shown as the status).
    pub(super) fn set_invalid(&mut self, e: String) {
        self.bytes = ByteSearch::Invalid(e);
    }
    fn is_running(&self, req: u64) -> bool {
        matches!(self.bytes, ByteSearch::Running { req: r, .. } if r == req)
    }
    pub(super) fn on_progress(&mut self, req: u64, done: u64, total: u64) {
        if self.is_running(req) {
            self.bytes = ByteSearch::Running { req, progress: (done, total) };
        }
    }
    /// A hit of the search in flight: the span to select; None for any other request.
    pub(super) fn on_hit(&mut self, req: u64, off: u64, len: u64) -> Option<Span> {
        if !self.is_running(req) {
            return None;
        }
        self.cursor = Some(off);
        self.bytes = ByteSearch::Hit { off };
        Some(Span::file(off, len))
    }
    /// The search in flight ended without a hit (or was cancelled); other requests are ignored.
    pub(super) fn on_done(&mut self, req: u64, cancelled: bool) {
        if self.is_running(req) {
            self.bytes = if cancelled { ByteSearch::Cancelled } else { ByteSearch::Exhausted { after_hit: self.cursor.is_some() } };
        }
    }
    /// The running search was cancelled (Cancel, Esc, a new query, Clear): it can no longer
    /// change this state.
    pub(super) fn cancel(&mut self) {
        if matches!(self.bytes, ByteSearch::Running { .. }) {
            self.bytes = ByteSearch::Cancelled;
        }
    }
    pub(super) fn clear_matches(&mut self) {
        self.row_matches.clear();
        self.match_idx = 0;
    }
    /// The file changed: byte-search cursor and state described the old file and go; the query
    /// stays.
    pub(super) fn forget_bytes(&mut self) {
        self.cursor = None;
        self.bytes = ByteSearch::Idle;
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

    /// Next/previous row match relative to the selected row; None when no row matches. The
    /// status bar derives "Rows k / n" from the selection (`status_segment`).
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
        Some(self.row_matches[next])
    }

    /// Clear search: query, row matches, byte-search cursor and state.
    pub(super) fn clear(&mut self) {
        self.query.clear();
        self.forget_bytes();
        self.clear_matches();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Field, Row};

    fn row(t: &str) -> Row {
        Row { cells: vec![t.into()], field: Field::new(t, None, t) }
    }

    /// 0019 §4: a text query that looks like hex ("0x…") but is not valid hex still steps
    /// through its row matches; without one it is reported as not a hex sequence and Next/Prev
    /// do nothing. A row step never writes a second row-count format into the status.
    #[test]
    fn hex_looking_text_query_and_one_row_count_format() {
        let v = View { rows: vec![row("0xzz marker"), row("other")], ..Default::default() };
        let mut s = SearchState::new();
        s.set_query("0xzz".into(), Some(&v));
        assert_eq!((s.row_matches(), s.can_step()), (&[0usize][..], true));
        assert_eq!(s.banner().as_deref(), Some("1 row of this table matches \"0xzz\" (decoded cells). Next/Prev to step."));
        assert_eq!(s.step_rows(None, false), Some(0));
        assert_eq!((s.status(), s.status_segment(Some(1)).as_deref()), (String::new(), Some("Rows 1 / 1 (current table)")));
        s.update_matches(Some(&View::default()));
        assert_eq!((s.can_step(), s.banner().as_deref(), s.status_segment(None)), (false, Some("Not a hex byte sequence. Use byte pairs such as 4d 41 4e."), None));
    }

    /// 0020 §5 + UI ruling F2: every `ByteSearch` state × {text, hex} × {0, 1, n row matches} →
    /// (banner, status segment, can_step). Text and hex queries both have valid needles here.
    #[test]
    fn byte_search_banner_status_can_step_table() {
        let states = [
            (None, ByteSearch::Idle),
            (None, ByteSearch::Running { req: 1, progress: (0, 1) }),
            (Some(0xc12), ByteSearch::Running { req: 2, progress: (5, 9) }),
            (Some(0xc12), ByteSearch::Hit { off: 0xc12 }),
            (None, ByteSearch::Exhausted { after_hit: false }),
            (Some(0xc12), ByteSearch::Exhausted { after_hit: true }),
            (None, ByteSearch::Cancelled),
            (Some(0xc12), ByteSearch::Cancelled),
        ];
        for rows in [0usize, 1, 3] {
            let v = View { rows: (0..4).map(|i| row(if i < rows { "has MAN" } else { "x" })).collect(), ..Default::default() };
            for hex in [false, true] {
                for (cursor, st) in &states {
                    let mut s = SearchState::new();
                    s.set_query(if hex { "4d 41 4e".into() } else { "MAN".into() }, Some(&v));
                    s.set_hex(hex, Some(&v));
                    (s.cursor, s.bytes) = (*cursor, st.clone());
                    let what = format!("rows {rows} hex {hex} {cursor:?} {st:?}");
                    let banner = s.banner();
                    let seg = s.status_segment(None);
                    assert!(s.can_step(), "{what}: a valid needle always steps");
                    let status = s.status();
                    let want_status = match st {
                        ByteSearch::Idle => "",
                        ByteSearch::Running { .. } => BYTES_SEARCHING,
                        ByteSearch::Hit { .. } => "Bytes: match at file 0xc12 (stored bytes)",
                        ByteSearch::Exhausted { after_hit: false } => BYTES_NO_MATCH,
                        ByteSearch::Exhausted { after_hit: true } => BYTES_NO_FURTHER,
                        ByteSearch::Cancelled => BYTES_CANCELLED,
                        ByteSearch::Invalid(_) => unreachable!(),
                    };
                    assert_eq!(status, want_status, "{what}");
                    if hex {
                        let want = match st {
                            ByteSearch::Hit { .. } => "Hex query 4d 41 4e: match at file 0xc12 (stored bytes). Next/Prev to step.",
                            ByteSearch::Exhausted { after_hit: false } => "Hex query 4d 41 4e: no match in this file's stored bytes (compressed contents are not searched).",
                            ByteSearch::Exhausted { after_hit: true } => "Hex query 4d 41 4e: no further match in this file's stored bytes (compressed contents are not searched).",
                            _ => "Hex query 4d 41 4e: Next/Prev search this file's stored bytes (not decompressed).",
                        };
                        assert_eq!(banner.as_deref(), Some(want), "{what}: the hex banner stays");
                        assert_eq!(seg.as_deref(), (!want_status.is_empty()).then_some(want_status), "{what}");
                        continue;
                    }
                    match rows {
                        0 => {
                            let running = matches!(st, ByteSearch::Running { .. });
                            let want = match (cursor, st) {
                                (_, ByteSearch::Exhausted { after_hit: false }) => Some("No row of this table matches \"MAN\"; no match in this file's stored bytes either.".to_string()),
                                (None, _) if !running => Some("No row of this table matches \"MAN\". Next/Prev search this file's stored bytes (not decompressed).".to_string()),
                                _ => None,
                            };
                            assert_eq!(banner, want, "{what}");
                            let want_seg = if want_status.is_empty() { "Rows 0 / 0 (current table)" } else { want_status };
                            assert_eq!(seg.as_deref(), Some(want_seg), "{what}");
                        }
                        1 => {
                            assert_eq!(banner.as_deref(), Some("1 row of this table matches \"MAN\" (decoded cells). Next/Prev to step."), "{what}");
                            assert_eq!(seg.as_deref(), Some("Rows – / 1 (current table)"), "{what}");
                        }
                        _ => {
                            assert_eq!(banner.as_deref(), Some("3 rows of this table match \"MAN\" (decoded cells). Next/Prev to step."), "{what}");
                            assert_eq!(seg.as_deref(), Some("Rows – / 3 (current table)"), "{what}");
                        }
                    }
                }
            }
        }
        // Invalid hex: banner, no segment, Next/Prev disabled; an invalid state shows its text.
        let mut s = SearchState::new();
        s.set_hex(true, None);
        s.set_query("zz".into(), None);
        assert_eq!((s.banner().as_deref(), s.status_segment(None), s.can_step()), (Some("Not a hex byte sequence. Use byte pairs such as 4d 41 4e."), None, false));
        s.set_invalid("hex query needs an even number of hex digits".into());
        assert_eq!(s.status_segment(None).as_deref(), Some("hex query needs an even number of hex digits"));
    }

    /// 0020 §5: hit, done and progress of an older request are ignored after a new query, a
    /// Clear, a cancel or a newer Next; the request in flight is applied.
    #[test]
    fn stale_byte_search_replies_are_ignored() {
        let mut s = SearchState::new();
        s.set_query("MAN".into(), None);
        // Old search 1 overtaken by a new query (the app cancels, then sets the query).
        s.started(1);
        s.cancel();
        s.set_query("OTHER".into(), None);
        assert_eq!(s.on_hit(1, 0x10, 3), None);
        s.on_progress(1, 5, 9);
        s.on_done(1, true);
        assert_eq!((s.byte_search(), s.hit()), (&ByteSearch::Idle, None), "after SetSearch");
        // Old search 2 overtaken by Clear.
        s.started(2);
        s.cancel();
        s.clear();
        assert_eq!(s.on_hit(2, 0x20, 3), None);
        s.on_done(2, false);
        assert_eq!((s.byte_search(), s.hit()), (&ByteSearch::Idle, None), "after ClearSearch");
        // Search 3 overtaken by a newer Next (4): 3's late replies cannot touch 4.
        s.set_query("MAN".into(), None);
        s.started(3);
        s.started(4);
        s.on_progress(3, 7, 9);
        assert_eq!(s.progress(), Some((0, 1)));
        assert_eq!(s.on_hit(3, 0x30, 3), None);
        s.on_done(3, true);
        assert_eq!(s.byte_search(), &ByteSearch::Running { req: 4, progress: (0, 1) }, "a cancelled older Done does not overwrite the newer search");
        s.on_progress(4, 2, 9);
        assert_eq!(s.progress(), Some((2, 9)));
        assert_eq!(s.on_hit(4, 0x40, 3), Some(Span::file(0x40, 3)));
        assert_eq!((s.byte_search(), s.hit()), (&ByteSearch::Hit { off: 0x40 }, Some(0x40)));
        // The cursor drives the next search; a late Done of 4 changes nothing now.
        s.on_done(4, false);
        assert_eq!(s.byte_search(), &ByteSearch::Hit { off: 0x40 });
        assert_eq!(s.next_byte_search(None, false), Ok(ByteQuery { needle: b"MAN".to_vec(), from: 0x41, backward: false }));
        assert_eq!(s.next_byte_search(None, true).map(|q| q.from), Ok(0x40));
        // Cancel while running: Cancelled; the cursor stays.
        s.started(5);
        s.cancel();
        s.on_done(5, true);
        assert_eq!((s.byte_search(), s.hit(), s.status()), (&ByteSearch::Cancelled, Some(0x40), BYTES_CANCELLED.to_string()));
        let fresh = SearchState { query: "MAN".into(), ..Default::default() };
        assert_eq!(fresh.next_byte_search(Some(0x99), false).map(|q| q.from), Ok(0x99));
        assert_eq!(fresh.next_byte_search(Some(0x99), true).map(|q| q.from), Ok(u64::MAX));
    }
}
