//! Layout configuration and pure geometry: window/panel/table geometry that Reset View
//! restores, the 16/8/4 bytes-per-row policy and byte hit-testing, group wrapping (toolbar, hex
//! header), the status-bar width budget, the minimum layout the zoom ceiling needs, and table
//! column widths by semantic `ColumnRole`. Everything here is egui-free and unit-tested; the
//! zoom policy itself (bounds/step/ceiling) is owned by the app layer (`app::ZoomPolicy`).
//! Panels are never hidden automatically: narrowing the window shrinks the hex rows first
//! (16 → 8 → 4), then the Explanation panel, then Navigation, down to their minimums.

use crate::app::view_prefs::SplitRole;
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
    /// Floor of the Explanation width when it is squeezed above 100 % (0020 §8). The zoom
    /// ceiling's minimum layout uses the same floor, so the squeeze reaches 8 B/row at every
    /// ceiling. Below it Navigation shrinks instead.
    pub right_squeeze_min: f32,
    /// Center column inner margin (each side) of the records and bytes areas.
    pub center_margin: [f32; 2],
    /// Inner margin (x, y) of the toolbar and side-panel frames (`panels::panel_frame`).
    pub panel_margin: [f32; 2],
    /// Stroke width of framed bars and chips.
    pub frame_stroke: f32,
    /// Inner margin (x, y) of status chips and the byte-view mode chip (`panels::chip_frame`).
    pub chip_margin: [f32; 2],
    /// Width a side panel's resize separator takes (line + grab tolerance), per panel.
    pub separator: f32,
    /// Records table body rows that stay visible at the minimum height / zoom ceiling, below
    /// the header (0015 F2; see `records_min_h`).
    pub records_min_rows: f32,
    /// Byte rows that must stay visible at the minimum height / zoom ceiling.
    pub bytes_min_rows: f32,
    /// Byte rows the byte pane keeps when it shrinks to give the records table its rows.
    pub hex_floor_rows: f32,
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
    /// LSM Map (0026): lane gap, level frame inner padding, gap between level frames, minimum
    /// drawn box width, the readable box width's padding (`map_readable_w` = width of "000000"
    /// in Monospace + this), focus ring distance, points per key rank at the largest map zoom;
    /// WAL box width [min, max] and gap, the band separator's space above/below, the map height
    /// below which the band is one line of chips; map tooltip width [min, max] and the window
    /// share of its maximum.
    pub map_lane_gap: f32,
    /// Lowest lane height of the Bar tier (0030 A1; `panels::lsm_map::tier_heights`:
    /// Bar = max(this, 0.6 Small)).
    pub map_lane_min_h: f32,
    pub map_frame_pad: f32,
    pub map_row_gap: f32,
    pub map_min_box_w: f32,
    pub map_readable_pad: f32,
    pub map_focus_gap: f32,
    pub map_slot_max_pt: f32,
    pub map_wal_box_w: [f32; 2],
    /// WAL card width tiers (0030 A5-2): below [0] only glyphs on line 2, from [1] the name and
    /// the short state; from `map_wal_box_w[1]` the full state.
    pub map_wal_tier_w: [f32; 2],
    pub map_wal_gap: f32,
    pub map_wal_sep_gap: f32,
    pub map_chips_below_h: f32,
    pub map_tip_w: [f32; 2],
    pub map_tip_window_share: f32,
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
    // 0020 §8: raised from 200 toward the advised 240 only as far as 1400×900 keeps its 160 %
    // ceiling with ≥ 8 B/row: the minimum layout needs 864.5 + (floor − 200) pt and 1400/1.6 =
    // 875 pt, so the floor may be at most 210.5 pt; 208 leaves 2.5 pt for measurement rounding.
    right_squeeze_min: 208.0,
    center_margin: [8.0, 4.0],
    panel_margin: [8.0, 4.0],
    frame_stroke: 1.0,
    chip_margin: [4.0, 1.0],
    separator: 4.0,
    records_min_rows: 3.0,
    bytes_min_rows: 3.0,
    hex_floor_rows: 6.0,
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
    map_lane_gap: 2.0,
    map_lane_min_h: 8.0,
    map_frame_pad: 6.0,
    map_row_gap: 8.0,
    map_min_box_w: 3.0,
    map_readable_pad: 8.0,
    map_focus_gap: 2.0,
    map_slot_max_pt: 24.0,
    map_wal_box_w: [56.0, 160.0],
    map_wal_tier_w: [72.0, 120.0],
    map_wal_gap: 6.0,
    map_wal_sep_gap: 8.0,
    map_chips_below_h: 220.0,
    map_tip_w: [220.0, 380.0],
    map_tip_window_share: 0.45,
    about_logo: 128.0,
    about_width: 360.0,
    help_width: 520.0,
    swatch: 10.0,
    start_button_width: 140.0,
    status_min_glyphs: 6.0,
};

/// Height a framed bar adds around its content: top/bottom panel margin + the stroke.
pub fn panel_pad_h() -> f32 {
    2.0 * LAYOUT.panel_margin[1] + LAYOUT.frame_stroke
}

/// Width a chip adds around its label: left/right chip margin + the stroke on both sides.
pub fn chip_pad_w() -> f32 {
    2.0 * LAYOUT.chip_margin[0] + 2.0 * LAYOUT.frame_stroke
}

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

/// Columns hidden first when a table does not fit its panel, in this order (all columns of
/// the role at once): Raw hex, then Encoding, then Len.
pub const HIDE_PRIORITY: [ColumnRole; 3] = [ColumnRole::RawHex, ColumnRole::Encoding, ColumnRole::Len];
/// Padding added to the widest status word for the Status column (cell margins + slack).
pub const STATUS_CELL_PAD: f32 = 10.0;

/// Width of the Status column: its role width, widened so the widest status word ("Not
/// checked", "✔ Present", "✂ Truncated", "⛔ Resource limit" …, measured in the current font)
/// is never cut (0015 C2 GUI-1). Logical points: the same at every zoom.
pub fn status_column_width(widest_word: f32) -> f32 {
    column_width(ColumnRole::Status).max(widest_word + STATUS_CELL_PAD)
}

