//! Panels: each `show` renders `&AppState` with the theme `Tokens` and returns the `Action`s
//! the user requested. Panels never mutate application state; only UI-local visual memory
//! (measured widths, hover, scroll offsets) lives in `PanelMemory`.

pub mod about;
pub mod explain;
pub mod help;
pub mod hex;
pub mod lsm_map;
pub mod nav;
pub mod records;
pub mod shortcuts;
pub mod status;
pub mod toolbar;

use super::layout::LAYOUT;
use super::theme::Tokens;
use crate::app::map_facts::{MapFact, TIP_FACTS, ValueKind};
use crate::model::Status;
use eframe::egui::{self, RichText};

/// egui temp-data key of View › Show Tooltips for this frame (`set_tooltips`).
fn tooltips_id() -> egui::Id {
    egui::Id::new("app_tooltips_on")
}

/// egui temp-data key of the tooltip look of this frame (`set_tooltips`).
fn tip_look_id() -> egui::Id {
    egui::Id::new("app_tooltip_look")
}

/// The central tooltip look (0026 item 3): the theme's tooltip frame and text colour.
#[derive(Clone)]
struct TipLook {
    frame: egui::Frame,
    fg: egui::Color32,
}

/// The shell copies the persisted preference and the theme's tooltip look into the context once
/// per frame; every app-owned tooltip goes through `TipExt`, the one gate (0024 §5, 0026 §3).
pub fn set_tooltips(ctx: &egui::Context, on: bool, tok: &Tokens) {
    let look = TipLook { frame: tok.tip_frame(&ctx.global_style()), fg: tok.p.tip_fg };
    ctx.data_mut(|d| {
        d.insert_temp(tooltips_id(), on);
        d.insert_temp(tip_look_id(), look);
    });
}

/// Tooltips are on (default when the shell has not said otherwise, e.g. headless tests).
pub fn tooltips_on(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(tooltips_id())).unwrap_or(true)
}

/// Pointer gap of a pointer-anchored tooltip (egui's `on_hover_ui_at_pointer` uses 12).
const TIP_POINTER_GAP: f32 = 12.0;
/// Gap between a hovered object and a tooltip placed beside it.
const TIP_TARGET_GAP: f32 = 6.0;

/// Shows `t` with the theme's tooltip frame and text colour (the only place egui tooltips are
/// shown). egui keeps the popup inside the screen (`Popup::get_best_align` tries the anchor's
/// symmetries and menu alignments within `content_rect`; containers/popup.rs 474–502).
fn show_tip(mut t: egui::Tooltip<'_>, ctx: &egui::Context, add: impl FnOnce(&mut egui::Ui)) {
    let look = ctx.data(|d| d.get_temp::<TipLook>(tip_look_id()));
    if let Some(l) = &look {
        t.popup = t.popup.frame(l.frame);
    }
    t.show(|ui| {
        if let Some(l) = &look {
            ui.visuals_mut().override_text_color = Some(l.fg);
        }
        add(ui)
    });
}

/// A plain text tooltip body (max width as egui's `on_hover_text`, egui#5167).
fn text_body(text: egui::WidgetText) -> impl FnOnce(&mut egui::Ui) {
    move |ui: &mut egui::Ui| {
        ui.set_max_width(ui.spacing().tooltip_width);
        ui.add(egui::Label::new(text));
    }
}

/// The tooltip gate: `tip` / `tip_disabled` / `tip_ui` / `tip_at` replace egui's hover-text
/// APIs for every app-owned tooltip (src/ui uses nothing else; see arch_tests). Off: no tooltip;
/// the Explanation and status bar carry the essentials.
pub trait TipExt {
    fn tip(self, text: impl Into<egui::WidgetText>) -> Self;
    fn tip_disabled(self, text: impl Into<egui::WidgetText>) -> Self;
    /// A tooltip with custom contents at the pointer (hex byte details).
    fn tip_ui(self, add: impl FnOnce(&mut egui::Ui)) -> Self;
    /// A text tooltip for one object inside a large widget (a map box): anchored to the
    /// pointer's column across `target` (the object's visible rect), so it opens just below the
    /// object near the pointer (above it when there is no room), never at the widget's corner
    /// and without covering the object; kept inside the screen by egui.
    fn tip_at(self, target: egui::Rect, text: impl Into<egui::WidgetText>) -> Self;
    /// `tip_at` with custom contents (map fact tooltips).
    fn tip_at_ui(self, target: egui::Rect, add: impl FnOnce(&mut egui::Ui)) -> Self;
}

