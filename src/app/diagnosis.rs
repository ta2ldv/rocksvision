//! Diagnosis data for the Explanation panel (egui-free, unit-tested): what a failed check means,
//! built only from the typed `Diagnostic` the parser attached (never by parsing display text).
//! Invalid/Truncated elements get a Diagnosis grid; Unsupported and Unknown are separate
//! statements and never appear under Diagnosis. Wording states the observed fact only; it never
//! names a physical cause.

use crate::format::log::{is_open_at_eof, is_read_error};

/// Label of the line saying what RocksVision did (never a claim about RocksDB).
pub const RECOVERY_LABEL: &str = "RocksVision recovery";
/// Label of the line citing RocksDB behaviour, shown only with source evidence.
pub const ENGINE_LABEL: &str = "Engine reference (source-cited)";
use crate::model::{CheckKind, DecodedValue, Diagnostic, RowTally, Span, Status, Trust, View};

/// Text shown when the parser recorded no value for a line.
pub const NOT_RECORDED: &str = "not recorded by the parser";
pub const NOT_AVAILABLE: &str = "not available";

/// Lines in a node summary before "+N more".
pub const SUMMARY_LINES: usize = 5;

/// One line of the Diagnosis grid. `link` is a span the UI selects in its own space.
#[derive(Clone, Debug, PartialEq)]
pub struct DiagLine {
    pub label: &'static str,
    pub text: String,
    pub link: Option<Span>,
    /// Prose (Finding, Decoded content, Reader) wraps at word boundaries; otherwise the text is
    /// one value (hex, number, enum name, span) that is never broken.
    pub prose: bool,
}

/// Status word of an element decoded from bytes that failed their enclosing checksum
/// (`Field::untrusted`, status Invalid) and its meaning.
pub const UNTRUSTED_WORD: &str = "Untrusted";
pub const UNTRUSTED_MEANING: &str = "Untrusted: decoded from a block whose checksum failed; not trustworthy.";
/// Short count line for a node whose own status does not show its failures, e.g.
/// "1 truncated" or "2 invalid · 1 truncated". None without failures.
pub fn failure_counts(fails: &[NodeFailure]) -> Option<(Status, String)> {
    let inv = fails.iter().filter(|f| f.status == Status::Invalid).count();
    // A fragmented record the end of file left open is an incomplete record (its fragments
    // may be intact), not a truncated structure.
    let open = fails.iter().filter(|f| f.status == Status::Truncated && f.diag.as_ref().is_some_and(is_open_at_eof)).count();
    let tr = fails.iter().filter(|f| f.status == Status::Truncated).count() - open;
    let rec = if open == 1 { "incomplete record" } else { "incomplete records" };
    let parts: Vec<String> = [(inv, "invalid"), (tr, "truncated"), (open, rec)].iter().filter(|(n, _)| *n > 0).map(|(n, w)| format!("{n} {w}")).collect();
    if parts.is_empty() {
        return None;
    }
    Some((if inv > 0 { Status::Invalid } else { Status::Truncated }, parts.join(" · ")))
}

/// The Diagnosis section of one Invalid/Truncated element.
#[derive(Clone, Debug, PartialEq)]
pub struct Diagnosis {
    /// Invalid or Truncated (heading symbol and colour).
    pub status: Status,
    pub lines: Vec<DiagLine>,
}

/// How an element's status is explained: a Diagnosis (corruption), or a separate statement for
/// unsupported features and missing context.
#[derive(Clone, Debug, PartialEq)]
pub enum Finding {
    Diagnosis(Diagnosis),
    Unsupported(String),
    Unknown(String),
    Limited(String),
}

/// Human name of a check.
pub fn check_label(c: CheckKind) -> &'static str {
    match c {
        CheckKind::Crc32c => "Checksum (crc32c)",
        CheckKind::XxHash => "Checksum (xxHash32)",
        CheckKind::XxHash64 => "Checksum (xxHash64)",
        CheckKind::Xxh3 => "Checksum (XXH3)",
        CheckKind::Magic => "Magic number",
        CheckKind::Length => "Length field",
        CheckKind::Varint => "Varint encoding",
        CheckKind::RestartCount => "Restart point count",
        CheckKind::Truncation => "File length (truncation)",
        CheckKind::Handle => "Block handle",
        CheckKind::Decompression => "Decompression",
        CheckKind::Limit => "Resource limit",
        CheckKind::Count => "Record count",
        CheckKind::UnknownType => "Type code",
        CheckKind::LogNumber => "Log number (recyclable record)",
        CheckKind::Structure => "Structure consistency",
        CheckKind::Unsupported => "Supported feature",
    }
}

/// "Check" line of one diagnostic: a record left open at EOF with every fragment intact is a
/// record-completeness finding ("no LAST fragment"); "File length (truncation)" stays for bytes
/// cut by the end of file (the cut-fragment variant expects a payload length, not a type).
pub fn diag_check_label(d: &Diagnostic) -> &'static str {
    if is_open_at_eof(d) && matches!(d.expected, Some(DecodedValue::Text(_))) { "Record completeness (no LAST fragment)" } else { check_label(d.check) }
}

/// Short phrase for a node summary line.
fn check_phrase(c: CheckKind) -> &'static str {
    match c {
        CheckKind::Crc32c | CheckKind::XxHash | CheckKind::XxHash64 | CheckKind::Xxh3 => "Checksum mismatch",
        CheckKind::Magic => "Magic number mismatch",
        CheckKind::Length => "Length inconsistent",
        CheckKind::Varint => "Undecodable varint",
        CheckKind::RestartCount => "Restart count does not fit",
        CheckKind::Truncation => "Truncated",
        CheckKind::Handle => "Invalid block handle",
        CheckKind::Decompression => "Decompression failed",
        CheckKind::Limit => "Resource limit",
        CheckKind::Count => "Count mismatch",
        CheckKind::UnknownType => "Unknown type code",
        CheckKind::LogNumber => "Log number mismatch",
        CheckKind::Structure => "Inconsistent structure",
        CheckKind::Unsupported => "Unsupported",
    }
}

