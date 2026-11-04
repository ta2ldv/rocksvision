//! Clipboard policy (egui-free, 0020 §2): what a copy of the current selection puts on the
//! clipboard (`plan`, bounded by the copy cap), the clipboard texts of exact bytes and hex, and
//! the copy gate (`ClipboardState`): a result may touch the clipboard only if it answers the copy
//! in flight and the selection has not changed since it was requested. The copy runs as a job
//! (`JobState`); its notes go to the one transient status notice (0020 UI ruling F1).

use super::actions::CopyKind;
use super::jobs::Outcome;
use super::provenance;
use crate::limits::LIMITS;
use crate::model::{Row, Space, Span, ValueBytes, View};
use std::path::{Path, PathBuf};

/// Note while a copy is in flight (and the status bar's busy label for it).
pub const COPYING: &str = "Copying…";
/// Note of a copy cancelled by the user or answered as cancelled.
pub const COPY_CANCELLED: &str = "Copy cancelled";

/// What to copy: the kind and the selection it applies to.
#[derive(Clone, Copy, Debug)]
pub struct CopyInput<'a> {
    pub kind: CopyKind,
    /// The selected row.
    pub row: Option<&'a Row>,
    /// The selected byte range.
    pub span: Option<Span>,
    pub view: Option<&'a View>,
    pub file: Option<&'a Path>,
}

/// How a copy is produced.
#[derive(Clone, Debug, PartialEq)]
pub enum CopyPlan {
    /// Text already known (value, key, offset, row, path); still goes through the copy gate.
    Text { text: String, note: String },
    /// Hex of physical file bytes `[off, off+len)` (I/O pool); `requested` = selection length.
    FileHex { path: PathBuf, off: u64, len: u64, requested: u64 },
    /// Hex of decompressed / reassembled bytes (decode worker).
    DerivedHex { space: Space, off: u64, len: u64, requested: u64 },
}

/// The copy of `i`, or the message saying why there is nothing to copy. Hex copies are bounded
/// by `LIMITS.copy_cap`.
pub fn plan(i: CopyInput) -> Result<CopyPlan, String> {
    let text = |text: String, note: String| Ok(CopyPlan::Text { text, note });
    match (i.kind, i.row) {
        (CopyKind::Hex, _) => {
            let s = i.span.ok_or("Nothing selected to copy")?;
            let len = s.len.min(LIMITS.copy_cap);
            if s.space == Space::File {
                let path = i.file.ok_or("No file open")?.to_path_buf();
                Ok(CopyPlan::FileHex { path, off: s.offset, len, requested: s.len })
            } else {
                Ok(CopyPlan::DerivedHex { space: s.space, off: s.offset, len, requested: s.len })
            }
        }
        (CopyKind::Value, Some(r)) => match &r.field.value_bytes {
            Some(b) => {
                let (t, note) = bytes_copy_text(b, "value");
                text(t, note)
            }
            None => {
                let n = r.field.value.chars().count();
                text(r.field.value.clone(), format!("Copied value ({n} chars, displayed text)"))
            }
        },
        (CopyKind::Key, Some(r)) => match &r.field.key_bytes {
            Some(b) => {
                let (t, note) = bytes_copy_text(b, "key");
                text(t, note)
            }
            None => Err("This row has no key".into()),
        },
        (CopyKind::Offset, _) => match i.span {
            Some(s) => text(provenance::copy_offset_text(i.view, &s), "Copied offset range".into()),
            None => Err("No byte range selected".into()),
        },
        (CopyKind::RowTsv, Some(r)) => text(r.cells.join("\t"), format!("Copied row ({} columns, TSV; cells are the table previews)", r.cells.len())),
        (CopyKind::FilePath, _) => match i.file {
            Some(p) => text(p.display().to_string(), "Copied file path".into()),
            None => Err("No file open".into()),
        },
        (_, None) => Err("No row selected".into()),
    }
}

/// Clipboard text of exact decoded bytes: valid UTF-8 → the text itself (real newlines);
/// otherwise a lossless escaped form (a backslash becomes two, bytes that are not UTF-8
/// become `\xNN`). Bounded by the copy cap (the note says so).
pub fn bytes_copy_text(b: &ValueBytes, what: &str) -> (String, String) {
    let cap = if b.total > b.data.len() as u64 { format!("; first {} of {} bytes, {} KiB copy cap", b.data.len(), b.total, LIMITS.copy_cap / 1024) } else { String::new() };
    if let Ok(s) = std::str::from_utf8(&b.data) {
        return (s.to_string(), format!("Copied {what} ({} bytes, UTF-8 text{cap})", b.data.len()));
    }
    let mut out = String::with_capacity(b.data.len() * 2);
    for chunk in b.data.utf8_chunks() {
        out.push_str(&chunk.valid().replace('\\', "\\\\"));
        for x in chunk.invalid() {
            out.push_str(&format!("\\x{x:02x}"));
        }
    }
    (out, format!("Copied {what} (escaped, {} bytes{cap})", b.data.len()))
}

