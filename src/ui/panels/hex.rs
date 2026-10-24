//! Byte view (bottom of the center column): header groups A (mode chip, space buttons, Hide
//! bytes), B (paging) and C (Go to) that wrap as units; the OFFSET / byte-column / ASCII ruler;
//! and the paged rows of the current `app::hex::HexState` page with 16, 8 or 4 bytes per row
//! (`layout::choose_bytes_per_row`). Offsets, selection, Shift+click ranges and copies stay
//! byte-absolute.
//!
//! Rows are painted, not built from labels: one click-only hit area per row and a pure column →
//! byte mapping (`layout::byte_at`). A plain click selects exactly the byte under the pointer;
//! only Shift+click extends; a press-and-drag selects nothing (no text selection, no drag
//! capture, no scroll-to-cursor animation).

use super::{strong, PanelMemory};
use crate::app::hex::{HexState, PageState};
use crate::app::{Action, AppState, Panel};
use crate::limits::LIMITS;
use crate::model::{Space, Span, SpaceInfo, Status};
use crate::ui::layout::{ascii_col, byte_at, byte_col, choose_bytes_per_row, hex_header_lines, hex_row_chars, offset_digits, HexMetrics, LAYOUT, PACK_SLACK};
use crate::ui::menu::{self, MenuCmd};
use crate::ui::theme::Tokens;
use eframe::egui::{self, RichText, Sense};

pub const GOTO_ID: &str = "goto";

/// Tooltip of the mode chip.
const CHIP_FILE_TIP: &str = "Offsets are positions in the file as stored on disk.";
const CHIP_DERIVED_TIP: &str = "Offsets count from the start of this decompressed block / reassembled record. They are NOT file offsets. Use 'show file bytes' for physical positions.";

/// UI-local byte-view memory.
pub struct HexMemory {
    pub hover: Option<u64>,
    /// Horizontal scroll offset of the byte rows (the ruler follows it).
    pub hscroll: f32,
    /// Bytes per row currently shown (16, 8 or 4).
    pub bytes_per_row: u64,
    /// Measured widths of the header groups A/B/C (last frame).
    pub groups: [f32; 3],
    /// Measured height of one header line.
    pub line_h: f32,
    /// Pointer press position of the current press (egui clears `press_origin` in the release
    /// frame, where `clicked()` fires).
    pub press: Option<egui::Pos2>,
}

impl Default for HexMemory {
    fn default() -> Self {
        HexMemory { hover: None, hscroll: 0.0, bytes_per_row: LIMITS.hex_bytes_per_row, groups: [260.0, 330.0, 230.0], line_h: 26.0, press: None }
    }
}

/// Row metrics for the current style and space size.
pub fn metrics(ui: &egui::Ui, size: u64) -> HexMetrics {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let cw = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
    HexMetrics { cw, pad: ui.spacing().scroll.allocated_width() + 2.0 * ui.spacing().item_spacing.x, digits: offset_digits(size) }
}

/// Worst-case widths of the header groups A/B/C for the zoom ceiling (spec §8.1). Static texts
/// only, so the ceiling never depends on the open file, the selected node or the space size:
/// A = widest chip + widest space button ("show block@0x<16 digits> (decompressed)" /
/// "show record@… (reassembled)" / "show file bytes") + Hide bytes; B = the paging label with
/// 16-digit offsets, a 15-digit page count and a 20-digit size; C = Go-to field + Go to. Row
/// planning keeps using the live widths in `HexMemory::groups`.
pub fn ceiling_groups(ui: &egui::Ui) -> [f32; 3] {
    let gap = ui.spacing().item_spacing.x;
    let pad = 2.0 * ui.spacing().button_padding.x;
    let color = ui.visuals().text_color();
    let width = |s: &str, style: egui::TextStyle| {
        let font = style.resolve(ui.style());
        ui.fonts_mut(|f| f.layout_no_wrap(s.to_string(), font, color).size().x)
    };
    // Widest glyph of a digit set, so a run of n digits is an upper bound for any value.
    let widest = |set: &str, style: egui::TextStyle| set.chars().max_by(|a, b| width(&a.to_string(), style.clone()).total_cmp(&width(&b.to_string(), style.clone()))).unwrap_or('0');
    let run = |c: char, n: usize| std::iter::repeat_n(c, n).collect::<String>();
    let (hb, db) = (widest("0123456789abcdef", egui::TextStyle::Button), widest("0123456789", egui::TextStyle::Body));
    let hl = widest("0123456789abcdef", egui::TextStyle::Body);
    let button = |s: &str| width(s, egui::TextStyle::Button) + pad;
    // Mode chip: label + frame margin (4 pt each side) + 1 pt stroke each side.
    let chip = ["File", "Decompressed block", "Reassembled record"].iter().map(|s| width(s, egui::TextStyle::Body)).fold(0.0, f32::max) + 2.0 * 4.0 + 2.0;
    let h16 = run(hb, 16);
    let space_btn = [format!("show block@0x{h16} (decompressed)"), format!("show record@0x{h16} (reassembled)"), "show file bytes".to_string()].iter().map(|s| button(s)).fold(0.0, f32::max);
    let a = chip + space_btn + button("Hide bytes") + 2.0 * gap;
    let (p15, h16l, d20) = (run(db, 15), run(hl, 16), run(db, 20));
    let b = button("◀ page") + width(&format!("page {p15}/{p15} · 0x{h16l}..0x{h16l} of {d20} bytes"), egui::TextStyle::Body) + button("page ▶") + 2.0 * gap;
    // TextEdit draws its default 4 pt frame margins outside `desired_width`.
    let c = LAYOUT.goto_width + 2.0 * 4.0 + button("Go to") + gap;
    [a, b, c]
}

