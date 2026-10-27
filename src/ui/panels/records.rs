//! Center panel: view title/notes, observed LSM boxes, and the records/fields table.
//! Column widths come from the column's semantic role (`layout::column_width`).

use super::{btn, btn_sized, chip, chip_word, status_meaning, status_text, strong, table_style};
use crate::app::diagnosis;
use crate::app::{Action, AppState, CopyKind};
use crate::model::{ColumnRole, NodeKind, Status, NOT_CHECKED};
use crate::ui::layout::{column_width, plan_columns, status_column_width, table_need_h, LAYOUT};
use crate::ui::menu;
use crate::ui::theme::Tokens;
use eframe::egui::{self, RichText};
use egui_extras::{Column as TCol, TableBuilder};

/// Table body rows that stay visible at the minimum window (900×700): the byte pane shrinks
/// first (see `ui::center`).
pub const TABLE_MIN_ROWS: f32 = 4.0;

/// Every word a Status cell shows: the legend statuses with their glyph, "Not checked" and
/// "✖ Untrusted".
pub fn status_words() -> Vec<String> {
    let mut v: Vec<String> = super::LEGEND.iter().map(|s| format!("{} {}", s.symbol(), s.label())).collect();
    v.push(NOT_CHECKED.to_string());
    v.push(format!("{} {}", Status::Invalid.symbol(), diagnosis::UNTRUSTED_WORD));
    v
}

/// Width (points) of the widest status word in the current body font.
pub fn widest_status_word(ui: &egui::Ui) -> f32 {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let color = ui.visuals().text_color();
    status_words().into_iter().map(|w| ui.fonts_mut(|f| f.layout_no_wrap(w, font.clone(), color).size().x)).fold(0.0, f32::max)
}

/// egui temp-data key: height the records panel needs above its table with its notes at full
/// height (title, location, notes, separator, column toggle), from the last frame.
pub fn overhead_id() -> egui::Id {
    egui::Id::new("records_overhead")
}

/// egui temp-data key: height between the notes and the table (last frame).
fn post_id() -> egui::Id {
    egui::Id::new("records_post_notes")
}

