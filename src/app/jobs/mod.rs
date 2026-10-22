//! Background jobs facade: one decode worker owning the open Inspector (`decode`), a bounded
//! pool of short-lived I/O jobs (`io_pool`: folder scans, hex pages, copies, stat) and a byte
//! search thread (`search`). All share one response channel.
//! Every request carries a generation; results from an older generation are discarded by the app.
//! Every request gets a monotonically increasing job id (returned to the caller and echoed in
//! the response as `req`); long jobs poll `cancel_upto >= job id`, so Cancel/close only affects
//! jobs dispatched before it. Nothing here blocks the caller: `send`/`io` only enqueue or spawn.
//! Every request kind has a kind-correct Failed/Cancelled reply, so no waiting state is stranded.

mod decode;
mod io_pool;
mod search;

use crate::discover::DirEntry;
use crate::model::{Child, NodeKind, Space, View};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::SystemTime;

#[cfg(feature = "harness")]
pub use io_pool::DelayBackend;
pub use io_pool::{FsBackend, IoBackend, REFUSED};
pub use search::{parse_query, search_file, SEARCH_CHUNK};

/// Decode-worker requests.
pub enum Request {
    Open(PathBuf),
    Children { node: u64, kind: NodeKind },
    View(NodeKind),
    /// A hex page of a derived (decompressed/reassembled) space.
    Bytes { space: Space, off: u64, len: u64 },
    /// Copy-hex of derived bytes for selection `sel`; `requested` is the selection length.
    CopyBytes { sel: u64, space: Space, off: u64, len: u64, requested: u64 },
}

/// Result of a background job: done, failed with a message, or cancelled before completion.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome<T> {
    Done(T),
    Failed(String),
    Cancelled,
}

pub enum Response {
    Opened { label: String, children: Vec<Child>, file_len: u64, mtime: Option<SystemTime> },
    OpenFailed(String),
    /// `error`: decoding the children failed (the node is marked Invalid).
    Children { node: u64, children: Vec<Child>, error: Option<String> },
    View { kind: NodeKind, view: View },
    /// A hex page (`data`, total size of the space).
    Page { req: u64, space: Space, off: u64, outcome: Outcome<(Vec<u8>, u64)> },
    /// Clipboard text + status note for selection id `sel`.
    Copied { req: u64, sel: u64, outcome: Outcome<(String, String)> },
    /// Answer of `IoRequest::Resolve`: is the path a folder (true) or a file (false)?
    Resolved { req: u64, path: PathBuf, outcome: Outcome<bool> },
    Scanned { req: u64, dir: PathBuf, outcome: Outcome<(Vec<DirEntry>, bool)> },
    Stat { req: u64, outcome: Outcome<(u64, Option<SystemTime>)> },
    SearchProgress { done: u64, total: u64 },
    SearchHit { off: u64, len: u64 },
    SearchDone { found: bool, cancelled: bool },
}

pub struct Tagged<T> {
    pub generation: u64,
    pub msg: T,
}

pub enum CopyJob {
    /// Text already known (value, offset, row); still goes through the job so the same
    /// stale-selection gate applies to every copy.
    Text { text: String, note: String },
    /// Copy hex of physical file bytes `[off, off+len)`; `requested` is the selection length.
    FileHex { path: PathBuf, off: u64, len: u64, requested: u64 },
}

/// I/O-pool requests.
pub enum IoRequest {
    /// Folder or file? Sends `Resolved` (stat only; the app then requests the scan/open).
    Resolve(PathBuf),
    Scan(PathBuf),
    /// Physical file page (`Response::Page` with `Space::File`).
    Page { path: PathBuf, off: u64, len: u64 },
    Copy { sel: u64, job: CopyJob },
    Stat(PathBuf),
}

/// Builds the clipboard text for a copy-hex of `data` (selection length `requested`).
pub fn hex_copy_text(data: &[u8], requested: u64) -> (String, String) {
    let note = if (data.len() as u64) < requested {
        format!("Copied {} of {} bytes as hex (truncated: {} KiB copy cap / end of data)", data.len(), requested, crate::limits::LIMITS.copy_cap / 1024)
    } else {
        format!("Copied {} bytes as hex", data.len())
    };
    (crate::util::hex(data), note)
}

