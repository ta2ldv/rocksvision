//! Shared, pure range services over a `View` (egui-free): which field holds a byte
//! (`field_at`), where any span lives in the file (`file_ranges`), the Explanation "Where"
//! lines and the status-bar mirror. Panels never compute provenance themselves.
//!
//! File offset is the primary location. Reassembled record spaces map byte-for-byte to
//! fragment payloads (a field may need two or more file ranges); decompressed block spaces
//! have no byte mapping — a field there is only *contained* in the compressed stored range.

use crate::model::{FieldKind, FileRanges, NodeKind, Space, SpaceInfo, SpaceMap, Span, View, range_text};

/// File location of `span` in the context of `view` (which describes the derived spaces).
pub fn file_ranges(view: Option<&View>, span: &Span) -> FileRanges {
    if span.space == Space::File {
        return FileRanges::Exact { ranges: vec![*span], skipped: Vec::new() };
    }
    view.and_then(|v| v.spaces.iter().find(|s| s.space == span.space)).map(|s| s.map.file_ranges(span.offset, span.len)).unwrap_or(FileRanges::Unknown)
}

/// Exact file ranges of `span` (empty when there is no byte mapping).
pub fn exact_file_ranges(view: Option<&View>, span: &Span) -> Vec<Span> {
    match file_ranges(view, span) {
        FileRanges::Exact { ranges, .. } => ranges,
        _ => Vec::new(),
    }
}

/// Row of the narrowest meaningful field holding byte `off` of `space` (span order; on equal
/// length the first row wins). In file space, fields of a reassembled record count through
/// their exact file ranges. List rows that stand for a whole structure shown elsewhere
/// (`FieldKind::StructureRef`) never answer: the byte is then located inside that structure.
pub fn field_at(view: &View, space: Space, off: u64) -> Option<usize> {
    view.rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.field.kind != FieldKind::StructureRef)
        .filter_map(|(i, r)| {
            let s = r.field.span?;
            if s.contains(space, off) {
                return Some((i, s.len));
            }
            if space == Space::File && s.space != Space::File && exact_file_ranges(Some(view), &s).iter().any(|x| x.contains(Space::File, off)) {
                return Some((i, s.len));
            }
            None
        })
        .min_by_key(|(_, len)| *len)
        .map(|(i, _)| i)
}

/// What a byte-search hit selects in the current view (0020 D2): the one decision, applied by
/// `AppState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitTarget {
    /// The narrowest field holding the hit's first byte (`field_at`).
    Field(usize),
    /// Bytes inside the structure shown (its file extent holds the whole hit), not one field.
    Structure,
    /// Bytes outside the structure shown: a raw range, no row.
    Raw,
}

/// Where hit `hit` (file bytes) lands in `view`: a field holding its first byte, else the
/// structure shown when its extent holds the whole hit, else a raw range. A structure's name
/// is never given to bytes outside it.
pub fn hit_target(view: Option<&View>, hit: &Span) -> HitTarget {
    let Some(v) = view else { return HitTarget::Raw };
    if let Some(i) = field_at(v, hit.space, hit.offset) {
        return HitTarget::Field(i);
    }
    match v.extent {
        Some(e) if e.contains(hit.space, hit.offset) && hit.end() <= e.end() => HitTarget::Structure,
        _ => HitTarget::Raw,
    }
}

fn ranges_text(ranges: &[Span]) -> String {
    ranges.iter().map(|r| range_text(r.space, r.offset, r.end())).collect::<Vec<_>>().join(" + ")
}

/// Explanation "Where": one line per address space, File first, in the one range notation
/// ("file [0xf,0x13) · 4 B", "record@0x0 [+0x8,+0xc) · 4 B").
pub fn where_lines(view: Option<&View>, span: &Span) -> Vec<String> {
    match file_ranges(view, span) {
        FileRanges::Exact { ranges, skipped } if span.space == Space::File => {
            let _ = skipped;
            vec![ranges[0].range_text()]
        }
        FileRanges::Exact { ranges, skipped } => {
            let n: u64 = ranges.iter().map(|r| r.len).sum();
            let file = if ranges.len() == 1 {
                ranges[0].range_text()
            } else {
                let hdr = if skipped.is_empty() { String::new() } else { format!(", fragment header {} skipped", ranges_text(&skipped)) };
                format!("file {} · {n} B — {} ranges{hdr}", ranges_text(&ranges), ranges.len())
            };
            vec![file, span.range_text()]
        }
        FileRanges::Contained { file, codec } => vec![format!("Contained in compressed block: {} ({codec}); no byte-to-byte mapping to file offsets", file.range_text()), span.range_text()],
        FileRanges::Unknown => vec!["file: range not known for this space".into(), span.range_text()],
    }
}

