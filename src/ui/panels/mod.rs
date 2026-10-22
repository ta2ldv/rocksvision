//! Panels: each `show` renders `&AppState` with the theme `Tokens` and returns the `Action`s
//! the user requested. Panels never mutate application state; only UI-local visual memory
//! (measured widths, hover, scroll offsets) lives in `PanelMemory`.

pub mod about;
pub mod explain;
pub mod help;
pub mod hex;
pub mod nav;
pub mod records;
pub mod shortcuts;
pub mod status;
pub mod toolbar;

use super::layout::LAYOUT;
use super::theme::Tokens;
use crate::model::Status;
use eframe::egui::{self, RichText};

/// UI-local visual memory that is not application state.
pub struct PanelMemory {
    /// Measured widths of the five atomic toolbar groups (path group at its minimum), used to
    /// plan the toolbar rows and the zoom ceiling.
    pub toolbar_w: [f32; toolbar::GROUPS],
    /// Height of one toolbar row (incl. spacing), measured.
    pub toolbar_row_h: f32,
    /// Measured heights of the toolbar and status bar panels.
    pub toolbar_h: f32,
    pub status_h: f32,
    /// Side panel widths of the last frame (0 when collapsed) and whether they equal their
    /// (window-clamped) defaults — Reset View is disabled when everything is default.
    pub left_w: f32,
    pub right_w: f32,
    pub widths_default: bool,
    pub hex: hex::HexMemory,
    /// Layout epoch these values belong to (Reset View starts fresh).
    pub layout_epoch: u32,
}

impl Default for PanelMemory {
    fn default() -> Self {
        let r = LAYOUT.toolbar_reserved;
        PanelMemory {
            toolbar_w: [r * 0.28, LAYOUT.path_min + 40.0, r * 0.6, r * 0.08, r * 0.28],
            toolbar_row_h: 30.0,
            toolbar_h: 40.0,
            status_h: 26.0,
            left_w: LAYOUT.left_default,
            right_w: LAYOUT.right_default,
            widths_default: true,
            hex: hex::HexMemory::default(),
            layout_epoch: 0,
        }
    }
}

impl PanelMemory {
    /// Forgets view-local offsets when the app's layout epoch changed (Reset View).
    pub fn follow_epoch(&mut self, epoch: u32) {
        if self.layout_epoch != epoch {
            self.layout_epoch = epoch;
            self.hex.hscroll = 0.0;
        }
    }
}

/// First line of a side panel: `title` (truncated before the chevron) and a frameless chevron
/// button at the right end that shows a frame only on hover. Returns true when it was clicked.
pub fn title_line(ui: &mut egui::Ui, tok: &Tokens, title: &str, chevron: &str, tip: &str) -> bool {
    let h = tok.header_height(ui);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), egui::Sense::hover());
    tok.paint_grain(ui.painter(), rect.expand2(egui::vec2(8.0, 4.0)).intersect(ui.clip_rect()));
    let chev = egui::Rect::from_min_size(egui::pos2(rect.right() - h, rect.top()), egui::vec2(h, h));
    let text = egui::Rect::from_min_max(rect.min, egui::pos2(chev.left() - 2.0, rect.bottom()));
    ui.put(text, egui::Label::new(RichText::new(title).strong().color(tok.p.heading)).truncate().selectable(false)).on_hover_text(title);
    ui.put(chev, egui::Button::new(RichText::new(chevron).color(tok.p.text)).frame_when_inactive(false)).on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

/// Full-height strip of a collapsed side panel: restore chevron at the top and the panel name
/// rotated (−90° on the left edge, +90° on the right). The whole strip is one click target.
pub fn collapse_strip(ui: &mut egui::Ui, tok: &Tokens, name: &str, chevron: &str, left: bool, tip: &str) -> bool {
    let rect = ui.max_rect();
    let resp = ui.interact(rect, ui.id().with(("strip", name)), egui::Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip);
    let painter = ui.painter();
    if resp.hovered() {
        painter.rect_filled(rect, 0.0, tok.p.hover_bg);
    } else {
        tok.paint_grain(painter, rect);
    }
    let h = tok.header_height(ui);
    let body = egui::TextStyle::Body.resolve(ui.style());
    painter.text(egui::pos2(rect.center().x, rect.top() + h / 2.0), egui::Align2::CENTER_CENTER, chevron, body.clone(), tok.p.text);
    let galley = painter.layout_no_wrap(name.to_string(), body, tok.p.text_muted);
    let center = egui::pos2(rect.center().x, rect.top() + h + 8.0 + galley.size().x / 2.0);
    let angle = if left { -std::f32::consts::FRAC_PI_2 } else { std::f32::consts::FRAC_PI_2 };
    let pos = center - galley.size() / 2.0;
    painter.add(egui::epaint::TextShape::new(pos, galley, tok.p.text_muted).with_angle_and_anchor(angle, egui::Align2::CENTER_CENTER));
    resp.clicked()
}

