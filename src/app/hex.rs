//! Hex page state machine (egui-free). Only a 64 KiB page of one offset space is held at a time.
//! The page has an explicit load state (`PageState`): a spinner is shown only while the exact
//! request in `Loading(req)` is in flight; absent data alone never means "loading".

use super::jobs::Outcome;
use crate::limits::LIMITS;
use crate::model::{Space, Span};

const PAGE: u64 = LIMITS.hex_page_bytes;

#[derive(Debug, Clone, PartialEq)]
pub enum PageState {
    /// No page requested (no file, or a folder without a selected file).
    Idle,
    /// Waiting for the response of job `req`.
    Loading(u64),
    /// Bytes of the current page (may be empty: empty file / past the end).
    Ready(Vec<u8>),
    Error(String),
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct HexState {
    pub(super) space: Space,
    pub(super) space_title: String,
    pub(super) size: u64,
    pub(super) page_base: u64,
    pub(super) load: PageState,
    /// One-shot request for the byte view to scroll this offset into view.
    pub(super) scroll_to: Option<u64>,
}

impl Default for HexState {
    fn default() -> Self {
        HexState { space: Space::File, space_title: String::new(), size: 0, page_base: 0, load: PageState::Idle, scroll_to: None }
    }
}

impl HexState {
    pub fn space(&self) -> Space {
        self.space
    }
    pub fn space_title(&self) -> &str {
        &self.space_title
    }
    /// Size of the current offset space in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }
    pub fn page_base(&self) -> u64 {
        self.page_base
    }
    pub fn load(&self) -> &PageState {
        &self.load
    }

    pub(super) fn set_space(&mut self, space: Space, size: u64, title: String) {
        if self.space != space {
            self.space = space;
            self.load = PageState::Idle;
            self.page_base = 0;
        }
        self.size = size;
        self.space_title = title;
    }

    /// A page request with job id `req` for `base` was sent.
    pub(super) fn begin(&mut self, req: u64, base: u64) {
        self.page_base = base;
        self.load = PageState::Loading(req);
    }

    /// Applies a page response. Only the response to the request currently in flight is
    /// accepted; anything else (stale file, superseded page, after Cancel) returns false.
    pub(super) fn complete(&mut self, req: u64, space: Space, outcome: Outcome<(Vec<u8>, u64)>) -> bool {
        if self.load != PageState::Loading(req) || space != self.space {
            return false;
        }
        self.load = match outcome {
            Outcome::Done((d, size)) => {
                self.size = size;
                PageState::Ready(d)
            }
            Outcome::Failed(e) => PageState::Error(e),
            Outcome::Cancelled => PageState::Cancelled,
        };
        true
    }

    /// Cancel: an in-flight page becomes Cancelled (never left spinning).
    pub(super) fn cancel(&mut self) {
        if matches!(self.load, PageState::Loading(_)) {
            self.load = PageState::Cancelled;
        }
    }

    pub fn is_loading(&self) -> bool {
        matches!(self.load, PageState::Loading(_))
    }

    pub fn page(&self) -> Option<&Vec<u8>> {
        match &self.load {
            PageState::Ready(p) => Some(p),
            _ => None,
        }
    }

    /// Moves the page so `off` is visible. Returns true if a new page must be requested.
    pub(super) fn ensure_visible(&mut self, off: u64) -> bool {
        let base = off / PAGE * PAGE;
        self.scroll_to = Some(off);
        let have = matches!(self.load, PageState::Ready(_) | PageState::Loading(_));
        if base != self.page_base || !have {
            self.page_base = base;
            return true;
        }
        false
    }

    pub fn bytes_in(&self, span: &Span) -> Option<Vec<u8>> {
        let p = self.page()?;
        if span.space != self.space || span.offset < self.page_base {
            return None;
        }
        let s = (span.offset - self.page_base) as usize;
        let e = s.checked_add(span.len as usize)?;
        p.get(s..e.min(p.len())).map(|x| x.to_vec())
    }
}

/// One interpretation row: a label and its value tokens. The UI renders each token unbroken
/// and wraps only between tokens (at the " / " separator), so a number is never split.
#[derive(Clone, Debug, PartialEq)]
pub struct Interpretation {
    pub label: String,
    pub tokens: Vec<String>,
}

/// Numeric interpretations of the bytes starting at the selection.
pub fn interpretations(b: &[u8]) -> Vec<Interpretation> {
    let mut v = Vec::new();
    let mut row = |label: &str, tokens: Vec<String>| v.push(Interpretation { label: label.into(), tokens });
    if let Some(&x) = b.first() {
        row("u8", vec![x.to_string(), format!("0x{x:02x}"), format!("0b{x:08b}")]);
    }
    if b.len() >= 2 {
        row("u16 LE / BE", vec![u16::from_le_bytes([b[0], b[1]]).to_string(), u16::from_be_bytes([b[0], b[1]]).to_string()]);
    }
    if b.len() >= 4 {
        let a = [b[0], b[1], b[2], b[3]];
        row("u32 LE / BE", vec![u32::from_le_bytes(a).to_string(), u32::from_be_bytes(a).to_string()]);
    }
    if b.len() >= 8 {
        let mut a = [0u8; 8];
        a.copy_from_slice(&b[..8]);
        row("u64 LE / BE", vec![u64::from_le_bytes(a).to_string(), u64::from_be_bytes(a).to_string()]);
    }
    if let Some((x, n)) = crate::util::varint64(b, 0) {
        row("varint64", vec![format!("{x} ({n} bytes)")]);
    }
    if !b.is_empty() {
        let n = b.len().min(LIMITS.interp_bytes as usize);
        let s = String::from_utf8_lossy(&b[..n]).into_owned();
        row(&format!("UTF-8 (first {n} B)"), vec![s]);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::interpretations;

    /// Every number is one token; separators are never inside a token.
    #[test]
    fn interpretation_tokens_are_whole_numbers() {
        let b = [0x8f, 0xe3, 0xd2, 0x47, 0x11, 0x22, 0x33, 0x44];
        let rows = interpretations(&b);
        let u64row = rows.iter().find(|r| r.label == "u64 LE / BE").unwrap();
        assert_eq!(u64row.tokens, vec![u64::from_le_bytes(b).to_string(), u64::from_be_bytes(b).to_string()]);
        let u8row = rows.iter().find(|r| r.label == "u8").unwrap();
        assert_eq!(u8row.tokens, vec!["143", "0x8f", "0b10001111"]);
        assert!(rows.iter().flat_map(|r| &r.tokens).all(|t| !t.contains(" / ")));
    }
}
