//! Layout configuration and pure geometry: window/panel/table geometry that Reset View
//! restores, the 16/8/4 bytes-per-row policy and byte hit-testing, group wrapping (toolbar, hex
//! header), the status-bar width budget, the minimum layout the zoom ceiling needs, and table
//! column widths by semantic `ColumnRole`. Everything here is egui-free and unit-tested; the
//! zoom policy itself (bounds/step/ceiling) is owned by the app layer (`app::ZoomPolicy`).
//! Panels are never hidden automatically: narrowing the window shrinks the hex rows first
//! (16 → 8 → 4), then the Explanation panel, then Navigation, down to their minimums.

use crate::model::ColumnRole;

pub struct LayoutDefaults {
    pub window_size: [f32; 2],
    pub window_min: [f32; 2],
    pub left_default: f32,
    pub left_min: f32,
    pub left_max: f32,
    pub right_default: f32,
    pub right_min: f32,
    pub right_max: f32,
    /// Center column inner margin (each side) of the records and bytes areas.
    pub center_margin: [f32; 2],
    /// Width a side panel's resize separator takes (line + grab tolerance), per panel.
    pub separator: f32,
    /// Records minimum in table row heights (title line, header, one row).
    pub records_min_rows: f32,
    /// Byte rows that must stay visible at the minimum height / zoom ceiling.
    pub bytes_min_rows: f32,
    /// Hit strip of the center split handle.
    pub split_handle: f32,
    /// Slack added to the width/height the minimum layout needs (frames, rounding).
    pub layout_margin_w: f32,
    pub layout_margin_h: f32,
    /// Status bar vertical padding (one text line).
    pub status_pad: f32,
    /// File list share of the navigation panel height.
    pub file_list_share: f32,
    pub tree_indent: f32,
    pub path_min: f32,
    /// Initial width estimate of the toolbar groups (measured from the first frame on).
    pub toolbar_reserved: f32,
    pub search_width: f32,
    pub goto_width: f32,
    /// Minimum size of a glyph-only button (compact hex paging "◀" / "▶").
    pub glyph_button: [f32; 2],
    pub progress_width: f32,
    /// Vertical padding added to the font height for table rows / headers.
    pub row_pad: f32,
    pub header_pad: f32,
    /// Minimum width of the remainder (Decoded / widest text) column.
    pub remainder_min: f32,
    pub column_min: f32,
    pub selected_stripe: f32,
    pub lsm_box_max_height: f32,
    pub about_logo: f32,
    pub about_width: f32,
    pub help_width: f32,
    pub swatch: f32,
    /// Width of the two centered open buttons on the empty start screen.
    pub start_button_width: f32,
    /// Status-bar segments narrower than this many glyphs are dropped (hint) or kept at it.
    pub status_min_glyphs: f32,
}

pub const LAYOUT: &LayoutDefaults = &LayoutDefaults {
    window_size: [1400.0, 900.0],
    window_min: [900.0, 560.0],
    left_default: 260.0,
    left_min: 180.0,
    left_max: 420.0,
    right_default: 320.0,
    right_min: 200.0,
    right_max: 520.0,
    center_margin: [8.0, 4.0],
    separator: 4.0,
    records_min_rows: 3.0,
    bytes_min_rows: 3.0,
    split_handle: 6.0,
    layout_margin_w: 16.0,
    layout_margin_h: 16.0,
    status_pad: 3.0,
    file_list_share: 0.35,
    tree_indent: 14.0,
    path_min: 160.0,
    toolbar_reserved: 900.0,
    search_width: 220.0,
    goto_width: 170.0,
    glyph_button: [24.0, 20.0],
    progress_width: 160.0,
    row_pad: 8.0,
    header_pad: 10.0,
    remainder_min: 160.0,
    column_min: 40.0,
    selected_stripe: 3.0,
    lsm_box_max_height: 220.0,
    about_logo: 128.0,
    about_width: 360.0,
    help_width: 520.0,
    swatch: 10.0,
    start_button_width: 140.0,
    status_min_glyphs: 6.0,
};