fn is_checksum(c: CheckKind) -> bool {
    matches!(c, CheckKind::Crc32c | CheckKind::XxHash | CheckKind::XxHash64 | CheckKind::Xxh3)
}

/// "file [0x1a2b,0x1a2f) · 4 B" / "block@0x5c9 [+0x10,+0x14) · 4 B": the one range notation.
pub fn span_text(s: &Span) -> String {
    s.range_text()
}

/// Label of the Diagnosis line giving a block checksum in the engine's own form.
pub const ENGINE_FORM_LABEL: &str = "Engine form";

/// The checksum values as RocksDB reports them, when a context modifier was applied
/// (format_version ≥ 6): stored = trailer − modifier, computed = without modifier. Source:
/// RocksDB 10.4.2 table/block_based/reader_common.cc VerifyBlockChecksum (stored −=
/// ChecksumModifierForContext; the message prints both in decimal). None for other checks,
/// without a modifier or without both values.
pub fn engine_form(d: &Diagnostic) -> Option<String> {
    let m = d.context_modifier?;
    if !is_checksum(d.check) {
        return None;
    }
    let (Some(DecodedValue::U32(exp)), Some(DecodedValue::U32(obs))) = (&d.expected, &d.observed) else { return None };
    let (stored, computed) = (obs.wrapping_sub(m), exp.wrapping_sub(m));
    Some(format!("stored 0x{stored:08x} · computed 0x{computed:08x} (context modifier 0x{m:08x} removed) = {stored} / {computed}"))
}

/// Derivation of an Invalid/Truncated element (0019 §1): the specific recorded cause from its
/// diagnostic, e.g. "Invalid: XXH3 checksum mismatch, found when this block was read. Details
/// in Diagnosis above."; None without a diagnostic (the caller keeps the generic meaning).
/// `block`: the element is an SST block (its row links to one), else "these bytes".
pub fn failure_derivation(status: Status, d: Option<&Diagnostic>, block: bool) -> Option<String> {
    let d = d.filter(|_| is_failure(status))?;
    let block = block || d.context_modifier.is_some();
    let when = if block { "found when this block was read" } else { "found when these bytes were read" };
    let what = match d.check {
        CheckKind::Crc32c => "CRC32c checksum mismatch,".to_string(),
        CheckKind::XxHash => "xxHash32 checksum mismatch,".to_string(),
        CheckKind::XxHash64 => "xxHash64 checksum mismatch,".to_string(),
        CheckKind::Xxh3 => "XXH3 checksum mismatch,".to_string(),
        // Review N1: bytes cut by the end of file (a record left open keeps its own label).
        CheckKind::Truncation if diag_check_label(d) == check_label(CheckKind::Truncation) => "cut off by the end of file,".to_string(),
        _ => format!("{} failed", diag_check_label(d)),
    };
    let when = if what.ends_with(',') { when.to_string() } else { when.replacen("found ", "", 1) };
    Some(format!("{}: {what} {when}. Details in Diagnosis above.", status.label()))
}

/// Display form of a typed expected/observed value (checksums and codes in hex).
pub fn value_text(v: &DecodedValue) -> String {
    match v {
        DecodedValue::U8(x) => format!("0x{x:02x} ({x})"),
        DecodedValue::U16(x) => format!("0x{x:04x} ({x})"),
        DecodedValue::U32(x) => format!("0x{x:08x}"),
        DecodedValue::U64(x) | DecodedValue::VarInt(x) => format!("{x} (0x{x:x})"),
        DecodedValue::Bytes(b) => {
            let n = b.data.len().min(16);
            let hex: Vec<String> = b.data[..n].iter().map(|x| format!("{x:02x}")).collect();
            format!("{}{} ({} B)", hex.join(" "), if b.data.len() > n { " …" } else { "" }, b.total)
        }
        DecodedValue::Text(s) => s.clone(),
        DecodedValue::Str(s) => (*s).to_string(),
        DecodedValue::Enum { name, code } => format!("{name} ({code})"),
        DecodedValue::Handle { offset, size } => format!("offset {offset} size {size}"),
        DecodedValue::Bool(b) => b.to_string(),
    }
}

fn at(s: &Span) -> String {
    s.position_text()
}

/// Explanation legend of a table view with lazily checked rows (0019 §1): one line per status
/// that occurs, in the order Present, Invalid, Truncated, Not checked. `None` = Not checked
/// (no glyph).
pub fn table_legend(t: &RowTally) -> Vec<(Option<Status>, &'static str)> {
    let mut v = Vec::new();
    if t.present > 0 {
        v.push((Some(Status::Present), "Present: checked; no problem found."));
    }
    if t.invalid > 0 {
        v.push((Some(Status::Invalid), "Invalid: a check failed when this block was read. Select the row for its Diagnosis."));
    }
    if t.truncated > 0 {
        v.push((Some(Status::Truncated), "Truncated: the file ends before this block does. Select the row for its Diagnosis."));
    }
    if t.not_checked > 0 {
        v.push((None, "Not checked: not read yet; checked when you open the block or expand its group."));
    }
    if t.other > 0 {
        v.push((None, "Other (Unknown, Resource limit): checked, but neither ✔ nor ✖ can be claimed. Select the row for its statement."));
    }
    v
}

