//! Navigation panel: folder file list and the lazy structure tree.

use super::{TipExt, strong, table_style, title_line};
use crate::app::{Action, AppState, Panel};
use crate::model::{ChildRole, NodeKind};
use crate::ui::layout::{LAYOUT, tree_label_wraps};
use crate::ui::menu::{self, MenuCmd};
use crate::ui::theme::Tokens;
use eframe::egui::{self, RichText};

/// `scroll_to`: the one-shot tree reveal request (node index) taken from the app state.
pub fn show(ui: &mut egui::Ui, st: &AppState, tok: &Tokens, scroll_to: Option<usize>) -> Vec<Action> {
    let mut acts = Vec::new();
    table_style(ui);
    let doc = st.session();
    let title = match doc.dir() {
        Some(d) => format!("Folder: {}", d.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
        None => "Navigation".to_string(),
    };
    if title_line(ui, tok, &title, "◀", &menu::tip("Hide Navigation", MenuCmd::TogglePanel(Panel::Navigation))) {
        acts.push(Action::TogglePanel(Panel::Navigation));
    }
    if doc.dir().is_some() {
        if st.jobs().scanning() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Scanning folder…").color(tok.p.text_muted));
            });
        } else if !doc.dir_note().is_empty() {
            ui.label(RichText::new(doc.dir_note()).small().color(tok.p.text_muted));
        }
        let h = (ui.available_height() * LAYOUT.file_list_share).max(80.0);
        egui::ScrollArea::both().id_salt("files").max_height(h).auto_shrink([false, true]).show(ui, |ui| {
            for e in doc.dir_entries() {
                let cur = doc.file() == Some(e.path.as_path());
                let txt = e.name.clone();
                let col = if cur {
                    tok.p.sel_fg
                } else if e.kind.decodable() {
                    tok.p.text
                } else {
                    tok.p.text_muted
                };
                let r = ui.add(egui::Button::selectable(cur, RichText::new(txt).color(col)).wrap_mode(egui::TextWrapMode::Extend));
                let r = r.tip(format!(
                    "{} · {} · {} bytes{}",
                    e.kind.label(),
                    if e.kind.decodable() { "decoded" } else { "not decoded" },
                    e.size,
                    if e.is_symlink { " · symlink (not followed)" } else { "" }
                ));
                if r.clicked() && !e.is_symlink {
                    acts.push(Action::OpenFile(e.path.clone()));
                }
            }
        });
        ui.separator();
    } else if doc.file().is_none() {
        ui.label(RichText::new("No folder opened").color(tok.p.text_muted));
        ui.separator();
    }
    strong(ui, tok, "Structure");
    // Vertical scrolling only: a reveal never moves the labels sideways (they start at their
    // first character); long labels end in "…" with the full text in the tooltip.
    egui::ScrollArea::vertical().id_salt("tree").auto_shrink([false, false]).show(ui, |ui| {
        if doc.file().is_some() && doc.roots().is_empty() && doc.is_opening() {
            ui.spinner();
        }
        if doc.file().is_some() {
            let sel = doc.sel_node().is_none() && doc.view_kind() == Some(&NodeKind::Root);
            let t = RichText::new(format!("{} file (root)", doc.file_label())).strong();
            if ui
                .add(egui::Button::selectable(sel, if sel { t.color(tok.p.sel_fg) } else { t.color(tok.p.heading) }).wrap_mode(egui::TextWrapMode::Truncate))
                .tip(format!("{} file (root)", doc.file_label()))
                .clicked()
            {
                acts.push(Action::SelectRoot);
            }
        }
        let mut stack: Vec<usize> = doc.roots().iter().rev().copied().collect();
        // Root level: "Physical layout (file order)" first, then "Views" (labeled entry points).
        let mut section: Option<bool> = None;
        let is_view = |i: &usize| matches!(doc.nodes()[*i].role, ChildRole::View | ChildRole::Absent);
        let sections = doc.roots().iter().any(is_view) && !doc.roots().iter().all(is_view);
        while let Some(i) = stack.pop() {
            let n = &doc.nodes()[i];
            if n.depth == 0 && sections {
                let view = matches!(n.role, ChildRole::View | ChildRole::Absent);
                if section != Some(view) {
                    section = Some(view);
                    ui.add(egui::Label::new(RichText::new(if view { "Views" } else { "Physical layout (file order)" }).small().color(tok.p.text_muted)).selectable(false));
                }
            }
            ui.horizontal(|ui| {
                ui.add_space(n.depth as f32 * LAYOUT.tree_indent + 4.0);
                if n.leaf {
                    ui.add_space(18.0);
                } else if ui.small_button(if n.expanded { "−" } else { "+" }).clicked() {
                    acts.push(Action::ToggleNode(i));
                }
                let is_sel = doc.sel_node() == Some(i);
                // One glyph per node: the worse of its own status and a problem inside it (the
                // label carries the text suffix, e.g. "· 1 incomplete record").
                let shown = n.shown_status();
                // A group that is not checked as a whole claims no status (no glyph, design A3).
                let unchecked = n.unchecked();
                let col = if is_sel {
                    tok.p.sel_fg
                } else if unchecked {
                    tok.p.text
                } else {
                    tok.status(shown)
                };
                let full = n.full_label();
                let shown_text = if unchecked { full.clone() } else { format!("{} {full}", shown.symbol()) };
                // A group's head (what, members and coverage) is never cut (design A5): only the
                // tail truncates; when even the head does not fit, the head alone is shown and
                // wraps (range and block count stay in the tooltip).
                let head = if n.head_len() > 0 { shown_text.len() - full.len() + n.head_len() } else { 0 };
                let wrap = head > 0 && {
                    let body = egui::TextStyle::Button.resolve(ui.style());
                    let w = ui.fonts_mut(|f| f.layout_no_wrap(shown_text[..head].to_string(), body, col).size().x);
                    tree_label_wraps(w, ui.available_width() - 2.0 * ui.spacing().button_padding.x)
                };
                let mut text = RichText::new(if wrap { &shown_text[..head] } else { shown_text.as_str() }).color(col);
                if n.role == ChildRole::Reference {
                    text = text.italics();
                }
                let r = ui.add(egui::Button::selectable(is_sel, text).wrap_mode(if wrap { egui::TextWrapMode::Wrap } else { egui::TextWrapMode::Truncate }));
                let tip = match (n.role, n.contains, n.checked) {
                    _ if n.tally.is_some() => n.tally.map(|t| t.tooltip()).unwrap_or_default(),
                    (_, _, Some((0, _))) if unchecked && n.leaf => "Not checked: listed beyond this listing's check budget; it is checked when opened".to_string(),
                    (_, _, Some((0, _))) if unchecked => "Not checked: no member of this group was checked yet (expand it to check its blocks)".to_string(),
                    (_, _, Some((x, y))) if unchecked => format!("Not checked as a whole: {x} of {y} members checked, none with a problem; the others are checked when expanded or opened"),
                    (ChildRole::Reference, _, _) => format!("{} · link: click to go to this structure at its place in file order", n.status.label()),
                    (_, Some(c), _) if c != n.status => format!("{} · contains {} {} element(s)", n.status.label(), c.symbol(), c.label()),
                    _ => n.status.label().to_string(),
                };
                // A table view node first says what it lists (0020 §5), then its status.
                let r = match crate::app::provenance::view_tooltip(&n.kind).filter(|_| n.role == ChildRole::View) {
                    Some(what) => r.tip(format!("{full}\n{what}\n{tip}")),
                    None => r.tip(format!("{full}\n{tip}")),
                };
                if scroll_to == Some(i) {
                    // Vertical reveal only (the tree has no horizontal scrolling).
                    r.scroll_to_me(Some(egui::Align::Center));
                }
                if r.clicked() {
                    acts.push(Action::SelectNode(i));
                }
                if r.double_clicked() && !n.leaf {
                    acts.push(Action::ToggleNode(i));
                }
                if n.loading {
                    ui.spinner();
                }
            });
            if n.expanded
                && let Some(ch) = &n.children
            {
                for c in ch.iter().rev() {
                    stack.push(*c);
                }
            }
        }
    });
    acts
}
