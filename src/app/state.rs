//! Application state (egui-independent): a coordinator of focused components with private
//! fields — `DocumentSession` (folder/file, generation, tree, view, `Navigation`, `HexState`),
//! `SelectionState` (row/range + selection epoch), `SearchState` (query, row matches, typed byte
//! search), `JobState` (worker, request ids, copy gate) and `ViewPreferences` (theme, zoom,
//! layout toggles, the one transient notice). AppState dispatches actions, issues jobs, routes
//! replies to the component that guards them and executes the components' plans
//! (`navigation::plan_arrival`, `clipboard::plan`, `HexState::reveal`). Panels read the
//! components through `&` getters; the only mutators are `apply(Action)`, `poll()` (job replies)
//! and `tick()` (plus the documented test shims `select_raw`/`do_copy`).

use super::actions::{Action, CopyKind, Effect, Panel, RowMove};
use super::clipboard::{self, CopyInput, CopyPlan};
use super::hex::{HexState, PageReq, PageState};
use super::job_state::JobState;
use super::jobs::{self, CopyJob, Outcome, Request, Response, SelectHint, Tagged, ViewTarget, Worker};
use super::navigation::{Arrival, Here, HistoryOp, Navigation, PendingSel, ViewReply, plan_arrival};
use super::provenance::{self, HitTarget, exact_file_ranges, field_at};
use super::search::SearchState;
use super::selection::{self, SelOrigin, SelectionState};
use super::session::DocumentSession;
use super::view_prefs::{COPY_NOTICE_SECS, NoticeOwner, ViewPreferences};
use crate::limits::LIMITS;
use crate::model::*;
use std::path::PathBuf;

pub use super::clipboard::{PendingCopy, bytes_copy_text, copy_accepted};
pub use super::session::{FILE_SPACE_TITLE, TreeNode};
pub use super::view_prefs::{CENTER_SPLIT_DEFAULT, ZoomFit, ZoomPolicy};

#[cfg(test)]
#[path = "state_tests.rs"]
mod state_tests;

