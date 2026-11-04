//! eframe shell: owns the `AppState`, the resolved theme `Tokens`, textures and the platform
//! adapter. Each frame it applies job responses, syncs the native menu, reads keys, reports the
//! layout measurements for the zoom ceiling, renders the panels and applies the `Action`s they
//! return. Layout: toolbar → one-line status bar → navigation (left) → explanation (right) →
//! center column split into records (top) and bytes (bottom) by `ViewPreferences::center_split`.
//! Panels are collapsed only by the user (chevrons, strips, View menu, Option+⌘1/2/3 or
//! Ctrl+Alt+1/2/3); a collapsed side panel leaves a full-height restore strip, collapsed bytes a one-line strip.

mod keys;
pub mod layout;
pub mod menu;
pub mod panels;
pub mod platform;
pub mod theme;

/// Test helper: runs one headless egui frame and consumes its texture delta (epaint's
/// `TexturesDelta` asserts on drop with unapplied deltas when debug assertions are on).
#[cfg(test)]
pub(crate) fn test_frame(ctx: &egui::Context, input: egui::RawInput, f: impl FnMut(&mut egui::Ui)) {
    let mut out = ctx.run_ui(input, f);
    out.textures_delta.clear();
}

use crate::app::jobs::Worker;
use crate::app::prefs::Prefs;
use crate::app::{Action, AppState, Effect, Panel, ZoomFit};
use crate::branding;
use crate::limits::LIMITS;
use eframe::egui;
use layout::{LAYOUT, Measured, side_max};
use menu::{MenuCmd, MenuNode};
use panels::PanelMemory;
use platform::Platform;
use std::path::PathBuf;
use std::sync::Arc;
use theme::Tokens;