impl TipExt for egui::Response {
    fn tip_ui(self, add: impl FnOnce(&mut egui::Ui)) -> Self {
        if tooltips_on(&self.ctx) {
            show_tip(egui::Tooltip::for_enabled(&self).at_pointer().gap(TIP_POINTER_GAP), &self.ctx, add);
        }
        self
    }
    fn tip(self, text: impl Into<egui::WidgetText>) -> Self {
        if tooltips_on(&self.ctx) {
            show_tip(egui::Tooltip::for_enabled(&self), &self.ctx, text_body(text.into()));
        }
        self
    }
    fn tip_disabled(self, text: impl Into<egui::WidgetText>) -> Self {
        if tooltips_on(&self.ctx) {
            show_tip(egui::Tooltip::for_disabled(&self), &self.ctx, text_body(text.into()));
        }
        self
    }
    fn tip_at(self, target: egui::Rect, text: impl Into<egui::WidgetText>) -> Self {
        self.tip_at_ui(target, text_body(text.into()))
    }
    fn tip_at_ui(self, target: egui::Rect, add: impl FnOnce(&mut egui::Ui)) -> Self {
        if tooltips_on(&self.ctx) {
            // Tester P3 B gap (not reproduced headless): egui's `should_show_tooltip` returns
            // false WITHOUT scheduling a repaint when another widget of the layer had the
            // tooltip last pass (containers/tooltip.rs 318–332) or the response is not hovered
            // yet (335–338). A target with a pending tip therefore asks for one frame after the
            // delay itself, so a still pointer over a new cell / box gets its tip without a move.
            if !self.is_tooltip_open() {
                self.ctx.request_repaint_after_secs(TIP_DELAY_S);
            }
            let mut t = egui::Tooltip::for_enabled(&self).gap(TIP_TARGET_GAP);
            // `for_enabled` anchors to the whole widget; the object's column replaces it (the
            // tooltip starts from this anchor, tooltip.rs 112–138).
            t.popup = t.popup.anchor(tip_anchor(target, self.hover_pos()));
            show_tip(t, &self.ctx, add);
        }
        self
    }
}

/// Hover delay before the first tooltip and the grace time in which the next hovered target
/// shows its tooltip at once (0030 B, UI expert §B-4: 0.30 s / 0.40 s, so moving along a table
/// row shows the next cell's tooltip at once). One place; the theme style applies it.
pub const TIP_DELAY_S: f32 = 0.30;
pub const TIP_GRACE_S: f32 = 0.40;

/// Applies the central tooltip timing to a style (`theme::apply_theme`). Tooltips open on hover
/// alone: egui shows a tooltip only for a hovered response after `tooltip_delay` of stillness
/// (containers/tooltip.rs 346–377); no click or selection is involved.
pub fn tip_timing(style: &mut egui::Style) {
    style.interaction.tooltip_delay = TIP_DELAY_S;
    style.interaction.tooltip_grace_time = TIP_GRACE_S;
}

/// A single-line label elided with `…` when it does not fit (0030 B) — the only way src/ui
/// truncates a label (see arch_tests). egui's own elided-text tooltip (a second, unthemed popup,
/// `Label::show_tooltip_when_elided`, widgets/label.rs 285) is off; instead the gate shows ONE
/// tooltip: the full text only when the text was actually elided, or an explicit `tip` (richer
/// meaning/source, which then carries the full text itself where it matters) always. A fully
/// visible, self-explanatory label without a tip has no tooltip.
pub struct Elided {
    label: egui::Label,
    tip: Option<String>,
    /// No tooltip of its own: the caller shows one for a larger target (a table row, 0030 B-2)
    /// and reads `was_elided` from the response.
    quiet: bool,
}

