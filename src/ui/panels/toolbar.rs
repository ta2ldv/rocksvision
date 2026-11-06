//! Toolbar (spec §6.6): [Open Folder… · Open File…] [LSM Map] [path · Go] [search · Hex bytes ·
//! Prev · Next · Clear search] [Copy…] [A− zoom A+ · Reset View]. The six groups are atomic: rows are
//! planned from last frame's measured widths (`layout::pack_rows`) and the path field takes the
//! rest of its row. View/Theme/About live in the menu bar; without a native menu bar the same
//! menu tree is offered as an in-window "Menu" button.

use super::{PanelMemory, TipExt, btn, disabled_btn, latched_btn, status_text};
use crate::app::{Action, AppState, CopyKind};
use crate::model::Status;
use crate::ui::layout::{LAYOUT, PACK_SLACK, pack_rows};
use crate::ui::menu::{self, MenuCmd, MenuNode, RESET_VIEW_DEFAULT_TIP, RESET_VIEW_TIP};
use crate::ui::theme::Tokens;
use eframe::egui;

const SEARCH_HINT: &str = "Rows: table · Bytes: file";
const SEARCH_HELP: &str = "Search the current table's decoded cells and this file's bytes. Enter: next match · Shift+Enter: previous · F3 / Shift+F3.";
const HEX_HELP: &str = "Read the query as hex byte values, e.g. 4d 41 4e (\"MAN\"). Search scope is unchanged.";
pub const SEARCH_ID: &str = "search";

/// Number of toolbar groups (open, LSM Map, path, search, copy, zoom).
pub const GROUPS: usize = 6;
/// Index of the path group (its field takes the leftover width of its row).
pub const PATH_GROUP: usize = 2;

pub fn show(ui: &mut egui::Ui, st: &AppState, tok: &Tokens, mem: &mut PanelMemory, fallback_menu: Option<&[MenuNode]>, view_is_default: bool) -> Vec<Action> {
    show_groups(ui, st, tok, mem, fallback_menu, view_is_default).0
}

