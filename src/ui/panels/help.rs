//! Help › RocksVision Help: short sections on the panels, opening, search, status meanings,
//! Reset View and the examples. Status meanings come from the shared legend; shortcuts are not
//! repeated here (Help › Keyboard Shortcuts renders the one table).

use super::{btn, status_meaning, strong, LEGEND};
use crate::app::{Action, AppState};
use crate::ui::layout::LAYOUT;
use crate::ui::menu::{panel_keys, shortcut_text, tip, KeyLabels, MenuCmd, RESET_VIEW_TIP};
use crate::ui::theme::Tokens;
use eframe::egui::{self, RichText};

const PANELS: &str = "Navigation (left): files and the structure tree. Records (center top): fields of the selected structure. Bytes (center bottom): the bytes behind your selection. Explanation (right): what the selection means and how it was decoded. Hide or show panels with the edge arrows, the View menu or";
const OPENING: &str = "Open Folder… reads a whole RocksDB directory (CURRENT, then MANIFEST, then SST/WAL), so names and levels come from the database. Open File… inspects one file alone; anything that needs the other files is shown as unknown, never guessed.";
const SEARCH: &str = "The search box finds text in the current table's decoded cells and in this file's stored bytes (not decompressed). Tick Hex bytes to search byte values such as 4d 41 4e. Enter: next match, Shift+Enter: previous.";
const LEVELS: &str = "LSM levels = which SST files sit at which level (MANIFEST); two-level index = index layout inside one SST.";
const EXAMPLES: &str = "The repository's examples/ folder has ready databases under data/ (use Open Folder) and single files under files/ (use Open File). examples/README.md says what each one shows.";
const KEYBOARD: &str = "All shortcuts: Help › Keyboard Shortcuts.";

pub fn show(ctx: &egui::Context, st: &AppState, tok: &Tokens) -> Vec<Action> {
    if !st.prefs().show_help() {
        return Vec::new();
    }
    let mut close = false;
    let resp = egui::Modal::new(egui::Id::new("help")).show(ctx, |ui| {
        ui.set_width(LAYOUT.help_width.min(ctx.content_rect().width() - 48.0));
        strong(ui, tok, "RocksVision Help");
        let max_h = (ctx.content_rect().height() - 140.0).max(120.0);
        egui::ScrollArea::vertical().max_height(max_h).show(ui, |ui| {
            let para = |ui: &mut egui::Ui, title: &str, text: &str| {
                ui.add_space(6.0);
                ui.label(RichText::new(title).strong().color(tok.p.heading));
                ui.add(egui::Label::new(text).wrap());
            };
            para(ui, "Panels", &format!("{PANELS} {}.", panel_keys(KeyLabels::HOST)));
            para(ui, "Open Folder or Open File", OPENING);
            para(ui, "Search", SEARCH);
            ui.add_space(6.0);
            ui.label(RichText::new("Status meanings").strong().color(tok.p.heading));
            for s in LEGEND {
                ui.add(egui::Label::new(RichText::new(format!("{} {}", s.symbol(), status_meaning(s))).color(tok.status(s))).wrap());
            }
            let zoom = format!(
                "{}\nZoom: {}, {}, {}.",
                tip(RESET_VIEW_TIP, MenuCmd::ResetView),
                shortcut_text(MenuCmd::ZoomIn).unwrap_or_default(),
                shortcut_text(MenuCmd::ZoomOut).unwrap_or_default(),
                shortcut_text(MenuCmd::ActualSize).unwrap_or_default()
            );
            para(ui, "Reset View", &zoom);
            para(ui, "Levels and indexes", LEVELS);
            para(ui, "Examples", EXAMPLES);
            para(ui, "Keyboard", KEYBOARD);
        });
        ui.add_space(8.0);
        ui.vertical_centered(|ui| {
            if btn(ui, tok, "Close").clicked() {
                close = true;
            }
        });
    });
    if close || resp.should_close() {
        vec![Action::CloseHelp]
    } else {
        Vec::new()
    }
}
