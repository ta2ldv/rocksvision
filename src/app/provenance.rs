//! Shared, pure range services over a `View` (egui-free): which field holds a byte
//! (`field_at`), where any span lives in the file (`file_ranges`), the Explanation "Where"
//! lines and the status-bar mirror. Panels never compute provenance themselves.
//!
//! File offset is the primary location. Reassembled record spaces map byte-for-byte to
//! fragment payloads (a field may need two or more file ranges); decompressed block spaces
//! have no byte mapping — a field there is only *contained* in the compressed stored range.

use crate::model::{FieldKind, FileRanges, Space, Span, View};

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

fn ranges_text(ranges: &[Span]) -> String {
    ranges.iter().map(|r| format!("[0x{:x},0x{:x})", r.offset, r.offset + r.len)).collect::<Vec<_>>().join(" + ")
}

/// Explanation "Where": one line per address space, File first.
pub fn where_lines(view: Option<&View>, span: &Span) -> Vec<String> {
    match file_ranges(view, span) {
        FileRanges::Exact { ranges, skipped } if span.space == Space::File => {
            let _ = skipped;
            vec![Space::File.where_line(ranges[0].offset, ranges[0].len)]
        }
        FileRanges::Exact { ranges, skipped } => {
            let n: u64 = ranges.iter().map(|r| r.len).sum();
            let file = if ranges.len() == 1 {
                Space::File.where_line(ranges[0].offset, ranges[0].len)
            } else {
                let hdr = if skipped.is_empty() { String::new() } else { format!(", fragment header {} skipped", ranges_text(&skipped)) };
                format!("File offset: {} {n} B — {} ranges{hdr}", ranges_text(&ranges), ranges.len())
            };
            vec![file, span.space.where_line(span.offset, span.len)]
        }
        FileRanges::Contained { file, codec } => vec![
            format!("Contained in compressed block: file [0x{:x},0x{:x}) {} B ({codec}); no byte-to-byte mapping to file offsets", file.offset, file.offset + file.len, file.len),
            span.space.where_line(span.offset, span.len),
        ],
        FileRanges::Unknown => vec!["File offset: not known for this space".into(), span.space.where_line(span.offset, span.len)],
    }
}

/// Status-bar mirror of a selection, e.g. "file [0xf,0x13) · rec@0x0 +0x8 · 4 B".
pub fn status_text(view: Option<&View>, span: &Span) -> String {
    let derived = match span.space {
        Space::File => None,
        Space::Block(o) => Some(format!("blk@0x{o:x} +0x{:x}", span.offset)),
        Space::Record(o) => Some(format!("rec@0x{o:x} +0x{:x}", span.offset)),
    };
    let file = match file_ranges(view, span) {
        FileRanges::Exact { ranges, .. } => format!("file {}", ranges_text(&ranges)),
        FileRanges::Contained { file, codec } => format!("in file [0x{:x},0x{:x}) ({codec}, no byte mapping)", file.offset, file.offset + file.len),
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
        // Record @0x7ff0 reassembled from FIRST payload [0x7ff7,0x8000) (9 B at rec+0) and LAST
        // payload [0x8007,0x8010) (9 B at rec+9); LAST's header [0x8000,0x8007) is skipped.
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
        assert_eq!(w[0], "File offset: [0x7ffc,0x8000) + [0x8007,0x8009) 6 B — 2 ranges, fragment header [0x8000,0x8007) skipped");
        assert_eq!(w[1], "Reassembled record offset: rec@0x7ff0 +0x5..+0xb");
        // Bytes on both sides of the boundary select "count"; header bytes do not.
        assert_eq!(field_at(&v, Space::File, 0x7ffd), Some(2));
        assert_eq!(field_at(&v, Space::File, 0x8008), Some(2));
        assert_eq!(field_at(&v, Space::File, 0x8003), None, "fragment header bytes are not a field");
        // Narrowest: seq (4 B) beats the fragment row (9 B) at 0x7ff8; the list row never answers.
        assert_eq!(field_at(&v, Space::File, 0x7ff8), Some(3));
        assert_eq!(field_at(&v, Space::File, 0x7ff0), None);
        assert_eq!(field_at(&v, Space::Record(0x7ff0), 6), Some(2));
        assert_eq!(status_text(Some(&v), &count), "file [0x7ffc,0x8000) + [0x8007,0x8009) · rec@0x7ff0 +0x5 · 6 B");
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
        assert_eq!(w, vec!["Contained in compressed block: file [0x5c9,0x62c) 99 B (ZSTD); no byte-to-byte mapping to file offsets".to_string(), "Decompressed block offset: blk@0x5c9 +0x6d, 103 B".to_string()]);
        for off in 0x5c9..0x62c {
            assert_eq!(field_at(&v, Space::File, off), None, "file byte 0x{off:x} must not map into the decompressed block");
        }
        assert_eq!(field_at(&v, sp, 0x70), Some(0));
    }
}