/// Initial width (points) of a table column by its semantic role. The fill column (first
/// role with `ColumnRole::fills` in display order) gets the rest, at least `remainder_min`.
pub fn column_width(role: ColumnRole) -> f32 {
    use ColumnRole as R;
    match role {
        R::Index => 46.0,
        R::Status => 96.0,
        R::Offset => 76.0,
        R::Len => 44.0,
        R::Field => 150.0,
        R::Decoded => 300.0,
        R::Encoding => 80.0,
        R::RawHex => 100.0,
        R::Kind => 170.0,
        R::Header => 150.0,
        R::Seq => 80.0,
        R::Count => 56.0,
        R::Cf => 40.0,
        R::CfName => 110.0,
        R::Level => 50.0,
        R::FileNo => 70.0,
        R::Size => 80.0,
        R::Key => 200.0,
        R::Value => 220.0,
        R::Crc => 90.0,
        R::Handle => 200.0,
        R::ValueType => 120.0,
        R::Flag => 24.0,
        R::Presence => 80.0,
        R::Summary => 420.0,
        R::Name => 260.0,
        R::Text => 900.0,
    }
}

/// Splits the center height `h` (handle excluded) into (records, bytes) heights by `ratio`
/// (records share), respecting both minimums. If the center is shorter than both minimums,
/// the bytes keep their minimum and the records get the rest (the table scrolls).
pub fn split_heights(h: f32, ratio: f32, rec_min: f32, hex_min: f32) -> (f32, f32) {
    let h = h.max(0.0);
    if h <= rec_min + hex_min {
        let hex = hex_min.min(h);
        return (h - hex, hex);
    }
    let rec = (h * ratio.clamp(0.0, 1.0)).clamp(rec_min, h - hex_min);
    (rec, h - rec)
}

// ------------------------------------------------------------------ hex rows (spec §2)

/// Digits of the offset gutter: max(8, hex digits of the largest offset in the space).
pub fn offset_digits(size: u64) -> u64 {
    let last = size.saturating_sub(1);
    let d = (64 - u64::from(last.leading_zeros())).div_ceil(4);
    d.max(8)
}

fn half_gap(n: u64) -> u64 {
    u64::from(n >= 16)
}

/// Row width in glyphs: W(n) = d + 2 + 3n + gap(n) + 1 + max(n, 5) (gap(16) = 1; the max keeps
/// the "ASCII" header inside the row at 4 bytes per row).
pub fn hex_row_chars(n: u64, d: u64) -> u64 {
    d + 2 + 3 * n + half_gap(n) + 1 + n.max(5)
}

/// Glyph column of byte `i` (of `n` per row) in the hex area, from the row start.
pub fn byte_col(i: u64, n: u64, d: u64) -> u64 {
    d + 2 + 3 * i + if n >= 16 && i >= n / 2 { 1 } else { 0 }
}

/// Glyph column of byte `i` in the ASCII area.
pub fn ascii_col(i: u64, n: u64, d: u64) -> u64 {
    d + 2 + 3 * n + half_gap(n) + 1 + i
}

/// Byte index (0..n) under glyph column `col` (fractional, from the row start), in the hex
/// cells (each byte with half a space on both sides) or the ASCII cells. None in the gutter
/// and gaps. Deterministic: the same position always maps to the same byte.
pub fn byte_at(col: f32, n: u64, d: u64) -> Option<u64> {
    let a0 = ascii_col(0, n, d) as f32;
    if col >= a0 && col < a0 + n as f32 {
        return Some(((col - a0).floor() as u64).min(n - 1));
    }
    (0..n).find(|&i| {
        let c = byte_col(i, n, d) as f32;
        col >= c - 0.5 && col < c + 2.5
    })
}

/// Measured metrics of the byte rows: glyph advance, fixed padding (row scrollbar + item
/// spacing) and offset digits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HexMetrics {
    pub cw: f32,
    pub pad: f32,
    pub digits: u64,
}

impl HexMetrics {
    /// Pixel width of a row block with `n` bytes per row.
    pub fn width(&self, n: u64) -> f32 {
        hex_row_chars(n, self.digits) as f32 * self.cw + self.pad
    }
}

/// Bytes per row (16 / 8 / 4) for the available width, with hysteresis: step down while the
/// row does not fit; step up to 2n only with two glyphs to spare. At fixed metrics, a narrower
/// width never yields more bytes per row. The floor is 4.
pub fn choose_bytes_per_row(current: u64, avail: f32, m: &HexMetrics) -> u64 {
    let mut n = if matches!(current, 4 | 8 | 16) { current } else { 16 };
    while n > 4 && avail < m.width(n) {
        n /= 2;
    }
    while n < 16 && avail >= m.width(n * 2) + 2.0 * m.cw {
        n *= 2;
    }
    n
}