pub fn run(initial: Option<PathBuf>) -> eframe::Result<()> {
    let mut viewport = egui::ViewportBuilder::default().with_title(branding::NAME).with_inner_size(LAYOUT.window_size).with_min_inner_size(LAYOUT.window_min);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(branding::LOGO_PNG) {
        viewport = viewport.with_icon(Arc::new(icon));
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native(
        branding::NAME,
        options,
        Box::new(move |cc| {
            // The app handles zoom shortcuts itself (single step per key press, layout ceiling).
            cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
            let ctx = cc.egui_ctx.clone();
            let worker = Worker::spawn(Arc::new(move || ctx.request_repaint()));
            let mut app = App::new(AppState::new(worker), platform::native());
            // Preferences (theme id + zoom) live in eframe's per-user storage, never in inspected folders.
            let prefs = Prefs::load(|k| cc.storage.and_then(|s| s.get_string(k)));
            app.start(&cc.egui_ctx, &prefs, initial);
            Ok(Box::new(app))
        }),
    )
}

/// Startup actions from the stored preferences. A saved valid theme id is kept; an unknown or
/// empty id opens in `DEFAULT_THEME` (XCopy) through `theme_by_id`. The stored value is not
/// rewritten here; on exit `save` stores the resolved id, as before.
fn startup_actions(prefs: &Prefs, initial: Option<PathBuf>) -> Vec<Action> {
    let mut acts = vec![Action::SetTheme(theme::theme_by_id(&prefs.theme).id), Action::SetZoom(prefs.zoom)];
    if let Some(p) = initial {
        acts.push(Action::OpenPath(p));
    }
    acts
}

/// eframe storage key of the native window settings (eframe 0.36.2 `STORAGE_WINDOW_KEY`).
const WINDOW_KEY: &str = "window";
/// While the window size differs from the saved one, eframe auto-saves this often (else 30 s).
const RESIZE_SAVE_SECS: u64 = 1;

/// 0020 F3: eframe 0.36.2 saves the window's inner size divided by the egui zoom factor
/// (`egui_winit::WindowSettings::from_window(egui_ctx.zoom_factor(), ..)`), but restores it
/// before any app code runs, at zoom 1.0 — so a 1400×900 window at 160 % came back as
/// 875×562.5. This replaces the stored `inner_size_points` with the size in OS logical points
/// measured by the app (zoom-independent); everything else eframe stored (position, maximized,
/// fullscreen) is kept. None when the stored value has no such field (left as is).
fn window_settings_with_inner_size(stored: &str, size: [f32; 2]) -> Option<String> {
    const FIELD: &str = "inner_size_points:";
    let at = stored.find(FIELD)? + FIELD.len();
    let rest = &stored[at..];
    let end = if rest.starts_with("None") {
        4
    } else if rest.starts_with("Some((") {
        rest.find("))")? + 2
    } else {
        return None;
    };
    // Two decimals: f32 rounding of points × zoom must not leak into the file (1400.0001).
    let r = |v: f32| (v * 100.0).round() / 100.0;
    Some(format!("{}Some((x:{:?},y:{:?})){}", &stored[..at], r(size[0]), r(size[1]), &rest[end..]))
}

struct App {
    st: AppState,
    tok: Tokens,
    /// The window's inner size in OS logical points (zoom-independent) of the last frame, and
    /// the size last written to storage (F3).
    window_pts: Option<egui::Vec2>,
    saved_window: Option<egui::Vec2>,
    /// Theme id whose style is applied to egui (None until the first application).
    applied_theme: Option<&'static str>,
    grain_cache: Vec<(&'static str, egui::TextureHandle)>,
    logo: Option<egui::TextureHandle>,
    mem: PanelMemory,
    platform: Box<dyn Platform>,
    menu: Vec<MenuNode>,
}

/// "Show: View › Explanation Panel (Option+⌘2)" (Ctrl+Alt+2 off macOS).
fn show_hint(p: Panel) -> (&'static str, String) {
    let (name, item) = match p {
        Panel::Navigation => ("Navigation", "Navigation Panel"),
        Panel::Explanation => ("Explanation", "Explanation Panel"),
        Panel::Bytes => ("Bytes", "Byte View"),
    };
    (name, format!("Show: View › {item} ({})", menu::shortcut_text(MenuCmd::TogglePanel(p)).unwrap_or_default()))
}

impl App {
    fn new(st: AppState, platform: Box<dyn Platform>) -> Self {
        let tok = Tokens::from_def(theme::theme_by_id(st.prefs().theme_id()));
        App { st, tok, window_pts: None, saved_window: None, applied_theme: None, grain_cache: Vec::new(), logo: None, mem: PanelMemory::default(), platform, menu: menu::tree() }
    }

    /// Startup (also driven by the headless restore test): stored preferences, theme, and the
    /// restored zoom set in egui's options before the first frame, so the first frame is laid
    /// out at that zoom with the saved panel widths as the base of any squeeze (review N3).
    /// Not `set_zoom_factor`: that queues the change and egui rescales the *previous* frame's
    /// screen rect for it, which does not exist before the first frame.
    fn start(&mut self, ctx: &egui::Context, prefs: &Prefs, initial: Option<PathBuf>) {
        self.dispatch(ctx, startup_actions(prefs, initial));
        self.sync_theme(ctx);
        let zoom = self.st.prefs().zoom();
        ctx.options_mut(|o| o.zoom_factor = zoom);
    }

    /// Applies the app's theme id to egui when it changed. Touches only style + tokens.
    fn sync_theme(&mut self, ctx: &egui::Context) {
        if self.applied_theme != Some(self.st.prefs().theme_id()) {
            let def = theme::theme_by_id(self.st.prefs().theme_id());
            self.tok = theme::apply_theme(ctx, def, &mut self.grain_cache);
            self.applied_theme = Some(self.st.prefs().theme_id());
        }
    }

    /// Applies actions in order and executes the platform effects they request.
    fn dispatch(&mut self, ctx: &egui::Context, acts: Vec<Action>) {
        for a in acts {
            for e in self.st.apply(a) {
                match e {
                    Effect::PickFolder(start) => {
                        if let Some(p) = self.platform.pick_folder(start.as_deref()) {
                            self.st.apply(Action::OpenPath(p));
                        }
                    }
                    Effect::PickFile(start) => {
                        if let Some(p) = self.platform.pick_file(start.as_deref()) {
                            self.st.apply(Action::OpenPath(p));
                        }
                    }
                    Effect::FocusSearch => ctx.memory_mut(|m| m.request_focus(egui::Id::new(panels::toolbar::SEARCH_ID))),
                    Effect::FocusGoto => ctx.memory_mut(|m| m.request_focus(egui::Id::new(panels::hex::GOTO_ID))),
                    Effect::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                }
            }
        }
    }

    fn about_logo(&mut self, ctx: &egui::Context) -> Option<egui::TextureHandle> {
        if self.logo.is_none()
            && let Ok(icon) = eframe::icon_data::from_png_bytes(branding::LOGO_PNG)
        {
            let img = egui::ColorImage::from_rgba_unmultiplied([icon.width as usize, icon.height as usize], &icon.rgba);
            self.logo = Some(ctx.load_texture("about-logo", img, egui::TextureOptions::LINEAR));
        }
        self.logo.clone()
    }

    /// The window size changed since it was last saved.
    fn window_unsaved(&self) -> bool {
        match (self.window_pts, self.saved_window) {
            (Some(a), Some(b)) => (a - b).length() > 0.5,
            (a, b) => a.is_some() != b.is_some(),
        }
    }

    /// Measurements of the minimum layout (logical points). With `ceiling` (zoom ceiling, spec
    /// §8.1) the inputs do not depend on data: the byte rows use 16 offset digits, the hex
    /// header groups worst-case static texts (`hex::ceiling_groups`) and the toolbar always
    /// reserves the path-error line. Without it (row planning: records/bytes split) the live
    /// space size and last frame's measured header widths are used.
    fn measured(&self, ui: &egui::Ui, ceiling: bool) -> Measured {
        measure_layout(ui, &self.tok, &self.mem, self.st.session().hex().size(), ceiling)
    }
}

/// `App::measured` as a function of the UI memory (shared with the headless ceiling test).
pub(crate) fn measure_layout(ui: &egui::Ui, tok: &theme::Tokens, mem: &PanelMemory, hex_size: u64, ceiling: bool) -> Measured {
    let hex = panels::hex::metrics(ui, if ceiling { u64::MAX } else { hex_size });
    let rh = tok.row_height(ui);
    let line = ui.text_style_height(&egui::TextStyle::Body);
    let hex_row = panels::hex::row_height(ui);
    Measured {
        nav_min: LAYOUT.left_min,
        // Above 100 % the Explanation is squeezed no further than this (0020 §8).
        explain_min: LAYOUT.right_squeeze_min,
        strip: tok.header_height(ui),
        hex,
        toolbar_groups: mem.toolbar_w.to_vec(),
        toolbar_gap: ui.spacing().item_spacing.x,
        toolbar_row_h: mem.toolbar_row_h,
        // Frame (4 pt top/bottom + 1 pt stroke) + the path-error line (`status_text`: one body
        // line, 1 pt margins, item spacing), reserved whether or not it is shown.
        toolbar_pad_h: layout::panel_pad_h() + if ceiling { line + 2.0 + ui.spacing().item_spacing.y } else { 0.0 },
        status_h: mem.status_h,
        // Overhead with the notes collapsed + header + 3 table rows (0015 F2): the ceiling and
        // the split keep them at the minimum height.
        records_min_h: layout::records_min_h(
            mem.toolbar_row_h,
            ui.text_style_height(&egui::TextStyle::Small) + ui.spacing().item_spacing.y,
            tok.header_height(ui),
            rh,
            ui.spacing().item_spacing.y,
            ui.spacing().scroll.allocated_width(),
        ),
        hex_groups: if ceiling { panels::hex::ceiling_groups(ui) } else { mem.hex.groups },
        hex_gap: ui.spacing().item_spacing.x,
        hex_line_h: mem.hex.line_h,
        hex_ruler_h: hex_row,
        hex_row_h: hex_row,
        hex_pad_h: 2.0 * LAYOUT.center_margin[1] + ui.spacing().scroll.allocated_width(),
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frame_ui(ui);
    }

    /// Theme id + chosen zoom; and the window size eframe just stored, corrected to OS logical
    /// points (F3). eframe calls this at quit and on every auto-save, right after it wrote the
    /// window settings into the same storage.
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.st.prefs().persisted().store(|k, v| storage.set_string(k, v));
        if let Some(size) = self.window_pts
            && let Some(fixed) = storage.get_string(WINDOW_KEY).and_then(|s| window_settings_with_inner_size(&s, [size.x, size.y]))
        {
            storage.set_string(WINDOW_KEY, fixed);
        }
        self.saved_window = self.window_pts;
    }

    /// Saved soon after a resize (and at quit), not only every 30 s (F3).
    fn auto_save_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(if self.window_unsaved() { RESIZE_SAVE_SECS } else { 30 })
    }

    /// Only the theme id and zoom are persisted; panel layout persistence is deferred.
    fn persist_egui_memory(&self) -> bool {
        false
    }
}

impl App {
    /// One frame of the whole window (also driven headlessly by the layout tests).
    fn frame_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // The zoom this pass is laid out at. A zoom change requested during the frame is applied
        // to egui below but takes effect next pass; the squeeze must use this pass's geometry.
        let pass_zoom = ctx.zoom_factor();
        for text in self.st.poll() {
            ctx.copy_text(text);
        }
        self.sync_theme(&ctx);
        self.st.tick();
        self.mem.follow_epoch(self.st.prefs().layout_epoch());
        // Native menu: state mirrored from the app (checkmarks every frame), chosen items
        // dispatched as actions.
        self.platform.sync_menu(&ctx, &self.st);
        let cmds = self.platform.take_menu_commands();
        let acts: Vec<Action> = cmds.into_iter().filter_map(|c| c.action(&self.st)).collect();
        self.dispatch(&ctx, acts);
        // Keys first: a text field focused at frame start still owns Enter (search Enter is
        // dispatched here exactly once; the field only re-takes focus).
        let acts = keys::read(&ctx, &self.st, self.platform.has_native_menu(), egui::Id::new(panels::toolbar::SEARCH_ID));
        self.dispatch(&ctx, acts);

