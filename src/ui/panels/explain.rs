//! Explanation panel: a Diagnosis of Invalid/Truncated elements (from the typed `Diagnostic`,
//! spec §12), where/encoding/value/raw/status/derivation of the selected field, a node summary
//! when no row is selected, the byte interpretations of the selection, or the status legend.
//! Unsupported and Unknown are stated separately and never under Diagnosis.

use super::{LEGEND, TipExt, btn, chip, chip_word, fit_value, long_value, status_meaning, strong, title_line, value_tip};
use crate::app::diagnosis::{self, DiagLine, Diagnosis, Finding, NodeFailure, SUMMARY_LINES};
use crate::app::explanation::{self, Failures, Subject, WhereText};
use crate::app::hex::interpretations;
use crate::app::map_facts::{MapFact, ValueKind};
use crate::app::{Action, AppState, Panel};
use crate::limits::LIMITS;
use crate::model::{NodeKind, Span, Status, WhereLine};
use crate::ui::layout::{self, nb_units};
use crate::ui::menu::{self, MenuCmd};
use crate::ui::theme::Tokens;
use eframe::egui::{self, RichText};

/// Renders whatever the explanation model (`app::explanation`) says about the selection; this
/// file keeps only layout, widgets, the Diagnosis width measurement and the actions.
pub fn show(ui: &mut egui::Ui, st: &AppState, tok: &Tokens) -> Vec<Action> {
    let mut acts = Vec::new();
    if title_line(ui, tok, "Explanation", "▶", &menu::tip("Hide Explanation", MenuCmd::TogglePanel(Panel::Explanation))) {
        acts.push(Action::TogglePanel(Panel::Explanation));
    }
    // Everything wraps to the panel width and scrolls vertically only: prose at words, values
    // at the designed break points (0015 C1–C3, `layout::wrap_value`).
    egui::ScrollArea::vertical().id_salt("explain").auto_shrink([false, false]).show(ui, |ui| {
        ui.set_max_width(ui.available_width());
        match explanation::subject(st.session(), st.selection()) {
            Subject::Field { row, field: f } => field(ui, st, tok, row, f, &mut acts),
            Subject::WholeStructure => {
                structure_card(ui, st, tok, &mut acts);
                levels_ui(ui, st, tok);
            }
            Subject::StructureRange { span: s, .. } | Subject::Raw(s) => selection_range(ui, st, tok, &s, &mut acts),
            Subject::Node | Subject::PendingStructure | Subject::Empty => {
                if let Some(u) = explanation::lsm_map_unavailable(st.session()) {
                    ui.add(egui::Label::new(RichText::new(u).color(tok.p.text_muted)).wrap());
                    ui.add_space(4.0);
                }
                if let Some(c) = explanation::map_card(st.session()) {
                    map_card_ui(ui, tok, &c, &mut acts);
                } else if !node(ui, st, tok, &mut acts) {
                    if explanation::levels_summary(st.session()).is_empty() {
                        empty(ui, tok);
                    } else {
                        levels_ui(ui, st, tok);
                    }
                }
            }
        }
    });
    acts
}

fn section(ui: &mut egui::Ui, tok: &Tokens, title: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(title).strong().color(tok.p.heading));
}

/// "Where" heading and its text (range text with the count and unit joined, ruling 6).
fn where_ui(ui: &mut egui::Ui, tok: &Tokens, w: &WhereText) {
    section(ui, tok, "Where");
    match w {
        WhereText::Lines(lines) => {
            for l in lines {
                ui.add(egui::Label::new(RichText::new(nb_units(l)).monospace()).wrap());
            }
        }
        WhereText::Prose(text) => {
            ui.add(egui::Label::new(nb_units(text)).wrap());
        }
        WhereText::Location => {}
    }
}

/// The structure's location lines; linked lines navigate (Back returns).
fn location_ui(ui: &mut egui::Ui, location: &[WhereLine], acts: &mut Vec<Action>) {
    for w in location {
        match &w.link {
            Some(t) => {
                if ui.link(nb_units(&w.text)).tip("Go to this structure (Back returns)").clicked() {
                    acts.push(Action::Navigate { target: t.node.clone(), span: t.span });
                }
            }
            None => {
                ui.add(egui::Label::new(RichText::new(nb_units(&w.text)).monospace()).wrap());
            }
        }
    }
}