/// Clipboard text of a copy-hex of `data` (selection length `requested`) and its note.
pub fn hex_copy_text(data: &[u8], requested: u64) -> (String, String) {
    let note = if (data.len() as u64) < requested {
        format!("Copied {} of {} bytes as hex (truncated: {} KiB copy cap / end of data)", data.len(), requested, LIMITS.copy_cap / 1024)
    } else {
        format!("Copied {} bytes as hex", data.len())
    };
    (crate::util::hex(data), note)
}

/// A copy in flight: job id and the selection epoch it was requested for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingCopy {
    pub req: u64,
    pub sel: u64,
}

/// A copy result may touch the clipboard only if it answers the copy in flight and the
/// selection has not changed since it was requested.
pub fn copy_accepted(pending: Option<PendingCopy>, req: u64, sel: u64, sel_now: u64) -> bool {
    pending == Some(PendingCopy { req, sel }) && sel == sel_now
}

/// What a copy reply may do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyReply {
    /// Answers the copy in flight for the current selection: apply it.
    Accepted,
    /// Answers the copy in flight but the selection changed: discard (pending cleared).
    Stale,
    /// Not the copy in flight: ignore.
    Ignored,
}

/// The applied result of a copy reply: clipboard text (only for an accepted, successful copy)
/// and the status note.
#[derive(Clone, Debug, PartialEq)]
pub struct CopyResult {
    pub text: Option<String>,
    pub note: String,
}

/// The copy gate: the copy in flight.
#[derive(Debug, Default)]
pub struct ClipboardState {
    pending: Option<PendingCopy>,
}