/// Which columns of a table are shown at `avail` points.
#[derive(Clone, Debug, PartialEq)]
pub struct ColumnPlan {
    pub shown: Vec<bool>,
    /// Columns hidden for lack of width ("+N columns").
    pub hidden: usize,
}

/// Plans the visible columns: while the fixed columns plus the fill minimum and spacing do not
/// fit `avail`, hide by `HIDE_PRIORITY`. Never hides Offset, Field, Status or the fill column;
/// Status keeps its full word width (no icon-only mode, 0015 C2): if the rest still does not
/// fit, the table scrolls horizontally. `show_all` keeps every column. Pure.
pub fn plan_columns(roles: &[ColumnRole], avail: f32, show_all: bool, width: &dyn Fn(ColumnRole) -> f32, spacing: f32) -> ColumnPlan {
    let mut shown = vec![true; roles.len()];
    let fill = roles.iter().position(|r| r.fills());
    let need = |shown: &[bool]| -> f32 { roles.iter().enumerate().filter(|(i, _)| shown[*i]).map(|(i, r)| if Some(i) == fill { LAYOUT.remainder_min } else { width(*r) } + spacing).sum() };
    if show_all {
        return ColumnPlan { shown, hidden: 0 };
    }
    for role in HIDE_PRIORITY {
        if need(&shown) <= avail {
            break;
        }
        for (i, r) in roles.iter().enumerate() {
            if *r == role {
                shown[i] = false;
            }
        }
    }
    let hidden = shown.iter().filter(|s| !**s).count();
    ColumnPlan { shown, hidden }
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

/// The LSM Map's split (0026 D2 F2): the bytes get `hex_keep` (their minimum + a row), never
/// more than leaves the records `rec_min`; the records (the map) get the rest.
pub fn map_first_split(h: f32, rec_min: f32, hex_keep: f32) -> (f32, f32) {
    let h = h.max(0.0);
    let hex = hex_keep.min((h - rec_min).max(0.0)).max(0.0);
    (h - hex, hex)
}

/// The levels view's Table without a user split (0030 A8): the records get `rec_want` (what
/// all its rows need), at most what leaves the bytes `hex_keep` (as under the Map), never less
/// than the standard plan's `std_rec`; the bytes take the rest.
pub fn levels_table_split(h: f32, std_rec: f32, rec_want: f32, hex_keep: f32) -> (f32, f32) {
    let h = h.max(0.0);
    let rec = rec_want.min((h - hex_keep).max(0.0)).max(std_rec).min(h);
    (rec, h - rec)
}

/// What the records / bytes split of the center needs (points; architect N5).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CenterInputs {
    /// Height of the two panes together (without the split handle).
    pub h: f32,
    pub records_min_h: f32,
    /// The byte pane's minimum, one byte row, and its preferred floor (six rows).
    pub hex_min: f32,
    pub hex_row_h: f32,
    pub hex_floor: f32,
    /// The records panel's measured overhead, the table with `TABLE_MIN_ROWS` and with all its
    /// rows.
    pub over: f32,
    pub table_need: f32,
    pub all_rows_need: f32,
    /// Byte rows of the open file (`file_rows`).
    pub file_rows: f32,
}

/// Records / bytes heights of the center for the split role shown (0026 D2 F2, 0030 A8): the
/// user's split of that role; without one, the Map favours the map (the bytes keep their
/// minimum + one row), the levels table gets all its rows with the same byte rows, the file
/// views use the standard plan — and the byte pane is at most the open file's content
/// (`bytes_min_rows` at least; the records get the rest).
pub fn center_heights(role: SplitRole, user: Option<f32>, i: &CenterInputs) -> (f32, f32) {
    let hex_keep = i.hex_min + i.hex_row_h;
    let plan = |ratio: f32| {
        let (r, x, _) = records_plan(i.h, ratio, i.records_min_h, i.hex_min, i.hex_floor, i.over, i.table_need);
        (r, x)
    };
    let (rec, hex) = match (role, user) {
        (SplitRole::Map, Some(r)) => split_heights(i.h, r, i.records_min_h, i.hex_min),
        (SplitRole::Map, None) => map_first_split(i.h, i.records_min_h, hex_keep),
        (SplitRole::LevelsTable, None) => levels_table_split(i.h, plan(crate::app::CENTER_SPLIT_DEFAULT).0, i.over + i.all_rows_need, hex_keep),
        (_, r) => plan(r.unwrap_or(crate::app::CENTER_SPLIT_DEFAULT)),
    };
    if user.is_some() {
        return (rec, hex);
    }
    let cap = i.hex_min + (i.file_rows - LAYOUT.bytes_min_rows).max(0.0) * i.hex_row_h;
    cap_bytes(rec, hex, cap)
}

/// Byte rows the whole file needs at `bytes_per_row` (0030 A8-2).
pub fn file_rows(file_len: u64, bytes_per_row: u64) -> f32 {
    file_len.div_ceil(bytes_per_row.max(1)) as f32
}

/// The byte pane at most `cap` high (the open file's content), the records taking the rest.
pub fn cap_bytes(rec: f32, hex: f32, cap: f32) -> (f32, f32) {
    let give = (hex - cap).max(0.0);
    (rec + give, hex - give)
}

/// Height the records table needs for its header and `rows` body rows (egui_extras adds the
/// item spacing to the header and every row) plus a horizontal scroll bar.
pub fn table_need_h(header_h: f32, row_h: f32, spacing_y: f32, hbar: f32, rows: f32) -> f32 {
    header_h + spacing_y + rows * (row_h + spacing_y) + hbar
}

