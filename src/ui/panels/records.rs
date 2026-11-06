//! Center panel: view title/notes and the records/fields table — or, for the Observed levels in
//! Map mode, the LSM Map (`lsm_map`) over the whole records area.
//! Column widths come from the column's semantic role (`layout::column_width`).

use super::{Elided, TipExt, btn, btn_sized, chip, chip_word, disabled_btn, latched_btn, status_meaning, status_text, table_style};
use crate::app::state::NO_PREVIOUS_VIEW;
use crate::app::{Action, AppState, CopyKind, MapMode, ScrollRequest};
use crate::app::{diagnosis, explanation};
use crate::model::{ColumnRole, NOT_CHECKED, NodeKind, Status};
use crate::ui::layout::{LAYOUT, column_width, nb_units, plan_columns, status_column_width, table_need_h};
use crate::ui::menu;
use crate::ui::theme::Tokens;
use eframe::egui::{self, RichText};
use egui_extras::{Column as TCol, TableBuilder};

const BACK_LABEL: &str = "◀ Back";

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

/// egui temp-data key: the header height (incl. full notes) of the last uncompacted frame of
/// this view (review N2: per view, so a shorter header is not kept compacted by another's).
fn full_overhead_id(view: Option<&NodeKind>, title: &str) -> egui::Id {
    egui::Id::new(("records_full_overhead", format!("{view:?}"), title))
}

/// Header compaction (0019 §3): the pane (`avail`, from its top) is shorter than the full header
/// measured in the last uncompacted frame + the table's `TABLE_MIN_ROWS` rows. Never before a
/// full header was measured.
pub fn header_compact(avail: f32, full_over: Option<f32>, table_need: f32) -> bool {
    full_over.is_some_and(|f| avail < f + table_need)
}

/// egui temp-data key: full table body rows visible in the last frame (layout acceptance).
pub fn rows_visible_id() -> egui::Id {
    egui::Id::new("records_rows_visible")
}

/// egui temp-data key: the search banner's rect and the records pane's clip rect of the last
/// frame (absent without a banner; layout acceptance 0020 §6).
pub fn banner_rect_id() -> egui::Id {
    egui::Id::new("records_banner_rect")
}

/// egui temp-data key: height between the notes and the table (last frame).
fn post_id() -> egui::Id {
    egui::Id::new("records_post_notes")
}