pub struct Worker {
    tx: Sender<(u64, u64, Request)>,
    pub rx: Receiver<Tagged<Response>>,
    resp_tx: Sender<Tagged<Response>>,
    /// Jobs with id <= this value are cancelled.
    cancel_upto: Arc<AtomicU64>,
    /// Folder scans / path resolves with id <= this value are cancelled. Separate from
    /// `cancel_upto` so opening a file (which cancels the previous file's jobs) does not
    /// cancel the still-running scan of its folder.
    scan_cancel_upto: Arc<AtomicU64>,
    next_job: AtomicU64,
    pub busy: Arc<AtomicU64>,
    /// I/O jobs currently running.
    pub io_inflight: Arc<AtomicUsize>,
    /// Current search id; a search runs while the id is unchanged (Cancel/new search bump it).
    pub search_id: Arc<AtomicU64>,
    wake: Arc<dyn Fn() + Send + Sync>,
    backend: Arc<dyn IoBackend>,
}

impl Worker {
    /// Worker on the real file system. With the `harness` feature, `ROCKSVISION_HARNESS_DELAY_MS`
    /// wraps the backend in a `DelayBackend` (test builds only; release builds never sleep).
    pub fn spawn(wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        #[cfg(feature = "harness")]
        if let Some(b) = DelayBackend::from_env(FsBackend) {
            return Self::spawn_with(wake, Arc::new(b));
        }
        Self::spawn_with(wake, Arc::new(FsBackend))
    }

    pub fn spawn_with(wake: Arc<dyn Fn() + Send + Sync>, backend: Arc<dyn IoBackend>) -> Self {
        let (tx, rx_req) = channel::<(u64, u64, Request)>();
        let (resp_tx, rx) = channel::<Tagged<Response>>();
        let cancel = Arc::new(AtomicU64::new(0));
        let busy = Arc::new(AtomicU64::new(0));
        decode::spawn(rx_req, resp_tx.clone(), cancel.clone(), busy.clone(), wake.clone());
        Worker { tx, rx, resp_tx, cancel_upto: cancel, next_job: AtomicU64::new(0), scan_cancel_upto: Arc::new(AtomicU64::new(0)), busy, io_inflight: Arc::new(AtomicUsize::new(0)), search_id: Arc::new(AtomicU64::new(0)), wake, backend }
    }

    fn next_id(&self) -> u64 {
        self.next_job.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Queues a decode-worker request; returns its job id immediately.
    pub fn send(&self, generation: u64, msg: Request) -> u64 {
        let job = self.next_id();
        let _ = self.tx.send((generation, job, msg));
        job
    }

    /// Runs a bounded I/O job on its own short-lived thread so a slow read never blocks the
    /// UI or the decode worker. Returns the job id immediately. At most
    /// `LIMITS.max_io_inflight` jobs run at once; further requests fail fast.
    pub fn io(&self, generation: u64, req: IoRequest) -> u64 {
        let job = self.next_id();
        let cancel = if matches!(req, IoRequest::Resolve(_) | IoRequest::Scan(_)) { self.scan_cancel_upto.clone() } else { self.cancel_upto.clone() };
        io_pool::run(io_pool::Ctx { generation, job, tx: self.resp_tx.clone(), wake: self.wake.clone(), inflight: self.io_inflight.clone(), cancel, backend: self.backend.clone() }, req);
        job
    }

    /// Cancels every decode/I-O job dispatched so far, including folder scans (later
    /// requests are unaffected).
    pub fn cancel_all(&self) {
        let n = self.next_job.load(Ordering::SeqCst);
        self.cancel_upto.store(n, Ordering::SeqCst);
        self.scan_cancel_upto.store(n, Ordering::SeqCst);
    }

    /// Cancels decode, page, copy and stat jobs dispatched so far, but not folder scans or
    /// path resolves (used when switching files inside the same folder).
    pub fn cancel_file_jobs(&self) {
        self.cancel_upto.store(self.next_job.load(Ordering::SeqCst), Ordering::SeqCst);
    }

    /// Cancels folder scans / path resolves dispatched so far.
    pub fn cancel_scans(&self) {
        self.scan_cancel_upto.store(self.next_job.load(Ordering::SeqCst), Ordering::SeqCst);
    }

    pub fn cancel_search(&self) {
        self.search_id.fetch_add(1, Ordering::SeqCst);
    }

    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst) > 0
    }

    pub fn io_busy(&self) -> bool {
        self.io_inflight.load(Ordering::SeqCst) > 0
    }

    /// Searches the physical bytes of `path` for `needle`, forward from `from` (or backward),
    /// in bounded chunks on a separate thread.
    pub fn search(&self, generation: u64, path: PathBuf, needle: Vec<u8>, from: u64, backward: bool) {
        search::spawn(self.search_id.clone(), self.resp_tx.clone(), self.wake.clone(), generation, path, needle, from, backward);
    }
}
