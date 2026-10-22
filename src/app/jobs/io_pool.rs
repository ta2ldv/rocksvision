//! Bounded I/O pool: each I/O request runs on its own short-lived thread, at most
//! `LIMITS.max_io_inflight` at once (further requests fail fast with `REFUSED`). File-system
//! access goes through the injectable `IoBackend`, so tests can delay or block reads.

use super::{hex_copy_text, CopyJob, IoRequest, Outcome, Response, Tagged};
use crate::discover::{self, DirEntry};
use crate::io::RangeReader;
use crate::limits::LIMITS;
use crate::model::Space;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;
use std::time::SystemTime;

/// Failure message of a request refused because `LIMITS.max_io_inflight` jobs are running.
pub const REFUSED: &str = "too many background reads in flight; try again";

/// File-system access used by I/O jobs; injectable so tests can delay or block reads.
pub trait IoBackend: Send + Sync {
    fn is_dir(&self, p: &Path) -> std::io::Result<bool>;
    fn scan(&self, dir: &Path) -> std::io::Result<(Vec<DirEntry>, bool)>;
    /// Bounded read; returns the bytes and the file size.
    fn read(&self, p: &Path, off: u64, len: u64) -> std::io::Result<(Vec<u8>, u64)>;
    fn stat(&self, p: &Path) -> std::io::Result<(u64, Option<SystemTime>)>;
}

pub struct FsBackend;

impl IoBackend for FsBackend {
    fn is_dir(&self, p: &Path) -> std::io::Result<bool> {
        let m = std::fs::metadata(p)?;
        if m.is_dir() {
            Ok(true)
        } else if m.is_file() {
            Ok(false)
        } else {
            Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file or folder"))
        }
    }
    fn scan(&self, dir: &Path) -> std::io::Result<(Vec<DirEntry>, bool)> {
        discover::scan(dir)
    }
    fn read(&self, p: &Path, off: u64, len: u64) -> std::io::Result<(Vec<u8>, u64)> {
        let r = RangeReader::open(p)?;
        Ok((r.read_at(off, len)?, r.len()))
    }
    fn stat(&self, p: &Path) -> std::io::Result<(u64, Option<SystemTime>)> {
        let m = std::fs::metadata(p)?;
        Ok((m.len(), m.modified().ok()))
    }
}

/// Test-harness backend: sleeps `delay` before every backend call, so the real GUI can be
/// exercised with slow jobs. Only compiled with the `harness` cargo feature.
#[cfg(feature = "harness")]
pub struct DelayBackend<B> {
    pub inner: B,
    pub delay: std::time::Duration,
}

#[cfg(feature = "harness")]
impl<B: IoBackend> DelayBackend<B> {
    /// Reads the delay from `ROCKSVISION_HARNESS_DELAY_MS`; None if unset, invalid or 0.
    pub fn from_env(inner: B) -> Option<Self> {
        let ms = std::env::var("ROCKSVISION_HARNESS_DELAY_MS").ok()?.trim().parse::<u64>().ok().filter(|&m| m > 0)?;
        Some(DelayBackend { inner, delay: std::time::Duration::from_millis(ms) })
    }
    fn wait(&self) {
        thread::sleep(self.delay);
    }
}

#[cfg(feature = "harness")]
impl<B: IoBackend> IoBackend for DelayBackend<B> {
    fn is_dir(&self, p: &Path) -> std::io::Result<bool> {
        self.wait();
        self.inner.is_dir(p)
    }
    fn scan(&self, dir: &Path) -> std::io::Result<(Vec<DirEntry>, bool)> {
        self.wait();
        self.inner.scan(dir)
    }
    fn read(&self, p: &Path, off: u64, len: u64) -> std::io::Result<(Vec<u8>, u64)> {
        self.wait();
        self.inner.read(p, off, len)
    }
    fn stat(&self, p: &Path) -> std::io::Result<(u64, Option<SystemTime>)> {
        self.wait();
        self.inner.stat(p)
    }
}

/// Everything one I/O job needs (cloned handles of the worker's shared state).
pub(super) struct Ctx {
    pub generation: u64,
    pub job: u64,
    pub tx: Sender<Tagged<Response>>,
    pub wake: Arc<dyn Fn() + Send + Sync>,
    pub inflight: Arc<AtomicUsize>,
    pub cancel: Arc<AtomicU64>,
    pub backend: Arc<dyn IoBackend>,
}

