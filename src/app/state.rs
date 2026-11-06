//! Application state (egui-independent): a coordinator of focused components with private
//! fields — `DocumentSession` (folder/file, generation, tree, view, `Navigation`, `HexState`),
//! `SelectionState` (row/range + selection epoch), `SearchState` (query, row matches, typed byte
//! search), `JobState` (worker, request ids, copy gate) and `ViewPreferences` (theme, zoom,
//! layout toggles, the one transient notice). AppState dispatches actions, issues jobs, routes
//! replies to the component that guards them and executes the components' plans
//! (`navigation::plan_arrival`, `clipboard::plan`, `HexState::reveal`). Panels read the
//! components through `&` getters; the only mutators are `apply(Action)`, `poll()` (job replies)
//! and `tick()` (plus the documented test shims `select_raw`/`do_copy`).

use super::actions::{Action, CopyKind, Effect, MapMode, MapMove, MapZoom, Panel, RowMove};
use super::clipboard::{self, CopyInput, CopyPlan};
use super::hex::{HexState, PageReq, PageState};
use super::job_state::JobState;
use super::jobs::{self, CopyJob, Outcome, Request, Response, SelectHint, Tagged, ViewTarget, Worker};
use super::lsm_map::{self, MapEntry, MapPick, MapSnapshot};
use super::navigation::{Arrival, Here, HistEntry, HistoryOp, Navigation, PendingSel, ViewReply, plan_arrival};
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

/// Status-bar message of Back with an empty history (Option+Left).
pub const NO_PREVIOUS_VIEW: &str = "No previous view";