/// Width the center column needs at minimum: 4 bytes per row plus its margins.
pub fn center_min(m: &HexMetrics) -> f32 {
    m.width(4) + 2.0 * LAYOUT.center_margin[0]
}

/// Maximum widths of the side panels so the center keeps `center_min`: the Explanation panel
/// shrinks first (it may take only what Navigation leaves), then Navigation (it must leave the
/// Explanation minimum). `left`/`right`: Some(min, max) of a shown panel, None = collapsed strip
/// of width `strip`. Returns (left_max, right_max).
pub fn side_max(window_w: f32, center: f32, strip: f32, left: Option<(f32, f32)>, left_actual: f32, right: Option<(f32, f32)>) -> (f32, f32) {
    let sep = LAYOUT.separator;
    let right_floor = right.map(|r| r.0 + sep).unwrap_or(strip);
    let left_max = left.map(|(mn, mx)| (window_w - center - right_floor - sep).clamp(mn, mx)).unwrap_or(strip);
    let left_used = if left.is_some() { left_actual + sep } else { strip };
    let right_max = right.map(|(mn, mx)| (window_w - center - left_used - sep).clamp(mn, mx)).unwrap_or(strip);
    (left_max, right_max)
}

// ------------------------------------------------------------------ wrapping groups

/// Safety margin of `pack_rows` (sub-point rounding of measured widths, frame strokes).
pub const PACK_SLACK: f32 = 2.0;

/// Greedy row packing of atomic groups: index of the first group of every row. A row keeps
/// `PACK_SLACK` points free so rounding never pushes its last group past the edge.
pub fn pack_rows(widths: &[f32], gap: f32, avail: f32) -> Vec<usize> {
    let avail = avail - PACK_SLACK;
    let mut starts = Vec::new();
    let mut x = f32::INFINITY;
    for (i, w) in widths.iter().enumerate() {
        if x.is_infinite() || x + gap + w > avail {
            starts.push(i);
            x = *w;
        } else {
            x += gap + w;
        }
    }
    starts
}

/// Line of each hex header group A (mode + space buttons), B (paging), C (go to): A+B+C on one
/// line, otherwise A / B+C, otherwise A / B / C (never words of one group across lines).
pub fn hex_header_lines(w: [f32; 3], gap: f32, avail: f32) -> [usize; 3] {
    if w[0] + w[1] + w[2] + 2.0 * gap <= avail {
        [0, 0, 0]
    } else if w[1] + w[2] + gap <= avail {
        [0, 1, 1]
    } else {
        [0, 1, 2]
    }
}

// ------------------------------------------------------------------ Diagnosis grid (spec §12)

/// Below this Explanation width the Diagnosis grid stacks each label above its value.
pub const DIAG_SIDE_MIN: f32 = 320.0;

/// Label column beside the values only when the panel is at least `DIAG_SIDE_MIN` wide and the
/// widest unbreakable value fits beside the label column; otherwise label above, value below.
pub fn diag_side_by_side(avail: f32, label_w: f32, widest_value: f32) -> bool {
    avail >= DIAG_SIDE_MIN && label_w + widest_value <= avail
}

// ------------------------------------------------------------------ status bar (spec §5)

/// Widths of (message, file label, hint) next to a selection segment that is never truncated.
/// Truncation order: hint first (dropped below `min`), then the message (down to `min`), then
/// the file label.
pub fn status_widths(avail: f32, sel: f32, msg: f32, file: f32, hint: f32, min: f32) -> (f32, f32, f32) {
    let rest = (avail - sel).max(0.0);
    if msg + file + hint <= rest {
        return (msg, file, hint);
    }
    let h = rest - msg - file;
    let hint = if h >= min { h } else { 0.0 };
    if msg + file + hint <= rest {
        return (msg, file, hint);
    }
    let m = (rest - file).max(msg.min(min)).min(msg);
    let f = (rest - m).clamp(0.0, file);
    (m, f, 0.0)
}

// ------------------------------------------------------------------ zoom ceiling (spec §8)