/// One failure (its Diagnosis grid and the link to the failing row) or a summary list.
fn failures_ui(ui: &mut egui::Ui, tok: &Tokens, f: &Failures, id: &str, acts: &mut Vec<Action>) {
    match f {
        Failures::None => {}
        Failures::One { diag, select } => {
            diagnosis_ui(ui, tok, diag, id, acts);
            if let Some((r, name)) = select
                && ui.link(format!("Select the failing field: {name}")).clicked()
            {
                acts.push(Action::SelectRow(*r));
            }
        }
        Failures::Many(fails) => summary(ui, tok, fails, acts),
    }
}

/// Secondary, collapsed by default (its open state is kept for the session): the alternate
/// notations of the decoded number ("0x7 / 0b111"), the presenter's extra rows (e.g. internal
/// and user key bytes, design D4) and the byte interpretations of the span.
fn other_interpretations(ui: &mut egui::Ui, st: &AppState, tok: &Tokens, s: Option<&Span>, number: Option<u64>, extra: &[(&str, String)], open: bool) {
    if s.is_none() && number.is_none() && extra.is_empty() {
        return;
    }
    ui.add_space(6.0);
    // egui's CollapsingHeader never wraps its title: in a squeezed panel at 160 % it was wider
    // than the panel, the panel reported a rect shifted right by the overflow and the center
    // painted over the left of every Explanation line (0020 D1, P3-F2). Same state id and
    // default; the title wraps and toggles like the arrow.
    let id = ui.make_persistent_id(("other_interp", open));
    let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, open);
    let header = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        state.show_toggle_button(ui, egui::collapsing_header::paint_default_icon);
        ui.add(egui::Label::new(RichText::new("Other byte interpretations").color(tok.p.heading)).wrap().sense(egui::Sense::click())).clicked()
    });
    if header.inner {
        state.toggle(ui);
    }
    state.show_body_indented(&header.response, ui, |ui| {
        if let Some(n) = number {
            interp_row(ui, tok, "decoded value", &format!("{n} / 0x{n:x} / 0b{n:b}"));
        }
        for (label, v) in extra {
            interp_row(ui, tok, label, v);
        }
        if let Some(s) = s {
            interp(ui, st, tok, s);
        }
    });
}

/// One interpretation row: muted label, then the value wrapped in the remaining width (no
/// horizontal scrolling, design C1).
fn interp_row(ui: &mut egui::Ui, tok: &Tokens, label: &str, v: &str) {
    ui.horizontal_top(|ui| {
        ui.label(RichText::new(format!("{label}  ")).color(tok.p.text_muted));
        long_value(ui, tok, v);
    });
}

/// The whole structure selected (tree click, structure step, navigation without a target
/// row): title + status chip, Meaning, Where (file range, contents/trailer), Diagnosis, the
/// "Referenced from" back-link and, for compressed contents, the decompressed view.
fn structure_card(ui: &mut egui::Ui, st: &AppState, tok: &Tokens, acts: &mut Vec<Action>) {
    let Some(c) = explanation::structure_card(st.session(), st.selection()) else { return };
    ui.horizontal_wrapped(|ui| {
        if let Some(s) = c.status {
            match c.status_word {
                Some(w) => chip_word(ui, tok, s, w, status_meaning(s)),
                None => chip(ui, tok, s),
            }
        }
        ui.add(egui::Label::new(RichText::new(&c.title).heading()).wrap());
    });
    ui.add_space(4.0);
    // 0030 B-8: no empty heading.
    if c.meaning.is_some() || c.subtitle.is_some() || c.note.is_some() {
        section(ui, tok, "Meaning");
    }
    if let Some(t) = c.meaning {
        ui.add(egui::Label::new(t).wrap());
    }
    if let Some(t) = c.subtitle {
        ui.add(egui::Label::new(t).wrap());
    }
    if let Some(n) = c.note {
        ui.add(egui::Label::new(RichText::new(nb_units(n)).color(tok.p.text_muted)).wrap());
    }
    // Where: the structure's location lines (their first line is its range, 0020 §1); without
    // bytes one muted line instead of a heading (0030 B-8).
    match &c.where_ {
        WhereText::Prose(t) if c.location.is_empty() => {
            ui.add(egui::Label::new(RichText::new(nb_units(t)).color(tok.p.text_muted)).wrap());
        }
        w => {
            where_ui(ui, tok, w);
            location_ui(ui, c.location, acts);
        }
    }
    // Diagnosis of the structure (same data as the node summary).
    if !c.failures.is_none() {
        ui.add_space(4.0);
        failures_ui(ui, tok, &c.failures, "card", acts);
    }
    table_legend(ui, tok, &c.legend);
    // Offered only while Back can go there (0024 §4: never a dead back-link).
    if let Some(via) = c.via.filter(|_| st.navigation().can_go_back()) {
        ui.add_space(6.0);
        if ui.link(format!("Referenced from: {via} ›")).tip("Back to where this link was followed (restores that selection)").clicked() {
            acts.push(Action::Back);
        }
    }
    if let Some(sp) = c.decompressed {
        ui.add_space(6.0);
        if btn(ui, tok, "Open decompressed view").tip(&sp.title).clicked() {
            acts.push(Action::HexShowSpace(sp.space));
        }
    }
}