/// The trust line (spec §12.4) from the parser's trust and check.
pub fn trust_text(d: &Diagnostic) -> String {
    match (d.trusted, d.check, d.span) {
        (Trust::Untrusted, c, _) if is_checksum(c) => "Not trustworthy: checksum failed".into(),
        // A record the reader discards (fragmented record left open at end of file).
        // No engine claim here: RocksDB behaviour is only in the source-cited Engine line.
        (Trust::Untrusted, CheckKind::Truncation, _) => "Not trustworthy: decoded from an incomplete record (see Engine reference).".into(),
        (_, CheckKind::Truncation, Some(s)) => format!("Decoded up to {}; the rest is missing", at(&s)),
        (_, CheckKind::Truncation, None) => "Decoded up to the end of the available bytes; the rest is missing".into(),
        (Trust::Untrusted, c, _) => format!("Not trustworthy: {} failed", check_label(c).to_lowercase()),
        (Trust::Partial, _, Some(s)) => format!("Fields before {} decoded normally", at(&s)),
        (Trust::Partial, _, None) => "Partly decoded: content before the failure point decoded normally".into(),
        (Trust::Trusted, _, _) => "Trusted: decoding continued and the other fields decoded normally".into(),
    }
}

/// The Diagnosis grid of one Invalid/Truncated element. Lines without a recorded value say so.
pub fn diagnosis(status: Status, d: Option<&Diagnostic>) -> Diagnosis {
    let mut lines = Vec::new();
    let mut line =
        |label: &'static str, text: String, link: Option<Span>| lines.push(DiagLine { label, text, link, prose: matches!(label, "Finding" | "Decoded content" | RECOVERY_LABEL | ENGINE_LABEL) });
    match d {
        Some(d) => {
            line("Check", diag_check_label(d).into(), None);
            match d.span {
                Some(s) => line("Bytes", span_text(&s), Some(s)),
                None => line("Bytes", NOT_AVAILABLE.into(), None),
            }
            line("Expected", d.expected.as_ref().map(value_text).unwrap_or_else(|| NOT_AVAILABLE.into()), None);
            line("Observed", d.observed.as_ref().map(value_text).unwrap_or_else(|| NOT_AVAILABLE.into()), None);
            if let Some(e) = engine_form(d) {
                line(ENGINE_FORM_LABEL, e, None);
            }
            line("Finding", if d.note.is_empty() { NOT_RECORDED.into() } else { d.note.clone() }, None);
            line("Decoded content", trust_text(d), None);
            line(RECOVERY_LABEL, if d.recovery.is_empty() { NOT_RECORDED.into() } else { d.recovery.to_string() }, None);
            // RocksDB behaviour only where the parser recorded source evidence; else omitted.
            if let Some(e) = d.engine {
                line(ENGINE_LABEL, e.to_string(), None);
            }
        }
        None => {
            for label in ["Check", "Bytes", "Expected", "Observed"] {
                line(label, NOT_RECORDED.into(), None);
            }
            line("Decoded content", "Not trustworthy: the element failed a check".into(), None);
            line(RECOVERY_LABEL, NOT_RECORDED.into(), None);
        }
    }
    Diagnosis { status, lines }
}

/// The diagnostic behind an Unsupported / Unknown / Limited statement: the first candidate whose
/// check matches the status (Unsupported → `Unsupported`, Unknown → `UnknownType`, Limited →
/// `Limit`). None when no candidate matches: the statement then uses its generic text instead
/// of an unrelated diagnostic's note.
pub fn statement_diag<'a>(status: Status, candidates: impl IntoIterator<Item = &'a Diagnostic>) -> Option<&'a Diagnostic> {
    let want = match status {
        Status::Unsupported => CheckKind::Unsupported,
        Status::Unknown => CheckKind::UnknownType,
        Status::Limited => CheckKind::Limit,
        _ => return None,
    };
    // A read error leaves the element Unknown (nothing is guessed about unread bytes).
    candidates.into_iter().find(|d| d.check == want || (status == Status::Unknown && is_read_error(d)))
}

/// How an element with this status (and optional diagnostic) is explained; None for good statuses.
pub fn finding(status: Status, d: Option<&Diagnostic>, what: &str) -> Option<Finding> {
    match status {
        Status::Invalid | Status::Truncated => Some(Finding::Diagnosis(diagnosis(status, d))),
        Status::Unsupported => Some(Finding::Unsupported(format!("RocksVision does not decode {what}; the bytes may be valid.{}", d.map(|d| format!(" ({})", d.note)).unwrap_or_default()))),
        Status::Unknown => Some(Finding::Unknown(match d {
            Some(d) => format!("not identifiable from the available bytes and context: {}. Nothing is guessed.", d.note),
            None => "not identifiable from the available bytes and context (for a standalone file, the MANIFEST/OPTIONS may be missing). Nothing is guessed.".into(),
        })),
        Status::Limited => Some(Finding::Limited(d.map(|d| d.note.clone()).unwrap_or_else(|| "decoding stopped at a configured bound".into()))),
        _ => None,
    }
}

/// One failing element of a node: a table row (with its field diagnostic) or a node-level
/// diagnostic no row carries.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeFailure {
    pub row: Option<usize>,
    pub status: Status,
    pub name: String,
    pub diag: Option<Diagnostic>,
}

impl NodeFailure {
    /// Summary line, e.g. "Checksum mismatch at file 0x1a2b (checksum)".
    pub fn summary(&self) -> String {
        let phrase = match &self.diag {
            Some(d) if is_open_at_eof(d) && matches!(d.expected, Some(DecodedValue::Text(_))) => "No LAST fragment",
            Some(d) => check_phrase(d.check),
            None if self.status == Status::Truncated => "Truncated",
            None => "Invalid",
        };
        let at = self.diag.as_ref().and_then(|d| d.span).map(|s| format!(" at {}", at(&s))).unwrap_or_default();
        if self.name.is_empty() { format!("{phrase}{at}") } else { format!("{phrase}{at} ({})", self.name) }
    }
}

fn is_failure(s: Status) -> bool {
    matches!(s, Status::Invalid | Status::Truncated)
}

