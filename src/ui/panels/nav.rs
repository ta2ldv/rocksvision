//! Navigation panel: folder file list and the lazy structure tree.

use super::{strong, table_style, title_line};
use crate::app::{Action, AppState, Panel};
use crate::model::NodeKind;
use crate::ui::layout::LAYOUT;
use crate::ui::menu::{self, MenuCmd};
use crate::ui::theme::Tokens;
use eframe::egui::{self, RichText};

pub fn show(ui: &mut egui::Ui, st: &AppState, tok: &Tokens) -> Vec<Action> {
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
                let r = r.on_hover_text(format!("{} · {} · {} bytes{}", e.kind.label(), if e.kind.decodable() { "decoded" } else { "not decoded" }, e.size, if e.is_symlink { " · symlink (not followed)" } else { "" }));
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
    egui::ScrollArea::both().id_salt("tree").auto_shrink([false, false]).show(ui, |ui| {
        if doc.file().is_some() && doc.roots().is_empty() && doc.is_opening() {
            ui.spinner();
        }
        if doc.file().is_some() {
            let sel = doc.sel_node().is_none() && doc.view_kind() == Some(&NodeKind::Root);
            let t = RichText::new(format!("{} file (root)", doc.file_label())).strong();
            if ui.add(egui::Button::selectable(sel, if sel { t.color(tok.p.sel_fg) } else { t.color(tok.p.heading) }).wrap_mode(egui::TextWrapMode::Extend)).clicked() {
                acts.push(Action::SelectRoot);
            }
        }
        let mut stack: Vec<usize> = doc.roots().iter().rev().copied().collect();
        while let Some(i) = stack.pop() {
            let n = &doc.nodes()[i];
            ui.horizontal(|ui| {
                ui.add_space(n.depth as f32 * LAYOUT.tree_indent + 4.0);
                if n.leaf {
                    ui.add_space(18.0);
                } else if ui.small_button(if n.expanded { "−" } else { "+" }).clicked() {
                    acts.push(Action::ToggleNode(i));
                }
                let is_sel = doc.sel_node() == Some(i);
                let col = if is_sel { tok.p.sel_fg } else { tok.status(n.status) };
                let r = ui.add(egui::Button::selectable(is_sel, RichText::new(format!("{} {}", n.status.symbol(), n.label)).color(col)).wrap_mode(egui::TextWrapMode::Extend));
                let r = match n.contains {
                    Some(c) => r.on_hover_text(format!("{} · contains {} {} element(s)", n.status.label(), c.symbol(), c.label())),
                    None => r.on_hover_text(n.status.label()),
                };
                // A problem inside the node that its own status does not show (e.g. a truncated
                // last WAL record under "Physical blocks").
                if let Some(c) = n.contains {
                    ui.label(RichText::new(c.symbol()).color(tok.status(c))).on_hover_text(format!("Contains {} element(s)", c.label()));
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
            if n.expanded {
                if let Some(ch) = &n.children {
                    for c in ch.iter().rev() {
                        stack.push(*c);
                    }
                }
            }
        }
    });
    acts
}