/// Measurements (logical points, zoom-independent) of the parts the minimum layout consists of.
#[derive(Clone, Debug, PartialEq)]
pub struct Measured {
    pub nav_min: f32,
    pub explain_min: f32,
    /// Width of a collapsed side-panel strip.
    pub strip: f32,
    pub hex: HexMetrics,
    /// Toolbar groups (path group at its minimum), gap, one row's height, vertical frame.
    pub toolbar_groups: Vec<f32>,
    pub toolbar_gap: f32,
    pub toolbar_row_h: f32,
    pub toolbar_pad_h: f32,
    pub status_h: f32,
    /// Records minimum: title line + table header + one row (+ one note line).
    pub records_min_h: f32,
    /// Hex header groups A/B/C, one header line, the ruler, one byte row, area padding.
    pub hex_groups: [f32; 3],
    pub hex_gap: f32,
    pub hex_line_h: f32,
    pub hex_ruler_h: f32,
    pub hex_row_h: f32,
    pub hex_pad_h: f32,
}

impl Measured {
    fn panels_w(&self, nav: bool, explain: bool) -> f32 {
        let side = |shown: bool, min: f32| if shown { min + LAYOUT.separator } else { self.strip };
        side(nav, self.nav_min) + side(explain, self.explain_min) + center_min(&self.hex)
    }
    fn toolbar_h(&self, w: f32) -> f32 {
        let rows = pack_rows(&self.toolbar_groups, self.toolbar_gap, w - 2.0 * LAYOUT.center_margin[0]).len().max(1);
        rows as f32 * self.toolbar_row_h + self.toolbar_pad_h
    }
    /// Height of the byte region with the hex header wrapped at `center_w`.
    pub fn bytes_h(&self, center_w: f32) -> f32 {
        let lines = hex_header_lines(self.hex_groups, self.hex_gap, center_w - 2.0 * LAYOUT.center_margin[0]).iter().max().copied().unwrap_or(0) + 1;
        lines as f32 * self.hex_line_h + self.hex_ruler_h + LAYOUT.bytes_min_rows * self.hex_row_h + self.hex_pad_h
    }
    /// Window width the widest toolbar group needs on a row of its own (frame margins + slack).
    fn widest_toolbar_group(&self) -> f32 {
        self.toolbar_groups.iter().copied().fold(0.0, f32::max) + 2.0 * LAYOUT.center_margin[0] + PACK_SLACK
    }
    /// Height at window width `w` with the panels at their minimum (the center gets the rest).
    fn height_at(&self, w: f32, nav: bool, explain: bool) -> f32 {
        let center_w = (w - self.panels_w(nav, explain) + center_min(&self.hex)).max(center_min(&self.hex));
        self.toolbar_h(w) + self.status_h + self.records_min_h + LAYOUT.split_handle + self.bytes_h(center_w) + LAYOUT.layout_margin_h
    }

    /// (need_w, need_h) of the minimum layout with the given panel visibility. The height is
    /// taken at width `need_w` (most toolbar rows / header lines), so a window that is at least
    /// `need_w` wide never needs more: the linear zoom ceiling built on it cannot clip.
    pub fn need(&self, nav: bool, explain: bool) -> (f32, f32) {
        let widest = self.widest_toolbar_group();
        let w = self.panels_w(nav, explain).max(widest) + LAYOUT.layout_margin_w;
        (w, self.height_at(w, nav, explain))
    }