        // Geometry of this frame (logical points at the current zoom). The ceiling is computed
        // with BOTH side panels at their minimum (spec §8.1), never by hiding one, so a panel
        // toggle never changes the zoom.
        let screen = ctx.content_rect();
        // This pass's input was laid out at `pass_zoom`: points × zoom = OS logical points (F3).
        self.window_pts = Some(screen.size() * pass_zoom);
        let width = screen.width();
        let rh = self.tok.row_height(ui);
        let mut hexm = panels::hex::metrics(ui, self.st.session().hex().size());
        if self.st.session().hex().space() != crate::model::Space::File {
            hexm.digits += 1; // derived offsets print "+0…" (hex panel uses the same width)
        }
        let center_min = layout::center_min(&hexm);
        let strip = self.tok.header_height(ui);
        let (show_left, show_right, show_hex) = (self.st.prefs().show_left(), self.st.prefs().show_right(), self.st.prefs().show_hex());
        let (need_w, need_h) = self.measured(ui, true).need(true, true);
        let measured = self.measured(ui, false);
        self.dispatch(&ctx, vec![Action::LayoutFit(ZoomFit { at_zoom: pass_zoom, avail_w: width, avail_h: screen.height(), need_w, need_h })]);
        let zoom = self.st.prefs().zoom();
        if (ctx.zoom_factor() - zoom).abs() > 0.001 {
            ctx.set_zoom_factor(zoom);
        }

        let p = self.tok.p;
        let frame = panels::panel_frame;
        let e = self.st.prefs().layout_epoch();
        let mut acts = Vec::new();
        let fallback = if self.platform.has_native_menu() { None } else { Some(self.menu.as_slice()) };
        let view_is_default = self.st.prefs().is_default_layout() && self.mem.widths_default;
        let r = egui::Panel::top("toolbar")
            .frame(frame(p.toolbar).stroke(egui::Stroke::new(LAYOUT.frame_stroke, p.border)))
            .show(ui, |ui| acts = panels::toolbar::show(ui, &self.st, &self.tok, &mut self.mem, fallback, view_is_default));
        self.mem.toolbar_h = r.response.rect.height();
        self.dispatch(&ctx, std::mem::take(&mut acts));
        let hidden: Vec<(&str, String)> = [Panel::Navigation, Panel::Explanation, Panel::Bytes].into_iter().filter(|p| !self.st.prefs().shown(*p)).map(show_hint).collect();
        let r = egui::Panel::bottom("status").frame(panels::status_frame(p.toolbar, p.border)).resizable(false).show(ui, |ui| acts = panels::status::show(ui, &self.st, &self.tok, &hidden));
        self.mem.status_h = r.response.rect.height();
        self.dispatch(&ctx, std::mem::take(&mut acts));

