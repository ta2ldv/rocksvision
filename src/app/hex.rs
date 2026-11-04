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

/// A page the byte view needs: the caller (`AppState`) issues the read through `JobState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageReq {
    pub space: Space,
    pub base: u64,
}

/// The byte view: offset space, page, load state, one-shot scroll request and the Go-to text
/// (which describes the position shown, design B3). Fields are private; `app` moves it through
/// `reveal` / `show_space` / `ensure_visible` and executes the returned `PageReq`.
#[derive(Clone, Debug)]
pub struct HexState {
    space: Space,
    space_title: String,
    size: u64,
    page_base: u64,
    load: PageState,
    /// One-shot request for the byte view to scroll this offset into view.
    scroll_to: Option<u64>,
    goto: String,
}

impl Default for HexState {
    fn default() -> Self {
        HexState { space: Space::File, space_title: String::new(), size: 0, page_base: 0, load: PageState::Idle, scroll_to: None, goto: String::new() }
    }
}

/// The Go-to text of an offset the byte view moved to.
fn goto_text(off: u64) -> String {
    format!("0x{off:x}")
}

impl HexState {
    /// The Go-to field text (follows the position the byte view moves to).
    pub fn goto(&self) -> &str {
        &self.goto
    }
    /// The pending one-shot scroll request (not taken).
    pub fn scroll_to(&self) -> Option<u64> {
        self.scroll_to
    }
    pub(super) fn set_goto(&mut self, s: String) {
        self.goto = s;
    }
    pub(super) fn take_scroll(&mut self) -> Option<u64> {
        self.scroll_to.take()
    }
    /// No page (no file open).
    pub(super) fn idle(&mut self) {
        self.load = PageState::Idle;
    }

    /// Moves the byte view to `s`: its space (`title` gives the space's title and size when it
    /// changes), the page holding `s.offset` (returned when it must be read) and the Go-to text.
    pub(super) fn reveal(&mut self, s: Span, title: impl FnOnce() -> (String, u64)) -> Option<PageReq> {
        if s.space != self.space {
            let (t, size) = title();
            self.set_space(s.space, size, t);
        }
        let need = self.ensure_visible(s.offset).then_some(PageReq { space: s.space, base: self.page_base });
        self.goto = goto_text(s.offset);
        need
    }

    /// Pages the byte view to `base` in its space (page buttons): the page to read; the Go-to
    /// text follows (B3, 0015 C3).
    pub(super) fn page_to(&mut self, base: u64) -> PageReq {
        self.goto = goto_text(base);
        PageReq { space: self.space, base }
    }

    /// Shows offset space `space` from its start (button "show block@… (decompressed)"): page
    /// 0 is always (re)read and Go-to says 0x0.
    pub(super) fn show_space(&mut self, space: Space, size: u64, title: String) -> PageReq {
        self.set_space(space, size, title);
        self.goto = goto_text(0);
        PageReq { space, base: 0 }
    }

    /// Parses the Go-to text: decimal, or hex with a `0x` prefix.
    pub(super) fn parse_offset(t: &str) -> Option<u64> {
        let t = t.trim();
        if let Some(h) = t.strip_prefix("0x") { u64::from_str_radix(h, 16).ok() } else { t.parse::<u64>().ok() }
    }

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
    use super::{HexState, PAGE, PageReq, PageState};
    use crate::model::{Space, Span};

    /// 0020 §4: `reveal` — same space and visible: no read, Go-to set; another page: PageReq;
    /// another space: space set + PageReq; an offset past the size still names its page.
    #[test]
    fn reveal_table() {
        let ready = |space: Space, base: u64| {
            let mut h = HexState::default();
            h.set_space(space, 3 * PAGE, "t".into());
            h.begin(1, base);
            assert!(h.complete(1, space, crate::app::jobs::Outcome::Done((vec![0; 16], 3 * PAGE))));
            h
        };
        let title = || ("Block".to_string(), 500);
        // Same space, visible page.
        let mut h = ready(Space::File, 0);
        assert_eq!(h.reveal(Span::file(0x10, 4), title), None);
        assert_eq!((h.goto(), h.scroll_to(), h.space()), ("0x10", Some(0x10), Space::File));
        // Another page of the same space.
        assert_eq!(h.reveal(Span::file(PAGE + 3, 1), title), Some(PageReq { space: Space::File, base: PAGE }));
        assert_eq!((h.goto(), h.page_base()), (format!("0x{:x}", PAGE + 3).as_str(), PAGE));
        // Another space: switched (title + size from the callback), page 0 requested.
        let mut h = ready(Space::File, 0);
        assert_eq!(h.reveal(Span::new(Space::Block(7), 0x20, 1), title), Some(PageReq { space: Space::Block(7), base: 0 }));
        assert_eq!((h.space(), h.space_title(), h.size(), h.load(), h.goto()), (Space::Block(7), "Block", 500, &PageState::Idle, "0x20"));
        // An offset past the size: its page is still requested (the reply says it is empty).
        let mut h = ready(Space::File, 0);
        assert_eq!(h.reveal(Span::file(10 * PAGE, 1), title), Some(PageReq { space: Space::File, base: 10 * PAGE }));
        // Go-to parsing and show_space.
        assert_eq!((HexState::parse_offset(" 0x1f "), HexState::parse_offset("31"), HexState::parse_offset("0xzz")), (Some(31), Some(31), None));
        assert_eq!(h.show_space(Space::Record(9), 40, "R".into()), PageReq { space: Space::Record(9), base: 0 });
        assert_eq!((h.space(), h.goto()), (Space::Record(9), "0x0"));
        // Page buttons: the page of the current space, Go-to follows (architect leak 4).
        assert_eq!(h.page_to(2 * PAGE), PageReq { space: Space::Record(9), base: 2 * PAGE });
        assert_eq!(h.goto(), format!("0x{:x}", 2 * PAGE));
    }

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
