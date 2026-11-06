//! eframe shell: owns the `AppState`, the resolved theme `Tokens`, textures and the platform
//! adapter. Each frame it applies job responses, syncs the native menu, reads keys, reports the
//! layout measurements for the zoom ceiling, renders the panels and applies the `Action`s they
//! return. Layout: toolbar → one-line status bar → navigation (left) → explanation (right) →
//! center column split into records (top) and bytes (bottom) by the split of the view's
//! `SplitRole` (file views, levels table, LSM Map; `ViewPreferences::split`).
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
use panels::{PanelMemory, TipExt};
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
    let mut acts = vec![Action::SetTheme(theme::theme_by_id(&prefs.theme).id), Action::SetZoom(prefs.zoom), Action::SetTooltips(prefs.tooltips)];
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
            // 0030 A2-4: the LSM Map entry (toolbar, View menu, Shift+⌘M) on a shown map that
            // lost keyboard focus (Tab, a click elsewhere) gives it focus back; with focus it
            // switches to the levels table as before.
            let a = if a == Action::ShowLsmMap && self.st.session().map_shown() && !panels::lsm_map::has_focus(ctx, &self.st) { Action::FocusLsmMap } else { a };
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

    /// Only the theme id, zoom and tooltips are persisted; panel layout persistence is deferred.
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
        // The one tooltip gate reads this (View › Show Tooltips) and the theme's tooltip look.
        panels::set_tooltips(&ctx, self.st.prefs().tooltips(), &self.tok);
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
                acts = panels::records::show(ui, &self.st, &self.tok, scroll);
            });
            let strip = egui::Rect::from_min_max(egui::pos2(full.left(), full.bottom() - strip_h), full.max);
            let resp = ui.interact(strip, ui.id().with("bytes_strip"), egui::Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).tip(show_hint(Panel::Bytes).1);
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
        // Each split role keeps its own user split (0026 D2 F2, 0030 A8); without one, the LSM
        // Map favours the map (the bytes keep about four byte rows: their minimum + one row) and
        // the levels table gets what all its rows need with the same byte rows, so switching
        // Table ↔ Map keeps the pane sizes; the file views use the standard plan. Without a user
        // split the byte pane holds at most the open file's bytes (0030 A8-2). The rule is the
        // pure `layout::center_heights` (architect N5).
        let role = self.st.session().split_role();
        let rows = self.st.session().view().map(|v| v.rows.len()).unwrap_or(0) as f32;
        let input = layout::CenterInputs {
            h: (full.height() - handle).max(0.0),
            records_min_h: measured.records_min_h,
            hex_min,
            hex_row_h: measured.hex_row_h,
            hex_floor,
            over,
            table_need,
            all_rows_need: layout::table_need_h(self.tok.header_height(ui), rh, ui.spacing().item_spacing.y, ui.spacing().scroll.allocated_width(), rows.max(panels::records::TABLE_MIN_ROWS)),
            file_rows: layout::file_rows(self.st.session().file_len(), self.mem.hex.bytes_per_row),
        };
        let (rec_h, hex_h) = layout::center_heights(role, self.st.prefs().split(role), &input);
        let rec_rect = egui::Rect::from_min_size(full.min, egui::vec2(full.width(), rec_h));
        let handle_rect = egui::Rect::from_min_size(egui::pos2(full.left(), rec_rect.bottom()), egui::vec2(full.width(), handle));
        let hex_rect = egui::Rect::from_min_size(egui::pos2(full.left(), handle_rect.bottom()), egui::vec2(full.width(), hex_h));
        ui.scope_builder(egui::UiBuilder::new().max_rect(rec_rect.shrink2(margin)), |ui| {
            ui.set_clip_rect(rec_rect);
            acts = panels::records::show(ui, &self.st, &self.tok, scroll);
        });
        // Split handle: 1 px border line in a hit strip; hover/drag → 2 px focus line; double-click resets.
        let id = egui::Id::new(("center_split", self.st.prefs().layout_epoch()));
        let resp = ui
            .interact(handle_rect, id, egui::Sense::click_and_drag())
            .on_hover_cursor(egui::CursorIcon::ResizeVertical)
            .tip("Drag to resize records / bytes (kept separately for Table and Map) · double-click resets");
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
    use crate::app::view_prefs::SplitRole;

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
            let prefs = Prefs { zoom: stored_zoom, ..Prefs::default() };
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

    /// 0024 §2–3 (rendered, headless): the LSM Map of db-levels fills the records area at
    /// 1400×900 and 900×560, draws file boxes (texts "000014" …), and its zoom changes only the
    /// map zoom, never the app zoom.
    #[test]
    fn lsm_map_renders_and_zooms_map_only() {
        let mut navigated = 0;
        for (w, h, z) in [(1400.0, 900.0, 1.0), (900.0, 560.0, 1.0), (900.0, 560.0, 1.6)] {
            let mut rig = Rig::new(w, h, z, Some("tests/fixtures/db-levels/CURRENT"));
            rig.opened();
            rig.act(vec![Action::ShowLsmMap]);
            rig.until("map", |a| a.st.session().map_shown() && a.st.navigation().pending_target().is_none());
            rig.frames(4);
            let texts = rig.texts();
            assert!(texts.iter().any(|t| t.2.contains("000014")), "{w}x{h}: box label drawn");
            assert!(texts.iter().any(|t| t.2.starts_with("L0")), "{w}x{h}: level header drawn");
            // 0026: the controls exist exactly in navigation mode (a fitting map is stationary).
            let nav = rig.app.st.session().map().navigable();
            assert_eq!(texts.iter().any(|t| t.2 == "Fit"), nav, "{w}x{h}@{z}: Fit shown = navigation mode");
            // D2b finding 3: the full §3.7 legend when there is room (1400×900), short or
            // Explanation-only at 900×560.
            let full = "x: user-key order (bytewise comparator), boxes placed by key rank, not to scale";
            let shown_full = texts.iter().any(|t| t.2 == full);
            if z == 1.0 {
                assert_eq!(shown_full, w > 1000.0, "{w}x{h}: full legend shown = {shown_full}");
            }
            if !nav {
                continue;
            }
            navigated += 1;
            let zoom = rig.app.st.prefs().zoom();
            let plus = |rig: &Rig| rig.ctx.data(|d| d.get_temp::<egui::Rect>(panels::lsm_map::plus_rect_id())).expect("strip drawn");
            let before = plus(&rig);
            rig.act(vec![Action::MapZoom(crate::app::MapZoom::In)]);
            rig.frames(2);
            // D3: the `+` button does not move when the ×N label grows (×1 → ×1.5).
            assert_eq!(plus(&rig), before, "{w}x{h}: + moved after a zoom step");
            rig.act(vec![Action::MapView { pan: (0.0, 0.0), zoom: 1234.5 }]);
            rig.frames(2);
            assert_eq!(plus(&rig), before, "{w}x{h}: + moved at a 4-digit zoom");
            rig.act(vec![Action::MapZoom(crate::app::MapZoom::Fit), Action::MapZoom(crate::app::MapZoom::In)]);
            rig.frames(2);
            assert!(rig.app.st.session().map().zoom() > 1.0 && (rig.app.st.prefs().zoom() - zoom).abs() < 1e-6, "map zoom only");
        }
        assert!(navigated > 0, "at least one setting overflows, so the zoom controls were exercised");
    }

    /// 0026 D2 F2: with the byte pane shown, the LSM Map takes all but about four byte rows of
    /// the centre column, so multilevel-db cf_users (2 L0 files) fits at 1400×900 (no
    /// navigation controls); Table restores the previous split; a drag in Map mode is the map's
    /// own split. D2 F1: the Explanation facts of a selected box are consecutive rows with the
    /// value on its label's line (Lane and Overlap visible without scrolling past blank space).
    /// D4 (tester phase 3 N2): at 900×700 with the byte pane shown, db-levels has a CF in
    /// navigation mode; its strip shows Center / Fit / − / +, and Fit keeps them (and the mode)
    /// over the next frames: no oscillation.
    #[test]
    fn navigation_controls_stay_after_fit_at_900x700() {
        let mut rig = Rig::new(900.0, 700.0, 1.0, Some("tests/fixtures/db-levels/CURRENT"));
        rig.opened();
        assert!(rig.app.st.prefs().show_hex(), "byte pane shown");
        rig.act(vec![Action::ShowLsmMap]);
        rig.until("map", |a| a.st.session().map_shown() && a.st.navigation().pending_target().is_none());
        let ids: Vec<u32> = rig.app.st.session().view().map(|v| v.lsm.cfs.iter().map(|c| c.id).collect()).unwrap_or_default();
        let controls = |t: &[(egui::Rect, egui::Rect, String)]| ["Center", "Fit", "−", "+"].iter().all(|s| t.iter().any(|x| x.2 == *s));
        let mut found = false;
        for id in ids {
            rig.act(vec![Action::MapSelectCf(id)]);
            rig.frames(4);
            if !rig.app.st.session().map().navigable() {
                assert!(!controls(&rig.texts()), "CF {id}: a fitting map shows no controls");
                continue;
            }
            found = true;
            assert!(controls(&rig.texts()), "CF {id}: Center / Fit / − / + in navigation mode");
            rig.act(vec![Action::MapZoom(crate::app::MapZoom::In), Action::MapZoom(crate::app::MapZoom::Fit)]);
            for frame in 0..2 {
                let t = rig.texts();
                assert!(controls(&t) && rig.app.st.session().map().navigable(), "CF {id} frame {frame} after Fit: controls stay");
                assert_eq!(rig.app.st.session().map().zoom(), 1.0);
            }
        }
        assert!(found, "db-levels has a CF in navigation mode at 900×700 with the byte pane shown");
    }

    /// D3 S3 (rendered): at 900×560 with 40 WALs the band folds the older ones into the
    /// "+N older" box / chip; keyboard Left past the oldest visible WAL selects a folded WAL,
    /// and the folded box then shows the selection (its text in `sel_fg`).
    #[test]
    fn folded_wal_selection_shows_on_the_older_box() {
        let d = std::env::temp_dir().join(format!("rocksvision-ui-{}-manywal", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/db-levels");
        for e in std::fs::read_dir(&src).unwrap().flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n == "CURRENT" || n.starts_with("MANIFEST-") || n.ends_with(".sst") {
                std::fs::copy(e.path(), d.join(&n)).unwrap();
            }
        }
        for n in 100..140u64 {
            std::fs::write(d.join(format!("{n:06}.log")), b"").unwrap();
        }
        let mut rig = Rig::new(900.0, 560.0, 1.0, Some(d.join("CURRENT").to_str().unwrap()));
        rig.opened();
        rig.act(vec![Action::ShowLsmMap]);
        rig.until("map + summaries", |a| a.st.session().map_shown() && a.st.navigation().pending_target().is_none() && !a.st.jobs().wal_scanning());
        rig.frames(3);
        rig.act(vec![Action::SelectWal(139)]);
        for _ in 0..39 {
            rig.act(vec![Action::MapMove(crate::app::MapMove::Left)]);
        }
        assert_eq!(rig.app.st.session().map().wal(), Some(100), "Left reaches the oldest WAL");
        let sel_fg = rig.app.tok.p.sel_fg;
        let z = rig.ctx.zoom_factor();
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(rig.w / z, rig.h / z))), ..Default::default() };
        let app = &mut rig.app;
        let mut out = rig.ctx.run_ui(input, |ui| app.frame_ui(ui));
        out.textures_delta.clear();
        let older: Vec<(String, egui::Color32)> = out
            .shapes
            .into_iter()
            .filter_map(|c| match c.shape {
                egui::Shape::Text(t) if t.galley.text().starts_with('+') => Some((t.galley.text().to_string(), t.galley.job.sections.first().map(|s| s.format.color).unwrap_or_default())),
                _ => None,
            })
            .collect();
        assert!(!older.is_empty(), "the band folded the older WALs");
        assert!(older.iter().any(|(_, c)| *c == sel_fg), "the older box shows the selection of a folded WAL: {older:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn map_mode_split_fits_multilevel_and_facts_rows_are_consecutive() {
        let mut rig = Rig::new(1400.0, 900.0, 1.0, Some("examples/data/multilevel-db/CURRENT"));
        rig.opened();
        assert!(rig.app.st.prefs().show_hex(), "byte pane shown");
        rig.act(vec![Action::ShowLsmMap]);
        rig.until("map", |a| a.st.session().map_shown() && a.st.navigation().pending_target().is_none());
        let id = rig.app.st.session().view().and_then(|v| v.lsm.cfs.iter().find(|c| c.name.eq("cf_users")).map(|c| c.id)).expect("cf_users");
        rig.act(vec![Action::MapSelectCf(id)]);
        rig.frames(6);
        assert!(!rig.app.st.session().map().navigable(), "cf_users fits with the byte pane shown: {:?}", rig.app.st.session().map().measure());
        let texts = rig.texts();
        assert!(!texts.iter().any(|t| t.2 == "Fit"), "no navigation controls");
        // F1: select 000018 (lane 2 of L0) and check the Explanation rows.
        let row = rig.app.st.session().view().and_then(|v| v.lsm.by_number(18)).map(|(_, _, f)| f.row).expect("000018");
        rig.act(vec![Action::SelectRow(row)]);
        rig.frames(2);
        let texts = rig.texts();
        let y = |s: &str| texts.iter().find(|t| t.2.starts_with(s)).map(|t| t.1.top()).unwrap_or_else(|| panic!("{s} drawn"));
        let (file_l, cf_l, lane_l, lane_v) = (y("File "), y("Column family "), y("Lane "), y("2 of 2 in L0"));
        // The value nearest the label (the card heading also says 000018.sst).
        let file_v = texts.iter().filter(|t| t.2 == "000018.sst").map(|t| t.1.top()).min_by(|a, b| (a - file_l).abs().total_cmp(&(b - file_l).abs())).expect("value drawn");
        assert!((file_v - file_l).abs() < 4.0, "value on the label's line: {file_l} / {file_v}");
        assert!((lane_v - lane_l).abs() < 4.0, "Lane value on its line: {lane_l} / {lane_v}");
        assert!(cf_l - file_l > 0.0 && cf_l - file_l < 60.0, "consecutive rows: {file_l} → {cf_l}");
        assert!(lane_l < 900.0, "Lane visible without scrolling: {lane_l}");
        // Table keeps its own split; a drag in Map mode is the map's.
        let table_split = rig.app.st.prefs().split(SplitRole::Records);
        rig.act(vec![Action::SetCenterSplit(0.5)]);
        assert_eq!((rig.app.st.prefs().split(SplitRole::Map), rig.app.st.prefs().split(SplitRole::Records)), (Some(0.5), table_split));
        rig.act(vec![Action::SetMapMode(crate::app::MapMode::Table)]);
        rig.act(vec![Action::SetCenterSplit(0.3)]);
        // 0030 A8: the levels table has its own split; the file views' is untouched.
        assert_eq!((rig.app.st.prefs().split(SplitRole::Map), rig.app.st.prefs().split(SplitRole::LevelsTable), rig.app.st.prefs().split(SplitRole::Records)), (Some(0.5), Some(0.3), table_split));
    }

    /// 0026 §1–§6 (rendered, headless): level frames with titles in the frame (no header
    /// column), the axis row, the WAL band with its title and the separator caption; the strip
    /// shows Center/Fit/−/+ exactly while the map is in navigation mode; the measure reached the
    /// state; app zoom untouched.
    #[test]
    fn lsm_map_frames_axis_band_and_conditional_controls() {
        for (w, h) in [(1400.0, 900.0), (900.0, 560.0)] {
            let mut rig = Rig::new(w, h, 1.0, Some("tests/fixtures/db-levels/CURRENT"));
            rig.opened();
            rig.act(vec![Action::ShowLsmMap]);
            rig.until("map", |a| a.st.session().map_shown() && a.st.navigation().pending_target().is_none());
            rig.frames(6);
            let texts = rig.texts();
            let has = |s: &str| texts.iter().any(|t| t.2 == s);
            let starts = |s: &str| texts.iter().any(|t| t.2.starts_with(s));
            assert!(starts("L0 · "), "{w}x{h}: level title in the frame");
            assert!(has("key rank, not to scale ->"), "{w}x{h}: axis row");
            if has("WAL:") {
                // Map height under 220 pt: the band is one line of chips (§4.8).
                assert!(h < 700.0 && starts("000025.log"), "{w}x{h}: chip of the WAL");
            } else {
                assert!(has("SST levels (live per MANIFEST)"), "{w}x{h}: band separator caption");
                assert!(starts("WAL · 1 file"), "{w}x{h}: WAL band title");
            }
            assert!(rig.app.st.session().map().measure().is_some(), "{w}x{h}: measured");
            let nav = rig.app.st.session().map().navigable();
            assert_eq!((has("Fit"), has("Center")), (nav, nav), "{w}x{h}: controls only in navigation mode");
            assert!(!texts.iter().any(|t| t.2.contains("u-009") || t.2.to_lowercase().contains("missing keys")));
        }
    }

    /// One frame with these key presses (no modifiers).
    fn press(rig: &mut Rig, keys: &[egui::Key]) {
        let z = rig.ctx.zoom_factor();
        let events = keys.iter().map(|k| egui::Event::Key { key: *k, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }).collect();
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(rig.w / z, rig.h / z))), events, ..Default::default() };
        let app = &mut rig.app;
        let mut out = rig.ctx.run_ui(input, |ui| app.frame_ui(ui));
        out.textures_delta.clear();
    }

    /// One frame with these pointer events.
    fn pointer(rig: &mut Rig, events: Vec<egui::Event>) {
        let z = rig.ctx.zoom_factor();
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(rig.w / z, rig.h / z))), events, ..Default::default() };
        let app = &mut rig.app;
        let mut out = rig.ctx.run_ui(input, |ui| app.frame_ui(ui));
        out.textures_delta.clear();
    }

    /// 0026 D7 (tester P6, rendered): multilevel-db CF 0 at 1400×900: a real click on the
    /// 000014 box selects it and gives the map keyboard focus; Up enters the WAL band at the
    /// newest WAL (000023); Enter opens it; Back returns to the map with the WAL pick.
    #[test]
    fn map_click_up_enter_back_reaches_the_wal_band() {
        let mut rig = Rig::new(1400.0, 900.0, 1.0, Some("examples/data/multilevel-db/CURRENT"));
        rig.opened();
        rig.act(vec![Action::ShowLsmMap]);
        rig.until("map", |a| a.st.session().map_shown() && a.st.navigation().pending_target().is_none() && !a.st.jobs().wal_scanning());
        rig.frames(4);
        let t = rig.texts();
        // The map box (right of the Navigation panel, which lists 000014.sst too).
        let nav_right = rig.app.mem.left_w + 8.0;
        let at = t.iter().find(|x| x.2.starts_with("000014") && x.1.left() > nav_right).map(|x| x.1.center()).expect("000014 box drawn");
        let btn = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        pointer(&mut rig, vec![egui::Event::PointerMoved(at)]);
        pointer(&mut rig, vec![btn(true)]);
        pointer(&mut rig, vec![btn(false)]);
        rig.frames(2);
        let row = rig.app.st.session().view().and_then(|v| v.lsm.by_number(14)).map(|(_, _, f)| f.row);
        assert_eq!(rig.app.st.selection().row(), row, "the click selected 000014");
        assert!(rig.ctx.memory(|m| m.has_focus(panels::lsm_map::map_id())), "the click gave the map keyboard focus");
        press(&mut rig, &[egui::Key::ArrowUp]);
        rig.frames(1);
        assert_eq!(rig.app.st.session().map().wal(), Some(23), "Up from the top level: the newest WAL");
        press(&mut rig, &[egui::Key::Enter]);
        rig.until("wal opened", |a| a.st.session().file().is_some_and(|f| f.ends_with("000023.log")) && a.st.navigation().pending_target().is_none());
        rig.act(vec![Action::Back]);
        rig.until("back", |a| a.st.session().map_shown() && a.st.navigation().pending_target().is_none());
        assert_eq!(rig.app.st.session().map().wal(), Some(23), "Back restores the WAL pick");
    }

    /// 0024 D1 (advice §4.4/§6.4): the LSM Map entry gives the map keyboard focus, so Right
    /// (through `keys`, `Keys::map_focused`) moves the map focus at once; Tab leaves the map and
    /// Tab reaches it again (it is in the Tab order); without focus Right is the tree's again.
    #[test]
    fn lsm_map_takes_keyboard_focus_and_is_in_the_tab_order() {
        let mut rig = Rig::new(1400.0, 900.0, 1.0, Some("tests/fixtures/db-levels/CURRENT"));
        rig.opened();
        rig.act(vec![Action::ShowLsmMap]);
        rig.until("map", |a| a.st.session().map_shown() && a.st.navigation().pending_target().is_none());
        rig.frames(3);
        let id = panels::lsm_map::map_id();
        assert!(rig.ctx.memory(|m| m.has_focus(id)), "the entry focused the map");
        assert_eq!(rig.app.st.session().map().focus(), None);
        press(&mut rig, &[egui::Key::ArrowRight]);
        rig.frames(1);
        let first = rig.app.st.session().map().focus();
        assert!(first.is_some(), "Right moved the map focus");
        press(&mut rig, &[egui::Key::ArrowDown]);
        rig.frames(1);
        assert_ne!(rig.app.st.session().map().focus(), first, "the box in the level below");
        assert!(rig.ctx.memory(|m| m.has_focus(id)), "arrows did not move egui's focus away");
        // Tab leaves the map; repeated Tab comes back to it.
        press(&mut rig, &[egui::Key::Tab]);
        rig.frames(1);
        assert!(!rig.ctx.memory(|m| m.has_focus(id)), "Tab left the map");
        let mut back = false;
        for _ in 0..80 {
            press(&mut rig, &[egui::Key::Tab]);
            if rig.ctx.memory(|m| m.has_focus(id)) {
                back = true;
                break;
            }
        }
        assert!(back, "Tab reaches the map");
    }

    /// 0024 D1 (§4.6): Back restores the records table's scroll offset (rendered): a far row of a
    /// long WAL table scrolls the table, a navigation leaves it, Back shows the same offset.
    #[test]
    fn back_restores_the_table_scroll_offset() {
        let mut rig = Rig::new(1400.0, 900.0, 1.0, Some("tests/fixtures/wal-fragmented/000004.log"));
        rig.opened();
        let list = crate::model::NodeKind::LogRecords { start: 0, page: 0 };
        rig.show(list.clone());
        rig.frames(2);
        let n = rig.app.st.session().view().map(|v| v.rows.len()).unwrap_or(0);
        assert!(n > 40, "a long table: {n}");
        rig.act(vec![Action::SelectRow(n - 5)]);
        // egui animates scroll_to_row: wait until the offset is steady.
        for _ in 0..200 {
            let before = rig.app.st.session().table_scroll();
            rig.frame();
            std::thread::sleep(std::time::Duration::from_millis(2));
            if before > 0.0 && (rig.app.st.session().table_scroll() - before).abs() < 0.01 {
                break;
            }
        }
        let y = rig.app.st.session().table_scroll();
        assert!(y > 50.0, "the table scrolled to the far row: {y}");
        rig.show(crate::model::NodeKind::Root);
        rig.frames(2);
        assert!(rig.app.st.session().table_scroll() < y, "another table");
        rig.act(vec![Action::Back]);
        rig.until("back", move |a| a.st.navigation().pending_target().is_none() && a.st.session().view_kind() == Some(&list));
        rig.frames(4);
        assert!((rig.app.st.session().table_scroll() - y).abs() < 1.0, "restored {} vs {y}", rig.app.st.session().table_scroll());
        assert_eq!(rig.app.st.selection().row(), Some(n - 5));
    }

    /// 0030 A8 (rendered, 1400×900, advanced example): the levels table showed 6 rows before
    /// and after the Map — not a stale Map split but the file views' default split (0.42) with
    /// the table floor of `TABLE_MIN_ROWS`. Now the levels table has its own split role: without
    /// a user split it gets its rows with the bytes keeping the Map's byte rows, so Table ↔ Map
    /// keeps the pane sizes; a user split in either mode survives the round trip; the file
    /// views keep theirs; the split handle's reset (and Reset View) clears all three.
    #[test]
    fn levels_table_keeps_its_rows_and_split_across_the_map() {
        let mut rig = Rig::new(1400.0, 900.0, 1.0, Some("examples/data/multilevel-advanced-db/CURRENT"));
        rig.opened();
        rig.frames(6);
        let shot = |rig: &Rig| rig.shot(0.0);
        let root_rows = shot(&rig).table_rows;
        let file_split = rig.app.st.prefs().split(SplitRole::Records);
        rig.show(crate::model::NodeKind::ManifestLevels);
        rig.frames(6);
        let before = shot(&rig);
        let n = rig.app.st.session().view().map(|v| v.rows.len()).unwrap_or(0) as f32;
        eprintln!("A8 root rows {root_rows}; levels table before map: {before:?} of {n} rows");
        assert!(before.table_rows > panels::records::TABLE_MIN_ROWS, "more than the table floor: {before:?}");
        assert!(before.table_rows >= n.min(15.0), "the levels table shows its rows: {before:?} of {n}");
        assert!(before.byte_rows >= LAYOUT.bytes_min_rows, "the bytes keep their minimum: {before:?}");
        rig.act(vec![Action::ShowLsmMap]);
        rig.until("map", |a| a.st.session().map_shown() && a.st.navigation().pending_target().is_none());
        rig.frames(6);
        let in_map = shot(&rig);
        rig.act(vec![Action::SetMapMode(crate::app::MapMode::Table)]);
        rig.frames(6);
        let after = shot(&rig);
        eprintln!("A8 map bytes {} rows; table after map: {after:?}", in_map.byte_rows);
        assert_eq!(after.table_rows, before.table_rows, "rows before == rows after the Map");
        assert_eq!(after.byte_rows, in_map.byte_rows, "Table ↔ Map keep the byte pane size");
        // A user split in the levels table survives Map and back; the Map's and the file views'
        // splits are separate.
        rig.act(vec![Action::SetCenterSplit(0.3)]);
        rig.frames(4);
        let user = shot(&rig);
        assert!(user.table_rows < before.table_rows, "the user's smaller split applies: {user:?}");
        rig.act(vec![Action::SetMapMode(crate::app::MapMode::Map)]);
        rig.frames(4);
        assert_eq!(rig.app.st.prefs().split(SplitRole::Map), None, "the Map keeps its automatic split");
        rig.act(vec![Action::SetCenterSplit(0.6)]);
        rig.act(vec![Action::SetMapMode(crate::app::MapMode::Table)]);
        rig.frames(4);
        assert_eq!(shot(&rig).table_rows, user.table_rows, "the levels table's user split survived the round trip");
        let p = rig.app.st.prefs();
        assert_eq!((p.split(SplitRole::LevelsTable), p.split(SplitRole::Map), p.split(SplitRole::Records)), (Some(0.3), Some(0.6), file_split));
        rig.act(vec![Action::ResetCenterSplit]);
        rig.frames(4);
        let p = rig.app.st.prefs();
        assert_eq!((p.split(SplitRole::LevelsTable), p.split(SplitRole::Map)), (None, Some(0.6)), "the handle's double-click resets only the mode shown");
        assert_eq!(shot(&rig).table_rows, before.table_rows, "reset restores the automatic levels split");
        rig.act(vec![Action::ResetView]);
        rig.frames(4);
        let p = rig.app.st.prefs();
        assert_eq!((p.split(SplitRole::LevelsTable), p.split(SplitRole::Map), p.split(SplitRole::Records)), (None, None, None), "Reset View resets every role");
        // 0030 A8-2: CURRENT (16 B) without a user split: the byte pane holds its content
        // (the byte minimum), the records the rest; an SST data view keeps the standard plan.
        rig.show(crate::model::NodeKind::Root);
        rig.frames(6);
        let cur = shot(&rig);
        assert_eq!(cur.byte_rows, LAYOUT.bytes_min_rows, "CURRENT: the byte minimum: {cur:?}");
        assert!(cur.table_rows > root_rows || cur.table_rows >= rig.app.st.session().view().map(|v| v.rows.len()).unwrap_or(0) as f32, "the records got the rest: {cur:?} (was {root_rows})");
    }

    /// 0030 A1 (tester P1: at 900×588 Fit changed nothing): the advanced example's CF 0 map at
    /// 900×588 overflows; entering it applies Fit once, which picks a smaller lane tier so every
    /// level frame shows (titles drawn); zoom-in returns to Full; Fit again picks the tier;
    /// Center keeps zoom and tier.
    #[test]
    fn fit_shows_every_level_at_900x588() {
        let mut rig = Rig::new(900.0, 588.0, 1.0, Some("examples/data/multilevel-advanced-db/CURRENT"));
        rig.opened();
        rig.act(vec![Action::ShowLsmMap]);
        rig.until("map", |a| a.st.session().map_shown() && a.st.navigation().pending_target().is_none());
        rig.frames(8);
        let map = rig.app.st.session().map();
        let m = map.measure().expect("measured");
        eprintln!("A1 900x588: nav {:?} tier {:?} heights {:?} view_h {} pan {:?}", map.nav(), map.tier(), map.heights(), m.view_h, map.pan());
        assert!(map.navigable(), "the Full scene overflows here");
        let fitted = map.tier();
        assert_ne!(fitted, crate::app::lsm_map::LaneTier::Full, "Fit reduced the lane detail");
        let h = map.heights().expect("tier heights");
        assert!(h[fitted.index()] <= m.view_h + 0.5 || fitted == crate::app::lsm_map::LaneTier::Bar, "every level fits: {h:?} in {}", m.view_h);
        // Reviewer N2: the rendered path centres the overflowing Bar scene vertically (top-aligned
        // when it fits).
        assert_eq!(map.pan().1, ((h[fitted.index()] - m.view_h) / 2.0).max(0.0), "Fit's vertical position");
        let texts = rig.texts();
        for l in ["L0 · ", "L1 · ", "L2 · "] {
            assert!(texts.iter().any(|t| t.2.starts_with(l)), "{l} title drawn");
        }
        rig.act(vec![Action::MapZoom(crate::app::MapZoom::In)]);
        rig.frames(2);
        assert_eq!(rig.app.st.session().map().tier(), crate::app::lsm_map::LaneTier::Full, "zoom-in: Full");
        rig.act(vec![Action::MapZoom(crate::app::MapZoom::Center)]);
        assert_eq!(rig.app.st.session().map().tier(), crate::app::lsm_map::LaneTier::Full, "Center keeps the tier");
        rig.act(vec![Action::MapZoom(crate::app::MapZoom::Fit)]);
        rig.frames(2);
        let map = rig.app.st.session().map();
        assert_eq!((map.tier(), map.zoom()), (fitted, 1.0), "Fit again");
        // No control flicker after Fit: the decision does not change over idle frames.
        let nav = map.navigable();
        for _ in 0..20 {
            rig.frame();
            assert_eq!(rig.app.st.session().map().navigable(), nav);
        }
    }

    /// 0030 A2-4/2: the LSM Map entry on a shown map that lost keyboard focus (Tab) gives it
    /// focus back and keeps the map (pick kept); with focus it switches to the table. While the
    /// map has focus the status bar shows its keys.
    #[test]
    fn lsm_map_entry_returns_focus_after_tab() {
        let mut rig = Rig::new(1400.0, 900.0, 1.0, Some("examples/data/multilevel-advanced-db/CURRENT"));
        rig.opened();
        rig.act(vec![Action::ShowLsmMap]);
        rig.until("map", |a| a.st.session().map_shown() && a.st.navigation().pending_target().is_none() && !a.st.jobs().wal_scanning());
        rig.frames(3);
        let id = panels::lsm_map::map_id();
        assert!(rig.ctx.memory(|m| m.has_focus(id)));
        assert!(rig.texts().iter().any(|t| t.2.starts_with("Map: arrows select")), "status-bar focus hint");
        press(&mut rig, &[egui::Key::ArrowUp]);
        rig.frames(1);
        let pick = rig.app.st.session().map().wal();
        assert!(pick.is_some(), "Up reached the WAL band");
        press(&mut rig, &[egui::Key::Tab]);
        rig.frames(1);
        assert!(!rig.ctx.memory(|m| m.has_focus(id)), "Tab left the map");
        assert!(!rig.texts().iter().any(|t| t.2.starts_with("Map: arrows select")), "no hint without focus");
        let mut out = Vec::new();
        rig.app.dispatch(&rig.ctx, vec![Action::ShowLsmMap]);
        out.push(rig.app.st.session().map_shown());
        rig.frames(2);
        assert!(rig.app.st.session().map_shown() && rig.ctx.memory(|m| m.has_focus(id)), "the entry gave the map focus back: {out:?}");
        assert_eq!(rig.app.st.session().map().wal(), pick, "pick kept");
        rig.app.dispatch(&rig.ctx, vec![Action::ShowLsmMap]);
        assert!(!rig.app.st.session().map_shown(), "with focus: the levels table");
    }

    /// Tester P3 B gap: a still pointer over a new cell of a hovered row gets that cell's tip after
    /// the delay, with frames only as the event loop schedules them (`on_demand`): (a) from a cell
    /// without a tip in the same row, (b) from a cell with a tip to the next row.
    #[test]
    fn a_still_pointer_on_a_new_cell_of_a_hovered_row_gets_its_tip() {
        // Tester P3 B gap: the row is hovered at a cell without a tooltip for 1 s; then one
        // pointer move to the File cell of the same row and a still pointer for 1 s: its tip.
        let mut rig = Rig::new(1400.0, 900.0, 1.0, Some("examples/data/multilevel-advanced-db/CURRENT"));
        rig.opened();
        rig.show(crate::model::NodeKind::ManifestLevels);
        rig.frames(4);
        let nav_right = rig.app.mem.left_w;
        let texts = rig.texts();
        let cf = texts.iter().find(|t| t.1.left() > nav_right && t.2 == "0 default").map(|t| t.1.left_center() + egui::vec2(3.0, 0.0)).expect("CF cell");
        let file = texts
            .iter()
            .filter(|x| (x.1.center().y - cf.y).abs() < 3.0 && x.2.len() == 6 && x.2.chars().all(|c| c.is_ascii_digit()))
            .map(|x| (x.1.left_center() + egui::vec2(3.0, 0.0), x.2.clone()))
            .next()
            .expect("File cell in the row");
        // The next row's File cell.
        let file2 = texts
            .iter()
            .filter(|x| x.1.center().y > cf.y + 8.0 && (x.1.left() - (file.0.x - 3.0)).abs() < 2.0 && x.2.len() == 6)
            .min_by(|a, b| a.1.top().total_cmp(&b.1.top()))
            .map(|x| (x.1.left_center() + egui::vec2(3.0, 0.0), x.2.clone()))
            .expect("File cell of the next row");
        let file_tip = |rig: &mut Rig, f: &str| rig.texts().iter().any(|x| x.2.starts_with(&format!("{f}.sst · Enter / double-click opens it")));
        // (a) The Level cell (between them) has no tooltip; a jump to the File cell of the same
        // row with a still pointer, frames only as the event loop would run them.
        let level = egui::pos2((cf.x + file.0.x) / 2.0 + 20.0, cf.y);
        on_demand(&mut rig, vec![egui::Event::PointerMoved(level)], 1.0);
        assert_eq!(tooltip_layers(&rig), 0, "no tooltip on the Level cell");
        let n = on_demand(&mut rig, vec![egui::Event::PointerMoved(file.0)], 1.2);
        assert!(file_tip(&mut rig, &file.1), "(a) the File cell's tip on a still pointer ({n} frames)");
        // (b) From a cell with a tooltip (CF) to the File cell of the NEXT row.
        on_demand(&mut rig, vec![egui::Event::PointerMoved(cf)], 1.0);
        assert_eq!(tooltip_layers(&rig), 1, "CF tip");
        let n = on_demand(&mut rig, vec![egui::Event::PointerMoved(file2.0)], 1.2);
        assert!(file_tip(&mut rig, &file2.1), "(b) the next row's File tip ({n} frames)");
    }

    /// 0030 B-2/B-6: in the levels table the CF cell explains the column family (three lines,
    /// comparator from the MANIFEST), and moving along the row to the File cell updates the
    /// tooltip in place (the open hint), without a click.
    #[test]
    fn levels_cf_cell_explains_and_row_tooltip_follows_the_pointer() {
        let mut rig = Rig::new(1400.0, 900.0, 1.0, Some("examples/data/multilevel-advanced-db/CURRENT"));
        rig.opened();
        rig.show(crate::model::NodeKind::ManifestLevels);
        rig.frames(4);
        let nav_right = rig.app.mem.left_w;
        let texts = rig.texts();
        let at = |s: &str| texts.iter().find(|t| t.1.left() > nav_right && t.2 == s && t.0.contains(t.1.left_center() + egui::vec2(3.0, 0.0))).map(|t| t.1.left_center() + egui::vec2(3.0, 0.0));
        let cf = at("0 default").expect("CF cell");
        let delay = (panels::TIP_DELAY_S * 60.0) as usize * 2;
        pointer(&mut rig, vec![egui::Event::PointerMoved(cf)]);
        rig.frames(delay);
        let t = rig.texts();
        assert_eq!(tooltip_layers(&rig), 1);
        assert!(t.iter().any(|x| x.2 == "Column family 0 \"default\" (per MANIFEST)."), "CF meaning");
        // Same row, File column (to the right of the CF cell).
        let file = texts
            .iter()
            .filter(|x| (x.1.center().y - cf.y).abs() < 3.0 && x.2.len() == 6 && x.2.chars().all(|c| c.is_ascii_digit()))
            .map(|x| (x.1.left_center() + egui::vec2(3.0, 0.0), x.2.clone()))
            .next()
            .expect("File cell in the row");
        pointer(&mut rig, vec![egui::Event::PointerMoved(file.0)]);
        rig.frames(3);
        let t = rig.texts();
        assert_eq!(tooltip_layers(&rig), 1, "one tooltip");
        assert!(
            t.iter().any(|x| x.2.starts_with(&format!("{}.sst · Enter / double-click opens it", file.1))),
            "content follows the pointer: {:?}",
            t.iter().map(|x| &x.2).filter(|s| s.contains(".sst")).collect::<Vec<_>>()
        );
    }

    /// 0030 A4: dragging the axis overview thumb pans the map (zoomed in, navigation mode).
    #[test]
    fn axis_thumb_drag_pans_the_map() {
        let mut rig = Rig::new(1400.0, 900.0, 1.0, Some("examples/data/multilevel-advanced-db/CURRENT"));
        rig.opened();
        rig.act(vec![Action::ShowLsmMap]);
        rig.until("map", |a| a.st.session().map_shown() && a.st.navigation().pending_target().is_none());
        rig.frames(4);
        if !rig.app.st.session().map().navigable() {
            // Fitting at this size: make it overflow with a short window.
            rig.h = 588.0;
            rig.frames(6);
        }
        assert!(rig.app.st.session().map().navigable(), "navigation mode");
        rig.act(vec![Action::MapView { pan: (0.0, 0.0), zoom: 3.0 }]);
        rig.frames(2);
        let texts = rig.texts();
        let axis_y = texts.iter().find(|t| t.2 == "key rank, not to scale ->").map(|t| t.1.bottom() + 3.0).expect("axis row");
        let nav_right = rig.app.mem.left_w;
        let x0 = nav_right + 30.0;
        let p0 = egui::pos2(x0, axis_y);
        let btn = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        let before = rig.app.st.session().map().pan().0;
        pointer(&mut rig, vec![egui::Event::PointerMoved(p0)]);
        pointer(&mut rig, vec![btn(p0, true)]);
        for k in 1..=10 {
            pointer(&mut rig, vec![egui::Event::PointerMoved(p0 + egui::vec2(k as f32 * 30.0, 0.0))]);
        }
        pointer(&mut rig, vec![btn(p0 + egui::vec2(300.0, 0.0), false)]);
        rig.frames(2);
        let after = rig.app.st.session().map().pan().0;
        assert!(after > before + 0.05, "the drag panned: {before} -> {after}");
    }

    /// Tester P4/P5 (A3, packages D2–D4): a click on the "600 files" bar zooms in; a click on one
    /// revealed box then keeps the zoom, selects that file and focuses it (ring). Rendered, with
    /// real pointer events through the painter. Uses the many-sst scratch of
    /// tests/tools/mk-wal-fixtures-0026.sh (600 L0 SSTs; skipped when it is absent).
    #[test]
    fn box_click_after_bar_zoom_keeps_the_zoom() {
        let dir = std::path::Path::new("/private/tmp/rocksvision-wal0026/many-sst");
        if !dir.join("CURRENT").exists() {
            eprintln!("SKIP: {} missing (bash tests/tools/mk-wal-fixtures-0026.sh)", dir.display());
            return;
        }
        let mut rig = Rig::new(1400.0, 900.0, 1.0, Some(dir.join("CURRENT").to_str().unwrap()));
        rig.opened();
        rig.act(vec![Action::ShowLsmMap]);
        rig.until("map", |a| a.st.session().map_shown() && a.st.navigation().pending_target().is_none());
        rig.frames(6);
        let click = |rig: &mut Rig, at: egui::Pos2| {
            let btn = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            pointer(rig, vec![egui::Event::PointerMoved(at)]);
            pointer(rig, vec![btn(true)]);
            pointer(rig, vec![btn(false)]);
            rig.frames(4);
        };
        let texts = rig.texts();
        let bar = texts.iter().find(|t| t.2 == "600 files" && t.1.left() > rig.app.mem.left_w).map(|t| t.1.center()).expect("the 600-files bar");
        click(&mut rig, bar);
        let z = rig.app.st.session().map().zoom();
        eprintln!("A3 after bar click: zoom {z} pan {:?}", rig.app.st.session().map().pan());
        assert!(z > 1.5, "the bar click zoomed: {z}");
        // A revealed box: the boxes now fill the L0 lane where the bar was (consecutive ranks).
        // A pause first, so the two clicks are not one double click (which opens the file).
        rig.frames(40);
        click(&mut rig, bar);
        let map = rig.app.st.session().map();
        eprintln!("A3 after box click: zoom {} pan {:?} focus {:?} sel {:?}", map.zoom(), map.pan(), map.focus(), rig.app.st.selection().row());
        let number = map.focus().expect("a file is focused");
        assert!((map.zoom() - z).abs() < 1e-3, "the zoom is kept: {z} -> {}", map.zoom());
        let row = rig.app.st.session().view().and_then(|v| v.lsm.by_number(number)).map(|(_, _, f)| f.row);
        assert_eq!(rig.app.st.selection().row(), row, "that file is selected");
        // The ring: the focused box is drawn with the focus ring (2 pt: the click gave the map
        // keyboard focus).
        assert!(rig.ctx.memory(|m| m.has_focus(panels::lsm_map::map_id())), "the map has keyboard focus");
    }

    /// Frames as a real event loop runs them (tester P3 B gap): one frame with `events`, then
    /// frames only when egui asked for a repaint (`repaint_delay`), with real time steps, for
    /// `secs` seconds. Returns how many frames ran.
    fn on_demand(rig: &mut Rig, events: Vec<egui::Event>, secs: f64) -> usize {
        let z = rig.ctx.zoom_factor();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(rig.w / z, rig.h / z));
        let mut t = rig.ctx.input(|i| i.time) + 1.0 / 60.0;
        let end = t + secs;
        let mut events = Some(events);
        let mut n = 0;
        loop {
            let input = egui::RawInput { screen_rect: Some(screen), time: Some(t), events: events.take().unwrap_or_default(), ..Default::default() };
            let app = &mut rig.app;
            let mut out = rig.ctx.run_ui(input, |ui| app.frame_ui(ui));
            out.textures_delta.clear();
            n += 1;
            let delay = out.viewport_output.get(&egui::ViewportId::ROOT).map(|v| v.repaint_delay).unwrap_or(std::time::Duration::MAX);
            if delay == std::time::Duration::MAX || t + delay.as_secs_f64() > end {
                return n;
            }
            t += delay.as_secs_f64().max(1.0 / 120.0);
        }
    }

    /// Tooltip layers shown in the last frame.
    fn tooltip_layers(rig: &Rig) -> usize {
        rig.ctx.memory(|m| m.areas().visible_layer_ids().into_iter().filter(|l| l.order == egui::Order::Tooltip).count())
    }

    /// 0030 B (owner's double popup, rendered): in the advanced example's WAL table an elided
    /// Field cell ("log number (from fil…") hovered without a click shows exactly ONE tooltip
    /// with the full text after the central delay (not before it); a fully visible
    /// self-explanatory cell ("file") shows none; a cell with an explanation ("—" without
    /// bytes) shows that explanation; Tooltips off shows nothing.
    #[test]
    fn table_cell_hover_shows_one_useful_tooltip_without_a_click() {
        let mut rig = Rig::new(1000.0, 700.0, 1.0, Some("examples/data/multilevel-advanced-db/000150.log"));
        rig.opened();
        rig.frames(4);
        let nav_right = rig.app.mem.left_w;
        let texts = rig.texts();
        // A visible table body cell: below the "Field" header, above the byte pane, its text's
        // left part inside its clip.
        let y_of = |s: &str| texts.iter().find(|t| t.2 == s).map(|t| t.1.center().y).unwrap_or_else(|| panic!("{s} drawn: {:?}", texts.iter().map(|t| (&t.2, t.1.left())).collect::<Vec<_>>()));
        let (top, bottom) = (y_of("Field") + 4.0, y_of("File bytes"));
        let cell = |p: &dyn Fn(&str) -> bool| {
            texts
                .iter()
                .find(|t| t.1.left() > nav_right && (top..bottom).contains(&t.1.center().y) && p(&t.2) && t.0.contains(t.1.left_center() + egui::vec2(4.0, 0.0)))
                .map(|t| (t.1.left_center() + egui::vec2(4.0, 0.0), t.2.clone()))
        };
        let (elided_at, elided) = cell(&|s| s.len() > 4 && s.ends_with('…')).unwrap_or_else(|| panic!("an elided value cell: {:?}", texts.iter().map(|t| (&t.2, t.0, t.1)).collect::<Vec<_>>()));
        let stem = elided.trim_end_matches('…');
        let full = rig.app.st.session().view().and_then(|v| v.rows.iter().flat_map(|r| r.cells.iter()).find(|c| c.starts_with(stem) && c.len() > stem.len()).cloned()).expect("the cell's full text");
        let full = full.as_str();
        let count = |texts: &[(egui::Rect, egui::Rect, String)], s: &str| texts.iter().filter(|t| t.2 == s).count();
        // Hover only: one frame moving there, then the pointer rests (1/60 s per frame).
        let sel0 = rig.app.st.selection().row();
        pointer(&mut rig, vec![egui::Event::PointerMoved(elided_at)]);
        let delay_frames = (panels::TIP_DELAY_S * 60.0) as usize;
        rig.frames(delay_frames / 2);
        assert_eq!(tooltip_layers(&rig), 0, "no tooltip before the delay");
        rig.frames(delay_frames);
        let t = rig.texts();
        assert_eq!(tooltip_layers(&rig), 1, "exactly one tooltip popup for {elided:?}");
        assert_eq!(count(&t, full), 1, "the full text once (egui's own elided tooltip is off)");
        assert_eq!(rig.app.st.selection().row(), sel0, "hover alone, no click: the selection is unchanged");
        // A fully visible, self-explanatory cell: no tooltip.
        let (file_at, _) = cell(&|s| s == "file").unwrap_or_else(|| panic!("'file' cell {top}..{bottom} {nav_right}: {:?}", texts.iter().filter(|t| t.2 == "file").collect::<Vec<_>>()));
        pointer(&mut rig, vec![egui::Event::PointerMoved(file_at)]);
        rig.frames(2 * delay_frames);
        assert_eq!(tooltip_layers(&rig), 0, "no tooltip on a fully visible cell without extra information");
        // A cell with an explanation: the explanation, once.
        let (dash_at, _) = cell(&|s| s == "—").expect("a '—' cell");
        pointer(&mut rig, vec![egui::Event::PointerMoved(dash_at)]);
        rig.frames(2 * delay_frames);
        let t = rig.texts();
        assert_eq!(tooltip_layers(&rig), 1, "the explanation of a byte-less cell");
        assert_eq!(t.iter().filter(|x| x.2.starts_with("No bytes in the file")).count(), 1);
        // Tooltips off: nothing.
        rig.act(vec![Action::SetTooltips(false)]);
        pointer(&mut rig, vec![egui::Event::PointerMoved(elided_at)]);
        rig.frames(2 * delay_frames);
        assert_eq!(tooltip_layers(&rig), 0, "View › Show Tooltips off");
    }

    /// 0024 §5: View › Show Tooltips off is saved by `App::save` and restored at startup; the
    /// menu item toggles it and shows it checked; the per-frame gate follows the preference.
    fn startup_tooltips_off_survives_restart() {
        use crate::app::prefs::KEY_TOOLTIPS;
        assert!(open_with(&[]).prefs().tooltips(), "default on");
        let mut rig = Rig::new(1400.0, 900.0, 1.0, None);
        rig.frame();
        assert!(panels::tooltips_on(&rig.ctx));
        let a = MenuCmd::ToggleTooltips.action(&rig.app.st).unwrap();
        assert_eq!(a, Action::SetTooltips(false));
        rig.act(vec![a]);
        rig.frame();
        assert!(!panels::tooltips_on(&rig.ctx) && MenuCmd::ToggleTooltips.state(&rig.app.st) == (true, false));
        let mut s = MemStorage::default();
        eframe::App::save(&mut rig.app, &mut s);
        use eframe::Storage as _;
        assert_eq!(s.get_string(KEY_TOOLTIPS).as_deref(), Some("false"));
        let st = open_with(&[(KEY_TOOLTIPS.to_string(), "false".to_string())]);
        assert!(!st.prefs().tooltips(), "restart keeps tooltips off");
    }

    /// 0017 A: missing → XCopy, unknown → XCopy, saved Matrix survives store/load and reopen.
    #[test]
    fn startup_theme_default_fallback_and_saved_choice() {
        startup_tooltips_off_survives_restart();
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