/// Failures of a node: every Invalid/Truncated row, then view/tree diagnostics that no row
/// already carries (deduplicated by value).
pub fn node_failures(view: &View, node_status: Option<Status>, node_diag: Option<&Diagnostic>) -> Vec<NodeFailure> {
    // Untrusted rows (decoded from a checksum-failed block) repeat the block's own failure,
    // which the view diagnostics already carry once.
    let mut out: Vec<NodeFailure> = view
        .rows
        .iter()
        .enumerate()
        .filter(|(_, r)| is_failure(r.field.status) && !r.field.untrusted)
        .map(|(i, r)| NodeFailure { row: Some(i), status: r.field.status, name: r.field.name.clone(), diag: r.field.diag.as_deref().cloned() })
        .collect();
    let status = view.status.or(node_status).filter(|s| is_failure(*s)).unwrap_or(Status::Invalid);
    let extra = view.diagnostics.iter().chain(node_diag);
    for d in extra {
        // The same failure seen twice (a row's diagnostic and the view's, e.g. a record row and
        // the physical cut it describes): same check over the same bytes.
        let same = |f: &NodeFailure| f.diag.as_ref().is_some_and(|x| x == d || (x.check == d.check && x.span.is_some() && x.span == d.span));
        if !out.iter().any(same) {
            // A diagnostic that does not mean corruption (limit, unsupported, read error) is
            // not a failure of the node.
            if matches!(d.check, CheckKind::Limit | CheckKind::Unsupported) || is_read_error(d) {
                continue;
            }
            // A truncation diagnostic is a Truncated failure even under a node without a status.
            let status = if d.check == CheckKind::Truncation && !matches!(view.status.or(node_status), Some(s) if is_failure(s)) { Status::Truncated } else { status };
            out.push(NodeFailure { row: None, status, name: String::new(), diag: Some(d.clone()) });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NodeKind, Space};
    use std::path::PathBuf;

    fn fx(rel: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
    }
    fn label<'a>(d: &'a Diagnosis, l: &str) -> &'a DiagLine {
        d.lines.iter().find(|x| x.label == l).unwrap_or_else(|| panic!("no line {l}"))
    }

    /// Reviewer B-4: an Unsupported/Unknown/Limited statement never borrows the note of an
    /// unrelated diagnostic (e.g. a Structure diag of the same view).
    #[test]
    fn statement_uses_only_a_matching_diagnostic() {
        let d = |c, note: &str| Diagnostic::new(c, None, Trust::Partial, "r", note);
        let (structure, unsup, unknown, limit) = (d(CheckKind::Structure, "shared"), d(CheckKind::Unsupported, "feature x"), d(CheckKind::UnknownType, "tag 0x7f"), d(CheckKind::Limit, "cap"));
        assert_eq!(statement_diag(Status::Unsupported, [&structure]), None);
        assert_eq!(statement_diag(Status::Unsupported, [&structure, &unsup]).map(|d| d.note.as_str()), Some("feature x"));
        assert_eq!(statement_diag(Status::Unknown, [&structure, &unsup, &unknown]).map(|d| d.note.as_str()), Some("tag 0x7f"));
        assert_eq!(statement_diag(Status::Limited, [&unknown, &limit]).map(|d| d.note.as_str()), Some("cap"));
        assert_eq!(statement_diag(Status::Invalid, [&structure]), None);
        match finding(Status::Unsupported, statement_diag(Status::Unsupported, [&structure]), "this structure") {
            Some(Finding::Unsupported(t)) => assert!(!t.contains("shared"), "{t}"),
            other => panic!("{other:?}"),
        }
    }

    /// Field-level grid of a block checksum mismatch: check, span link, expected ≠ observed,
    /// trust and the reader's actual recovery.
    /// 0019 §2c: the engine form of a block checksum with a context modifier. Numbers from the
    /// 0017 corrupt block #100 (RocksDB reported stored = 3862634852, computed = 2307332483).
    /// Omitted without a modifier, for non-checksum checks and without both values.
    #[test]
    fn engine_form_removes_the_context_modifier() {
        let d = |m: u32| {
            Diagnostic::new(CheckKind::Xxh3, Some(Span::file(0x6390c, 4078)), Trust::Untrusted, "r", "Block checksum MISMATCH")
                .values(Some(DecodedValue::U32(0x8e833287)), Some(DecodedValue::U32(0xeb373a68)))
                .modifier(m)
        };
        let with = d(0x04fc1904);
        assert_eq!(engine_form(&with).as_deref(), Some("stored 0xe63b2164 · computed 0x89871983 (context modifier 0x04fc1904 removed) = 3862634852 / 2307332483"));
        let g = diagnosis(Status::Invalid, Some(&with));
        let labels: Vec<_> = g.lines.iter().map(|l| l.label).collect();
        assert_eq!(&labels[..5], &["Check", "Bytes", "Expected", "Observed", ENGINE_FORM_LABEL], "directly below Observed");
        assert_eq!(label(&g, "Bytes").text, "file [0x6390c,0x648fa) · 4078 B");
        assert_eq!(label(&g, "Expected").text, "0x8e833287", "Expected/Observed unchanged");
        assert_eq!(engine_form(&d(0)), None, "modifier 0 (format_version < 6): Expected/Observed already are the engine values");
        assert!(!diagnosis(Status::Invalid, Some(&d(0))).lines.iter().any(|l| l.label == ENGINE_FORM_LABEL));
        let mut other = with.clone();
        other.check = CheckKind::Magic;
        assert_eq!(engine_form(&other), None);
        let mut one = with.clone();
        one.expected = None;
        assert_eq!(engine_form(&one), None);
        // Derivation: specific cause, generic template for other checks, None without a diag.
        assert_eq!(failure_derivation(Status::Invalid, Some(&with), false).as_deref(), Some("Invalid: XXH3 checksum mismatch, found when this block was read. Details in Diagnosis above."));
        let s = Diagnostic::new(CheckKind::RestartCount, None, Trust::Partial, "r", "n");
        assert_eq!(failure_derivation(Status::Invalid, Some(&s), true).as_deref(), Some("Invalid: Restart point count failed when this block was read. Details in Diagnosis above."));
        assert_eq!(failure_derivation(Status::Truncated, Some(&s), false).as_deref(), Some("Truncated: Restart point count failed when these bytes were read. Details in Diagnosis above."));
        assert_eq!(failure_derivation(Status::Invalid, None, true), None);
        let cut = Diagnostic::new(CheckKind::Truncation, Some(Span::file(0x10, 8)), Trust::Untrusted, "r", "short read");
        assert_eq!(failure_derivation(Status::Truncated, Some(&cut), true).as_deref(), Some("Truncated: cut off by the end of file, found when this block was read. Details in Diagnosis above."));
        assert_eq!(failure_derivation(Status::Present, Some(&with), true), None);
    }

    /// 0019 §2c on a real fv6 file: the engine "computed" equals the raw checksum RocksVision
    /// computes over payload + type byte (trailer view), "stored" = trailer − modifier.
    #[test]
    fn engine_form_matches_the_raw_computed_checksum() {
        let p = fx("tests/fixtures/corrupt/sst-bitflip-data-block.sst");
        let mut s = crate::sst::SstInspector::open(crate::io::RangeReader::open(&p).unwrap());
        let l = s.layout(&|| false);
        let h = l.items[0].handle();
        let b = s.block(h, crate::model::BlockKind::Data);
        let d = b.diags.iter().find(|d| is_checksum(d.check)).expect("checksum diagnostic");
        assert_eq!(d.context_modifier, Some(b.modifier));
        let e = engine_form(d).expect("engine form");
        let (stored, computed) = (b.stored_checksum.wrapping_sub(b.modifier), b.computed_checksum.unwrap());
        assert!(e.starts_with(&format!("stored 0x{stored:08x} · computed 0x{computed:08x} ")) && e.ends_with(&format!(" = {stored} / {computed}")), "{e}");
        // A clean fv6 block has no diagnostic, hence no engine line.
        let mut z = crate::sst::SstInspector::open(crate::io::RangeReader::open(&fx("examples/files/sst-zstd/sst-zstd.sst")).unwrap());
        let zl = z.layout(&|| false);
        let zb = z.block(zl.items[0].handle(), crate::model::BlockKind::Data);
        assert!(zb.diags.iter().all(|d| engine_form(d).is_none()));
    }

    #[test]
    fn checksum_field_diagnosis_has_every_line() {
        for f in ["tests/fixtures/corrupt/sst-bitflip-data-block.sst", "examples/files/corrupt-sst-checksum-flip"] {
            let p = fx(f);
            let p = if p.is_dir() { std::fs::read_dir(&p).unwrap().next().unwrap().unwrap().path() } else { p };
            let mut i = crate::inspect::open(&p).unwrap();
            let blocks = i.children(&NodeKind::SstDataBlocks { skip: 0 }, &|| false);
            let bad = blocks.iter().find(|c| c.status == Status::Invalid).expect("corrupt data block");
            let NodeKind::SstBlock { off, size, kind } = bad.kind.clone() else { panic!("{:?}", bad.kind) };
            let trailer = i.view(&NodeKind::SstTrailer { off, size, kind }, &|| false);
            let row = trailer.rows.iter().find(|r| r.field.status == Status::Invalid).expect("invalid trailer row");
            let Some(Finding::Diagnosis(d)) = finding(row.field.status, row.field.diag.as_deref(), &row.field.name) else { panic!("no diagnosis") };
            assert!(label(&d, "Check").text.starts_with("Checksum ("), "{f}");
            let bytes = label(&d, "Bytes");
            assert_eq!(bytes.link.map(|s| (s.space, s.offset, s.len)), Some((Space::File, off, size + 5)), "{f}: whole block + trailer");
            assert_ne!(label(&d, "Expected").text, label(&d, "Observed").text);
            assert!(!label(&d, "Expected").text.contains(NOT_AVAILABLE));
            assert_eq!(label(&d, "Decoded content").text, "Not trustworthy: checksum failed");
            assert!(label(&d, RECOVERY_LABEL).text.contains("entries are still decoded"), "{f}");
            assert!(!d.lines.iter().any(|l| l.label == ENGINE_LABEL), "no engine claim without source evidence");
            // Node summary of the block: exactly one failure → the full grid can be inline.
            let v = i.view(&bad.kind, &|| false);
            let fails = node_failures(&v, Some(bad.status), bad.diag.as_deref());
            assert_eq!(fails.len(), 1, "{f}: {fails:?}");
            assert!(fails[0].summary().starts_with("Checksum mismatch at file 0x"), "{}", fails[0].summary());
        }
    }

    #[test]
    fn truncation_and_footer_diagnoses() {
        // Truncated SST: the footer node is Invalid/Truncated with a typed diagnostic.
        for f in ["tests/fixtures/corrupt/sst-truncated-footer.sst", "tests/fixtures/corrupt/sst-bad-footer-magic.sst"] {
            let mut i = crate::inspect::open(&fx(f)).unwrap();
            let root = i.children(&NodeKind::Root, &|| false);
            let foot = root.iter().find(|c| c.kind == NodeKind::SstFooter).unwrap();
            assert!(is_failure(foot.status), "{f}: {:?}", foot.status);
            let v = i.view(&NodeKind::SstFooter, &|| false);
            let fails = node_failures(&v, Some(foot.status), foot.diag.as_deref());
            assert!(!fails.is_empty() && fails.iter().all(|x| x.diag.is_some()), "{f}: {fails:?}");
            let d = diagnosis(fails[0].status, fails[0].diag.as_ref());
            assert!(d.lines.iter().all(|l| !l.text.is_empty()));
        }
        // Magic: expected is the block-based magic, the finding states the fact.
        let mut i = crate::inspect::open(&fx("tests/fixtures/corrupt/sst-bad-footer-magic.sst")).unwrap();
        let v = i.view(&NodeKind::SstFooter, &|| false);
        let d = v.diagnostics.iter().find(|d| d.check == CheckKind::Magic).unwrap();
        let g = diagnosis(Status::Invalid, Some(d));
        assert!(label(&g, "Expected").text.contains("0x88e241b785f4cff7"), "{:?}", label(&g, "Expected"));
        // WAL truncated mid record: Truncated with "Decoded up to …; the rest is missing".
        let mut i = crate::inspect::open(&fx("tests/fixtures/corrupt/wal-truncated-mid-record.log")).unwrap();
        let v = i.view(&NodeKind::LogRecord(59), &|| false);
        let fails = node_failures(&v, None, None);
        let t = fails.iter().find(|f| f.diag.as_ref().map(|d| d.check) == Some(CheckKind::Truncation)).expect("truncation failure");
        let g = diagnosis(t.status, t.diag.as_ref());
        assert!(label(&g, "Decoded content").text.ends_with("the rest is missing"), "{:?}", g);
    }

    /// Fix C2 (2): entries of a checksum-failed block are Invalid + untrusted with the block's
    /// checksum diagnostic (not Unknown); their dump cell keeps "? Unknown"; the block summary
    /// still has exactly one failure.
    #[test]
    fn checksum_failed_block_entries_are_untrusted_not_unknown() {
        let mut i = crate::inspect::open(&fx("tests/fixtures/corrupt/sst-bitflip-data-block.sst")).unwrap();
        let blocks = i.children(&NodeKind::SstDataBlocks { skip: 0 }, &|| false);
        let bad = blocks.iter().find(|c| c.status == Status::Invalid).expect("corrupt data block");
        let v = i.view(&bad.kind, &|| false);
        let un: Vec<_> = v.rows.iter().filter(|r| r.field.untrusted).collect();
        assert!(!un.is_empty());
        for r in &un {
            assert_eq!(r.field.status, Status::Invalid);
            assert_eq!(r.cells.last().map(String::as_str), Some("? Unknown"), "dump wording unchanged");
            let d = r.field.diag.as_deref().expect("block checksum diagnostic");
            assert!(is_checksum(d.check) && d.trusted == Trust::Untrusted, "{d:?}");
            assert!(d.note.contains("checksum failed; not trustworthy"), "{}", d.note);
            let Some(Finding::Diagnosis(g)) = finding(r.field.status, Some(d), &r.field.name) else { panic!() };
            assert_eq!(label(&g, "Decoded content").text, "Not trustworthy: checksum failed");
        }
        assert_eq!(node_failures(&v, Some(bad.status), bad.diag.as_deref()).len(), 1);
    }

    /// Fix C2 (3): a WAL whose last record is cut off marks Physical blocks / Logical records /
    /// the last block (tree marker only) and the root view summarises "1 truncated".
    #[test]
    fn truncated_wal_marks_tree_and_root_summary() {
        for f in ["examples/files/corrupt-wal-truncated/corrupt-wal-truncated.log", "tests/fixtures/corrupt/wal-truncated-mid-record.log"] {
            let mut i = crate::inspect::open(&fx(f)).unwrap();
            let root = i.children(&NodeKind::Root, &|| false);
            assert!(root.iter().take(2).all(|c| c.contains == Some(Status::Truncated) && c.status == Status::Present), "{f}: {root:?}");
            let blocks = i.children(&root[0].kind, &|| false);
            assert_eq!(blocks.last().and_then(|b| b.contains), Some(Status::Truncated), "{f}");
            let v = i.view(&NodeKind::Root, &|| false);
            let fails = node_failures(&v, None, None);
            assert_eq!(failure_counts(&fails), Some((Status::Truncated, "1 truncated".into())), "{f}: {fails:?}");
        }
        // An intact WAL has no marker and no summary.
        let mut i = crate::inspect::open(&fx("tests/fixtures/wal-basic/000004.log")).unwrap();
        assert!(i.children(&NodeKind::Root, &|| false).iter().all(|c| c.contains.is_none()));
        assert_eq!(failure_counts(&node_failures(&i.view(&NodeKind::Root, &|| false), None, None)), None);
    }

    /// Corrections 1–2 (0011): a fragmented WAL record left open by EOF (complete FIRST + 2
    /// MIDDLE, no LAST). Its fragments are physically intact: no marker on Physical blocks,
    /// blocks or fragments; only "Logical records" carries the marker and "· 1 incomplete
    /// record"; the root says "1 incomplete record"; the record is "Incomplete (no LAST)" (the
    /// Truncated glyph). Diagnosis: Truncation over start..EOF, expected a LAST fragment,
    /// observed EOF, Untrusted, "RocksVision recovery" and the source-cited engine reference.
    /// A cut inside a MIDDLE marks that fragment (its own bytes) and the record "Incomplete
    /// (fragment cut at EOF)". The valid fragmented control has no marker and no failure.
    #[test]
    fn wal_record_without_last_is_diagnosed() {
        use crate::format::log::{ENGINE_OPEN_AT_EOF, RECOVER_OPEN_AT_EOF, Reassembly, open_tail};
        let mut i = crate::inspect::open(&fx("tests/fixtures/corrupt/wal-missing-last.log")).unwrap();
        let root = i.children(&NodeKind::Root, &|| false);
        assert_eq!((root[0].contains, root[0].status), (None, Status::Present), "intact fragments: plain Physical blocks: {:?}", root[0]);
        assert!(!root[0].label.contains("truncated"), "{}", root[0].label);
        assert_eq!(root[1].contains, Some(Status::Truncated));
        assert!(root[1].label.ends_with("· 1 incomplete record"), "{}", root[1].label);
        let blocks = i.children(&root[0].kind, &|| false);
        assert!(blocks.iter().all(|b| b.contains.is_none()), "no block is physically cut: {blocks:?}");
        for b in 0..3 {
            let frags = i.children(&NodeKind::LogBlock(b), &|| false);
            assert!(frags.iter().all(|c| c.contains.is_none() && c.status == Status::Present), "block {b}: fragments stay Present: {frags:?}");
        }
        // Root summary and Diagnosis.
        let v = i.view(&NodeKind::Root, &|| false);
        let fails = node_failures(&v, None, None);
        assert_eq!(failure_counts(&fails), Some((Status::Truncated, "1 incomplete record".into())), "{fails:?}");
        let d = fails.iter().find_map(|f| f.diag.clone().filter(|d| d.recovery == RECOVER_OPEN_AT_EOF)).unwrap();
        assert_eq!((d.check, d.trusted, d.engine), (CheckKind::Truncation, Trust::Untrusted, Some(ENGINE_OPEN_AT_EOF)));
        assert_eq!(d.span, Some(Span::file(0x5eec, 0x18000 - 0x5eec)));
        let g = diagnosis(Status::Truncated, Some(&d));
        assert_eq!(label(&g, "Expected").text, "LAST fragment");
        assert_eq!(label(&g, "Observed").text, "end of file at 0x18000");
        assert!(label(&g, "Finding").text.contains("no LAST fragment before end of file (FIRST @0x5eec + 2 MIDDLE; 73983 payload bytes collected)"), "{g:?}");
        assert_eq!(label(&g, "Decoded content").text, "Not trustworthy: decoded from an incomplete record (see Engine reference).");
        assert_eq!(label(&g, "Check").text, "Record completeness (no LAST fragment)");
        assert!(label(&g, RECOVERY_LABEL).text.starts_with("RocksVision shows the collected fragments"), "{g:?}");
        assert!(!label(&g, RECOVERY_LABEL).text.contains("RocksDB"), "recovery line makes no engine claim");
        let eng = &label(&g, ENGINE_LABEL).text;
        assert!(eng.contains("db/log_reader.cc") && eng.contains("lines 253–271") && eng.contains("options.h:1312") && eng.contains("10.4.2"), "{eng}");
        for l in &g.lines {
            for w in ["disk", "crash", "dying", "hardware"] {
                assert!(!l.text.contains(w), "{l:?}");
            }
        }
        // The record node and its view say which fragment is missing; same diagnostic.
        assert!(i.children(&root[1].kind, &|| false).iter().filter(|c| matches!(c.kind, NodeKind::LogRecord(_))).all(|c| c.status == Status::Present), "earlier records stay ✔");
        let recs = i.children(&NodeKind::LogRecords { start: 0x3f48, page: 1 }, &|| false);
        let r = recs.iter().find(|c| c.kind == NodeKind::LogRecord(0x5eec)).expect("open record on the second tree page");
        assert_eq!((r.status, r.label.ends_with("3 frag) · Incomplete (no LAST)")), (Status::Truncated, true), "{}", r.label);
        assert_eq!(r.diag.as_deref(), Some(&d));
        assert!(recs.iter().filter(|c| c.kind != r.kind).all(|c| c.status == Status::Present), "earlier records stay ✔");
        let rv = i.view(&NodeKind::LogRecord(0x5eec), &|| false);
        assert_eq!((rv.status, rv.status_word), (Some(Status::Truncated), Some("Incomplete (no LAST)")));
        let last = rv.rows.iter().find(|row| row.field.name == "LAST fragment").unwrap();
        assert_eq!((last.field.status, last.field.value.as_str()), (Status::Truncated, "missing: end of file at 0x18000 after FIRST + 2 MIDDLE"));
        let reas = rv.rows.iter().find(|row| row.field.name == "reassembly").unwrap();
        assert_eq!(reas.field.value, Reassembly::IncompleteNoLast.text());
        // Record table: reassembly column, never "ok"; the fragments' CRCs all match.
        let page = i.view(&NodeKind::LogRecords { start: 0x3f48, page: 1 }, &|| false);
        let row = page.rows.iter().find(|r| r.field.span.map(|s| s.offset) == Some(0x5eec)).unwrap();
        assert!(
            row.cells.contains(&"incomplete (no LAST)".to_string()) && row.cells.contains(&"all match".to_string()) && row.cells.last().map(String::as_str) == Some("✂ Incomplete (no LAST)"),
            "{:?}",
            row.cells
        );
        assert!(page.rows.iter().all(|r| !r.cells.contains(&"ok".to_string())));
        // Bounded scan: one block back finds only the last MIDDLE, the start stays unknown.
        let rr = crate::io::RangeReader::open(&fx("tests/fixtures/corrupt/wal-missing-last.log")).unwrap();
        let t = open_tail(&rr, 1).unwrap().unwrap();
        assert_eq!((t.start, t.from, t.middles), (None, 0x10000, 1));
        assert!(t.diagnostic().note.contains("FIRST before the scanned blocks"));
        // Cut inside the 2nd MIDDLE: named in the record view.
        let mut i = crate::inspect::open(&fx("tests/fixtures/corrupt/wal-cut-in-middle.log")).unwrap();
        let rv = i.view(&NodeKind::LogRecord(0x5eec), &|| false);
        let last = rv.rows.iter().find(|row| row.field.name == "LAST fragment").unwrap();
        assert_eq!(last.field.value, "missing: MIDDLE at 0x10000 cut off by end of file");
        let d = last.field.diag.as_deref().unwrap();
        assert_eq!((d.trusted, d.expected.clone(), d.observed.clone()), (Trust::Untrusted, Some(DecodedValue::U64(32761)), Some(DecodedValue::U64(16384))));
        assert!(d.note.contains("MIDDLE fragment at 0x10000 is cut off by the end of file (16384 of 32761 payload bytes): the file ends inside this fragment"), "{}", d.note);
        // Root, record and tree markers describe the same thing: the record from 0x5eec to EOF.
        assert_eq!((d.check, d.recovery, d.span), (CheckKind::Truncation, RECOVER_OPEN_AT_EOF, Some(Span::file(0x5eec, 0x14007 - 0x5eec))));
        let fails = node_failures(&i.view(&NodeKind::Root, &|| false), None, None);
        assert_eq!(failure_counts(&fails), Some((Status::Truncated, "1 incomplete record".into())), "{fails:?}");
        assert!(fails.iter().any(|f| f.diag.as_ref() == Some(d)), "root Diagnosis = record diagnostic");
        assert_eq!(rv.status_word, Some("Incomplete (fragment cut at EOF)"));
        let root = i.children(&NodeKind::Root, &|| false);
        assert_eq!(root[0].contains, Some(Status::Truncated), "the cut MIDDLE is physically truncated");
        assert!(root[0].label.ends_with("· 1 truncated fragment") && root[1].label.ends_with("· 1 incomplete record"), "{} / {}", root[0].label, root[1].label);
        let blocks = i.children(&root[0].kind, &|| false);
        assert_eq!(blocks.iter().map(|b| b.contains).collect::<Vec<_>>(), vec![None, None, Some(Status::Truncated)], "only the block holding the cut fragment");
        let b0 = i.children(&NodeKind::LogBlock(0), &|| false);
        assert!(b0.iter().all(|c| c.contains.is_none()));
        let b2 = i.children(&NodeKind::LogBlock(2), &|| false);
        assert_eq!((b2.len(), b2[0].status), (1, Status::Truncated), "the cut fragment keeps its physical status: {b2:?}");
        assert_eq!(label(&diagnosis(Status::Truncated, Some(d)), "Check").text, "File length (truncation)", "a cut fragment keeps the truncation check");
        // G5: one typed result for the block holding the cut fragment — tree node, block table
        // row, block view chip (+ title suffix) all say Truncated; the Type column keeps the type.
        assert_eq!((blocks[2].status, blocks[2].label.ends_with("· 1 truncated fragment")), (Status::Truncated, true), "{:?}", blocks[2]);
        assert!(blocks[..2].iter().all(|b| b.status == Status::Present));
        let bv = i.view(&NodeKind::LogBlock(2), &|| false);
        assert_eq!((bv.status, bv.title.as_str()), (Some(Status::Truncated), "Block 2 @0x10000 · 1 truncated fragment"));
        assert_eq!(bv.rows[0].cells[2], "3 MIDDLE", "{:?}", bv.rows[0].cells);
        assert_eq!(bv.rows[0].cells.last().map(String::as_str), Some("✂ Truncated"));
        let table = i.view(&root[0].kind, &|| false);
        assert_eq!(table.rows.iter().map(|r| r.field.status).collect::<Vec<_>>(), vec![Status::Present, Status::Present, Status::Truncated]);
        // Valid fragmented control.
        let mut i = crate::inspect::open(&fx("tests/fixtures/wal-fragmented/000004.log")).unwrap();
        let root = i.children(&NodeKind::Root, &|| false);
        assert!(root.iter().all(|c| c.contains.is_none()));
        assert_eq!(failure_counts(&node_failures(&i.view(&NodeKind::Root, &|| false), None, None)), None);
        assert!(i.children(&root[1].kind, &|| false).iter().all(|c| c.status == Status::Present && !c.label.contains("no LAST")));
    }

    /// Missing values are stated, never silently left out; Unsupported/Unknown are not Diagnosis.
    #[test]
    fn missing_values_and_non_corruption_statuses() {
        let g = diagnosis(Status::Invalid, None);
        assert_eq!(g.lines.iter().filter(|l| l.text == NOT_RECORDED).count(), 5);
        let d = Diagnostic::new(CheckKind::UnknownType, None, Trust::Partial, "", "unknown tag 99");
        let g = diagnosis(Status::Invalid, Some(&d));
        assert_eq!(label(&g, "Bytes").text, NOT_AVAILABLE);
        assert_eq!(label(&g, "Expected").text, NOT_AVAILABLE);
        assert_eq!(label(&g, RECOVERY_LABEL).text, NOT_RECORDED);
        assert!(matches!(finding(Status::Unsupported, None, "this codec"), Some(Finding::Unsupported(s)) if s.contains("may be valid")));
        assert!(matches!(finding(Status::Unknown, None, "x"), Some(Finding::Unknown(_))));
        assert_eq!(finding(Status::Present, None, "x"), None);
        // No physical cause is ever named.
        for l in diagnosis(Status::Invalid, Some(&d)).lines {
            for w in ["disk", "bit flip", "crash", "hardware"] {
                assert!(!l.text.contains(w), "{l:?}");
            }
        }
    }

    /// Reviewer finding 5: a restart-point entry with shared ≠ 0 carries a Structure diagnostic.
    #[test]
    fn restart_with_shared_prefix_is_a_structure_diagnostic() {
        use crate::model::{BlockKind, DecodedValue as DV};
        let e = crate::sst::block::Entry {
            idx: 3,
            off: 0x20,
            shared: 4,
            non_shared: 2,
            value_len_field: Some(1),
            hdr_len: 3,
            hdr_lens: [1, 1, 1],
            key_off: 0x23,
            key: b"abcdef".to_vec(),
            value_off: 0x25,
            value_len: 1,
            restart: true,
            handle: None,
            status: Status::Invalid,
            note: "restart point with non-zero shared length".into(),
            issue: Some(CheckKind::Structure),
        };
        let _ = BlockKind::Data;
        let mut s = crate::sst::SstInspector::open(crate::io::RangeReader::open(&fx("tests/fixtures/sst-none/data.sst")).unwrap());
        let h = s.footer().unwrap().metaindex;
        let b = s.block(h, BlockKind::Metaindex);
        let d = e.diagnostic(&b).expect("diagnostic");
        assert_eq!(d.check, CheckKind::Structure);
        assert_eq!((d.expected.clone(), d.observed.clone()), (Some(DV::U32(0)), Some(DV::U32(4))));
        assert_eq!(d.span.map(|s| s.len), Some(1));
        assert!(matches!(finding(Status::Invalid, Some(&d), "entry 3"), Some(Finding::Diagnosis(_))));
    }
}
