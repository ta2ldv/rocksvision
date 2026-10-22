//! Help › Keyboard Shortcuts: one modal rendering `menu::shortcut_table()` (the single source
//! that tooltips and Help also use).

use super::{btn, strong};
use crate::app::{Action, AppState};
use crate::ui::layout::LAYOUT;
use crate::ui::menu::shortcut_table;
use crate::ui::theme::Tokens;
use eframe::egui::{self, RichText};

pub fn show(ctx: &egui::Context, st: &AppState, tok: &Tokens) -> Vec<Action> {
    if !st.prefs().show_shortcuts() {
        return Vec::new();
    }
    let mut close = false;
    let resp = egui::Modal::new(egui::Id::new("shortcuts")).show(ctx, |ui| {
        ui.set_width(LAYOUT.about_width * 1.4);
        strong(ui, tok, "Keyboard Shortcuts");
        ui.add_space(6.0);
        egui::Grid::new("shortcut_grid").num_columns(2).striped(true).show(ui, |ui| {
            for (keys, what) in shortcut_table() {
                ui.label(RichText::new(keys).monospace());
                ui.label(what);
                ui.end_row();
            }
        });
        ui.add_space(8.0);
        ui.vertical_centered(|ui| {
            if btn(ui, tok, "Close").clicked() {
                close = true;
            }
        });
    });
    if close || resp.should_close() {
        vec![Action::CloseShortcuts]
    } else {
        Vec::new()
    }
}