/// Height of one byte row (and of the ruler).
pub fn row_height(ui: &egui::Ui) -> f32 {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    ui.fonts_mut(|f| f.row_height(&font)) + 2.0
}

enum HexEvent {
    Clicked(u64),
    ShiftClicked(u64),
    /// Re-request the current page (after Error / Cancelled).
    Retry,
}

pub fn show(ui: &mut egui::Ui, st: &AppState, tok: &Tokens, mem: &mut PanelMemory, scroll_to: Option<u64>) -> Vec<Action> {
    let mut acts = Vec::new();
    let doc = st.session();
    let hex = doc.hex();
    let m = metrics(ui, hex.size());
    mem.hex.bytes_per_row = choose_bytes_per_row(mem.hex.bytes_per_row, ui.available_width(), &m);
    // Header strip background (Wooden grain), filled in once its height is known.
    let strip = ui.painter().add(egui::Shape::Noop);
    let top = ui.cursor().top();
    if doc.file().is_none() {
        ui.horizontal(|ui| {
            strong(ui, tok, "Bytes");
            hide_button(ui, &mut acts);
        });
        let idle = if doc.dir().is_some() { "Select a file to view its bytes" } else { "No file opened" };
        show_bytes(ui, hex, tok, None, &|_| None, idle, &mut mem.hex, &m, None, strip, top);
        return acts;
    }
    header(ui, st, tok, &mut mem.hex, &mut acts);
    let view_rows: Vec<(Option<Span>, String)> = doc.view().map(|v| v.rows.iter().map(|r| (r.field.span, r.field.name.clone())).collect()).unwrap_or_default();
    let space = hex.space();
    let owner = move |off: u64| -> Option<String> { view_rows.iter().filter(|(s, _)| s.map(|s| s.contains(space, off)).unwrap_or(false)).min_by_key(|(s, _)| s.map(|s| s.len).unwrap_or(u64::MAX)).map(|(_, n)| format!("field: {n}")) };
    if let Some(ev) = show_bytes(ui, hex, tok, st.selection().span(), &owner, "No file opened", &mut mem.hex, &m, scroll_to, strip, top) {
        acts.push(match ev {
            HexEvent::Clicked(off) => Action::HexClick(off),
            HexEvent::ShiftClicked(off) => Action::HexShiftClick(off),
            HexEvent::Retry => Action::HexRetry,
        });
    }
    acts
}

fn hide_button(ui: &mut egui::Ui, acts: &mut Vec<Action>) {
    if ui.small_button("Hide bytes").on_hover_text(menu::tip("Hide the byte view", MenuCmd::TogglePanel(Panel::Bytes))).clicked() {
        acts.push(Action::TogglePanel(Panel::Bytes));
    }
}

