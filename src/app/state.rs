//! Application state (egui-independent): a coordinator of five focused components with private
//! fields — `DocumentSession` (folder/file, generation, tree, view, history, hex page),
//! `SelectionState` (row/range + selection epoch), `SearchState`, `JobState` (worker, request
//! ids, copy gate) and `ViewPreferences` (theme, zoom, layout toggles). Panels read them through
//! `&` getters; the only mutators are `apply(Action)`, `poll()` (job replies) and `tick()`
//! (plus the documented test shims `select_raw`/`do_copy`).

use super::actions::{Action, CopyKind, Effect, Panel, RowMove};
use super::hex::PageState;
use super::job_state::{CopyReply, JobState};
use super::jobs::{self, CopyJob, Outcome, Request, Response, Tagged, Worker};
use super::search::SearchState;
use super::selection::SelectionState;
use super::session::DocumentSession;
use super::view_prefs::ViewPreferences;
use crate::limits::LIMITS;
use crate::model::*;
use std::path::PathBuf;

pub use super::job_state::{copy_accepted, PendingCopy};
pub use super::session::{TreeNode, FILE_SPACE_TITLE};
pub use super::view_prefs::{ZoomFit, ZoomPolicy, CENTER_SPLIT_DEFAULT};

#[cfg(test)]
#[path = "state_tests.rs"]
mod state_tests;

/// One-shot scroll requests for the UI (taken once per frame).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScrollRequest {
    /// Records table row to bring into view.
    pub row: Option<usize>,
    /// Byte offset to bring into view in the hex rows.
    pub hex: Option<u64>,
}

/// Clipboard text of exact decoded bytes: valid UTF-8 → the text itself (real newlines);
/// otherwise a lossless escaped form (a backslash becomes two, bytes that are not UTF-8
/// become `\xNN`). Bounded by the copy cap (the note says so).
pub fn bytes_copy_text(b: &ValueBytes, what: &str) -> (String, String) {
    let cap = if b.total > b.data.len() as u64 { format!("; first {} of {} bytes, {} KiB copy cap", b.data.len(), b.total, LIMITS.copy_cap / 1024) } else { String::new() };
    if let Ok(s) = std::str::from_utf8(&b.data) {
        return (s.to_string(), format!("Copied {what} ({} bytes, UTF-8 text{cap})", b.data.len()));
    }
    let mut out = String::with_capacity(b.data.len() * 2);
    for chunk in b.data.utf8_chunks() {
        out.push_str(&chunk.valid().replace('\\', "\\\\"));
        for x in chunk.invalid() {
            out.push_str(&format!("\\x{x:02x}"));
        }
    }
    (out, format!("Copied {what} (escaped, {} bytes{cap})", b.data.len()))
}

pub struct AppState {
    doc: DocumentSession,
    sel: SelectionState,
    find: SearchState,
    jobs: JobState,
    view_prefs: ViewPreferences,
}

impl AppState {
    pub fn new(worker: Worker) -> Self {
        AppState { doc: DocumentSession::new(), sel: SelectionState::new(), find: SearchState::new(), jobs: JobState::new(worker), view_prefs: ViewPreferences::new() }
    }

    /// Test helper: a state with an idle worker, for UI tests (panels never name `app::jobs`).
    #[cfg(test)]
    pub fn idle_for_tests() -> Self {
        AppState::new(Worker::spawn(std::sync::Arc::new(|| {})))
    }

    // ---------------- read-only component views (for panels) ----------------

    pub fn session(&self) -> &DocumentSession {
        &self.doc
    }
    pub fn selection(&self) -> &SelectionState {
        &self.sel
    }
    pub fn search(&self) -> &SearchState {
        &self.find
    }
    pub fn jobs(&self) -> &JobState {
        &self.jobs
    }
    pub fn prefs(&self) -> &ViewPreferences {
        &self.view_prefs
    }

