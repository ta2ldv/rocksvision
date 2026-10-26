//! Application state (egui-independent): a coordinator of five focused components with private
//! fields — `DocumentSession` (folder/file, generation, tree, view, history, hex page),
//! `SelectionState` (row/range + selection epoch), `SearchState`, `JobState` (worker, request
//! ids, copy gate) and `ViewPreferences` (theme, zoom, layout toggles). Panels read them through
//! `&` getters; the only mutators are `apply(Action)`, `poll()` (job replies) and `tick()`
//! (plus the documented test shims `select_raw`/`do_copy`).

use super::actions::{Action, CopyKind, Effect, Panel, RowMove};
use super::hex::PageState;
use super::job_state::{CopyReply, JobState};
use super::jobs::{self, CopyJob, Outcome, Request, Response, SelectHint, Tagged, ViewTarget, Worker};
use super::provenance::{exact_file_ranges, field_at};
use super::search::SearchState;
use super::selection::{SelOrigin, SelSnapshot, SelectionState};
use super::session::DocumentSession;
use super::view_prefs::ViewPreferences;
use crate::limits::LIMITS;
use crate::model::*;
use std::path::PathBuf;

/// Selection to apply when an async view arrives — only if the selection epoch is still the
/// one recorded at the request (a newer user choice always wins).
#[derive(Clone, Debug)]
struct PendingSel {
    epoch: u64,
    origin: SelOrigin,
    anchor: Option<Span>,
    hint: Option<SelectHint>,
    /// Origin of a navigation, for the structure card ("Referenced from").
    via: Option<String>,
    /// Back: the saved selection and byte-view position to restore as they were.
    restore: Option<(SelSnapshot, Space, u64)>,
}

impl PendingSel {
    fn new(epoch: u64, origin: SelOrigin, anchor: Option<Span>, hint: Option<SelectHint>) -> Self {
        PendingSel { epoch, origin, anchor, hint, via: None, restore: None }
    }
}

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
    /// Tree node to bring into view (a reveal).
    pub tree: Option<usize>,
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
    pending_sel: Option<PendingSel>,
}