/// Header groups A/B/C, laid out by last frame's measured widths (`layout::hex_header_lines`).
fn header(ui: &mut egui::Ui, st: &AppState, tok: &Tokens, mem: &mut HexMemory, acts: &mut Vec<Action>) {
    let doc = st.session();
    let hex = doc.hex();
    let ready = hex.page().map(|pg| !pg.is_empty()).unwrap_or(false);
    let n_groups = if ready { 3 } else { 1 };
    let gap = ui.spacing().item_spacing.x;
    let lines = hex_header_lines(mem.groups, gap, ui.available_width());
    let last = lines[..n_groups].iter().copied().max().unwrap_or(0);
    let mut line_h: f32 = 0.0;
    for line in 0..=last {
        let r = ui.horizontal(|ui| {
            for g in (0..n_groups).filter(|g| lines[*g] == line) {
                let r = ui.horizontal(|ui| match g {
                    0 => group_space(ui, st, tok, acts),
                    1 => mem.groups[1] = group_paging(ui, hex.page_base(), hex.size(), acts),
                    _ => group_goto(ui, st, acts),
                });
                // Paging plans its line with the wide width (it falls back to compact itself).
                if g != 1 {
                    mem.groups[g] = r.response.rect.width();
                }
            }
        });
        line_h = line_h.max(r.response.rect.height());
    }
    mem.line_h = line_h + ui.spacing().item_spacing.y;
}

/// Group A: mode chip ("File" / "Decompressed block" / "Reassembled record"), space buttons,
/// Hide bytes.
fn group_space(ui: &mut egui::Ui, st: &AppState, tok: &Tokens, acts: &mut Vec<Action>) {
    let p = &tok.p;
    let hex = st.session().hex();
    let (word, tint, tip) = match hex.space() {
        Space::File => ("File", p.hex_file_space, CHIP_FILE_TIP.to_string()),
        Space::Block(_) => ("Decompressed block", p.hex_derived_space, format!("{CHIP_DERIVED_TIP}\n{}", hex.space_title())),
        Space::Record(_) => ("Reassembled record", p.hex_derived_space, format!("{CHIP_DERIVED_TIP}\n{}", hex.space_title())),
    };
    egui::Frame::new()
        .fill(p.data)
        .stroke(egui::Stroke::new(1.0, tint))
        .inner_margin(egui::Margin::symmetric(4, 1))
        .corner_radius(tok.w.rounding)
        .show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(word).color(tint).strong()).extend().selectable(false));
        })
        .response
        .on_hover_text(tip);
    // Space switcher: file vs derived spaces of the current view.
    let spaces: Vec<SpaceInfo> = st.session().view().map(|v| v.spaces.clone()).unwrap_or_default();
    if !spaces.is_empty() {
        if hex.space() != Space::File && ui.small_button("show file bytes").clicked() {
            acts.push(Action::HexShowSpace(Space::File));
        }
        for s in spaces {
            if hex.space() != s.space && ui.small_button(format!("show {}", s.space.label())).on_hover_text(&s.title).clicked() {
                acts.push(Action::HexShowSpace(s.space));
            }
        }
    }
    hide_button(ui, acts);
}

/// Texts of the paging group B for the page at `base` of a `size`-byte space with `page`-byte
/// pages: the wide label, the compact label, and the tooltips that keep every detail.
#[derive(Clone, Debug, PartialEq)]
pub struct PagingText {
    /// "page 2/3 · 0x10000..0x20000 of 150000 bytes"
    pub wide: String,
    /// "2/3"
    pub compact: String,
    /// "Page 2 of 3 · 0x10000..0x20000 of 150000 bytes · 64 KiB per page"
    pub tip: String,
    /// "Previous page (0x0..0x10000)"; None on the first page.
    pub prev: Option<(u64, String)>,
    /// "Next page (0x20000..0x24a10)"; None on the last page.
    pub next: Option<(u64, String)>,
}

pub fn paging_text(base: u64, size: u64, page: u64) -> PagingText {
    let pages = size.div_ceil(page).max(1);
    let cur = base / page;
    let end = |b: u64| b.saturating_add(page).min(size);
    let range = format!("0x{base:x}..0x{:x}", end(base));
    let per = if page % 1024 == 0 { format!("{} KiB", page / 1024) } else { format!("{page} bytes") };
    PagingText {
        wide: format!("page {}/{pages} · {range} of {size} bytes", cur + 1),
        compact: format!("{}/{pages}", cur + 1),
        tip: format!("Page {} of {pages} · {range} of {size} bytes · {per} per page", cur + 1),
        prev: (cur > 0).then(|| (base - page, format!("Previous page (0x{:x}..0x{:x})", base - page, base))),
        next: (cur + 1 < pages).then(|| (base + page, format!("Next page (0x{:x}..0x{:x})", base + page, end(base + page)))),
    }
}