impl Elided {
    pub fn new(label: egui::Label) -> Self {
        Elided { label: label.truncate().show_tooltip_when_elided(false), tip: None, quiet: false }
    }
    /// A cell label whose tooltip is the row's (`TipExt::tip_at` on the row response).
    pub fn quiet(label: egui::Label) -> Self {
        Elided { quiet: true, ..Elided::new(label) }
    }
    /// Extra information beyond the label's own text (shown whether or not the text is elided).
    pub fn tip(mut self, tip: impl Into<String>) -> Self {
        self.tip = Some(tip.into());
        self
    }
}

/// Whether a label laid out at `rect` lost text to elision: its intrinsic (unwrapped, uncut)
/// size exceeds the shown size (epaint `Galley::intrinsic_size`, egui `Label` sets it).
pub fn was_elided(resp: &egui::Response) -> bool {
    resp.intrinsic_size().is_some_and(|s| s.x > resp.rect.width() + 0.5 || s.y > resp.rect.height() + 0.5)
}

/// The tooltip text of an elided-capable label: `None` when nothing would be added.
pub fn elided_tip(full: &str, elided: bool, tip: Option<&str>) -> Option<String> {
    match (elided, tip) {
        (_, Some(t)) => Some(t.to_string()),
        (true, None) => Some(value_tip(full)),
        (false, None) => None,
    }
}

impl egui::Widget for Elided {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        if self.quiet {
            return ui.add(self.label);
        }
        let full = self.label.text().to_string();
        let resp = ui.add(self.label);
        match elided_tip(&full, was_elided(&resp), self.tip.as_deref()) {
            Some(t) => resp.tip(t),
            None => resp,
        }
    }
}

/// The body of a map fact tooltip (0026 §3.5): the object's name (Body strong), at most
/// `TIP_FACTS` facts in Small (label in `tip_muted`, value in `tip_fg`, keys in Monospace, the
/// muted `(derived)` / `(partial)` / `(unknown)` suffix), each value at most two lines then
/// `…`, then the open hint. Width min(380, 45 % of the window), at least 220.
pub fn fact_tip_body<'a>(tok: &'a Tokens, title: &'a str, facts: &'a [MapFact], hint: Option<&'a str>) -> impl FnOnce(&mut egui::Ui) + 'a {
    move |ui: &mut egui::Ui| {
        let [lo, hi] = LAYOUT.map_tip_w;
        let w = (ui.ctx().content_rect().width() * LAYOUT.map_tip_window_share).min(hi).max(lo);
        ui.set_min_width(lo);
        ui.set_max_width(w);
        ui.label(RichText::new(title).strong().color(tok.p.tip_fg));
        let small = egui::TextStyle::Small.resolve(ui.style());
        let mono = egui::FontId::monospace(small.size);
        for f in facts.iter().take(TIP_FACTS) {
            let mut job = egui::text::LayoutJob { wrap: egui::text::TextWrapping { max_width: w, max_rows: 2, break_anywhere: false, overflow_character: Some('…') }, ..Default::default() };
            job.append(&format!("{}  ", f.label), 0.0, egui::TextFormat::simple(small.clone(), tok.p.tip_muted));
            let font = if f.kind == ValueKind::Key { mono.clone() } else { small.clone() };
            job.append(&f.value, 0.0, egui::TextFormat::simple(font, tok.p.tip_fg));
            if let Some(s) = f.tip_suffix() {
                job.append(&format!(" {s}"), 0.0, egui::TextFormat::simple(small.clone(), tok.p.tip_muted));
            }
            ui.add(egui::Label::new(job));
        }
        if let Some(h) = hint {
            ui.label(RichText::new(h).small().color(tok.p.tip_fg));
        }
    }
}

/// The tooltip starts this far left of the pointer (0026 §3.1).
const TIP_POINTER_LEAD: f32 = 12.0;

/// Anchor of `tip_at`: a zero-width column 12 pt left of the pointer's x (the pointer clamped
/// into `target`, its centre without a pointer) spanning the object's height, so the tooltip
/// opens 6 pt below the object (above it without room; beside the pointer when neither fits).
pub fn tip_anchor(target: egui::Rect, pointer: Option<egui::Pos2>) -> egui::Rect {
    let x = pointer.map(|p| p.x.clamp(target.left(), target.right())).unwrap_or(target.center().x) - TIP_POINTER_LEAD;
    egui::Rect::from_min_max(egui::pos2(x, target.top()), egui::pos2(x, target.bottom()))
}

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
    /// The user's own side-panel widths (last frame not shrunk for zoom, 0019 §3).
    pub left_user: f32,
    pub right_user: f32,
    pub widths_default: bool,
    pub hex: hex::HexMemory,
    /// Layout epoch these values belong to (Reset View starts fresh).
    pub layout_epoch: u32,
}