    pub fn selected_field(&self) -> Option<&Field> {
        self.doc.row_field(self.sel.row())
    }
    /// Position (1-based) of the selected row among the row matches, if any.
    pub fn row_match_position(&self) -> Option<usize> {
        self.find.match_position(self.sel.row())
    }
    pub fn can_zoom_in(&self) -> bool {
        self.view_prefs.can_zoom_in()
    }
    pub fn space_title(&self, space: Space) -> (String, u64) {
        self.doc.space_title(space)
    }

    // ---------------- action dispatch ----------------

    /// Applies one action. Returns side effects for the platform layer (native dialogs).
    pub fn apply(&mut self, a: Action) -> Vec<Effect> {
        match a {
            Action::PickFolder => return vec![Effect::PickFolder(self.doc.dir().map(|d| d.to_path_buf()))],
            Action::PickFile => return vec![Effect::PickFile(self.doc.dir().map(|d| d.to_path_buf()))],
            Action::OpenPath(p) => self.open_path(p),
            Action::OpenFile(p) => self.open_file(p),
            Action::SetPathInput(s) => self.doc.set_path_input(s),
            Action::GoPathInput => {
                let p = PathBuf::from(self.doc.path_input().trim());
                self.open_path(p);
            }
            Action::SetSearch(s) => self.find.set_query(s, self.doc.view()),
            Action::SetSearchHex(b) => self.find.set_hex(b),
            Action::SearchNext => self.search_step(false),
            Action::SearchPrev => self.search_step(true),
            Action::ClearSearch => {
                self.find.clear();
                self.jobs.cancel_search();
            }
            Action::CancelSearch => {
                self.jobs.cancel_search();
                self.find.set_progress(None);
                self.find.set_status("Byte search cancelled");
            }
            Action::Copy(k) => self.copy(k),
            Action::CopyRow(i, k) => {
                self.select_row(i);
                self.copy(k);
            }
            Action::CopyFilePath => {
                if let Some(p) = self.doc.file() {
                    let t = p.display().to_string();
                    self.copy_text(t, "Copied file path");
                }
            }
            Action::ZoomIn => self.view_prefs.zoom_in(),
            Action::ZoomOut => self.view_prefs.zoom_out(),
            Action::ZoomReset => self.view_prefs.zoom_reset(),
            Action::SetZoom(z) => self.view_prefs.set_zoom(z),
            Action::LayoutFit(fit) => self.view_prefs.fit(fit),
            Action::SetCenterSplit(r) => self.view_prefs.set_center_split(r),
            Action::ResetCenterSplit => self.view_prefs.reset_center_split(),
            Action::ShowShortcuts => self.view_prefs.set_show_shortcuts(true),
            Action::CloseShortcuts => self.view_prefs.set_show_shortcuts(false),
            Action::FocusSearch => return vec![Effect::FocusSearch],
            Action::FocusGoto => {
                if self.doc.file().is_some() {
                    return vec![Effect::FocusGoto];
                }
            }
            Action::Quit => return vec![Effect::Quit],
            Action::SetShowLeft(b) => self.view_prefs.set_shown(Panel::Navigation, b),
            Action::SetShowRight(b) => self.view_prefs.set_shown(Panel::Explanation, b),
            Action::SetShowHex(b) => self.view_prefs.set_shown(Panel::Bytes, b),
            Action::TogglePanel(p) => {
                let b = !self.view_prefs.shown(p);
                self.view_prefs.set_shown(p, b);
            }
            Action::ResetView => self.reset_view(),
            Action::SetTheme(id) => self.view_prefs.set_theme(id),
            Action::ShowAbout => self.view_prefs.set_show_about(true),
            Action::CloseAbout => self.view_prefs.set_show_about(false),
            Action::ShowHelp => self.view_prefs.set_show_help(true),
            Action::CloseHelp => self.view_prefs.set_show_help(false),
            Action::SelectRoot => {
                if let Some(k) = self.doc.select_node(None) {
                    self.request_view(k, true);
                }
            }
            Action::SelectNode(i) => {
                if let Some(k) = self.doc.select_node(Some(i)) {
                    self.request_view(k, true);
                }
            }
            Action::ToggleNode(i) => self.toggle_node(i),
            Action::ExpandSelectedNode => {
                if let Some(i) = self.doc.expandable_selected() {
                    self.toggle_node(i);
                }
            }
            Action::CollapseSelectedNode => self.doc.collapse_selected(),
            Action::Back => {
                if let Some(k) = self.doc.pop_history() {
                    self.request_view(k, false);
                }
            }
            Action::SelectRow(i) => self.select_row(i),
            Action::FollowRow(i) => {
                if let Some(f) = self.doc.row(Some(i)).map(|r| r.field.clone()) {
                    self.follow(&f);
                }
            }
            Action::FollowSelected => {
                if let Some(f) = self.selected_field().cloned() {
                    self.follow(&f);
                }
            }
            Action::MoveRow(m) => self.move_row(m),
            Action::HexPage(base) => self.load_hex_page(self.doc.hex().space, base),
            Action::HexShowSpace(space) => {
                let (t, size) = self.doc.space_title(space);
                self.doc.hex_mut().set_space(space, size, t);
                self.load_hex_page(space, 0);
            }
            Action::SetGoto(s) => self.doc.set_goto(s),
            Action::GotoOffset => self.goto_offset(),
            Action::HexClick(off) => {
                let space = self.doc.hex().space;
                if !self.select_row_for(space, off) {
                    self.sel.select_raw(Span::new(space, off, 1), "No field in the current table covers this offset.");
                }
            }
            Action::HexShiftClick(off) => {
                let space = self.doc.hex().space;
                let s = match self.sel.span().filter(|s| s.space == space) {
                    Some(s) if off >= s.offset => Span::new(space, s.offset, off - s.offset + 1),
                    Some(s) => Span::new(space, off, s.offset + s.len - off),
                    None => Span::new(space, off, 1),
                };
                self.sel.select_raw(s, "Raw byte range (Shift+click).");
            }
            Action::SelectSpan(span) => self.select_span(span),
            Action::HexRetry => self.load_hex_page(self.doc.hex().space, self.doc.hex().page_base),
            Action::CancelJobs => self.cancel_jobs(),
        }
        Vec::new()
    }