/// Table/tree scope: labels are not text-selectable, so a click always selects the row/node.
pub fn table_style(ui: &mut egui::Ui) {
    ui.style_mut().interaction.selectable_labels = false;
}

/// Section heading/caption in the theme's heading role (never egui's strong color, which is
/// the pressed-button foreground and unreadable on panels in light-on-dark-accent themes).
pub fn strong(ui: &mut egui::Ui, tok: &Tokens, text: impl Into<String>) {
    ui.add(egui::Label::new(RichText::new(text.into()).strong().color(tok.p.heading)).wrap());
}

/// Button with the theme's bevel edges (no-op for flat themes).
pub fn btn(ui: &mut egui::Ui, tok: &Tokens, text: &str) -> egui::Response {
    let r = ui.button(text);
    tok.bevel(ui.painter(), r.rect);
    r
}

pub fn btn_sized(ui: &mut egui::Ui, tok: &Tokens, text: &str) -> egui::Response {
    let r = ui.add_sized([LAYOUT.start_button_width, tok.row_height(ui)], egui::Button::new(text));
    tok.bevel(ui.painter(), r.rect);
    r
}

/// Unwrappable value (hex, keys): monospace, horizontal scroll instead of widening the panel.
pub fn long_value(ui: &mut egui::Ui, id: &str, v: &str) {
    egui::ScrollArea::horizontal().id_salt(id).auto_shrink([false, true]).show(ui, |ui| {
        ui.add(egui::Label::new(RichText::new(v).monospace()).extend());
    });
}

/// Statuses in legend order (Explanation legend and Help share it).
pub const LEGEND: [Status; 8] = [Status::Present, Status::NotPresent, Status::Implicit, Status::Unsupported, Status::Unknown, Status::Invalid, Status::Truncated, Status::Limited];

pub fn status_meaning(s: Status) -> &'static str {
    match s {
        Status::Present => "Present: decoded from bytes in the file.",
        Status::NotPresent => "Not present: the format says this optional part is absent here.",
        Status::Implicit => "Implicit: derived or defaulted; no on-disk byte range.",
        Status::Unsupported => "Unsupported: exists but RocksVision does not decode it.",
        Status::Unknown => "Unknown: not identifiable (unknown tag/type, stale or unreadable context).",
        Status::Invalid => "Invalid: checksum mismatch or inconsistent structure.",
        Status::Truncated => "Truncated: the file ends before this structure does.",
        Status::Limited => "Resource limit: decoding stopped at a configured bound.",
    }
}

/// Status chip: symbol + word on a flat data-colored fill (never status color on chrome).
pub fn chip(ui: &mut egui::Ui, tok: &Tokens, s: Status) {
    chip_word(ui, tok, s, s.label(), status_meaning(s));
}

/// Chip with the status symbol/colour but its own word and tooltip (e.g. "✖ Untrusted").
pub fn chip_word(ui: &mut egui::Ui, tok: &Tokens, s: Status, word: &str, tip: &str) {
    let c = tok.status(s);
    egui::Frame::new()
        .fill(tok.p.data)
        .stroke(egui::Stroke::new(1.0, c))
        .inner_margin(egui::Margin::symmetric(4, 1))
        .corner_radius(tok.w.rounding)
        .show(ui, |ui| {
            ui.label(RichText::new(format!("{} {word}", s.symbol())).color(c).small());
        })
        .response
        .on_hover_text(tip);
}

/// Status-colored message on a flat data fill (used on chrome bars).
pub fn status_text(ui: &mut egui::Ui, tok: &Tokens, s: Status, text: &str) {
    egui::Frame::new().fill(tok.p.data).inner_margin(egui::Margin::symmetric(4, 1)).corner_radius(tok.w.rounding).show(ui, |ui| {
        ui.add(egui::Label::new(RichText::new(text).color(tok.status(s))).wrap());
    });
}
