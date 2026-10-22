//! Decode worker: one thread owning the open `Inspector`; requests are handled in order and
//! requests of an older generation are skipped. A panic is answered with a Failed response of
//! the same kind and job id.

use super::{hex_copy_text, Outcome, Request, Response, Tagged};
use crate::inspect::{self, Inspector};
use crate::model::{NodeKind, Space, Status, View};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread;

pub(super) fn spawn(rx_req: Receiver<(u64, u64, Request)>, rt: Sender<Tagged<Response>>, cancel_upto: Arc<AtomicU64>, busy: Arc<AtomicU64>, wake: Arc<dyn Fn() + Send + Sync>) {
    thread::Builder::new()
        .name("rocksvision-decode".into())
        .spawn(move || {
            let mut insp: Option<Inspector> = None;
            let mut generation = 0u64;
            while let Ok((req_gen, job, msg)) = rx_req.recv() {
                if req_gen < generation {
                    continue;
                }
                generation = req_gen;
                busy.fetch_add(1, Ordering::SeqCst);
                let cancel = cancel_upto.clone();
                let cfn = move || cancel.load(Ordering::Relaxed) >= job;
                // A panic is answered with a Failed response of the same kind and job id,
                // so no UI state keeps waiting for it.
                let kind = DecodeKind::of(&msg);
                let resp = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle(&mut insp, job, msg, &cfn)));
                let msg = resp.unwrap_or_else(|_| kind.failed(job, "internal decoder panic (reported instead of crashing)"));
                busy.fetch_sub(1, Ordering::SeqCst);
                let _ = rt.send(Tagged { generation: req_gen, msg });
                wake();
            }
        })
        .expect("spawn decode worker");
}

/// What a decode request needs to be answered with a Failed response of its own kind.
enum DecodeKind {
    Open,
    Children(u64),
    View(NodeKind),
    Bytes(Space, u64),
    CopyBytes(u64),
}

impl DecodeKind {
    fn of(r: &Request) -> Self {
        match r {
            Request::Open(_) => DecodeKind::Open,
            Request::Children { node, .. } => DecodeKind::Children(*node),
            Request::View(k) => DecodeKind::View(k.clone()),
            Request::Bytes { space, off, .. } => DecodeKind::Bytes(*space, *off),
            Request::CopyBytes { sel, .. } => DecodeKind::CopyBytes(*sel),
        }
    }
    fn failed(self, req: u64, msg: &str) -> Response {
        match self {
            DecodeKind::Open => Response::OpenFailed(msg.into()),
            DecodeKind::Children(node) => Response::Children { node, children: Vec::new(), error: Some(msg.into()) },
            DecodeKind::View(kind) => Response::View { kind, view: View { title: "Decoding failed".into(), notes: vec![msg.into()], status: Some(Status::Invalid), ..Default::default() } },
            DecodeKind::Bytes(space, off) => Response::Page { req, space, off, outcome: Outcome::Failed(msg.into()) },
            DecodeKind::CopyBytes(sel) => Response::Copied { req, sel, outcome: Outcome::Failed(msg.into()) },
        }
    }
}

fn handle(insp: &mut Option<Inspector>, job: u64, req: Request, cancel: &dyn Fn() -> bool) -> Response {
    match req {
        Request::Open(p) => match inspect::open(&p) {
            Ok(mut i) => {
                let children = i.children(&NodeKind::Root, cancel);
                let label = i.kind_label().to_string();
                let file_len = i.file_len();
                let mtime = i.mtime();
                *insp = Some(i);
                Response::Opened { label, children, file_len, mtime }
            }
            Err(e) => {
                *insp = None;
                Response::OpenFailed(e)
            }
        },
        Request::Children { node, kind } => Response::Children { node, children: insp.as_mut().map(|i| i.children(&kind, cancel)).unwrap_or_default(), error: None },
        Request::View(kind) => {
            let view = insp.as_mut().map(|i| i.view(&kind, cancel)).unwrap_or_default();
            Response::View { kind, view }
        }
        Request::Bytes { space, off, len } => {
            let outcome = if cancel() {
                Outcome::Cancelled
            } else {
                match insp.as_mut() {
                    Some(i) => match i.space_bytes(space, off, len) {
                        Some(d) => Outcome::Done((d, i.space_size(space))),
                        None => Outcome::Failed("bytes of this space are not available (decode failed or evicted)".into()),
                    },
                    None => Outcome::Failed("no file is open".into()),
                }
            };
            Response::Page { req: job, space, off, outcome }
        }
        Request::CopyBytes { sel, space, off, len, requested } => {
            let outcome = if cancel() {
                Outcome::Cancelled
            } else {
                match insp.as_mut().and_then(|i| i.space_bytes(space, off, len)) {
                    Some(d) => Outcome::Done(hex_copy_text(&d, requested)),
                    None => Outcome::Failed("selected derived bytes are not available".into()),
                }
            };
            Response::Copied { req: job, sel, outcome }
        }
    }
}
