//! Explanation panel: a Diagnosis of Invalid/Truncated elements (from the typed `Diagnostic`,
//! spec §12), where/encoding/value/raw/status/derivation of the selected field, a node summary
//! when no row is selected, the byte interpretations of the selection, or the status legend.
//! Unsupported and Unknown are stated separately and never under Diagnosis.

use super::{btn, chip, chip_word, long_value, status_meaning, strong, title_line, LEGEND};
use crate::app::diagnosis::{self, DiagLine, Diagnosis, Finding, NodeFailure, SUMMARY_LINES};
use crate::app::hex::interpretations;
use crate::app::{Action, AppState, Panel};
use crate::limits::LIMITS;
use crate::model::{Span, Status};
use crate::ui::layout;
use crate::ui::menu::{self, MenuCmd};
use crate::ui::theme::Tokens;
use eframe::egui::{self, RichText};

pub fn show(ui: &mut egui::Ui, st: &AppState, tok: &Tokens) -> Vec<Action> {
    let mut acts = Vec::new();
    if title_line(ui, tok, "Explanation", "▶", &menu::tip("Hide Explanation", MenuCmd::TogglePanel(Panel::Explanation))) {
        acts.push(Action::TogglePanel(Panel::Explanation));
    }
    // Prose wraps to the panel width and scrolls vertically; only unwrappable values and
    // interpretation tokens scroll horizontally, in their own row.
    egui::ScrollArea::vertical().id_salt("explain").auto_shrink([false, false]).show(ui, |ui| {
        ui.set_max_width(ui.available_width());
        if let Some(f) = st.selected_field() {
            field(ui, st, tok, f, &mut acts);
        } else if let Some(s) = st.selection().span() {
            if let Some(n) = st.selection().raw_note() {
                ui.add(egui::Label::new(n).wrap());
            }
            ui.add(egui::Label::new(format!("Raw selection: {s}")).wrap());
            interp(ui, st, tok, &s);
        } else if !node(ui, st, tok, &mut acts) {
            empty(ui, tok);
        }
    });
    acts
}

fn section(ui: &mut egui::Ui, tok: &Tokens, title: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(title).strong().color(tok.p.heading));
}

fn field(ui: &mut egui::Ui, st: &AppState, tok: &Tokens, f: &crate::model::Field, acts: &mut Vec<Action>) {
    ui.horizontal_wrapped(|ui| {
        if f.untrusted {
            chip_word(ui, tok, f.status, diagnosis::UNTRUSTED_WORD, diagnosis::UNTRUSTED_MEANING);
        } else {
            chip(ui, tok, f.status);
        }
        ui.heading(&f.name);
    });
    ui.add_space(4.0);
    // Diagnosis (or the separate Unsupported / Unknown statement) directly under the title.
    if let Some(fd) = diagnosis::finding(f.status, f.diag.as_deref(), &f.name) {
        finding(ui, tok, &fd, "field", acts);
    }
    section(ui, tok, "Where");
    match f.span {
        Some(s) => ui.add(egui::Label::new(format!("{}\n0x{:x}..0x{:x} ({} bytes)", s.space.label(), s.offset, s.offset + s.len, s.len)).wrap()),
        None => ui.add(egui::Label::new("No on-disk bytes (derived, inferred or default value)").wrap()),
    };
    if !f.encoding.is_empty() {
        section(ui, tok, "Encoding");
        ui.add(egui::Label::new(&f.encoding).wrap());
    }
    section(ui, tok, "Value");
    long_value(ui, "v_value", &f.value);
    // Typed decoded value only; the display string is never parsed back.
    if let Some(n) = f.decoded.as_ref().and_then(|d| d.as_u64()) {
        section(ui, tok, "Hex / binary");
        long_value(ui, "v_bin", &format!("0x{n:x}\n0b{n:b}"));
    }
    if !f.raw.is_empty() {
        section(ui, tok, "Raw");
        long_value(ui, "v_raw", &f.raw);
    }
    section(ui, tok, "Status");
    let meaning = if f.untrusted { diagnosis::UNTRUSTED_MEANING } else { status_meaning(f.status) };
    ui.add(egui::Label::new(format!("{} {meaning}", f.status.symbol())).wrap());
    if !f.explain.is_empty() {
        section(ui, tok, "Derivation");
        ui.add(egui::Label::new(&f.explain).wrap());
    }
    if f.link.is_some() || f.open_path.is_some() {
        ui.add_space(6.0);
        let label = if f.open_path.is_some() { "Open referenced file ›" } else { "Go to target ›" };
        if btn(ui, tok, label).clicked() {
            acts.push(Action::FollowSelected);
        }
    }
    if let Some(s) = f.span {
        ui.add_space(6.0);
        ui.separator();
        section(ui, tok, "Interpretations of the first bytes");
        interp(ui, st, tok, &s);
    }
}