/// Minimum height of the records panel (0015 F2): view title row, two location lines, the notes
/// collapsed to one line, separator + column toggle, then the table header and
/// `records_min_rows` body rows (+ horizontal bar), inside the center margins. `small_line` is
/// one small text line including item spacing; `title_row` one control row.
pub fn records_min_h(title_row: f32, small_line: f32, header_h: f32, row_h: f32, spacing_y: f32, hbar: f32) -> f32 {
    let post = 6.0 + spacing_y + title_row;
    2.0 * LAYOUT.center_margin[1] + title_row + 3.0 * small_line + post + table_need_h(header_h, row_h, spacing_y, hbar, LAYOUT.records_min_rows)
}

/// Records / bytes split for the center: the records get their full overhead (title, location,
/// notes, column toggle) + the table minimum, the bytes shrinking first to `hex_floor`. If even
/// that is not enough, the notes collapse to one line (`over_collapsed`). Returns (records,
/// bytes, notes collapsed). Overheads include the records margins.
pub fn records_plan(h: f32, ratio: f32, rec_min: f32, hex_min: f32, hex_floor: f32, over_full: f32, table_need: f32) -> (f32, f32, bool) {
    let want = over_full + table_need;
    let (rec, hex) = split_heights_records_first(h, ratio, rec_min, hex_min, want, hex_floor);
    (rec, hex, rec + 0.5 < want)
}

/// `split_heights`, then the records get up to `rec_want` (title, location, notes, header and
/// `TABLE_MIN_ROWS` body rows) by shrinking the bytes first, down to `hex_floor` (six byte rows,
/// never below `hex_min`). The user's ratio wins whenever it already gives the records enough.
pub fn split_heights_records_first(h: f32, ratio: f32, rec_min: f32, hex_min: f32, rec_want: f32, hex_floor: f32) -> (f32, f32) {
    let (rec, hex) = split_heights(h, ratio, rec_min, hex_min);
    if rec >= rec_want {
        return (rec, hex);
    }
    let give = (rec_want - rec).min((hex - hex_floor.max(hex_min)).max(0.0));
    (rec + give, hex - give)
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

/// Width the center wants at a zoom above 100 % and the zoom ceiling requires (0019 §3): 8
/// bytes per row plus its margins.
pub fn center_want(m: &HexMetrics) -> f32 {
    m.width(8) + 2.0 * LAYOUT.center_margin[0]
}

/// Effective side-panel widths above 100 % (0019 §3): when the user's widths leave the center
/// less than `center` (`center_want`), the Explanation panel shrinks first toward its minimum,
/// then Navigation toward its minimum. `left`/`right`: Some((min, user width)) of a shown panel,
/// None = collapsed strip of width `strip`. None when no shrinking is needed (the user's widths
/// apply unchanged).
pub fn squeeze_panels(window_w: f32, center: f32, strip: f32, left: Option<(f32, f32)>, right: Option<(f32, f32)>) -> Option<(f32, f32)> {
    let sep = LAYOUT.separator;
    let used = |p: Option<(f32, f32)>| p.map(|(_, w)| w + sep).unwrap_or(strip);
    let mut deficit = center - (window_w - used(left) - used(right));
    if deficit <= 0.0 {
        return None;
    }
    let shrink = |p: Option<(f32, f32)>, deficit: &mut f32| match p {
        Some((min, w)) => {
            let nw = (w - *deficit).max(min.min(w));
            *deficit -= w - nw;
            nw
        }
        None => 0.0,
    };
    let r = shrink(right, &mut deficit);
    let l = shrink(left, &mut deficit);
    Some((l, r))
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

/// Widths of the status-bar segments, in screen order `notice · warn · selection · search ·
/// file` plus the right-aligned hidden-panel hint (0020 UI ruling F1). Used for the wanted
/// (full text) widths and for the planned ones.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StatusWidths {
    /// The one transient notice (zoom, Reset View, copy).
    pub notice: f32,
    /// "file changed on disk" warning.
    pub warn: f32,
    pub sel: f32,
    /// Search segment ("Rows k / n", "Bytes: …"; the banner above the table repeats it).
    pub search: f32,
    /// "kind · file name" (the name is elided from its end).
    pub file: f32,
    pub hint: f32,
}

/// No-break space (U+00A0): egui never breaks a line at it (epaint `RowBreakCandidates::add`).
pub const NBSP: char = '\u{a0}';

/// Render-layer no-break join of a count and its byte unit (0020 UI ruling 6): "· 8 B" becomes
/// "· 8\u{a0}B", so a range text such as `file [0xc12,0xc1a) · 8 B` never wraps between the
/// count and `B`. Only for labels drawn on screen: model, clipboard and dump text keep plain
/// spaces. Units: B, KiB, MiB, GiB, after a decimal digit and before a non-alphanumeric char.
pub fn nb_units(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 4);
    for (i, &ch) in c.iter().enumerate() {
        let joins = ch == ' '
            && i > 0
            && c[i - 1].is_ascii_digit()
            && ["B", "KiB", "MiB", "GiB"].iter().any(|u| {
                let u: Vec<char> = u.chars().collect();
                let end = i + 1 + u.len();
                end <= c.len() && c[i + 1..end] == u[..] && c.get(end).is_none_or(|n| !n.is_alphanumeric())
            });
        out.push(if joins { NBSP } else { ch });
    }
    out
}

/// Plans the status-bar widths in `avail`. The notice and the warning keep their full width
/// whenever the bar is at least that wide (never elided for the others). Then: the hint is
/// truncated (dropped below `min`) first, then the search segment, the selection and the file
/// label are elided in that order, each down to `min` (or its own width if smaller).
pub fn status_widths(avail: f32, want: StatusWidths, min: f32) -> StatusWidths {
    let notice = want.notice.min(avail.max(0.0));
    let warn = want.warn.min((avail - notice).max(0.0));
    let rest = (avail - notice - warn).max(0.0);
    let mut w = StatusWidths { notice, warn, ..want };
    if w.sel + w.search + w.file + w.hint <= rest {
        return w;
    }
    let h = rest - w.sel - w.search - w.file;
    w.hint = if h >= min { h } else { 0.0 };
    if w.sel + w.search + w.file + w.hint <= rest {
        return w;
    }
    w.hint = 0.0;
    w.search = (rest - w.sel - w.file).max(want.search.min(min)).min(want.search);
    w.sel = (rest - w.search - w.file).max(want.sel.min(min)).min(want.sel);
    w.file = (rest - w.search - w.sel).clamp(0.0, want.file);
    w
}

