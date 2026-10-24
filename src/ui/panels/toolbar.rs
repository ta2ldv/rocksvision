//! Toolbar (spec §6.6): [Open Folder… · Open File…] [path · Go] [search · Hex bytes · Prev ·
//! Next · Clear search] [Copy…] [A− zoom A+ · Reset View]. The five groups are atomic: rows are
//! planned from last frame's measured widths (`layout::pack_rows`) and the path field takes the
//! rest of its row. View/Theme/About live in the menu bar; without a native menu bar the same
//! menu tree is offered as an in-window "Menu" button.

use super::{btn, status_text, PanelMemory};
use crate::app::{Action, AppState, CopyKind};
use crate::model::Status;
use crate::ui::layout::{pack_rows, LAYOUT, PACK_SLACK};
use crate::ui::menu::{self, MenuCmd, MenuNode, RESET_VIEW_DEFAULT_TIP, RESET_VIEW_TIP};
use crate::ui::theme::Tokens;
use eframe::egui;

const SEARCH_HINT: &str = "Rows: table · Bytes: file";
const SEARCH_HELP: &str = "Search the current table's decoded cells and this file's bytes. Enter: next match · Shift+Enter: previous · F3 / Shift+F3.";
const HEX_HELP: &str = "Read the query as hex byte values, e.g. 4d 41 4e (\"MAN\"). Search scope is unchanged.";
pub const SEARCH_ID: &str = "search";

/// Number of toolbar groups (open, path, search, copy, zoom).
pub const GROUPS: usize = 5;

pub fn show(ui: &mut egui::Ui, st: &AppState, tok: &Tokens, mem: &mut PanelMemory, fallback_menu: Option<&[MenuNode]>, view_is_default: bool) -> Vec<Action> {
    show_groups(ui, st, tok, mem, fallback_menu, view_is_default).0
}

/// `show` plus the rects of the five groups (tests check that none ends past the toolbar).
fn show_groups(ui: &mut egui::Ui, st: &AppState, tok: &Tokens, mem: &mut PanelMemory, fallback_menu: Option<&[MenuNode]>, view_is_default: bool) -> (Vec<Action>, [egui::Rect; GROUPS]) {
    let mut acts = Vec::new();
    let mut rects = [egui::Rect::NOTHING; GROUPS];
    tok.paint_chrome(ui, ui.clip_rect());
    let gap = ui.spacing().item_spacing.x;
    let avail = ui.available_width();
    let starts = pack_rows(&mem.toolbar_w, gap, avail);
    let row_of = |g: usize| starts.iter().rposition(|&s| s <= g).unwrap_or(0);
    // The path field gets the leftover width of its row.
    let path_row = row_of(1);
    let row_end = starts.get(path_row + 1).copied().unwrap_or(GROUPS);
    let used: f32 = mem.toolbar_w[starts[path_row]..row_end].iter().sum::<f32>() + gap * (row_end - starts[path_row]).saturating_sub(1) as f32;
    let path_w = LAYOUT.path_min + (avail - PACK_SLACK - used).max(0.0);
    let mut measured = mem.toolbar_w;
    let mut row_h: f32 = 0.0;
    ui.vertical(|ui| {
        for (r, &s) in starts.iter().enumerate() {
            let e = starts.get(r + 1).copied().unwrap_or(GROUPS);
            let row = ui.horizontal(|ui| {
                for g in s..e {
                    let resp = ui.horizontal(|ui| group(ui, g, st, tok, &mut acts, fallback_menu, path_w, view_is_default));
                    rects[g] = resp.response.rect;
                    // The path group is measured at its minimum (the field shrinks first).
                    measured[g] = if g == 1 { resp.response.rect.width() - path_w + LAYOUT.path_min } else { resp.response.rect.width() };
                }
            });
            row_h = row_h.max(row.response.rect.height());
        }
    });
    // Rows were planned from last frame's widths. After a theme/font/zoom change they are stale
    // and a group may end past the edge; egui repaints reactively, so without another pass that
    // frame would stay on screen (screenshots/matrix.png). Re-run the pass once instead.
    if measured.iter().zip(&mem.toolbar_w).any(|(a, b)| (a - b).abs() > 0.5) {
        ui.ctx().request_discard("toolbar group widths changed");
        ui.ctx().request_repaint();
    }
    mem.toolbar_w = measured;
    mem.toolbar_row_h = row_h + ui.spacing().item_spacing.y;
    if let Some(e) = st.session().path_error() {
        status_text(ui, tok, Status::Invalid, e);
    }
    (acts, rects)
}

