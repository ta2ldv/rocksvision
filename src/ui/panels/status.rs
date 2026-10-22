//! Status bar: exactly one line (spec §5). From the left: the selection (never truncated), the
//! message/job/search state (with Cancel), the file label; the hidden-panel hint is right-aligned.
//! Widths come from `layout::status_widths`: the hint is truncated (then dropped) first, then
//! the message, then the file label; every truncated segment shows its full text on hover.

use crate::app::{Action, AppState};
use crate::model::Status;
use crate::ui::layout::{status_widths, LAYOUT};
use crate::ui::theme::Tokens;
use eframe::egui::{self, RichText};

/// `hidden`: (name, how to show it) of panels the user collapsed.
pub fn show(ui: &mut egui::Ui, st: &AppState, tok: &Tokens, hidden: &[(&str, String)]) -> Vec<Action> {
    let mut acts = Vec::new();
    tok.paint_chrome(ui, ui.clip_rect());
    let doc = st.session();
    let find = st.search();
    let sel = st.selection().span().map(|s| format!("selection: {s}")).unwrap_or_default();
    let mut msg: Vec<String> = Vec::new();
    if doc.file().is_some() && doc.changed() {
        msg.push("⚠ file changed on disk since open — reopen to refresh".into());
    }
    if !st.jobs().copy_note().is_empty() {
        msg.push(st.jobs().copy_note().to_string());
    }
    if !find.query().is_empty() && !find.row_matches().is_empty() {
        let cur = st.row_match_position().map(|i| i.to_string()).unwrap_or("–".into());
        msg.push(format!("Rows {cur} / {} (current table)", find.row_matches().len()));
    } else if !find.status().is_empty() {
        msg.push(find.status().to_string());
    }
    if !st.prefs().notice().is_empty() {
        msg.push(st.prefs().notice().to_string());
    }
    let msg = msg.join("  ·  ");
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
        let fixed = busy_w + 6.0 * gap + 2.0 * w("·");
        let (mw, fw, hw) = status_widths(ui.available_width() - fixed, w(&sel), w(&msg), w(&file), w(&hint), LAYOUT.status_min_glyphs * cw);
        let seg = |ui: &mut egui::Ui, text: &str, width: f32, warn: bool| {
            if width <= 0.0 || text.is_empty() {
                return;
            }
            ui.allocate_ui_with_layout(egui::vec2(width, h), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                // Status colour only on a flat data fill, never directly on chrome.
                let t = if warn { RichText::new(text).color(tok.status(Status::Unsupported)).background_color(tok.p.data) } else { RichText::new(text).color(tok.p.text) };
                ui.add(egui::Label::new(t).truncate()).on_hover_text(text);
            });
        };
        if !sel.is_empty() {
            ui.add(egui::Label::new(RichText::new(&sel).color(tok.p.text)).extend());
            ui.label(RichText::new("·").color(tok.p.text_muted));
        }
        if let Some((d, t)) = find.progress() {
            ui.label("Searching file bytes");
            ui.add(egui::ProgressBar::new(if t > 0 { d as f32 / t as f32 } else { 0.0 }).desired_width(LAYOUT.progress_width).desired_height(ui.text_style_height(&egui::TextStyle::Small)).show_percentage());
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
        let warn = doc.file().is_some() && doc.changed();
        seg(ui, &msg, mw, warn);
        if (!msg.is_empty() || busy) && fw > 0.0 {
            ui.label(RichText::new("·").color(tok.p.text_muted));
        }
        seg(ui, &file, fw, false);
        if hw > 0.0 {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.allocate_ui_with_layout(egui::vec2(hw, h), egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
    if kind == name || name.is_empty() {
        kind.to_string()
    } else {
        format!("{kind} · {name}")
    }
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