impl ClipboardState {
    pub fn pending(&self) -> Option<PendingCopy> {
        self.pending
    }
    /// Copy `req` was issued for selection epoch `sel` (replaces any copy in flight).
    pub(super) fn started(&mut self, req: u64, sel: u64) {
        self.pending = Some(PendingCopy { req, sel });
    }
    /// Classifies a reply against the copy in flight and the current epoch.
    pub(super) fn classify(&mut self, req: u64, sel: u64, sel_now: u64) -> CopyReply {
        if copy_accepted(self.pending, req, sel, sel_now) {
            self.pending = None;
            CopyReply::Accepted
        } else if self.pending.map(|p| p.req) == Some(req) {
            self.pending = None;
            CopyReply::Stale
        } else {
            CopyReply::Ignored
        }
    }
    /// A copy reply: the gate and every result note. None: not the copy in flight.
    pub(super) fn on_reply(&mut self, req: u64, sel: u64, sel_now: u64, outcome: Outcome<(String, String)>) -> Option<CopyResult> {
        match self.classify(req, sel, sel_now) {
            CopyReply::Accepted => Some(match outcome {
                Outcome::Done((text, note)) => CopyResult { text: Some(text), note },
                Outcome::Failed(e) => CopyResult { text: None, note: format!("Copy failed: {e}") },
                Outcome::Cancelled => CopyResult { text: None, note: COPY_CANCELLED.into() },
            }),
            CopyReply::Stale => Some(CopyResult { text: None, note: "Discarded a stale copy (selection changed); clipboard unchanged".into() }),
            CopyReply::Ignored => None,
        }
    }
    /// Cancel: true if a copy was in flight (it can no longer touch the clipboard).
    pub(super) fn cancelled(&mut self) -> bool {
        self.pending.take().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Field;

    fn row(value_bytes: Option<&[u8]>, key: Option<&[u8]>) -> Row {
        let mut f = Field::new("entry 0", Some(Span::file(0x10, 4)), "shown value");
        f.value_bytes = value_bytes.map(ValueBytes::of);
        f.key_bytes = key.map(ValueBytes::of);
        Row { cells: vec!["0".into(), "0x10".into(), "shown value".into()], field: f }
    }

    /// 0020 §2: every CopyKind × {no span, no row, file span, derived span, span > copy cap,
    /// UTF-8 / escaped / displayed value, row without key} → the exact plan or message.
    #[test]
    fn plan_table() {
        let path = Path::new("/data/000012.sst");
        let utf8 = row(Some(b"MANIFEST-000005\n"), Some(b"key-000000"));
        let bin = row(Some(&[b'a', 0xff]), None);
        let shown = row(None, None);
        let big = Span::file(0, LIMITS.copy_cap + 10);
        let derived = Span::new(Space::Block(0x5c9), 0x6d, 103);
        let inp = |kind, row, span, file| CopyInput { kind, row, span, view: None, file };
        let txt = |t: &str, n: &str| Ok(CopyPlan::Text { text: t.into(), note: n.into() });
        let cases: Vec<(&str, CopyInput, Result<CopyPlan, String>)> = vec![
            ("hex, no span", inp(CopyKind::Hex, Some(&utf8), None, Some(path)), Err("Nothing selected to copy".into())),
            ("hex, file span", inp(CopyKind::Hex, None, Some(Span::file(0x10, 4)), Some(path)), Ok(CopyPlan::FileHex { path: path.into(), off: 0x10, len: 4, requested: 4 })),
            ("hex, file span, no file", inp(CopyKind::Hex, None, Some(Span::file(0x10, 4)), None), Err("No file open".into())),
            ("hex, span > cap", inp(CopyKind::Hex, None, Some(big), Some(path)), Ok(CopyPlan::FileHex { path: path.into(), off: 0, len: LIMITS.copy_cap, requested: LIMITS.copy_cap + 10 })),
            ("hex, derived span", inp(CopyKind::Hex, None, Some(derived), None), Ok(CopyPlan::DerivedHex { space: Space::Block(0x5c9), off: 0x6d, len: 103, requested: 103 })),
            ("value, UTF-8", inp(CopyKind::Value, Some(&utf8), None, None), txt("MANIFEST-000005\n", "Copied value (16 bytes, UTF-8 text)")),
            ("value, escaped", inp(CopyKind::Value, Some(&bin), None, None), txt("a\\xff", "Copied value (escaped, 2 bytes)")),
            ("value, displayed", inp(CopyKind::Value, Some(&shown), None, None), txt("shown value", "Copied value (11 chars, displayed text)")),
            ("value, no row", inp(CopyKind::Value, None, Some(Span::file(0, 1)), None), Err("No row selected".into())),
            ("key", inp(CopyKind::Key, Some(&utf8), None, None), txt("key-000000", "Copied key (10 bytes, UTF-8 text)")),
            ("key, row without key", inp(CopyKind::Key, Some(&shown), None, None), Err("This row has no key".into())),
            ("offset, file span", inp(CopyKind::Offset, None, Some(Span::file(0x10, 4)), None), txt("file [0x10,0x14) · 4 B", "Copied offset range")),
            ("offset, derived span", inp(CopyKind::Offset, None, Some(derived), None), txt("block@0x5c9 [+0x6d,+0xd4) · 103 B · decompressed", "Copied offset range")),
            ("offset, no span", inp(CopyKind::Offset, Some(&utf8), None, None), Err("No byte range selected".into())),
            ("row", inp(CopyKind::RowTsv, Some(&utf8), None, None), txt("0\t0x10\tshown value", "Copied row (3 columns, TSV; cells are the table previews)")),
            ("row, no row", inp(CopyKind::RowTsv, None, None, None), Err("No row selected".into())),
            ("file path", inp(CopyKind::FilePath, None, None, Some(path)), txt("/data/000012.sst", "Copied file path")),
            ("file path, no file", inp(CopyKind::FilePath, None, None, None), Err("No file open".into())),
        ];
        for (what, i, want) in cases {
            assert_eq!(plan(i), want, "{what}");
        }
        assert_eq!(hex_copy_text(&[0xde, 0xad], 2), ("de ad".into(), "Copied 2 bytes as hex".into()));
        assert_eq!(hex_copy_text(&[0xde], 5).1, format!("Copied 1 of 5 bytes as hex (truncated: {} KiB copy cap / end of data)", LIMITS.copy_cap / 1024));
    }

    /// 0020 §2: the gate and every result note.
    #[test]
    fn on_reply_table() {
        let done = || Outcome::Done(("text".to_string(), "Copied value (4 bytes, UTF-8 text)".to_string()));
        let mut c = ClipboardState::default();
        c.started(7, 3);
        assert_eq!(c.on_reply(8, 3, 3, done()), None, "Ignored: another request");
        assert_eq!(c.pending(), Some(PendingCopy { req: 7, sel: 3 }));
        assert_eq!(c.on_reply(7, 3, 3, done()), Some(CopyResult { text: Some("text".into()), note: "Copied value (4 bytes, UTF-8 text)".into() }), "Accepted");
        assert_eq!(c.pending(), None);
        c.started(9, 3);
        assert_eq!(c.on_reply(9, 3, 4, done()), Some(CopyResult { text: None, note: "Discarded a stale copy (selection changed); clipboard unchanged".into() }), "Stale");
        c.started(10, 4);
        assert_eq!(c.on_reply(10, 4, 4, Outcome::Failed("disk".into())), Some(CopyResult { text: None, note: "Copy failed: disk".into() }), "Failed");
        c.started(11, 4);
        assert_eq!(c.on_reply(11, 4, 4, Outcome::Cancelled), Some(CopyResult { text: None, note: "Copy cancelled".into() }), "Cancelled");
        c.started(12, 4);
        assert!(c.cancelled() && !c.cancelled());
        assert_eq!(c.on_reply(12, 4, 4, done()), None, "after Cancel the late reply is ignored");
    }
}