/// A structure or raw byte range (no single field): what it is, where, and for compressed
/// stored bytes the way to the decompressed fields.
fn selection_range(ui: &mut egui::Ui, st: &AppState, tok: &Tokens, s: &Span, acts: &mut Vec<Action>) {
    let c = explanation::range_card(st.session(), st.selection(), *s);
    ui.horizontal_wrapped(|ui| {
        ui.heading(if c.structure { "Structure bytes" } else { "Raw range" });
    });
    if let Some(n) = c.note {
        ui.add(egui::Label::new(nb_units(n)).wrap());
    }
    where_ui(ui, tok, &c.where_);
    // Compressed stored bytes: no byte maps to a field; offer the decompressed space.
    if let Some(sp) = c.decompressed {
        ui.add_space(6.0);
        if btn(ui, tok, "Open decompressed view").tip(&sp.title).clicked() {
            acts.push(Action::HexShowSpace(sp.space));
        }
    }
    other_interpretations(ui, st, tok, Some(s), None, &[], !c.structure);
}

/// Levels view summary: comparator per CF, files and bytes per level, the axis sentence.
fn levels_ui(ui: &mut egui::Ui, st: &AppState, tok: &Tokens) {
    let lines = explanation::levels_summary(st.session());
    if lines.is_empty() {
        return;
    }
    section(ui, tok, "Live SST files (MANIFEST)");
    for l in lines {
        ui.add(egui::Label::new(RichText::new(nb_units(&l)).monospace().color(tok.p.text)).wrap());
    }
}

/// A selected levels row / map box: every fact the map tooltip shows (0024 §6.2).
fn lsm_file_ui(ui: &mut egui::Ui, tok: &Tokens, c: &explanation::LsmFileCard) {
    section(ui, tok, "LSM file");
    facts_ui(ui, tok, &c.typed);
    // The x-axis sentence stays on the map strip and the levels card (0030 UI final §5.2: the
    // Key order fact carries the order here).
    let col = if c.on_disk { tok.p.text } else { tok.status(Status::NotPresent) };
    ui.add(egui::Label::new(RichText::new(&c.open).color(col)).wrap());
}

/// Facts with their source tag in a Small `text_muted` column on the right (0026 §5.1); key
/// previews in monospace; values wrap (no horizontal scrolling).
fn facts_ui(ui: &mut egui::Ui, tok: &Tokens, facts: &[MapFact]) {
    // 0026 D2 F1: fixed-size cells, no nested right-to-left / wrapping layouts (those took the
    // scroll area's whole height and pushed every value hundreds of points down).
    let small = egui::TextStyle::Small.resolve(ui.style());
    let tag_w = ["MANIFEST", "derived", "scan", "disk", "name"].iter().map(|t| ui.fonts_mut(|f| f.layout_no_wrap(t.to_string(), small.clone(), tok.p.text_muted).size().x)).fold(0.0, f32::max);
    for f in facts {
        ui.horizontal_top(|ui| {
            let sp = ui.spacing().item_spacing.x;
            ui.label(RichText::new(format!("{} ", f.label)).color(tok.p.text_muted));
            let value_w = (ui.available_width() - tag_w - sp).max(40.0);
            ui.allocate_ui_with_layout(egui::vec2(value_w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(value_w);
                let v = RichText::new(nb_units(&f.value)).color(tok.p.text);
                let v = if f.kind == ValueKind::Key { v.monospace() } else { v };
                ui.add(egui::Label::new(v).wrap());
            });
            ui.label(RichText::new(f.source.tag()).small().color(tok.p.text_muted));
        });
    }
}

