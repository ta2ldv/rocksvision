//! Document session: the open folder (listing) and file, the file generation, the lazy tree,
//! the current view and the view requested, navigation history, the hex page state, the
//! change-on-disk stat result and the path/go-to inputs. Owns the generation: closing or
//! switching the file bumps it, and replies of an older generation are dropped by `AppState`.
//! Mutators are visible to `app` only.

use super::hex::HexState;
use crate::discover::{DirEntry, FileKind};
use crate::limits::LIMITS;
use crate::model::*;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

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
    pending_view: Option<NodeKind>,
    history: Vec<NodeKind>,
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
    pub fn pending_view(&self) -> Option<&NodeKind> {
        self.pending_view.as_ref()
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
    /// Smallest row of the current view whose span contains the offset.
    pub(super) fn row_at(&self, space: Space, off: u64) -> Option<(usize, Option<Span>)> {
        let v = self.view.as_ref()?;
        v.rows.iter().enumerate().filter(|(_, r)| r.field.span.map(|s| s.contains(space, off)).unwrap_or(false)).min_by_key(|(_, r)| r.field.span.map(|s| s.len).unwrap_or(u64::MAX)).map(|(i, r)| (i, r.field.span))
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
        }
    }

    fn add_node(&mut self, c: Child, depth: usize) -> usize {
        self.nodes.push(TreeNode { label: c.label, kind: c.kind, status: c.status, leaf: c.leaf, depth, children: None, expanded: false, loading: false, diag: c.diag, contains: c.contains });
        self.nodes.len() - 1
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

    /// Records the view request (and pushes the current view on the history if asked).
    pub(super) fn request_view(&mut self, k: NodeKind, push_history: bool) {
        if push_history {
            if let Some(cur) = self.view_kind.clone() {
                self.history.push(cur);
                if self.history.len() > LIMITS.history {
                    self.history.remove(0);
                }
            }
        }
        self.pending_view = Some(k);
    }

    pub(super) fn pop_history(&mut self) -> Option<NodeKind> {
        self.history.pop()
    }

    /// Applies a view reply if it answers the view requested. Returns true if it was applied.
    pub(super) fn accept_view(&mut self, kind: NodeKind, view: View) -> bool {
        if self.pending_view.as_ref() != Some(&kind) {
            return false;
        }
        self.pending_view = None;
        self.view_kind = Some(kind);
        self.view = Some(view);
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