/// `scroll` is the one-shot "scroll this row into view" request taken from the app state (row,
/// centered?).
pub fn show(ui: &mut egui::Ui, st: &AppState, tok: &Tokens, scroll: Option<(usize, bool)>) -> Vec<Action> {
    let mut acts = Vec::new();
    let top_y = ui.cursor().top();
    // F3: after a zoom, split or window resize the selected row is brought into view again
    // (minimal scroll), so it is never left outside the table or under its header.
    let geo_id = egui::Id::new("records_geometry");
    let geo = [ui.available_width(), ui.available_height(), ui.ctx().zoom_factor()];
    let resized = ui.ctx().data(|d| d.get_temp::<[f32; 3]>(geo_id)).is_some_and(|g| g != geo);
    ui.ctx().data_mut(|d| d.insert_temp(geo_id, geo));
    let scroll = scroll.or_else(|| st.selection().row().filter(|_| resized).map(|r| (r, false)));
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
            match view.status_word {
                Some(w) => chip_word(ui, tok, s, w, status_meaning(s)),
                None => chip(ui, tok, s),
            }
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
    // Location header: file range, stored size, contents/trailer, next structure or gap (links).
    for w in &view.location {
        match &w.link {
            Some(t) => {
                let r = ui.add(egui::Label::new(RichText::new(&w.text).small().color(tok.p.accent_text).underline()).sense(egui::Sense::click()).wrap());
                if r.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Go to this structure (Back returns)").clicked() {
                    acts.push(Action::Navigate { target: t.node.clone(), span: t.span });
                }
            }
            None => {
                ui.add(egui::Label::new(RichText::new(&w.text).small().monospace().color(tok.p.text)).wrap());
            }
        }
    }
    // Notes scroll in their own area when space is short, so the table keeps its header and at
    // least `TABLE_MIN_ROWS` rows (900×700 acceptance). At narrow heights the subtitle notes
    // collapse to one line; the tooltip has the full text.
    let small_h = ui.text_style_height(&egui::TextStyle::Small);
    let sp_y = ui.spacing().item_spacing.y;
    let table_need = table_need_h(tok.header_height(ui), tok.row_height(ui), sp_y, ui.spacing().scroll.allocated_width(), TABLE_MIN_ROWS);
    // Natural height of the notes (and the search line) at this width; the part between the
    // notes and the table (separator, LSM boxes, column toggle) as measured last frame.
    let small = egui::TextStyle::Small.resolve(ui.style());
    let wrap_w = ui.available_width();
    let search_line = !find.row_matches().is_empty() || (!find.query().is_empty() && find.hit().is_none() && find.progress().is_none());
    let notes_h: f32 = view.notes.iter().map(|n| ui.fonts_mut(|f| f.layout(n.text.clone(), small.clone(), tok.p.text, wrap_w).size().y) + sp_y).sum::<f32>() + if search_line { small_h + sp_y } else { 0.0 };
    let post = ui.ctx().data(|d| d.get_temp::<f32>(post_id())).unwrap_or(tok.row_height(ui) + 8.0);
    let fixed = ui.cursor().top() - top_y;
    ui.ctx().data_mut(|d| d.insert_temp(overhead_id(), fixed + notes_h + post));
    // Narrow height: the full notes would leave the table fewer than TABLE_MIN_ROWS rows.
    let narrow = ui.available_height() - notes_h - post < table_need;
    let notes_max = (ui.available_height() - post - table_need).max(small_h + 4.0);
    if narrow && !view.notes.is_empty() {
        let worst = view.notes.iter().find_map(|n| n.status);
        let col = worst.map(|s| tok.status(s)).unwrap_or(tok.p.text_muted);
        let first = view.notes.iter().find(|n| n.status.is_some()).unwrap_or(&view.notes[0]);
        let more = if view.notes.len() > 1 { format!("  (+{} more)", view.notes.len() - 1) } else { String::new() };
        let all = view.notes.iter().map(|n| n.text.as_str()).collect::<Vec<_>>().join("\n");
        ui.add(egui::Label::new(RichText::new(format!("{}{more}", first.text)).small().color(col)).truncate()).on_hover_text(all);
    }
    egui::ScrollArea::vertical().id_salt(("notes", epoch)).max_height(notes_max).auto_shrink([false, true]).show(ui, |ui| {
        for n in view.notes.iter().filter(|_| !narrow) {
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
    let after_notes = ui.cursor().top();
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
    // Columns that fit: Raw hex, then Encoding, then Len hide first; Status may become
    // icon-only; Offset keeps 10 monospace characters. "+N columns" brings them back.
    let mono = egui::TextStyle::Monospace.resolve(ui.style());
    let mono_w = ui.fonts_mut(|f| f.glyph_width(&mono, '0'));
    // Status keeps its widest word ("Not checked", "✂ Truncated" …) at every zoom (C2 GUI-1).
    let status_w = status_column_width(widest_status_word(ui));
    let width_of = |r: ColumnRole| match r {
        ColumnRole::Offset => column_width(r).max(10.0 * mono_w + 8.0),
        ColumnRole::Status => status_w,
        _ => column_width(r),
    };
    let roles: Vec<ColumnRole> = view.columns.iter().map(|c| c.role).collect();
    let avail_cols = ui.available_width() - ui.spacing().scroll.allocated_width();
    let plan = plan_columns(&roles, avail_cols, st.prefs().all_columns(), &width_of, ui.spacing().item_spacing.x);
    let would_hide = plan_columns(&roles, avail_cols, false, &width_of, ui.spacing().item_spacing.x).hidden;
    if would_hide > 0 {
        let label = if st.prefs().all_columns() { "Fewer columns".to_string() } else { format!("+{} columns", plan.hidden) };
        let tip = if st.prefs().all_columns() { "Hide Raw hex / Encoding / Len again so the table fits (Reset View also restores this)" } else { "Show the columns hidden for lack of width (the table then scrolls horizontally)" };
        if ui.small_button(label).on_hover_text(tip).clicked() {
            acts.push(Action::ToggleAllColumns);
        }
    }
    // Display order: first column, then Status (never clipped), then the rest.
    let ncols = view.columns.len();
    let status_idx = view.columns.iter().position(|c| c.role == ColumnRole::Status);
    let mut order: Vec<usize> = Vec::with_capacity(ncols);
    if ncols > 0 {
        order.push(0);
    }
    if let Some(si) = status_idx.filter(|&i| i != 0) {
        order.push(si);
    }
    order.extend((1..ncols).filter(|i| Some(*i) != status_idx));
    order.retain(|&i| plan.shown[i]);
    let ncols = order.len();
    let col_w = |ci: usize| width_of(view.columns[ci].role);
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
    let fixed: f32 = order.iter().take(rem_pos).map(|&ci| col_w(ci)).sum();
    // Width the table needs: fixed columns + Decoded minimum + cell spacing + its vertical
    // scrollbar. The horizontal bar appears only when this exceeds the panel (or the user
    // widened columns), because the content is exactly the panel width otherwise.
    let need = fixed + LAYOUT.remainder_min + spacing * ncols as f32 + ui.spacing().scroll.allocated_width();
    let table_w = ui.available_width().max(need);
    let row_h = tok.row_height(ui);
    let header_h = tok.header_height(ui);
    let header_font = tok.header_font();
    let post_now = ui.cursor().top() - after_notes;
    ui.ctx().data_mut(|d| d.insert_temp(post_id(), post_now));
    egui::ScrollArea::horizontal().id_salt(("tblh", &salt, epoch)).auto_shrink([false, false]).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded).show(ui, |ui| {
        ui.set_width(table_w);
        let bg = ui.available_rect_before_wrap();
        ui.painter().rect_filled(bg, 0.0, tok.p.stripe_a);
        let hover_painter = ui.painter().clone();
        let body_clip = std::cell::Cell::new(egui::Rect::NOTHING);
        let mut tb = TableBuilder::new(ui).id_salt(("tbl", &salt, epoch)).striped(true).sense(egui::Sense::click()).resizable(true).cell_layout(egui::Layout::left_to_right(egui::Align::Center));
        for (pos, &ci) in order.iter().enumerate() {
            // Status never narrower than its widest word (also when resized by hand).
            let min = if Some(ci) == status_idx { status_w } else { LAYOUT.column_min };
            let col = if pos == rem_pos { TCol::remainder().at_least(LAYOUT.remainder_min).clip(true) } else { TCol::initial(col_w(ci)).at_least(min).resizable(true).clip(true) };
            tb = tb.column(col);
        }
        if let Some((r, center)) = scroll.filter(|(r, _)| *r < nrows) {
            tb = tb.scroll_to_row(r, center.then_some(egui::Align::Center));
        }
        tb.header(header_h, |mut h| {
            for &ci in &order {
                h.col(|ui| {
                    let r = ui.max_rect().expand2(egui::vec2(spacing / 2.0, 2.0));
                    tok.paint_header(ui.painter(), r);
                    // A header narrower than its title — by column width or cut by the table's
                    // horizontal scroll viewport — ends in "…" or shows nothing; never a
                    // clipped glyph.
                    let title = view.columns[ci].title;
                    let text = RichText::new(title).font(header_font.clone()).strong().color(tok.p.header_fg);
                    let visible = ui.max_rect().intersect(ui.clip_rect());
                    let ell = ui.fonts_mut(|f| f.layout_no_wrap("…".into(), header_font.clone(), tok.p.header_fg).size().x);
                    if visible.width() >= ui.max_rect().width() - 0.5 {
                        ui.add(egui::Label::new(text).truncate()).on_hover_text(title);
                    } else if visible.width() >= ell + 2.0 {
                        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(visible).layout(egui::Layout::left_to_right(egui::Align::Center)));
                        child.add(egui::Label::new(text).truncate()).on_hover_text(title);
                    }
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
                    // A field without bytes has no offset/length: "—" (never a clipped "(").
                    let role = view.columns[ci].role;
                    let no_bytes = r.field.span.is_none() && matches!(role, ColumnRole::Offset | ColumnRole::Len) && (cell.starts_with('(') || cell == "-" || cell == "—");
                    if no_bytes {
                        cell = "—";
                    }
                    let full_cell = cell.to_string();
                    // Not checked: no glyph and no status colour (design A2).
                    let not_checked = Some(ci) == status_idx && r.field.not_checked.is_some();
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
                        if not_checked {
                            t = t.color(tok.p.text_muted);
                        } else if Some(ci) == status_idx {
                            t = t.color(tok.status(r.field.status));
                        }
                        if pos == 0 && matches.contains(&i) {
                            t = t.color(tok.p.match_fg).background_color(tok.p.match_bg).strong();
                        }
                        if is_sel {
                            // Selection overrides row/status colors so they stay readable.
                            t = t.color(tok.p.sel_fg);
                        }
                        let tip = if Some(ci) == status_idx && r.field.untrusted {
                            diagnosis::UNTRUSTED_MEANING.to_string()
                        } else if let Some(d) = r.field.not_checked.as_ref().filter(|_| not_checked) {
                            d.clone()
                        } else if no_bytes {
                            "No bytes in the file (derived, inferred, default or metadata value)".to_string()
                        } else {
                            full_cell.clone()
                        };
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
                        let item = ui.add_enabled(enabled, egui::Button::new(label));
                        tok.menu_item_hover(ui, &item);
                        if item.clicked() {
                            copy_act = Some((i, k));
                            ui.close();
                        }
                    }
                    if r.field.link.is_some() || r.field.open_path.is_some() {
                        ui.separator();
                        let item = ui.button("Follow link ›");
                        tok.menu_item_hover(ui, &item);
                        if item.clicked() {
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