    /// Acceptance rule (spec §8.2) at a window of `w`×`h` points: every toolbar group fits the
    /// width, the side panels at their minimum leave the center ≥ 4 bytes per row without
    /// horizontal scrolling, and the toolbar + status + records minimum + bytes (header, ruler,
    /// three byte rows) fit the height.
    pub fn fits(&self, w: f32, h: f32, nav: bool, explain: bool) -> bool {
        let widest = self.widest_toolbar_group();
        widest <= w && self.panels_w(nav, explain) + LAYOUT.layout_margin_w <= w && self.height_at(w, nav, explain) <= h
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ZoomPolicy;
    use crate::app::ZoomFit;

    /// Fix C2: a ~300 pt Explanation (1400 px, 100 %) stacks; a wide panel keeps two columns
    /// only while the widest value fits beside the labels.
    #[test]
    fn diagnosis_grid_stacks_when_narrow_or_values_do_not_fit() {
        assert!(!diag_side_by_side(300.0, 110.0, 80.0));
        assert!(!diag_side_by_side(319.9, 0.0, 0.0));
        assert!(diag_side_by_side(400.0, 110.0, 200.0));
        assert!(!diag_side_by_side(400.0, 110.0, 300.0));
    }

    #[test]
    fn split_respects_ratio_and_minimums() {
        assert_eq!(split_heights(1000.0, 0.42, 75.0, 150.0), (420.0, 580.0));
        assert_eq!(split_heights(1000.0, 0.0, 75.0, 150.0), (75.0, 925.0));
        assert_eq!(split_heights(1000.0, 1.0, 75.0, 150.0), (850.0, 150.0));
        // Too short for both: bytes keep their minimum, records get the rest.
        assert_eq!(split_heights(200.0, 0.42, 75.0, 150.0), (50.0, 150.0));
        assert_eq!(split_heights(100.0, 0.42, 75.0, 150.0), (0.0, 100.0));
    }

    #[test]
    fn row_width_formula() {
        assert_eq!([16, 8, 4].map(|n| hex_row_chars(n, 8)), [76, 43, 28]);
        assert_eq!(offset_digits(0), 8);
        assert_eq!(offset_digits(0x1_0000_0000), 8);
        assert_eq!(offset_digits(0x1_0000_0001), 9);
    }

    const M: HexMetrics = HexMetrics { cw: 8.0, pad: 30.0, digits: 8 };

    /// Both directions at fixed metrics: shrinking never increases bytes per row; widening
    /// steps up only with the hysteresis margin; the floor is 4.
    #[test]
    fn bytes_per_row_policy_is_monotonic_with_hysteresis() {
        let mut n = 16;
        let mut prev = 16;
        let mut w = 900.0;
        while w > 100.0 {
            n = choose_bytes_per_row(n, w, &M);
            assert!(n <= prev, "shrinking to {w} increased bytes/row {prev} → {n}");
            assert!(matches!(n, 4 | 8 | 16));
            prev = n;
            w -= 3.0;
        }
        assert_eq!(n, 4, "floor is 4");
        let mut seen = vec![n];
        while w < 900.0 {
            let m = choose_bytes_per_row(n, w, &M);
            assert!(m >= n, "widening to {w} decreased bytes/row");
            if m != n {
                seen.push(m);
                // Stepped up only with two glyphs to spare.
                assert!(w >= M.width(m) + 2.0 * M.cw, "{w} {m}");
            }
            n = m;
            w += 3.0;
        }
        assert_eq!(seen, vec![4, 8, 16]);
        // Exactly at the 16-byte width it stays 16; just below it goes to 8; back to 16 only
        // with the margin.
        assert_eq!(choose_bytes_per_row(16, M.width(16), &M), 16);
        assert_eq!(choose_bytes_per_row(16, M.width(16) - 0.5, &M), 8);
        assert_eq!(choose_bytes_per_row(8, M.width(16) + 1.0, &M), 8);
        assert_eq!(choose_bytes_per_row(8, M.width(16) + 2.0 * M.cw, &M), 16);
    }

    /// Byte hit-testing is byte-absolute and deterministic for every layout.
    #[test]
    fn byte_hit_test_maps_hex_and_ascii_cells() {
        for n in [16u64, 8, 4] {
            for i in 0..n {
                let c = byte_col(i, n, 8) as f32;
                assert_eq!(byte_at(c, n, 8), Some(i));
                assert_eq!(byte_at(c + 1.9, n, 8), Some(i));
                assert_eq!(byte_at(ascii_col(i, n, 8) as f32 + 0.5, n, 8), Some(i));
            }
            assert_eq!(byte_at(2.0, n, 8), None, "gutter");
            assert_eq!(byte_at(hex_row_chars(n, 8) as f32 + 3.0, n, 8), None, "past the row");
        }
        // Columns of the 16-byte row match the rendered text "xx xx … xx  xx …".
        assert_eq!(byte_col(7, 16, 8), 10 + 21);
        assert_eq!(byte_col(8, 16, 8), 10 + 25);
        assert_eq!(ascii_col(0, 16, 8), 10 + 48 + 1 + 1);
    }

    #[test]
    fn hex_header_groups_wrap_as_units() {
        assert_eq!(hex_header_lines([100.0, 200.0, 150.0], 8.0, 500.0), [0, 0, 0]);
        assert_eq!(hex_header_lines([100.0, 200.0, 150.0], 8.0, 400.0), [0, 1, 1]);
        assert_eq!(hex_header_lines([100.0, 200.0, 150.0], 8.0, 300.0), [0, 1, 2]);
        assert_eq!(pack_rows(&[100.0, 200.0, 300.0], 10.0, 320.0), vec![0, 2]);
        assert_eq!(pack_rows(&[500.0, 10.0], 10.0, 320.0), vec![0, 1]);
    }

    /// The selection is never truncated; the hint goes first, then the message, then the file.
    #[test]
    fn status_budget_truncation_order() {
        assert_eq!(status_widths(1000.0, 200.0, 300.0, 200.0, 100.0, 48.0), (300.0, 200.0, 100.0));
        assert_eq!(status_widths(760.0, 200.0, 300.0, 200.0, 100.0, 48.0), (300.0, 200.0, 60.0));
        assert_eq!(status_widths(730.0, 200.0, 300.0, 200.0, 100.0, 48.0), (300.0, 200.0, 0.0), "hint below the minimum is dropped");
        assert_eq!(status_widths(600.0, 200.0, 300.0, 200.0, 100.0, 48.0), (200.0, 200.0, 0.0), "message truncates before the file label");
        let (m, f, h) = status_widths(300.0, 200.0, 300.0, 200.0, 100.0, 48.0);
        assert_eq!((m, h), (48.0, 0.0));
        assert!((f - 52.0).abs() < 1e-3);
        assert_eq!(status_widths(150.0, 200.0, 300.0, 200.0, 100.0, 48.0).2, 0.0);
    }

    /// Metrics as the default theme measures them at 100 % (body 14.5 pt, mono 13.5 pt, toolbar
    /// group widths from the label texts). Measured with egui's bundled fonts.
    fn measured() -> Measured {
        use eframe::egui::{self, FontId};
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |_| {});
        let tw = |s: &str, size: f32| ctx.fonts_mut(|f| f.layout_no_wrap(s.into(), FontId::proportional(size), egui::Color32::WHITE).size().x);
        let cw = ctx.fonts_mut(|f| f.glyph_width(&FontId::monospace(13.5), '0'));
        let line = ctx.fonts_mut(|f| f.row_height(&FontId::proportional(14.5)));
        let mono_line = ctx.fonts_mut(|f| f.row_height(&FontId::monospace(13.5)));
        let sp = egui::Spacing::default();
        let (bp, gap) = (2.0 * sp.button_padding.x, sp.item_spacing.x);
        let btn = |s: &str| tw(s, 14.5) + bp;
        let group = |items: &[f32]| items.iter().sum::<f32>() + gap * (items.len() as f32 - 1.0);
        let toolbar_groups = vec![
            group(&[btn("📂 Open Folder…"), btn("📄 Open File…")]),
            group(&[LAYOUT.path_min, btn("Go")]),
            group(&[LAYOUT.search_width, sp.icon_width + tw("Hex bytes", 14.5) + gap, btn("Prev"), btn("Next"), btn("Clear search")]),
            btn("Copy…"),
            group(&[btn("A−"), tw("100%", 14.5), btn("A+"), btn("Reset View")]),
        ];
        let small = |s: &str| tw(s, 12.0) + bp;
        let hex_groups = [
            group(&[tw("Decompressed block", 12.0) + 8.0, small("show file bytes"), small("Hide bytes")]),
            group(&[btn("◀ page"), tw("page 1/1 · 0x0..0x10000 of 65536 bytes", 14.5), btn("page ▶")]),
            group(&[LAYOUT.goto_width, btn("Go to")]),
        ];
        let rh = line + LAYOUT.row_pad;
        let ctl = sp.interact_size.y.max(line + 2.0 * sp.button_padding.y) + sp.item_spacing.y;
        Measured {
            nav_min: LAYOUT.left_min,
            explain_min: LAYOUT.right_min,
            strip: line + LAYOUT.header_pad,
            hex: HexMetrics { cw, pad: 10.0 + 2.0 * gap, digits: 8 },
            toolbar_groups,
            toolbar_gap: gap,
            toolbar_row_h: ctl,
            toolbar_pad_h: 8.0,
            status_h: line + LAYOUT.status_pad + 8.0,
            records_min_h: LAYOUT.records_min_rows * rh + line,
            hex_groups,
            hex_gap: gap,
            hex_line_h: ctl,
            hex_ruler_h: mono_line + 2.0,
            hex_row_h: mono_line + 2.0,
            hex_pad_h: 2.0 * LAYOUT.center_margin[1] + sp.scroll.allocated_width(),
        }
    }