/// A tree label whose never-cut head (design A5: what, members, coverage, with its glyph) is
/// wider than the row wraps instead of truncating; otherwise only the tail truncates.
pub fn tree_label_wraps(head_w: f32, avail: f32) -> bool {
    head_w > avail
}

// ------------------------------------------------------------------ Explanation values (0015 C)

/// Lines an Explanation value may take before it ends in "…" (full text in the tooltip).
pub const VALUE_MAX_LINES: usize = 4;

/// Breaks a monospace value into lines of at most `cols` characters, never scrolling sideways
/// (design C1–C3). Break points in order: a space (" · " included); after "/" (", " is a
/// space); between hex byte pairs (after "0x", at even positions — a byte is never split);
/// anywhere as a last resort. At most `max_lines` lines; when cut, the last one ends in "…".
/// Returns the lines and whether the value was cut. Pure (monospace: one glyph = one column).
pub fn wrap_value(text: &str, cols: usize, max_lines: usize) -> (Vec<String>, bool) {
    let cols = cols.max(4);
    let mut lines: Vec<String> = Vec::new();
    for para in text.split('\n') {
        let mut rest: Vec<char> = para.chars().collect();
        loop {
            if rest.len() <= cols {
                lines.push(rest.iter().collect());
                break;
            }
            let (line_end, next) = value_break(&rest, cols);
            lines.push(rest[..line_end].iter().collect::<String>().trim_end().to_string());
            rest.drain(..next);
        }
    }
    if lines.len() <= max_lines.max(1) {
        return (lines, false);
    }
    lines.truncate(max_lines.max(1));
    if let Some(last) = lines.last_mut() {
        let keep: String = last.chars().take(cols - 1).collect();
        *last = format!("{}…", keep.trim_end());
    }
    (lines, true)
}