/// Node-level explanation when no row is selected: title, status and a summary of the
/// failures (≤ 5 lines with links, or the full grid when there is exactly one). Returns false
/// when there is nothing node-specific to say.
fn node(ui: &mut egui::Ui, st: &AppState, tok: &Tokens, acts: &mut Vec<Action>) -> bool {
    let doc = st.session();
    let Some(view) = doc.view() else { return false };
    let tree = doc.sel_node().and_then(|i| doc.nodes().get(i));
    let status = view.status.or(tree.map(|n| n.status));
    let fails = diagnosis::node_failures(view, tree.map(|n| n.status), tree.and_then(|n| n.diag.as_deref()));
    let other = status.filter(|s| matches!(s, Status::Unsupported | Status::Unknown | Status::Limited));
    if fails.is_empty() && other.is_none() {
        return false;
    }
    ui.horizontal_wrapped(|ui| {
        if let Some(s) = status {
            chip(ui, tok, s);
        }
        ui.add(egui::Label::new(RichText::new(&view.title).heading()).wrap());
    });
    ui.add_space(4.0);
    if let [one] = fails.as_slice() {
        let d = diagnosis::diagnosis(one.status, one.diag.as_ref());
        diagnosis_ui(ui, tok, &d, "node", acts);
        if let Some(r) = one.row {
            if ui.link(format!("Select the failing field: {}", one.name)).clicked() {
                acts.push(Action::SelectRow(r));
            }
        }
    } else if !fails.is_empty() {
        summary(ui, tok, &fails, acts);
    }
    if let Some(s) = other {
        let d = diagnosis::statement_diag(s, tree.and_then(|n| n.diag.as_deref()).into_iter().chain(view.diagnostics.iter()));
        if let Some(fd) = diagnosis::finding(s, d, "this structure") {
            finding(ui, tok, &fd, "node_other", acts);
        }
    }
    ui.add_space(6.0);
    ui.add(egui::Label::new(RichText::new("Select a row to see its full explanation.").color(tok.p.text_muted)).wrap());
    true
}

fn summary(ui: &mut egui::Ui, tok: &Tokens, fails: &[NodeFailure], acts: &mut Vec<Action>) {
    let worst = if fails.iter().any(|f| f.status == Status::Invalid) { Status::Invalid } else { Status::Truncated };
    ui.label(RichText::new(format!("{} Diagnosis", worst.symbol())).strong().color(tok.status(worst)));
    for f in fails.iter().take(SUMMARY_LINES) {
        let text = f.summary();
        match (f.row, f.diag.as_ref().and_then(|d| d.span)) {
            (Some(r), _) => {
                if ui.link(text).clicked() {
                    acts.push(Action::SelectRow(r));
                }
            }
            (None, Some(s)) => {
                if ui.link(text).clicked() {
                    acts.push(Action::SelectSpan(s));
                }
            }
            (None, None) => {
                ui.add(egui::Label::new(text).wrap());
            }
        }
    }
    if fails.len() > SUMMARY_LINES {
        ui.label(RichText::new(format!("+{} more", fails.len() - SUMMARY_LINES)).color(tok.p.text_muted));
    }
}

fn finding(ui: &mut egui::Ui, tok: &Tokens, f: &Finding, id: &str, acts: &mut Vec<Action>) {
    match f {
        Finding::Diagnosis(d) => diagnosis_ui(ui, tok, d, id, acts),
        Finding::Unsupported(t) => statement(ui, tok, Status::Unsupported, "Unsupported", t),
        Finding::Unknown(t) => statement(ui, tok, Status::Unknown, "Unknown", t),
        Finding::Limited(t) => statement(ui, tok, Status::Limited, "Resource limit", t),
    }
}

fn statement(ui: &mut egui::Ui, tok: &Tokens, s: Status, word: &str, text: &str) {
    ui.add(egui::Label::new(RichText::new(format!("{} {word}: {text}", s.symbol())).color(tok.status(s))).wrap());
    ui.add_space(4.0);
}

/// "✖ Diagnosis" / "✂ Diagnosis" heading and the label/value lines; the Bytes line is a link
/// that selects the span in its own offset space. Values (hex, numbers, enum names, spans) are
/// single unbroken monospace labels; prose wraps at word boundaries only. Below
/// `layout::DIAG_SIDE_MIN`, or when the widest value does not fit beside the labels, each label
/// sits on its own line above its full-width value. A token wider than the panel scrolls
/// horizontally in its own row; no token is ever split.
fn diagnosis_ui(ui: &mut egui::Ui, tok: &Tokens, d: &Diagnosis, id: &str, acts: &mut Vec<Action>) {
    ui.label(RichText::new(format!("{} Diagnosis", d.status.symbol())).strong().color(tok.status(d.status)));
    let body = egui::TextStyle::Body.resolve(ui.style());
    let mono = egui::TextStyle::Monospace.resolve(ui.style());
    let w = |ui: &egui::Ui, s: &str, f: &egui::FontId| ui.fonts_mut(|x| x.layout_no_wrap(s.to_string(), f.clone(), tok.p.text).size().x);
    let label_w = d.lines.iter().map(|l| w(ui, l.label, &body)).fold(0.0, f32::max) + 8.0;
    // Unbreakable width of each line: the whole value, or the longest word of prose.
    let need: Vec<f32> = d.lines.iter().map(|l| if l.prose { l.text.split_whitespace().map(|t| w(ui, t, &body)).fold(0.0, f32::max) } else { w(ui, &l.text, &mono) }).collect();
    let widest = need.iter().copied().fold(0.0, f32::max);
    let side = layout::diag_side_by_side(ui.available_width(), label_w, widest);
    for (i, l) in d.lines.iter().enumerate() {
        ui.push_id((id, i), |ui| {
            let label = RichText::new(l.label).color(tok.p.text_muted);
            if side {
                ui.horizontal_top(|ui| {
                    ui.add_sized([label_w, ui.text_style_height(&egui::TextStyle::Body)], egui::Label::new(label));
                    diag_value(ui, tok, l, need[i], acts);
                });
            } else {
                ui.add(egui::Label::new(label).extend());
                diag_value(ui, tok, l, need[i], acts);
                ui.add_space(2.0);
            }
        });
    }
    ui.add_space(4.0);
}

