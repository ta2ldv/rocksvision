//! Decode worker: one thread owning the open `Inspector`; requests are handled in order and
//! requests of an older generation are skipped. A panic is answered with a Failed response of
//! the same kind and job id.

use super::{Outcome, Request, Response, Reveal, SelectHint, Tagged, ViewTarget};
use crate::app::clipboard::hex_copy_text;
use crate::inspect::{self, Inspector};
use crate::model::{NodeKind, Space, Status, View};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::thread;

pub(super) fn spawn(
    rx_req: Receiver<(u64, u64, Request)>,
    rt: Sender<Tagged<Response>>,
    cancel_upto: Arc<AtomicU64>,
    latest_view: Arc<AtomicU64>,
    busy: Arc<AtomicU64>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
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
                // A view request is obsolete once a newer one was sent (bounded scans stop).
                let is_view = matches!(msg, Request::View { .. });
                let latest = latest_view.clone();
                let cfn = move || cancel.load(Ordering::Relaxed) >= job || (is_view && latest.load(Ordering::Relaxed) > job);
                // A panic is answered with a Failed response of the same kind and job id,
                // so no UI state keeps waiting for it.
                let kind = DecodeKind::of(&msg);
                let msg = if is_view && cfn() {
                    // Superseded before it started: answered as cancelled (the session matches
                    // view replies by job id and ignores it).
                    kind.failed(job, "superseded by a newer view request")
                } else {
                    let resp = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle(&mut insp, job, msg, &cfn)));
                    resp.unwrap_or_else(|_| kind.failed(job, "internal decoder panic (reported instead of crashing)"))
                };
                busy.fetch_sub(1, Ordering::SeqCst);
                let _ = rt.send(Tagged { generation: req_gen, job, msg });
                wake();
            }
        })
        .expect("spawn decode worker");
}

/// What a decode request needs to be answered with a Failed response of its own kind.
enum DecodeKind {
    Open,
    Children(u64),
    View(ViewTarget),
    Bytes(Space, u64),
    CopyBytes(u64),
}

impl DecodeKind {
    fn of(r: &Request) -> Self {
        match r {
            Request::Open(_) => DecodeKind::Open,
            Request::Children { node, .. } => DecodeKind::Children(*node),
            Request::View { target, .. } => DecodeKind::View(target.clone()),
            Request::Bytes { space, off, .. } => DecodeKind::Bytes(*space, *off),
            Request::CopyBytes { sel, .. } => DecodeKind::CopyBytes(*sel),
        }
    }
    fn failed(self, req: u64, msg: &str) -> Response {
        match self {
            DecodeKind::Open => Response::OpenFailed(msg.into()),
            DecodeKind::Children(node) => Response::Children { node, children: Vec::new(), error: Some(msg.into()) },
            DecodeKind::View(target) => {
                let kind = match &target {
                    ViewTarget::Node(k) => Some(k.clone()),
                    ViewTarget::Locate { .. } => None,
                };
                Response::View {
                    req,
                    target,
                    kind,
                    view: Box::new(View { title: "Decoding failed".into(), notes: vec![msg.into()], status: Some(Status::Invalid), ..Default::default() }),
                    reveal: None,
                    select: None,
                }
            }
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
        Request::View { target, reveal } => {
            let Some(i) = insp.as_mut() else {
                return Response::View { req: job, kind: None, target, view: Box::default(), reveal: None, select: None };
            };
            let (kind, select) = match &target {
                ViewTarget::Node(k) => (Some(k.clone()), None),
                ViewTarget::Locate { space, off } => match i.locate(*space, *off, cancel) {
                    Some((k, at, span)) => (Some(k), Some(SelectHint { at, span })),
                    None => (None, None),
                },
            };
            let Some(kind) = kind else {
                return Response::View { req: job, target, kind: None, view: Box::default(), reveal: None, select: None };
            };
            let view = Box::new(i.view(&kind, cancel));
            // Canonical path: each ancestor with its children (bounded by the inspector).
            let reveal = if reveal && !cancel() {
                i.path_to(&kind, cancel).map(|p| Reveal {
                    path: p
                        .into_iter()
                        .map(|a| {
                            let ch = i.children(&a, cancel);
                            (a, ch)
                        })
                        .collect(),
                })
            } else {
                None
            };
            Response::View { req: job, target, kind: Some(kind), view, reveal, select }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// One request of every kind. The exhaustive `match` (no wildcard) makes a new `Request`
    /// variant fail to compile here until it is listed (0020 §6).
    fn all_requests() -> Vec<Request> {
        let reqs = vec![
            Request::Open(PathBuf::from("x.sst")),
            Request::Children { node: 3, kind: NodeKind::Root },
            Request::View { target: ViewTarget::Node(NodeKind::SstFooter), reveal: false },
            Request::View { target: ViewTarget::Locate { space: Space::File, off: 9 }, reveal: true },
            Request::Bytes { space: Space::Block(1), off: 16, len: 4 },
            Request::CopyBytes { sel: 5, space: Space::Block(1), off: 0, len: 4, requested: 9 },
        ];
        for r in &reqs {
            match r {
                Request::Open(_) | Request::Children { .. } | Request::View { .. } | Request::Bytes { .. } | Request::CopyBytes { .. } => {}
            }
        }
        reqs
    }

    /// Every decode request has a Failed reply of its own kind carrying its request id.
    #[test]
    fn every_request_fails_with_its_own_kind_and_req() {
        for (i, r) in all_requests().iter().enumerate() {
            let resp = DecodeKind::of(r).failed(7, "x");
            let ok = match (r, &resp) {
                (Request::Open(_), Response::OpenFailed(m)) => m == "x",
                (Request::Children { node, .. }, Response::Children { node: n, children, error: Some(e) }) => node == n && children.is_empty() && e == "x",
                (Request::View { target, .. }, Response::View { req: 7, target: t, kind, view, .. }) => {
                    target == t && kind.is_some() == matches!(target, ViewTarget::Node(_)) && view.status == Some(Status::Invalid)
                }
                (Request::Bytes { space, off, .. }, Response::Page { req: 7, space: s, off: o, outcome: Outcome::Failed(_) }) => space == s && off == o,
                (Request::CopyBytes { sel, .. }, Response::Copied { req: 7, sel: s, outcome: Outcome::Failed(_) }) => sel == s,
                _ => false,
            };
            assert!(ok, "request #{i}: Failed reply of another kind or without its request id");
        }
    }
}