/// The map's picked object (0026 §4–§5): a WAL, the older-WAL summary or a level — title,
/// what it is, every fact with its source, and the way to open a WAL.
fn map_card_ui(ui: &mut egui::Ui, tok: &Tokens, c: &explanation::MapCard, acts: &mut Vec<Action>) {
    ui.add(egui::Label::new(RichText::new(&c.title).heading()).wrap());
    if let Some(n) = c.note {
        ui.add(egui::Label::new(RichText::new(n).color(tok.p.text_muted)).wrap());
    }
    ui.add_space(4.0);
    facts_ui(ui, tok, &c.facts);
    if let Some(o) = &c.open {
        ui.add(egui::Label::new(RichText::new(o).color(tok.p.text)).wrap());
    }
    if let Some(n) = c.opens {
        ui.add_space(6.0);
        if btn(ui, tok, "Open WAL ›").clicked() {
            acts.push(Action::OpenWal(n));
        }
    }
}

fn field(ui: &mut egui::Ui, st: &AppState, tok: &Tokens, row: usize, f: &crate::model::Field, acts: &mut Vec<Action>) {
    // Title + status.
    ui.horizontal_wrapped(|ui| {
        // Not checked: no chip (no status is claimed, design A9).
        // The status sentence is the table tooltip's (`field_status_meaning`, architect N1).
        if f.not_checked.is_none() {
            let word = if f.untrusted { diagnosis::UNTRUSTED_WORD } else { f.status.label() };
            chip_word(ui, tok, f.status, word, explanation::field_status_meaning(f));
        }
        ui.heading(&f.name);
    });
    ui.add_space(4.0);
    // 0030 B-8 (rules in `explanation::field_body`; this only renders them).
    let explanation::FieldBody { show_value, no_bytes } = explanation::field_body(st.session(), st.selection(), f);
    if !f.explain.is_empty() || show_value || !f.encoding.is_empty() || !f.raw.is_empty() || no_bytes.is_some() {
        // Meaning: what the field is and its decoded value.
        section(ui, tok, "Meaning");
    }
    if !f.explain.is_empty() {
        ui.add(egui::Label::new(&f.explain).wrap());
    }
    // The decoded value once; its alternate notations sit in "Other byte interpretations".
    if show_value {
        long_value(ui, tok, &f.value);
    }
    if !f.encoding.is_empty() {
        ui.add(egui::Label::new(RichText::new(format!("Encoding: {}", f.encoding)).color(tok.p.text_muted)).wrap());
    }
    if !f.raw.is_empty() {
        long_value(ui, tok, &f.raw);
    }
    match &no_bytes {
        Some(t) => {
            ui.add(egui::Label::new(RichText::new(nb_units(t)).color(tok.p.text_muted)).wrap());
        }
        // Where: every address space, File first.
        None => where_ui(ui, tok, &explanation::where_text(st.session(), st.selection(), f.span)),
    }
    // 0030 A7: the MANIFEST rows lead to the same file on the LSM Map (the existing entry
    // focuses the open SST; Back returns).
    if f.kind == crate::model::FieldKind::ManifestEvidence
        && st.session().lsm_map_availability().is_ok()
        && ui.link("Show in LSM Map ›").tip("Open the folder's LSM Map with this file selected (Back returns)").clicked()
    {
        acts.push(Action::ShowLsmMap);
    }
    let card = explanation::lsm_file_card(st.session(), row);
    if let Some(c) = &card {
        lsm_file_ui(ui, tok, c);
    }
    // Diagnosis (or the separate Unsupported / Unknown statement); nothing for bytes that were
    // simply not checked.
    if let Some(fd) = diagnosis::finding(f.status, f.diag.as_deref(), &f.name).filter(|_| f.not_checked.is_none()) {
        ui.add_space(4.0);
        finding(ui, tok, &fd, "field", acts);
    }
    // Derivation: what the status means and where the link leads.
    section(ui, tok, "Derivation");
    match &f.not_checked {
        // A levels row: on disk / missing on disk (never the "absent optional part" legend).
        _ if card.is_some() => {
            let c = card.as_ref().map(|c| (c.derivation.as_str(), c.on_disk)).unwrap_or_default();
            let col = if c.1 { tok.p.text } else { tok.status(Status::NotPresent) };
            ui.add(egui::Label::new(RichText::new(c.0).color(col)).wrap());
        }
        Some(d) => {
            ui.add(egui::Label::new(RichText::new(d).color(tok.p.text_muted)).wrap());
        }
        None => {
            // A failure states its recorded cause (0019 §1); the legend sentence only when no
            // diagnostic was recorded.
            let block = matches!(f.link, Some(NodeKind::SstBlock { .. }));
            let meaning = match f.status {
                _ if f.untrusted => diagnosis::UNTRUSTED_MEANING.to_string(),
                _ if f.check_note.is_some() => f.check_note.unwrap_or_default().to_string(),
                s => diagnosis::failure_derivation(s, f.diag.as_deref(), block).unwrap_or_else(|| status_meaning(s).to_string()),
            };
            ui.add(egui::Label::new(format!("{} {meaning}", f.status.symbol())).wrap());
        }
    }
    if f.link.is_some() || f.open_path.is_some() {
        ui.add_space(6.0);
        let label = if f.open_path.is_some() { "Open referenced file ›" } else { "Go to target ›" };
        if btn(ui, tok, label).clicked() {
            acts.push(Action::FollowSelected);
        }
    }
    // Typed decoded value only; the display string is never parsed back.
    let number = f.decoded.as_ref().and_then(|d| d.as_u64());
    other_interpretations(ui, st, tok, f.span.as_ref(), number, &f.interp, false);
}