/// Where to break `rest` (longer than `cols`): (end of this line, start of the next).
fn value_break(rest: &[char], cols: usize) -> (usize, usize) {
    // 1) the last space within the line (the space itself is dropped).
    if let Some(i) = (1..=cols).rev().find(|&i| rest[i] == ' ') {
        return (i, i + 1);
    }
    // 2) after a "/".
    if let Some(i) = (0..cols).rev().find(|&i| rest[i] == '/') {
        return (i + 1, i + 1);
    }
    // 3) a hex run ("0x…" or a continuation of hex digits): an even number of digits.
    let hex = |c: &char| c.is_ascii_hexdigit();
    let base = if rest.len() > 2 && rest[0] == '0' && rest[1] == 'x' && hex(&rest[2]) { 2 } else { 0 };
    if rest[base..cols].iter().all(hex) {
        let i = base + (cols - base) / 2 * 2;
        if i > base {
            return (i, i);
        }
    }
    // 4) anywhere.
    (cols, cols)
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
    /// Records minimum (`records_min_h`): overhead with collapsed notes + header + 3 rows.
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
    /// Side panels at their minimum + the center at 8 bytes per row (0019 §3; was 4).
    fn panels_w(&self, nav: bool, explain: bool) -> f32 {
        let side = |shown: bool, min: f32| if shown { min + LAYOUT.separator } else { self.strip };
        side(nav, self.nav_min) + side(explain, self.explain_min) + center_want(&self.hex)
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
        let center_w = (w - self.panels_w(nav, explain) + center_want(&self.hex)).max(center_want(&self.hex));
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
    /// width, the side panels at their minimum leave the center ≥ 8 bytes per row (0019 §3) without
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
    use crate::app::ZoomFit;

    /// 0030 A8 / architect N5: the center split rule per role, without the rig. 800 pt; bytes
    /// minimum 60 + 20 per row; a small file (1 row) caps the bytes to the minimum without a
    /// user split; a user split is kept as dragged; the Map keeps min + 1 row; the levels table
    /// gets all its rows.
    #[test]
    fn center_heights_per_role() {
        let i = CenterInputs { h: 800.0, records_min_h: 150.0, hex_min: 60.0, hex_row_h: 20.0, hex_floor: 120.0, over: 100.0, table_need: 200.0, all_rows_need: 500.0, file_rows: 1.0 };
        assert_eq!(center_heights(SplitRole::Records, None, &i), (740.0, 60.0), "CURRENT-sized file: the byte minimum");
        let big = CenterInputs { file_rows: 10_000.0, ..i };
        let (r, x) = center_heights(SplitRole::Records, None, &big);
        assert!((r - 800.0 * crate::app::CENTER_SPLIT_DEFAULT).abs() < 0.5 && (r + x - 800.0).abs() < 0.01, "a large file: the standard plan ({r}, {x})");
        assert_eq!(center_heights(SplitRole::Records, Some(0.25), &i), (300.0, 500.0), "a user split (200) is kept uncapped, raised only to the records' need (100 + 200)");
        assert_eq!(center_heights(SplitRole::Map, None, &big), (720.0, 80.0), "Map: minimum + one row");
        assert_eq!(center_heights(SplitRole::LevelsTable, None, &big), (600.0, 200.0), "levels table: its rows (100 + 500)");
        assert_eq!(center_heights(SplitRole::LevelsTable, None, &i), (740.0, 60.0), "capped by the small file");
    }
    use crate::app::ZoomPolicy;

    /// 900 px / 110 %: the fields table hides Raw hex, then Encoding, then Len (in that order),
    /// never Offset/Field/Status/Decoded; Status keeps its word width (no icon-only mode since
    /// 0015 C2; the table scrolls instead); "show all" keeps all.
    #[test]
    fn column_hide_priority() {
        use ColumnRole as R;
        let roles = [R::Field, R::Offset, R::Len, R::Decoded, R::Encoding, R::RawHex, R::Status];
        let w = |r: ColumnRole| column_width(r);
        let full: f32 = roles.iter().map(|r| if r.fills() { LAYOUT.remainder_min } else { w(*r) } + 4.0).sum();
        let p = plan_columns(&roles, full + 1.0, false, &w, 4.0);
        assert_eq!(p.hidden, 0);
        let p = plan_columns(&roles, full - 1.0, false, &w, 4.0);
        assert_eq!(p.shown, vec![true, true, true, true, true, false, true], "Raw hex goes first");
        let p = plan_columns(&roles, full - w(R::RawHex) - 5.0, false, &w, 4.0);
        assert_eq!(p.shown, vec![true, true, true, true, false, false, true], "then Encoding");
        let p = plan_columns(&roles, full - w(R::RawHex) - w(R::Encoding) - 9.0, false, &w, 4.0);
        assert_eq!((p.shown.clone(), p.hidden), (vec![true, true, false, true, false, false, true], 3), "then Len");
        let p = plan_columns(&roles, 300.0, false, &w, 4.0);
        assert!(p.shown[0] && p.shown[1] && p.shown[3] && p.shown[6], "{p:?}");
        let p = plan_columns(&roles, 300.0, true, &w, 4.0);
        assert_eq!(p.hidden, 0, "+N columns shows everything");
    }

    /// 0015 C2 GUI-1: the Status column fits its widest word in every theme's font (header
    /// "Status" too); points are logical, so the width holds at 900×560 and at any zoom (the
    /// table scrolls horizontally rather than cutting Status).
    #[test]
    fn status_column_fits_every_status_word() {
        use crate::ui::{panels, theme};
        use eframe::egui;
        for t in theme::THEMES {
            let ctx = egui::Context::default();
            let tok = theme::apply_theme(&ctx, t, &mut Vec::new());
            crate::ui::test_frame(&ctx, Default::default(), |ui| {
                let widest = panels::records::widest_status_word(ui);
                let w = status_column_width(widest);
                for word in panels::records::status_words() {
                    let ww = ui.fonts_mut(|f| f.layout_no_wrap(word.clone(), egui::TextStyle::Body.resolve(ui.style()), egui::Color32::WHITE).size().x);
                    assert!(ww + STATUS_CELL_PAD <= w + 0.01, "{}: {word:?} {ww} in {w}", t.id);
                }
                let header = ui.fonts_mut(|f| f.layout_no_wrap("Status".into(), tok.header_font(), egui::Color32::WHITE).size().x);
                assert!(header + STATUS_CELL_PAD <= w, "{}: header", t.id);
                eprintln!("{}: Status column {w:.0} pt (widest word {widest:.0} pt)", t.id);
            });
        }
    }

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

    /// G6 (0011 fix): at a short center the bytes shrink first (to six rows) so the records
    /// table keeps its header + 4 rows; a ratio that already suffices is kept.
    #[test]
    fn records_first_split_shrinks_bytes_to_their_floor() {
        // 575 pt center, ratio 0.42 → 241 records; want 300; bytes 334 → floor 230: give 59.
        assert_eq!(split_heights_records_first(575.0, 0.42, 75.0, 150.0, 300.0, 230.0), (300.0, 275.0));
        // Not enough even at the floor: bytes stop at six rows.
        assert_eq!(split_heights_records_first(500.0, 0.42, 75.0, 150.0, 400.0, 230.0), (270.0, 230.0));
        // The user's larger records share is untouched.
        assert_eq!(split_heights_records_first(1000.0, 0.42, 75.0, 150.0, 300.0, 230.0), (420.0, 580.0));

        // FREEZE D: the real 900×700 window at 100 % and at the 120 % ceiling (logical window
        // 900/z × 700/z; real bundled-font metrics of the default theme). The WAL root view's
        // overhead: Back/title row, 2 location lines, a 3-line subtitle, separator, "+3 columns".
        use eframe::egui::{self, FontId};
        let m = measured();
        let ctx = egui::Context::default();
        crate::ui::test_frame(&ctx, Default::default(), |_| {});
        let h_of = |f: FontId| ctx.fonts_mut(|x| x.row_height(&f));
        let sp = egui::Spacing::default();
        let sp_y = sp.item_spacing.y;
        let row_h = h_of(FontId::proportional(14.5)).max(h_of(FontId::monospace(13.5))) + LAYOUT.row_pad;
        let header_h = h_of(FontId::proportional(15.5)) + LAYOUT.header_pad;
        let small = h_of(FontId::proportional(12.0)) + sp_y;
        let margins = 2.0 * LAYOUT.center_margin[1];
        let post = 6.0 + sp_y + m.toolbar_row_h;
        let over_full = margins + m.toolbar_row_h + 2.0 * small + 3.0 * small + post;
        let over_collapsed = margins + m.toolbar_row_h + 2.0 * small + small + post;
        let need = table_need_h(header_h, row_h, sp_y, sp.scroll.allocated_width(), 4.0);
        for z in [1.0f32, 1.2] {
            let (w, h) = (900.0 / z, 700.0 / z);
            let center_h = h - m.toolbar_h(w) - m.status_h - LAYOUT.split_handle;
            let hex_min = m.bytes_h(center_min(&m.hex));
            let hex_floor = hex_min + (LAYOUT.hex_floor_rows - LAYOUT.bytes_min_rows) * m.hex_row_h;
            let (rec, hex, collapsed) = records_plan(center_h, 0.42, m.records_min_h, hex_min, hex_floor, over_full, need);
            let over = if collapsed { over_collapsed } else { over_full };
            let body_rows = ((rec - over - header_h - sp_y - sp.scroll.allocated_width()) / (row_h + sp_y)).floor();
            assert!(body_rows >= 4.0, "zoom {z}: {body_rows} rows (records {rec}, bytes {hex}, collapsed {collapsed})");
            assert!(hex >= hex_min - 0.01, "zoom {z}: bytes keep their minimum");
        }
    }

    /// 0015 C1–C3: values wrap (no horizontal scrolling) at spaces, after "/", between hex byte
    /// pairs, anywhere last; at most four lines, the fourth ending in "…".
    #[test]
    fn explanation_values_wrap_at_the_designed_break_points() {
        assert_eq!(wrap_value("short", 20, 4), (vec!["short".to_string()], false));
        // Spaces and " · " first.
        let (l, cut) = wrap_value("\"key-006\" · seq 7 · Value (Put)", 14, 4);
        assert_eq!((l, cut), (vec!["\"key-006\" ·".to_string(), "seq 7 · Value".into(), "(Put)".into()], false));
        // After "/".
        let (l, _) = wrap_value("/Users/me/rocks/db/000004.log", 12, 4);
        assert_eq!(l, vec!["/Users/me/".to_string(), "rocks/db/".into(), "000004.log".into()]);
        // Hex: after 0x at even positions, never inside a byte.
        let (l, _) = wrap_value("0x6b65792d3030360107000000000000", 9, 4);
        assert_eq!(l, vec!["0x6b6579".to_string(), "2d303036".into(), "01070000".into(), "00000000".into()]);
        for (n, line) in l.iter().enumerate() {
            let digits = if n == 0 { line.len() - 2 } else { line.len() };
            assert_eq!(digits % 2, 0, "byte split in {line:?}");
        }
        assert_eq!(l.concat(), "0x6b65792d3030360107000000000000");
        // Anywhere as a last resort; four lines max with "…" and the cut flag.
        let (l, cut) = wrap_value(&"q".repeat(50), 10, 4);
        assert_eq!(l.len(), 4);
        assert!(cut && l[3].ends_with('…') && l[3].chars().count() <= 10);
        assert!(l.iter().all(|x| x.chars().count() <= 10));
    }

    /// 0015 F4: the zoom ceiling as the shell computes it (`App::measured(ui, true)`): default
    /// theme applied, worst-case hex header groups, the reserved path-error line, 16 offset
    /// digits. Toolbar group widths are `measured()`'s label estimates (the shell measures them
    /// live), so the printed values are to be cross-checked in the running app.
    #[test]
    fn shell_like_zoom_ceilings() {
        use crate::ui::{panels, theme};
        use eframe::egui;
        let est = measured();
        let ctx = egui::Context::default();
        let tok = theme::apply_theme(&ctx, theme::theme_by_id(crate::app::prefs::DEFAULT_THEME), &mut Vec::new());
        let mut m = None;
        crate::ui::test_frame(&ctx, Default::default(), |ui| {
            let line = ui.text_style_height(&egui::TextStyle::Body);
            let hex_row = panels::hex::row_height(ui);
            let sp_y = ui.spacing().item_spacing.y;
            m = Some(Measured {
                nav_min: LAYOUT.left_min,
                explain_min: LAYOUT.right_squeeze_min,
                strip: tok.header_height(ui),
                hex: panels::hex::metrics(ui, u64::MAX),
                toolbar_groups: est.toolbar_groups.clone(),
                toolbar_gap: ui.spacing().item_spacing.x,
                toolbar_row_h: est.toolbar_row_h,
                toolbar_pad_h: panel_pad_h() + line + 2.0 + sp_y,
                status_h: est.status_h,
                records_min_h: records_min_h(
                    est.toolbar_row_h,
                    ui.text_style_height(&egui::TextStyle::Small) + sp_y,
                    tok.header_height(ui),
                    tok.row_height(ui),
                    sp_y,
                    ui.spacing().scroll.allocated_width(),
                ),
                hex_groups: panels::hex::ceiling_groups(ui),
                hex_gap: ui.spacing().item_spacing.x,
                hex_line_h: panels::hex::HexMemory::default().line_h,
                hex_ruler_h: hex_row,
                hex_row_h: hex_row,
                hex_pad_h: 2.0 * LAYOUT.center_margin[1] + ui.spacing().scroll.allocated_width(),
            });
        });
        let m = m.unwrap();
        let (nw, nh) = m.need(true, true);
        for (w, h) in [(900.0, 560.0), (900.0, 700.0), (1400.0, 900.0)] {
            let c = ZoomPolicy::DEFAULT.ceiling(ZoomFit { at_zoom: 1.0, avail_w: w, avail_h: h, need_w: nw, need_h: nh });
            eprintln!("shell-like {w}x{h}: ceiling {:.0}% (need {nw:.1}x{nh:.1})", c * 100.0);
            assert!(m.fits(w / c, h / c, true, true));
        }
    }

    /// 0015 A5: the head of a group label (glyph, what, members, coverage) at the default
    /// Navigation width: measured here; the tree wraps instead of cutting when it does not fit.
    #[test]
    fn tree_group_head_width_at_default_left_panel() {
        use crate::ui::theme;
        use eframe::egui;
        let ctx = egui::Context::default();
        let _ = theme::apply_theme(&ctx, theme::theme_by_id(crate::app::prefs::DEFAULT_THEME), &mut Vec::new());
        let mut out = Vec::new();
        crate::ui::test_frame(&ctx, Default::default(), |ui| {
            let font = egui::TextStyle::Button.resolve(ui.style());
            let w = |s: &str| ui.fonts_mut(|f| f.layout_no_wrap(s.to_string(), font.clone(), egui::Color32::WHITE).size().x);
            // Row: frame margin 8 each side, indent 4 + depth·14, "+" button, item spacing, button padding.
            let plus = w("+") + 2.0 * ui.spacing().button_padding.x;
            let avail = |depth: f32| {
                LAYOUT.left_default - 2.0 * 8.0 - ui.spacing().scroll.allocated_width() - 4.0 - depth * LAYOUT.tree_indent - plus - ui.spacing().item_spacing.x - 2.0 * ui.spacing().button_padding.x
            };
            for (depth, head) in [(0.0, "Data blocks #0–#4095 · not checked"), (0.0, "✖ Data blocks #20480–#24575 · 4095 of 4096 checked"), (1.0, "Data blocks #4096–#4159 · 63 of 64 checked")]
            {
                out.push((head, w(head), avail(depth)));
            }
        });
        for (head, need, avail) in &out {
            eprintln!("A5 head {head:?}: {need:.0} pt, available {avail:.0} pt at left {}", LAYOUT.left_default);
            // Never cut: a head wider than the row wraps the label; otherwise only the tail
            // truncates. (Widening the default to 290 pt would add 30 pt: still short for the
            // common head, so the default stays 260.)
            assert_eq!(tree_label_wraps(*need, *avail), need > avail);
            assert!(!tree_label_wraps(*need, need + 1.0));
        }
        assert!(out[0].1 > out[0].2 + 30.0, "re-check the 260 → 290 fallback: the common head now fits a wider default");
    }

    /// 0015 F2: at the minimum window 900×560 — at 100 % and at its zoom ceiling — the notes
    /// collapse, the records table keeps its header + ≥ 3 body rows and the byte pane ≥ 3 byte
    /// rows (it shrinks first, down to its floor).
    #[test]
    fn minimum_height_keeps_three_table_rows_and_three_byte_rows() {
        use eframe::egui::{self, FontId};
        let m = measured();
        let ctx = egui::Context::default();
        crate::ui::test_frame(&ctx, Default::default(), |_| {});
        let h_of = |f: FontId| ctx.fonts_mut(|x| x.row_height(&f));
        let sp = egui::Spacing::default();
        let sp_y = sp.item_spacing.y;
        let row_h = h_of(FontId::proportional(14.5)).max(h_of(FontId::monospace(13.5))) + LAYOUT.row_pad;
        let header_h = h_of(FontId::proportional(15.5)) + LAYOUT.header_pad;
        let small = h_of(FontId::proportional(12.0)) + sp_y;
        let margins = 2.0 * LAYOUT.center_margin[1];
        let post = 6.0 + sp_y + m.toolbar_row_h;
        // Root view overhead (SST root: two notes; WAL root: two notes + 3-line wrap).
        let over_full = margins + m.toolbar_row_h + 2.0 * small + 3.0 * small + post;
        let over_collapsed = margins + m.toolbar_row_h + 2.0 * small + small + post;
        let need = table_need_h(header_h, row_h, sp_y, sp.scroll.allocated_width(), 4.0);
        let (nw, nh) = m.need(true, true);
        let ceiling = ZoomPolicy::DEFAULT.ceiling(ZoomFit { at_zoom: 1.0, avail_w: 900.0, avail_h: 560.0, need_w: nw, need_h: nh });
        for z in [1.0f32, ceiling] {
            let (w, h) = (LAYOUT.window_min[0] / z, LAYOUT.window_min[1] / z);
            let center_h = h - m.toolbar_h(w) - m.status_h - LAYOUT.split_handle;
            // Byte header wrapped at the center width the side panels (at their minimum) leave.
            let hex_min = m.bytes_h((w - m.panels_w(true, true) + center_min(&m.hex)).max(center_min(&m.hex)));
            let hex_floor = hex_min + (LAYOUT.hex_floor_rows - LAYOUT.bytes_min_rows) * m.hex_row_h;
            let (rec, hex, collapsed) = records_plan(center_h, 0.42, m.records_min_h, hex_min, hex_floor, over_full, need);
            let over = if collapsed { over_collapsed } else { over_full };
            let body_rows = ((rec - over - header_h - sp_y - sp.scroll.allocated_width()) / (row_h + sp_y)).floor();
            eprintln!("900x560 at {:.0}%: records {rec:.0} ({body_rows} rows, notes collapsed {collapsed}), bytes {hex:.0} (min {hex_min:.0})", z * 100.0);
            assert!(body_rows >= 3.0, "zoom {z}: {body_rows} table rows");
            assert!(hex >= hex_min - 0.01, "zoom {z}: bytes below three rows");
        }
    }

    /// 0019 §3: Explanation shrinks first, then Navigation, never below their minimums; no
    /// squeeze when the user's widths already leave the center its 8 bytes per row.
    #[test]
    fn squeeze_shrinks_explanation_first_then_navigation() {
        let sep = LAYOUT.separator;
        // 875 − (260+4) − (320+4) = 287 left for the center; it wants 400: deficit 113.
        assert_eq!(squeeze_panels(875.0, 400.0, 20.0, Some((180.0, 260.0)), Some((200.0, 320.0))), Some((260.0, 207.0)));
        // Deficit beyond the Explanation minimum: Navigation gives the rest, down to its minimum.
        assert_eq!(squeeze_panels(700.0, 400.0, 20.0, Some((180.0, 260.0)), Some((200.0, 320.0))), Some((180.0, 200.0)));
        assert_eq!(squeeze_panels(1000.0, 400.0, 20.0, Some((180.0, 260.0)), Some((200.0, 320.0))), None);
        // A collapsed Explanation (strip) leaves Navigation to shrink alone.
        let s = squeeze_panels(620.0, 400.0, 20.0, Some((180.0, 260.0)), None).unwrap();
        assert!((s.0 - (620.0 - 400.0 - 20.0 - sep)).abs() < 1e-3, "{s:?}");
        assert!(center_want(&M) > center_min(&M));
        // Header compaction: only below the measured full header + table rows.
        use crate::ui::panels::records::header_compact;
        assert!(!header_compact(100.0, None, 150.0), "nothing measured yet");
        assert!(header_compact(299.0, Some(150.0), 150.0));
        assert!(!header_compact(300.0, Some(150.0), 150.0));
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

    /// 0020 UI ruling 6: only "<digits> <unit>" joins; everything else keeps its spaces.
    #[test]
    fn nb_units_joins_count_and_unit_only() {
        let nb = |s: &str| nb_units(s).replace(NBSP, "~");
        assert_eq!(nb("file [0xc12,0xc1a) · 8 B"), "file [0xc12,0xc1a) · 8~B");
        assert_eq!(nb("file [0x0,0x68) · 104 B stored -> block@0x0 [+0x0,+0x143) · 323 B decompressed"), "file [0x0,0x68) · 104~B stored -> block@0x0 [+0x0,+0x143) · 323~B decompressed");
        assert_eq!(nb("Page 2 of 3 · 65536 B of 150000 B · 64 KiB per page"), "Page 2 of 3 · 65536~B of 150000~B · 64~KiB per page");
        assert_eq!(nb("2115195 B. Select"), "2115195~B. Select");
        assert_eq!(nb("0x2 Bytes · 3 Blocks · a B · 4 B,"), "0x2 Bytes · 3 Blocks · a B · 4~B,");
        assert_eq!(nb_units("selection: none"), "selection: none");
    }

    /// 0020 UI ruling F1: the notice keeps its width while the bar holds it; the hint goes
    /// first, then search, then selection, then the file label.
    #[test]
    fn status_budget_truncation_order() {
        let want = StatusWidths { notice: 250.0, warn: 0.0, sel: 200.0, search: 300.0, file: 200.0, hint: 100.0 };
        let w = |avail: f32| status_widths(avail, want, 48.0);
        let t = |notice, sel, search, file, hint| StatusWidths { notice, warn: 0.0, sel, search, file, hint };
        assert_eq!(w(1250.0), want);
        assert_eq!(w(1010.0), t(250.0, 200.0, 300.0, 200.0, 60.0));
        assert_eq!(w(980.0), t(250.0, 200.0, 300.0, 200.0, 0.0), "hint below the minimum is dropped");
        assert_eq!(w(850.0), t(250.0, 200.0, 200.0, 200.0, 0.0), "search elides first");
        assert_eq!(w(650.0), t(250.0, 152.0, 48.0, 200.0, 0.0), "then the selection");
        assert_eq!(w(450.0), t(250.0, 48.0, 48.0, 104.0, 0.0), "then the file label");
        assert_eq!(w(250.0), t(250.0, 48.0, 48.0, 0.0, 0.0), "the notice keeps its width while the bar holds it");
        assert_eq!(w(200.0).notice, 200.0, "a bar narrower than the notice: the notice fills it");
        // The warning keeps its width like the notice; no notice at all leaves the room to the rest.
        let w2 = status_widths(700.0, StatusWidths { notice: 0.0, warn: 150.0, ..want }, 48.0);
        assert_eq!((w2.warn, w2.sel, w2.search, w2.file), (150.0, 200.0, 150.0, 200.0));
    }

    /// Metrics as the default theme measures them at 100 % (body 14.5 pt, mono 13.5 pt, toolbar
    /// group widths from the label texts). Measured with egui's bundled fonts.
    fn measured() -> Measured {
        use eframe::egui::{self, FontId};
        let ctx = egui::Context::default();
        crate::ui::test_frame(&ctx, Default::default(), |_| {});
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
            btn("LSM Map"),
            group(&[LAYOUT.path_min, btn("Go")]),
            group(&[LAYOUT.search_width, sp.icon_width + tw("Hex bytes", 14.5) + gap, btn("Prev"), btn("Next"), btn("Clear search")]),
            btn("Copy…"),
            group(&[btn("A−"), tw("100%", 14.5), btn("A+"), btn("Reset View")]),
        ];
        let small = |s: &str| tw(s, 12.0) + bp;
        let hex_groups = [
            group(&[tw("Decompressed block", 12.0) + 8.0, small("show file bytes"), small("Hide bytes")]),
            group(&[btn("◀ page"), tw("file [0x0,0x10000) of 65536 B", 14.5), btn("page ▶")]),
            group(&[LAYOUT.goto_width, btn("Go to")]),
        ];
        let ctl = sp.interact_size.y.max(line + 2.0 * sp.button_padding.y) + sp.item_spacing.y;
        let small_line = ctx.fonts_mut(|f| f.row_height(&FontId::proportional(12.0))) + sp.item_spacing.y;
        let header_h = ctx.fonts_mut(|f| f.row_height(&FontId::proportional(15.5))) + LAYOUT.header_pad;
        Measured {
            nav_min: LAYOUT.left_min,
            explain_min: LAYOUT.right_squeeze_min,
            strip: line + LAYOUT.header_pad,
            hex: HexMetrics { cw, pad: 10.0 + 2.0 * gap, digits: 8 },
            toolbar_groups,
            toolbar_gap: gap,
            toolbar_row_h: ctl,
            toolbar_pad_h: 8.0,
            status_h: line + LAYOUT.status_pad + 8.0,
            records_min_h: records_min_h(ctl, small_line, header_h, line.max(mono_line) + LAYOUT.row_pad, sp.item_spacing.y, sp.scroll.allocated_width()),
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
        for (w, h) in [(900.0, 560.0), (900.0, 700.0), (900.0, 900.0), (1400.0, 900.0)] {
            let c = p.ceiling(ZoomFit { at_zoom: 1.0, avail_w: w, avail_h: h, need_w: nw, need_h: nh });
            // Logical window at the ceiling.
            let (lw, lh) = (w / c, h / c);
            assert!(m.fits(lw, lh, true, true), "{w}×{h}: ceiling {c} does not fit ({lw}×{lh}, need {nw}×{nh})");
            eprintln!("{w}x{h}: ceiling {:.0}%", c * 100.0);
        }
        // Former rule: need_h = (2+1+3+6)·rh + 6 + 40, need_w = 180 + max(row(8), 300) + 200 + 48
        // (rh as the former `records_min_h / 4` = (3 row heights + 1 line) / 4).
        let line = {
            let ctx = eframe::egui::Context::default();
            crate::ui::test_frame(&ctx, Default::default(), |_| {});
            ctx.fonts_mut(|f| f.row_height(&eframe::egui::FontId::proportional(14.5)))
        };
        let rh = (3.0 * (line + LAYOUT.row_pad) + line) / 4.0;
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
