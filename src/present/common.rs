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
