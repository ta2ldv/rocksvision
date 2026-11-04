//! Request identity: the only holder of the worker. Every background request is issued here
//! (the worker assigns the id) and the pending id of each request kind that is matched by id
//! (path resolve, folder scan, change-on-disk stat) lives here, together with the copy gate
//! (`ClipboardState`, the copy in flight) and the cancel tokens. Hex pages record their id in
//! the hex page state, views in `Navigation`, byte searches in `SearchState`; tree children are
//! matched by generation + node identity. Mutators are visible to `app` only.

use super::clipboard::{ClipboardState, PendingCopy};
use super::jobs::{CopyJob, IoRequest, Request, Response, Tagged, Worker};
use crate::model::Space;
use std::path::PathBuf;

pub struct JobState {
    worker: Worker,
    /// Path resolve (file or folder?) in flight.
    resolve: Option<u64>,
    /// Folder scan in flight, and whether to auto-open CURRENT/MANIFEST when it completes.
    scan: Option<(u64, bool)>,
    /// Change-on-disk stat in flight.
    stat: Option<u64>,
    /// The copy in flight and its gate.
    clip: ClipboardState,
}

impl JobState {
    pub(super) fn new(worker: Worker) -> Self {
        JobState { worker, resolve: None, scan: None, stat: None, clip: ClipboardState::default() }
    }

    pub fn resolving(&self) -> bool {
        self.resolve.is_some()
    }
    pub fn scanning(&self) -> bool {
        self.scan.is_some()
    }
    pub fn stat_pending(&self) -> bool {
        self.stat.is_some()
    }
    /// The copy gate.
    pub fn clipboard(&self) -> &ClipboardState {
        &self.clip
    }
    /// The copy in flight (`ClipboardState::pending`; kept here for the integration tests).
    pub fn pending_copy(&self) -> Option<PendingCopy> {
        self.clip.pending()
    }
    /// The decode worker is running a request.
    pub fn decoding(&self) -> bool {
        self.worker.is_busy()
    }
    pub fn io_busy(&self) -> bool {
        self.worker.io_busy()
    }

    // ---------------- issuing ----------------

    /// Queues a decode-worker request (open, view, children, derived bytes); returns its id.
    pub(super) fn decode(&mut self, generation: u64, r: Request) -> u64 {
        self.worker.send(generation, r)
    }
    /// Requests a physical file page; returns its id (recorded by the hex page state).
    pub(super) fn page(&mut self, generation: u64, path: PathBuf, off: u64, len: u64) -> u64 {
        self.worker.io(generation, IoRequest::Page { path, off, len })
    }
    pub(super) fn resolve(&mut self, generation: u64, path: PathBuf) {
        self.resolve = Some(self.worker.io(generation, IoRequest::Resolve(path)));
    }
    /// Starts a folder scan; earlier scans are cancelled.
    pub(super) fn scan(&mut self, generation: u64, dir: PathBuf, auto_open: bool) {
        self.worker.cancel_scans();
        self.scan = Some((self.worker.io(generation, IoRequest::Scan(dir)), auto_open));
    }
    pub(super) fn stat(&mut self, generation: u64, path: PathBuf) {
        self.stat = Some(self.worker.io(generation, IoRequest::Stat(path)));
    }
    /// Copy through the I/O pool for selection epoch `sel` (text or physical bytes); the gate
    /// records it as the copy in flight. Returns the request id.
    pub(super) fn copy(&mut self, generation: u64, sel: u64, job: CopyJob) -> u64 {
        let req = self.worker.io(generation, IoRequest::Copy { sel, job });
        self.clip.started(req, sel);
        req
    }
    /// Copy of derived (decompressed/reassembled) bytes through the decode worker.
    pub(super) fn copy_bytes(&mut self, generation: u64, sel: u64, space: Space, off: u64, len: u64, requested: u64) -> u64 {
        let req = self.worker.send(generation, Request::CopyBytes { sel, space, off, len, requested });
        self.clip.started(req, sel);
        req
    }
    /// Starts a byte search (superseding any running one); returns its request id.
    pub(super) fn search(&self, generation: u64, path: PathBuf, needle: Vec<u8>, from: u64, backward: bool) -> u64 {
        self.worker.search(generation, path, needle, from, backward)
    }

    // ---------------- cancelling ----------------

    /// Not cancel_all: a running scan of this folder must survive a file switch.
    pub(super) fn cancel_file_jobs(&mut self) {
        self.worker.cancel_file_jobs();
        self.stat = None;
        self.clip.cancelled();
    }
    pub(super) fn cancel_search(&self) {
        self.worker.cancel_search();
    }
    /// Cancels every job in flight. Returns (copy, scan, resolve) that were pending.
    pub(super) fn cancel_all(&mut self) -> (bool, bool, bool) {
        self.worker.cancel_all();
        self.worker.cancel_search();
        (self.clip.cancelled(), self.scan.take().is_some(), self.resolve.take().is_some())
    }

    // ---------------- matching replies ----------------

    pub(super) fn next_response(&self) -> Option<Tagged<Response>> {
        self.worker.rx.try_recv().ok()
    }
    /// True (and cleared) if `req` is the resolve in flight.
    pub(super) fn take_resolve(&mut self, req: u64) -> bool {
        let ok = self.resolve == Some(req);
        if ok {
            self.resolve = None;
        }
        ok
    }
    /// Auto-open flag if `req` is the scan in flight (not cleared; see `finish_scan`).
    pub(super) fn scan_matches(&self, req: u64) -> Option<bool> {
        self.scan.filter(|(r, _)| *r == req).map(|(_, a)| a)
    }
    pub(super) fn finish_scan(&mut self) {
        self.scan = None;
    }
    pub(super) fn take_stat(&mut self, req: u64) -> bool {
        let ok = self.stat == Some(req);
        if ok {
            self.stat = None;
        }
        ok
    }
    /// The copy gate, for a copy reply.
    pub(super) fn clipboard_mut(&mut self) -> &mut ClipboardState {
        &mut self.clip
    }

    #[cfg(test)]
    pub(super) fn worker(&self) -> &Worker {
        &self.worker
    }
    #[cfg(test)]
    pub(super) fn set_stat_pending(&mut self, req: u64) {
        self.stat = Some(req);
    }
}
