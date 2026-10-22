//! About modal: logo, name/version/year and author from `crate::branding`.

use super::btn;
use crate::app::{Action, AppState};
use crate::branding;
use crate::ui::layout::LAYOUT;
use crate::ui::theme::Tokens;
use eframe::egui::{self, RichText};

pub fn show(ctx: &egui::Context, st: &AppState, tok: &Tokens, logo: Option<&egui::TextureHandle>) -> Vec<Action> {
    if !st.prefs().show_about() {
        return Vec::new();
    }
    let mut close = false;
    let resp = egui::Modal::new(egui::Id::new("about")).show(ctx, |ui| {
        ui.set_width(LAYOUT.about_width);
        ui.vertical_centered(|ui| {
            if let Some(t) = logo {
                ui.add(egui::Image::new(t).fit_to_exact_size(egui::vec2(LAYOUT.about_logo, LAYOUT.about_logo)));
            }
            ui.add_space(8.0);
            ui.heading(branding::about_title());
            ui.label(RichText::new(branding::about_byline()).color(tok.p.text_muted));
            ui.add_space(10.0);
            if btn(ui, tok, "Close").clicked() {
                close = true;
            }
        });
    });
    if close || resp.should_close() {
        vec![Action::CloseAbout]
    } else {
        Vec::new()
    }
}