/// Node-level explanation when no row is selected: title, status and a summary of the
/// failures (≤ 5 lines with links, or the full grid when there is exactly one). Returns false
/// when there is nothing node-specific to say.
fn node(ui: &mut egui::Ui, st: &AppState, tok: &Tokens, acts: &mut Vec<Action>) -> bool {
    let Some(c) = explanation::node_card(st.session()) else { return false };
    ui.horizontal_wrapped(|ui| {
        if let Some(s) = c.status {
            chip(ui, tok, s);
        }
        ui.add(egui::Label::new(RichText::new(c.title).heading()).wrap());
    });
    ui.add_space(4.0);
    failures_ui(ui, tok, &c.failures, "node", acts);
    if let Some(fd) = &c.other {
        finding(ui, tok, fd, "node_other", acts);
    }
    table_legend(ui, tok, &c.legend);
    ui.add_space(6.0);
    ui.add(egui::Label::new(RichText::new("Select a row to see its full explanation.").color(tok.p.text_muted)).wrap());
    true
}

/// What the Status column of a lazily checked table means, one line per status present in it
/// (0019 §1); nothing for other views.
fn table_legend(ui: &mut egui::Ui, tok: &Tokens, lines: &[(Option<Status>, &'static str)]) {
    if lines.is_empty() {
        return;
    }
    section(ui, tok, "Status column");
    for &(s, text) in lines {
        let t = match s {
            Some(s) => RichText::new(format!("{} {text}", s.symbol())).color(tok.status(s)),
            None => RichText::new(text).color(tok.p.text),
        };
        ui.add(egui::Label::new(t).wrap());
    }
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
/// that selects the span in its own offset space. Prose wraps at word boundaries; monospace
/// values (hex, numbers, enum names, spans) wrap at the designed break points (0015 C2) — no
/// horizontal scrolling. Below `layout::DIAG_SIDE_MIN`, or when the widest value does not fit
/// beside the labels, each label (wrapping itself, C4/C5) sits above its full-width value.
fn diagnosis_ui(ui: &mut egui::Ui, tok: &Tokens, d: &Diagnosis, id: &str, acts: &mut Vec<Action>) {
    ui.label(RichText::new(format!("{} Diagnosis", d.status.symbol())).strong().color(tok.status(d.status)));
    let body = egui::TextStyle::Body.resolve(ui.style());
    let mono = egui::TextStyle::Monospace.resolve(ui.style());
    let w = |ui: &egui::Ui, s: &str, f: &egui::FontId| ui.fonts_mut(|x| x.layout_no_wrap(s.to_string(), f.clone(), tok.p.text).size().x);
    let label_w = d.lines.iter().map(|l| w(ui, l.label, &body)).fold(0.0, f32::max) + 8.0;
    // Unbreakable width of each line: the whole value, or the longest word of prose.
    let widest = d.lines.iter().map(|l| if l.prose { l.text.split_whitespace().map(|t| w(ui, t, &body)).fold(0.0, f32::max) } else { w(ui, &l.text, &mono) }).fold(0.0, f32::max);
    let side = layout::diag_side_by_side(ui.available_width(), label_w, widest);
    for (i, l) in d.lines.iter().enumerate() {
        ui.push_id((id, i), |ui| {
            let label = RichText::new(l.label).color(tok.p.text_muted);
            if side {
                ui.horizontal_top(|ui| {
                    ui.add_sized([label_w, ui.text_style_height(&egui::TextStyle::Body)], egui::Label::new(label));
                    diag_value(ui, tok, l, acts);
                });
            } else {
                // Stacked: the label wraps too, so it is never cut ("Record completeness (no
                // LAST fragment)" fits in two lines at the default width).
                ui.add(egui::Label::new(label).wrap());
                diag_value(ui, tok, l, acts);
                ui.add_space(2.0);
            }
        });
    }
    ui.add_space(4.0);
}

/// One Diagnosis value in the remaining width (see `diagnosis_ui`): prose wraps at words,
/// monospace values at the designed break points (`fit_value`), full value on hover.
fn diag_value(ui: &mut egui::Ui, tok: &Tokens, l: &DiagLine, acts: &mut Vec<Action>) {
    let mut t = if l.prose { RichText::new(&l.text) } else { RichText::new(fit_value(ui, &l.text).0).monospace() };
    if l.link.is_some() {
        t = t.color(tok.p.accent_text).underline();
    }
    let mut lab = if l.prose { egui::Label::new(t).wrap() } else { egui::Label::new(t).extend() };
    if l.link.is_some() {
        lab = lab.sense(egui::Sense::click());
    }
    let mut r = ui.add(lab);
    if !l.prose && l.link.is_none() {
        r = r.tip(value_tip(&l.text));
    }
    if let Some(span) = l.link
        && r.on_hover_cursor(egui::CursorIcon::PointingHand).tip("Select these bytes in their own offset space").clicked()
    {
        acts.push(Action::SelectSpan(span));
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
        "• Click a table row or a byte: a byte selects the field that holds it (the byte stays outlined); double-click / Enter follows a link.",
        "• Shift+click in the byte view selects a raw range from the clicked byte.",
        "• ] / [ step through fields; Shift+] / Shift+[ step through structures in file order.",
        "• Help › RocksVision Help explains the panels, search and Reset View.",
        "• Help › Keyboard Shortcuts lists every shortcut.",
    ] {
        ui.add(egui::Label::new(h).wrap());
    }
}

/// Interpretations: one row per reading, tokens joined by " / " and wrapped to the panel width
/// (break at the separators first; no horizontal scrolling, design C1/C2).
fn interp(ui: &mut egui::Ui, st: &AppState, tok: &Tokens, s: &Span) {
    let sp = Span { len: s.len.clamp(1, LIMITS.interp_bytes), ..*s };
    let Some(b) = st.session().hex().bytes_in(&sp) else {
        ui.label(RichText::new("(bytes not in the loaded hex page)").color(tok.p.text_muted));
        return;
    };
    for row in interpretations(&b) {
        interp_row(ui, tok, &row.label, &row.tokens.join(" / "));
    }
}
