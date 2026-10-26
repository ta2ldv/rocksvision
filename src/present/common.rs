//! Shared presentation helpers: fields tables, error views, status cells, and the views of
//! CURRENT and plain-text files.

use crate::discover::FileKind;
use crate::io::{read_vec, RangeRead};
use crate::limits::{CURRENT_PREVIEW, TEXT_PREVIEW, TEXT_PREVIEW_LINES};
use crate::model::*;
use std::path::PathBuf;

pub fn fields_columns() -> Vec<Column> {
    use ColumnRole as R;
    vec![
        col(R::Field, "Field"),
        col(R::Offset, "Offset"),
        col(R::Len, "Len"),
        col(R::Decoded, "Decoded"),
        col(R::Encoding, "Encoding"),
        col(R::RawHex, "Raw hex"),
        col(R::Status, "Status"),
    ]
}

/// A view whose rows are plain fields.
pub fn of_fields(title: impl Into<String>, fields: Vec<Field>) -> View {
    let rows = fields.into_iter().map(field_row).collect();
    View { title: title.into(), columns: fields_columns(), rows, ..Default::default() }
}

pub fn error_view(title: impl Into<String>, status: Status, msg: impl Into<String>) -> View {
    let msg = msg.into();
    let f = Field::new("error", None, msg.clone()).status(status).explain(msg);
    let mut v = of_fields(title, vec![f]);
    v.status = Some(status);
    v
}

/// Attaches the typed diagnostic behind an `error_view` to its error field and the view.
pub fn with_error_diag(mut v: View, d: Option<Diagnostic>) -> View {
    if let Some(d) = d {
        if let Some(r) = v.rows.first_mut() {
            r.field.diag = Some(Box::new(d.clone()));
        }
        v.diagnostics.push(d);
    }
    v
}

pub fn span_cells(span: &Option<Span>) -> (String, String) {
    match span {
        Some(s) => {
            let prefix = match s.space {
                Space::File => "",
                Space::Block(_) => "blk+",
                Space::Record(_) => "rec+",
            };
            (format!("{prefix}0x{:x}", s.offset), s.len.to_string())
        }
        None => ("(no bytes)".into(), "-".into()),
    }
}

/// "[0x0,0x59)": a half-open file range.
pub fn range(a: u64, b: u64) -> String {
    format!("[0x{a:x},0x{b:x})")
}

/// "89 B (0x59)": a byte count in decimal and hex.
pub fn bytes_dec_hex(n: u64) -> String {
    format!("{n} B (0x{n:x})")
}

/// Explanation of the file length row: metadata, not bytes of the file.
pub const SIZE_EXPLAIN: &str = "File length reported by the file system, not stored in or decoded from the file's bytes.";

/// The file length row: file-system metadata (no offset, no length, no interpretations).
pub fn size_row(len: u64) -> Row {
    let f = Field::new("size", None, crate::util::human_size(len)).dv(DecodedValue::U64(len)).enc("file system metadata").explain(SIZE_EXPLAIN);
    Row { cells: vec![f.name.clone(), "—".into(), "—".into(), f.value.clone(), f.encoding.clone(), String::new(), st_cell(f.status)], field: f }
}

/// Location lines of a physical structure (`[start, end)` in the file): file range and stored
/// size; contents and trailer ranges when they differ from the whole; decompressed length kept
/// separate; the next structure (link) or the gap before it (link).
pub struct Location {
    pub start: u64,
    pub end: u64,
    /// Contents range (before a trailer), if the structure has one.
    pub contents: Option<(u64, u64)>,
    pub trailer: Option<(u64, u64)>,
    /// Header range (log fragments).
    pub header: Option<(u64, u64)>,
    pub decompressed: Option<u64>,
    /// Next known structure: its start and node; or None at the end of the file.
    pub next: Option<(u64, NodeKind)>,
    /// Gap before the next structure, as a node (`[end, next)`).
    pub gap: Option<NodeKind>,
    pub eof: u64,
}

impl Location {
    /// Sets the view's location lines and extent (the structure's file range).
    pub fn apply(&self, v: &mut View) {
        v.location = self.lines();
        v.extent = Some(Span::file(self.start, self.end.saturating_sub(self.start)));
    }