    /// §1.6: at the minimum window and 100 % both side panels at their minimum and the center
    /// with 4 bytes per row fit side by side.
    #[test]
    fn minimum_window_fits_both_panels_and_four_byte_rows() {
        let m = measured();
        let w = m.nav_min + m.explain_min + center_min(&m.hex) + 2.0 * LAYOUT.separator;
        assert!(w <= LAYOUT.window_min[0], "{w} > {}", LAYOUT.window_min[0]);
        assert!(m.fits(LAYOUT.window_min[0], LAYOUT.window_min[1], true, true), "minimum window at 100 % does not fit");
        // Panels clamp so the center keeps 4 bytes per row; nothing overlaps.
        let (lmax, rmax) = side_max(LAYOUT.window_min[0], center_min(&m.hex), m.strip, Some((LAYOUT.left_min, LAYOUT.left_max)), LAYOUT.left_default, Some((LAYOUT.right_min, LAYOUT.right_max)));
        assert!(lmax >= LAYOUT.left_min && rmax >= LAYOUT.right_min);
        assert!(LAYOUT.left_default.min(lmax) + rmax.min(LAYOUT.right_default) + center_min(&m.hex) + 2.0 * LAYOUT.separator <= LAYOUT.window_min[0] + 0.5);
    }

    /// §8.1: the ceiling is computed with both side panels shown (the shell always calls
    /// `need(true, true)`); at that ceiling every panel combination fits as well, so a panel
    /// toggle neither changes the zoom nor clips the layout.
    #[test]
    fn ceiling_with_both_panels_fits_every_panel_state() {
        let m = measured();
        let p = ZoomPolicy::DEFAULT;
        let (nw, nh) = m.need(true, true);
        for (w, h) in [(900.0, 560.0), (1100.0, 800.0), (1400.0, 900.0), (2000.0, 1200.0)] {
            let c = p.ceiling(ZoomFit { at_zoom: 1.0, avail_w: w, avail_h: h, need_w: nw, need_h: nh });
            for (nav, explain) in [(true, true), (false, true), (true, false), (false, false)] {
                assert!(m.fits(w / c, h / c, nav, explain), "{w}×{h} at {c}: panels {nav}/{explain} do not fit");
            }
        }
    }