/// Group B: "◀ page · page 1/3 · 0x0..0x10000 of 150000 bytes · page ▶" when it fits the rest of
/// its header line, otherwise the compact "◀ · 1/3 · ▶" (chosen per frame from the measured wide
/// width; the label never wraps and the full text is in the tooltips). Both buttons are always
/// shown (disabled at the ends). Returns the wide width, which plans the header lines.
fn group_paging(ui: &mut egui::Ui, base: u64, size: u64, acts: &mut Vec<Action>) -> f32 {
    let t = paging_text(base, size, LIMITS.hex_page_bytes);
    let pad = 2.0 * ui.spacing().button_padding.x;
    let color = ui.visuals().text_color();
    let width = |s: &str, style: egui::TextStyle| {
        let font = style.resolve(ui.style());
        ui.fonts_mut(|f| f.layout_no_wrap(s.to_string(), font, color).size().x)
    };
    let wide_w = width("◀ page", egui::TextStyle::Button) + width(&t.wide, egui::TextStyle::Body) + width("page ▶", egui::TextStyle::Button) + 2.0 * pad + 2.0 * ui.spacing().item_spacing.x;
    let compact = wide_w + PACK_SLACK > ui.available_width();
    let (prev, label, next) = if compact { ("◀", &t.compact, "▶") } else { ("◀ page", &t.wide, "page ▶") };
    let next_w = (width(next, egui::TextStyle::Button) + pad).max(LAYOUT.glyph_button[0]);
    let label_text_w = width(label, egui::TextStyle::Body);
    let button = |s: &str| egui::Button::new(s).min_size(egui::vec2(LAYOUT.glyph_button[0], LAYOUT.glyph_button[1]));
    let r = ui.add_enabled(t.prev.is_some(), button(prev)).on_disabled_hover_text("First page");
    if let Some((to, tip)) = &t.prev {
        if r.on_hover_text(tip).clicked() {
            acts.push(Action::HexPage(*to));
        }
    }
    if compact {
        // Even "n/N" may not fit beside the buttons (huge spaces): elide it, keep "▶" reachable.
        let room = (ui.available_width() - next_w - ui.spacing().item_spacing.x - PACK_SLACK).max(0.0);
        let label_w = label_text_w.min(room);
        ui.allocate_ui(egui::vec2(label_w, ui.spacing().interact_size.y), |ui| ui.add(egui::Label::new(label.as_str()).truncate()).on_hover_text(&t.tip));
    } else {
        ui.add(egui::Label::new(label.as_str()).wrap_mode(egui::TextWrapMode::Extend)).on_hover_text(&t.tip);
    }
    let r = ui.add_enabled(t.next.is_some(), button(next)).on_disabled_hover_text("Last page");
    if let Some((to, tip)) = &t.next {
        if r.on_hover_text(tip).clicked() {
            acts.push(Action::HexPage(*to));
        }
    }
    wide_w
}

/// Group C: Go-to field + Go to.
fn group_goto(ui: &mut egui::Ui, st: &AppState, acts: &mut Vec<Action>) {
    let mut goto = st.session().goto().to_string();
    let tip = menu::tip("Go to Offset", MenuCmd::GotoOffset);
    let r = ui.add(egui::TextEdit::singleline(&mut goto).hint_text("Go to offset (0x… or decimal)").desired_width(LAYOUT.goto_width).id(egui::Id::new(GOTO_ID))).on_hover_text(tip);
    if r.changed() {
        acts.push(Action::SetGoto(goto));
    }
    if ui.button("Go to").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
        acts.push(Action::GotoOffset);
    }
}

/// Selected byte index range `[a, b]` of a row starting at `row_off` with `n` bytes.
pub fn row_selection(sel: Option<Span>, space: Space, row_off: u64, n: u64) -> Option<(u64, u64)> {
    let s = sel.filter(|s| s.space == space && s.len > 0)?;
    let end = s.offset.saturating_add(s.len);
    let a = s.offset.max(row_off);
    let b = end.min(row_off + n);
    (a < b).then(|| (a - row_off, b - 1 - row_off))
}