/// `show` plus the rects of the six groups (tests check that none ends past the toolbar).
fn show_groups(ui: &mut egui::Ui, st: &AppState, tok: &Tokens, mem: &mut PanelMemory, fallback_menu: Option<&[MenuNode]>, view_is_default: bool) -> (Vec<Action>, [egui::Rect; GROUPS]) {
    let mut acts = Vec::new();
    let mut rects = [egui::Rect::NOTHING; GROUPS];
    tok.paint_chrome(ui, ui.clip_rect());
    let gap = ui.spacing().item_spacing.x;
    let avail = ui.available_width();
    let starts = pack_rows(&mem.toolbar_w, gap, avail);
    let row_of = |g: usize| starts.iter().rposition(|&s| s <= g).unwrap_or(0);
    // The path field gets the leftover width of its row.
    let path_row = row_of(PATH_GROUP);
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
                    measured[g] = if g == PATH_GROUP { resp.response.rect.width() - path_w + LAYOUT.path_min } else { resp.response.rect.width() };
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
            if btn(ui, tok, "📂 Open Folder…").tip(menu::tip("Open a RocksDB data folder", MenuCmd::OpenFolder)).clicked() {
                acts.push(Action::PickFolder);
            }
            if btn(ui, tok, "📄 Open File…").tip(menu::tip("Open a WAL, SST, MANIFEST or CURRENT file", MenuCmd::OpenFile)).clicked() {
                acts.push(Action::PickFile);
            }
        }
        1 => lsm_map_button(ui, st, tok, acts),
        PATH_GROUP => {
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
        3 => {
            let font = egui::TextStyle::Body.resolve(ui.style());
            let hint_w = ui.fonts_mut(|f| f.layout_no_wrap(SEARCH_HINT.into(), font, tok.p.text_muted).size().x) + 16.0;
            let mut q = st.search().query().to_string();
            let sr = ui.add(egui::TextEdit::singleline(&mut q).id(egui::Id::new(SEARCH_ID)).hint_text(SEARCH_HINT).desired_width(hint_w.max(LAYOUT.search_width))).tip(SEARCH_HELP);
            if sr.changed() {
                acts.push(Action::SetSearch(q));
            }
            // Enter / Shift+Enter were dispatched once by `keys` (the field had focus at frame
            // start); the field gave up focus on Enter, so take it back for the next press.
            if sr.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                sr.request_focus();
            }
            let mut hex = st.search().hex_mode();
            if ui.checkbox(&mut hex, "Hex bytes").tip(HEX_HELP).changed() {
                acts.push(Action::SetSearchHex(hex));
            }
            // Disabled for an empty query or an invalid hex sequence (they would do nothing).
            let step = st.search().can_step();
            if ui
                .add_enabled_ui(step, |ui| btn(ui, tok, "Prev"))
                .inner
                .tip(format!("Previous match ({} / Shift+F3 / Shift+Enter)", menu::shortcut_text(MenuCmd::FindPrev).unwrap_or_default()))
                .clicked()
            {
                acts.push(Action::SearchPrev);
            }
            if ui.add_enabled_ui(step, |ui| btn(ui, tok, "Next")).inner.tip(format!("Next match ({} / F3 / Enter)", menu::shortcut_text(MenuCmd::FindNext).unwrap_or_default())).clicked() {
                acts.push(Action::SearchNext);
            }
            if btn(ui, tok, "Clear search").tip("Clear search text and match highlights. Selection is kept.").clicked() {
                acts.push(Action::ClearSearch);
            }
        }
        4 => {
            let copy = ui.menu_button("Copy…", |ui| {
                let has_sel = st.selection().span().is_some();
                let has_field = st.selected_field().is_some();
                let has_key = st.selected_field().map(|f| f.key_bytes.is_some()).unwrap_or(false);
                for (k, label, en) in [
                    (CopyKind::Value, "Copy value", has_field),
                    (CopyKind::Key, "Copy key", has_key),
                    (CopyKind::Hex, "Copy hex (max 64 KiB)", has_sel),
                    (CopyKind::Offset, "Copy offset", has_sel),
                    (CopyKind::RowTsv, "Copy row (TSV)", has_field),
                ] {
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
            if btn(ui, tok, "A−").tip(menu::tip("Zoom out", MenuCmd::ZoomOut)).clicked() {
                acts.push(Action::ZoomOut);
            }
            ui.label(format!("{:.0}%", st.prefs().zoom() * 100.0));
            let can = st.can_zoom_in();
            if can {
                let plus = ui.button("A+");
                tok.bevel(ui.painter(), plus.rect);
                if plus.tip(menu::tip("Zoom in", MenuCmd::ZoomIn)).clicked() {
                    acts.push(Action::ZoomIn);
                }
            } else {
                // Disabled A+: muted text, no fill, a thin outline only, no hover effect.
                let plus = disabled_btn(ui, tok, "A+");
                plus.tip_disabled(zoom_limit_tip(st.prefs().zoom_ceiling()));
            }
            let reset = ui.add_enabled(!view_is_default, egui::Button::new("Reset View"));
            tok.bevel(ui.painter(), reset.rect);
            if reset.tip(menu::tip(RESET_VIEW_TIP, MenuCmd::ResetView)).tip_disabled(RESET_VIEW_DEFAULT_TIP).clicked() {
                acts.push(Action::ResetView);
            }
        }
    }
}

/// Tooltip of the LSM Map button in each state (UI advice 0024 §1.6–1.7); the disabled reason
/// is the one availability text (`DocumentSession::lsm_map_availability`).
pub fn lsm_map_tip(st: &AppState) -> (bool, bool, String) {
    use crate::app::lsm_map::MapEntry;
    let key = menu::shortcut_text(MenuCmd::LsmMap).unwrap_or_default();
    // Enabled/active from the one rule the menu item uses too; only the wording is the toolbar's.
    let (enabled, active) = st.session().lsm_map_entry_state();
    let tip = match (active, st.session().lsm_map_availability()) {
        (true, _) => format!("LSM Map ({key}) — also returns keyboard focus to the map; when the map has focus, shows the levels table"),
        (false, Ok(MapEntry::Via { from: Some(name), .. })) => format!("Show this folder's LSM map and locate {name} ({key})"),
        (false, Ok(_)) => format!("{} ({key})", menu::LSM_MAP_TIP),
        // "… Open Folder… (Shift+⌘O), or …" (Ctrl+Shift+O elsewhere), advice §1.6.
        (false, Err(r)) => crate::app::lsm_map::with_open_folder_key(&r.0, &menu::shortcut_text(MenuCmd::OpenFolder).unwrap_or_default()),
    };
    (enabled, active, tip)
}

/// The LSM Map entry: latched while the map is shown, disabled with the reason when there is no
/// trustworthy MANIFEST. Button, menu item and Shift+⌘M dispatch the same action.
fn lsm_map_button(ui: &mut egui::Ui, st: &AppState, tok: &Tokens, acts: &mut Vec<Action>) {
    let (enabled, active, tip) = lsm_map_tip(st);
    let r = if active {
        latched_btn(ui, tok, LSM_MAP_LABEL)
    } else if enabled {
        btn(ui, tok, LSM_MAP_LABEL)
    } else {
        disabled_btn(ui, tok, LSM_MAP_LABEL)
    };
    let r = if enabled { r.tip(tip) } else { r.tip_disabled(tip) };
    if r.clicked() {
        acts.push(Action::ShowLsmMap);
    }
}

/// The LSM Map button label (text only; no glyph is free of other meanings).
pub const LSM_MAP_LABEL: &str = "LSM Map";

/// Tooltip of the disabled A+ at the layout-aware zoom ceiling.
pub fn zoom_limit_tip(ceiling: f32) -> String {
    format!("Largest zoom that fits this window ({:.0} %). Enlarge the window to zoom further.", ceiling * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::{THEMES, apply_theme};

    /// 0024 §1: the LSM Map group sits right after Open (index 1, the path group moved to 2) and
    /// is disabled with the availability reason when nothing is open.
    #[test]
    fn lsm_map_group_and_disabled_reason() {
        assert_eq!((GROUPS, PATH_GROUP), (6, 2));
        let st = AppState::idle_for_tests();
        let (enabled, active, tip) = lsm_map_tip(&st);
        let key = menu::shortcut_text(MenuCmd::OpenFolder).unwrap();
        assert_eq!((enabled, active), (false, false));
        assert_eq!(tip, format!("LSM Map needs a RocksDB folder. Open Folder… ({key}), or open its CURRENT or MANIFEST file."), "advice §1.6 exact text");
        if cfg!(target_os = "macos") {
            assert_eq!(tip, "LSM Map needs a RocksDB folder. Open Folder… (Shift+⌘O), or open its CURRENT or MANIFEST file.");
        }
        assert_eq!(
            crate::app::lsm_map::with_open_folder_key(crate::app::lsm_map::NEED_FOLDER, "Ctrl+Shift+O"),
            "LSM Map needs a RocksDB folder. Open Folder… (Ctrl+Shift+O), or open its CURRENT or MANIFEST file."
        );
        assert_eq!(menu::MenuCmd::LsmMap.state(&st), (false, false));
    }

    #[test]
    fn disabled_zoom_in_tooltip_names_the_limit() {
        assert_eq!(zoom_limit_tip(1.1), "Largest zoom that fits this window (110 %). Enlarge the window to zoom further.");
    }

    /// Runs `frames` toolbar frames at `w` points (logical, zoom `z`) and returns the rightmost
    /// group edge minus the toolbar's right edge (> 0 = cut off).
    fn run(ctx: &egui::Context, st: &AppState, mem: &mut PanelMemory, theme: usize, w: f32, z: f32, frames: usize) -> f32 {
        let mut cache = Vec::new();
        let tok = apply_theme(ctx, &THEMES[theme], &mut cache);
        ctx.set_zoom_factor(z);
        let mut out = 0.0;
        for _ in 0..frames {
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w * z, 900.0 * z))), ..Default::default() };
            crate::ui::test_frame(ctx, input, |ui| {
                let frame = crate::ui::panels::panel_frame(egui::Color32::TRANSPARENT);
                egui::Panel::top("toolbar").frame(frame).show(ui, |ui| {
                    let right = ui.max_rect().right();
                    let (_, rects) = show_groups(ui, st, &tok, mem, None, false);
                    out = rects.iter().map(|r| r.right()).fold(f32::MIN, f32::max) - right;
                });
            });
        }
        out
    }

    /// 0015 C3 GUI-3: the zoom ceiling from the shell's own measurement (`measure_layout` over
    /// real toolbar widths measured by `show_groups`, Autistic theme `THEMES[0]`) at the README windows; and
    /// it is the same whether measured at 100 % or at the ceiling itself (no flip-flop).
    #[test]
    fn shell_zoom_ceilings_from_measured_toolbar() {
        use crate::app::{ZoomFit, ZoomPolicy};
        let st = AppState::idle_for_tests();
        let mut out = Vec::new();
        for (w, h) in [(1400.0f32, 900.0f32), (900.0, 700.0), (900.0, 560.0)] {
            let mut at = Vec::new();
            for z in [1.0f32, 1.1, 1.2, 1.6] {
                let ctx = egui::Context::default();
                let mut mem = PanelMemory::default();
                let _ = run(&ctx, &st, &mut mem, 0, w / z, z, 4);
                let tok = apply_theme(&ctx, &THEMES[0], &mut Vec::new());
                let mut c = 0.0;
                crate::ui::test_frame(&ctx, egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, h))), ..Default::default() }, |ui| {
                    // The status bar and a hex header line as the shell measures them.
                    let r = egui::Panel::bottom("status").frame(crate::ui::panels::status_frame(tok.p.toolbar, tok.p.border)).resizable(false).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.set_height(ui.text_style_height(&egui::TextStyle::Body) + LAYOUT.status_pad);
                            ui.label("selection");
                        });
                    });
                    mem.status_h = r.response.rect.height();
                    let line = ui.horizontal(|ui| {
                        let mut s = String::new();
                        ui.add(egui::TextEdit::singleline(&mut s).desired_width(LAYOUT.goto_width));
                        let _ = ui.button("Go to");
                        let _ = ui.small_button("Hide bytes");
                    });
                    mem.hex.line_h = line.response.rect.height() + ui.spacing().item_spacing.y;
                    let (nw, nh) = crate::ui::measure_layout(ui, &tok, &mem, 0, true).need(true, true);
                    c = ZoomPolicy::DEFAULT.ceiling(ZoomFit { at_zoom: z, avail_w: w / z, avail_h: h / z, need_w: nw, need_h: nh });
                    eprintln!("{w}x{h} measured at {:.0}%: need {nw:.1}x{nh:.1} → ceiling {:.0}%", z * 100.0, c * 100.0);
                    // 0024 D2b: the sixth group (LSM Map) never binds the ceiling — the widest
                    // group on a row of its own stays below the side-panel/8-byte width need.
                    let widest = mem.toolbar_w.iter().copied().fold(0.0, f32::max) + 2.0 * LAYOUT.center_margin[0] + PACK_SLACK;
                    assert!(widest < nw, "{w}x{h} at {z}: widest toolbar group {widest:.1} binds the width need {nw:.1}");
                });
                at.push((z, c));
            }
            // The ceiling seen at 100 % must still hold when measured at that ceiling.
            let c100 = at[0].1;
            if let Some(&(_, c_at)) = at.iter().find(|(z, _)| (*z - c100).abs() < 1e-3) {
                assert!((c_at - c100).abs() < 1e-3, "{w}x{h}: ceiling {c100} at 100 % but {c_at} at {c100}");
            }
            out.push(c100);
        }
        eprintln!("shell ceilings (1400x900, 900x700, 900x560): {out:?}");
        // Same as 0.1.3 (`rendered_layout_at_the_zoom_ceilings`): 160 % at 1400×900, the 100 %
        // floor at 900 pt (bound by side panels + 8 bytes per row since 0019 §3, not the toolbar).
        assert_eq!(out, vec![1.6, 1.0, 1.0], "ceilings unchanged by the LSM Map group");
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