    /// Per-frame housekeeping: periodic change-on-disk check of the open file; no byte page
    /// state survives without a file.
    pub fn tick(&mut self) {
        self.view_prefs.expire_notice(std::time::Instant::now());
        if let Some(p) = self.doc.file().map(|p| p.to_path_buf()) {
            if self.doc.stat_due(self.jobs.stat_pending()) {
                self.doc.mark_stat_started();
                self.jobs.stat(self.doc.generation(), p);
            }
        } else if self.doc.hex().load != PageState::Idle {
            self.doc.hex_mut().load = PageState::Idle;
        }
    }

    /// Takes the one-shot scroll requests. The hex request survives until a non-empty page of
    /// an open file is shown; the row request until a view is shown.
    pub fn take_scroll_request(&mut self) -> ScrollRequest {
        let hex_ready = self.doc.file().is_some() && self.doc.hex().page().map(|p| !p.is_empty()).unwrap_or(false);
        let row = if self.doc.view().is_some() { self.sel.take_scroll_row() } else { None };
        let hex = if hex_ready && self.view_prefs.show_hex() { self.doc.hex_mut().scroll_to.take() } else { None };
        ScrollRequest { row, hex }
    }

    /// Background work that warrants a short repaint interval.
    pub fn needs_fast_repaint(&self) -> bool {
        self.jobs.decoding() || self.jobs.io_busy() || self.find.progress().is_some()
    }

    /// A file is open and its change-on-disk check is waiting for the next interval.
    pub fn watching_file(&self) -> bool {
        self.doc.watching(self.jobs.stat_pending())
    }