/// `req` holds the one-shot requests taken from the app state: `row` = "scroll this row into
/// view" (row, centered?), `table` = the scroll offset Back restores (preferred when it lies
/// within the table), `map_focus` = the LSM Map takes keyboard focus.
pub fn show(ui: &mut egui::Ui, st: &AppState, tok: &Tokens, req: ScrollRequest) -> Vec<Action> {
    let (scroll, table_offset) = (req.row, req.table);
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
                if st.navigation().pending_target().is_some() {
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
    // 0019 §3: below its wanted height (full header + `TABLE_MIN_ROWS` rows, from the last
    // uncompacted frame) the header takes at most four text lines: one elided title line, the
    // first location line, the collapsed notes and one banner line; tooltips keep the full text.
    let sp_y = ui.spacing().item_spacing.y;
    let sp_x = ui.spacing().item_spacing.x;
    let table_need = table_need_h(tok.header_height(ui), tok.row_height(ui), sp_y, ui.spacing().scroll.allocated_width(), TABLE_MIN_ROWS);
    let full_id = full_overhead_id(doc.view_kind(), &view.title);
    let full_over = ui.ctx().data(|d| d.get_temp::<f32>(full_id));
    let compact = header_compact(ui.available_height(), full_over, table_need);
    let levels = doc.view_kind() == Some(&NodeKind::ManifestLevels);
    let map_mode = doc.map_shown();
    let mut mode_click = None;
    let title_row = |ui: &mut egui::Ui, go_back: &mut bool, mode_click: &mut Option<MapMode>| {
        // Back is visibly disabled without history (0024 §4.6).
        if st.navigation().can_go_back() {
            if btn(ui, tok, BACK_LABEL).tip(format!("Back ({})", menu::back_key(menu::KeyLabels::HOST))).clicked() {
                *go_back = true;
            }
        } else {
            disabled_btn(ui, tok, BACK_LABEL).tip_disabled(NO_PREVIOUS_VIEW);
        }
        // Table | Map of the Observed levels view (no history entry).
        if levels && !view.lsm.is_empty() {
            ui.spacing_mut().item_spacing.x = 0.0;
            for (m, label) in [(MapMode::Table, "Table"), (MapMode::Map, "Map")] {
                let on = (m == MapMode::Map) == map_mode;
                let r = if on { latched_btn(ui, tok, label) } else { btn(ui, tok, label) };
                if r.tip(if m == MapMode::Map { "Show the levels as an LSM map" } else { "Show the levels table" }).clicked() && !on {
                    *mode_click = Some(m);
                }
            }
            ui.spacing_mut().item_spacing.x = sp_x;
        }
        if let Some(s) = view.status {
            match view.status_word {
                Some(w) => chip_word(ui, tok, s, w, status_meaning(s)),
                None => chip(ui, tok, s),
            }
        }
        if !compact {
            ui.heading(&view.title);
        }
        // A view without a status of its own still names the failures inside it, e.g. the
        // WAL root "✂ 1 truncated" (details: Explanation with nothing selected).
        if let (None, Some(t)) = (view.status, view.tally) {
            // Table of lazily checked structures (0019 §1): "✖ 1 invalid · ✔ 515 present · 4
            // not checked"; "not checked" has no glyph and no status colour.
            for (i, (s, text)) in t.parts().into_iter().enumerate() {
                if i > 0 {
                    ui.label(RichText::new("·").color(tok.p.text_muted));
                }
                match s {
                    Some(s) => status_text(ui, tok, s, &text),
                    None => {
                        ui.label(RichText::new(text).color(tok.p.text));
                    }
                }
            }
        } else if view.status.is_none() {
            let tree = doc.sel_node().and_then(|i| doc.nodes().get(i));
            let fails = diagnosis::node_failures(view, tree.map(|n| n.status), tree.and_then(|n| n.diag.as_deref()));
            if let Some((s, text)) = diagnosis::failure_counts(&fails) {
                status_text(ui, tok, s, &format!("{} {text}", s.symbol()));
            }
        }
        if st.navigation().pending_target().is_some() {
            ui.spinner();
        }
        if compact {
            // The title takes what is left of the one line.
            ui.add(Elided::new(egui::Label::new(RichText::new(&view.title).heading())));
        }
    };
    if compact {
        ui.horizontal(|ui| title_row(ui, &mut go_back, &mut mode_click));
    } else {
        ui.horizontal_wrapped(|ui| title_row(ui, &mut go_back, &mut mode_click));
    }
    if let Some(m) = mode_click {
        acts.push(Action::SetMapMode(m));
    }
    // 0020 §6: the search banner has a reserved line directly below the title, outside the
    // scrolling notes, so it never scrolls away; it is elided to one line (full text in the
    // tooltip) only in the compacted header. Banner and status bar come from one state (0019 §4).
    let banner = find.banner();
    match &banner {
        Some(b) => {
            let label = egui::Label::new(RichText::new(b).small().color(tok.p.text));
            let r = if compact { ui.add(Elided::new(label)) } else { ui.add(label.wrap()) };
            ui.ctx().data_mut(|d| d.insert_temp(banner_rect_id(), [r.rect, ui.clip_rect()]));
        }
        None => ui.ctx().data_mut(|d| d.remove::<[egui::Rect; 2]>(banner_rect_id())),
    }
    // Location header: file range, stored size, contents/trailer, next structure or gap (links).
    // Compact: the first line only, elided, the others in its tooltip. Count and unit are joined
    // for the screen (0020 UI ruling 6).
    if compact && let Some(first) = view.location.first() {
        let mut label = Elided::new(egui::Label::new(RichText::new(nb_units(&first.text)).small().monospace().color(tok.p.text)).sense(egui::Sense::click()));
        if view.location.len() > 1 {
            label = label.tip(view.location.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join("\n"));
        }
        let r = ui.add(label);
        if let Some(t) = &first.link
            && r.clicked()
        {
            acts.push(Action::Navigate { target: t.node.clone(), span: t.span });
        }
    }
    for w in view.location.iter().filter(|_| !compact) {
        match &w.link {
            Some(t) => {
                let r = ui.add(egui::Label::new(RichText::new(nb_units(&w.text)).small().color(tok.p.accent_text).underline()).sense(egui::Sense::click()).wrap());
                if r.on_hover_cursor(egui::CursorIcon::PointingHand).tip("Go to this structure (Back returns)").clicked() {
                    acts.push(Action::Navigate { target: t.node.clone(), span: t.span });
                }
            }
            None => {
                ui.add(egui::Label::new(RichText::new(nb_units(&w.text)).small().monospace().color(tok.p.text)).wrap());
            }
        }
    }
    // Notes scroll in their own area when space is short, so the table keeps its header and at
    // least `TABLE_MIN_ROWS` rows (900×700 acceptance). At narrow heights the subtitle notes
    // collapse to one line; the tooltip has the full text.
    let small_h = ui.text_style_height(&egui::TextStyle::Small);
    // Natural height of the notes (and the search line) at this width; the part between the
    // notes and the table (separator, LSM boxes, column toggle) as measured last frame.
    let small = egui::TextStyle::Small.resolve(ui.style());
    let wrap_w = ui.available_width();
    let notes_h: f32 = view.notes.iter().map(|n| ui.fonts_mut(|f| f.layout(n.text.clone(), small.clone(), tok.p.text, wrap_w).size().y) + sp_y).sum::<f32>();
    let post = ui.ctx().data(|d| d.get_temp::<f32>(post_id())).unwrap_or(tok.row_height(ui) + 8.0);
    let fixed = ui.cursor().top() - top_y;
    ui.ctx().data_mut(|d| {
        d.insert_temp(overhead_id(), fixed + notes_h + post);
        // Only an uncompacted frame measures the full header (the compaction threshold).
        if !compact {
            d.insert_temp(full_id, fixed + notes_h + post);
        }
    });
    // Narrow height: the full notes would leave the table fewer than TABLE_MIN_ROWS rows.
    // The map shows the notes as the one-line summary (full text in the tooltip/Explanation).
    let narrow = compact || map_mode || ui.available_height() - notes_h - post < table_need;
    let notes_max = (ui.available_height() - post - table_need).max(small_h + 4.0);
    if narrow && !view.notes.is_empty() {
        let worst = view.notes.iter().find_map(|n| n.status);
        let col = worst.map(|s| tok.status(s)).unwrap_or(tok.p.text_muted);
        let first = view.notes.iter().find(|n| n.status.is_some()).unwrap_or(&view.notes[0]);
        let more = if view.notes.len() > 1 { format!("  (+{} more)", view.notes.len() - 1) } else { String::new() };
        let mut label = Elided::new(egui::Label::new(RichText::new(format!("{}{more}", first.text)).small().color(col)));
        if view.notes.len() > 1 {
            label = label.tip(view.notes.iter().map(|n| n.text.as_str()).collect::<Vec<_>>().join("\n"));
        }
        ui.add(label);
    }
    egui::ScrollArea::vertical().id_salt(("notes", epoch)).max_height(notes_max).auto_shrink([false, true]).show(ui, |ui| {
        for n in view.notes.iter().filter(|_| !narrow) {
            // Problem notes carry their status and use that status color.
            let col = n.status.map(|s| tok.status(s)).unwrap_or(tok.p.text_muted);
            ui.add(egui::Label::new(RichText::new(&n.text).small().color(col)).wrap());
        }
    });
    let after_notes = ui.cursor().top();
    ui.separator();
    // The LSM Map takes the whole rest of the records area (0024 §2); the table is not drawn.
    if map_mode {
        let post_now = ui.cursor().top() - after_notes;
        ui.ctx().data_mut(|d| d.insert_temp(post_id(), post_now));
        acts.extend(super::lsm_map::show(ui, st, tok, req.map_focus));
        if go_back {
            acts.push(Action::Back);
        }
        return acts;
    }
    // Columns that fit: Raw hex, then Encoding, then Len hide first; Status may become
    // icon-only; Offset keeps 10 monospace characters, 14 in tables with derived offsets
    // ("record+0x1ffff", review N4). "+N columns" brings them back.
    let mono = egui::TextStyle::Monospace.resolve(ui.style());
    let mono_w = ui.fonts_mut(|f| f.glyph_width(&mono, '0'));
    let derived = view.rows.iter().any(|r| r.field.span.is_some_and(|s| s.space != crate::model::Space::File));
    let offset_chars = if derived { 14.0 } else { 10.0 };
    // Status keeps its widest word ("Not checked", "✂ Truncated" …) at every zoom (C2 GUI-1).
    let status_w = status_column_width(widest_status_word(ui));
    let width_of = |r: ColumnRole| match r {
        ColumnRole::Offset => column_width(r).max(offset_chars * mono_w + 8.0),
        ColumnRole::Status => status_w,
        _ => column_width(r),
    };
    let roles: Vec<ColumnRole> = view.columns.iter().map(|c| c.role).collect();
    let avail_cols = ui.available_width() - ui.spacing().scroll.allocated_width();
    let plan = plan_columns(&roles, avail_cols, st.prefs().all_columns(), &width_of, ui.spacing().item_spacing.x);
    let would_hide = plan_columns(&roles, avail_cols, false, &width_of, ui.spacing().item_spacing.x).hidden;
    if would_hide > 0 {
        let label = if st.prefs().all_columns() { "Fewer columns".to_string() } else { format!("+{} columns", plan.hidden) };
        let tip = if st.prefs().all_columns() {
            "Hide Raw hex / Encoding / Len again so the table fits (Reset View also restores this)"
        } else {
            "Show the columns hidden for lack of width (the table then scrolls horizontally)"
        };
        if ui.small_button(label).tip(tip).clicked() {
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
    // Full body rows the table shows in this frame (zoom acceptance, 0019 §3).
    let hbar = if need > ui.available_width() { ui.spacing().scroll.allocated_width() } else { 0.0 };
    let sp_y = ui.spacing().item_spacing.y;
    let body_rows = ((ui.available_height() - header_h - sp_y - hbar) / (row_h + sp_y)).floor().max(0.0);
    ui.ctx().data_mut(|d| d.insert_temp(rows_visible_id(), body_rows));
    let mut scrolled = None;
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
        // Back: the saved scroll offset when it still lies within this table; otherwise the row
        // request (select + scroll into view) is the fallback.
        let max_off = (nrows as f32 * (row_h + sp_y) - body_rows * (row_h + sp_y)).max(0.0);
        match table_offset.filter(|o| *o <= max_off + 0.5) {
            Some(o) => tb = tb.vertical_scroll_offset(o),
            None => {
                if let Some((r, center)) = scroll.filter(|(r, _)| *r < nrows) {
                    tb = tb.scroll_to_row(r, center.then_some(egui::Align::Center));
                }
            }
        }
        let out = tb
            .header(header_h, |mut h| {
                for &ci in &order {
                    let mut elided = false;
                    let mut visible_rect = egui::Rect::NOTHING;
                    let (_, resp) = h.col(|ui| {
                        let r = ui.max_rect().expand2(egui::vec2(spacing / 2.0, 2.0));
                        tok.paint_header(ui.painter(), r);
                        // A header narrower than its title — by column width or cut by the table's
                        // horizontal scroll viewport — ends in "…" or shows nothing; never a
                        // clipped glyph.
                        let title = view.columns[ci].title;
                        let text = RichText::new(title).font(header_font.clone()).strong().color(tok.p.header_fg);
                        let visible = ui.max_rect().intersect(ui.clip_rect());
                        visible_rect = visible;
                        let ell = ui.fonts_mut(|f| f.layout_no_wrap("…".into(), header_font.clone(), tok.p.header_fg).size().x);
                        if visible.width() >= ui.max_rect().width() - 0.5 {
                            elided = super::was_elided(&ui.add(Elided::quiet(egui::Label::new(text))));
                        } else if visible.width() >= ell + 2.0 {
                            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(visible).layout(egui::Layout::left_to_right(egui::Align::Center)));
                            elided = super::was_elided(&child.add(Elided::quiet(egui::Label::new(text))));
                        } else {
                            elided = true;
                        }
                    });
                    // 0030 B-3: one tooltip per header cell, through the shared policy.
                    if resp.hovered()
                        && visible_rect.width() > 0.0
                        && let Some(t) = explanation::header_tip(view, ci, elided)
                    {
                        let _ = resp.tip_at(visible_rect, t);
                    }
                }
            })
            .body(|body| {
                body.rows(row_h, nrows, |mut row| {
                    let i = row.index();
                    let r = &view.rows[i];
                    let is_sel = sel_row == Some(i);
                    row.set_selected(is_sel);
                    // The cell under the pointer (visible rect, column, shown text, elided) for the
                    // row's one tooltip; only that cell is recorded (architect N6).
                    let mut hit: Option<(egui::Rect, usize, String, bool)> = None;
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
                            // The label has no tooltip of its own; the row shows one (0030 B-2).
                            let lr = ui.add(Elided::quiet(egui::Label::new(t).selectable(false)));
                            let vis = ui.max_rect().intersect(ui.clip_rect());
                            if ui.ctx().pointer_hover_pos().is_some_and(|p| vis.contains(p)) {
                                hit = Some((vis, ci, cell.to_string(), super::was_elided(&lr)));
                            }
                        });
                    }
                    let resp = row.response();
                    // 0030 B-2: the row is the one hover target; the tooltip of the cell under the
                    // pointer (shared policy `explanation::cell_tip`), anchored at that cell, so
                    // moving along the row updates it in place and it follows the hover, never the
                    // selected row.
                    if resp.hovered()
                        && let Some(p) = resp.hover_pos()
                        && let Some((rect, ci, shown, elided)) = hit.as_ref().filter(|c| c.0.contains(p))
                        && let Some(t) = explanation::cell_tip(view, i, *ci, shown, *elided)
                    {
                        let _ = resp.clone().tip_at(*rect, t);
                    }
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
                        for (k, label, enabled) in [
                            (CopyKind::Value, "Copy value", true),
                            (CopyKind::Key, "Copy key", has_key),
                            (CopyKind::Hex, "Copy hex (max 64 KiB)", has_span),
                            (CopyKind::Offset, "Copy offset", has_span),
                            (CopyKind::RowTsv, "Copy row (TSV)", true),
                        ] {
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
        // The table's scroll offset goes to the session when it changes (what Back restores).
        let y = out.state.offset.y;
        if (y - st.session().table_scroll()).abs() > 0.5 {
            scrolled = Some(y);
        }
    });
    if let Some(y) = scrolled {
        acts.push(Action::SetTableScroll(y));
    }
    if let Some((i, k)) = copy_act {
        acts.push(Action::CopyRow(i, k));
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