/// One Diagnosis value in the remaining width (see `diagnosis_ui`); `need` is its unbreakable width.
fn diag_value(ui: &mut egui::Ui, tok: &Tokens, l: &DiagLine, need: f32, acts: &mut Vec<Action>) {
    let draw = |ui: &mut egui::Ui| {
        let mut t = if l.prose { RichText::new(&l.text) } else { RichText::new(&l.text).monospace() };
        if l.link.is_some() {
            t = t.color(tok.p.accent_text).underline();
        }
        let mut lab = if l.prose { egui::Label::new(t).wrap() } else { egui::Label::new(t).extend() };
        if l.link.is_some() {
            lab = lab.sense(egui::Sense::click());
        }
        ui.add(lab)
    };
    let r = if need > ui.available_width() {
        egui::ScrollArea::horizontal()
            .id_salt("value")
            .auto_shrink([false, true])
            .show(ui, |ui| {
                // Prose wraps at its longest word, never inside it.
                ui.set_max_width(need);
                draw(ui)
            })
            .inner
    } else {
        draw(ui)
    };
    if let Some(span) = l.link {
        if r.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Select these bytes in their own offset space").clicked() {
            acts.push(Action::SelectSpan(span));
        }
    }
}

fn empty(ui: &mut egui::Ui, tok: &Tokens) {
    ui.add(egui::Label::new(RichText::new("Nothing selected.").color(tok.p.text_muted)).wrap());
    ui.add(egui::Label::new("Select a row or a byte to see where it lives, how it is encoded and how it was derived.").wrap());
    ui.add_space(6.0);
    strong(ui, tok, "Status legend");
    for st in LEGEND {
        ui.add(egui::Label::new(RichText::new(format!("{} {}", st.symbol(), status_meaning(st))).color(tok.status(st))).wrap());
    }
    ui.add_space(6.0);
    strong(ui, tok, "Hints");
    for h in [
        "• Click a table row or a hex byte; double-click / Enter follows a handle or link.",
        "• Shift+click in the byte view extends the byte selection.",
        "• Help › RocksVision Help explains the panels, search and Reset View.",
        "• Help › Keyboard Shortcuts lists every shortcut.",
    ] {
        ui.add(egui::Label::new(h).wrap());
    }
}

/// Interpretations: every token is one unbroken label; rows wrap only at the " / " separators.
/// A token wider than the panel puts its row into a horizontal scroll area of its own.
fn interp(ui: &mut egui::Ui, st: &AppState, tok: &Tokens, s: &Span) {
    let sp = Span { len: s.len.clamp(1, LIMITS.interp_bytes), ..*s };
    let Some(b) = st.session().hex().bytes_in(&sp) else {
        ui.label(RichText::new("(bytes not in the loaded hex page)").color(tok.p.text_muted));
        return;
    };
    let mono = egui::TextStyle::Monospace.resolve(ui.style());
    for (i, row) in interpretations(&b).into_iter().enumerate() {
        let widest = row.tokens.iter().map(|t| ui.fonts_mut(|f| f.layout_no_wrap(t.clone(), mono.clone(), tok.p.text).size().x)).fold(0.0, f32::max);
        let draw = |ui: &mut egui::Ui| {
            ui.label(RichText::new(format!("{}  ", row.label)).color(tok.p.text_muted));
            for (j, t) in row.tokens.iter().enumerate() {
                if j > 0 {
                    ui.add(egui::Label::new(RichText::new(" / ").monospace().color(tok.p.text_muted)).extend());
                }
                ui.add(egui::Label::new(RichText::new(t).monospace().color(tok.p.text)).extend());
            }
        };
        if widest > ui.available_width() * 0.8 {
            egui::ScrollArea::horizontal().id_salt(("interp", i)).auto_shrink([false, true]).show(ui, |ui| ui.horizontal(draw));
        } else {
            ui.horizontal_wrapped(draw);
        }
    }
}