impl Default for PanelMemory {
    fn default() -> Self {
        let r = LAYOUT.toolbar_reserved;
        PanelMemory {
            toolbar_w: [r * 0.28, r * 0.08, LAYOUT.path_min + 40.0, r * 0.6, r * 0.08, r * 0.28],
            toolbar_row_h: 30.0,
            toolbar_h: 40.0,
            status_h: 26.0,
            left_w: LAYOUT.left_default,
            right_w: LAYOUT.right_default,
            left_user: LAYOUT.left_default,
            right_user: LAYOUT.right_default,
            widths_default: true,
            hex: hex::HexMemory::default(),
            layout_epoch: 0,
        }
    }
}

impl PanelMemory {
    /// Forgets view-local offsets and the user's panel widths when the app's layout epoch
    /// changed (Reset View).
    pub fn follow_epoch(&mut self, epoch: u32) {
        if self.layout_epoch != epoch {
            self.layout_epoch = epoch;
            self.hex.hscroll = 0.0;
            self.left_user = LAYOUT.left_default;
            self.right_user = LAYOUT.right_default;
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
    ui.put(text, Elided::new(egui::Label::new(RichText::new(title).strong().color(tok.p.heading)).selectable(false)));
    ui.put(chev, egui::Button::new(RichText::new(chevron).color(tok.p.text)).frame_when_inactive(false)).tip(tip).on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

/// Full-height strip of a collapsed side panel: restore chevron at the top and the panel name
/// rotated (−90° on the left edge, +90° on the right). The whole strip is one click target.
pub fn collapse_strip(ui: &mut egui::Ui, tok: &Tokens, name: &str, chevron: &str, left: bool, tip: &str) -> bool {
    let rect = ui.max_rect();
    let resp = ui.interact(rect, ui.id().with(("strip", name)), egui::Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).tip(tip);
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

/// Latched toggle button (LSM Map shown, selected Table | Map segment): `active_bg` fill,
/// `active_fg` text, 1 px `outline`, sunken bevel; same size as `btn`.
pub fn latched_btn(ui: &mut egui::Ui, tok: &Tokens, text: &str) -> egui::Response {
    let r = ui.add(egui::Button::new(RichText::new(text).color(tok.p.active_fg)).fill(tok.p.active_bg).stroke(egui::Stroke::new(1.0, tok.p.outline)));
    tok.bevel_sunken(ui.painter(), r.rect);
    r
}

/// Disabled button with the disabled-A+ look: muted text, no fill, thin `border_control`
/// outline, no hover effect.
pub fn disabled_btn(ui: &mut egui::Ui, tok: &Tokens, text: &str) -> egui::Response {
    let r = ui.add_enabled(false, egui::Button::new(RichText::new(text).color(tok.p.text_muted)).frame(false));
    ui.painter().rect_stroke(r.rect, tok.w.rounding, egui::Stroke::new(1.0, tok.p.border_control), egui::StrokeKind::Inside);
    r
}

pub fn btn_sized(ui: &mut egui::Ui, tok: &Tokens, text: &str) -> egui::Response {
    let r = ui.add_sized([LAYOUT.start_button_width, tok.row_height(ui)], egui::Button::new(text));
    tok.bevel(ui.painter(), r.rect);
    r
}

/// A monospace value broken into lines that fit the available width (`layout::wrap_value`):
/// the text to show and whether it was cut after `VALUE_MAX_LINES` lines.
pub fn fit_value(ui: &egui::Ui, v: &str) -> (String, bool) {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let cw = ui.fonts_mut(|f| f.glyph_width(&font, '0')).max(1.0);
    // One column of slack: non-ASCII glyphs ("·", "…") may be a little wider.
    let cols = ((ui.available_width() / cw).floor() as usize).saturating_sub(1);
    let (lines, cut) = super::layout::wrap_value(v, cols, super::layout::VALUE_MAX_LINES);
    (lines.join("\n"), cut)
}

/// Full text of a value for its tooltip (bounded).
pub fn value_tip(v: &str) -> String {
    crate::util::truncate_chars(v, crate::limits::TIP_TEXT_CHARS)
}

/// Value (hex, keys, paths): monospace, wrapped to the panel width — never a horizontal
/// scrollbar (design C1–C3); at most four lines ending in "…", the full value on hover.
pub fn long_value(ui: &mut egui::Ui, tok: &Tokens, v: &str) {
    let (text, _) = fit_value(ui, v);
    ui.add(egui::Label::new(RichText::new(text).monospace().color(tok.p.text)).extend()).tip(value_tip(v));
}

/// Statuses in legend order (Explanation legend and Help share it).
pub const LEGEND: [Status; 8] = [Status::Present, Status::NotPresent, Status::Implicit, Status::Unsupported, Status::Unknown, Status::Invalid, Status::Truncated, Status::Limited];

pub use crate::app::explanation::status_meaning;

/// Status chip: symbol + word on a flat data-colored fill (never status color on chrome).
pub fn chip(ui: &mut egui::Ui, tok: &Tokens, s: Status) {
    chip_word(ui, tok, s, s.label(), status_meaning(s));
}

/// Frame of the toolbar and the side panels: fill and `LAYOUT.panel_margin` (the one
/// definition `measure_layout` also counts with, `layout::panel_pad_h`).
pub fn panel_frame(fill: egui::Color32) -> egui::Frame {
    let m = LAYOUT.panel_margin;
    egui::Frame::new().fill(fill).inner_margin(egui::Margin::symmetric(m[0] as i8, m[1] as i8))
}

/// Frame of the status bar: `LAYOUT.panel_margin[0]` at the sides, `LAYOUT.status_pad` above
/// and below, the frame stroke (the shell and the toolbar measurement rig share it).
pub fn status_frame(fill: egui::Color32, border: egui::Color32) -> egui::Frame {
    egui::Frame::new().fill(fill).inner_margin(egui::Margin::symmetric(LAYOUT.panel_margin[0] as i8, LAYOUT.status_pad as i8)).stroke(egui::Stroke::new(LAYOUT.frame_stroke, border))
}

/// Frame of a collapsed panel's strip: no margin, the frame stroke.
pub fn strip_frame(fill: egui::Color32, border: egui::Color32) -> egui::Frame {
    egui::Frame::new().fill(fill).stroke(egui::Stroke::new(LAYOUT.frame_stroke, border))
}

/// Frame of a chip on a flat data fill, with an optional status/tint stroke
/// (`LAYOUT.chip_margin`, `layout::chip_pad_w`).
pub fn chip_frame(tok: &Tokens, stroke: Option<egui::Color32>) -> egui::Frame {
    let m = LAYOUT.chip_margin;
    let f = egui::Frame::new().fill(tok.p.data).inner_margin(egui::Margin::symmetric(m[0] as i8, m[1] as i8)).corner_radius(tok.w.rounding);
    match stroke {
        Some(c) => f.stroke(egui::Stroke::new(LAYOUT.frame_stroke, c)),
        None => f,
    }
}

/// Chip with the status symbol/colour but its own word and tooltip (e.g. "✖ Untrusted").
pub fn chip_word(ui: &mut egui::Ui, tok: &Tokens, s: Status, word: &str, tip: &str) {
    let c = tok.status(s);
    chip_frame(tok, Some(c))
        .show(ui, |ui| {
            ui.label(RichText::new(format!("{} {word}", s.symbol())).color(c).small());
        })
        .response
        .tip(tip);
}

/// Status-colored message on a flat data fill (used on chrome bars).
pub fn status_text(ui: &mut egui::Ui, tok: &Tokens, s: Status, text: &str) {
    chip_frame(tok, None).show(ui, |ui| {
        ui.add(egui::Label::new(RichText::new(text).color(tok.status(s))).wrap());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 0030 B: one tooltip, and only when it adds something: the full text of an elided label,
    /// an explicit tip always, nothing for a fully visible label without a tip.
    #[test]
    fn elided_tip_policy() {
        assert_eq!(elided_tip("log number (from file name)", true, None).as_deref(), Some("log number (from file name)"));
        assert_eq!(elided_tip("0 default", false, None), None);
        assert_eq!(elided_tip("—", false, Some("No bytes")).as_deref(), Some("No bytes"));
        assert_eq!(elided_tip("a…", true, Some("a b\nc")).as_deref(), Some("a b\nc"));
        assert_eq!((TIP_DELAY_S, TIP_GRACE_S), (0.30, 0.40));
    }

    /// Architect §5.6 (S4): an elided label and an elided table cell repeat the full text with
    /// the same cap (`limits::TIP_TEXT_CHARS`).
    #[test]
    fn elided_text_has_one_cap() {
        let long = "k".repeat(crate::limits::TIP_TEXT_CHARS + 50);
        let label = elided_tip(&long, true, None).unwrap();
        let mut v = crate::model::View { columns: vec![crate::model::col(crate::model::ColumnRole::Decoded, "Decoded")], ..Default::default() };
        v.rows.push(crate::model::Row { cells: vec![long.clone()], field: crate::model::Field::new("x", None, long.clone()) });
        let cell = crate::app::explanation::cell_tip(&v, 0, 0, &long, true).unwrap();
        assert_eq!(label, cell);
        assert_eq!(label.chars().count(), crate::limits::TIP_TEXT_CHARS + 1, "cut, then `…`");
    }

    /// 0026 item 3: the anchor of a map-box tooltip is the pointer column across the box.
    #[test]
    fn tip_anchor_is_the_pointer_column_of_the_target() {
        let r = egui::Rect::from_min_max(egui::pos2(100.0, 50.0), egui::pos2(300.0, 90.0));
        assert_eq!(tip_anchor(r, Some(egui::pos2(180.0, 70.0))), egui::Rect::from_min_max(egui::pos2(168.0, 50.0), egui::pos2(168.0, 90.0)));
        assert_eq!(tip_anchor(r, Some(egui::pos2(900.0, 70.0))).left(), 288.0, "pointer clamped into the box, 12 pt lead");
        assert_eq!(tip_anchor(r, None).left(), 188.0);
    }

    /// 0026 item 3: shown headlessly, a `tip_at` tooltip for a box at the bottom of a large
    /// widget opens next to the box near the pointer (above it: no room below), inside the
    /// screen, not over the box and not at the widget corner; with the theme frame and text.
    #[test]
    fn tip_at_opens_near_the_box_inside_the_screen() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|s| s.interaction.tooltip_delay = 0.0);
        let tok = Tokens::from_def(crate::ui::theme::theme_by_id("xcopy"));
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let boxr = egui::Rect::from_min_max(egui::pos2(150.0, 550.0), egui::pos2(400.0, 590.0));
        let pointer = egui::pos2(220.0, 570.0);
        let mut id = None;
        for i in 0..4 {
            let input = egui::RawInput { screen_rect: Some(screen), time: Some(i as f64), events: vec![egui::Event::PointerMoved(pointer)], ..Default::default() };
            crate::ui::test_frame(&ctx, input, |ui| {
                set_tooltips(ui.ctx(), true, &tok);
                let resp = ui.interact(screen, egui::Id::new("big"), egui::Sense::click());
                id = Some(resp.id);
                let _ = resp.tip_at(boxr, "000018.sst\nL0 · 1.2 MiB");
            });
        }
        let tip = ctx.memory(|m| m.area_rect(egui::Tooltip::tooltip_id(id.unwrap(), 0))).expect("tooltip shown");
        assert!(screen.contains_rect(tip), "inside the screen: {tip:?}");
        assert!(!tip.intersects(boxr), "does not cover the box: {tip:?}");
        assert!(tip.bottom() <= boxr.top() + 0.5 && (tip.left() - pointer.x).abs() < 40.0, "above the box near the pointer: {tip:?}");
        // Off: nothing.
        let input = egui::RawInput { screen_rect: Some(screen), time: Some(9.0), ..Default::default() };
        crate::ui::test_frame(&ctx, input, |ui| set_tooltips(ui.ctx(), false, &tok));
        assert!(!tooltips_on(&ctx));
    }
}