/// 0020 §1: a whole-structure selection (no span, or the view's own extent) is described by the
/// view's location lines alone — their first line is the structure's range — so the Where
/// section shows one line, not the same range twice.
pub fn where_is_location(view: &View, span: Option<Span>) -> bool {
    !view.location.is_empty() && (span.is_none() || span == view.extent)
}

/// 0020 §4: Copy Offset text of the selected range, in the one range notation. The location
/// comes first, then `= file …` when the bytes map to one exact file range, and the provenance
/// qualifier is the last ` · ` segment:
/// - file: "file [0x6390c,0x648fa) · 4078 B"
/// - block, exact mapping: "block@0x5c9 [+0x6d,+0xd4) · 103 B = file [0x636,0x69d) · exact"
/// - compressed block: "block@0x5c9 [+0x6d,+0xd4) · 103 B · decompressed · compressed container
///   file [0x5c9,0x62c)"
/// - reassembled record: "record@0x0 [+0x8,+0xc) · 4 B = file [0xf,0x13) · reassembled" (no
///   `= file …` when the bytes are not one contiguous file range)
pub fn copy_offset_text(view: Option<&View>, span: &Span) -> String {
    let what = span.range_text();
    let derived = match span.space {
        Space::File => return what,
        Space::Block(_) => "decompressed",
        Space::Record(_) => "reassembled",
    };
    match file_ranges(view, span) {
        FileRanges::Exact { ranges, .. } if ranges.len() == 1 => {
            let qual = if matches!(span.space, Space::Block(_)) { "exact" } else { derived };
            format!("{what} = file {} · {qual}", range_text(Space::File, ranges[0].offset, ranges[0].end()))
        }
        FileRanges::Contained { file, .. } => format!("{what} · decompressed · compressed container file {}", range_text(Space::File, file.offset, file.end())),
        _ => format!("{what} · {derived}"),
    }
}

/// 0020 §5: Explanation "Where" of a table view node that has no range of its own (the view
/// lists structures; it is not bytes itself). None for other views.
pub fn view_where(kind: Option<&NodeKind>, view: &View, file_len: u64) -> Option<String> {
    match kind? {
        NodeKind::SstLayout => Some(format!("Whole file: {}. Select a row for its byte range.", Span::file(0, file_len).range_text())),
        NodeKind::SstDataBlocks { .. } => Some(match view.tally {
            Some(t) => format!("{} data blocks spread across the file (not one range). Select a row for its byte range.", t.total()),
            None => "Data blocks spread across the file (not one range). Select a row for its byte range.".into(),
        }),
        NodeKind::LogRecords { .. } => Some("Records reassembled from fragments across the file (not one range). Select a row for its byte range.".into()),
        _ => None,
    }
}

/// 0020 §5: what a table view node lists, first line of its tooltip after the label.
pub fn view_tooltip(kind: &NodeKind) -> Option<&'static str> {
    match kind {
        NodeKind::SstLayout => Some("Every structure in file order, gaps included. Select a row to see its bytes."),
        NodeKind::SstDataBlocks { .. } => Some("Data blocks in index (key) order, as the reader finds them. Select a row to see its bytes."),
        NodeKind::LogRecords { .. } => Some("Logical records in file order, reassembled from their fragments. Select a row to see its bytes."),
        _ => None,
    }
}

/// Where the byte view goes for a field without bytes of its own (design B2/B6): the first byte
/// of the enclosing structure — +0x0 of the view's derived space when the byte view already
/// shows it (BLOCK+/RECORD+ stay), else the structure's file start (`extent`). Never leaves File for
/// a derived space just for this. None when the view has no honest enclosing range.
pub fn byteless_reveal(view: Option<&View>, hex_space: Space) -> Option<Span> {
    let v = view?;
    if hex_space != Space::File && v.spaces.iter().any(|s| s.space == hex_space) {
        return Some(Span::new(hex_space, 0, 1));
    }
    v.extent.map(|e| Span::file(e.offset, 1))
}

/// The compressed container of a file range: the view's decompressed space whose stored file
/// range holds the start of `span` (0020 §2: one overflow-safe predicate, `Span::contains`).
/// None for derived-space spans and bytes outside every compressed range.
pub fn compressed_container<'a>(view: &'a View, span: &Span) -> Option<&'a SpaceInfo> {
    if span.space != Space::File {
        return None;
    }
    view.spaces.iter().find(|sp| matches!(&sp.map, SpaceMap::Compressed { file, .. } if file.contains(Space::File, span.offset)))
}