impl AppState {
    pub fn new(worker: Worker) -> Self {
        AppState { doc: DocumentSession::new(), sel: SelectionState::new(), find: SearchState::new(), jobs: JobState::new(worker), view_prefs: ViewPreferences::new(), pending_sel: None }
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
                // A reference node is a link: go to the canonical structure.
                let reference = self.doc.nodes().get(i).filter(|n| n.role == ChildRole::Reference).map(|n| n.kind.clone());
                if let Some(k) = reference {
                    self.navigate(k, None);
                } else if let Some(k) = self.doc.select_node(Some(i)) {
                    self.open_structure(k);
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
                // The previous view with its exact selection and byte-view position.
                if let Some(h) = self.doc.pop_history() {
                    self.doc.select_loaded(&h.kind);
                    let mut p = PendingSel::new(self.sel.epoch(), h.sel.origin, None, None);
                    p.restore = Some((h.sel, h.hex_space, h.hex_at));
                    self.pending_sel = Some(p);
                    self.request_view(h.kind, false);
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
            Action::Navigate { target, span } => self.navigate(target, span),
            Action::StepField(forward) => self.step_field(forward),
            Action::StepStructure(forward) => self.step_structure(forward),
            Action::ToggleAllColumns => self.view_prefs.toggle_all_columns(),
            Action::HexPage(base) => self.load_hex_page(self.doc.hex().space, base),
            Action::HexShowSpace(space) => {
                let (t, size) = self.doc.space_title(space);
                self.doc.hex_mut().set_space(space, size, t);
                self.load_hex_page(space, 0);
            }
            Action::SetGoto(s) => self.doc.set_goto(s),
            Action::GotoOffset => self.goto_offset(),
            Action::HexClick(off) => self.hex_click(off),
            Action::HexShiftClick(off) => {
                let space = self.doc.hex().space;
                // Extends from the anchor (the last clicked byte), else from the range start.
                let from = self.sel.anchor().filter(|a| a.space == space).or(self.sel.span().filter(|s| s.space == space)).map(|s| s.offset).unwrap_or(off);
                let (a, b) = (from.min(off), from.max(off) + 1);
                self.sel.extend_raw(Span::new(space, a, b - a));
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
        let tree = if self.view_prefs.show_left() { self.doc.take_tree_scroll() } else { None };
        ScrollRequest { row, hex, tree }
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
        self.pending_sel = None;
        self.doc.begin_open(p.clone());
        self.jobs.decode(self.doc.generation(), Request::Open(p));
        self.request_view(NodeKind::Root, false);
        // A new file's byte view starts at 0x0 (no scroll position kept from the previous file).
        let _ = self.doc.hex_mut().ensure_visible(0);
        self.load_hex_page(Space::File, 0);
    }

    fn request_view(&mut self, k: NodeKind, push_history: bool) {
        self.request(ViewTarget::Node(k), push_history, false);
    }

    /// The current selection and byte-view position (what Back restores).
    fn here(&self) -> (SelSnapshot, Space, u64) {
        let hex = self.doc.hex();
        let at = self.sel.span().filter(|s| s.space == hex.space()).map(|s| s.offset).unwrap_or(hex.page_base());
        (self.sel.snapshot(), hex.space(), at)
    }

    /// Issues a view request; `reveal` also asks for the canonical tree path.
    fn request(&mut self, t: ViewTarget, push_history: bool, reveal: bool) {
        let h = push_history.then(|| self.here());
        self.request_with(t, h, reveal);
    }

    fn request_with(&mut self, t: ViewTarget, history: Option<(SelSnapshot, Space, u64)>, reveal: bool) {
        let req = self.jobs.decode(self.doc.generation(), Request::View { target: t.clone(), reveal });
        self.doc.request_view(t, req, history);
    }

    /// What is selected now, in words (the origin of a navigation): "entry 2 of Index block
    /// @0x…", else the tree node or view shown.
    fn describe_here(&self) -> Option<String> {
        let v = self.doc.view()?;
        match self.selected_field() {
            Some(f) => Some(format!("{} of {}", f.name, v.title)),
            None => Some(self.doc.sel_node().and_then(|i| self.doc.nodes().get(i)).map(|n| n.label.clone()).unwrap_or_else(|| v.title.clone())),
        }
    }

    /// The one navigation: canonical node, tree reveal, view, selection of `span` (or the
    /// whole target structure), byte view, history (Back returns to the source).
    fn navigate(&mut self, target: NodeKind, span: Option<Span>) {
        let mut p = PendingSel::new(self.sel.epoch(), SelOrigin::Navigate, None, Some(SelectHint { at: None, span }));
        p.via = self.describe_here();
        self.pending_sel = Some(p);
        self.request(ViewTarget::Node(target), true, true);
        if span.is_some() {
            self.show_span(span);
        }
    }

    /// A tree click / structure step: the node's view with the whole structure selected (same
    /// path for both).
    fn open_structure(&mut self, k: NodeKind) {
        self.pending_sel = Some(PendingSel::new(self.sel.epoch(), SelOrigin::Tree, None, None));
        self.request_view(k, true);
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
            Response::View { req, kind, view, reveal, select, .. } => {
                let Some(kind) = kind else {
                    // Nothing located (or a superseded request): the raw selection stays.
                    if self.doc.drop_pending(req) {
                        self.pending_sel = None;
                        self.doc.set_status_msg("No known structure holds this byte");
                    }
                    return None;
                };
                // Keep the hex space only if the new view still describes it.
                let hex_space = self.doc.hex().space;
                let keep_space = hex_space == Space::File || view.spaces.iter().any(|s| s.space == hex_space);
                let extent = view.extent;
                if self.doc.accept_view(req, kind.clone(), view) {
                    // A pending selection applies only if nothing was selected since the request.
                    let pending = self.pending_sel.take().filter(|p| p.epoch == self.sel.epoch());
                    self.sel.clear();
                    self.find.clear_matches();
                    if let Some(r) = reveal {
                        self.doc.reveal(r.path, &kind);
                    }
                    if !keep_space {
                        let len = self.doc.file_len();
                        self.doc.hex_mut().set_space(Space::File, len, FILE_SPACE_TITLE.into());
                        self.load_hex_page(Space::File, 0);
                    }
                    match pending {
                        Some(p) => self.apply_pending(p, select, extent),
                        // A plain node view moves the byte view to the structure's start.
                        None => {
                            if let Some(e) = extent {
                                self.show_span(Some(Span::file(e.offset, 1)));
                            }
                        }
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
        self.select_field_row(i, SelOrigin::Row, None);
    }

    /// Selects field row `i` with its span and exact file ranges. A byte-view click keeps the
    /// byte view where it is (`anchor` = the clicked byte); other origins move it: to the file
    /// range when the field maps exactly to file bytes (file offset is the primary location),
    /// else to the field's own space.
    fn select_field_row(&mut self, i: usize, origin: SelOrigin, anchor: Option<Span>) {
        self.jobs.set_copy_note("");
        let span = self.doc.row(Some(i)).and_then(|r| r.field.span);
        let extra: Vec<Span> = span.filter(|s| s.space != Space::File).map(|s| exact_file_ranges(self.doc.view(), &s)).unwrap_or_default();
        self.sel.select_field(i, span, extra.clone(), origin, anchor);
        if origin == SelOrigin::Hex {
            return;
        }
        match extra.first() {
            Some(f) if self.doc.hex().space == Space::File || self.doc.hex().space != span.map(|s| s.space).unwrap_or(Space::File) => self.show_span(Some(*f)),
            _ => self.show_span(span),
        }
    }

    /// A byte click: the narrowest field of the current table holding the byte (the clicked
    /// byte stays outlined); otherwise the structure holding it is located (bounded worker
    /// request) and its view opens with the field — or the structure — selected.
    fn hex_click(&mut self, off: u64) {
        let space = self.doc.hex().space;
        let byte = Span::new(space, off, 1);
        if let Some(i) = self.doc.view().and_then(|v| field_at(v, space, off)) {
            self.select_field_row(i, SelOrigin::Hex, Some(byte));
            // Reveal the left node of the table shown (no load, no new selection).
            if let Some(k) = self.doc.view_kind().cloned() {
                self.doc.select_loaded(&k);
            }
            return;
        }
        if space != Space::File && !matches!(space, Space::Record(_)) {
            self.sel.click_raw(byte, "No field of the current table covers this offset.");
            return;
        }
        let before = self.here();
        self.sel.click_raw(byte, "Locating the structure that holds this byte…");
        self.pending_sel = Some(PendingSel::new(self.sel.epoch(), SelOrigin::Hex, Some(byte), None));
        self.request_with(ViewTarget::Locate { space, off }, Some(before), true);
    }

    /// Applies the selection of an arrived view (navigation or located byte): the narrowest
    /// field at the byte, else the row with exactly the span, else the structure range. A
    /// navigation without a span moves the byte view to the structure (`extent`).
    fn apply_pending(&mut self, p: PendingSel, select: Option<SelectHint>, extent: Option<Span>) {
        if let Some((snap, space, at)) = p.restore {
            // Back: the exact previous selection, in the byte space and position it had.
            self.sel.restore(snap);
            let known = space == Space::File || self.doc.view().is_some_and(|v| v.spaces.iter().any(|s| s.space == space));
            if known {
                if space != self.doc.hex().space() {
                    let (t, size) = self.doc.space_title(space);
                    self.doc.hex_mut().set_space(space, size, t);
                }
                if self.doc.hex_mut().ensure_visible(at) {
                    let base = self.doc.hex().page_base();
                    self.load_hex_page(space, base);
                }
            }
            return;
        }
        let hint = select.or(p.hint.clone()).unwrap_or(SelectHint { at: None, span: None });
        let Some(view) = self.doc.view() else { return };
        if let Some((space, off)) = hint.at {
            if let Some(i) = field_at(view, space, off) {
                self.select_field_row(i, p.origin, p.anchor);
                return;
            }
        }
        if let Some(s) = hint.span {
            if let Some(i) = view.rows.iter().position(|r| r.field.span == Some(s)) {
                self.select_field_row(i, p.origin, p.anchor);
                return;
            }
            let compressed = view.spaces.iter().find(|sp| matches!(&sp.map, SpaceMap::Compressed { file, .. } if file.offset <= s.offset && s.offset < file.offset + file.len.max(1)));
            let note = match compressed {
                Some(sp) => format!("Compressed contents of {} [0x{:x},0x{:x}): stored bytes have no byte-to-byte mapping to decompressed fields. Open the decompressed view to select a field ({}).", view.title, s.offset, s.offset + s.len, sp.space.label()),
                None => format!("{} [0x{:x},0x{:x}): structure bytes, not a single field.", view.title, s.offset, s.offset + s.len),
            };
            self.sel.select_structure(s, note, p.origin, p.anchor);
            if p.origin != SelOrigin::Hex {
                self.show_span(Some(s));
            }
            return;
        }
        if let Some(a) = p.anchor {
            // Located, but no field of that view holds the byte: the structure is the answer.
            let s = extent.unwrap_or(a);
            self.sel.select_structure(s, format!("{}: the clicked byte is part of this structure, not of a single field.", view.title), p.origin, Some(a));
        } else {
            // Navigation without a target row, tree click, structure step: the whole structure
            // (never "Nothing selected"); the byte view shows its first byte.
            let note = "The whole structure is selected (no single field). Select a row or a byte for a field.".to_string();
            self.sel.select_whole(extent, note, p.origin, p.via);
            if let Some(e) = extent {
                self.show_span(Some(Span::file(e.offset, 1)));
            }
        }
    }

    /// `]` / `[`: the next / previous field of the current table in span order (one step).
    fn step_field(&mut self, forward: bool) {
        let Some(v) = self.doc.view() else { return };
        let mut order: Vec<(usize, Span)> = v.rows.iter().enumerate().filter_map(|(i, r)| r.field.span.map(|s| (i, s))).collect();
        let space_key = |s: &Span| match s.space {
            Space::File => 0u64,
            Space::Block(o) | Space::Record(o) => 1 + o,
        };
        order.sort_by_key(|(i, s)| (space_key(s), s.offset, *i));
        if order.is_empty() {
            return;
        }
        let pos = self.sel.row().and_then(|r| order.iter().position(|(i, _)| *i == r));
        let next = match (pos, forward) {
            (Some(p), true) => (p + 1).min(order.len() - 1),
            (Some(p), false) => p.saturating_sub(1),
            (None, true) => 0,
            (None, false) => order.len() - 1,
        };
        let row = order[next].0;
        self.select_field_row(row, SelOrigin::Keyboard, None);
    }

    /// `Shift+]` / `Shift+[`: the next / previous sibling structure of the selected tree node
    /// in tree (file) order, as one navigation.
    fn step_structure(&mut self, forward: bool) {
        let next = match self.doc.sel_node() {
            Some(i) => self.doc.sibling(i, forward),
            None if forward => self.doc.roots().first().copied(),
            None => None,
        };
        // Same path as a tree click on the sibling (plus a one-shot tree reveal).
        if let Some(k) = next.and_then(|j| self.doc.select_and_reveal(j)) {
            self.open_structure(k);
        }
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

    /// Selects the narrowest field holding the offset (byte view stays). Returns false if none.
    fn select_row_for(&mut self, space: Space, off: u64) -> bool {
        match self.doc.view().and_then(|v| field_at(v, space, off)) {
            Some(i) => {
                self.select_field_row(i, SelOrigin::Hex, Some(Span::new(space, off, 1)));
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
            Some(i) => self.select_field_row(i, SelOrigin::Diagnosis, None),
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
            self.navigate(k, None);
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
                    self.sel.click_raw(Span::new(sp, off, 1), "No field in the current table covers this offset.");
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