pub use crate::model::NotScannedWhy;

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
    /// Records table vertical scroll offset (points) to restore (Back, 0024 D1); the records
    /// panel uses it when it is within the table, else the `row` request applies.
    pub table: Option<f32>,
    /// The LSM Map takes keyboard focus (it was opened by Shift+⌘M, the toolbar or the menu).
    pub map_focus: bool,
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
            Action::OpenFile(p) => self.switch_file(p),
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
            // 0026 D2 F2 / 0030 A8: a drag sets the split of the role shown (file views, the
            // levels table, the LSM Map); the others keep theirs.
            Action::SetCenterSplit(r) => self.view_prefs.set_split(self.doc.split_role(), r),
            Action::ResetCenterSplit => self.view_prefs.reset_split(self.doc.split_role()),
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
                // The previous view with its exact selection, byte-view position and map state;
                // overtaken before it arrives, the entry goes back on the stack (`Navigation`).
                // An entry of another file opens that file first (0024 §4).
                match self.doc.back() {
                    Some(h) => {
                        let mut p = PendingSel::new(self.sel.epoch(), h.sel.origin, None, None);
                        p.restore = Some(h.here());
                        let t = ViewTarget::Node(h.kind.clone());
                        if self.doc.file() == Some(h.file.as_path()) {
                            self.doc.select_loaded(&h.kind);
                            self.request_with(t, HistoryOp::Popped(h), false, Some(p));
                        } else {
                            let file = h.file.clone();
                            self.open_file_then(file, t, HistoryOp::Popped(h), p);
                        }
                    }
                    // The one transient status notice (shown with or without a file open), for
                    // the key, the menu and the button alike.
                    // (`status_msg` is only drawn without a file, hence the notice.)
                    None => {
                        self.doc.set_status_msg(NO_PREVIOUS_VIEW);
                        self.view_prefs.notify(NoticeOwner::Nav, NO_PREVIOUS_VIEW, std::time::Instant::now(), Some(COPY_NOTICE_SECS));
                    }
                }
            }
            Action::ShowLsmMap => self.show_lsm_map(),
            Action::FocusLsmMap => {
                if self.doc.map_shown() {
                    self.doc.map_mut().request_focus();
                }
            }
            Action::SetMapMode(m) => self.set_map_mode(m),
            Action::MapSelectCf(cf) => {
                // A selected file of another CF is deselected: the Explanation never describes a
                // file the map does not show (it shows the levels summary instead).
                let foreign = self.sel.row().and_then(|r| self.doc.view().and_then(|v| v.lsm.by_row(r))).is_some_and(|(c, _, _)| c.id != cf);
                if foreign {
                    self.sel.clear();
                }
                let shown = self.doc.shown_cf();
                self.doc.map_mut().set_cf(cf, shown);
            }
            // 0026 item 6: a stationary (fitting) map ignores zoom, pan and Center; Fit always.
            Action::MapZoom(z) => match z {
                MapZoom::Fit => self.doc.map_mut().fit(),
                _ if !self.doc.map().navigable() => {}
                MapZoom::In => {
                    let max = self.doc.map().zoom_max();
                    self.doc.map_mut().zoom_about(lsm_map::ZOOM_STEP, 0.5, max)
                }
                MapZoom::Out => {
                    let max = self.doc.map().zoom_max();
                    self.doc.map_mut().zoom_about(1.0 / lsm_map::ZOOM_STEP, 0.5, max)
                }
                MapZoom::Center => self.doc.map_mut().center(),
            },
            Action::MapView { pan, zoom } => {
                if self.doc.map().navigable() {
                    self.doc.map_mut().set_view(pan, zoom)
                } else {
                    // Stationary: nothing moves; a pending reveal is done (the scene is visible).
                    self.doc.map_mut().clear_reveal()
                }
            }
            Action::MapMeasure(m) => {
                self.doc.map_mut().set_measure(m);
            }
            Action::MapFitHeights(h) => self.doc.map_mut().set_heights(h),
            Action::SelectWal(n) => self.select_wal(n),
            Action::SelectMapPick(p) => self.select_pick(p),
            Action::OpenWal(n) => {
                // A symlink is listed but never followed (like the folder list).
                if let Some(p) = self.doc.wal_inventory().and_then(|w| w.by_number(n)).filter(|f| !f.symlink).map(|f| f.path.clone()) {
                    self.select_wal(n);
                    self.switch_file(p);
                }
            }
            Action::MapMaxZoom(max) => self.doc.map_mut().set_max_zoom(max),
            Action::MapRevealDone => self.doc.map_mut().clear_reveal(),
            Action::MapMove(m) => self.map_move(m),
            Action::SetTooltips(on) => self.view_prefs.set_tooltips(on),
            Action::SelectRow(i) => self.select_row(i),
            Action::FollowRow(i) => {
                if let Some(f) = self.doc.row(Some(i)).map(|r| r.field.clone()) {
                    self.follow(&f);
                }
            }
            Action::FollowSelected => {
                if let Some(f) = self.selected_field().cloned() {
                    self.follow(&f);
                } else if let Some(n) = self.doc.map().wal().filter(|_| self.doc.map_shown()) {
                    // Enter on a selected WAL of the band opens it (§4.9).
                    return self.apply(Action::OpenWal(n));
                }
            }
            Action::MoveRow(m) => self.move_row(m),
            Action::Navigate { target, span } => self.navigate(target, span),
            Action::StepField(forward) => self.step_field(forward),
            Action::StepStructure(forward) => self.step_structure(forward),
            Action::ToggleAllColumns => self.view_prefs.toggle_all_columns(),
            Action::SetTableScroll(y) => self.doc.set_table_scroll(y),
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
        let table = if self.doc.view().is_some() { self.doc.take_table_restore() } else { None };
        // Taken only while the map is drawn (the request survives the levels view's arrival).
        let map_focus = self.doc.map_shown() && self.doc.map_mut().take_focus_request();
        ScrollRequest { row, hex, tree, table, map_focus }
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
    /// A path opened by the user is a new root context: Back starts empty.
    fn open_path(&mut self, p: PathBuf) {
        self.doc.reset_history();
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

    /// A file switch within the app (folder list, a referenced file): the view shown now
    /// becomes a history entry, so Back returns to it (0024 §4), then the file opens.
    /// A file outside the listed folder gets its folder listed (the same generation-guarded scan
    /// as `OpenPath`), so `OpenFile` and `OpenPath` of one SST see the same CURRENT/MANIFEST.
    fn switch_file(&mut self, p: PathBuf) {
        self.push_here_for_switch(&p);
        let parent = p.parent().map(|d| d.to_path_buf());
        self.open_file(p);
        if let Some(d) = parent.filter(|d| !self.doc.is_dir(d)) {
            self.start_scan(d, false);
        }
    }

    /// Pushes the current file's view as a cross-file history entry (not for the same file).
    /// The request in flight is undone first (a Back's popped entry goes back on the stack), so
    /// the history reads `[…, popped, current]`.
    fn push_here_for_switch(&mut self, to: &std::path::Path) {
        self.doc.drop_inflight();
        if let Some(kind) = self.doc.view_kind().cloned()
            && self.doc.file().is_some_and(|f| f != to)
        {
            let h = self.here();
            self.doc.push_history(HistEntry::at(kind, h));
        }
    }

    /// Opens `p` and requests `t` (not Root) with `pending` applied on arrival, as one
    /// generation-guarded step: Back to another file's entry and the LSM Map entry (0024 §4).
    fn open_file_then(&mut self, p: PathBuf, t: ViewTarget, hist: HistoryOp, mut pending: PendingSel) {
        self.close_file();
        self.doc.begin_open(p.clone());
        self.jobs.decode(self.doc.generation(), Request::Open(p));
        // The pending selection is guarded by the epoch after the file switch cleared it.
        pending.epoch = self.sel.epoch();
        self.request_with(t, hist, true, Some(pending));
        let _ = self.doc.hex_mut().ensure_visible(0);
        self.load_hex_page(Space::File, 0);
    }

    /// The LSM Map entry (toolbar, View menu, Shift+⌘M): the shown map goes back to the table;
    /// otherwise the levels view of this CURRENT/MANIFEST, or of the folder's (focusing the open
    /// SST), opens in Map mode. Unavailable: nothing (the button is disabled with the reason).
    fn show_lsm_map(&mut self) {
        if self.doc.map_shown() {
            self.doc.map_mut().set_mode(MapMode::Table);
            return;
        }
        // An entry already on its way (e.g. the Via open with the SST focus) is not replaced.
        if self.navigation().pending_target() == Some(&ViewTarget::Node(NodeKind::ManifestLevels)) {
            return;
        }
        let map = |focus| MapSnapshot { mode: MapMode::Map, cf: None, pan: (0.0, 0.0), zoom: lsm_map::ZOOM_MIN, focus, pick: None, tier: lsm_map::LaneTier::Full };
        match self.doc.lsm_map_availability() {
            Ok(MapEntry::Shown) => {
                self.set_map_mode(MapMode::Map);
                self.doc.map_mut().request_focus();
            }
            Ok(MapEntry::InFile) => {
                let mut p = PendingSel::new(self.sel.epoch(), SelOrigin::Tree, None, None);
                p.map = Some(map(None));
                self.request(ViewTarget::Node(NodeKind::ManifestLevels), true, true, Some(p));
            }
            Ok(MapEntry::Via { path, focus, .. }) => {
                self.push_here_for_switch(&path);
                let mut p = PendingSel::new(0, SelOrigin::Tree, None, None);
                p.map = Some(map(focus));
                self.open_file_then(path, ViewTarget::Node(NodeKind::ManifestLevels), HistoryOp::None, p);
            }
            Err(_) => {}
        }
    }

    /// Table | Map of the levels view (no history entry); the selected file stays selected and
    /// the map focuses it (in its CF).
    fn set_map_mode(&mut self, m: MapMode) {
        if self.doc.view_kind() != Some(&NodeKind::ManifestLevels) {
            return;
        }
        self.doc.map_mut().set_mode(m);
        if m == MapMode::Map {
            // 0026 D1 decision 3: a WAL left unscanned (cancelled / refused) is requested again.
            self.start_wal_summaries(true);
        }
        if let Some((cf, n)) = self.sel.row().and_then(|r| self.doc.view().and_then(|v| v.lsm.by_row(r)).map(|(c, _, f)| (c.id, f.number))) {
            let shown = self.doc.shown_cf();
            self.doc.map_mut().set_cf(cf, shown);
            self.doc.map_mut().set_focus(Some(n), true);
        }
    }

    /// Arrow / Home / End on the map: the neighbouring box becomes focused and selected (its
    /// row), and the map pans to it.
    fn map_move(&mut self, m: MapMove) {
        if !self.doc.map_shown() {
            return;
        }
        // The WAL band (0026 §4.9): Left/Right/Home/End along the WALs (file number order),
        // Down to the first box of the first non-empty level; Up from the top level enters
        // the band at the newest WAL.
        let wals: Vec<u64> = self.doc.wal_inventory().map(|w| w.files.iter().filter_map(|f| f.number).collect()).unwrap_or_default();
        let on_wal = self.doc.map().wal();
        if let Some(n) = on_wal {
            let i = wals.iter().position(|x| *x == n);
            let to = match (m, i) {
                (MapMove::Left, Some(i)) => i.checked_sub(1).map(|j| wals[j]),
                (MapMove::Right, Some(i)) => wals.get(i + 1).copied(),
                (MapMove::Home, _) => wals.first().copied(),
                (MapMove::End, _) => wals.last().copied(),
                _ => None,
            };
            if let Some(t) = to {
                self.select_wal(t);
            }
            if m != MapMove::Down {
                return;
            }
        }
        let Some(v) = self.doc.view() else { return };
        let inv = &v.lsm;
        let Some(c) = self.doc.map().shown_cf(inv).and_then(|id| inv.cf(id)) else { return };
        // D7: the focus, else the selected levels row's file when it is in the CF shown.
        let in_cf = |n: &u64| c.levels.iter().any(|l| l.files.iter().any(|f| f.number == *n));
        let selected = self.sel.row().and_then(|r| inv.by_row(r)).map(|(_, _, f)| f.number);
        let from = self.doc.map().focus().or(selected).filter(|n| on_wal.is_none() && in_cf(n));
        // Up with nothing focused enters the WAL band at the newest WAL (D7), not the first box.
        if m == MapMove::Up
            && from.is_none()
            && on_wal.is_none()
            && let Some(newest) = wals.last().copied()
        {
            self.select_wal(newest);
            return;
        }
        let target = lsm_map::neighbour(c, from, m).map(|f| {
            let level = c.levels.iter().find(|l| l.files.iter().any(|g| g.number == f.number));
            (f.row, f.number, c.id, level.map(|l| lsm_map::fraction(l, f)))
        });
        let Some((row, number, cf, frac)) = target else {
            if m == MapMove::Up
                && from.is_some()
                && let Some(newest) = wals.last().copied()
            {
                self.select_wal(newest);
            }
            return;
        };
        self.select_row(row);
        let shown = self.doc.shown_cf();
        let map = self.doc.map_mut();
        map.set_cf(cf, shown);
        map.set_focus(Some(number), true);
        if let Some((a, b)) = frac {
            map.reveal_x(a, b);
        }
    }

    /// The LSM Map entry's arrival: the map with its focused SST selected (or a note when it is
    /// not live). (Back's snapshot is restored in `arrive`, `Arrival::Restore`.)
    fn apply_map_entry(&mut self, snap: MapSnapshot) {
        self.doc.map_mut().restore(&snap);
        // A new entry: an overflowing map opens fitted (0030 A1-9).
        self.doc.map_mut().fit_on_next_overflow();
        // The LSM Map entry: arrows / Enter work at once (keyboard path, UI advice §6.4).
        self.doc.map_mut().request_focus();
        let Some(n) = snap.focus else { return };
        let hit = self.doc.view().and_then(|v| v.lsm.by_number(n)).map(|(c, l, f)| (c.id, f.row, lsm_map::fraction(l, f)));
        match hit {
            Some((cf, row, (a, b))) => {
                self.select_row(row);
                let shown = self.doc.shown_cf();
                let map = self.doc.map_mut();
                map.set_cf(cf, shown);
                map.set_focus(Some(n), true);
                map.reveal_x(a, b);
            }
            None => {
                let map = self.doc.map_mut();
                map.set_focus(None, false);
                map.set_note(Some(format!("{n:06}.sst is not a live file in this MANIFEST")));
            }
        }
    }

    /// The current file, selection, byte-view position, Go-to text and (in the levels view) the
    /// map state: what Back restores.
    fn here(&self) -> Here {
        let hex = self.doc.hex();
        let at = self.sel.span().or(self.sel.shown()).filter(|s| s.space == hex.space()).map(|s| s.offset).unwrap_or(hex.page_base());
        let map = (self.doc.view_kind() == Some(&NodeKind::ManifestLevels)).then(|| self.doc.map().snapshot());
        Here {
            file: self.doc.file().map(|f| f.to_path_buf()).unwrap_or_default(),
            sel: self.sel.snapshot(),
            hex_space: hex.space(),
            hex_at: at,
            goto: hex.goto().to_string(),
            map,
            table_scroll: self.doc.table_scroll(),
        }
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
        if self.jobs.finish_wal_scan() {
            self.doc.wal_scan_ended(NotScannedWhy::Cancelled);
        }
        self.find.cancel();
        self.doc.hex_mut().cancel();
        if copy {
            self.copy_notice(clipboard::COPY_CANCELLED);
        }
        if scan {
            self.doc.scan_cancelled();
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
                    Outcome::Cancelled => self.doc.scan_cancelled(),
                }
            }
            _ if !current => return None, // stale result from a previous file/folder
            Response::Opened { label, children, file_len, mtime } => self.doc.opened(label, children, file_len, mtime),
            Response::OpenFailed(e) => self.doc.open_failed(e),
            Response::Children { node, children, error } => self.doc.children_arrived(node as usize, children, error),
            // A node view without a view is a file that did not open (no inspector): for a Back
            // the popped entry is consumed and the status says why (reviewer nit 8).
            Response::View { req, kind: None, target: ViewTarget::Node(_), .. } if self.navigation().pending_target().is_some() && self.doc.on_view_reply_is_ours(req) => {
                let back = self.navigation().back_in_flight();
                if self.doc.on_view_reply(req, false, self.sel.epoch()) != ViewReply::NothingLocated {
                    return None; // overtaken by a newer choice: the history was restored
                }
                let why = self.doc.path_error().unwrap_or("the file could not be opened").to_string();
                let text = if back { format!("Previous view unavailable: {why}") } else { format!("View unavailable: {why}") };
                self.doc.set_status_msg(text.clone());
                self.view_prefs.notify(NoticeOwner::Nav, text, std::time::Instant::now(), Some(COPY_NOTICE_SECS));
            }
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
            // Generation-guarded (above) and matched by the job in flight; one reply per WAL.
            Response::WalScanned { req, path, outcome } => {
                if !self.jobs.wal_scan_matches(req) {
                    return None;
                }
                match (path, outcome) {
                    (Some(p), Outcome::Done(s)) => self.doc.wal_scanned(&p, s),
                    (Some(p), Outcome::Failed(e)) => self.doc.wal_scanned(&p, WalSummary::Unreadable(e)),
                    (_, Outcome::Cancelled) => self.doc.wal_scan_ended(NotScannedWhy::Cancelled),
                    (None, Outcome::Failed(e)) => self.doc.wal_scan_ended(NotScannedWhy::Failed(e)),
                    (None, Outcome::Done(_)) => {}
                }
                if !self.doc.wal_scan_pending() {
                    // Normally the job has answered its last file; if the inventory was
                    // replaced, the job is orphaned: cancel it before starting another (N8).
                    self.jobs.abandon_wal_scan();
                    // Files that appeared in a re-shown inventory meanwhile (bounded by the cap).
                    self.start_wal_summaries(false);
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
        if kind == NodeKind::ManifestLevels {
            self.start_wal_summaries(true);
        }
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
        // A levels view that carries no map state (tree node, navigation) opens as the table;
        // the LSM Map entry and Back carry theirs (0024 D2, architect should-fix 5).
        let carries_map = pending.as_ref().is_some_and(|p| p.map.is_some() || p.restore.as_ref().is_some_and(|h| h.map.is_some()));
        if kind == NodeKind::ManifestLevels && !carries_map {
            self.doc.map_mut().set_mode(MapMode::Table);
        }
        match pending {
            Some(p) => {
                // The LSM Map entry's map state (Back's snapshot is restored in `arrive`).
                let entry = p.map.clone();
                if let Some(v) = self.doc.view() {
                    let a = plan_arrival(*p, select, v, extent);
                    self.arrive(a);
                }
                if let Some(m) = entry {
                    self.apply_map_entry(m);
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
            Arrival::Restore(h) => {
                let Here { sel, hex_space: space, hex_at: at, goto, table_scroll, map, .. } = *h;
                // Back: the exact previous selection, in the byte space and position it had, and
                // the Go-to text (design B7) — restored only with the byte view it describes.
                self.sel.restore(sel);
                // The map mode, CF, pan, zoom and focus of a levels view, as they were.
                if let Some(m) = map {
                    self.doc.map_mut().restore(&m);
                    // 0030 A2-7: Back to the map gives it keyboard focus (arrows work at once).
                    if m.mode == MapMode::Map {
                        self.doc.map_mut().request_focus();
                    }
                }
                // The table's scroll offset as it was (0024 D1); the row request is the fallback.
                self.doc.restore_table_scroll(table_scroll);
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
        // A levels row is also the map's focused box (table and map share the selection).
        if let Some((cf, n)) = self.doc.view().and_then(|v| v.lsm.by_row(i)).map(|(c, _, f)| (c.id, f.number)) {
            let shown = self.doc.shown_cf();
            let map = self.doc.map_mut();
            map.set_cf(cf, shown);
            map.set_focus(Some(n), false);
            map.set_pick(None);
        }
    }

    /// A WAL of the band (0026 item 4): the selection is the WAL (no levels row, no SST focus),
    /// so the Explanation describes it. Only a WAL of the inventory, only on the levels view.
    fn select_wal(&mut self, n: u64) {
        self.select_pick(MapPick::Wal(n));
    }

    /// A map object that is not a row (a WAL, the older-WAL summary, a level title; 0026 §1.7,
    /// §4.7, §4.9): the levels row selection and the SST focus are cleared, the Explanation shows
    /// the object's card. Only objects that exist, only on the levels view.
    fn select_pick(&mut self, p: MapPick) {
        if self.doc.view_kind() != Some(&NodeKind::ManifestLevels) {
            return;
        }
        let exists = match p {
            MapPick::Wal(n) => self.doc.wal_inventory().and_then(|w| w.by_number(n)).is_some(),
            MapPick::OlderWals(n) => self.doc.wal_inventory().and_then(|w| w.by_number(n)).is_some(),
            MapPick::Level(l) => self.doc.view().is_some_and(|v| self.doc.map().shown_cf(&v.lsm).and_then(|c| v.lsm.cf(c)).is_some_and(|c| c.levels.iter().any(|x| x.level == l))),
        };
        if !exists {
            return;
        }
        self.sel.clear();
        let map = self.doc.map_mut();
        map.set_focus(None, false);
        map.set_pick(Some(p));
    }

    /// Starts the bounded background summaries of the levels view's WAL files (one job at a
    /// time; never on the UI thread).
    /// `again`: the map is shown again (cancelled / refused files are requested once more).
    fn start_wal_summaries(&mut self, again: bool) {
        if self.jobs.wal_scanning() {
            return;
        }
        let files = self.doc.plan_wal_scan(again);
        if !files.is_empty() {
            self.jobs.wal_summaries(self.doc.generation(), files);
        }
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
            self.switch_file(p);
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