#[allow(clippy::too_many_arguments)]
fn group(ui: &mut egui::Ui, g: usize, st: &AppState, tok: &Tokens, acts: &mut Vec<Action>, fallback_menu: Option<&[MenuNode]>, path_w: f32, view_is_default: bool) {
    match g {
        0 => {
            if let Some(nodes) = fallback_menu {
                let r = ui.menu_button("Menu", |ui| {
                    if let Some(a) = menu::show_fallback(ui, st, tok, nodes).and_then(|c| c.action(st)) {
                        acts.push(a);
                    }
                });
                tok.bevel(ui.painter(), r.response.rect);
            }
            if btn(ui, tok, "📂 Open Folder…").on_hover_text(menu::tip("Open a RocksDB data folder", MenuCmd::OpenFolder)).clicked() {
                acts.push(Action::PickFolder);
            }
            if btn(ui, tok, "📄 Open File…").on_hover_text(menu::tip("Open a WAL, SST, MANIFEST or CURRENT file", MenuCmd::OpenFile)).clicked() {
                acts.push(Action::PickFile);
            }
        }
        1 => {
            let mut path = st.session().path_input().to_string();
            // TextEdit draws its frame margins outside `desired_width`.
            let w = path_w - 2.0 * ui.spacing().button_padding.x;
            let r = ui.add(egui::TextEdit::singleline(&mut path).hint_text("Path to folder or file").desired_width(w));
            if r.changed() {
                acts.push(Action::SetPathInput(path));
            }
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if btn(ui, tok, "Go").clicked() || enter {
                acts.push(Action::GoPathInput);
            }
        }
        2 => {
            let font = egui::TextStyle::Body.resolve(ui.style());
            let hint_w = ui.fonts_mut(|f| f.layout_no_wrap(SEARCH_HINT.into(), font, tok.p.text_muted).size().x) + 16.0;
            let mut q = st.search().query().to_string();
            let sr = ui.add(egui::TextEdit::singleline(&mut q).id(egui::Id::new(SEARCH_ID)).hint_text(SEARCH_HINT).desired_width(hint_w.max(LAYOUT.search_width))).on_hover_text(SEARCH_HELP);
            if sr.changed() {
                acts.push(Action::SetSearch(q));
            }
            // Enter / Shift+Enter were dispatched once by `keys` (the field had focus at frame
            // start); the field gave up focus on Enter, so take it back for the next press.
            if sr.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                sr.request_focus();
            }
            let mut hex = st.search().hex_mode();
            if ui.checkbox(&mut hex, "Hex bytes").on_hover_text(HEX_HELP).changed() {
                acts.push(Action::SetSearchHex(hex));
            }
            if btn(ui, tok, "Prev").on_hover_text(format!("Previous match ({} / Shift+F3 / Shift+Enter)", menu::shortcut_text(MenuCmd::FindPrev).unwrap_or_default())).clicked() {
                acts.push(Action::SearchPrev);
            }
            if btn(ui, tok, "Next").on_hover_text(format!("Next match ({} / F3 / Enter)", menu::shortcut_text(MenuCmd::FindNext).unwrap_or_default())).clicked() {
                acts.push(Action::SearchNext);
            }
            if btn(ui, tok, "Clear search").on_hover_text("Clear search text and match highlights. Selection is kept.").clicked() {
                acts.push(Action::ClearSearch);
            }
        }
        3 => {
            let copy = ui.menu_button("Copy…", |ui| {
                let has_sel = st.selection().span().is_some();
                let has_field = st.selected_field().is_some();
                let has_key = st.selected_field().map(|f| f.key_bytes.is_some()).unwrap_or(false);
                for (k, label, en) in [(CopyKind::Value, "Copy value", has_field), (CopyKind::Key, "Copy key", has_key), (CopyKind::Hex, "Copy hex (max 64 KiB)", has_sel), (CopyKind::Offset, "Copy offset", has_sel), (CopyKind::RowTsv, "Copy row (TSV)", has_field)] {
                    let item = ui.add_enabled(en, egui::Button::new(label));
                    tok.menu_item_hover(ui, &item);
                    if item.clicked() {
                        acts.push(Action::Copy(k));
                        ui.close();
                    }
                }
                let item = ui.add_enabled(st.session().file().is_some(), egui::Button::new("Copy file path"));
                tok.menu_item_hover(ui, &item);
                if item.clicked() {
                    acts.push(Action::CopyFilePath);
                    ui.close();
                }
            });
            tok.bevel(ui.painter(), copy.response.rect);
        }
        _ => {
            if btn(ui, tok, "A−").on_hover_text(menu::tip("Zoom out", MenuCmd::ZoomOut)).clicked() {
                acts.push(Action::ZoomOut);
            }
            ui.label(format!("{:.0}%", st.prefs().zoom() * 100.0));
            let can = st.can_zoom_in();
            let plus = ui.add_enabled(can, egui::Button::new("A+"));
            tok.bevel(ui.painter(), plus.rect);
            let plus = plus.on_hover_text(menu::tip("Zoom in", MenuCmd::ZoomIn)).on_disabled_hover_text(format!("At layout limit ({:.0}%)", st.prefs().zoom_ceiling() * 100.0));
            if plus.clicked() {
                acts.push(Action::ZoomIn);
            }
            let reset = ui.add_enabled(!view_is_default, egui::Button::new("Reset View"));
            tok.bevel(ui.painter(), reset.rect);
            if reset.on_hover_text(menu::tip(RESET_VIEW_TIP, MenuCmd::ResetView)).on_disabled_hover_text(RESET_VIEW_DEFAULT_TIP).clicked() {
                acts.push(Action::ResetView);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::{apply_theme, THEMES};

    /// Runs `frames` toolbar frames at `w` points (logical, zoom `z`) and returns the rightmost
    /// group edge minus the toolbar's right edge (> 0 = cut off).
    fn run(ctx: &egui::Context, st: &AppState, mem: &mut PanelMemory, theme: usize, w: f32, z: f32, frames: usize) -> f32 {
        let mut cache = Vec::new();
        let tok = apply_theme(ctx, &THEMES[theme], &mut cache);
        ctx.set_zoom_factor(z);
        let mut out = 0.0;
        for _ in 0..frames {
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w * z, 900.0 * z))), ..Default::default() };
            let _ = ctx.run_ui(input, |ui| {
                let frame = egui::Frame::new().inner_margin(egui::Margin::symmetric(8, 4));
                egui::Panel::top("toolbar").frame(frame).show(ui, |ui| {
                    let right = ui.max_rect().right();
                    let (_, rects) = show_groups(ui, st, &tok, mem, None, false);
                    out = rects.iter().map(|r| r.right()).fold(f32::MIN, f32::max) - right;
                });
            });
        }
        out
    }

    /// Spec §6.6 / 0009 item 2: every group (incl. A− 100% A+ · Reset View) ends inside the
    /// toolbar for all six themes with their real fonts, at 1400 pt, the minimum window and a
    /// high zoom, also right after a theme switch from another font family.
    #[test]
    fn toolbar_groups_never_cut_in_any_theme() {
        let st = AppState::idle_for_tests();
        for (i, t) in THEMES.iter().enumerate() {
            for (w, z) in [(1400.0, 1.0), (1728.0, 1.0), (900.0, 1.0), (1400.0 / 1.5, 1.5), (900.0 / 1.3, 1.3)] {
                let ctx = egui::Context::default();
                let mut mem = PanelMemory::default();
                // Start in another theme (other font family), then switch.
                let _ = run(&ctx, &st, &mut mem, (i + 1) % THEMES.len(), w, z, 3);
                // The first frame after the switch already fits (the stale pass is discarded).
                for frames in [1usize, 1, 4] {
                    let over = run(&ctx, &st, &mut mem, i, w, z, frames);
                    assert!(over <= 0.5, "{} at {w}x{z}: cut by {over} pt after {frames} frame(s)", t.id);
                }
                // Stable: no further discards/repaints once measured.
                let before = mem.toolbar_w;
                let _ = run(&ctx, &st, &mut mem, i, w, z, 1);
                assert_eq!(before, mem.toolbar_w, "{} at {w}x{z}: widths keep changing", t.id);
            }
        }
    }
}