/// Draws the current page state. `idle_text` is shown when no page is requested.
#[allow(clippy::too_many_arguments)]
fn show_bytes(ui: &mut egui::Ui, hex: &HexState, tok: &Tokens, sel: Option<Span>, owner: &dyn Fn(u64) -> Option<String>, idle_text: &str, mem: &mut HexMemory, m: &HexMetrics, scroll_to: Option<u64>, strip: egui::layers::ShapeIdx, top: f32) -> Option<HexEvent> {
    let p = &tok.p;
    let page = match hex.load() {
        PageState::Idle => {
            centered(ui, |ui| {
                ui.label(RichText::new(idle_text).color(p.text_muted));
            });
            return None;
        }
        PageState::Loading(_) => {
            centered(ui, |ui| {
                ui.spinner();
                ui.label(RichText::new("Loading bytes…").color(p.text_muted));
            });
            return None;
        }
        PageState::Error(e) => {
            let mut retry = false;
            centered(ui, |ui| {
                ui.label(RichText::new(format!("✖ Could not read bytes: {e}")).color(tok.status(Status::Invalid)));
                retry = ui.button("Retry").clicked();
            });
            return retry.then_some(HexEvent::Retry);
        }
        PageState::Cancelled => {
            let mut retry = false;
            centered(ui, |ui| {
                ui.label(RichText::new("⏹ Loading cancelled").color(p.text_muted));
                retry = ui.button("Load bytes").clicked();
            });
            return retry.then_some(HexEvent::Retry);
        }
        PageState::Ready(pg) if pg.is_empty() => {
            let t = if hex.size() == 0 { "File is empty (0 bytes)".to_string() } else { format!("No bytes at 0x{:x} (space is {} bytes)", hex.page_base(), hex.size()) };
            centered(ui, |ui| {
                ui.label(RichText::new(t).color(p.text_muted));
            });
            return None;
        }
        PageState::Ready(pg) => pg,
    };
    let n = mem.bytes_per_row;
    let d = m.digits;
    let cw = m.cw;
    let rows = (page.len() as u64).div_ceil(n) as usize;
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let row_h = row_height(ui);
    let space = hex.space();
    let derived = space != Space::File;
    let gutter_col = if derived { p.hex_derived_space } else { p.hex_file_space };
    let offset_col = if derived { p.hex_derived_space } else { p.hex_offset };
    // Light centering: at most three glyphs of left gutter, otherwise left-aligned.
    let lead = ((ui.available_width() - m.width(n)) / 2.0).clamp(0.0, 3.0 * cw);
    let content_w = lead + hex_row_chars(n, d) as f32 * cw;
    let text_at = |painter: &egui::Painter, x: f32, y: f32, t: &str, c: egui::Color32| {
        painter.text(egui::pos2(x, y), egui::Align2::LEFT_CENTER, t, font.clone(), c);
    };
    // Ruler: OFFSET · 00..0f · ASCII; follows the rows' horizontal scroll.
    egui::ScrollArea::horizontal()
        .id_salt("hex_ruler")
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .scroll_source(egui::scroll_area::ScrollSource::NONE)
        .horizontal_scroll_offset(mem.hscroll)
        .show(ui, |ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(content_w, row_h), Sense::hover());
            let (x0, y) = (rect.left() + lead, rect.center().y);
            let painter = ui.painter();
            text_at(painter, x0, y, "OFFSET", gutter_col);
            for i in 0..n {
                text_at(painter, x0 + byte_col(i, n, d) as f32 * cw, y, &format!("{i:02x}"), gutter_col);
            }
            text_at(painter, x0 + ascii_col(0, n, d) as f32 * cw, y, "ASCII", gutter_col);
        });
    // Header strip (header lines + ruler) background.
    let strip_rect = egui::Rect::from_x_y_ranges(ui.max_rect().x_range(), top..=ui.cursor().top()).expand2(egui::vec2(LAYOUT.center_margin[0], 0.0));
    if let Some(s) = tok.grain_shape(strip_rect) {
        ui.painter().set(strip, s);
    }
    let mut area = egui::ScrollArea::both().id_salt("hex_scroll").auto_shrink([false, false]);
    if let Some(t) = scroll_to {
        if t >= hex.page_base() {
            let row = (t - hex.page_base()) / n;
            area = area.vertical_scroll_offset((row as f32 - 3.0).max(0.0) * row_h);
        }
    }
    let page_base = hex.page_base();
    let mut new_hover = None;
    let mut ev = None;
    let rounding = f32::from(tok.w.rounding);
    let shift = ui.input(|i| i.modifiers.shift);
    let origin = ui.input(|i| i.pointer.press_origin());
    let press = origin.or(mem.press);
    mem.press = origin;
    let out = area.show_rows(ui, row_h, rows, |ui, range| {
        ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
        for r in range {
            let row_off = page_base + r as u64 * n;
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(content_w, row_h), Sense::click());
            if !ui.is_rect_visible(rect) {
                continue;
            }
            let x0 = rect.left() + lead;
            let y = rect.center().y;
            let cell = |i: u64| egui::Rect::from_min_max(egui::pos2(x0 + byte_col(i, n, d) as f32 * cw - 1.0, rect.top()), egui::pos2(x0 + (byte_col(i, n, d) + 2) as f32 * cw + 1.0, rect.bottom()));
            let acell = |i: u64| egui::Rect::from_min_max(egui::pos2(x0 + ascii_col(i, n, d) as f32 * cw, rect.top()), egui::pos2(x0 + (ascii_col(i, n, d) + 1) as f32 * cw, rect.bottom()));
            let have = |i: u64| ((row_off - page_base + i) as usize) < page.len();
            // Pointer → byte of this row (pure mapping).
            let at = |pos: egui::Pos2| byte_at((pos.x - x0) / cw, n, d).filter(|&i| have(i));
            let hovered = resp.hover_pos().and_then(at);
            let painter = ui.painter();
            let sel_range = row_selection(sel, space, row_off, n);
            if let Some((a, b)) = sel_range {
                let edge = egui::Stroke::new(1.0, p.sel_edge);
                for r in [cell(a).union(cell(b)), acell(a).union(acell(b))] {
                    painter.rect_filled(r, rounding, p.sel_bg);
                    painter.rect_stroke(r, rounding, edge, egui::StrokeKind::Inside);
                }
            }
            if let Some(i) = hovered {
                let st = egui::Stroke::new(1.0, p.outline);
                painter.rect_stroke(cell(i), rounding, st, egui::StrokeKind::Outside);
                painter.rect_stroke(acell(i), rounding, st, egui::StrokeKind::Outside);
            }
            text_at(painter, x0, y, &format!("{row_off:0w$x}", w = d as usize), offset_col);
            for i in 0..n {
                let Some(&b) = page.get((row_off - page_base + i) as usize) else { break };
                let selected = sel_range.map(|(a, z)| i >= a && i <= z).unwrap_or(false);
                text_at(painter, x0 + byte_col(i, n, d) as f32 * cw, y, &format!("{b:02x}"), if selected { p.sel_fg } else { p.hex_byte });
                let ch = if (0x20..0x7f).contains(&b) { b as char } else { '.' };
                text_at(painter, x0 + ascii_col(i, n, d) as f32 * cw, y, &ch.to_string(), if selected { p.sel_fg } else { p.hex_ascii });
            }
            if let Some(i) = hovered {
                let off = row_off + i;
                new_hover = Some(off);
                let b = page[(off - page_base) as usize];
                let where_ = match space {
                    Space::File => format!("offset 0x{off:x} ({off})"),
                    Space::Block(_) => format!("block offset 0x{off:x} (not a file offset)"),
                    Space::Record(_) => format!("record offset 0x{off:x} (not a file offset)"),
                };
                let resp = resp.clone().on_hover_ui_at_pointer(|ui| {
                    ui.monospace(format!("{where_}\nbyte 0x{b:02x} = {b} = 0b{b:08b} = '{}'", if (0x20..0x7f).contains(&b) { b as char } else { '·' }));
                    if let Some(o) = owner(off) {
                        ui.label(o);
                    }
                });
                resp.on_hover_cursor(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() {
                // The byte under the PRESS, not the release: egui accepts a click with a few
                // points of movement, which may end on the neighbouring byte.
                if let Some(i) = press.or(resp.interact_pointer_pos()).and_then(at) {
                    let off = row_off + i;
                    ev = Some(if shift { HexEvent::ShiftClicked(off) } else { HexEvent::Clicked(off) });
                }
            }
        }
    });
    mem.hscroll = out.state.offset.x;
    mem.hover = new_hover;
    ev
}