/// Where the byte view goes when field row's `span` is selected (not by a byte click): a field
/// without bytes shows the enclosing structure's first byte (`byteless_reveal`); a derived
/// field with exact file ranges (`extra`) shows its first file range unless the byte view
/// already shows the field's own derived space; otherwise the field's own span.
pub fn reveal_for_field(view: Option<&View>, span: Option<Span>, extra: &[Span], hex_space: Space) -> Option<Span> {
    let Some(s) = span else { return byteless_reveal(view, hex_space) };
    match extra.first() {
        Some(f) if hex_space == Space::File || hex_space != s.space => Some(*f),
        _ => Some(s),
    }
}

/// "file 0x5eec" / "block@0xd0 +0x0" / "record@0x5eec +0x0": a position shown by the byte view.
pub fn position_text(s: &Span) -> String {
    s.position_text()
}

/// Explanation "Where" of a field without bytes whose enclosing structure is shown (B4).
pub fn byteless_where(view: Option<&View>, shown: &Span) -> String {
    let what = view.map(|v| v.title.as_str()).unwrap_or("structure");
    format!("No bytes of its own (derived, inferred, default or metadata value). The hex view shows the start of the enclosing {what}: {}.", position_text(shown))
}

/// Status-bar mirror of a field without bytes (B5): "selection: reassembly · no bytes ·
/// showing file 0x5eec".
pub fn byteless_status(name: &str, shown: &Span) -> String {
    format!("selection: {name} · no bytes · showing {}", position_text(shown))
}