/// Starts (or refuses) one I/O job. Never blocks; every path sends exactly one response of the
/// request's own kind.
pub(super) fn run(c: Ctx, req: IoRequest) {
    let Ctx { generation, job, tx, wake, inflight, cancel, backend } = c;
    let kind = IoKind::of(&req);
    if inflight.load(Ordering::SeqCst) >= LIMITS.max_io_inflight {
        let _ = tx.send(Tagged { generation, msg: kind.failed(job, REFUSED) });
        wake();
        return;
    }
    inflight.fetch_add(1, Ordering::SeqCst);
    let inflight2 = inflight.clone();
    let (tx2, wake2, kind2) = (tx.clone(), wake.clone(), kind.clone());
    let spawned = thread::Builder::new().name("rocksvision-io".into()).spawn(move || {
        let cancelled = || cancel.load(Ordering::Relaxed) >= job;
        let send = |msg: Response| {
            let _ = tx2.send(Tagged { generation, msg });
            wake2();
        };
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_io(&*backend, job, req, &cancelled, &send)));
        if r.is_err() {
            // Same response kind and id as the request, so the waiting UI state ends.
            send(kind2.failed(job, "internal I/O panic (reported instead of crashing)"));
        }
        inflight2.fetch_sub(1, Ordering::SeqCst);
        wake2();
    });
    if let Err(e) = spawned {
        inflight.fetch_sub(1, Ordering::SeqCst);
        let _ = tx.send(Tagged { generation, msg: kind.failed(job, &format!("cannot start an I/O thread: {e}")) });
        wake();
    }
}

/// What an I/O request needs to be answered with a Failed response of its own kind.
#[derive(Clone)]
enum IoKind {
    Resolve(PathBuf),
    Scan(PathBuf),
    Page(u64),
    Copy(u64),
    Stat,
}

impl IoKind {
    fn of(r: &IoRequest) -> Self {
        match r {
            IoRequest::Resolve(p) => IoKind::Resolve(p.clone()),
            IoRequest::Scan(d) => IoKind::Scan(d.clone()),
            IoRequest::Page { off, .. } => IoKind::Page(*off),
            IoRequest::Copy { sel, .. } => IoKind::Copy(*sel),
            IoRequest::Stat(_) => IoKind::Stat,
        }
    }
    fn failed(self, req: u64, msg: &str) -> Response {
        let e = || msg.to_string();
        match self {
            IoKind::Resolve(path) => Response::Resolved { req, path, outcome: Outcome::Failed(e()) },
            IoKind::Scan(dir) => Response::Scanned { req, dir, outcome: Outcome::Failed(e()) },
            IoKind::Page(off) => Response::Page { req, space: Space::File, off, outcome: Outcome::Failed(e()) },
            IoKind::Copy(sel) => Response::Copied { req, sel, outcome: Outcome::Failed(e()) },
            IoKind::Stat => Response::Stat { req, outcome: Outcome::Failed(e()) },
        }
    }
}

fn outcome<T>(r: std::io::Result<T>, cancelled: &dyn Fn() -> bool) -> Outcome<T> {
    if cancelled() {
        return Outcome::Cancelled;
    }
    match r {
        Ok(v) => Outcome::Done(v),
        Err(e) => Outcome::Failed(e.to_string()),
    }
}

fn run_io(b: &dyn IoBackend, req: u64, r: IoRequest, cancelled: &dyn Fn() -> bool, send: &dyn Fn(Response)) {
    if cancelled() {
        send(io_cancelled(req, r));
        return;
    }
    match r {
        IoRequest::Resolve(path) => {
            let o = outcome(b.is_dir(&path), cancelled);
            send(Response::Resolved { req, path, outcome: o });
        }
        IoRequest::Scan(dir) => {
            let o = outcome(b.scan(&dir), cancelled);
            send(Response::Scanned { req, dir, outcome: o });
        }
        IoRequest::Page { path, off, len } => {
            let o = outcome(b.read(&path, off, len), cancelled);
            send(Response::Page { req, space: Space::File, off, outcome: o });
        }
        IoRequest::Copy { sel, job } => {
            let o = match job {
                CopyJob::Text { text, note } => outcome(Ok((text, note)), cancelled),
                CopyJob::FileHex { path, off, len, requested } => match outcome(b.read(&path, off, len), cancelled) {
                    Outcome::Done((d, _)) => Outcome::Done(hex_copy_text(&d, requested)),
                    Outcome::Failed(e) => Outcome::Failed(e),
                    Outcome::Cancelled => Outcome::Cancelled,
                },
            };
            send(Response::Copied { req, sel, outcome: o });
        }
        IoRequest::Stat(p) => {
            let o = outcome(b.stat(&p), cancelled);
            send(Response::Stat { req, outcome: o });
        }
    }
}

fn io_cancelled(req: u64, r: IoRequest) -> Response {
    match r {
        IoRequest::Resolve(path) => Response::Resolved { req, path, outcome: Outcome::Cancelled },
        IoRequest::Scan(dir) => Response::Scanned { req, dir, outcome: Outcome::Cancelled },
        IoRequest::Page { off, .. } => Response::Page { req, space: Space::File, off, outcome: Outcome::Cancelled },
        IoRequest::Copy { sel, .. } => Response::Copied { req, sel, outcome: Outcome::Cancelled },
        IoRequest::Stat(_) => Response::Stat { req, outcome: Outcome::Cancelled },
    }
}
