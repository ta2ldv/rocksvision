//! Document session: the open folder (listing) and file, the file generation, the lazy tree,
//! the current view, the navigation (history + the view request in flight, `Navigation`), the
//! byte view (`HexState`, incl. the Go-to text), the change-on-disk stat result and the path
//! input. Owns the generation: closing or switching the file bumps it and resets all of the
//! above together; replies of an older generation are dropped by `AppState`. Mutators are
//! visible to `app` only.

use super::hex::HexState;
use super::jobs::ViewTarget;
use super::lsm_map::{self, MapEntry, MapMode, MapState, Unavailable};
use super::navigation::{HistEntry, HistoryOp, Navigation, PendingSel, ViewReply};
use super::view_prefs::SplitRole;
use crate::discover::{DirEntry, FileKind};
use crate::limits::{LIMITS, WAL_SUMMARY_FILES};
use crate::model::*;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

pub use super::navigation::Here;

pub const FILE_SPACE_TITLE: &str = "File offsets (physical bytes as stored)";

#[derive(Clone, Debug)]
pub struct TreeNode {
    pub label: String,
    pub kind: NodeKind,
    pub status: Status,
    pub leaf: bool,
    pub depth: usize,
    pub children: Option<Vec<usize>>,
    pub expanded: bool,
    pub loading: bool,
    pub diag: Option<Box<Diagnostic>>,
    /// Problem status inside the node that `status` does not show (`Child::contains`).
    pub contains: Option<Status>,
    /// Structure at its canonical place, labeled view, reference (link) or "not present" line.
    pub role: ChildRole,
    /// Group check coverage (`Child::checked`); updated from the members when they load.
    pub checked: Option<(u64, u64)>,
    /// Table view node: status counts of its rows (`Child::tally`), refreshed whenever its
    /// view is shown (0019 §1).
    pub tally: Option<RowTally>,
}

impl TreeNode {
    /// No status glyph (design A3): a group none of whose members was checked yet, or whose
    /// checked members are clean while not all were checked; a table view node while one of
    /// its rows is not checked and none failed.
    pub fn unchecked(&self) -> bool {
        match &self.tally {
            Some(t) => t.glyph().is_none(),
            None => group_glyph(self.status, self.checked).is_none(),
        }
    }
    /// Label with the group coverage after its head ("Data blocks #0–#63 · not checked ·
    /// [0x0,0x3f780) · 64 blocks"); the same string as the dump and the card title.
    pub fn full_label(&self) -> String {
        with_coverage(&self.label, self.checked)
    }
    /// Length (bytes) of the label head that is never truncated: up to and including the
    /// coverage of a group (design A5); 0 for other nodes.
    pub fn head_len(&self) -> usize {
        match (coverage_text(self.checked), self.label.split_once(" · ")) {
            (Some(cov), Some((head, _))) => head.len() + " · ".len() + cov.len(),
            _ => 0,
        }
    }
    /// The one glyph a node shows: the worse of its own status and a problem inside it.
    pub fn shown_status(&self) -> Status {
        match self.contains {
            Some(c) if rank(c) > rank(self.status) => c,
            _ => self.status,
        }
    }
}

/// The folder line under the Navigation title: counts and the explicit layout status (0024
/// §1). A standard DB and a checkpoint look alike, so no provenance is claimed; a missing WAL
/// is neutral.
pub fn folder_note(entries: &[DirEntry], trunc: bool, blocked: Option<&str>) -> String {
    let n = |k: FileKind| entries.iter().filter(|e| e.kind == k).count();
    let (cur, man, wal) = (n(FileKind::Current), n(FileKind::Manifest), n(FileKind::Wal));
    let layout = match (cur > 0, man > 0) {
        // Parsed and failed (MANIFEST missing / CURRENT invalid): say so (D2b finding 5).
        (true, _) if blocked.is_some() => format!("CURRENT present · live SST files unknown: {}", blocked.unwrap_or_default()),
        // Not parsed yet: the levels view says which MANIFEST CURRENT names and whether it exists.
        (true, _) => "CURRENT present".to_string(),
        (false, true) => "CURRENT absent: MANIFEST opened directly (not confirmed as the active one)".to_string(),
        (false, false) => "no CURRENT or MANIFEST: no live-file layout".to_string(),
    };
    format!("{layout} · {} SST · {man} MANIFEST · {}{}", n(FileKind::Sst), crate::model::wal_words(wal), if trunc { " · listing truncated" } else { "" })
}