/// Status-bar mirror of a selection, e.g. "file [0xf,0x13) · record@0x0 [+0x8,+0xc) · 4 B".
pub fn status_text(view: Option<&View>, span: &Span) -> String {
    let derived = (span.space != Space::File).then(|| format!("{} {}", span.space.name(), range_text(span.space, span.offset, span.end())));
    let file = match file_ranges(view, span) {
        FileRanges::Exact { ranges, .. } => format!("file {}", ranges_text(&ranges)),
        FileRanges::Contained { file, codec } => format!("in file {} ({codec}, no byte mapping)", range_text(Space::File, file.offset, file.end())),
        FileRanges::Unknown => "file range unknown".into(),
    };
    match derived {
        Some(d) => format!("{file} · {d} · {} B", span.len),
        None => format!("{file} · {} B", span.len),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Field, Row, SpaceInfo, SpaceMap, View};

    fn rec_view() -> View {
        // Record @0x7ff0 reassembled from FIRST payload [0x7ff7,0x8000) (9 B at record+0) and LAST
        // payload [0x8007,0x8010) (9 B at record+9); LAST's header [0x8000,0x8007) is skipped.
        let sp = Space::Record(0x7ff0);
        let map = SpaceMap::Fragments { segs: vec![(0, 0x7ff7, 9), (9, 0x8007, 9)], headers: vec![Span::file(0x8000, 7)] };
        let row = |name: &str, s: Span| Row { cells: vec![name.into()], field: Field::new(name, Some(s), "") };
        let mut list = Field::new("record 0", Some(Span::file(0x7ff0, 0x20)), "");
        list.kind = FieldKind::StructureRef;
        View {
            rows: vec![Row { cells: vec![], field: list }, row("fragment 0", Span::file(0x7ff7, 9)), row("count", Span::new(sp, 5, 6)), row("seq", Span::new(sp, 0, 4))],
            spaces: vec![SpaceInfo { space: sp, size: 18, title: String::new(), map }],
            ..Default::default()
        }
    }

    /// A field crossing a fragment boundary has two exact file ranges; the header between
    /// them is skipped; a byte of either range selects the field (narrowest wins).
    #[test]
    fn cross_fragment_field_maps_to_two_file_ranges() {
        let v = rec_view();
        let count = Span::new(Space::Record(0x7ff0), 5, 6);
        match file_ranges(Some(&v), &count) {
            FileRanges::Exact { ranges, skipped } => {
                assert_eq!(ranges, vec![Span::file(0x7ffc, 4), Span::file(0x8007, 2)]);
                assert_eq!(skipped, vec![Span::file(0x8000, 7)]);
            }
            other => panic!("{other:?}"),
        }
        let w = where_lines(Some(&v), &count);
        assert_eq!(w[0], "file [0x7ffc,0x8000) + [0x8007,0x8009) · 6 B — 2 ranges, fragment header [0x8000,0x8007) skipped");
        assert_eq!(w[1], "record@0x7ff0 [+0x5,+0xb) · 6 B");
        // Bytes on both sides of the boundary select "count"; header bytes do not.
        assert_eq!(field_at(&v, Space::File, 0x7ffd), Some(2));
        assert_eq!(field_at(&v, Space::File, 0x8008), Some(2));
        assert_eq!(field_at(&v, Space::File, 0x8003), None, "fragment header bytes are not a field");
        // Narrowest: seq (4 B) beats the fragment row (9 B) at 0x7ff8; the list row never answers.
        assert_eq!(field_at(&v, Space::File, 0x7ff8), Some(3));
        assert_eq!(field_at(&v, Space::File, 0x7ff0), None);
        assert_eq!(field_at(&v, Space::Record(0x7ff0), 6), Some(2));
        assert_eq!(status_text(Some(&v), &count), "file [0x7ffc,0x8000) + [0x8007,0x8009) · record@0x7ff0 [+0x5,+0xb) · 6 B");
        // 0019 §2: one notation; a file span is named and counted the same way everywhere.
        let f = Span::file(0x6390c, 4078);
        assert_eq!(where_lines(None, &f), vec!["file [0x6390c,0x648fa) · 4078 B".to_string()]);
        assert_eq!(status_text(None, &f), "file [0x6390c,0x648fa) · 4078 B");
        assert_eq!(crate::app::diagnosis::span_text(&f), "file [0x6390c,0x648fa) · 4078 B");
        assert_eq!(position_text(&Span::new(Space::Block(0x5c9), 0x6d, 1)), "block@0x5c9 +0x6d");
    }

    /// 0020 §4: Copy Offset text per space; the qualifier is the last segment.
    #[test]
    fn copy_offset_text_names_space_mapping_and_provenance() {
        let v = rec_view();
        let sp = Space::Record(0x7ff0);
        assert_eq!(copy_offset_text(None, &Span::file(0x6390c, 4078)), "file [0x6390c,0x648fa) · 4078 B");
        assert_eq!(copy_offset_text(Some(&v), &Span::new(sp, 0, 4)), "record@0x7ff0 [+0x0,+0x4) · 4 B = file [0x7ff7,0x7ffb) · reassembled");
        assert_eq!(copy_offset_text(Some(&v), &Span::new(sp, 5, 6)), "record@0x7ff0 [+0x5,+0xb) · 6 B · reassembled", "two file ranges: no '= file'");
        assert_eq!(copy_offset_text(None, &Span::new(sp, 5, 6)), "record@0x7ff0 [+0x5,+0xb) · 6 B · reassembled", "origin unknown");
        // A block space with an exact byte mapping (uncompressed contents described as a space).
        let b = Space::Block(0x100);
        let exact = View { spaces: vec![SpaceInfo { space: b, size: 0x40, title: String::new(), map: SpaceMap::Fragments { segs: vec![(0, 0x100, 0x40)], headers: vec![] } }], ..Default::default() };
        assert_eq!(copy_offset_text(Some(&exact), &Span::new(b, 0x6d - 0x60, 3)), "block@0x100 [+0xd,+0x10) · 3 B = file [0x10d,0x110) · exact");
        let zstd = View {
            spaces: vec![SpaceInfo { space: Space::Block(0x5c9), size: 323, title: String::new(), map: SpaceMap::Compressed { file: Span::file(0x5c9, 99), codec: "ZSTD".into() } }],
            ..Default::default()
        };
        assert_eq!(copy_offset_text(Some(&zstd), &Span::new(Space::Block(0x5c9), 0x6d, 103)), "block@0x5c9 [+0x6d,+0xd4) · 103 B · decompressed · compressed container file [0x5c9,0x62c)");
        assert!(where_is_location(
            &View { location: vec![crate::model::WhereLine { text: "file [0x0,0x5) · 5 B".into(), link: None }], extent: Some(Span::file(0, 5)), ..Default::default() },
            Some(Span::file(0, 5))
        ));
    }

    /// Compressed SST fields: decompressed offset + containing compressed block range, never a
    /// byte mapping (no file byte selects them).
    #[test]
    fn compressed_provenance_has_no_byte_mapping() {
        let sp = Space::Block(0x5c9);
        let v = View {
            rows: vec![Row { cells: vec![], field: Field::new("entry 3", Some(Span::new(sp, 0x6d, 103)), "") }],
            spaces: vec![SpaceInfo { space: sp, size: 323, title: String::new(), map: SpaceMap::Compressed { file: Span::file(0x5c9, 99), codec: "ZSTD".into() } }],
            ..Default::default()
        };
        let s = Span::new(sp, 0x6d, 103);
        assert_eq!(file_ranges(Some(&v), &s), FileRanges::Contained { file: Span::file(0x5c9, 99), codec: "ZSTD".into() });
        assert!(exact_file_ranges(Some(&v), &s).is_empty());
        let w = where_lines(Some(&v), &s);
        assert_eq!(w, vec!["Contained in compressed block: file [0x5c9,0x62c) · 99 B (ZSTD); no byte-to-byte mapping to file offsets".to_string(), "block@0x5c9 [+0x6d,+0xd4) · 103 B".to_string()]);
        for off in 0x5c9..0x62c {
            assert_eq!(field_at(&v, Space::File, off), None, "file byte 0x{off:x} must not map into the decompressed block");
        }
        assert_eq!(field_at(&v, sp, 0x70), Some(0));
    }
}
