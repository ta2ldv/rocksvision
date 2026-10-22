//! Byte search over the physical file bytes (bounded chunks, cancellable) and query parsing.

use super::{Response, Tagged};
use crate::io::RangeReader;
pub use crate::limits::SEARCH_CHUNK;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;

/// Runs one search on its own thread; it stops when `search_id` moves past its own id.
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn(search_id: Arc<AtomicU64>, tx: Sender<Tagged<Response>>, wake: Arc<dyn Fn() + Send + Sync>, generation: u64, path: PathBuf, needle: Vec<u8>, from: u64, backward: bool) {
    let my = search_id.fetch_add(1, Ordering::SeqCst) + 1;
    thread::spawn(move || {
        let res = search_file(&path, &needle, from, backward, &|| search_id.load(Ordering::Relaxed) != my, &|d, t| {
            let _ = tx.send(Tagged { generation, msg: Response::SearchProgress { done: d, total: t } });
            wake();
        });
        let msg = match res {
            Some(Some(off)) => Response::SearchHit { off, len: needle.len() as u64 },
            Some(None) => Response::SearchDone { found: false, cancelled: false },
            None => Response::SearchDone { found: false, cancelled: true },
        };
        let _ = tx.send(Tagged { generation, msg });
        wake();
    });
}

/// Bounded chunked search. Returns None if cancelled, Some(None) if not found.
pub fn search_file(path: &std::path::Path, needle: &[u8], from: u64, backward: bool, cancel: &dyn Fn() -> bool, progress: &dyn Fn(u64, u64)) -> Option<Option<u64>> {
    if needle.is_empty() {
        return Some(None);
    }
    let r = RangeReader::open(path).ok()?;
    let len = r.len();
    let nl = needle.len() as u64;
    let overlap = nl - 1;
    if !backward {
        let mut pos = from.min(len);
        while pos < len {
            if cancel() {
                return None;
            }
            let n = SEARCH_CHUNK.min(len - pos) + overlap.min(len - pos.min(len));
            let buf = r.read_at(pos, n).ok()?;
            if let Some(i) = find(&buf, needle) {
                return Some(Some(pos + i as u64));
            }
            pos += SEARCH_CHUNK;
            progress(pos.min(len), len);
        }
    } else {
        let mut end = from.min(len);
        loop {
            if cancel() {
                return None;
            }
            let start = end.saturating_sub(SEARCH_CHUNK);
            let buf = r.read_at(start, (end + overlap).min(len) - start).ok()?;
            let mut best = None;
            let mut i = 0usize;
            while let Some(j) = find(&buf[i..], needle) {
                let at = start + (i + j) as u64;
                if at < end {
                    best = Some(at);
                }
                i += j + 1;
            }
            if best.is_some() {
                return Some(best);
            }
            if start == 0 {
                break;
            }
            end = start;
            progress(len - end, len);
        }
    }
    Some(None)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > hay.len() {
        return None;
    }
    let first = needle[0];
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        match hay[i..hay.len() - needle.len() + 1].iter().position(|&b| b == first) {
            Some(p) => {
                let s = i + p;
                if &hay[s..s + needle.len()] == needle {
                    return Some(s);
                }
                i = s + 1;
            }
            None => return None,
        }
    }
    None
}

/// Parses a search query: "hex:" prefix or 0x… / spaced hex pairs → bytes, else literal UTF-8.
pub fn parse_query(q: &str, hex_mode: bool) -> Result<Vec<u8>, String> {
    let t = q.trim();
    let hexish = hex_mode || t.starts_with("0x") || t.starts_with("hex:");
    if !hexish {
        return Ok(q.as_bytes().to_vec());
    }
    let s: String = t.trim_start_matches("hex:").trim_start_matches("0x").chars().filter(|c| !c.is_whitespace()).collect();
    if s.len() % 2 != 0 || s.is_empty() {
        return Err("hex query needs an even number of hex digits".into());
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| format!("invalid hex digits {:?}", &s[i..i + 2]))).collect()
}