/// One-shot scroll requests for the UI (taken once per frame).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScrollRequest {
    /// Records table row to bring into view, and whether to center it (a jump) or scroll
    /// minimally (row keys).
    pub row: Option<(usize, bool)>,
    /// Byte offset to bring into view in the hex rows.
    pub hex: Option<u64>,
    /// Tree node to bring into view (a reveal).
    pub tree: Option<usize>,
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
    /// History and the view request in flight.
    pub fn navigation(&self) -> &Navigation {
        self.doc.navigation()
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
            // 0020 §5: a new query, Clear and Cancel stop the running byte search; its late
            // replies are ignored by request id (`SearchState::on_*`).
            Action::SetSearch(s) => {
                self.cancel_byte_search();
                self.find.set_query(s, self.doc.view());
            }
            Action::SetSearchHex(b) => {
                self.cancel_byte_search();
                self.find.set_hex(b, self.doc.view());
            }
            Action::SearchNext => self.search_step(false),
            Action::SearchPrev => self.search_step(true),
            Action::ClearSearch => {
                self.cancel_byte_search();
                self.find.clear();
            }
            Action::CancelSearch => self.cancel_byte_search(),
            Action::Copy(k) => self.copy(k),
            Action::CopyRow(i, k) => {
                self.select_row(i);
                self.copy(k);
            }
            Action::CopyFilePath => {
                if self.doc.file().is_some() {
                    self.copy(CopyKind::FilePath);
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
                // A newer target: the request replaces any in flight, so an in-flight Locate's
                // pending selection cannot reach Root.
                if let Some(k) = self.doc.select_node(None) {
                    self.request(ViewTarget::Node(k), true, false, None);
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
                // The previous view with its exact selection and byte-view position; overtaken
                // before it arrives, the entry goes back on the stack (`Navigation`).
                if let Some(h) = self.doc.back() {
                    self.doc.select_loaded(&h.kind);
                    let mut p = PendingSel::new(self.sel.epoch(), h.sel.origin, None, None);
                    p.restore = Some(Here { sel: h.sel.clone(), hex_space: h.hex_space, hex_at: h.hex_at, goto: h.goto.clone() });
                    let t = ViewTarget::Node(h.kind.clone());
                    self.request_with(t, HistoryOp::Popped(h), false, Some(p));
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
            // The Go-to text follows whatever the byte pane moves to (B3, 0015 C3).
            Action::HexPage(base) => {
                let p = self.doc.hex_mut().page_to(base);
                self.load_page(p);
            }
            Action::HexShowSpace(space) => {
                let (t, size) = self.doc.space_title(space);
                let p = self.doc.hex_mut().show_space(space, size, t);
                self.load_page(p);
            }
            Action::SetGoto(s) => self.doc.hex_mut().set_goto(s),
            Action::GotoOffset => self.goto_offset(),
            Action::HexClick(off) => self.hex_click(off),
            Action::HexShiftClick(off) => self.sel.extend_to(self.doc.hex().space(), off),
            Action::SelectSpan(span) => self.select_span(span),
            Action::HexRetry => self.load_hex_page(self.doc.hex().space(), self.doc.hex().page_base()),
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
        } else if *self.doc.hex().load() != PageState::Idle {
            self.doc.hex_mut().idle();
        }
    }

    /// Takes the one-shot scroll requests. The hex request survives until a non-empty page of
    /// an open file is shown; the row request until a view is shown.
    pub fn take_scroll_request(&mut self) -> ScrollRequest {
        let hex_ready = self.doc.file().is_some() && self.doc.hex().page().map(|p| !p.is_empty()).unwrap_or(false);
        let row = if self.doc.view().is_some() { self.sel.take_scroll_row() } else { None };
        let hex = if hex_ready && self.view_prefs.show_hex() { self.doc.hex_mut().take_scroll() } else { None };
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
        } else if self.jobs.decoding() || self.navigation().pending_target().is_some() {
            Some("Decoding…")
        } else if self.jobs.clipboard().pending().is_some() {
            Some(clipboard::COPYING)
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

    /// Ends the file session (new generation: no view, history, request in flight or page).
    fn close_file(&mut self) {
        self.doc.close();
        self.jobs.cancel_file_jobs();
        self.jobs.cancel_search();
        self.sel.clear();
        // The query stays; byte-search results and row matches described the old file.
        self.find.forget_bytes();
        self.find.clear_matches();
    }

    fn open_file(&mut self, p: PathBuf) {
        self.close_file();
        self.doc.begin_open(p.clone());
        self.jobs.decode(self.doc.generation(), Request::Open(p));
        self.request(ViewTarget::Node(NodeKind::Root), false, false, None);
        // A new file's byte view starts at 0x0 (no scroll position kept from the previous file).
        let _ = self.doc.hex_mut().ensure_visible(0);
        self.load_hex_page(Space::File, 0);
    }

    /// The current selection, byte-view position and Go-to text (what Back restores).
    fn here(&self) -> Here {
        let hex = self.doc.hex();
        let at = self.sel.span().or(self.sel.shown()).filter(|s| s.space == hex.space()).map(|s| s.offset).unwrap_or(hex.page_base());
        Here { sel: self.sel.snapshot(), hex_space: hex.space(), hex_at: at, goto: hex.goto().to_string() }
    }

    /// Issues a view request; `push_history` pushes the current view (Back returns to it);
    /// `reveal` also asks for the canonical tree path; `pending` is selected on arrival.
    fn request(&mut self, t: ViewTarget, push_history: bool, reveal: bool, pending: Option<PendingSel>) {
        let hist = if push_history { HistoryOp::Push(self.here()) } else { HistoryOp::None };
        self.request_with(t, hist, reveal, pending);
    }

    /// The decode request and its navigation record (guarded by the epoch read now, after any
    /// selection change the request itself made).
    fn request_with(&mut self, t: ViewTarget, hist: HistoryOp, reveal: bool, pending: Option<PendingSel>) {
        let req = self.jobs.decode(self.doc.generation(), Request::View { target: t.clone(), reveal });
        self.doc.begin_view(req, t, hist, pending, self.sel.epoch());
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
        self.request(ViewTarget::Node(target), true, true, Some(p));
        if span.is_some() {
            self.show_span(span);
        }
    }

    /// A tree click / structure step: the node's view with the whole structure selected (same
    /// path for both).
    fn open_structure(&mut self, k: NodeKind) {
        let p = PendingSel::new(self.sel.epoch(), SelOrigin::Tree, None, None);
        self.request(ViewTarget::Node(k), true, false, Some(p));
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
                    self.doc.hex_mut().idle();
                    return;
                }
            },
            _ => self.jobs.decode(g, Request::Bytes { space, off: base, len: LIMITS.hex_page_bytes }),
        };
        self.doc.hex_mut().begin(req, base);
    }

    fn load_page(&mut self, p: PageReq) {
        self.load_hex_page(p.space, p.base);
    }

    /// Moves the byte view to `s` (space, page, Go-to text: `HexState::reveal`) and reads the
    /// page if it is not the one shown.
    fn reveal(&mut self, s: Span) {
        let title = self.doc.space_title(s.space);
        if let Some(p) = self.doc.hex_mut().reveal(s, || title) {
            self.load_page(p);
        }
    }

    /// Moves the byte view to the selected span (switching space / page as needed); the Go-to
    /// text follows the offset shown (design B3).
    fn show_span(&mut self, span: Option<Span>) {
        if let Some(s) = span {
            self.reveal(s);
        }
    }

    /// Cancel button / Esc: cancels every job in flight and leaves no spinner behind.
    fn cancel_jobs(&mut self) {
        let (copy, scan, resolve) = self.jobs.cancel_all();
        self.find.cancel();
        self.doc.hex_mut().cancel();
        if copy {
            self.copy_notice(clipboard::COPY_CANCELLED);
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
        let job = t.job;
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
                let auto_open = self.jobs.scan_matches(req)?;
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
            Response::View { req, kind, view, reveal, select, .. } => match (self.doc.on_view_reply(req, kind.is_some(), self.sel.epoch()), kind) {
                (ViewReply::Accepted { pending }, Some(kind)) => self.view_arrived(kind, *view, reveal.map(|r| r.path), select, pending),
                // A newer selection was made since the request: the reply is discarded entirely
                // (no view, no selection, no tree reveal); the tree follows the view that stays
                // (a tree click may have moved it).
                (ViewReply::Overtaken, _) => {
                    if let Some(k) = self.doc.view_kind().cloned() {
                        self.doc.select_loaded(&k);
                    }
                }
                // Nothing located: the raw selection stays.
                (ViewReply::NothingLocated, _) => self.doc.set_status_msg("No known structure holds this byte"),
                _ => {}
            },
            Response::Page { req, space, outcome, .. } => {
                let size = match (space, &outcome) {
                    (Space::File, Outcome::Done((_, size))) => Some(*size),
                    _ => None,
                };
                // The file length is taken only from the page the hex view accepted.
                if self.doc.hex_mut().complete(req, space, outcome)
                    && let Some(size) = size
                {
                    self.doc.set_file_len(size);
                }
            }
            Response::Copied { req, sel, outcome } => {
                if let Some(r) = self.jobs.clipboard_mut().on_reply(req, sel, self.sel.epoch(), outcome) {
                    clip = r.text;
                    self.copy_notice(r.note);
                }
            }
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
            Response::SearchProgress { done, total } => self.find.on_progress(job, done, total),
            Response::SearchHit { off, len } => {
                if let Some(span) = self.find.on_hit(job, off, len) {
                    self.show_span(Some(span));
                    // One decision (field / structure bytes / raw range); never a row the hit
                    // is not in (0020 D2).
                    match provenance::hit_target(self.doc.view(), &span) {
                        HitTarget::Field(i) => self.select_field_row(i, SelOrigin::Hex, Some(Span::file(off, 1))),
                        HitTarget::Structure => {
                            let what = self.doc.view().map(|v| v.title.clone()).unwrap_or_default();
                            self.sel.select_hit(span, true, format!("Byte-search match in {what}; not a single field."));
                        }
                        HitTarget::Raw => self.sel.select_hit(span, false, "Byte-search match outside the structure shown; no field of the current table covers these bytes.".into()),
                    }
                }
            }
            Response::SearchDone { found: _, cancelled } => self.find.on_done(job, cancelled),
        }
        clip
    }

    /// An accepted view: installed, selection replaced (by the pending one if it is still
    /// current), tree revealed, byte view kept only if the new view still describes its space.
    fn view_arrived(&mut self, kind: NodeKind, view: View, reveal: Option<Vec<(NodeKind, Vec<Child>)>>, select: Option<SelectHint>, pending: Option<Box<PendingSel>>) {
        let extent = view.extent;
        self.doc.show_view(kind.clone(), view);
        let keep_space = self.doc.describes_space(self.doc.hex().space());
        self.sel.clear();
        self.find.clear_matches();
        if let Some(path) = reveal {
            self.doc.reveal(path, &kind);
        }
        if !keep_space {
            let len = self.doc.file_len();
            let p = self.doc.hex_mut().show_space(Space::File, len, crate::app::FILE_SPACE_TITLE.into());
            self.load_page(p);
        }
        match pending {
            Some(p) => {
                if let Some(v) = self.doc.view() {
                    let a = plan_arrival(*p, select, v, extent);
                    self.arrive(a);
                }
            }
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

    /// Executes the arrival plan of a view (`navigation::plan_arrival`).
    fn arrive(&mut self, a: Arrival) {
        match a {
            Arrival::Restore(Here { sel, hex_space: space, hex_at: at, goto }) => {
                // Back: the exact previous selection, in the byte space and position it had, and
                // the Go-to text (design B7) — restored only with the byte view it describes.
                self.sel.restore(sel);
                if self.doc.describes_space(space) {
                    self.reveal(Span::new(space, at, 1));
                    self.doc.hex_mut().set_goto(goto);
                }
            }
            Arrival::Field { row, origin, anchor } => self.select_field_row(row, origin, anchor),
            Arrival::Structure { span, note, origin, anchor, reveal } => {
                self.sel.select_structure(span, note, origin, anchor);
                if reveal {
                    self.show_span(Some(span));
                }
            }
            Arrival::Whole { extent, note, origin, via } => {
                self.sel.select_whole(extent, note, origin, via);
                if let Some(e) = extent {
                    self.show_span(Some(Span::file(e.offset, 1)));
                }
            }
        }
    }

    /// Stops the running byte search (worker and state).
    fn cancel_byte_search(&mut self) {
        self.jobs.cancel_search();
        self.find.cancel();
    }

    // ---------------- search ----------------

    fn search_step(&mut self, backward: bool) {
        // Empty query or invalid hex: Next/Prev do nothing (and are disabled).
        if !self.find.can_step() {
            return;
        }
        // 1) decoded rows of the current table only
        if let Some(row) = self.find.step_rows(self.sel.row(), backward) {
            self.select_row(row);
            return;
        }
        // 2) physical bytes of the file (bounded chunked background search; not decompressed)
        let Some(p) = self.doc.file().map(|p| p.to_path_buf()) else { return };
        let sel_off = self.sel.span().filter(|s| s.space == Space::File).map(|s| s.offset);
        match self.find.next_byte_search(sel_off, backward) {
            Ok(q) => {
                // The worker supersedes any running search; its id guards the replies.
                let req = self.jobs.search(self.doc.generation(), p, q.needle, q.from, q.backward);
                self.find.started(req);
            }
            Err(e) => self.find.set_invalid(e),
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
    /// byte view where it is (`anchor` = the clicked byte); other origins move it
    /// (`provenance::reveal_for_field`): to the file range when the field maps exactly to file
    /// bytes (file offset is the primary location), else to the field's own space; a field
    /// without bytes shows the enclosing structure's first byte (design B1–B6: no span, nothing
    /// filled or outlined, no history entry; without an honest enclosing range the view stays).
    fn select_field_row(&mut self, i: usize, origin: SelOrigin, anchor: Option<Span>) {
        self.view_prefs.clear_notice(NoticeOwner::Copy);
        let span = self.doc.row(Some(i)).and_then(|r| r.field.span);
        let extra: Vec<Span> = span.filter(|s| s.space != Space::File).map(|s| exact_file_ranges(self.doc.view(), &s)).unwrap_or_default();
        self.sel.select_field(i, span, extra.clone(), origin, anchor);
        if origin == SelOrigin::Hex {
            return;
        }
        let at = provenance::reveal_for_field(self.doc.view(), span, &extra, self.doc.hex().space());
        if span.is_none() {
            self.sel.set_shown(at);
        }
        self.show_span(at);
    }

    /// A byte click: the narrowest field of the current table holding the byte (the clicked
    /// byte stays outlined); otherwise the structure holding it is located (bounded worker
    /// request) and its view opens with the field — or the structure — selected.
    fn hex_click(&mut self, off: u64) {
        let space = self.doc.hex().space();
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
        let p = PendingSel::new(self.sel.epoch(), SelOrigin::Hex, Some(byte), None);
        self.request_with(ViewTarget::Locate { space, off }, HistoryOp::Push(before), true, Some(p));
    }

    /// `]` / `[`: the next / previous field of the current table in span order (one step).
    fn step_field(&mut self, forward: bool) {
        let Some(row) = self.doc.view().and_then(|v| selection::next_field(v, self.sel.row(), forward)) else { return };
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
        self.select_row(selection::row_after(m, self.sel.row(), n));
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
        let parsed = HexState::parse_offset(self.doc.hex().goto());
        let size = self.doc.hex().size();
        let sp = self.doc.hex().space();
        match parsed {
            Some(off) if off < size.max(1) => {
                if !self.select_row_for(sp, off) {
                    self.sel.click_raw(Span::new(sp, off, 1), "No field in the current table covers this offset.");
                }
                if self.doc.hex_mut().ensure_visible(off) {
                    let base = self.doc.hex().page_base();
                    self.load_hex_page(sp, base);
                }
            }
            Some(off) => {
                let all = Span::new(sp, 0, size).range_text();
                self.doc.set_status_msg(format!("offset {off} is outside the current space ({all})"))
            }
            None => self.doc.set_status_msg("enter a decimal offset or 0x-prefixed hex"),
        }
    }

    /// Shim (tests/phase_b_state.rs): same as `Action::Copy`.
    pub fn do_copy(&mut self, k: CopyKind) {
        self.copy(k);
    }

    /// Starts a copy job for the current selection (`clipboard::plan`). The clipboard is
    /// written when the result arrives, and only if the selection is still the one it was
    /// requested for (`ClipboardState`).
    fn copy(&mut self, k: CopyKind) {
        let input = CopyInput { kind: k, row: self.doc.row(self.sel.row()), span: self.sel.span(), view: self.doc.view(), file: self.doc.file() };
        let (g, epoch) = (self.doc.generation(), self.sel.epoch());
        match clipboard::plan(input) {
            Ok(plan) => {
                match plan {
                    CopyPlan::Text { text, note } => self.jobs.copy(g, epoch, CopyJob::Text { text, note }),
                    CopyPlan::FileHex { path, off, len, requested } => self.jobs.copy(g, epoch, CopyJob::FileHex { path, off, len, requested }),
                    CopyPlan::DerivedHex { space, off, len, requested } => self.jobs.copy_bytes(g, epoch, space, off, len, requested),
                };
                // The file path is copied without a "Copying…" step (as before).
                if k != CopyKind::FilePath {
                    self.copy_notice(clipboard::COPYING);
                }
            }
            Err(e) => self.copy_notice(e),
        }
    }

    /// A copy message in the one transient status notice, timed (0020 UI ruling F1).
    fn copy_notice(&mut self, text: impl Into<String>) {
        self.view_prefs.notify(NoticeOwner::Copy, text, std::time::Instant::now(), Some(COPY_NOTICE_SECS));
    }
}
