//! Center panel: view title/notes, observed LSM boxes, and the records/fields table.
//! Column widths come from the column's semantic role (`layout::column_width`).

use super::{btn, btn_sized, chip, status_text, strong, table_style};
use crate::app::diagnosis;
use crate::app::{Action, AppState, CopyKind};
use crate::model::{NodeKind, Status};
use crate::ui::layout::{column_width, LAYOUT};
use crate::ui::menu;
use crate::ui::theme::Tokens;
use eframe::egui::{self, RichText};
use egui_extras::{Column as TCol, TableBuilder};

/// `scroll` is the one-shot "scroll this row into view" request taken from the app state.
pub fn show(ui: &mut egui::Ui, st: &AppState, tok: &Tokens, scroll: Option<usize>) -> Vec<Action> {
    let mut acts = Vec::new();
    table_style(ui);
    let doc = st.session();
    let find = st.search();
    let epoch = st.prefs().layout_epoch();
    let Some(view) = doc.view() else {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            ui.heading("RocksVision");
            ui.label(RichText::new("Read-only inspector for RocksDB 10.4.2 WAL, SST, MANIFEST and CURRENT files.").color(tok.p.text_muted));
            ui.add_space(12.0);
            if doc.file().is_some() {
                if doc.pending_view().is_some() {
                    ui.spinner();
                }
            } else {
                ui.label(if doc.dir().is_some() { "Select a file in the folder list." } else { "No file opened" });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    // Center the two buttons.
                    let w = 2.0 * LAYOUT.start_button_width + ui.spacing().item_spacing.x;
                    ui.add_space(((ui.available_width() - w) / 2.0).max(0.0));
                    if btn_sized(ui, tok, "📂 Open Folder…").clicked() {
                        acts.push(Action::PickFolder);
                    }
                    if btn_sized(ui, tok, "📄 Open File…").clicked() {
                        acts.push(Action::PickFile);
                    }
                });
            }
        });
        return acts;
    };
    let mut go_back = false;
    ui.horizontal_wrapped(|ui| {
        if btn(ui, tok, "◀ Back").on_hover_text(format!("Back ({})", menu::back_key(menu::KeyLabels::HOST))).clicked() && doc.can_go_back() {
            go_back = true;
        }
        if let Some(s) = view.status {
            chip(ui, tok, s);
        }
        ui.heading(&view.title);
        // A view without a status of its own still names the failures inside it, e.g. the
        // WAL root "✂ 1 truncated" (details: Explanation with nothing selected).
        if view.status.is_none() {
            let tree = doc.sel_node().and_then(|i| doc.nodes().get(i));
            let fails = diagnosis::node_failures(view, tree.map(|n| n.status), tree.and_then(|n| n.diag.as_deref()));
            if let Some((s, text)) = diagnosis::failure_counts(&fails) {
                status_text(ui, tok, s, &format!("{} {text}", s.symbol()));
            }
        }
        if doc.pending_view().is_some() {
            ui.spinner();
        }
    });
    // Notes scroll in their own area when space is short, so the table keeps its header and at
    // least one row (zoom-ceiling acceptance, spec §8.2 d).
    let small_h = ui.text_style_height(&egui::TextStyle::Small);
    let notes_max = (ui.available_height() - tok.header_height(ui) - 2.0 * tok.row_height(ui) - 8.0).max(small_h + 4.0);
    egui::ScrollArea::vertical().id_salt(("notes", epoch)).max_height(notes_max).auto_shrink([false, true]).show(ui, |ui| {
        for n in &view.notes {
            // Problem notes carry their status and use that status color.
            let col = n.status.map(|s| tok.status(s)).unwrap_or(tok.p.text_muted);
            ui.add(egui::Label::new(RichText::new(&n.text).small().color(col)).wrap());
        }
        if !find.row_matches().is_empty() {
            ui.small(format!("Rows: {} rows of the current table match \"{}\" (decoded cells; Prev/Next to step)", find.row_matches().len(), find.query()));
        } else if !find.query().is_empty() && find.hit().is_none() && find.progress().is_none() {
            ui.small(if find.hex_mode() { "Hex query: Enter searches the physical file bytes (not decompressed)." } else { "No row of the current table matches — Enter searches the physical file bytes (not decompressed)." });
        }
    });
    ui.separator();
    // Observed LSM boxes (CF → level → SST files) above the table.
    let mut box_click = None;
    if doc.view_kind() == Some(&NodeKind::ManifestLevels) && !view.lsm.is_empty() {
        egui::CollapsingHeader::new("Observed LSM shape (boxes; from MANIFEST, not history)").default_open(true).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt(("lsm_boxes", epoch)).max_height(LAYOUT.lsm_box_max_height).show(ui, |ui| {
                let mut last = (String::new(), String::new());
                for (i, f) in view.lsm.iter().enumerate() {
                    let key = (f.cf.clone(), f.level.clone());
                    if key.0 != last.0 {
                        strong(ui, tok, format!("CF {}", key.0));
                    }
                    if key != last {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(format!("{:>3}", key.1)).monospace());
                            for b in view.lsm.iter().skip(i) {
                                if (b.cf.as_str(), b.level.as_str()) != (key.0.as_str(), key.1.as_str()) {
                                    break;
                                }
                                let c = if b.status == Status::Present { tok.p.lsm_present } else { tok.p.lsm_missing };
                                let resp = egui::Frame::new()
                                    .fill(tok.p.lsm_bg)
                                    .stroke(egui::Stroke::new(1.5, c))
                                    .inner_margin(egui::Margin::symmetric(6, 3))
                                    .corner_radius(tok.w.rounding)
                                    .show(ui, |ui| {
                                        ui.label(RichText::new(format!("{} {} · {} B\n{} …\n{}", b.status.symbol(), b.file, b.size, b.smallest, b.largest)).small().color(tok.p.text));
                                    })
                                    .response
                                    .interact(egui::Sense::click());
                                let resp = resp.on_hover_text(format!("{} · overlap: {}", b.status.label(), b.overlap));
                                if resp.clicked() {
                                    box_click = Some(b.row);
                                }
                            }
                        });
                        last = key;
                    }
                }
            });
        });
        ui.separator();
    }
    // Display order: first column, then Status (never clipped), then the rest.
    let ncols = view.columns.len();
    let status_idx = view.columns.iter().position(|c| c.role == crate::model::ColumnRole::Status);
    let mut order: Vec<usize> = Vec::with_capacity(ncols);
    if ncols > 0 {
        order.push(0);
    }
    if let Some(si) = status_idx.filter(|&i| i != 0) {
        order.push(si);
    }
    order.extend((1..ncols).filter(|i| Some(*i) != status_idx));
    let nrows = view.rows.len();
    let mut clicked = None;
    let mut dbl = None;
    let mut copy_act: Option<(usize, CopyKind)> = None;
    let matches: std::collections::HashSet<usize> = find.row_matches().iter().copied().collect();
    let sel_row = st.selection().row();
    let salt = format!("{:?}", doc.view_kind());
    // The first fill-role column takes the remaining width (at least `remainder_min`); it is moved
    // to the end. Other columns keep fixed useful widths; if they do not fit, the table gets
    // a horizontal scrollbar instead of squeezing.
    if let Some(p) = order.iter().position(|&ci| view.columns[ci].role.fills()) {
        let ci = order.remove(p);
        order.push(ci);
    }
    let rem_pos = ncols.saturating_sub(1);
    let spacing = ui.spacing().item_spacing.x;
    let fixed: f32 = order.iter().take(rem_pos).map(|&ci| column_width(view.columns[ci].role)).sum();
    // Width the table needs: fixed columns + Decoded minimum + cell spacing + its vertical
    // scrollbar. The horizontal bar appears only when this exceeds the panel (or the user
    // widened columns), because the content is exactly the panel width otherwise.
    let need = fixed + LAYOUT.remainder_min + spacing * ncols as f32 + ui.spacing().scroll.allocated_width();
    let table_w = ui.available_width().max(need);
    let row_h = tok.row_height(ui);
    let header_h = tok.header_height(ui);
    let header_font = tok.header_font();
    egui::ScrollArea::horizontal().id_salt(("tblh", &salt, epoch)).auto_shrink([false, false]).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded).show(ui, |ui| {
        ui.set_width(table_w);
        let bg = ui.available_rect_before_wrap();
        ui.painter().rect_filled(bg, 0.0, tok.p.stripe_a);
        let hover_painter = ui.painter().clone();
        let body_clip = std::cell::Cell::new(egui::Rect::NOTHING);
        let mut tb = TableBuilder::new(ui).id_salt(("tbl", &salt, epoch)).striped(true).sense(egui::Sense::click()).resizable(true).cell_layout(egui::Layout::left_to_right(egui::Align::Center));
        for (pos, &ci) in order.iter().enumerate() {
            let c = &view.columns[ci];
            let col = if pos == rem_pos { TCol::remainder().at_least(LAYOUT.remainder_min).clip(true) } else { TCol::initial(column_width(c.role)).at_least(LAYOUT.column_min).resizable(true).clip(true) };
            tb = tb.column(col);
        }
        if let Some(r) = scroll {
            tb = tb.scroll_to_row(r, Some(egui::Align::Center));
        }
        tb.header(header_h, |mut h| {
            for &ci in &order {
                h.col(|ui| {
                    let r = ui.max_rect().expand2(egui::vec2(spacing / 2.0, 2.0));
                    tok.paint_header(ui.painter(), r);
                    ui.label(RichText::new(view.columns[ci].title).font(header_font.clone()).strong().color(tok.p.header_fg));
                });
            }
        })
        .body(|body| {
            body.rows(row_h, nrows, |mut row| {
                let i = row.index();
                let r = &view.rows[i];
                let is_sel = sel_row == Some(i);
                row.set_selected(is_sel);
                for (pos, &ci) in order.iter().enumerate() {
                    let mut cell = r.cells.get(ci).map(|c| c.as_str()).unwrap_or("");
                    // Untrusted rows (decoded from a checksum-failed block) keep the dump's cell
                    // word; the table shows the Invalid-derived "Untrusted" word instead.
                    let untrusted_cell;
                    if Some(ci) == status_idx && r.field.untrusted {
                        untrusted_cell = format!("{} {}", r.field.status.symbol(), diagnosis::UNTRUSTED_WORD);
                        cell = &untrusted_cell;
                    }
                    row.col(|ui| {
                        if pos == 0 {
                            let c = ui.clip_rect();
                            body_clip.set(egui::Rect::from_x_y_ranges(hover_painter.clip_rect().x_range(), c.y_range()));
                        }
                        if pos == 0 && is_sel {
                            // Left accent stripe on the selected row.
                            let rr = ui.max_rect();
                            ui.painter().rect_filled(egui::Rect::from_min_size(rr.left_top() - egui::vec2(4.0, 0.0), egui::vec2(LAYOUT.selected_stripe, rr.height())), 0.0, tok.p.sel_edge);
                        }
                        let mut t = RichText::new(cell).color(tok.p.text);
                        if Some(ci) == status_idx {
                            t = t.color(tok.status(r.field.status));
                        }
                        if pos == 0 && matches.contains(&i) {
                            t = t.color(tok.p.match_fg).background_color(tok.p.match_bg).strong();
                        }
                        if is_sel {
                            // Selection overrides row/status colors so they stay readable.
                            t = t.color(tok.p.sel_fg);
                        }
                        let tip = if Some(ci) == status_idx && r.field.untrusted { diagnosis::UNTRUSTED_MEANING } else { cell };
                        ui.add(egui::Label::new(t).truncate().selectable(false)).on_hover_text(tip);
                    });
                }
                let resp = row.response();
                if resp.hovered() {
                    // Edge cue so hover does not rely on the fill color alone (on a selected row
                    // the selection fill and stripe stay; hover only adds the outline).
                    let r = resp.rect.intersect(body_clip.get());
                    if r.height() > 0.0 {
                        hover_painter.rect_stroke(r, 0.0, egui::Stroke::new(1.0, tok.p.outline), egui::StrokeKind::Inside);
                    }
                }
                if resp.clicked() || resp.secondary_clicked() {
                    clicked = Some(i);
                }
                if resp.double_clicked() {
                    dbl = Some(i);
                }
                let has_span = r.field.span.is_some();
                let has_key = r.field.key_bytes.is_some();
                resp.context_menu(|ui| {
                    for (k, label, enabled) in [(CopyKind::Value, "Copy value", true), (CopyKind::Key, "Copy key", has_key), (CopyKind::Hex, "Copy hex (max 64 KiB)", has_span), (CopyKind::Offset, "Copy offset", has_span), (CopyKind::RowTsv, "Copy row (TSV)", true)] {
                        if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                            copy_act = Some((i, k));
                            ui.close();
                        }
                    }
                    if r.field.link.is_some() || r.field.open_path.is_some() {
                        ui.separator();
                        if ui.button("Follow link ›").clicked() {
                            dbl = Some(i);
                            ui.close();
                        }
                    }
                });
            });
        });
    });
    if let Some((i, k)) = copy_act {
        acts.push(Action::CopyRow(i, k));
    }
    if let Some(j) = box_click {
        acts.push(Action::SelectRow(j));
    }
    if go_back {
        acts.push(Action::Back);
    }
    if let Some(i) = clicked {
        acts.push(Action::SelectRow(i));
    }
    if let Some(i) = dbl {
        acts.push(Action::FollowRow(i));
    }
    acts
}