/// `NNNNNN.sst` (or `.ldb`) → file number (the SST the LSM Map entry focuses).
pub fn sst_number(p: &Path) -> Option<u64> {
    let name = p.file_name()?.to_str()?;
    name.strip_suffix(".sst").or_else(|| name.strip_suffix(".ldb"))?.parse().ok()
}

pub struct DocumentSession {
    generation: u64,
    path_input: String,
    path_error: Option<String>,
    dir: Option<PathBuf>,
    dir_entries: Vec<DirEntry>,
    dir_note: String,
    file: Option<PathBuf>,
    file_label: String,
    /// Open requested, decoder answer not yet applied.
    opening: bool,
    file_len: u64,
    /// Size/mtime observed when the decoder opened the file.
    file_stat: Option<(u64, Option<SystemTime>)>,
    changed: bool,
    last_stat: Instant,
    nodes: Vec<TreeNode>,
    roots: Vec<usize>,
    sel_node: Option<usize>,
    view: Option<View>,
    view_kind: Option<NodeKind>,
    /// History and the view request in flight (0020 §1); survives file switches (0024 §4).
    nav: Navigation,
    /// Table/Map mode, CF, pan, map zoom and focus of the Observed levels view.
    map: MapState,
    /// WAL files of the levels view's folder with their background summaries (0026 item 4);
    /// taken over from the levels view, reset with the file like the map.
    wals: Option<WalInventory>,
    /// Records table vertical scroll offset of the view shown (points; reported by the records
    /// panel), and a one-shot offset to restore (Back).
    table_scroll: f32,
    table_restore: Option<f32>,
    /// Why this file's live SSTs cannot be observed (`View::lsm_blocked` of a view shown).
    lsm_blocked: Option<String>,
    /// The folder listing of `dir` has arrived (or failed), and whether it was truncated.
    listed: bool,
    dir_trunc: bool,
    /// One-shot request for the tree to scroll this node into view (a reveal).
    tree_scroll: Option<usize>,
    hex: HexState,
    status_msg: String,
}

impl DocumentSession {
    pub(super) fn new() -> Self {
        DocumentSession {
            generation: 0,
            path_input: String::new(),
            path_error: None,
            dir: None,
            dir_entries: Vec::new(),
            dir_note: String::new(),
            file: None,
            file_label: String::new(),
            opening: false,
            file_len: 0,
            file_stat: None,
            changed: false,
            last_stat: Instant::now(),
            nodes: Vec::new(),
            roots: Vec::new(),
            sel_node: None,
            view: None,
            view_kind: None,
            nav: Navigation::default(),
            map: MapState::default(),
            wals: None,
            table_scroll: 0.0,
            table_restore: None,
            lsm_blocked: None,
            listed: false,
            dir_trunc: false,
            tree_scroll: None,
            hex: HexState::default(),
            status_msg: "Open a RocksDB data folder or a single WAL / SST / MANIFEST / CURRENT file.".into(),
        }
    }

