//! Status bar: exactly one line (spec §5). Segment order (0020 UI ruling F1): the one transient
//! notice (zoom, Reset View, copy), the file-changed warning, the selection, the job/search state
//! (with Cancel), "kind · file"; the hidden-panel hint is right-aligned. Widths come from
//! `layout::status_widths`: the notice and the warning keep their width while the bar holds them;
//! the hint is truncated (then dropped) first, then the search segment, the selection and the
//! file label are elided in that order; every elided segment shows its full text on hover.

use crate::app::{Action, AppState};
use crate::model::Status;
use crate::ui::layout::{LAYOUT, StatusWidths, nb_units, status_widths};
use crate::ui::theme::Tokens;
use eframe::egui::{self, RichText};

/// Id of the planned and wanted widths of the notice and search segments of the last frame
/// (`[notice, notice wanted, search, search wanted]`, tests).
pub fn notice_width_id() -> egui::Id {
    egui::Id::new("status_notice_widths")
}

/// `hidden`: (name, how to show it) of panels the user collapsed.
pub fn show(ui: &mut egui::Ui, st: &AppState, tok: &Tokens, hidden: &[(&str, String)]) -> Vec<Action> {
    let mut acts = Vec::new();
    tok.paint_chrome(ui, ui.clip_rect());
    let doc = st.session();
    let find = st.search();
    // Mirror of the Explanation "Where" (one classifier, `app::explanation`); count and unit
    // joined for the screen only (ruling 6).
    let sel = nb_units(&crate::app::explanation::selection_status(doc, st.selection()));
    let notice = nb_units(st.prefs().notice());
    let warn = if doc.file().is_some() && doc.changed() { "⚠ file changed on disk since open — reopen to refresh".to_string() } else { String::new() };
    // Same state as the banner above the table (`SearchState::status_segment`, 0019 §4).
    let search = find.status_segment(st.row_match_position()).unwrap_or_default();
    let file = match doc.file() {
        Some(p) => kind_and_name(doc.file_label(), &p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
        None => doc.status_msg().to_string(),
    };
    let (hint, hint_full) = match hidden {
        [] => (String::new(), String::new()),
        [(name, how)] => (format!("{name} hidden"), how.clone()),
        many => (format!("{} hidden", many.iter().map(|x| x.0).collect::<Vec<_>>().join(", ")), many.iter().map(|x| x.1.clone()).collect::<Vec<_>>().join("\n")),
    };
    ui.horizontal(|ui| {
        let h = ui.text_style_height(&egui::TextStyle::Body) + LAYOUT.status_pad;
        ui.set_height(h);
        let body = egui::TextStyle::Body.resolve(ui.style());
        let w = |t: &str| if t.is_empty() { 0.0 } else { ui.fonts_mut(|f| f.layout_no_wrap(t.to_string(), body.clone(), tok.p.text).size().x) + 2.0 };
        let gap = ui.spacing().item_spacing.x;
        let cw = ui.fonts_mut(|f| f.glyph_width(&body, '0'));
        let busy = find.progress().is_some() || st.busy_label().is_some();
        // Fixed parts: separators and the busy widgets (spinner/progress + Cancel).
        let busy_w = if find.progress().is_some() {
            LAYOUT.progress_width + 70.0 + w("Searching file bytes")
        } else if let Some(b) = st.busy_label() {
            h + 70.0 + w(b)
        } else {
            0.0
        };
        // Fixed parts: busy widgets, item gaps and up to four "·" separators.
        let fixed = busy_w + 10.0 * gap + 4.0 * w("·");
        let want = StatusWidths { notice: w(&notice), warn: w(&warn), sel: w(&sel), search: w(&search), file: w(&file), hint: w(&hint) };
        let plan = status_widths(ui.available_width() - fixed, want, LAYOUT.status_min_glyphs * cw);
        ui.ctx().data_mut(|d| d.insert_temp(notice_width_id(), [plan.notice, want.notice, plan.search, want.search]));
        let seg = |ui: &mut egui::Ui, text: &str, width: f32, warn: bool| {
            if width <= 0.0 || text.is_empty() {
                return false;
            }
            ui.allocate_ui_with_layout(egui::vec2(width, h), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                // Status colour only on a flat data fill, never directly on chrome.
                let t = if warn { RichText::new(text).color(tok.status(Status::Unsupported)).background_color(tok.p.data) } else { RichText::new(text).color(tok.p.text) };
                ui.add(egui::Label::new(t).truncate()).on_hover_text(text);
            });
            true
        };
        let dot = |ui: &mut egui::Ui| {
            ui.label(RichText::new("·").color(tok.p.text_muted));
        };
        // Left of the selection: notice, then the warning, each followed by a separator.
        if seg(ui, &notice, plan.notice, false) {
            dot(ui);
        }
        if seg(ui, &warn, plan.warn, true) {
            dot(ui);
        }
        if seg(ui, &sel, plan.sel, false) {
            dot(ui);
        }
        if let Some((d, t)) = find.progress() {
            ui.label("Searching file bytes");
            ui.add(
                egui::ProgressBar::new(if t > 0 { d as f32 / t as f32 } else { 0.0 })
                    .desired_width(LAYOUT.progress_width)
                    .desired_height(ui.text_style_height(&egui::TextStyle::Small))
                    .show_percentage(),
            );
            if ui.small_button("Cancel").clicked() {
                acts.push(Action::CancelSearch);
            }
        } else if let Some(what) = st.busy_label() {
            ui.add(egui::Spinner::new().size(ui.text_style_height(&egui::TextStyle::Body)));
            ui.label(what);
            if ui.small_button("Cancel").on_hover_text("Cancel decoding, folder scan, byte reads and copies (Esc)").clicked() {
                acts.push(Action::CancelJobs);
            }
        }
        let shown_search = seg(ui, &search, plan.search, false);
        if (shown_search || busy) && plan.file > 0.0 {
            dot(ui);
        }
        seg(ui, &file, plan.file, false);
        if plan.hint > 0.0 {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.allocate_ui_with_layout(egui::vec2(plan.hint, h), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(egui::Label::new(RichText::new(&hint).color(tok.p.text_muted)).truncate()).on_hover_text(&hint_full);
                });
            });
        }
    });
    acts
}

/// "kind · file name"; the name alone when it equals the kind (e.g. "CURRENT"). Only exact
/// equality collapses ("MANIFEST · MANIFEST-000005" stays).
fn kind_and_name(kind: &str, name: &str) -> String {
    if kind == name || name.is_empty() { kind.to_string() } else { format!("{kind} · {name}") }
}

#[cfg(test)]
mod tests {
    use super::kind_and_name;

    #[test]
    fn kind_equal_to_name_is_printed_once() {
        assert_eq!(kind_and_name("CURRENT", "CURRENT"), "CURRENT");
        assert_eq!(kind_and_name("IDENTITY", "IDENTITY"), "IDENTITY");
        assert_eq!(kind_and_name("MANIFEST", "MANIFEST-000005"), "MANIFEST · MANIFEST-000005");
        assert_eq!(kind_and_name("SST", "000012.sst"), "SST · 000012.sst");
    }
}
