//! Document session: the open folder (listing) and file, the file generation, the lazy tree,
//! the current view and the view requested, navigation history, the hex page state, the
//! change-on-disk stat result and the path/go-to inputs. Owns the generation: closing or
//! switching the file bumps it, and replies of an older generation are dropped by `AppState`.
//! Mutators are visible to `app` only.

use super::hex::HexState;
use super::jobs::ViewTarget;
use super::selection::SelSnapshot;
use crate::discover::{DirEntry, FileKind};
use crate::limits::LIMITS;
use crate::model::*;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

pub const FILE_SPACE_TITLE: &str = "File offsets (physical bytes as stored)";

/// One history step: the view, its selection and the byte-view position (space + the offset
/// shown), restored together by Back.
#[derive(Clone, Debug, PartialEq)]
pub struct HistEntry {
    pub kind: NodeKind,
    pub sel: SelSnapshot,
    pub hex_space: Space,
    pub hex_at: u64,
}

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
}

impl TreeNode {
    /// No status glyph: a group none of whose members was checked yet.
    pub fn unchecked(&self) -> bool {
        matches!(self.checked, Some((0, _)))
    }
    /// Label with the group coverage suffix (" · not checked" / " · 3 of 64 checked").
    pub fn full_label(&self) -> String {
        format!("{}{}", self.label, check_suffix(self.checked))
    }
    /// The one glyph a node shows: the worse of its own status and a problem inside it.
    pub fn shown_status(&self) -> Status {
        match self.contains {
            Some(c) if rank(c) > rank(self.status) => c,
            _ => self.status,
        }
    }
}