    // ---------------- read-only views ----------------

    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn path_input(&self) -> &str {
        &self.path_input
    }
    pub fn path_error(&self) -> Option<&str> {
        self.path_error.as_deref()
    }
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }
    pub fn dir_entries(&self) -> &[DirEntry] {
        &self.dir_entries
    }
    pub fn dir_note(&self) -> &str {
        &self.dir_note
    }
    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }
    pub fn file_label(&self) -> &str {
        &self.file_label
    }
    /// The open request was sent and the decoder has not answered yet.
    pub fn is_opening(&self) -> bool {
        self.opening
    }
    pub fn file_len(&self) -> u64 {
        self.file_len
    }
    pub fn changed(&self) -> bool {
        self.changed
    }
    pub fn nodes(&self) -> &[TreeNode] {
        &self.nodes
    }
    pub fn roots(&self) -> &[usize] {
        &self.roots
    }
    pub fn sel_node(&self) -> Option<usize> {
        self.sel_node
    }
    pub fn view(&self) -> Option<&View> {
        self.view.as_ref()
    }
    pub fn view_kind(&self) -> Option<&NodeKind> {
        self.view_kind.as_ref()
    }
    /// History and the view request in flight.
    pub fn navigation(&self) -> &Navigation {
        &self.nav
    }
    /// The LSM Map state of the levels view.
    pub fn map(&self) -> &MapState {
        &self.map
    }
    /// WAL files of the levels view's folder and their summaries (None: no levels view shown
    /// for this file, or its folder was not listed).
    pub fn wal_inventory(&self) -> Option<&WalInventory> {
        self.wals.as_ref()
    }
    /// A WAL summary is still Scanning.
    pub fn wal_scan_pending(&self) -> bool {
        self.wals.as_ref().is_some_and(|w| w.files.iter().any(|f| f.summary == WalSummary::Scanning))
    }
    /// Records table scroll offset (points) of the view shown.
    pub fn table_scroll(&self) -> f32 {
        self.table_scroll
    }
    /// The Observed levels view is shown in Map mode.
    pub fn map_shown(&self) -> bool {
        self.map.mode() == MapMode::Map && self.view_kind == Some(NodeKind::ManifestLevels)
    }
    /// The CF the map shows for the view shown (`MapState::shown_cf`), if any.
    pub fn shown_cf(&self) -> Option<u32> {
        self.view.as_ref().and_then(|v| self.map.shown_cf(&v.lsm))
    }
    /// The split role of the view shown (0030 A8): the levels view has its own Table and Map
    /// splits; every other view shares the records split.
    pub fn split_role(&self) -> SplitRole {
        match (self.view_kind == Some(NodeKind::ManifestLevels), self.map.mode()) {
            (true, MapMode::Map) => SplitRole::Map,
            (true, MapMode::Table) => SplitRole::LevelsTable,
            (false, _) => SplitRole::Records,
        }
    }
    /// The view requested and not answered yet (`Navigation::pending_target`; kept here for the
    /// integration tests in tests/*.rs).
    pub fn pending_view(&self) -> Option<&ViewTarget> {
        self.nav.pending_target()
    }
    /// Index of a loaded tree node of this kind (the first one at its canonical place).
    pub fn node_of(&self, k: &NodeKind) -> Option<usize> {
        self.nodes.iter().position(|n| &n.kind == k && n.role != ChildRole::Reference)
    }
    /// Sibling of tree node `i` in tree order (`forward` = next), skipping "not present" lines.
    pub fn sibling(&self, i: usize, forward: bool) -> Option<usize> {
        let list: &[usize] = match self.nodes.iter().position(|n| n.children.as_deref().is_some_and(|c| c.contains(&i))) {
            Some(p) => self.nodes[p].children.as_deref().unwrap_or(&[]),
            None => &self.roots,
        };
        let pos = list.iter().position(|&x| x == i)?;
        let ok = |x: &usize| self.nodes[*x].role != ChildRole::Absent;
        if forward { list[pos + 1..].iter().find(|x| ok(x)).copied() } else { list[..pos].iter().rev().find(|x| ok(x)).copied() }
    }
    pub fn hex(&self) -> &HexState {
        &self.hex
    }
    pub fn status_msg(&self) -> &str {
        &self.status_msg
    }
    /// The field of table row `row` of the current view.
    pub fn row_field(&self, row: Option<usize>) -> Option<&Field> {
        self.view.as_ref().and_then(|v| row.and_then(|i| v.rows.get(i))).map(|r| &r.field)
    }
    pub(super) fn row(&self, row: Option<usize>) -> Option<&Row> {
        self.view.as_ref().and_then(|v| row.and_then(|i| v.rows.get(i)))
    }
    /// The current view describes offset space `space` (the file always; a derived space only
    /// while its view is shown): the byte view may stay in it / be restored to it.
    pub fn describes_space(&self, space: Space) -> bool {
        space == Space::File || self.view.as_ref().is_some_and(|v| v.spaces.iter().any(|s| s.space == space))
    }
    /// Title and size of an offset space (the file, or a derived space of the current view).
    pub fn space_title(&self, space: Space) -> (String, u64) {
        if space == Space::File {
            return (FILE_SPACE_TITLE.into(), self.file_len);
        }
        if let Some(v) = &self.view
            && let Some(s) = v.spaces.iter().find(|s| s.space == space)
        {
            return (s.title.clone(), s.size);
        }
        (space.label(), self.hex.size())
    }
    /// A change-on-disk check is due (interval elapsed, file opened, no check in flight).
    pub(super) fn stat_due(&self, stat_pending: bool) -> bool {
        self.file.is_some() && !stat_pending && self.file_stat.is_some() && self.last_stat.elapsed() > std::time::Duration::from_secs(LIMITS.change_check_secs)
    }
    /// A file is open and its change-on-disk check is waiting for the next interval.
    pub(super) fn watching(&self, stat_pending: bool) -> bool {
        !stat_pending && self.file_stat.is_some()
    }

    // ---------------- mutation (app only) ----------------

    pub(super) fn hex_mut(&mut self) -> &mut HexState {
        &mut self.hex
    }
    pub(super) fn set_path_input(&mut self, s: String) {
        self.path_input = s;
    }
    /// Path typed or opened: shown in the path field, previous error cleared.
    pub(super) fn set_path(&mut self, p: &Path) {
        self.path_input = p.display().to_string();
        self.path_error = None;
    }
    pub(super) fn set_path_error(&mut self, e: String) {
        self.path_error = Some(e);
    }
    pub(super) fn set_status_msg(&mut self, s: impl Into<String>) {
        self.status_msg = s.into();
    }
    pub(super) fn mark_stat_started(&mut self) {
        self.last_stat = Instant::now();
    }

    /// Ends the file session: new generation, no file/tree/view/hex page, nothing in flight. The
    /// history survives (Back may return to the previous file); `reset_history` ends it.
    pub(super) fn close(&mut self) {
        self.generation += 1;
        self.file = None;
        self.opening = false;
        self.file_len = 0;
        self.file_stat = None;
        self.changed = false;
        self.nodes.clear();
        self.roots.clear();
        self.sel_node = None;
        self.view = None;
        self.view_kind = None;
        self.nav.drop_inflight();
        self.map = MapState::default();
        self.wals = None;
        self.table_scroll = 0.0;
        self.table_restore = None;
        self.lsm_blocked = None;
        self.tree_scroll = None;
        // A new file starts at page 0 with the Go-to placeholder (design B3).
        self.hex = HexState::default();
    }

    /// File chosen (after `close`): label "opening…" until the decoder answers.
    pub(super) fn begin_open(&mut self, p: PathBuf) {
        self.set_path(&p);
        self.file = Some(p);
        self.file_label = "opening…".into();
        self.opening = true;
        self.hex.set_space(Space::File, 0, FILE_SPACE_TITLE.into());
    }

    pub(super) fn begin_scan(&mut self, dir: PathBuf) {
        self.dir = Some(dir);
        self.dir_entries.clear();
        self.dir_note = "Scanning folder…".into();
        self.listed = false;
    }

    pub(super) fn is_dir(&self, d: &Path) -> bool {
        self.dir.as_deref() == Some(d)
    }

    /// Folder listing arrived. Returns the file to auto-open (CURRENT, else MANIFEST) if asked.
    pub(super) fn scanned(&mut self, entries: Vec<DirEntry>, trunc: bool, auto_open: bool) -> Option<PathBuf> {
        self.dir_trunc = trunc;
        self.dir_note = folder_note(&entries, trunc, self.blocked_here());
        self.dir_entries = entries;
        self.listed = true;
        if auto_open && self.file.is_none() {
            return self.dir_entries.iter().find(|e| e.kind == FileKind::Current).or_else(|| self.dir_entries.iter().find(|e| e.kind == FileKind::Manifest)).map(|e| e.path.clone());
        }
        None
    }

    pub(super) fn scan_failed(&mut self, e: String) {
        self.dir_note.clear();
        self.path_error = Some(format!("cannot list folder: {e}"));
        self.listed = true;
    }
    /// A cancelled folder scan: the listing is final (empty) for availability.
    pub(super) fn scan_cancelled(&mut self) {
        self.dir_note = "Folder scan cancelled".into();
        self.listed = true;
    }

    pub(super) fn opened(&mut self, label: String, children: Vec<Child>, file_len: u64, mtime: Option<SystemTime>) {
        self.file_label = label;
        self.opening = false;
        self.file_len = file_len;
        self.file_stat = Some((file_len, mtime));
        self.roots = children.into_iter().map(|c| self.add_node(c, 0)).collect();
    }

    pub(super) fn open_failed(&mut self, e: String) {
        self.file_label = "error".into();
        self.opening = false;
        self.path_error = Some(format!("cannot decode: {e}"));
        self.status_msg = e;
    }

    pub(super) fn children_arrived(&mut self, node: usize, children: Vec<Child>, error: Option<String>) {
        // A reveal may already have filled this node; its children stay (no duplicate nodes).
        if error.is_none() && self.nodes.get(node).is_some_and(|n| n.children.is_some()) {
            if let Some(n) = self.nodes.get_mut(node) {
                n.loading = false;
            }
            return;
        }
        let depth = self.nodes.get(node).map(|n| n.depth + 1).unwrap_or(0);
        let mut ids: Vec<usize> = children.into_iter().map(|c| self.add_node(c, depth)).collect();
        if let Some(e) = error {
            // A failed expansion is visible (Invalid node + error leaf), never "no children".
            if let Some(k) = self.nodes.get(node).map(|n| n.kind.clone()) {
                ids.push(self.add_node(Child::new(format!("decoding failed: {e}"), k).leaf().status(Status::Invalid), depth));
            }
            if let Some(n) = self.nodes.get_mut(node) {
                n.status = Status::Invalid;
            }
        }
        if let Some(n) = self.nodes.get_mut(node) {
            n.loading = false;
            n.children = Some(ids);
            self.aggregate_group(node);
        }
    }

    fn add_node(&mut self, c: Child, depth: usize) -> usize {
        self.nodes.push(TreeNode {
            label: c.label,
            kind: c.kind,
            status: c.status,
            leaf: c.leaf,
            depth,
            children: None,
            expanded: false,
            loading: false,
            diag: c.diag,
            contains: c.contains,
            role: c.role,
            checked: c.checked,
            tally: c.tally,
        });
        self.nodes.len() - 1
    }

    /// A group's status from its loaded members (gaps are not members): the worst checked
    /// status (Present when the checked ones are clean; the glyph rule is `group_glyph`); then
    /// its parent groups.
    fn aggregate_group(&mut self, node: usize) {
        let mut cur = Some(node);
        while let Some(i) = cur {
            if self.nodes[i].checked.is_none() {
                return;
            }
            let Some(kids) = self.nodes[i].children.clone() else { return };
            let members: Vec<usize> = kids.into_iter().filter(|&k| !matches!(self.nodes[k].kind, NodeKind::SstGap { .. })).collect();
            // Coverage in blocks (0015 C3 UI-6): a listed block is 1 of 1, a sub-group its own
            // (x, y), so the parent reads "64 of 4096 checked" like the table's `group_check`.
            // The worst status is taken over every member that shows a glyph, so a problem
            // inside a partly checked sub-group still shows.
            let (x, n) = members.iter().fold((0u64, 0u64), |(x, n), &k| match self.nodes[k].checked {
                Some((a, b)) => (x + a, n + b),
                None => (x + 1, n + 1),
            });
            let worst = members.iter().filter(|&&k| !self.nodes[k].unchecked()).map(|&k| self.nodes[k].shown_status()).max_by_key(|s| rank(*s)).unwrap_or(Status::Present);
            self.nodes[i].checked = Some((x, n));
            self.nodes[i].status = if rank(worst) > 0 {
                worst
            } else if x == 0 {
                Status::Unknown
            } else {
                Status::Present
            };
            cur = self.nodes.iter().position(|p| p.children.as_deref().is_some_and(|c| c.contains(&i)));
        }
    }

    /// Applies a canonical path: expands each ancestor (adding the children the worker sent
    /// where the node has none yet), selects the target node and asks the tree to scroll to it.
    /// Never touches the row/byte selection. Returns the target node, if found.
    pub(super) fn reveal(&mut self, path: Vec<(NodeKind, Vec<Child>)>, target: &NodeKind) -> Option<usize> {
        if *target == NodeKind::Root {
            self.sel_node = None;
            return None;
        }
        let mut level: Vec<usize> = self.roots.clone();
        for (anc, kids) in path {
            let i = level.iter().copied().find(|&i| self.nodes[i].kind == anc && self.nodes[i].role != ChildRole::Reference)?;
            if self.nodes[i].children.is_none() {
                let depth = self.nodes[i].depth + 1;
                let ids: Vec<usize> = kids.into_iter().map(|c| self.add_node(c, depth)).collect();
                self.nodes[i].children = Some(ids);
                self.aggregate_group(i);
            }
            self.nodes[i].loading = false;
            self.nodes[i].expanded = true;
            level = self.nodes[i].children.clone().unwrap_or_default();
        }
        let t = level.iter().copied().find(|&i| self.nodes[i].kind == *target && self.nodes[i].role != ChildRole::Reference)?;
        self.sel_node = Some(t);
        self.tree_scroll = Some(t);
        Some(t)
    }

    /// Selects an already loaded node of this kind (Back), without loading anything.
    pub(super) fn select_loaded(&mut self, k: &NodeKind) {
        if *k == NodeKind::Root {
            self.sel_node = None;
        } else if let Some(i) = self.node_of(k) {
            self.sel_node = Some(i);
            self.tree_scroll = Some(i);
        }
    }

    /// Selects loaded tree node `i` and asks the tree to scroll to it (structure step).
    pub(super) fn select_and_reveal(&mut self, i: usize) -> Option<NodeKind> {
        let k = self.select_node(Some(i))?;
        self.tree_scroll = Some(i);
        Some(k)
    }

    pub(super) fn take_tree_scroll(&mut self) -> Option<usize> {
        self.tree_scroll.take()
    }

    /// A new root context (folder or file opened by path): Back starts empty.
    pub(super) fn reset_history(&mut self) {
        self.nav.reset();
    }
    /// `req` is the view request in flight.
    pub(super) fn on_view_reply_is_ours(&self, req: u64) -> bool {
        self.nav.is_inflight(req)
    }
    /// Undoes the request in flight before a file switch (a popped Back entry is kept).
    pub(super) fn drop_inflight(&mut self) {
        self.nav.undo_inflight();
    }
    /// A cross-file history entry (the view shown before a file switch).
    pub(super) fn push_history(&mut self, e: HistEntry) {
        self.nav.push(e);
    }
    pub(super) fn map_mut(&mut self) -> &mut MapState {
        &mut self.map
    }

    /// The WAL files to summarise now, highest numbers first, at most `WAL_SUMMARY_FILES`
    /// scanned per inventory (the rest say why); marked Scanning. `again`: the map is shown
    /// again, so files left unscanned by a cancel or a refused job are requested once more
    /// (never a Done / Partial / Corrupt one); otherwise only never-requested files.
    pub(super) fn plan_wal_scan(&mut self, again: bool) -> Vec<PathBuf> {
        let Some(w) = self.wals.as_mut() else { return Vec::new() };
        let used = w.files.iter().filter(|f| !matches!(f.summary, WalSummary::NotScanned(_))).count();
        let want = |s: &WalSummary| if again { s.rescannable() } else { *s == WalSummary::NotScanned(NotScannedWhy::Pending) };
        let mut idle: Vec<usize> = (0..w.files.len()).filter(|&i| want(&w.files[i].summary)).collect();
        idle.sort_by_key(|&i| std::cmp::Reverse(w.files[i].number));
        let room = WAL_SUMMARY_FILES.saturating_sub(used);
        let mut out = Vec::new();
        for (k, i) in idle.into_iter().enumerate() {
            let f = &mut w.files[i];
            if k < room {
                f.summary = WalSummary::Scanning;
                out.push(f.path.clone());
            } else {
                f.summary = WalSummary::NotScanned(NotScannedWhy::BeyondCap);
            }
        }
        out
    }
    /// A WAL summary arrived (applied only to a file still Scanning).
    pub(super) fn wal_scanned(&mut self, path: &Path, s: WalSummary) {
        if let Some(f) = self.wals.as_mut().and_then(|w| w.files.iter_mut().find(|f| f.path == path && f.summary == WalSummary::Scanning)) {
            f.summary = s;
        }
    }
    /// The summary job ended early (cancelled, refused, failed): files still Scanning become
    /// not scanned with the reason.
    pub(super) fn wal_scan_ended(&mut self, why: NotScannedWhy) {
        for f in self.wals.iter_mut().flat_map(|w| w.files.iter_mut()).filter(|f| f.summary == WalSummary::Scanning) {
            f.summary = WalSummary::NotScanned(why.clone());
        }
    }
    /// The records panel reports the table's scroll offset (what Back restores).
    pub(super) fn set_table_scroll(&mut self, y: f32) {
        self.table_scroll = if y.is_finite() { y.max(0.0) } else { 0.0 };
    }
    /// Back: the saved offset is restored once by the records panel.
    pub(super) fn restore_table_scroll(&mut self, y: f32) {
        self.table_scroll = y.max(0.0);
        self.table_restore = Some(y.max(0.0));
    }
    pub(super) fn take_table_restore(&mut self) -> Option<f32> {
        self.table_restore.take()
    }

    /// The open file's kind by name (CURRENT, MANIFEST, SST …).
    fn file_kind(&self) -> Option<FileKind> {
        self.file.as_deref().and_then(|p| p.file_name()).map(|n| crate::discover::classify(&n.to_string_lossy()))
    }

    /// (enabled, active) of the LSM Map entry — the toolbar button and the View menu item share
    /// it: active while the map is shown (clicking returns to the table), else enabled when
    /// available.
    pub fn lsm_map_entry_state(&self) -> (bool, bool) {
        let shown = self.map_shown();
        (shown || self.lsm_map_availability().is_ok(), shown)
    }

    /// The one availability rule of the LSM Map (toolbar button, menu item, key, Explanation
    /// line): where the entry leads, or why it is unavailable. No I/O: the open file's kind, the
    /// folder listing and the views already shown.
    pub fn lsm_map_availability(&self) -> Result<MapEntry, Unavailable> {
        // A view of this file said why its live files cannot be observed (typed, `lsm_blocked`).
        if let Some(why) = &self.lsm_blocked {
            return Err(Unavailable(why.clone()));
        }
        if self.view_kind == Some(NodeKind::ManifestLevels) && self.view.is_some() {
            return Ok(MapEntry::Shown);
        }
        let kind = self.file_kind();
        if matches!(kind, Some(FileKind::Current | FileKind::Manifest)) {
            return Ok(MapEntry::InFile);
        }
        // The folder of the open file (or the folder alone).
        let same_dir = match (self.file.as_deref().and_then(|f| f.parent()), self.dir.as_deref()) {
            (Some(p), Some(d)) => p == d,
            (None, Some(_)) => true,
            _ => false,
        };
        let entries: &[DirEntry] = if same_dir { &self.dir_entries } else { &[] };
        let via = entries.iter().find(|e| e.kind == FileKind::Current).or_else(|| entries.iter().find(|e| e.kind == FileKind::Manifest));
        let name = self.file.as_deref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
        match (via, kind) {
            // The file's folder is being listed (OpenFile / OpenPath of a file scans its parent).
            (None, Some(FileKind::Sst)) if same_dir && !self.listed => Err(Unavailable(lsm_map::LISTING.into())),
            (Some(e), _) => Ok(MapEntry::Via { path: e.path.clone(), focus: self.file.as_deref().and_then(sst_number), from: name.filter(|_| kind == Some(FileKind::Sst)) }),
            (None, Some(FileKind::Sst)) => Err(Unavailable(lsm_map::lone_sst(name.as_deref().unwrap_or("This SST")))),
            (None, _) if self.dir.is_some() && same_dir => Err(Unavailable(lsm_map::NO_CURRENT.into())),
            _ => Err(Unavailable(lsm_map::NEED_FOLDER.into())),
        }
    }

    #[cfg(test)]
    pub(super) fn push_test_node(&mut self, n: TreeNode) {
        self.nodes.push(n);
    }
    #[cfg(test)]
    pub(super) fn set_test_view(&mut self, kind: NodeKind, view: View) {
        self.view_kind = Some(kind);
        self.view = Some(view);
    }
    #[cfg(test)]
    pub(super) fn set_test_file(&mut self, p: PathBuf, stat: (u64, Option<SystemTime>)) {
        self.file = Some(p);
        self.file_stat = Some(stat);
    }

    /// Records view request `req` with the navigation (history op and the selection to apply on
    /// arrival); the view shown now is the history entry of a push and arms the overtake guard.
    pub(super) fn begin_view(&mut self, req: u64, t: ViewTarget, hist: HistoryOp, pending: Option<PendingSel>, epoch_now: u64) {
        self.nav.begin(req, t, hist, pending, epoch_now, self.view.is_some(), self.view_kind.as_ref());
    }
    /// Back: the previous history entry.
    pub(super) fn back(&mut self) -> Option<HistEntry> {
        self.nav.back()
    }
    /// The guards of a view reply (`Navigation::on_view_reply`).
    pub(super) fn on_view_reply(&mut self, req: u64, located: bool, epoch_now: u64) -> ViewReply {
        self.nav.on_view_reply(req, located, epoch_now)
    }

    /// Installs an accepted view. A table view node follows the counts of the table just shown
    /// (blocks checked since the tree was listed, e.g. one opened and found Invalid before
    /// Back); every view also carries the tallies of the file's other table view nodes (review
    /// S2), so Back to the layout table refreshes "Data blocks via index" too.
    pub(super) fn show_view(&mut self, kind: NodeKind, mut view: View) {
        // The levels view's WAL listing becomes the session's inventory (summaries of the same
        // files are kept, so showing the view again does not rescan them).
        if let Some(mut w) = view.wals.take().filter(|_| kind == NodeKind::ManifestLevels) {
            if let Some(old) = &self.wals {
                w.keep_summaries(old);
            }
            self.wals = Some(w);
        }
        for (k, t) in view.tally.map(|t| (kind.clone(), t)).into_iter().chain(view.tallies.iter().cloned()) {
            for n in self.nodes.iter_mut().filter(|n| n.kind == k && n.role == ChildRole::View) {
                n.tally = Some(t);
                n.status = t.glyph().unwrap_or(Status::Unknown);
            }
        }
        self.view_kind = Some(kind);
        self.view = Some(view);
        // A new table starts at its top (Back restores its own offset after this).
        self.table_scroll = 0.0;
        self.table_restore = None;
        if let Some(b) = self.view.as_ref().and_then(|v| v.lsm_blocked.clone()) {
            self.lsm_blocked = Some(b);
            // The folder note of this file's folder now says why (no "CURRENT present" alone).
            if self.listed && self.blocked_here().is_some() {
                self.dir_note = folder_note(&self.dir_entries, self.dir_trunc, self.blocked_here());
            }
        }
    }

    /// The block reason of the open file when it lies in the listed folder.
    fn blocked_here(&self) -> Option<&str> {
        let here = self.file.as_deref().and_then(|f| f.parent()).is_some_and(|p| self.dir.as_deref() == Some(p));
        self.lsm_blocked.as_deref().filter(|_| here)
    }

    /// Selects a tree node (None = the file root). Returns the node kind to show.
    pub(super) fn select_node(&mut self, i: Option<usize>) -> Option<NodeKind> {
        match i {
            None => {
                self.sel_node = None;
                Some(NodeKind::Root)
            }
            Some(i) if i < self.nodes.len() => {
                self.sel_node = Some(i);
                Some(self.nodes[i].kind.clone())
            }
            Some(_) => None,
        }
    }

    /// Toggles a tree node. Returns the node kind whose children must be requested.
    pub(super) fn toggle(&mut self, i: usize) -> Option<NodeKind> {
        let n = self.nodes.get_mut(i)?;
        n.expanded = !n.expanded;
        if n.expanded && n.children.is_none() && !n.loading {
            n.loading = true;
            return Some(n.kind.clone());
        }
        None
    }

    /// Selected node, if it can be expanded (not a leaf, not expanded).
    pub(super) fn expandable_selected(&self) -> Option<usize> {
        self.sel_node.filter(|&i| !self.nodes[i].leaf && !self.nodes[i].expanded)
    }

    pub(super) fn collapse_selected(&mut self) {
        if let Some(n) = self.sel_node.and_then(|i| self.nodes.get_mut(i))
            && n.expanded
        {
            n.expanded = false;
        }
    }

    /// A physical page answered: the file length it reports is the current one.
    pub(super) fn set_file_len(&mut self, len: u64) {
        self.file_len = len;
    }

    /// Change-on-disk stat answer: `Some(now)` = observed size/mtime, `None` = no information.
    pub(super) fn stat_result(&mut self, now: Option<(u64, Option<SystemTime>)>, failed_counts: bool) {
        self.changed = match (now, self.file_stat) {
            (Some(now), Some(open)) => now != open,
            (None, Some(_)) => failed_counts || self.changed,
            _ => self.changed,
        };
    }
}