fn centered(ui: &mut egui::Ui, f: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(LAYOUT.status_pad * 4.0);
    ui.vertical_centered(f);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Space, Span};
    use crate::ui::layout::center_min;
    use crate::ui::theme::{apply_theme, THEMES};

    #[test]
    fn paging_text_first_middle_last() {
        let p = 65536;
        let first = paging_text(0, 150000, p);
        assert_eq!((first.wide.as_str(), first.compact.as_str()), ("page 1/3 · 0x0..0x10000 of 150000 bytes", "1/3"));
        assert_eq!(first.tip, "Page 1 of 3 · 0x0..0x10000 of 150000 bytes · 64 KiB per page");
        assert_eq!(first.prev, None);
        assert_eq!(first.next, Some((0x10000, "Next page (0x10000..0x20000)".into())));
        let mid = paging_text(0x10000, 150000, p);
        assert_eq!(mid.compact, "2/3");
        assert_eq!(mid.prev, Some((0, "Previous page (0x0..0x10000)".into())));
        assert_eq!(mid.next, Some((0x20000, "Next page (0x20000..0x249f0)".into())));
        let last = paging_text(0x20000, 150000, p);
        assert_eq!((last.compact.as_str(), last.next.is_none()), ("3/3", true));
        let one = paging_text(0, 16, p);
        assert_eq!((one.compact.as_str(), one.prev.is_none(), one.next.is_none()), ("1/1", true, true));
    }

    /// 0009 item 1: at 4 bytes per row (the center at its minimum = both side panels at the
    /// minimum window, and at the zoom ceiling, which keeps the same logical center) the paging
    /// group — both buttons included — ends inside the byte area for the first, middle and last
    /// page of multi-page files, in every theme and at zoom 1.0 and 2.0. With room it stays wide.
    #[test]
    fn paging_group_fits_the_four_byte_center() {
        for t in THEMES {
            for z in [1.0f32, 2.0] {
                let ctx = egui::Context::default();
                let _ = apply_theme(&ctx, t, &mut Vec::new());
                ctx.set_zoom_factor(z);
                for size in [102_400u64, 150_000, 10_000_000_000_000_000_000] {
                    let bases = [0, (size / 2) / 65536 * 65536, (size - 1) / 65536 * 65536];
                    for base in bases {
                        for wide_room in [false, true] {
                            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0 * z, 900.0 * z))), ..Default::default() };
                            let _ = ctx.run_ui(input, |ui| {
                                let m = metrics(ui, size);
                                // The hex area inside the center margins at 4 bytes per row.
                                let avail = if wide_room { 1000.0 } else { center_min(&m) - 2.0 * LAYOUT.center_margin[0] };
                                assert!(wide_room || choose_bytes_per_row(4, avail, &m) == 4, "{}: center is not at 4 B/row", t.id);
                                let rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(avail, 200.0));
                                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                                let mut acts = Vec::new();
                                let mut wide = 0.0;
                                let r = child.horizontal(|ui| wide = group_paging(ui, base, size, &mut acts));
                                let w = r.response.rect.width();
                                assert!(r.response.rect.right() <= rect.right() + 0.5, "{} z{z} size {size} base {base:#x}: group {w} > center {avail}", t.id);
                                if wide_room && wide + PACK_SLACK <= avail {
                                    assert!((w - wide).abs() < 2.0, "{}: wide width {wide} vs drawn {w}", t.id);
                                }
                            });
                        }
                    }
                }
            }
        }
    }

    /// Selection painting per row is byte-absolute: the same span highlights the same bytes at
    /// 16, 8 and 4 bytes per row.
    #[test]
    fn row_selection_is_byte_absolute() {
        let s = Some(Span::file(0x13, 6)); // bytes 0x13..=0x18
        for n in [16u64, 8, 4] {
            let mut got = Vec::new();
            for row in 0..(0x40 / n) {
                if let Some((a, b)) = row_selection(s, Space::File, row * n, n) {
                    got.extend((a..=b).map(|i| row * n + i));
                }
            }
            assert_eq!(got, (0x13..0x19).collect::<Vec<u64>>(), "{n} bytes/row");
        }
        assert_eq!(row_selection(s, Space::Block(0), 0x10, 16), None, "other space");
        assert_eq!(row_selection(Some(Span::file(5, 0)), Space::File, 0, 16), None, "empty span");
    }
}