        // Side panels: never hidden automatically; clamped so the center keeps 4 bytes per row
        // (Explanation shrinks first, then Navigation).
        let left = show_left.then_some((LAYOUT.left_min, LAYOUT.left_max));
        let right = show_right.then_some((LAYOUT.right_min, LAYOUT.right_max));
        let (left_max, _) = side_max(width, center_min, strip, left, self.mem.left_w, right);
        // 0019 §3 / 0020 §8 / review N3: one rule for startup, restore, zoom and resize. The
        // user's (saved) widths are the base; above 100 % (this pass's zoom) the side panels
        // shrink from them — Explanation first, down to `right_squeeze_min`, then Navigation —
        // until the center fits 8 bytes per row. Squeezed panels are drawn under separate ids
        // at exact widths, so the user's widths stay untouched and return on zoom out. At 100 %
        // nothing is squeezed (4 B/row at 900 pt is accepted).
        let squeeze = if pass_zoom > 1.0 + 1e-3 {
            layout::squeeze_panels(
                width,
                layout::center_want(&hexm),
                strip,
                show_left.then_some((LAYOUT.left_min, self.mem.left_user)),
                show_right.then_some((LAYOUT.right_squeeze_min, self.mem.right_user)),
            )
        } else {
            None
        };
        let sep = self.tok.w.separator;
        let mut widths_default = true;
        // One-shot scroll requests (row, byte, tree reveal) are taken once per frame.
        let scroll = self.st.take_scroll_request();
        if show_left {
            let panel = match squeeze {
                Some((l, _)) => egui::Panel::left(egui::Id::new(("nav_squeezed", e))).resizable(false).exact_size(l),
                None => egui::Panel::left(egui::Id::new(("nav", e))).resizable(true).default_size(LAYOUT.left_default.min(left_max)).size_range(LAYOUT.left_min..=left_max),
            };
            let r = panel.frame(frame(p.panel)).show_separator_line(false).show(ui, |ui| acts = panels::nav::show(ui, &self.st, &self.tok, scroll.tree));
            let rect = r.response.rect;
            self.mem.left_w = rect.width();
            if squeeze.is_none() {
                self.mem.left_user = rect.width();
            }
            widths_default &= (self.mem.left_user - LAYOUT.left_default.min(left_max)).abs() < 1.5;
            let x = rect.right() - sep.width / 2.0;
            ui.painter().line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], sep);
        } else {
            egui::Panel::left(egui::Id::new("nav_strip")).frame(panels::strip_frame(p.panel, p.border)).resizable(false).exact_size(strip).show(ui, |ui| {
                if panels::collapse_strip(ui, &self.tok, "Navigation", "▶", true, &menu::tip("Show Navigation", MenuCmd::TogglePanel(Panel::Navigation))) {
                    acts.push(Action::TogglePanel(Panel::Navigation));
                }
            });
            self.mem.left_w = 0.0;
        }
        self.dispatch(&ctx, std::mem::take(&mut acts));
        let (_, right_max) = side_max(width, center_min, strip, left, self.mem.left_w, right);
        if show_right {
            let panel = match squeeze {
                Some((_, r)) => egui::Panel::right(egui::Id::new(("explain_squeezed", e))).resizable(false).exact_size(r),
                None => egui::Panel::right(egui::Id::new(("explain", e))).resizable(true).default_size(LAYOUT.right_default.min(right_max)).size_range(LAYOUT.right_min..=right_max),
            };
            let r = panel.frame(frame(p.explain)).show_separator_line(false).show(ui, |ui| acts = panels::explain::show(ui, &self.st, &self.tok));
            let rect = r.response.rect;
            self.mem.right_w = rect.width();
            if squeeze.is_none() {
                self.mem.right_user = rect.width();
            }
            widths_default &= (self.mem.right_user - LAYOUT.right_default.min(right_max)).abs() < 1.5;
            let x = rect.left() + sep.width / 2.0;
            ui.painter().line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], sep);
        } else {
            egui::Panel::right(egui::Id::new("explain_strip")).frame(panels::strip_frame(p.panel, p.border)).resizable(false).exact_size(strip).show(ui, |ui| {
                if panels::collapse_strip(ui, &self.tok, "Explanation", "◀", false, &menu::tip("Show Explanation", MenuCmd::TogglePanel(Panel::Explanation))) {
                    acts.push(Action::TogglePanel(Panel::Explanation));
                }
            });
            self.mem.right_w = 0.0;
        }
        self.mem.widths_default = widths_default;
        self.dispatch(&ctx, std::mem::take(&mut acts));
        egui::CentralPanel::default().frame(egui::Frame::new().fill(p.canvas)).show(ui, |ui| acts = self.center(ui, rh, &measured, show_hex, scroll));
        self.dispatch(&ctx, std::mem::take(&mut acts));
        if self.st.prefs().show_about() {
            let logo = self.about_logo(&ctx);
            let acts = panels::about::show(&ctx, &self.st, &self.tok, logo.as_ref());
            self.dispatch(&ctx, acts);
        }
        let acts = panels::shortcuts::show(&ctx, &self.st, &self.tok);
        self.dispatch(&ctx, acts);
        let acts = panels::help::show(&ctx, &self.st, &self.tok);
        self.dispatch(&ctx, acts);
        if self.st.prefs().zoom_fit_pending() {
            // One over-fit frame seen: measure once more before lowering the zoom.
            ctx.request_repaint();
        }
        if self.st.needs_fast_repaint() {
            ctx.request_repaint_after(std::time::Duration::from_millis(LIMITS.busy_repaint_ms));
        }
        if self.st.watching_file() {
            ctx.request_repaint_after(std::time::Duration::from_secs(LIMITS.change_check_secs));
        }
        if let Some(left) = self.st.prefs().notice_remaining() {
            // Clear a timed notice (Reset View, copy) on time.
            ctx.request_repaint_after(left + std::time::Duration::from_millis(50));
        }
        if self.window_unsaved() {
            // eframe auto-saves only from a frame: one more frame after the resize interval.
            ctx.request_repaint_after(std::time::Duration::from_millis(RESIZE_SAVE_SECS * 1000 + 100));
        }
    }

    /// Center column: records above, bytes below, split by ratio with a draggable handle. The
    /// byte region keeps its header, ruler and three byte rows; collapsed, it is a one-line
    /// restore strip at the bottom.
    fn center(&mut self, ui: &mut egui::Ui, rh: f32, measured: &Measured, show_hex: bool, scroll: crate::app::ScrollRequest) -> Vec<Action> {
        let mut acts = Vec::new();
        let p = self.tok.p;
        let full = ui.available_rect_before_wrap();
        let margin = egui::vec2(LAYOUT.center_margin[0], LAYOUT.center_margin[1]);
        if !show_hex {
            let strip_h = rh;
            let rec_rect = egui::Rect::from_min_max(full.min, egui::pos2(full.right(), full.bottom() - strip_h));
            ui.scope_builder(egui::UiBuilder::new().max_rect(rec_rect.shrink2(margin)), |ui| {
                ui.set_clip_rect(rec_rect);
                acts = panels::records::show(ui, &self.st, &self.tok, scroll.row);
            });
            let strip = egui::Rect::from_min_max(egui::pos2(full.left(), full.bottom() - strip_h), full.max);
            let resp = ui.interact(strip, ui.id().with("bytes_strip"), egui::Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(show_hint(Panel::Bytes).1);
            ui.painter().rect_filled(strip, 0.0, if resp.hovered() { p.hover_bg } else { p.panel });
            if !resp.hovered() {
                self.tok.paint_grain(ui.painter(), strip);
            }
            ui.painter().line_segment([strip.left_top(), strip.right_top()], egui::Stroke::new(1.0, p.border));
            let text = format!("Bytes hidden · click to show ({})", menu::shortcut_text(MenuCmd::TogglePanel(Panel::Bytes)).unwrap_or_default());
            ui.painter().text(strip.center(), egui::Align2::CENTER_CENTER, text, egui::TextStyle::Body.resolve(ui.style()), p.text_muted);
            if resp.clicked() {
                acts.push(Action::TogglePanel(Panel::Bytes));
            }
            return acts;
        }
        let handle = LAYOUT.split_handle;
        let hex_min = measured.bytes_h(full.width());
        // Records first at short heights: the panel's measured overhead (title, location,
        // notes) + header + TABLE_MIN_ROWS body rows + horizontal bar; bytes shrink to 6 rows.
        let over = ui.ctx().data(|d| d.get_temp::<f32>(panels::records::overhead_id())).unwrap_or(0.0) + 2.0 * LAYOUT.center_margin[1];
        let table_need = layout::table_need_h(self.tok.header_height(ui), rh, ui.spacing().item_spacing.y, ui.spacing().scroll.allocated_width(), panels::records::TABLE_MIN_ROWS);
        let hex_floor = hex_min + (LAYOUT.hex_floor_rows - LAYOUT.bytes_min_rows).max(0.0) * measured.hex_row_h;
        // The records panel collapses its notes itself when this split leaves too little.
        let (rec_h, hex_h, _) = layout::records_plan(full.height() - handle, self.st.prefs().center_split(), measured.records_min_h, hex_min, hex_floor, over, table_need);
        let rec_rect = egui::Rect::from_min_size(full.min, egui::vec2(full.width(), rec_h));
        let handle_rect = egui::Rect::from_min_size(egui::pos2(full.left(), rec_rect.bottom()), egui::vec2(full.width(), handle));
        let hex_rect = egui::Rect::from_min_size(egui::pos2(full.left(), handle_rect.bottom()), egui::vec2(full.width(), hex_h));
        ui.scope_builder(egui::UiBuilder::new().max_rect(rec_rect.shrink2(margin)), |ui| {
            ui.set_clip_rect(rec_rect);
            acts = panels::records::show(ui, &self.st, &self.tok, scroll.row);
        });
        // Split handle: 1 px border line in a hit strip; hover/drag → 2 px focus line; double-click resets.
        let id = egui::Id::new(("center_split", self.st.prefs().layout_epoch()));
        let resp = ui.interact(handle_rect, id, egui::Sense::click_and_drag()).on_hover_cursor(egui::CursorIcon::ResizeVertical).on_hover_text("Drag to resize records / bytes · double-click resets");
        let active = resp.hovered() || resp.dragged();
        self.tok.paint_grain(ui.painter(), handle_rect);
        let y = handle_rect.center().y;
        ui.painter().line_segment([egui::pos2(handle_rect.left(), y), egui::pos2(handle_rect.right(), y)], egui::Stroke::new(if active { 2.0 } else { 1.0 }, if active { p.focus } else { p.border }));
        if resp.double_clicked() {
            acts.push(Action::ResetCenterSplit);
        } else if resp.dragged()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let h = (full.height() - handle).max(1.0);
            acts.push(Action::SetCenterSplit(((pos.y - handle / 2.0 - full.top()) / h).clamp(0.0, 1.0)));
        }
        ui.painter().rect_filled(hex_rect, 0.0, p.data);
        let mut hex_acts = Vec::new();
        ui.scope_builder(egui::UiBuilder::new().max_rect(hex_rect.shrink2(margin)), |ui| {
            ui.set_clip_rect(hex_rect);
            hex_acts = panels::hex::show(ui, &self.st, &self.tok, &mut self.mem, scroll.hex);
        });
        acts.extend(hex_acts);
        acts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::prefs::{KEY_THEME, KEY_ZOOM};

    /// Opens an app state with the given stored key/values, as `run` does at startup.
    fn open_with(stored: &[(String, String)]) -> AppState {
        let prefs = Prefs::load(|k| stored.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone()));
        let mut st = AppState::idle_for_tests();
        for a in startup_actions(&prefs, None) {
            st.apply(a);
        }
        st
    }

    struct Headless;
    impl Platform for Headless {
        fn pick_folder(&self, _: Option<&std::path::Path>) -> Option<PathBuf> {
            None
        }
        fn pick_file(&self, _: Option<&std::path::Path>) -> Option<PathBuf> {
            None
        }
    }

    /// Numbers of one rendered frame of the whole window (0019 §3).
    #[derive(Debug, Clone, Copy)]
    struct Shot {
        zoom: f32,
        ceiling: f32,
        table_rows: f32,
        bytes_per_row: u64,
        byte_rows: f32,
        toolbar_rows: f32,
        nav_w: f32,
        explain_w: f32,
    }

    /// The real window driven headlessly (window points at 100 %), XCopy, started as `run`
    /// does (`App::start`, stored zoom applied before the first frame).
    struct Rig {
        ctx: egui::Context,
        app: App,
        w: f32,
        h: f32,
    }

    impl Rig {
        fn new(w: f32, h: f32, stored_zoom: f32, file: Option<&str>) -> Rig {
            let ctx = egui::Context::default();
            let worker = Worker::spawn(Arc::new(|| {}));
            let mut app = App::new(AppState::new(worker), Box::new(Headless));
            let prefs = Prefs { theme: crate::app::prefs::DEFAULT_THEME.to_string(), zoom: stored_zoom };
            app.start(&ctx, &prefs, file.map(|f| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(f)));
            Rig { ctx, app, w, h }
        }
        /// One frame; as the integration does, the window in points at the current zoom.
        fn frame(&mut self) {
            let z = self.ctx.zoom_factor();
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(self.w / z, self.h / z))), ..Default::default() };
            let app = &mut self.app;
            let mut out = self.ctx.run_ui(input, |ui| app.frame_ui(ui));
            out.textures_delta.clear();
        }
        fn frames(&mut self, n: usize) {
            for _ in 0..n {
                self.frame();
            }
        }
        /// One frame, returning its text shapes: (clip rect, laid-out text rect, first row).
        fn texts(&mut self) -> Vec<(egui::Rect, egui::Rect, String)> {
            let z = self.ctx.zoom_factor();
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(self.w / z, self.h / z))), ..Default::default() };
            let app = &mut self.app;
            let out = self.ctx.run_ui(input, |ui| app.frame_ui(ui));
            out.shapes
                .into_iter()
                .filter_map(|c| match c.shape {
                    egui::Shape::Text(t) => Some((c.clip_rect, t.galley.rect.translate(t.pos.to_vec2()), t.galley.rows.first().map(|r| r.row.text()).unwrap_or_default())),
                    _ => None,
                })
                .collect()
        }
        /// Explanation text that is cut on the left (0020 D1, P3-F2): drawn left of the panel's
        /// inner edge, or under text of another panel whose clip reaches into the Explanation
        /// (the center is laid out after it, up to the rect the panel reported).
        fn explanation_cut_text(&mut self) -> Vec<String> {
            let width = self.w / self.ctx.zoom_factor();
            let panel_left = width - self.app.mem.right_w;
            let inner_left = panel_left + LAYOUT.panel_margin[0];
            let texts = self.texts();
            let inside: Vec<_> = texts.iter().filter(|(clip, _, _)| clip.left() >= panel_left - 0.5).collect();
            assert!(inside.len() > 5, "Explanation text drawn: {inside:?}");
            let left_of_edge = inside.iter().filter(|(_, r, _)| r.left() < inner_left - 0.5).map(|(_, r, t)| format!("{t:?} at x {:.1} < {inner_left:.1}", r.left()));
            // Between the toolbar and the status bar (the panel's own height).
            let (top, bottom) = inside.iter().fold((f32::MAX, f32::MIN), |(t, b), (clip, _, _)| (t.min(clip.top()), b.max(clip.bottom())));
            let overlap = texts
                .iter()
                .filter(|(clip, _, _)| clip.top() >= top - 0.5 && clip.bottom() <= bottom + 0.5)
                .filter(|(clip, _, _)| clip.left() < panel_left - 0.5 && clip.right() > panel_left + 0.5)
                .map(|(clip, _, t)| format!("{t:?} clipped to {clip:?} reaches into the Explanation at {panel_left:.1}"));
            left_of_edge.chain(overlap).collect()
        }
        fn until(&mut self, what: &str, done: impl Fn(&App) -> bool) {
            let t = std::time::Instant::now();
            while !done(&self.app) {
                assert!(t.elapsed() < std::time::Duration::from_secs(10), "timeout: {what}");
                self.frame();
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
        fn opened(&mut self) {
            self.until("open", |a| a.st.session().view().is_some() && a.st.navigation().pending_target().is_none() && !a.st.session().roots().is_empty() && a.st.session().hex().page().is_some());
        }
        fn show(&mut self, k: crate::model::NodeKind) {
            self.app.dispatch(&self.ctx, vec![Action::Navigate { target: k.clone(), span: None }]);
            self.until("view", move |a| a.st.navigation().pending_target().is_none() && a.st.session().view_kind() == Some(&k));
        }
        fn act(&mut self, acts: Vec<Action>) {
            self.app.dispatch(&self.ctx, acts);
        }
        fn shot(&self, ceiling: f32) -> Shot {
            let mem = &self.app.mem;
            Shot {
                zoom: self.app.st.prefs().zoom(),
                ceiling,
                table_rows: self.ctx.data(|d| d.get_temp::<f32>(panels::records::rows_visible_id())).unwrap_or(-1.0),
                bytes_per_row: mem.hex.bytes_per_row,
                byte_rows: mem.hex.rows_visible,
                toolbar_rows: ((mem.toolbar_h - 9.0) / mem.toolbar_row_h).round(),
                nav_w: mem.left_w,
                explain_w: mem.right_w,
            }
        }
        /// The search banner's rect and the records pane's clip rect of the last frame.
        fn banner(&self) -> Option<[egui::Rect; 2]> {
            self.ctx.data(|d| d.get_temp::<[egui::Rect; 2]>(panels::records::banner_rect_id()))
        }
        /// The banner is laid out entirely inside the visible records pane.
        fn assert_banner_visible(&self, what: &str) {
            let [b, clip] = self.banner().unwrap_or_else(|| panic!("{what}: no banner laid out"));
            assert!(b.height() > 0.0 && b.top() >= clip.top() - 0.5 && b.bottom() <= clip.bottom() + 0.5, "{what}: banner {b:?} outside the records pane {clip:?}");
        }
    }

    /// Renders the real window headlessly at `w`×`h` (window points at 100 %): XCopy, SST
    /// Footer of sst-zstd selected (title, location, note) and a no-match search banner (the
    /// worst-case records header). `zoom`: None = the measured ceiling.
    fn shoot(w: f32, h: f32, zooms: &[Option<f32>]) -> Vec<Shot> {
        let mut rig = Rig::new(w, h, 1.0, Some("examples/files/sst-zstd/sst-zstd.sst"));
        rig.opened();
        rig.act(vec![Action::SetSearch("no-such-text".into())]);
        rig.show(crate::model::NodeKind::SstFooter);
        rig.frames(4);
        let mut shots = Vec::new();
        for zoom in zooms {
            let ceiling = rig.app.st.prefs().zoom_ceiling();
            rig.act(vec![Action::SetZoom(zoom.unwrap_or(ceiling))]);
            rig.frames(8);
            let s = rig.shot(ceiling);
            rig.assert_banner_visible(&format!("{w}×{h} at {:.0}%", s.zoom * 100.0));
            eprintln!("{w}x{h} at {:.0}% (ceiling {:.0}%): {s:?}", s.zoom * 100.0, s.ceiling * 100.0);
            shots.push(s);
        }
        shots
    }

    /// 0020 §6: at 900×700 / 100 % the search banner keeps its reserved line below the title in
    /// the views with the longest WAL notes, for no match, match, invalid hex and the empty query
    /// (no banner). Banner, status segment and Next/Prev come from one state.
    #[test]
    fn search_banner_stays_visible_above_long_wal_notes() {
        use crate::model::NodeKind;
        for (file, node) in [
            ("examples/files/wal-fragmented/000004.log", NodeKind::Root),
            ("examples/files/corrupt-wal-truncated/corrupt-wal-truncated.log", NodeKind::Root),
            ("examples/files/wal-fragmented/000004.log", NodeKind::LogRecords { start: 0, page: 0 }),
        ] {
            let mut rig = Rig::new(900.0, 700.0, 1.0, Some(file));
            rig.opened();
            rig.show(node.clone());
            let notes = rig.app.st.session().view().map(|v| v.notes.len()).unwrap_or(0);
            let what = format!("{file} {node:?} ({notes} notes)");
            // No match.
            rig.act(vec![Action::SetSearch("no-such-text".into())]);
            rig.frames(6);
            let seg = |rig: &Rig| rig.app.st.search().status_segment(rig.app.st.row_match_position());
            let find = rig.app.st.search();
            assert_eq!(find.banner().as_deref(), Some("No row of this table matches \"no-such-text\". Next/Prev search this file's stored bytes (not decompressed)."), "{what}");
            assert_eq!(seg(&rig).as_deref(), Some("Rows 0 / 0 (current table)"), "{what}");
            assert!(find.can_step(), "{what}");
            rig.assert_banner_visible(&format!("{what}: no match"));
            // Match (a row of every WAL table has a "0x" offset cell or value).
            let cell = rig.app.st.session().view().and_then(|v| v.rows.first()).map(|r| r.cells[0].clone()).unwrap_or_default();
            rig.act(vec![Action::SetSearch(cell.clone())]);
            rig.frames(6);
            let find = rig.app.st.search();
            assert!(find.banner().is_some_and(|b| b.contains("of this table match") && b.ends_with("(decoded cells). Next/Prev to step.")), "{what}: {:?}", find.banner());
            assert!(seg(&rig).is_some_and(|s| s.starts_with("Rows – / ")) && find.can_step(), "{what}: {:?}", seg(&rig));
            rig.assert_banner_visible(&format!("{what}: match {cell:?}"));
            // Hex mode, invalid hex.
            rig.act(vec![Action::SetSearch("zz".into()), Action::SetSearchHex(true)]);
            rig.frames(6);
            let find = rig.app.st.search();
            assert_eq!(find.banner().as_deref(), Some("Not a hex byte sequence. Use byte pairs such as 4d 41 4e."), "{what}");
            assert_eq!(seg(&rig), None, "{what}");
            assert!(!find.can_step(), "{what}");
            rig.assert_banner_visible(&format!("{what}: invalid hex"));
            // Hex mode, valid query.
            rig.act(vec![Action::SetSearch("4d 41 4e".into())]);
            rig.frames(6);
            let find = rig.app.st.search();
            assert_eq!(find.banner().as_deref(), Some("Hex query 4d 41 4e: Next/Prev search this file's stored bytes (not decompressed)."), "{what}");
            assert!(find.can_step(), "{what}");
            rig.assert_banner_visible(&format!("{what}: hex query"));
            // Empty query: no banner, no status segment, Next/Prev disabled.
            rig.act(vec![Action::SetSearch(String::new())]);
            rig.frames(4);
            let find = rig.app.st.search();
            assert!(find.banner().is_none() && seg(&rig).is_none() && !find.can_step(), "{what}: empty query");
            assert!(rig.banner().is_none(), "{what}: no banner laid out for an empty query");
        }
    }

    /// 0020 §7: the "reduced to fit" notice is cleared by the first unclamped resize, rendered:
    /// 1400×900 at 140 % → 900×700 (clamped to 100 %, notice) → 1400×900 (cleared).
    #[test]
    fn zoom_notice_clears_after_resize_back() {
        let mut rig = Rig::new(1400.0, 900.0, 1.4, Some("examples/files/sst-zstd/sst-zstd.sst"));
        rig.opened();
        rig.frames(4);
        assert!((rig.app.st.prefs().zoom() - 1.4).abs() < 1e-3 && rig.app.st.prefs().notice().is_empty());
        (rig.w, rig.h) = (900.0, 700.0);
        rig.frames(8);
        assert_eq!((rig.app.st.prefs().zoom(), rig.app.st.prefs().notice()), (1.0, "Zoom reduced to 100% to fit window"));
        rig.frames(4);
        assert_eq!(rig.app.st.prefs().notice(), "Zoom reduced to 100% to fit window", "unchanged window: kept");
        (rig.w, rig.h) = (1400.0, 900.0);
        rig.frames(4);
        assert_eq!(rig.app.st.prefs().notice(), "", "resized back without a clamp: cleared");
        // UI ruling (zoom return): the chosen 140 % comes back by itself.
        assert!((rig.app.st.prefs().zoom() - 1.4).abs() < 1e-3 && (rig.ctx.zoom_factor() - 1.4).abs() < 1e-3, "zoom returned: {}", rig.app.st.prefs().zoom());
    }

    /// In-memory eframe storage (F3 test).
    #[derive(Default)]
    struct MemStorage(std::collections::HashMap<String, String>);
    impl eframe::Storage for MemStorage {
        fn get_string(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
        fn set_string(&mut self, key: &str, value: String) {
            self.0.insert(key.to_string(), value);
        }
        fn remove_string(&mut self, key: &str) {
            self.0.remove(key);
        }
        fn flush(&mut self) {}
    }

    /// 0020 F3: the stored window size is the OS logical size, not the egui screen rect at the
    /// zoom (eframe stores 1400×900 at 160 % as 875×562.5 and restores it at zoom 1.0). The
    /// rewrite keeps every other field and is idempotent.
    #[test]
    fn saved_window_size_is_zoom_independent() {
        use eframe::Storage as _;
        let eframe_at_160 = "(inner_position_pixels:Some((x:328.0,y:208.0)),outer_position_pixels:Some((x:328.0,y:152.0)),fullscreen:false,maximized:false,inner_size_points:Some((x:875.0,y:562.5)))";
        let want = "(inner_position_pixels:Some((x:328.0,y:208.0)),outer_position_pixels:Some((x:328.0,y:152.0)),fullscreen:false,maximized:false,inner_size_points:Some((x:1400.0,y:900.0)))";
        assert_eq!(window_settings_with_inner_size(eframe_at_160, [1400.0, 900.0]).as_deref(), Some(want));
        assert_eq!(window_settings_with_inner_size(want, [1400.0, 900.0]).as_deref(), Some(want), "idempotent");
        assert_eq!(window_settings_with_inner_size("(inner_size_points:None,fullscreen:true)", [800.0, 600.5]).as_deref(), Some("(inner_size_points:Some((x:800.0,y:600.5)),fullscreen:true)"));
        assert_eq!(window_settings_with_inner_size("(fullscreen:false)", [1.0, 1.0]), None, "unknown shape: left alone");
        // The real save path after a 1400×900 session at 160 %: eframe wrote the zoomed size
        // first, then `App::save` (same storage) — theme, chosen zoom and the real size.
        let mut rig = Rig::new(1400.0, 900.0, 1.6, None);
        rig.frames(4);
        assert!((rig.ctx.zoom_factor() - 1.6).abs() < 1e-3);
        assert!(rig.app.window_unsaved(), "a size not saved yet: auto-save soon");
        assert_eq!(eframe::App::auto_save_interval(&rig.app), std::time::Duration::from_secs(RESIZE_SAVE_SECS));
        let mut s = MemStorage::default();
        s.set_string(WINDOW_KEY, eframe_at_160.to_string());
        eframe::App::save(&mut rig.app, &mut s);
        assert_eq!(s.get_string(WINDOW_KEY).as_deref(), Some(want));
        assert_eq!(s.get_string(crate::app::prefs::KEY_ZOOM).as_deref(), Some("1.6"));
        assert!(!rig.app.window_unsaved());
        assert_eq!(eframe::App::auto_save_interval(&rig.app), std::time::Duration::from_secs(30));
        // Clamped by a small window: the chosen zoom is what is saved (and restored).
        (rig.w, rig.h) = (900.0, 700.0);
        rig.frames(8);
        assert!((rig.app.st.prefs().zoom() - 1.0).abs() < 1e-3);
        eframe::App::save(&mut rig.app, &mut s);
        assert_eq!(s.get_string(crate::app::prefs::KEY_ZOOM).as_deref(), Some("1.6"));
        assert!(s.get_string(WINDOW_KEY).unwrap().ends_with("inner_size_points:Some((x:900.0,y:700.0)))"), "{:?}", s.get_string(WINDOW_KEY));
    }

    /// 0020 F1 (rendered): at 900×700 / 100 % with the long byte-search status and a selection,
    /// the zoom notice keeps its full width; the search segment is elided first.
    #[test]
    fn zoom_notice_survives_a_long_byte_search_status_at_900() {
        let mut rig = Rig::new(1400.0, 900.0, 1.4, Some("tests/fixtures/wal-fragmented/000004.log"));
        rig.opened();
        rig.act(vec![Action::SetSearch("Data block @407820".into()), Action::SearchNext]);
        rig.until("byte search done", |a| a.st.search().status() == crate::app::search::BYTES_NO_MATCH);
        rig.act(vec![Action::SelectSpan(crate::model::Span::file(0x10, 4))]);
        (rig.w, rig.h) = (900.0, 700.0);
        rig.frames(8);
        assert_eq!(rig.app.st.prefs().notice(), "Zoom reduced to 100% to fit window");
        assert_eq!(rig.app.st.search().status_segment(None).as_deref(), Some(crate::app::search::BYTES_NO_MATCH));
        assert!(rig.app.st.selection().span().is_some(), "a selection is shown");
        let [notice, notice_want, search, search_want] = rig.ctx.data(|d| d.get_temp::<[f32; 4]>(panels::status::notice_width_id())).expect("status bar drawn");
        assert!(notice_want > 0.0 && notice >= notice_want - 0.01, "notice elided: planned {notice} of {notice_want}");
        assert!(search < search_want, "the search segment is elided first: {search} of {search_want}");
    }

    /// 0020 UI ruling 6: in the bundled fonts the no-break join keeps "8 B" on one line where a
    /// plain space wraps between the count and the unit; U+00A0 renders (no replacement box).
    #[test]
    fn range_text_never_wraps_between_count_and_unit() {
        let ctx = egui::Context::default();
        test_frame(&ctx, egui::RawInput::default(), |_| {});
        for font in [egui::FontId::proportional(14.0), egui::FontId::monospace(13.5)] {
            let w = |t: &str| ctx.fonts_mut(|f| f.layout_no_wrap(t.to_string(), font.clone(), egui::Color32::WHITE).size().x);
            let text = "file [0xc12,0xc1a) · 8 B";
            // A width that holds "… · 8" but not "… · 8 B".
            let wrap_w = (w("file [0xc12,0xc1a) · 8 ") + w(text)) / 2.0;
            let rows = |t: String| ctx.fonts_mut(|f| f.layout(t, font.clone(), egui::Color32::WHITE, wrap_w).rows.iter().map(|r| r.row.text()).collect::<Vec<_>>());
            let plain = rows(text.to_string());
            assert!(plain.len() == 2 && plain[1].trim() == "B", "{font:?}: a plain space wraps before B: {plain:?}");
            let joined = rows(layout::nb_units(text));
            assert!(joined.iter().any(|l| l.contains("8\u{a0}B")), "{font:?}: count and unit together: {joined:?}");
            let uv = |c: char| ctx.fonts_mut(|f| f.layout_no_wrap(c.to_string(), font.clone(), egui::Color32::WHITE).rows[0].row.glyphs[0].uv_rect);
            assert_ne!(uv(layout::NBSP), uv('◻'), "{font:?}: U+00A0 is not the replacement box");
        }
    }

    /// Review N3: a session restored above 100 % is squeezed from the saved widths from its
    /// very first frame (not one frame at 100 % / default widths first); the same widths hold
    /// in steady state; zoom out and Reset View give the saved (default) widths back.
    #[test]
    fn first_frame_after_restore_squeezes_from_saved_widths() {
        let mut rig = Rig::new(1400.0, 900.0, 1.6, None);
        // The restored zoom is in egui's options before the first frame, so the first frame is
        // laid out at 160 % (it was one frame at 100 % first: the zoom was only queued in frame 1).
        assert!((rig.ctx.zoom_factor() - 1.6).abs() < 1e-3, "zoom before the first frame: {}", rig.ctx.zoom_factor());
        rig.frame();
        let first = rig.shot(0.0);
        assert!(first.explain_w < LAYOUT.right_default - 0.5 && first.explain_w >= LAYOUT.right_squeeze_min - 0.5, "first frame squeezed from the saved widths: {first:?}");
        rig.frames(8);
        let steady = rig.shot(0.0);
        assert!((first.nav_w - steady.nav_w).abs() < 0.5 && (first.explain_w - steady.explain_w).abs() < 0.5, "first {first:?} vs steady {steady:?}");
        // With a file whose byte rows need the room, the squeeze starts from the saved widths.
        rig.act(vec![Action::OpenPath(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/files/sst-zstd/sst-zstd.sst"))]);
        rig.opened();
        rig.frames(8);
        let steady = rig.shot(0.0);
        assert!(steady.explain_w < LAYOUT.right_default - 0.5 && steady.explain_w >= LAYOUT.right_squeeze_min - 0.5 && steady.bytes_per_row >= 8, "squeezed: {steady:?}");
        assert_eq!((rig.app.mem.left_user, rig.app.mem.right_user), (LAYOUT.left_default, LAYOUT.right_default), "saved widths untouched by the squeeze");
        // 0020 D1 (P3-F2): in the squeezed Explanation every line starts at the panel's inner
        // left edge: nothing selected, an SST entry (field card) and the whole data block.
        assert_eq!(rig.explanation_cut_text(), Vec::<String>::new(), "nothing selected");
        let block = crate::model::NodeKind::SstBlock { off: 0, size: 0x63, kind: crate::model::BlockKind::Data };
        rig.show(block);
        for row in [0, 1] {
            rig.act(vec![Action::SelectRow(row)]);
            rig.frames(4);
            assert!(matches!(crate::app::explanation::subject(rig.app.st.session(), rig.app.st.selection()), crate::app::explanation::Subject::Field { .. }), "entry {row} selected");
            assert_eq!(rig.explanation_cut_text(), Vec::<String>::new(), "entry {row} at 160 %: {:?}", rig.shot(0.0));
        }
        rig.act(vec![Action::SelectSpan(crate::model::Span::file(0x10, 4))]);
        rig.frames(4);
        assert_eq!(rig.explanation_cut_text(), Vec::<String>::new(), "raw range (interpretations open)");
        rig.act(vec![Action::ZoomReset]);
        rig.frames(4);
        assert_eq!((rig.app.mem.left_w, rig.app.mem.right_w), (LAYOUT.left_default, LAYOUT.right_default), "zoom out: saved widths");
        rig.act(vec![Action::SetZoom(1.6)]);
        rig.frames(4);
        let again = rig.shot(0.0);
        assert!((again.explain_w - steady.explain_w).abs() < 0.5 && (again.nav_w - steady.nav_w).abs() < 0.5, "zoom in again: same squeeze {again:?}");
        rig.act(vec![Action::ResetView]);
        rig.frames(4);
        assert_eq!(
            (rig.app.mem.left_w, rig.app.mem.right_w, rig.app.mem.left_user, rig.app.mem.right_user),
            (LAYOUT.left_default, LAYOUT.right_default, LAYOUT.left_default, LAYOUT.right_default),
            "Reset View"
        );
    }

    /// 0019 §3: rendered frames (whole window, XCopy, SST Footer + no-match banner) at the
    /// measured ceilings. 1400×900: 100 % unchanged; at the ceiling ≥ 3 full table rows, ≥ 8
    /// bytes per row, ≥ 6 byte rows, toolbar ≤ 2 rows; zoom out restores the user's panel
    /// widths. 900×700 and 900×560: the ceiling is the 100 % floor (Zoom In disabled); there
    /// ≥ 3 table rows and the byte rows of today (4 per row, panels at the user's widths).
    #[test]
    fn rendered_layout_at_the_zoom_ceilings() {
        let s = shoot(1400.0, 900.0, &[Some(1.0), None, Some(1.0)]);
        let (base, top, back) = (s[0], s[1], s[2]);
        assert_eq!((base.table_rows, base.bytes_per_row, base.toolbar_rows, base.nav_w, base.explain_w), (6.0, 16, 1.0, LAYOUT.left_default, LAYOUT.right_default), "100 %: {base:?}");
        assert!((top.ceiling - 1.6).abs() < 1e-3, "measured 1400×900 ceiling: {top:?}");
        assert!((top.zoom - top.ceiling).abs() < 1e-3, "{top:?}");
        assert!(top.table_rows >= 3.0 && top.bytes_per_row >= 8 && top.byte_rows >= 6.0 && top.toolbar_rows <= 2.0, "1400×900 at the ceiling: {top:?}");
        assert!(top.explain_w < LAYOUT.right_default && top.explain_w >= LAYOUT.right_squeeze_min - 0.5, "Explanation shrinks first, to its squeeze floor: {top:?}");
        assert_eq!((back.nav_w, back.explain_w, back.bytes_per_row), (LAYOUT.left_default, LAYOUT.right_default, 16), "zoom out restores the user's widths");
        for (w, h) in [(900.0, 700.0), (900.0, 560.0)] {
            let s = shoot(w, h, &[None])[0];
            assert!((s.ceiling - 1.0).abs() < 1e-3, "{w}×{h}: {s:?}");
            assert!(s.table_rows >= 3.0 && s.bytes_per_row >= 4 && s.byte_rows >= 3.0 && s.toolbar_rows <= 2.0, "{w}×{h}: {s:?}");
        }
    }

    /// 0017 A: missing → XCopy, unknown → XCopy, saved Matrix survives store/load and reopen.
    #[test]
    fn startup_theme_default_fallback_and_saved_choice() {
        assert_eq!(AppState::idle_for_tests().prefs().theme_id(), "xcopy");
        assert_eq!(open_with(&[]).prefs().theme_id(), "xcopy");
        let unknown = [(KEY_THEME.to_string(), "no-such-theme".to_string())];
        assert_eq!(open_with(&unknown).prefs().theme_id(), "xcopy");
        let saved = [(KEY_THEME.to_string(), "matrix".to_string()), (KEY_ZOOM.to_string(), "1".to_string())];
        let st = open_with(&saved);
        assert_eq!(st.prefs().theme_id(), "matrix");
        let mut kv = Vec::new();
        st.prefs().persisted().store(|k, v| kv.push((k.to_string(), v)));
        assert!(kv.contains(&(KEY_THEME.to_string(), "matrix".to_string())));
        assert_eq!(open_with(&kv).prefs().theme_id(), "matrix");
        assert_eq!(theme::theme_by_id(open_with(&kv).prefs().theme_id()).name, "Matrix");
    }
}
