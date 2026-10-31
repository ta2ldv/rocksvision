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

/// Shown for each control character or invalid byte in the UTF-8 preview (for ASCII input one cell per byte, like the hex
/// pane's ASCII column; a valid multi-byte character stays one glyph). Listed in `ui::theme::UI_GLYPHS` (render-tested). U+25FD:
/// U+25A2 "▢" is not in the bundled proportional font (it rendered as egui's tofu U+25FB).
pub const UNPRINTABLE: char = '◽';

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
        row("varint64", vec![format!("{x} ({n} byte{})", if n == 1 { "" } else { "s" })]);
    }
    if !b.is_empty() {
        let n = b.len().min(LIMITS.interp_bytes as usize);
        row(&format!("UTF-8 (first {n} B)"), vec![utf8_preview(&b[..n])]);
    }
    v
}

/// UTF-8 text of `b` with one `UNPRINTABLE` per control character (NUL, tab, CR, LF, DEL, C1)
/// and per byte of an invalid or truncated sequence; valid printable characters are kept (one glyph per character, so only ASCII input lines up byte for byte with the hex pane).
fn utf8_preview(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len());
    for chunk in b.utf8_chunks() {
        s.extend(chunk.valid().chars().map(|c| if c.is_control() { UNPRINTABLE } else { c }));
        s.extend(std::iter::repeat_n(UNPRINTABLE, chunk.invalid().len()));
    }
    s
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

    fn utf8_row(b: &[u8]) -> String {
        interpretations(b).into_iter().find(|r| r.label.starts_with("UTF-8")).unwrap().tokens.concat()
    }

    /// 0017 D5 UI-7: every control byte (NUL, tab, CR, LF, 0x01, DEL) and every byte of an
    /// invalid sequence is its own `UNPRINTABLE` glyph, so ASCII input lines up with the hex
    /// pane's ASCII column (one cell per byte); valid multi-byte UTF-8 stays one char. D6: a
    /// 3-byte char cut by the `interp_bytes` limit leaves one glyph per leftover byte.
    #[test]
    fn utf8_preview_marks_each_unprintable_byte() {
        // sst-zstd block #14 entry 0 / entry 1 (GUI finding).
        let e0 = [0x00, 0x0f, 0x5b, 0x6b, 0x65, 0x79, 0x2d, 0x30, 0x34, 0x32, 0x01, 0x2b, 0, 0, 0, 0, 0, 0, 0x76, 0x61, 0x6c, 0x75, 0x65, 0x2d, 0x30, 0x34, 0x32];
        assert_eq!(utf8_row(&e0), "◽◽[key-042◽+◽◽◽◽◽◽value-042");
        let e1 = [0x06, 0x09, 0x5b, 0x33, 0x01, 0x2c, 0, 0, 0, 0, 0, 0];
        assert_eq!(utf8_row(&e1), "◽◽[3◽,◽◽◽◽◽◽");
        assert_eq!(utf8_row(b"a\r\nb\x7f"), "a◽◽b◽");
        assert_eq!(utf8_row(&[0xff, 0xfe, b'x']), "◽◽x", "invalid bytes: one glyph each");
        assert_eq!(utf8_row("é→".as_bytes()), "é→", "valid UTF-8 is kept");
        assert_eq!(super::UNPRINTABLE, '◽');
        // Truncation boundary: "→" (E2 86 92) straddles the cut at byte `interp_bytes` (64).
        let n = crate::limits::LIMITS.interp_bytes as usize;
        assert_eq!(n, 64);
        for (ascii, left) in [(n - 1, 1), (n - 2, 2)] {
            let mut b = vec![b'a'; ascii];
            b.extend_from_slice("→".as_bytes());
            b.extend_from_slice(b"tail");
            let r = interpretations(&b).into_iter().find(|r| r.label.starts_with("UTF-8")).unwrap();
            assert_eq!(r.label, format!("UTF-8 (first {n} B)"));
            let s = r.tokens.concat();
            assert_eq!(s.chars().filter(|&c| c == super::UNPRINTABLE).count(), left, "{left} leftover byte(s) of the cut char");
            assert_eq!(s, format!("{}{}", "a".repeat(ascii), "◽".repeat(left)));
        }
    }
}
