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

use crate::app::jobs::Worker;
use crate::app::prefs::Prefs;
use crate::app::{Action, AppState, Effect, Panel, ZoomFit};
use crate::branding;
use crate::limits::LIMITS;
use eframe::egui;
use layout::{side_max, split_heights, Measured, LAYOUT};
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
            let mut acts = vec![Action::SetTheme(theme::theme_by_id(&prefs.theme).id), Action::SetZoom(prefs.zoom)];
            if let Some(p) = initial {
                acts.push(Action::OpenPath(p));
            }
            app.dispatch(&cc.egui_ctx, acts);
            app.sync_theme(&cc.egui_ctx);
            Ok(Box::new(app))
        }),
    )
}

struct App {
    st: AppState,
    tok: Tokens,
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
        App { st, tok, applied_theme: None, grain_cache: Vec::new(), logo: None, mem: PanelMemory::default(), platform, menu: menu::tree() }
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
        if self.logo.is_none() {
            if let Ok(icon) = eframe::icon_data::from_png_bytes(branding::LOGO_PNG) {
                let img = egui::ColorImage::from_rgba_unmultiplied([icon.width as usize, icon.height as usize], &icon.rgba);
                self.logo = Some(ctx.load_texture("about-logo", img, egui::TextureOptions::LINEAR));
            }
        }
        self.logo.clone()
    }

    /// Measurements of the minimum layout (logical points). With `ceiling` (zoom ceiling, spec
    /// §8.1) the inputs do not depend on data: the byte rows use 16 offset digits, the hex
    /// header groups worst-case static texts (`hex::ceiling_groups`) and the toolbar always
    /// reserves the path-error line. Without it (row planning: records/bytes split) the live
    /// space size and last frame's measured header widths are used.
    fn measured(&self, ui: &egui::Ui, ceiling: bool) -> Measured {
        let hex = panels::hex::metrics(ui, if ceiling { u64::MAX } else { self.st.session().hex().size() });
        let rh = self.tok.row_height(ui);
        let line = ui.text_style_height(&egui::TextStyle::Body);
        let hex_row = panels::hex::row_height(ui);
        Measured {
            nav_min: LAYOUT.left_min,
            explain_min: LAYOUT.right_min,
            strip: self.tok.header_height(ui),
            hex,
            toolbar_groups: self.mem.toolbar_w.to_vec(),
            toolbar_gap: ui.spacing().item_spacing.x,
            toolbar_row_h: self.mem.toolbar_row_h,
            // Frame (4 pt top/bottom + 1 pt stroke) + the path-error line (`status_text`: one
            // body line, 1 pt margins, item spacing), reserved whether or not it is shown.
            toolbar_pad_h: 2.0 * 4.0 + 1.0 + if ceiling { line + 2.0 + ui.spacing().item_spacing.y } else { 0.0 },
            status_h: self.mem.status_h,
            records_min_h: self.tok.header_height(ui) + (LAYOUT.records_min_rows - 1.0) * rh + line + ui.text_style_height(&egui::TextStyle::Small) + 2.0 * LAYOUT.center_margin[1],
            hex_groups: if ceiling { panels::hex::ceiling_groups(ui) } else { self.mem.hex.groups },
            hex_gap: ui.spacing().item_spacing.x,
            hex_line_h: self.mem.hex.line_h,
            hex_ruler_h: hex_row,
            hex_row_h: hex_row,
            hex_pad_h: 2.0 * LAYOUT.center_margin[1] + ui.spacing().scroll.allocated_width(),
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
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
        let width = screen.width();
        let rh = self.tok.row_height(ui);
        let hexm = panels::hex::metrics(ui, self.st.session().hex().size());
        let center_min = layout::center_min(&hexm);
        let strip = self.tok.header_height(ui);
        let (show_left, show_right, show_hex) = (self.st.prefs().show_left(), self.st.prefs().show_right(), self.st.prefs().show_hex());
        let (need_w, need_h) = self.measured(ui, true).need(true, true);
        let measured = self.measured(ui, false);
        self.dispatch(&ctx, vec![Action::LayoutFit(ZoomFit { at_zoom: ctx.zoom_factor(), avail_w: width, avail_h: screen.height(), need_w, need_h })]);
        let zoom = self.st.prefs().zoom();
        if (ctx.zoom_factor() - zoom).abs() > 0.001 {
            ctx.set_zoom_factor(zoom);
        }

        let p = self.tok.p;
        let frame = |fill| egui::Frame::new().fill(fill).inner_margin(egui::Margin::symmetric(8, 4));
        let e = self.st.prefs().layout_epoch();
        let mut acts = Vec::new();
        let fallback = if self.platform.has_native_menu() { None } else { Some(self.menu.as_slice()) };
        let view_is_default = self.st.prefs().is_default_layout() && self.mem.widths_default;
        let r = egui::Panel::top("toolbar")
            .frame(frame(p.toolbar).stroke(egui::Stroke::new(1.0, p.border)))
            .show(ui, |ui| acts = panels::toolbar::show(ui, &self.st, &self.tok, &mut self.mem, fallback, view_is_default));
        self.mem.toolbar_h = r.response.rect.height();
        self.dispatch(&ctx, std::mem::take(&mut acts));
        let hidden: Vec<(&str, String)> = [Panel::Navigation, Panel::Explanation, Panel::Bytes].into_iter().filter(|p| !self.st.prefs().shown(*p)).map(show_hint).collect();
        let r = egui::Panel::bottom("status")
            .frame(egui::Frame::new().fill(p.toolbar).inner_margin(egui::Margin::symmetric(8, LAYOUT.status_pad as i8)).stroke(egui::Stroke::new(1.0, p.border)))
            .resizable(false)
            .show(ui, |ui| acts = panels::status::show(ui, &self.st, &self.tok, &hidden));
        self.mem.status_h = r.response.rect.height();
        self.dispatch(&ctx, std::mem::take(&mut acts));

        // Side panels: never hidden automatically; clamped so the center keeps 4 bytes per row
        // (Explanation shrinks first, then Navigation).
        let left = show_left.then_some((LAYOUT.left_min, LAYOUT.left_max));
        let right = show_right.then_some((LAYOUT.right_min, LAYOUT.right_max));
        let (left_max, _) = side_max(width, center_min, strip, left, self.mem.left_w, right);
        let sep = self.tok.w.separator;
        let mut widths_default = true;
        if show_left {
            let r = egui::Panel::left(egui::Id::new(("nav", e)))
                .frame(frame(p.panel))
                .resizable(true)
                .show_separator_line(false)
                .default_size(LAYOUT.left_default.min(left_max))
                .size_range(LAYOUT.left_min..=left_max)
                .show(ui, |ui| acts = panels::nav::show(ui, &self.st, &self.tok));
            let rect = r.response.rect;
            self.mem.left_w = rect.width();
            widths_default &= (rect.width() - LAYOUT.left_default.min(left_max)).abs() < 1.5;
            let x = rect.right() - sep.width / 2.0;
            ui.painter().line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], sep);
        } else {
            egui::Panel::left(egui::Id::new("nav_strip"))
                .frame(egui::Frame::new().fill(p.panel).stroke(egui::Stroke::new(1.0, p.border)))
                .resizable(false)
                .exact_size(strip)
                .show(ui, |ui| {
                    if panels::collapse_strip(ui, &self.tok, "Navigation", "▶", true, &menu::tip("Show Navigation", MenuCmd::TogglePanel(Panel::Navigation))) {
                        acts.push(Action::TogglePanel(Panel::Navigation));
                    }
                });
            self.mem.left_w = 0.0;
        }
        self.dispatch(&ctx, std::mem::take(&mut acts));
        let (_, right_max) = side_max(width, center_min, strip, left, self.mem.left_w, right);
        if show_right {
            let r = egui::Panel::right(egui::Id::new(("explain", e)))
                .frame(frame(p.explain))
                .resizable(true)
                .show_separator_line(false)
                .default_size(LAYOUT.right_default.min(right_max))
                .size_range(LAYOUT.right_min..=right_max)
                .show(ui, |ui| acts = panels::explain::show(ui, &self.st, &self.tok));
            let rect = r.response.rect;
            self.mem.right_w = rect.width();
            widths_default &= (rect.width() - LAYOUT.right_default.min(right_max)).abs() < 1.5;
            let x = rect.left() + sep.width / 2.0;
            ui.painter().line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], sep);
        } else {
            egui::Panel::right(egui::Id::new("explain_strip"))
                .frame(egui::Frame::new().fill(p.panel).stroke(egui::Stroke::new(1.0, p.border)))
                .resizable(false)
                .exact_size(strip)
                .show(ui, |ui| {
                    if panels::collapse_strip(ui, &self.tok, "Explanation", "◀", false, &menu::tip("Show Explanation", MenuCmd::TogglePanel(Panel::Explanation))) {
                        acts.push(Action::TogglePanel(Panel::Explanation));
                    }
                });
            self.mem.right_w = 0.0;
        }
        self.mem.widths_default = widths_default;
        self.dispatch(&ctx, std::mem::take(&mut acts));
        let scroll = self.st.take_scroll_request();
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
        if self.st.needs_fast_repaint() {
            ctx.request_repaint_after(std::time::Duration::from_millis(LIMITS.busy_repaint_ms));
        }
        if self.st.watching_file() {
            ctx.request_repaint_after(std::time::Duration::from_secs(LIMITS.change_check_secs));
        }
        if let Some(left) = self.st.prefs().notice_remaining() {
            // Clear a timed notice (Reset View) on time.
            ctx.request_repaint_after(left + std::time::Duration::from_millis(50));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.st.prefs().persisted().store(|k, v| storage.set_string(k, v));
    }

    /// Only the theme id and zoom are persisted; panel layout persistence is deferred.
    fn persist_egui_memory(&self) -> bool {
        false
    }
}

impl App {
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
        let (rec_h, hex_h) = split_heights(full.height() - handle, self.st.prefs().center_split(), measured.records_min_h, hex_min);
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
        } else if resp.dragged() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let h = (full.height() - handle).max(1.0);
                acts.push(Action::SetCenterSplit(((pos.y - handle / 2.0 - full.top()) / h).clamp(0.0, 1.0)));
            }
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