    /// §8: the ceiling from `Measured::need` satisfies the acceptance rule at 900×560, 900×900
    /// and 1400×900 with both panels visible; and the former estimate (fixed 12 row heights,
    /// 8 bytes per row) reproduces the reported clipping at 1400 px / 170 %.
    #[test]
    fn zoom_ceiling_meets_the_measured_acceptance_rule() {
        let m = measured();
        let p = ZoomPolicy::DEFAULT;
        let (nw, nh) = m.need(true, true);
        for (w, h) in [(900.0, 560.0), (900.0, 900.0), (1400.0, 900.0)] {
            let c = p.ceiling(ZoomFit { at_zoom: 1.0, avail_w: w, avail_h: h, need_w: nw, need_h: nh });
            // Logical window at the ceiling.
            let (lw, lh) = (w / c, h / c);
            assert!(m.fits(lw, lh, true, true), "{w}×{h}: ceiling {c} does not fit ({lw}×{lh}, need {nw}×{nh})");
            eprintln!("{w}x{h}: ceiling {:.0}%", c * 100.0);
        }
        // Former rule: need_h = (2+1+3+6)·rh + 6 + 40, need_w = 180 + max(row(8), 300) + 200 + 48.
        let rh = m.records_min_h / 4.0;
        let old_w = LAYOUT.left_min + m.hex.width(8).max(300.0) + LAYOUT.right_min + 48.0;
        let old_h = 12.0 * rh + 6.0 + 40.0;
        let old = p.ceiling(ZoomFit { at_zoom: 1.0, avail_w: 1400.0, avail_h: 900.0, need_w: old_w, need_h: old_h });
        assert!(old >= 1.7 - 1e-3, "old ceiling {old}");
        // At 170 % the old layout gave the byte region a fixed 6 row heights; with the center
        // squeezed to (823 − left_max − right_min) the wrapped header + ruler + 3 byte rows need
        // more than that, i.e. byte rows were cut off.
        let lw = 1400.0 / 1.7;
        let center_need = m.hex.width(8).max(300.0);
        let old_left = (lw - center_need - LAYOUT.right_min).clamp(LAYOUT.left_min, LAYOUT.left_max);
        let old_center = lw - old_left - LAYOUT.right_min;
        assert!(6.0 * rh < m.bytes_h(old_center), "170 % clipping not reproduced: {} >= {}", 6.0 * rh, m.bytes_h(old_center));
    }
}