/// Severity order for the single tree glyph (higher is worse).
fn rank(s: Status) -> u8 {
    match s {
        Status::Present | Status::Implicit | Status::NotPresent => 0,
        Status::Unknown | Status::Unsupported | Status::Limited => 1,
        Status::Truncated => 2,
        Status::Invalid => 3,
    }
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
    /// View request in flight and its job id (replies are matched by id).
    pending_view: Option<(ViewTarget, u64)>,
    history: Vec<HistEntry>,
    /// One-shot request for the tree to scroll this node into view (a reveal).
    tree_scroll: Option<usize>,
    hex: HexState,
    goto: String,
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
            pending_view: None,
            history: Vec::new(),
            tree_scroll: None,
            hex: HexState::default(),
            goto: String::new(),
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
    pub fn pending_view(&self) -> Option<&ViewTarget> {
        self.pending_view.as_ref().map(|p| &p.0)
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
        if forward {
            list[pos + 1..].iter().find(|x| ok(x)).copied()
        } else {
            list[..pos].iter().rev().find(|x| ok(x)).copied()
        }
    }
    pub fn can_go_back(&self) -> bool {
        !self.history.is_empty()
    }
    pub fn hex(&self) -> &HexState {
        &self.hex
    }
    pub fn goto(&self) -> &str {
        &self.goto
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
    /// Title and size of an offset space (the file, or a derived space of the current view).
    pub fn space_title(&self, space: Space) -> (String, u64) {
        if space == Space::File {
            return (FILE_SPACE_TITLE.into(), self.file_len);
        }
        if let Some(v) = &self.view {
            if let Some(s) = v.spaces.iter().find(|s| s.space == space) {
                return (s.title.clone(), s.size);
            }
        }
        (space.label(), self.hex.size)
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
    pub(super) fn set_goto(&mut self, s: String) {
        self.goto = s;
    }
    pub(super) fn set_dir_note(&mut self, s: impl Into<String>) {
        self.dir_note = s.into();
    }
    pub(super) fn mark_stat_started(&mut self) {
        self.last_stat = Instant::now();
    }

    /// Ends the file session: new generation, no file/tree/view/history/hex page.
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
        self.pending_view = None;
        self.history.clear();
        self.tree_scroll = None;
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
    }

    pub(super) fn is_dir(&self, d: &Path) -> bool {
        self.dir.as_deref() == Some(d)
    }

    /// Folder listing arrived. Returns the file to auto-open (CURRENT, else MANIFEST) if asked.
    pub(super) fn scanned(&mut self, entries: Vec<DirEntry>, trunc: bool, auto_open: bool) -> Option<PathBuf> {
        let n = |k: FileKind| entries.iter().filter(|e| e.kind == k).count();
        self.dir_note = format!(
            "{} WAL · {} SST · {} MANIFEST · CURRENT {}{}",
            n(FileKind::Wal),
            n(FileKind::Sst),
            n(FileKind::Manifest),
            if n(FileKind::Current) > 0 { "yes" } else { "no" },
            if trunc { " · listing truncated" } else { "" }
        );
        self.dir_entries = entries;
        if auto_open && self.file.is_none() {
            return self.dir_entries.iter().find(|e| e.kind == FileKind::Current).or_else(|| self.dir_entries.iter().find(|e| e.kind == FileKind::Manifest)).map(|e| e.path.clone());
        }
        None
    }

    pub(super) fn scan_failed(&mut self, e: String) {
        self.dir_note.clear();
        self.path_error = Some(format!("cannot list folder: {e}"));
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
        self.nodes.push(TreeNode { label: c.label, kind: c.kind, status: c.status, leaf: c.leaf, depth, children: None, expanded: false, loading: false, diag: c.diag, contains: c.contains, role: c.role, checked: c.checked });
        self.nodes.len() - 1
    }

    /// A group's status from its loaded members (gaps are not members): the worst checked
    /// status, Present only when every member was checked and Present; then its parent groups.
    fn aggregate_group(&mut self, node: usize) {
        let mut cur = Some(node);
        while let Some(i) = cur {
            if self.nodes[i].checked.is_none() {
                return;
            }
            let Some(kids) = self.nodes[i].children.clone() else { return };
            let members: Vec<usize> = kids.into_iter().filter(|&k| !matches!(self.nodes[k].kind, NodeKind::SstGap { .. })).collect();
            let checked: Vec<Status> = members.iter().filter(|&&k| !self.nodes[k].unchecked() && !(self.nodes[k].checked.is_some_and(|(x, y)| x < y))).map(|&k| self.nodes[k].shown_status()).collect();
            let worst = checked.iter().copied().max_by_key(|s| rank(*s)).unwrap_or(Status::Unknown);
            let n = members.len() as u64;
            let x = checked.len() as u64;
            self.nodes[i].checked = Some((x, n));
            self.nodes[i].status = if x == 0 || (x < n && rank(worst) == 0) { Status::Unknown } else { worst };
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

    #[cfg(test)]
    pub(super) fn push_test_node(&mut self, n: TreeNode) {
        self.nodes.push(n);
    }
    #[cfg(test)]
    pub(super) fn set_test_file(&mut self, p: PathBuf, stat: (u64, Option<SystemTime>)) {
        self.file = Some(p);
        self.file_stat = Some(stat);
    }

    /// Records the view request `req`; with `history` (the current selection and byte-view
    /// position) the current view is pushed on the history so Back restores all of it.
    pub(super) fn request_view(&mut self, t: ViewTarget, req: u64, history: Option<(SelSnapshot, Space, u64)>) {
        if let (Some((sel, hex_space, hex_at)), Some(kind)) = (history, self.view_kind.clone()) {
            let e = HistEntry { kind, sel, hex_space, hex_at };
            if self.history.last() != Some(&e) {
                self.history.push(e);
            }
            if self.history.len() > LIMITS.history {
                self.history.remove(0);
            }
        }
        self.pending_view = Some((t, req));
    }

    pub(super) fn pop_history(&mut self) -> Option<HistEntry> {
        self.history.pop()
    }

    /// Applies a view reply if it answers the view request in flight (by job id). Returns true
    /// if it was applied.
    pub(super) fn accept_view(&mut self, req: u64, kind: NodeKind, view: View) -> bool {
        if self.pending_view.as_ref().map(|p| p.1) != Some(req) {
            return false;
        }
        self.pending_view = None;
        self.view_kind = Some(kind);
        self.view = Some(view);
        true
    }

    /// The located byte had no structure: the request is answered without a new view.
    pub(super) fn drop_pending(&mut self, req: u64) -> bool {
        if self.pending_view.as_ref().map(|p| p.1) != Some(req) {
            return false;
        }
        self.pending_view = None;
        true
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
        if let Some(n) = self.sel_node.and_then(|i| self.nodes.get_mut(i)) {
            if n.expanded {
                n.expanded = false;
            }
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