    pub fn lines(&self) -> Vec<WhereLine> {
        let line = |text: String| WhereLine { text, link: None };
        let stored = self.end.saturating_sub(self.start);
        let mut v = vec![line(format!("file {} · stored {}", range(self.start, self.end), bytes_dec_hex(stored)))];
        let mut parts = Vec::new();
        if let Some((a, b)) = self.header {
            parts.push(format!("header {} = {} B", range(a, b), b - a));
        }
        if let Some((a, b)) = self.contents {
            let last = if b > a { format!(", last byte 0x{:x}", b - 1) } else { String::new() };
            parts.push(format!("{} {} = {}{last}", if self.header.is_some() { "payload" } else { "contents" }, range(a, b), bytes_dec_hex(b - a)));
        }
        if let Some((a, b)) = self.trailer {
            parts.push(format!("trailer {} = {} B", range(a, b), b - a));
        }
        if !parts.is_empty() {
            v.push(line(parts.join(" · ")));
        }
        if let Some(n) = self.decompressed {
            v.push(line(format!("decompressed {} (block-local offsets, not file bytes)", bytes_dec_hex(n))));
        }
        match (&self.gap, &self.next) {
            (Some(g), Some((at, _))) => v.push(WhereLine { text: format!("gap {} before the next structure ›", range(self.end, *at)), link: Some(Target { node: g.clone(), span: Some(Span::file(self.end, at - self.end)) }) }),
            (None, Some((at, k))) => v.push(WhereLine { text: format!("next structure at 0x{at:x} ›"), link: Some(Target { node: k.clone(), span: None }) }),
            _ if self.end >= self.eof => v.push(line(format!("ends at end of file (0x{:x})", self.eof))),
            _ => {}
        }
        v
    }
}

pub fn field_row(f: Field) -> Row {
    let (o, l) = span_cells(&f.span);
    Row {
        cells: vec![f.name.clone(), o, l, f.value.clone(), f.encoding.clone(), f.raw.clone(), st_cell(f.status)],
        field: f,
    }
}

pub fn st_cell(s: Status) -> String {
    format!("{} {}", s.symbol(), s.label())
}

/// Status cell with its own word (same glyph), e.g. "✂ Incomplete (no LAST)".
pub fn st_word_cell(s: Status, word: &str) -> String {
    format!("{} {word}", s.symbol())
}

/// Fact → field with an explanation (empty explanation leaves the field without one).
pub fn explained(f: Fact, explain: impl Into<String>) -> Field {
    Field::from(f).explain(explain)
}

// ---------------- CURRENT and text files ----------------

/// CURRENT view from its first bytes and the resolved MANIFEST (path + exists on disk).
pub fn current_view(raw: &[u8], target: Result<(PathBuf, bool), String>) -> View {
    let mut f = vec![Field::new("contents", Some(Span::file(0, raw.len() as u64)), crate::util::text_preview(raw, CURRENT_PREVIEW)).raw(raw).value_bytes(raw).enc("text line + \\n").explain("CURRENT names the active MANIFEST (db/filename.cc SetCurrentFile).")];
    match target {
        Ok((p, exists)) => {
            let mut fld = Field::new("referenced MANIFEST", Some(Span::file(0, raw.len().saturating_sub(1) as u64)), p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
                .status(if exists { Status::Present } else { Status::NotPresent })
                .explain(if exists { "Double-click to open the MANIFEST." } else { "The referenced MANIFEST is missing in this directory." });
            if exists {
                fld = fld.open(p);
            }
            f.push(fld);
        }
        Err(e) => f.push(Field::new("referenced MANIFEST", None, e).status(Status::Invalid)),
    }
    of_fields("CURRENT", f)
}

pub fn text_view(r: &dyn RangeRead, kind: FileKind) -> View {
    let raw = read_vec(r, 0, TEXT_PREVIEW.min(r.len())).unwrap_or_default();
    let mut rows = Vec::new();
    let mut off = 0usize;
    for (i, line) in raw.split(|&b| b == b'\n').enumerate().take(TEXT_PREVIEW_LINES) {
        let fld = Field::new(format!("line {}", i + 1), Some(Span::file(off as u64, line.len() as u64)), String::from_utf8_lossy(line).into_owned());
        rows.push(Row { cells: vec![(i + 1).to_string(), format!("0x{off:x}"), String::from_utf8_lossy(line).into_owned()], field: fld });
        off += line.len() + 1;
    }
    use ColumnRole as R;
    let mut v = View { title: format!("{} — bounded text preview", kind.label()), columns: vec![col(R::Index, "Line"), col(R::Offset, "Offset"), col(R::Text, "Text")], rows, ..Default::default() };
    if r.len() > TEXT_PREVIEW {
        v.notes.push(format!("Showing the first {TEXT_PREVIEW} bytes of {}.", r.len()).into());
    }
    if kind == FileKind::InfoLog {
        v.notes.push("This is RocksDB's diagnostic info LOG, not a write-ahead log.".into());
    }
    v
}