    pub fn busy_label(&self) -> Option<&'static str> {
        if self.jobs.resolving() {
            Some("Opening…")
        } else if self.jobs.scanning() {
            Some("Scanning folder…")
        } else if self.jobs.decoding() || self.doc.pending_view().is_some() {
            Some("Decoding…")
        } else if self.jobs.pending_copy().is_some() {
            Some("Copying…")
        } else if self.doc.hex().is_loading() {
            Some("Reading bytes…")
        } else {
            None
        }
    }

    // ---------------- opening ----------------

    /// Opens a folder or file. The stat runs in a job; nothing here touches the file system.
    fn open_path(&mut self, p: PathBuf) {
        self.doc.set_path(&p);
        self.jobs.resolve(self.doc.generation(), p);
    }

    fn start_scan(&mut self, dir: PathBuf, auto_open: bool) {
        self.doc.begin_scan(dir.clone());
        self.jobs.scan(self.doc.generation(), dir, auto_open);
    }

    fn close_file(&mut self) {
        self.doc.close();
        self.jobs.cancel_file_jobs();
        self.jobs.cancel_search();
        self.sel.clear();
        self.find.set_hit(None);
        self.find.clear_matches();
        self.find.set_progress(None);
    }

    fn open_file(&mut self, p: PathBuf) {
        self.close_file();
        self.doc.begin_open(p.clone());
        self.jobs.decode(self.doc.generation(), Request::Open(p));
        self.request_view(NodeKind::Root, false);
        self.load_hex_page(Space::File, 0);
    }

    fn request_view(&mut self, k: NodeKind, push_history: bool) {
        self.doc.request_view(k.clone(), push_history);
        self.jobs.decode(self.doc.generation(), Request::View(k));
    }

    fn toggle_node(&mut self, i: usize) {
        if let Some(kind) = self.doc.toggle(i) {
            self.jobs.decode(self.doc.generation(), Request::Children { node: i as u64, kind });
        }
    }

    /// Requests a hex page; the hex panel shows Loading for exactly this request.
    fn load_hex_page(&mut self, space: Space, base: u64) {
        let g = self.doc.generation();
        let req = match space {
            Space::File => match self.doc.file().map(|p| p.to_path_buf()) {
                Some(p) => self.jobs.page(g, p, base, LIMITS.hex_page_bytes),
                None => {
                    self.doc.hex_mut().load = PageState::Idle;
                    return;
                }
            },
            _ => self.jobs.decode(g, Request::Bytes { space, off: base, len: LIMITS.hex_page_bytes }),
        };
        self.doc.hex_mut().begin(req, base);
    }

    /// Moves the hex view to the selected span (switching space / page as needed).
    fn show_span(&mut self, span: Option<Span>) {
        if let Some(s) = span {
            if s.space != self.doc.hex().space {
                let (t, size) = self.doc.space_title(s.space);
                self.doc.hex_mut().set_space(s.space, size, t);
            }
            if self.doc.hex_mut().ensure_visible(s.offset) {
                let base = self.doc.hex().page_base;
                self.load_hex_page(s.space, base);
            }
        }
    }

    /// Cancel button / Esc: cancels every job in flight and leaves no spinner behind.
    fn cancel_jobs(&mut self) {
        let (copy, scan, resolve) = self.jobs.cancel_all();
        self.find.set_progress(None);
        self.doc.hex_mut().cancel();
        if copy {
            self.jobs.set_copy_note("Copy cancelled");
        }
        if scan {
            self.doc.set_dir_note("Folder scan cancelled");
        }
        if resolve {
            self.doc.set_status_msg("Open cancelled");
        }
    }

    // ---------------- worker messages ----------------

    /// Applies worker responses. Returns the texts to put on the clipboard (only from copy
    /// results that are still current).
    pub fn poll(&mut self) -> Vec<String> {
        let mut clip = Vec::new();
        while let Some(t) = self.jobs.next_response() {
            clip.extend(self.apply_response(t));
        }
        clip
    }

    /// Applies one job response; returns clipboard text of a copy that is still current.
    fn apply_response(&mut self, t: Tagged<Response>) -> Option<String> {
        let mut clip = None;
        let current = t.generation == self.doc.generation();
        match t.msg {
            // Path resolve and folder scans are matched by request id (they span generations).
            Response::Resolved { req, path, outcome } => {
                if !self.jobs.take_resolve(req) {
                    return None;
                }
                match outcome {
                    Outcome::Done(true) => {
                        self.close_file();
                        self.start_scan(path, true);
                    }
                    Outcome::Done(false) => {
                        let parent = path.parent().map(|d| d.to_path_buf());
                        self.open_file(path);
                        if let Some(d) = parent.filter(|d| !self.doc.is_dir(d)) {
                            self.start_scan(d, false);
                        }
                    }
                    Outcome::Failed(e) => self.doc.set_path_error(format!("cannot open {}: {e}", path.display())),
                    Outcome::Cancelled => self.doc.set_status_msg("Open cancelled"),
                }
            }
            Response::Scanned { req, dir, outcome } => {
                let Some(auto_open) = self.jobs.scan_matches(req) else { return None };
                if !self.doc.is_dir(&dir) {
                    return None;
                }
                self.jobs.finish_scan();
                match outcome {
                    Outcome::Done((entries, trunc)) => {
                        if let Some(f) = self.doc.scanned(entries, trunc, auto_open) {
                            self.open_file(f);
                        }
                    }
                    Outcome::Failed(e) => self.doc.scan_failed(e),
                    Outcome::Cancelled => self.doc.set_dir_note("Folder scan cancelled"),
                }
            }
            _ if !current => return None, // stale result from a previous file/folder
            Response::Opened { label, children, file_len, mtime } => self.doc.opened(label, children, file_len, mtime),
            Response::OpenFailed(e) => self.doc.open_failed(e),
            Response::Children { node, children, error } => self.doc.children_arrived(node as usize, children, error),
            Response::View { kind, view } => {
                // Keep the hex space only if the new view still describes it.
                let hex_space = self.doc.hex().space;
                let keep_space = hex_space == Space::File || view.spaces.iter().any(|s| s.space == hex_space);
                if self.doc.accept_view(kind, view) {
                    self.sel.clear();
                    self.find.clear_matches();
                    if !keep_space {
                        let len = self.doc.file_len();
                        self.doc.hex_mut().set_space(Space::File, len, FILE_SPACE_TITLE.into());
                        self.load_hex_page(Space::File, 0);
                    }
                    if !self.find.query().is_empty() {
                        self.find.update_matches(self.doc.view());
                    }
                }
            }
            Response::Page { req, space, outcome, .. } => {
                let size = match (space, &outcome) {
                    (Space::File, Outcome::Done((_, size))) => Some(*size),
                    _ => None,
                };
                // The file length is taken only from the page the hex view accepted.
                if self.doc.hex_mut().complete(req, space, outcome) {
                    if let Some(size) = size {
                        self.doc.set_file_len(size);
                    }
                }
            }
            Response::Copied { req, sel, outcome } => match self.jobs.copy_reply(req, sel, self.sel.epoch()) {
                CopyReply::Accepted => {
                    let note = match outcome {
                        Outcome::Done((text, note)) => {
                            clip = Some(text);
                            note
                        }
                        Outcome::Failed(e) => format!("Copy failed: {e}"),
                        Outcome::Cancelled => "Copy cancelled".into(),
                    };
                    self.jobs.set_copy_note(note);
                }
                CopyReply::Stale => self.jobs.set_copy_note("Discarded a stale copy (selection changed); clipboard unchanged"),
                CopyReply::Ignored => {}
            },
            Response::Stat { req, outcome } => {
                if self.jobs.take_stat(req) {
                    match outcome {
                        Outcome::Done(now) => self.doc.stat_result(Some(now), false),
                        // A refused request (in-flight cap) says nothing about the file.
                        Outcome::Failed(m) => self.doc.stat_result(None, m != jobs::REFUSED),
                        Outcome::Cancelled => self.doc.stat_result(None, false),
                    }
                }
            }
            Response::SearchProgress { done, total } => self.find.set_progress(Some((done, total))),
            Response::SearchHit { off, len } => {
                self.find.set_progress(None);
                self.find.set_hit(Some(off));
                self.find.set_status(format!("Bytes: match at file 0x{off:x} (physical bytes)"));
                let span = Some(Span::file(off, len));
                self.sel.set_span(span);
                self.show_span(span);
                self.select_row_for(Space::File, off);
            }
            Response::SearchDone { found: _, cancelled } => {
                self.find.set_progress(None);
                self.find.set_status(if cancelled { "Byte search cancelled" } else { "Bytes: no further match in the physical file bytes (compressed contents are not searched)" });
            }
        }
        clip
    }

    // ---------------- search ----------------

    fn search_step(&mut self, backward: bool) {
        if self.find.query().is_empty() {
            return;
        }
        // 1) decoded rows of the current table only
        if let Some(row) = self.find.step_rows(self.sel.row(), backward) {
            self.select_row(row);
            return;
        }
        // 2) physical bytes of the file (bounded chunked background search; not decompressed)
        let Some(p) = self.doc.file().map(|p| p.to_path_buf()) else { return };
        match jobs::parse_query(self.find.query(), self.find.hex_mode()) {
            Ok(needle) => {
                let from = match (self.find.hit(), backward) {
                    (Some(h), false) => h + 1,
                    (Some(h), true) => h,
                    (None, false) => self.sel.span().filter(|s| s.space == Space::File).map(|s| s.offset).unwrap_or(0),
                    (None, true) => u64::MAX,
                };
                self.find.set_status("Bytes: searching the physical file bytes…");
                self.find.set_progress(Some((0, 1)));
                self.jobs.search(self.doc.generation(), p, needle, from, backward);
            }
            Err(e) => self.find.set_status(e),
        }
    }

    /// Layout reset: panel sizes/visibility, center split, table columns and zoom (100 %)
    /// back to defaults. Keeps file, history, selection, hex page, search and theme.
    fn reset_view(&mut self) {
        self.view_prefs.reset_layout(std::time::Instant::now());
        self.sel.rescroll();
    }

    // ---------------- selection ----------------

    fn select_row(&mut self, i: usize) {
        self.jobs.set_copy_note("");
        let span = self.doc.row(Some(i)).and_then(|r| r.field.span);
        self.sel.select_row(i, span);
        self.show_span(span);
    }

    fn move_row(&mut self, m: RowMove) {
        let n = self.doc.view().map(|v| v.rows.len()).unwrap_or(0);
        if n == 0 {
            return;
        }
        let cur = self.sel.row();
        let t = match m {
            RowMove::Up => cur.map(|c| c.saturating_sub(1)).unwrap_or(0),
            RowMove::Down => cur.map(|c| (c + 1).min(n - 1)).unwrap_or(0),
            RowMove::PageUp => cur.map(|c| c.saturating_sub(LIMITS.page_rows)).unwrap_or(0),
            RowMove::PageDown => cur.map(|c| (c + LIMITS.page_rows).min(n - 1)).unwrap_or(0),
            RowMove::Home => 0,
            RowMove::End => n - 1,
        };
        self.select_row(t);
    }

    /// Selects the smallest row whose span contains the offset. Returns false if none.
    fn select_row_for(&mut self, space: Space, off: u64) -> bool {
        match self.doc.row_at(space, off) {
            Some((i, span)) => {
                self.sel.select_row(i, span);
                true
            }
            None => false,
        }
    }

    /// Selects `span` in its own space: the row whose span is exactly this one, otherwise a raw
    /// range. The byte view switches to the span's space and page.
    fn select_span(&mut self, span: Span) {
        let row = self.doc.view().and_then(|v| v.rows.iter().position(|r| r.field.span == Some(span)));
        match row {
            Some(i) => self.select_row(i),
            None => {
                self.sel.select_raw(span, "Bytes named by a diagnostic.");
                self.show_span(Some(span));
            }
        }
    }

    /// Shim (tests/phase_b_state.rs): raw byte-range selection, as a hex click would make it.
    pub fn select_raw(&mut self, s: Span, note: &str) {
        self.sel.select_raw(s, note);
    }

    fn follow(&mut self, f: &Field) {
        if let Some(p) = &f.open_path {
            let p = p.clone();
            self.open_file(p);
        } else if let Some(k) = &f.link {
            let k = k.clone();
            self.request_view(k, true);
        }
    }

    fn goto_offset(&mut self) {
        let t = self.doc.goto().trim().to_string();
        let parsed = if let Some(h) = t.strip_prefix("0x") { u64::from_str_radix(h, 16).ok() } else { t.parse::<u64>().ok() };
        let size = self.doc.hex().size;
        match parsed {
            Some(off) if off < size.max(1) => {
                let sp = self.doc.hex().space;
                if !self.select_row_for(sp, off) {
                    self.sel.select_raw(Span::new(sp, off, 1), "No field in the current table covers this offset.");
                }
                if self.doc.hex_mut().ensure_visible(off) {
                    let base = self.doc.hex().page_base;
                    self.load_hex_page(sp, base);
                }
            }
            Some(off) => self.doc.set_status_msg(format!("offset {off} is outside the current space ({size} bytes)")),
            None => self.doc.set_status_msg("enter a decimal offset or 0x-prefixed hex"),
        }
    }

    /// Shim (tests/phase_b_state.rs): same as `Action::Copy`.
    pub fn do_copy(&mut self, k: CopyKind) {
        self.copy(k);
    }

    /// Starts a copy job for the current selection. The clipboard is written when the result
    /// arrives, and only if the selection is still the one it was requested for.
    fn copy(&mut self, k: CopyKind) {
        let row = self.doc.row(self.sel.row()).cloned();
        let g = self.doc.generation();
        let epoch = self.sel.epoch();
        let text = |text: String, note: String| Ok(CopyJob::Text { text, note });
        let job: Result<CopyJob, String> = match (k, row) {
            (CopyKind::Hex, _) => match self.sel.span() {
                None => Err("Nothing selected to copy".into()),
                Some(s) => {
                    let len = s.len.min(LIMITS.copy_cap);
                    if s.space == Space::File {
                        match self.doc.file() {
                            Some(p) => Ok(CopyJob::FileHex { path: p.to_path_buf(), off: s.offset, len, requested: s.len }),
                            None => Err("No file open".into()),
                        }
                    } else {
                        self.jobs.copy_bytes(g, epoch, s.space, s.offset, len, s.len);
                        self.jobs.set_copy_note("Copying…");
                        return;
                    }
                }
            },
            (CopyKind::Value, Some(r)) => match &r.field.value_bytes {
                Some(b) => {
                    let (t, note) = bytes_copy_text(b, "value");
                    text(t, note)
                }
                None => {
                    let n = r.field.value.chars().count();
                    text(r.field.value.clone(), format!("Copied value ({n} chars, displayed text)"))
                }
            },
            (CopyKind::Key, Some(r)) => match &r.field.key_bytes {
                Some(b) => {
                    let (t, note) = bytes_copy_text(b, "key");
                    text(t, note)
                }
                None => Err("This row has no key".into()),
            },
            (CopyKind::Offset, _) => match self.sel.span() {
                Some(s) => text(format!("{} 0x{:x}..0x{:x} ({} bytes)", s.space.label(), s.offset, s.offset + s.len, s.len), "Copied offset range".into()),
                None => Err("No byte range selected".into()),
            },
            (CopyKind::RowTsv, Some(r)) => text(r.cells.join("\t"), format!("Copied row ({} columns, TSV; cells are the table previews)", r.cells.len())),
            (_, None) => Err("No row selected".into()),
        };
        match job {
            Ok(job) => {
                self.jobs.copy(g, epoch, job);
                self.jobs.set_copy_note("Copying…");
            }
            Err(e) => self.jobs.set_copy_note(e),
        }
    }

    /// Clipboard text that needs no read still goes through the copy gate.
    fn copy_text(&mut self, text: String, note: &str) {
        self.jobs.copy(self.doc.generation(), self.sel.epoch(), CopyJob::Text { text, note: note.into() });
    }
}
