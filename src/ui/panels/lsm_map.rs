//! LSM Map painter (0024 §2–4, 0026 §1–§6): the strip (CF selector · note · navigation
//! controls only while the map overflows), the legend and lane caption lines, the key axis row,
//! the pinned WAL band, and the scroll region with one frame per level (title bar, lane bands,
//! boxes). Geometry, titles, axis ends, empty-lane sentences and the fit decision come from
//! `app::lsm_map` (egui-free); facts from `app::map_facts`. This file only measures, paints,
//! hit-tests and turns input into `Action`s. Map pan/zoom are map-local (`Action::MapView` /
//! `MapZoom`), never the app zoom.

use super::{Elided, TipExt, fact_tip_body};
use crate::app::lsm_map::{self, BoxKind, Geometry, LaneTier, MapBox, MapLayout, MapPick, MapRow, MeasureInput, TitleRole, short_size};
use crate::app::map_facts;
use crate::app::{Action, AppState, MapZoom};
use crate::model::{LsmCf, LsmFile, LsmInventory, Status, WalFile, WalInventory, WalSummary, files_word};
use crate::ui::layout::LAYOUT;
use crate::ui::theme::Tokens;
use eframe::egui::{self, RichText};

/// Focus id of the map (keys.rs routes arrows/Home/End/+ - 0 c to it while focused).
pub fn map_id() -> egui::Id {
    egui::Id::new("lsm_map")
}

/// The map is shown and has keyboard focus, with no modal window over it (architect N4: the
/// one check for the keys, the status hint, the focus rings and the LSM Map entry).
pub fn has_focus(ctx: &egui::Context, st: &AppState) -> bool {
    st.session().map_shown() && ctx.memory(|m| m.has_focus(map_id())) && !st.prefs().modal_open()
}

/// Status-bar hint while the map has keyboard focus (0030 A2-2).
pub const FOCUS_HINT: &str = "Map: arrows select · Up from L0 reaches the WALs · Enter opens · Esc leaves · 0 Fit · C Center";

/// The widest map-zoom label (`ZOOM_CAP` = ×100000): the label's reserved width.
const ZOOM_LABEL_WIDEST: &str = "×100000";

/// egui temp-data key: the strip's `+` button rect of the last frame (layout test).
pub fn plus_rect_id() -> egui::Id {
    egui::Id::new("lsm_map_plus_rect")
}

/// The full legend gets its own line only above this map height (900×560 keeps every row).
const LEGEND_LINE_MIN_MAP_H: f32 = 260.0;

/// The vertical regions of the map widget (screen coordinates).
struct Regions {
    axis: egui::Rect,
    band: egui::Rect,
    /// The scroll region (level frames), its frame column (without the scrollbar) and the box
    /// area (frame column minus the frame padding).
    region: egui::Rect,
    frames: egui::Rect,
    area: egui::Rect,
    scrollbar: egui::Rect,
}

/// What a click/hover in the WAL band hits.
#[derive(Clone, Copy, PartialEq)]
enum BandHit {
    Wal(u64),
    Older(usize),
}

/// Renders the map of the Observed levels view; returns the requested actions. `take_focus`:
/// the map widget takes keyboard focus now (it was opened by Shift+⌘M, the toolbar or the menu).
///
/// Keyboard: the map is one focusable widget in the Tab order (click / drag / Tab / entry focus).
/// While it has focus, a focus-lock filter keeps the arrow keys from moving egui's focus, and
/// `ui::keys` (`Keys::map_focused`) maps arrows/Home/End/+ - 0 c/Enter to the map.
pub fn show(ui: &mut egui::Ui, st: &AppState, tok: &Tokens, take_focus: bool) -> Vec<Action> {
    let mut acts = Vec::new();
    let doc = st.session();
    let Some(view) = doc.view() else { return acts };
    let inv = &view.lsm;
    let map = doc.map();
    if inv.is_empty() {
        let why = match doc.lsm_map_availability() {
            Err(r) => r.0,
            Ok(_) => "This MANIFEST lists no live SST files.".to_string(),
        };
        ui.add(egui::Label::new(RichText::new(why).color(tok.p.text_muted)).wrap());
        return acts;
    }
    let cf = map.shown_cf(inv);
    let c = cf.and_then(|id| inv.cf(id));
    // D3 S6: until the first measure of this map has been decided, no controls and no input
    // capture (the controls can then only appear once, never flash and vanish).
    let nav = map.navigable() && map.measure().is_some();
    strip(ui, st, tok, inv, cf, nav, &mut acts);
    let rect = ui.available_rect_before_wrap();
    if rect.width() < 40.0 || rect.height() < 20.0 {
        return acts;
    }
    let sense = if nav { egui::Sense::click_and_drag() } else { egui::Sense::click() };
    let resp = ui.interact(rect, map_id(), sense | egui::Sense::FOCUSABLE);
    if take_focus || resp.clicked() || resp.drag_started() {
        resp.request_focus();
    }
    if resp.has_focus() {
        // Arrows belong to the map (not egui's focus navigation); Tab and Esc still leave it.
        ui.memory_mut(|m| m.set_focus_lock_filter(map_id(), egui::EventFilter { tab: false, horizontal_arrows: true, vertical_arrows: true, escape: false }));
    }
    let small = ui.text_style_height(&egui::TextStyle::Small);
    let mono = ui.text_style_height(&egui::TextStyle::Monospace);
    let body = ui.text_style_height(&egui::TextStyle::Body);
    // 0030 A1-2: lane heights per tier (Full: number, size, key range; Compact: one line; Bar:
    // no text), all from the text-style heights.
    let tier_h = tier_heights(mono, small);
    let wals = doc.wal_inventory();
    let chips = rect.height() < LAYOUT.map_chips_below_h;
    let band_h = band_height(chips, wals, body, small, wal_box_h(ui, inv.cfs.len() > 1));
    let pad = LAYOUT.map_frame_pad;
    let sb_w = ui.spacing().scroll.bar_width;
    // 0030 A4-1: the axis row always reserves the overview track, so the region height never
    // depends on the navigation mode.
    let axis = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), small + 4.0 + AXIS_TRACK_H));
    let band = egui::Rect::from_min_size(egui::pos2(rect.left(), axis.bottom()), egui::vec2(rect.width(), band_h));
    let region = egui::Rect::from_min_max(egui::pos2(rect.left(), band.bottom()), rect.max);
    // The scrollbar width is always subtracted (§6.1), so its appearance cannot change the fit.
    let frames = egui::Rect::from_min_max(region.min, egui::pos2(region.right() - sb_w, region.bottom()));
    let area = egui::Rect::from_min_max(egui::pos2(frames.left() + pad, region.top()), egui::pos2(frames.right() - pad, region.bottom()));
    let scrollbar = egui::Rect::from_min_max(egui::pos2(frames.right(), region.top()), region.max);
    let r = Regions { axis, band, region, frames, area, scrollbar };
    if area.width() < 10.0 || region.height() < 4.0 {
        paint_band(ui, st, tok, inv, cf, &r, chips, &resp, &mut acts);
        return acts;
    }
    let geo_of = |t: LaneTier| Geometry {
        width: area.width(),
        lane_h: tier_h[t.index()],
        lane_gap: LAYOUT.map_lane_gap,
        title_h: ui.text_style_height(&egui::TextStyle::Heading),
        pad,
        row_gap: LAYOUT.map_row_gap,
        min_w: LAYOUT.map_min_box_w,
        dense_lane_h: tier_h[LaneTier::Compact.index()],
        visible: [0.0, 0.0, area.width(), region.height()],
    };
    // §6.1: the fit measure of this frame at Full / ×1 (decided by the state only on a real
    // change; the tier never feeds the Fits / Overflow decision, 0030 A1-8).
    let mono_font = egui::TextStyle::Monospace.resolve(ui.style());
    let readable_w = ui.fonts_mut(|f| f.layout_no_wrap("000000".into(), mono_font, tok.p.text).size().x) + LAYOUT.map_readable_pad;
    let m = lsm_map::measure(inv, cf, &geo_of(LaneTier::Full), MeasureInput { view_h: region.height(), readable_w, app_zoom: ui.ctx().zoom_factor() });
    // The scene heights per tier for Fit (sent before the measure, which may apply Fit);
    // recomputed only when the measure inputs changed (S5: no passes over the files when idle).
    let heights = match map.heights() {
        Some(h) if map.measure() == Some(m) => h,
        _ => LaneTier::ALL.map(|t| lsm_map::scene_height(inv, cf, &geo_of(t))),
    };
    if map.heights() != Some(heights) {
        acts.push(Action::MapFitHeights(heights));
    }
    if map.measure() != Some(m) {
        acts.push(Action::MapMeasure(m));
    }
    let scale = c.map(|c| c.levels.iter().map(|l| l.scale).max().unwrap_or(1)).unwrap_or(1);
    let zmax = lsm_map::max_zoom(scale, area.width(), LAYOUT.map_slot_max_pt);
    if (zmax - map.zoom_max()).abs() > 1e-3 {
        // `+` / MapZoom::In use the same bound from the next action on.
        acts.push(Action::MapMaxZoom(zmax));
    }
    // This frame's pan/zoom: the state, then this frame's input (navigation mode only, pointer
    // over the scene; §6.6), applied now and sent as MapView.
    let (mut pan, mut zoom) = (map.pan(), map.zoom().min(zmax));
    let mut changed = (map.zoom() - zoom).abs() > 1e-6;
    let pointer = resp.hover_pos();
    let press = ui.input(|i| i.pointer.press_origin());
    // A wheel / pinch zoom-in of this frame is drawn at Full at once (as the state will be).
    let tier_of = |z: f32| lsm_map::tier_after_zoom(map.tier(), map.zoom(), z);
    let scene_of = |z: f32| heights[tier_of(z).index()];
    let mut max_y = (scene_of(zoom) - region.height()).max(0.0);
    if nav {
        if let Some(p) = pointer.filter(|p| region.contains(*p)) {
            let (scroll, zd) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta()));
            if (zd - 1.0).abs() > 1e-4 {
                // ⌘/Ctrl+wheel and pinch: map zoom about the pointer (the app zoom is untouched).
                let at = (p.x - area.left()) / area.width();
                (pan.0, zoom) = lsm_map::zoom_about(pan.0, zoom, zd, at, zmax);
                max_y = (scene_of(zoom) - region.height()).max(0.0);
                changed = true;
            } else if scroll != egui::Vec2::ZERO {
                // Wheel = vertical scroll; Shift+wheel / a horizontal swipe = pan.
                pan.0 = lsm_map::clamp_pan_x(pan.0 - scroll.x / (area.width() * zoom), zoom);
                pan.1 = (pan.1 - scroll.y).clamp(0.0, max_y);
                changed = true;
            }
        }
        if resp.dragged() {
            let d = resp.drag_delta();
            match press {
                // The axis overview (§6.7): the visible window follows the pointer.
                Some(o) if r.axis.contains(o) => {
                    if let Some(p) = resp.interact_pointer_pos() {
                        let at = ((p.x - area.left()) / area.width()).clamp(0.0, 1.0);
                        pan.0 = lsm_map::clamp_pan_x(at - 0.5 / zoom, zoom);
                    }
                }
                // The scrollbar: the thumb follows the drag.
                Some(o) if r.scrollbar.contains(o) => {
                    let k = if region.height() > 0.0 { scene_of(zoom) / region.height() } else { 1.0 };
                    pan.1 = (pan.1 + d.y * k).clamp(0.0, max_y);
                }
                Some(o) if r.band.contains(o) => {}
                _ => {
                    pan.0 = lsm_map::clamp_pan_x(pan.0 - d.x / (area.width() * zoom), zoom);
                    pan.1 = (pan.1 - d.y).clamp(0.0, max_y);
                }
            }
            changed = true;
        }
        if resp.clicked()
            && let Some(p) = resp.interact_pointer_pos().filter(|p| r.axis.contains(*p))
        {
            let at = ((p.x - area.left()) / area.width()).clamp(0.0, 1.0);
            pan.0 = lsm_map::clamp_pan_x(at - 0.5 / zoom, zoom);
            changed = true;
        }
    }
    // 0030 A3-5: an arrow move onto a file merged into a density bar zooms in until that file is
    // readable (capped at the maximum) and pans to it, instead of dropping the reveal.
    if map.reveal()
        && let (Some(n), Some(c)) = (map.focus(), c)
        && let Some((a, b)) = c.levels.iter().find_map(|l| l.files.iter().find(|f| f.number == n && f.lane < lsm_map::MAX_LANES).map(|f| lsm_map::fraction(l, f)))
        && let Some((px, z)) = lsm_map::reveal_zoom(a, b, area.width(), zoom, readable_w, LAYOUT.map_min_box_w, zmax)
    {
        zoom = z;
        pan.0 = px;
        max_y = (scene_of(zoom) - region.height()).max(0.0);
        changed = true;
    }
    let geo = geo_of(tier_of(zoom));
    let mut lay = lsm_map::layout(inv, cf, pan, zoom, &geo);
    // Vertical bounds, and the one-shot reveal of the focused box (x via `reveal_x`, y here).
    let mut want_y = pan.1.min(max_y);
    if map.reveal() {
        let all = Geometry { visible: [geo.visible[0], f32::MIN, geo.visible[2], f32::MAX], ..geo };
        let full = lsm_map::layout(inv, cf, pan, zoom, &all);
        match map.focus().and_then(|n| full.boxes.iter().find(|b| matches!(b.kind, BoxKind::File { number, .. } if number == n))) {
            Some(b) => {
                let row = full.rows.iter().find(|r| r.level == b.level);
                // Keep the frame's title visible above the box.
                let top = row.map(|r| r.y0).unwrap_or(b.rect[1]);
                if top < 0.0 {
                    want_y = (pan.1 + top).max(0.0);
                } else if b.rect[3] > region.height() {
                    want_y = (pan.1 + b.rect[3] - region.height() + LAYOUT.map_row_gap).min(max_y);
                }
                changed = true;
            }
            // Not laid out (hidden lane, over the cap): drop the reveal.
            None => acts.push(Action::MapRevealDone),
        }
    }
    if (want_y - pan.1).abs() > 0.01 {
        pan.1 = want_y;
        lay = lsm_map::layout(inv, cf, pan, zoom, &geo);
    }
    // A strip command of this frame (Center Fit − + , CF choice) wins over the painter's view.
    let strip_cmd = acts.iter().any(|a| matches!(a, Action::MapZoom(_) | Action::MapSelectCf(_)));
    if changed && !strip_cmd {
        acts.push(Action::MapView { pan, zoom });
    }
    let tier = tier_of(zoom);
    paint(ui, st, tok, c, &lay, &r, &geo, (pan, zoom), nav, scene_of(zoom), tier, readable_w);
    paint_band(ui, st, tok, inv, cf, &r, chips, &resp, &mut acts);
    // 0030 A2-1: the whole map (axis, WAL band, levels) is outlined while it has keyboard focus.
    if has_focus(ui.ctx(), st) {
        let all = r.axis.union(r.band).union(r.region);
        ui.painter().with_clip_rect(all.intersect(ui.clip_rect())).rect_stroke(all, tok.w.rounding, egui::Stroke::new(2.0, tok.p.focus), egui::StrokeKind::Inside);
    }
    // 0030 A4: the axis overview is a scroll thumb: cursors and its tooltip (key rank, or file
    // number order when the comparator is not supported).
    if nav
        && zoom > lsm_map::ZOOM_MIN
        && let Some(p) = pointer.filter(|p| r.axis.contains(*p))
    {
        let dragging = resp.dragged() && press.is_some_and(|o| r.axis.contains(o));
        let thumb = axis_thumb(&r, pan.0, zoom);
        ui.ctx().set_cursor_icon(if dragging {
            egui::CursorIcon::Grabbing
        } else if thumb.x_range().contains(p.x) {
            egui::CursorIcon::Grab
        } else {
            egui::CursorIcon::PointingHand
        });
        let order = if c.is_some_and(|c| c.order.ordered()) { "key rank, not to scale" } else { "file number order" };
        let _ = resp.clone().tip_at(egui::Rect::from_x_y_ranges(thumb.x_range(), r.axis.y_range()), format!("Visible part of the key range — drag to pan ({order})"));
    }
    // Hit test of the scene (topmost last); the band handled its own.
    let in_region = |p: &egui::Pos2| r.region.contains(*p);
    let hit = |p: egui::Pos2| lay.boxes.iter().rev().find(|b| to_rect(&r, b).contains(p));
    let title_hit = |p: egui::Pos2| lay.rows.iter().find(|row| title_rect(&r, row, &geo).contains(p) || pinned_title_rect(&r, row, &geo).is_some_and(|t| t.contains(p)));
    if let Some(p) = pointer.filter(in_region) {
        if let Some(b) = hit(p) {
            box_tip(&resp, tok, c, b, to_rect(&r, b).intersect(r.region));
            if matches!(b.kind, BoxKind::Density { .. }) && nav {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ZoomIn);
            }
            if resp.double_clicked() {
                if let BoxKind::File { row, on_disk, .. } = b.kind {
                    // A missing file is only selected; the Explanation says it cannot be opened.
                    acts.push(if on_disk { Action::FollowRow(row) } else { Action::SelectRow(row) });
                }
            } else if resp.clicked() {
                match b.kind {
                    BoxKind::File { row, .. } => acts.push(Action::SelectRow(row)),
                    BoxKind::Density { count, .. } if nav => {
                        // 0030 A3-3: zoom into the bar until its files are readable (at least ×2,
                        // capped), the bar centred; selects nothing.
                        let (px, z) = lsm_map::zoom_to_bar(pan.0, zoom, (b.rect[0], b.rect[2]), count, area.width(), readable_w, zmax);
                        acts.push(Action::MapView { pan: (px, pan.1), zoom: z });
                    }
                    BoxKind::Density { .. } => {}
                    BoxKind::Summary { .. } => acts.push(Action::SetMapMode(crate::app::MapMode::Table)),
                }
            }
        } else if let (Some(row), Some(c)) = (title_hit(p), c) {
            if let Some(l) = c.levels.iter().find(|l| l.level == row.level).filter(|_| super::tooltips_on(ui.ctx())) {
                let facts = map_facts::level_facts(c, l);
                let title = map_facts::level_title(c, l);
                let _ = resp.clone().tip_at_ui(title_rect(&r, row, &geo).intersect(r.region), fact_tip_body(tok, &title, &facts, Some("Click to show the level in the Explanation")));
            }
            if resp.clicked() {
                acts.push(Action::SelectMapPick(MapPick::Level(row.level)));
            }
        } else if let Some(c) = c.filter(|_| super::tooltips_on(ui.ctx())) {
            // Empty lane space (§2.5): which files cover this position, bounded, no keys; built only
            // when a tooltip can show (architect N7).
            let full = area.width() * zoom.max(1.0);
            let x = (p.x - area.left() + pan.0 * full) / full;
            for (li, row) in lay.rows.iter().enumerate() {
                if let Some(lane) = row.lane_at(p.y - region.top(), &geo).filter(|_| row.files > 0 && (row.hidden == 0 || row.lane_at(p.y - region.top(), &geo) != Some(row.lane_rows - 1))) {
                    if let Some(t) = lsm_map::empty_lane_text(c, li, lane, x) {
                        let band = egui::Rect::from_min_size(egui::pos2(area.left(), region.top() + row.lane_y(lane, &geo)), egui::vec2(area.width(), row.lane_h)).intersect(r.region);
                        let _ = resp.clone().tip_at(band, t);
                    }
                    break;
                }
            }
        }
    }
    acts
}

/// Height of the axis row's overview track (0030 A4-1), reserved in both modes.
const AXIS_TRACK_H: f32 = 6.0;

/// The axis overview thumb (0030 A4-3): x from the pan, width area/zoom (at least 16 pt), the
/// track's 6 pt at the bottom of the axis row.
fn axis_thumb(r: &Regions, pan_x: f32, zoom: f32) -> egui::Rect {
    let w = (r.area.width() / zoom.max(lsm_map::ZOOM_MIN)).max(16.0).min(r.area.width());
    let x0 = (r.area.left() + pan_x * r.area.width()).min(r.area.right() - w);
    egui::Rect::from_min_size(egui::pos2(x0, r.axis.bottom() - AXIS_TRACK_H), egui::vec2(w, AXIS_TRACK_H))
}

/// The count text of a density bar `w` points wide inside its stroke (0030 A3-2): "600 files",
/// else "600", else none (the tooltip and the frame title still give it).
pub fn density_label(count: usize, w: f32, width: &dyn Fn(&str) -> f32) -> Option<String> {
    let long = files_word(count);
    let short = count.to_string();
    if width(&long) + 8.0 <= w {
        Some(long)
    } else if width(&short) + 6.0 <= w {
        Some(short)
    } else {
        None
    }
}

/// Lane heights per tier (0030 A1-2): Full = Monospace + 2 Small + 6, Compact = Monospace + 6,
/// Bar = max(`map_lane_min_h`, 0.6 Small).
pub fn tier_heights(mono: f32, small: f32) -> [f32; 3] {
    [mono + 2.0 * small + 6.0, mono + 6.0, LAYOUT.map_lane_min_h.max(small * 0.6)]
}

/// Height of a WAL box: name and state lines, plus the membership line when the database has
/// more than one CF (a CF to choose; §4.3 point 3).
fn wal_box_h(ui: &egui::Ui, multi_cf: bool) -> f32 {
    ui.text_style_height(&egui::TextStyle::Monospace) + ui.text_style_height(&egui::TextStyle::Small) * if multi_cf { 2.0 } else { 1.0 } + 6.0
}

/// Height of the WAL band: chips form one line; otherwise title line, boxes, separator gaps,
/// separator and caption; a one-line message without WAL boxes.
fn band_height(chips: bool, wals: Option<&WalInventory>, body: f32, small: f32, lane_h: f32) -> f32 {
    let sep = 2.0 * LAYOUT.map_wal_sep_gap + 1.0 + small;
    if chips {
        return body + 6.0;
    }
    match wals {
        Some(w) if !w.files.is_empty() => body + 4.0 + WAL_TITLE_GAP + lane_h + sep,
        _ => body + 4.0 + sep,
    }
}

/// Box rect on screen (box-area x, region y).
fn to_rect(r: &Regions, b: &MapBox) -> egui::Rect {
    let o = egui::pos2(r.area.left(), r.region.top());
    egui::Rect::from_min_max(o + egui::vec2(b.rect[0], b.rect[1]), o + egui::vec2(b.rect[2], b.rect[3]))
}

/// The frame of a level on screen.
fn frame_rect(r: &Regions, row: &MapRow) -> egui::Rect {
    egui::Rect::from_min_max(egui::pos2(r.frames.left(), r.region.top() + row.y0), egui::pos2(r.frames.right(), r.region.top() + row.y1))
}

/// The title bar of a level (where it scrolls with the frame).
fn title_rect(r: &Regions, row: &MapRow, g: &Geometry) -> egui::Rect {
    egui::Rect::from_min_size(egui::pos2(r.frames.left(), r.region.top() + row.title_y), egui::vec2(r.frames.width(), g.title_h))
}

/// The pinned title bar (§1.6): the frame's top has scrolled above the region while the frame
/// is still visible; the bar sticks to the region top (never below the frame's last lane).
fn pinned_title_rect(r: &Regions, row: &MapRow, g: &Geometry) -> Option<egui::Rect> {
    if row.title_y >= 0.0 || row.y1 <= g.title_h + g.pad {
        return None;
    }
    let y = (row.y1 - g.title_h - g.pad).min(0.0);
    Some(egui::Rect::from_min_size(egui::pos2(r.frames.left(), r.region.top() + y.max(0.0)), egui::vec2(r.frames.width(), g.title_h + g.pad)))
}

/// Tooltip of a box: the SST facts (≤ 9) and the open hint; density / summary sentences.
fn box_tip(resp: &egui::Response, tok: &Tokens, c: Option<&LsmCf>, b: &MapBox, at: egui::Rect) {
    // N3: nothing is built while tooltips are off.
    if !super::tooltips_on(&resp.ctx) {
        return;
    }
    match b.kind {
        BoxKind::File { li, fi, .. } => {
            let Some((l, f)) = c.and_then(|c| c.levels.get(li).and_then(|l| l.files.get(fi).map(|f| (l, f)))) else { return };
            let Some(c) = c else { return };
            let facts = map_facts::sst_facts(c, l, f);
            let hint = map_facts::sst_hint(f);
            let _ = resp.clone().tip_at_ui(at, fact_tip_body(tok, &f.name(), &facts, Some(&hint)));
        }
        BoxKind::Density { count, bytes } => {
            let _ = resp.clone().tip_at(at, format!("{} · {} — click to zoom to these files", files_word(count), short_size(bytes)));
        }
        BoxKind::Summary { count } => {
            let _ = resp.clone().tip_at(at, format!("+{count} overlapping files — see Table"));
        }
    }
}

/// The strip: CF selector · note · (navigation mode only) Center Fit − + ×N; then the full
/// legend line when it does not fit in the strip, and the lane caption (§2.3).
fn strip(ui: &mut egui::Ui, st: &AppState, tok: &Tokens, inv: &LsmInventory, cf: Option<u32>, nav: bool, acts: &mut Vec<Action>) {
    let map = st.session().map();
    let label = |c: &crate::model::LsmCf| format!("CF {} {} · {} · {}", c.id, c.name, files_word(c.files()), short_size(c.bytes()));
    let order = lsm_map::order_of(inv, cf);
    let (strip_w, map_h) = (ui.available_width(), ui.available_height());
    let mut below: Option<String> = None;
    ui.horizontal(|ui| {
        // The strip keeps one row height whether or not the controls show (§6.3).
        ui.set_min_height(ui.spacing().interact_size.y);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if nav {
                // Right to left: ×N + − Fit Center (reads Center Fit − + ×N). Fixed label width
                // (the widest value, "×100000") so the buttons never move while it grows.
                let mono = egui::TextStyle::Monospace.resolve(ui.style());
                let label_w = ui.fonts_mut(|f| f.layout_no_wrap(ZOOM_LABEL_WIDEST.into(), mono, tok.p.text).size().x);
                let h = ui.spacing().interact_size.y;
                ui.add_sized([label_w, h], egui::Label::new(RichText::new(format!("×{}", zoom_text(map.zoom()))).monospace()))
                    .tip("Map zoom: key-axis magnification (×1 = Fit width). Fit also reduces lane detail to show every level. The app zoom % is unchanged.");
                let plus = super::btn(ui, tok, "+");
                ui.ctx().data_mut(|d| d.insert_temp(plus_rect_id(), plus.rect));
                if plus.tip("Map zoom in (+ on the map; ⌘/Ctrl+wheel)").clicked() {
                    acts.push(Action::MapZoom(MapZoom::In));
                }
                if super::btn(ui, tok, "−").tip("Map zoom out (- on the map)").clicked() {
                    acts.push(Action::MapZoom(MapZoom::Out));
                }
                if super::btn(ui, tok, "Fit").tip("Fit: show every level and the whole key range (0)").clicked() {
                    acts.push(Action::MapZoom(MapZoom::Fit));
                }
                if super::btn(ui, tok, "Center").tip("Center: keep this zoom, centre the map (C)").clicked() {
                    acts.push(Action::MapZoom(MapZoom::Center));
                }
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                let shown = cf.and_then(|c| inv.cf(c));
                let text = shown.map(label).unwrap_or_default();
                if inv.cfs.len() > 1 {
                    let w = (ui.available_width() * 0.45).clamp(80.0, 320.0);
                    egui::ComboBox::from_id_salt("lsm_map_cf").selected_text(text).width(w).wrap_mode(egui::TextWrapMode::Truncate).show_ui(ui, |ui| {
                        for c in &inv.cfs {
                            if ui.selectable_label(Some(c.id) == cf, label(c)).clicked() {
                                acts.push(Action::MapSelectCf(c.id));
                            }
                        }
                    });
                } else {
                    ui.add(Elided::new(egui::Label::new(RichText::new(text).color(tok.p.text))));
                }
                let note = map.note().map(|n| n.to_string()).or_else(|| inv.truncated.then(|| format!("⚠ Listing truncated at {} live files", crate::limits::MAX_LIVE_FILES)));
                if let Some(n) = note {
                    super::status_text(ui, tok, Status::Unknown, &n);
                }
                let avail = ui.available_width();
                let full = order.legend();
                let full_w = ui.fonts_mut(|f| f.layout_no_wrap(full.clone(), egui::TextStyle::Small.resolve(ui.style()), tok.p.text).size().x) + 8.0;
                if avail > full_w {
                    ui.add(Elided::new(egui::Label::new(RichText::new(&full).small().color(tok.p.text_muted))));
                } else {
                    // Full sentence on its own line below the strip when it fits there; otherwise
                    // the short form marked with `…` (D4: never a silently cut sentence), the full
                    // one in its tooltip and the Explanation.
                    below = (strip_w > full_w && map_h > LEGEND_LINE_MIN_MAP_H).then(|| full.clone());
                    if below.is_none() && avail > 120.0 {
                        ui.add(Elided::new(egui::Label::new(RichText::new(lsm_map::short_legend_cut(&order)).small().color(tok.p.text_muted))).tip(full));
                    }
                }
            });
        });
    });
    if let Some(l) = below {
        ui.add(Elided::new(egui::Label::new(RichText::new(l).small().color(tok.p.text_muted))));
    }
    if let Some(cap) = cf.and_then(|c| inv.cf(c)).and_then(lsm_map::lane_caption) {
        ui.add(Elided::new(egui::Label::new(RichText::new(cap).small().color(tok.p.text_muted))));
    }
}

/// "2.5", "12", "1500" (the map zoom after ×).
fn zoom_text(z: f32) -> String {
    if z < 10.0 { format!("{:.1}", z).trim_end_matches(".0").to_string() } else { format!("{:.0}", z) }
}

/// A dashed line.
fn dashed(p: &egui::Painter, a: egui::Pos2, b: egui::Pos2, stroke: egui::Stroke) {
    p.extend(egui::Shape::dashed_line(&[a, b], stroke, 4.0, 3.0));
}

/// Axis row, level frames (title bars, lane bands), boxes, range guides, pinned titles and the
/// scrollbar, each clipped to its region.
#[allow(clippy::too_many_arguments)]
fn paint(
    ui: &egui::Ui,
    st: &AppState,
    tok: &Tokens,
    c: Option<&LsmCf>,
    lay: &MapLayout,
    r: &Regions,
    g: &Geometry,
    (pan, zoom): ((f32, f32), f32),
    nav: bool,
    scene_h: f32,
    tier: LaneTier,
    readable_w: f32,
) {
    let p = tok.p;
    let clip = ui.clip_rect();
    let all = r.axis.union(r.band).union(r.region);
    ui.painter().with_clip_rect(all.intersect(clip)).rect_filled(all, 0.0, p.data);
    let small = egui::TextStyle::Small.resolve(ui.style());
    let mono = egui::TextStyle::Monospace.resolve(ui.style());
    let body = egui::TextStyle::Heading.resolve(ui.style()); // the level id (D2: Heading)
    // ---- axis row (§2.2): observed end keys (≤ 30 % each), the middle text, the overview.
    let axp = ui.painter().with_clip_rect(r.axis.intersect(clip));
    let order = c.map(|c| c.order.clone()).unwrap_or_default();
    let cy = r.axis.top() + (r.axis.height() - AXIS_TRACK_H) / 2.0;
    let mid = lsm_map::axis_middle(&order);
    let width = r.area.width();
    let mid_w = ui.fonts_mut(|f| f.layout_no_wrap(mid.to_string(), small.clone(), p.text_muted).size().x);
    let mut show_mid = true;
    if let Some((a, b)) = c.and_then(lsm_map::axis_ends) {
        let job = |t: String, w: f32| egui::text::LayoutJob {
            wrap: egui::text::TextWrapping::truncate_at_width(w),
            ..egui::text::LayoutJob::simple_singleline(t, egui::FontId::monospace(small.size), p.text_muted)
        };
        let (mut ga, mut gb) = (ui.fonts_mut(|f| f.layout_job(job(a.clone(), width * 0.3))), ui.fonts_mut(|f| f.layout_job(job(b.clone(), width * 0.3))));
        // D6: never overlap at narrow widths: drop the middle label first (the strip and the
        // caption say "not to scale"), then shorten each end key to (width − 8) / 2 with `…`.
        if ga.size().x + mid_w + gb.size().x + 16.0 > width {
            show_mid = false;
            if ga.size().x + gb.size().x + 8.0 > width {
                let half = ((width - 8.0) / 2.0).max(8.0);
                ga = ui.fonts_mut(|f| f.layout_job(job(a, half)));
                gb = ui.fonts_mut(|f| f.layout_job(job(b, half)));
            }
        }
        let wb = gb.size().x;
        axp.galley(egui::pos2(r.area.left(), cy - ga.size().y / 2.0), ga, p.text_muted);
        axp.galley(egui::pos2(r.area.right() - wb, cy - gb.size().y / 2.0), gb, p.text_muted);
    }
    if show_mid {
        axp.text(egui::pos2(r.area.center().x, cy), egui::Align2::CENTER_CENTER, mid, small.clone(), p.text_muted);
    }
    if nav && zoom > 1.0 {
        // 0030 A4: the overview is a scroll thumb on a track (the vertical scrollbar's
        // vocabulary): track 4 pt `scroll_track`, thumb 6 pt `scroll_thumb`, `focus` while the
        // pointer is on the axis row (hover / drag); no stroke, no button frame.
        let track = egui::Rect::from_min_max(egui::pos2(r.area.left(), r.axis.bottom() - AXIS_TRACK_H + 1.0), egui::pos2(r.area.right(), r.axis.bottom() - 1.0));
        axp.rect_filled(track, 2.0, p.scroll_track);
        let hot = ui.ctx().pointer_hover_pos().is_some_and(|h| r.axis.contains(h));
        axp.rect_filled(axis_thumb(r, pan.0, zoom), 3.0, if hot { p.focus } else { p.scroll_thumb });
    }
    // ---- level frames.
    let rp = ui.painter().with_clip_rect(r.region.intersect(clip));
    let ap = ui.painter().with_clip_rect(r.area.intersect(r.region).intersect(clip));
    let ordered = order.ordered();
    for row in &lay.rows {
        let fr = frame_rect(r, row);
        if fr.bottom() < r.region.top() || fr.top() > r.region.bottom() {
            continue;
        }
        // Lane bands (§1.5): alternating `lane_b` / `lane_a` (D2 F4), no stroke, no lane lines.
        for k in 0..row.lane_rows {
            let y = r.region.top() + row.lane_y(k, g);
            let band = egui::Rect::from_min_size(egui::pos2(r.area.left(), y), egui::vec2(r.area.width(), g.lane_h));
            ap.rect_filled(band, 0.0, if k % 2 == 0 { p.lane_b } else { p.lane_a });
        }
        // Frame stroke: `border_control` (≥ 3:1 on `data` in every theme; `border` is not).
        rp.rect_stroke(fr, tok.w.rounding, egui::Stroke::new(1.0, p.border_control), egui::StrokeKind::Inside);
        title(ui, &rp, tok, row, title_rect(r, row, g).translate(egui::vec2(g.pad, 0.0)), ordered, &body, &small);
    }
    // ---- boxes.
    let sel_row = st.selection().row();
    let focus = st.session().map().focus();
    let focused_map = has_focus(ui.ctx(), st);
    let hover = ui.ctx().pointer_hover_pos().filter(|h| r.region.contains(*h));
    let mut guides: Vec<(f32, f32)> = Vec::new();
    for b in &lay.boxes {
        let rect = to_rect(r, b);
        if !rect.intersects(r.region) {
            continue;
        }
        let hovered = hover.is_some_and(|h| rect.contains(h));
        match &b.kind {
            BoxKind::File { number, row, on_disk, li, fi, .. } => {
                let selected = sel_row == Some(*row);
                if (selected || hovered) && ordered {
                    guides.push((rect.left(), rect.right()));
                }
                let (fill, stroke, fg) = if selected {
                    (p.sel_bg, egui::Stroke::new(2.0, p.sel_edge), p.sel_fg)
                } else if hovered {
                    (p.hover_bg, egui::Stroke::new(1.5, p.outline), p.text)
                } else {
                    (p.lsm_bg, egui::Stroke::new(1.5, if *on_disk { p.lsm_present } else { p.lsm_missing }), p.text)
                };
                ap.rect_filled(rect, tok.w.rounding, fill);
                if *on_disk {
                    ap.rect_stroke(rect, tok.w.rounding, stroke, egui::StrokeKind::Inside);
                } else {
                    // Missing: dashed border (a cue that does not rely on colour).
                    let colour = if selected {
                        p.sel_fg
                    } else if hovered {
                        p.outline
                    } else {
                        p.lsm_missing
                    };
                    let dash = egui::Stroke::new(stroke.width, colour);
                    let q = rect.shrink(dash.width / 2.0);
                    for seg in [[q.left_top(), q.right_top()], [q.right_top(), q.right_bottom()], [q.right_bottom(), q.left_bottom()], [q.left_bottom(), q.left_top()]] {
                        ap.extend(egui::Shape::dashed_line(&seg, dash, 4.0, 3.0));
                    }
                }
                if focus == Some(*number) {
                    let ring = focus_ring(rect);
                    ap.rect_stroke(ring, tok.w.rounding, egui::Stroke::new(if focused_map { 2.0 } else { 1.0 }, p.focus), egui::StrokeKind::Inside);
                    if selected {
                        ap.rect_stroke(ring.shrink(2.0), tok.w.rounding, egui::Stroke::new(1.0, p.sel_fg), egui::StrokeKind::Inside);
                    }
                }
                // 0030 A1-7: text only at Compact / Full and on a box at least `readable_w` wide.
                if tier != LaneTier::Bar
                    && rect.width() >= readable_w
                    && let Some(f) = c.and_then(|c| c.levels.get(*li)).and_then(|l| l.files.get(*fi))
                {
                    box_text(&ap, ui, rect, f, &BoxFonts { fg, mono: &mono, small: &small, inset: stroke.width + 2.0 });
                }
            }
            BoxKind::Density { count, .. } => {
                // 0030 A3-1/2: a stacked box (lsm_bg, 1.5 pt lsm_present stroke and a second
                // outline offset by (+2, −2) clipped to the lane), hover like boxes; the count
                // centred in `text` when it fits. Adjacent bars keep 1 pt between them.
                let rect = rect.shrink2(egui::vec2(0.5, 0.0));
                let (fill, stroke) = if hovered { (p.hover_bg, egui::Stroke::new(1.5, p.outline)) } else { (p.lsm_bg, egui::Stroke::new(1.5, p.lsm_present)) };
                let stacked = rect.width() > 8.0 && rect.height() > 8.0;
                let main = if stacked { egui::Rect::from_min_max(egui::pos2(rect.left(), rect.top() + 2.0), egui::pos2(rect.right() - 2.0, rect.bottom())) } else { rect };
                if stacked {
                    ap.with_clip_rect(rect.intersect(ap.clip_rect())).rect_stroke(
                        main.translate(egui::vec2(2.0, -2.0)),
                        tok.w.rounding,
                        egui::Stroke::new(1.0, p.lsm_present),
                        egui::StrokeKind::Inside,
                    );
                }
                ap.rect_filled(main, tok.w.rounding, fill);
                ap.rect_stroke(main, tok.w.rounding, stroke, egui::StrokeKind::Inside);
                let vis = main.intersect(ap.clip_rect());
                if let Some(t) = density_label(*count, vis.width() - 2.0 * (stroke.width + 2.0), &|t| ui.fonts_mut(|f| f.layout_no_wrap(t.to_string(), small.clone(), p.text).size().x))
                    && main.height() >= small.size
                {
                    ap.with_clip_rect(vis).text(vis.center(), egui::Align2::CENTER_CENTER, t, small.clone(), p.text);
                }
            }
            BoxKind::Summary { count } => {
                ap.rect_stroke(rect, tok.w.rounding, egui::Stroke::new(1.0, p.border_control), egui::StrokeKind::Inside);
                ap.text(egui::pos2(rect.left().max(r.area.left()) + 4.0, rect.center().y), egui::Align2::LEFT_CENTER, format!("+{count} overlapping files — see Table"), small.clone(), p.text_muted);
            }
        }
    }
    // ---- range guides (§2.4): through the level frames only, never the WAL band.
    if let (Some(first), Some(last)) = (lay.rows.first(), lay.rows.last()) {
        let (y0, y1) = ((r.region.top() + first.lanes_top).max(r.region.top()), (r.region.top() + last.y1).min(r.region.bottom()));
        for (a, b) in guides {
            for x in [a, b] {
                dashed(&ap, egui::pos2(x, y0), egui::pos2(x, y1), egui::Stroke::new(1.0, p.text_muted));
            }
        }
    }
    // ---- pinned titles (§1.6) over the boxes.
    for row in &lay.rows {
        if let Some(t) = pinned_title_rect(r, row, g) {
            rp.rect_filled(t, 0.0, p.data);
            rp.line_segment([t.left_bottom(), t.right_bottom()], egui::Stroke::new(1.0, p.border));
            let inner = egui::Rect::from_min_size(t.min + egui::vec2(g.pad, g.pad / 2.0), egui::vec2(t.width() - 2.0 * g.pad, g.title_h));
            title(ui, &rp, tok, row, inner, ordered, &body, &small);
        }
    }
    // ---- scrollbar (navigation mode, overflowing height).
    if nav && scene_h > r.region.height() + 0.5 {
        let sp = ui.painter().with_clip_rect(r.scrollbar.intersect(clip));
        sp.rect_filled(r.scrollbar, 0.0, p.scroll_track);
        let h = (r.region.height() / scene_h * r.region.height()).max(12.0);
        let y = r.region.top() + pan.1 / scene_h * r.region.height();
        let thumb = egui::Rect::from_min_size(egui::pos2(r.scrollbar.left() + 2.0, y), egui::vec2((r.scrollbar.width() - 4.0).max(2.0), h.min(r.region.height())));
        sp.rect_filled(thumb, 3.0, p.scroll_thumb);
    }
}

/// One level title in `rect` (§1.3–1.4) at the first compaction step that fits.
#[allow(clippy::too_many_arguments)]
fn title(ui: &egui::Ui, painter: &egui::Painter, tok: &Tokens, row: &MapRow, rect: egui::Rect, ordered: bool, body: &egui::FontId, small: &egui::FontId) {
    let p = tok.p;
    let job_of = |step: u8| {
        let mut job = egui::text::LayoutJob::default();
        for (text, role) in lsm_map::title_parts(row, ordered, step) {
            let (font, color) = match role {
                TitleRole::Level => (body.clone(), p.heading),
                TitleRole::Muted => (small.clone(), p.text_muted),
                TitleRole::Status(s) => (small.clone(), tok.status(s)),
            };
            job.append(&text, 0.0, egui::TextFormat { font_id: font, color, valign: egui::Align::Center, ..Default::default() });
        }
        job
    };
    let mut job = job_of(lsm_map::TITLE_STEPS - 1);
    for step in 0..lsm_map::TITLE_STEPS {
        let j = job_of(step);
        let w = ui.fonts_mut(|f| f.layout_job(j.clone())).size().x;
        if w <= rect.width() || step + 1 == lsm_map::TITLE_STEPS {
            job = j;
            break;
        }
    }
    job.wrap = egui::text::TextWrapping::truncate_at_width(rect.width().max(10.0));
    let g = ui.fonts_mut(|f| f.layout_job(job));
    painter.galley(egui::pos2(rect.left(), rect.center().y - g.size().y / 2.0), g, p.heading);
}

/// The pinned WAL band (§4): title line, WAL boxes (or the chips line), the dashed separator
/// and its caption; hover tooltips, click / double-click actions.
#[allow(clippy::too_many_arguments)]
fn paint_band(ui: &egui::Ui, st: &AppState, tok: &Tokens, inv: &LsmInventory, cf: Option<u32>, r: &Regions, chips: bool, resp: &egui::Response, acts: &mut Vec<Action>) {
    let p = tok.p;
    let clip = ui.clip_rect();
    let bp = ui.painter().with_clip_rect(r.band.intersect(clip));
    bp.rect_filled(r.band, 0.0, p.data);
    let small = egui::TextStyle::Small.resolve(ui.style());
    let mono = egui::TextStyle::Monospace.resolve(ui.style());
    let body = egui::TextStyle::Body.resolve(ui.style());
    let left = r.area.left();
    let width = r.area.width().max(10.0);
    let doc = st.session();
    let pick = doc.map().pick();
    let focused_map = has_focus(ui.ctx(), st);
    let ring_w = if focused_map { 2.0 } else { 1.0 };
    let shown = cf.and_then(|id| inv.cf(id));
    let line_y = r.band.top() + body.size * 0.75 + 2.0;
    let mut hits: Vec<(egui::Rect, BandHit)> = Vec::new();
    let files: &[WalFile] = doc.wal_inventory().map(|w| w.files.as_slice()).unwrap_or(&[]);
    // Title / message line.
    let message = match doc.wal_inventory() {
        None => Some("WAL files: not listed (folder not scanned)."),
        Some(w) if w.files.is_empty() => Some("No WAL files found in this folder."),
        _ => None,
    };
    if files.iter().any(|f| f.summary == WalSummary::Scanning) {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    }
    if chips {
        // §4.8: one line of chips: `WAL: [000025.log ✔] …`, selectable.
        let mut x = left;
        let g = bp.layout_no_wrap("WAL:".into(), body.clone(), p.heading);
        bp.galley(egui::pos2(x, line_y - g.size().y / 2.0), g.clone(), p.heading);
        x += g.size().x + 6.0;
        if let Some(m) = message {
            bp.text(egui::pos2(x, line_y), egui::Align2::LEFT_CENTER, m, small.clone(), p.text_muted);
        }
        let room = r.area.right() - x;
        // 0030 A5-2: `000146 ✔✔` — number, state glyph, membership glyph (several CFs).
        let multi = inv.cfs.len() > 1;
        let chip_text = |f: &WalFile| {
            let name = f.number.map(|n| format!("{n:06}")).unwrap_or_else(|| f.name.clone());
            let member = shown.filter(|_| multi).map(|c| map_facts::wal_member_glyph(f, c.id).0).unwrap_or("");
            format!("{name} {}{member}", map_facts::wal_state(f).glyph)
        };
        let chip_w = |f: &WalFile| bp.layout_no_wrap(chip_text(f), small.clone(), p.text).size().x + 10.0;
        // D3 S5: measure from the newest backwards until the room is used (at most the chips
        // that fit + 1 are laid out), never every listed WAL.
        let mut ws: Vec<f32> = Vec::new();
        let mut used = 0.0;
        for f in files.iter().rev() {
            let w = chip_w(f);
            if used + w + 4.0 > room - 60.0 {
                break;
            }
            used += w + 4.0;
            ws.push(w);
        }
        ws.reverse();
        let fit_from = files.len() - ws.len();
        if fit_from > 0 {
            let t = format!("+{}", fit_from);
            let w = bp.layout_no_wrap(t.clone(), small.clone(), p.text).size().x + 10.0;
            let rc = egui::Rect::from_min_size(egui::pos2(x, line_y - small.size * 0.75), egui::vec2(w, small.size * 1.5));
            chip(&bp, tok, rc, &t, None, older_selected(pick, &files[..fit_from]), &small);
            hits.push((rc, BandHit::Older(fit_from)));
            x += w + 4.0;
        }
        for (f, w) in files.iter().skip(fit_from).zip(ws.iter()) {
            let rc = egui::Rect::from_min_size(egui::pos2(x, line_y - small.size * 0.75), egui::vec2(*w, small.size * 1.5));
            let s = map_facts::wal_state(f);
            chip(&bp, tok, rc, &chip_text(f), s.status, f.number.is_some() && pick == f.number.map(MapPick::Wal), &small);
            if let Some(n) = f.number {
                hits.push((rc, BandHit::Wal(n)));
            }
            x += w + 4.0;
        }
    } else {
        // Title line: `WAL` · counts · source (· lifecycle count for the CF shown).
        let fmt = |font: &egui::FontId, color| egui::TextFormat { font_id: font.clone(), color, valign: egui::Align::Center, ..Default::default() };
        // Trailing parts hide like level titles (D2 nit): the lifecycle note first, then the
        // source words, then the bytes; the file count and a truncation note stay.
        // Computed once per frame (D5 nit 7), not per hiding step.
        let bytes: u64 = files.iter().map(|f| f.size).sum();
        let lifecycle = if message.is_none() { map_facts::band_lifecycle(files, inv, cf) } else { None };
        let tail = |step: u8| -> String {
            match message {
                Some(m) => format!("  {m}"),
                None => {
                    let mut t = format!(" · {}", files_word(files.len()));
                    if step < 3 {
                        t.push_str(&format!(" · {}", short_size(bytes)));
                    }
                    if step < 2 {
                        t.push_str(" · folder scan, file number order");
                    }
                    if doc.wal_inventory().is_some_and(|w| w.truncated) {
                        t.push_str(&format!(" · listing truncated: the {} newest shown", crate::limits::MAX_WAL_LISTED));
                    }
                    if step < 1
                        && let Some(l) = &lifecycle
                    {
                        t.push_str(&format!(" · {l}"));
                    }
                    t
                }
            }
        };
        let job_of = |step: u8| {
            let mut job = egui::text::LayoutJob::default();
            job.append("WAL", 0.0, fmt(&body, p.heading));
            job.append(&tail(step), 0.0, fmt(&small, p.text_muted));
            job
        };
        let mut job = job_of(3);
        for step in 0..4u8 {
            let j = job_of(step);
            if ui.fonts_mut(|f| f.layout_job(j.clone())).size().x <= width {
                job = j;
                break;
            }
        }
        job.wrap = egui::text::TextWrapping::truncate_at_width(width);
        let g = ui.fonts_mut(|f| f.layout_job(job));
        bp.galley(egui::pos2(left, line_y - g.size().y / 2.0), g, p.heading);
        let multi_cf = inv.cfs.len() > 1;
        let lane_h = wal_box_h(ui, multi_cf);
        let top = r.band.top() + body.size + 4.0 + WAL_TITLE_GAP;
        if !files.is_empty() {
            // Equal boxes up to 160 pt (D2 F3), at least 56 pt (0030 A5); when they do not fit, the leftmost slot is the summary of
            // the older files, then the newest that fit (§4.3, §4.7).
            let [min_w, max_w] = LAYOUT.map_wal_box_w;
            let gap = LAYOUT.map_wal_gap;
            let n = files.len();
            let fits = (n as f32) * (min_w + gap) - gap <= width;
            let (slots, older) = if fits {
                (n, 0)
            } else {
                let k = (((width + gap) / (min_w + gap)).floor() as usize).max(2);
                (k, n - (k - 1))
            };
            let w = ((width + gap) / slots as f32 - gap).clamp(min_w, max_w);
            let mut x = left;
            if older > 0 {
                let rc = egui::Rect::from_min_size(egui::pos2(x, top), egui::vec2(w, lane_h));
                // D3 S3: also selected while the picked WAL is folded into it.
                let sel = older_selected(pick, &files[..older]);
                let hovered = resp.hover_pos().is_some_and(|h| rc.contains(h));
                wal_box_frame(&bp, tok, rc, sel, hovered, false, sel.then_some(ring_w));
                let fg = if sel { p.sel_fg } else { p.text };
                // D6: `+33 older`, then the number range in monospace (`000023–000131`); full
                // names stay in the tooltip / Explanation.
                let num = |f: &WalFile| f.number.map(|n| format!("{n:06}")).unwrap_or_else(|| f.name.clone());
                bp.text(egui::pos2(rc.left() + 4.0, rc.top() + 2.0), egui::Align2::LEFT_TOP, format!("+{older} older"), small.clone(), fg);
                let range = format!("{}–{}", num(&files[0]), num(&files[older - 1]));
                let rg = ui.fonts_mut(|x| {
                    x.layout_job(egui::text::LayoutJob { wrap: egui::text::TextWrapping::truncate_at_width(rc.width() - 8.0), ..egui::text::LayoutJob::simple_singleline(range, mono.clone(), fg) })
                });
                bp.galley(egui::pos2(rc.left() + 4.0, rc.top() + 2.0 + small.size * 1.15), rg, fg);
                hits.push((rc, BandHit::Older(older)));
                x += w + gap;
            }
            for f in &files[older..] {
                let rc = egui::Rect::from_min_size(egui::pos2(x, top), egui::vec2(w, lane_h));
                let sel = f.number.is_some() && pick == f.number.map(MapPick::Wal);
                let hovered = resp.hover_pos().is_some_and(|h| rc.contains(h));
                // Membership (and dimming) when there is a CF to choose (§4.5).
                let member = shown.filter(|_| multi_cf).map(|c| (map_facts::wal_member_glyph(f, c.id), &*c.name));
                let dim = member.is_some_and(|m| m.0.1);
                wal_box_frame(&bp, tok, rc, sel, hovered, dim, sel.then_some(ring_w));
                wal_box_text(&bp, ui, tok, rc, f, member.map(|((g, _), name)| (g, name)), sel, dim, (&mono, &small));
                if let Some(n) = f.number {
                    hits.push((rc, BandHit::Wal(n)));
                }
                x += w + gap;
            }
        }
        // Separator (§4.2): dashed `border`, 8 pt above and below, then the caption.
        let sy = r.band.bottom() - small.size * 1.2 - LAYOUT.map_wal_sep_gap;
        dashed(&bp, egui::pos2(r.frames.left(), sy), egui::pos2(r.frames.right(), sy), egui::Stroke::new(1.0, p.border_control));
        bp.text(egui::pos2(left, sy + LAYOUT.map_wal_sep_gap), egui::Align2::LEFT_TOP, "SST levels (live per MANIFEST)", small.clone(), p.text_muted);
    }
    // Interaction.
    let Some(ptr) = resp.hover_pos().filter(|h| r.band.contains(*h)) else { return };
    let Some((rc, h)) = hits.iter().find(|(rc, _)| rc.contains(ptr)).copied() else { return };
    match h {
        BandHit::Wal(n) => {
            // N3: facts are built only when a tooltip can show.
            if let Some(f) = files.iter().find(|f| f.number == Some(n)).filter(|_| super::tooltips_on(ui.ctx())) {
                let facts = map_facts::wal_facts(f, inv, cf);
                let hint = map_facts::wal_hint(f);
                let _ = resp.clone().tip_at_ui(rc, fact_tip_body(tok, &f.name, &facts, Some(&hint)));
            }
            if resp.double_clicked() {
                acts.push(Action::OpenWal(n));
            } else if resp.clicked() {
                acts.push(Action::SelectWal(n));
            }
        }
        BandHit::Older(k) => {
            let folded = &files[..k.min(files.len())];
            if super::tooltips_on(ui.ctx()) {
                let facts = map_facts::older_wals_facts(folded, inv);
                let title = map_facts::older_title(folded.len());
                let _ = resp.clone().tip_at_ui(rc, fact_tip_body(tok, &title, &facts, Some("Click to list them in the Explanation")));
            }
            // The pick names the newest folded WAL, so it survives a resize (N6).
            if resp.clicked()
                && let Some(n) = folded.iter().rev().find_map(|f| f.number)
            {
                acts.push(Action::SelectMapPick(MapPick::OlderWals(n)));
            }
        }
    }
}

/// The older-WAL box / chip is selected: its own pick, or a WAL folded into it is picked
/// (keyboard Left past the oldest visible box; D3 S3).
fn older_selected(pick: Option<MapPick>, folded: &[WalFile]) -> bool {
    match pick {
        Some(MapPick::Wal(m) | MapPick::OlderWals(m)) => folded.iter().any(|f| f.number == Some(m)),
        _ => false,
    }
}

/// A chip of the band's one-line form: text in a small frame, status-coloured when given.
fn chip(p: &egui::Painter, tok: &Tokens, rc: egui::Rect, text: &str, status: Option<Status>, selected: bool, small: &egui::FontId) {
    let (fill, fg) = if selected { (tok.p.sel_bg, tok.p.sel_fg) } else { (tok.p.lsm_bg, status.map(|s| tok.status(s)).unwrap_or(tok.p.text_muted)) };
    p.rect_filled(rc, tok.w.rounding, fill);
    p.rect_stroke(rc, tok.w.rounding, egui::Stroke::new(1.0, tok.p.border_control), egui::StrokeKind::Inside);
    p.text(rc.left_center() + egui::vec2(5.0, 0.0), egui::Align2::LEFT_CENTER, text, small.clone(), fg);
}

/// A WAL box's fill, stroke and focus ring (§4.3): `lsm_bg` with 1 px `border_control` (SSTs
/// use 1.5 px `lsm_present`); hover / selected / focus as SST boxes; dimmed: `border` stroke.
fn wal_box_frame(p: &egui::Painter, tok: &Tokens, rc: egui::Rect, selected: bool, hovered: bool, dim: bool, focus: Option<f32>) {
    let c = tok.p;
    let (fill, stroke) = if selected {
        (c.sel_bg, egui::Stroke::new(2.0, c.sel_edge))
    } else if hovered {
        (c.hover_bg, egui::Stroke::new(1.5, c.outline))
    } else {
        (c.lsm_bg, egui::Stroke::new(1.0, if dim { c.text_muted } else { c.border_control }))
    };
    p.rect_filled(rc, tok.w.rounding, fill);
    p.rect_stroke(rc, tok.w.rounding, stroke, egui::StrokeKind::Inside);
    // 0030 A2-1: the picked card's ring is 2 pt while the map has keyboard focus, else 1 pt.
    if let Some(w) = focus {
        p.rect_stroke(focus_ring(rc), tok.w.rounding, egui::Stroke::new(w, c.focus), egui::StrokeKind::Inside);
    }
}

/// WAL card lines by card width (0030 A5-2): name (monospace), scan state (glyph first, status
/// colour) and the membership glyph + CF name for the CF shown. ≥ 160 pt: `000146.log` /
/// `✔ 12 rec · seq 919..930` / `✔ cf_users`; 120–159: name or number / `✔ 12 rec` / glyph + cut
/// name; 72–119: number / `✔ 12` / `✔ cf_u…`; < 72: number / state glyph · membership glyph.
/// A glyph is laid out on its own and never cut; `?` (unknown) is never drawn as absent.
#[allow(clippy::too_many_arguments)]
fn wal_box_text(p: &egui::Painter, ui: &egui::Ui, tok: &Tokens, rc: egui::Rect, f: &WalFile, member: Option<(&str, &str)>, selected: bool, dim: bool, (mono, small): (&egui::FontId, &egui::FontId)) {
    let c = tok.p;
    let w = rc.width() - 8.0;
    if w < 8.0 {
        return;
    }
    let card = rc.width();
    let [tiny_w, short_w] = LAYOUT.map_wal_tier_w;
    let full_w = LAYOUT.map_wal_box_w[1];
    let base = if selected {
        c.sel_fg
    } else if dim {
        c.text_muted
    } else {
        c.text
    };
    let s = map_facts::wal_state(f);
    let state_col = if selected || dim { base } else { s.status.map(|x| tok.status(x)).unwrap_or(c.text_muted) };
    let measure = |t: &str, font: &egui::FontId| ui.fonts_mut(|x| x.layout_no_wrap(t.to_string(), font.clone(), base).size().x);
    let x0 = rc.left() + 4.0;
    let mut y = rc.top() + 2.0;
    // Line 1: the name when it fits (from 120 pt), else the number (D6: never a cut name).
    let number = f.number.map(|n| format!("{n:06}")).unwrap_or_else(|| f.name.clone());
    let name = if card >= short_w && measure(&f.name, mono) <= w { f.name.clone() } else { number };
    p.text(egui::pos2(x0, y), egui::Align2::LEFT_TOP, fit_chars(&name, w, &|t| measure(t, mono)), mono.clone(), base);
    y += mono.size * 1.15;
    // Line 2: the state by width; below 72 pt the state and membership glyphs only.
    if card < tiny_w {
        let t = match member {
            Some((g, _)) => format!("{} {g}", s.glyph),
            None => s.glyph.to_string(),
        };
        p.text(egui::pos2(x0, y), egui::Align2::LEFT_TOP, t, small.clone(), state_col);
        return;
    }
    let state = if card >= full_w {
        // The full state cut at a word boundary (the size and the full text are in the
        // tooltip / Explanation).
        fit_words(&s.text, w, &|t| measure(t, small))
    } else {
        let (short, tiny) = map_facts::wal_state_short(f);
        glyph_line(if card >= short_w { &short } else { &tiny }, w, &|t| measure(t, small))
    };
    p.text(egui::pos2(x0, y), egui::Align2::LEFT_TOP, state, small.clone(), state_col);
    y += small.size * 1.15;
    // Line 3: membership glyph + CF name (glyph never cut).
    if let Some((g, cf)) = member
        && y + small.size <= rc.bottom() + 1.0
    {
        let col = if selected || dim { base } else { c.text_muted };
        p.text(egui::pos2(x0, y), egui::Align2::LEFT_TOP, glyph_line(&format!("{g} {cf}"), w, &|t| measure(t, small)), small.clone(), col);
    }
}

/// `text` (a leading glyph, a space, then words) fitted to `w`: the glyph is kept whole and the
/// rest is cut with `…` (0030 A5-3: truncation never removes a glyph).
fn glyph_line(text: &str, w: f32, measure: &dyn Fn(&str) -> f32) -> String {
    if measure(text) <= w {
        return text.to_string();
    }
    match text.split_once(' ') {
        Some((g, rest)) => {
            let room = w - measure(&format!("{g} "));
            if room <= 0.0 { g.to_string() } else { format!("{g} {}", fit_chars(rest, room, measure)) }
        }
        None => text.to_string(),
    }
}

/// `text` cut to fit `w` with a trailing `…` (or `…` alone).
fn fit_chars(text: &str, w: f32, measure: &dyn Fn(&str) -> f32) -> String {
    if measure(text) <= w {
        return text.to_string();
    }
    let mut s: String = text.to_string();
    while !s.is_empty() && measure(&format!("{s}…")) > w {
        s.pop();
    }
    format!("{s}…")
}

/// `text` if it fits in `w`, else its leading words followed by ` …` (no trailing `·`), else
/// `…`: a cut is always at a word boundary, never inside a number or word (D2 F3).
fn fit_words(text: &str, w: f32, measure: &dyn Fn(&str) -> f32) -> String {
    if measure(text) <= w {
        return text.to_string();
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    for k in (1..words.len()).rev() {
        let mut head = &words[..k];
        while let Some((last, rest)) = head.split_last() {
            if *last == "·" { head = rest } else { break }
        }
        if head.is_empty() {
            continue;
        }
        let t = format!("{} …", head.join(" "));
        if measure(&t) <= w {
            return t;
        }
    }
    "…".into()
}

/// Space between the WAL band title line and its boxes (D6).
const WAL_TITLE_GAP: f32 = 3.0;

/// Text colour and fonts of a box label.
#[derive(Clone, Copy)]
struct BoxFonts<'a> {
    fg: egui::Color32,
    mono: &'a egui::FontId,
    small: &'a egui::FontId,
    /// Text inset from the box edge: the stroke width + 2 pt (D6: no glyph under the stroke).
    inset: f32,
}

/// Box text, elided as the box narrows: key line, then size, then the number truncated, then
/// nothing (a coloured bar only).
fn box_text(ap: &egui::Painter, ui: &egui::Ui, rect: egui::Rect, f: &LsmFile, fonts: &BoxFonts) {
    let BoxFonts { fg, mono, small, inset } = *fonts;
    let (number, on_disk, size) = (f.number, f.on_disk(), f.size);
    let pad = inset.max(4.0);
    // Text starts at the visible left edge of a box that is panned partly out of view.
    let left = rect.left().max(ap.clip_rect().left()) + pad;
    let w = rect.right() - left - pad;
    if w < 8.0 {
        return;
    }
    let width = |t: &str, f: &egui::FontId| ui.fonts_mut(|x| x.layout_no_wrap(t.to_string(), f.clone(), fg).size().x);
    let id = format!("{}{:06}", if on_disk { "" } else { "○ " }, number);
    let id = if width(&id, mono) <= w {
        id
    } else {
        let mut s = id.clone();
        while !s.is_empty() && width(&format!("{s}…"), mono) > w {
            s.pop();
        }
        if s.is_empty() {
            return;
        }
        format!("{s}…")
    };
    let size_t = short_size(size);
    // Compact tier (0030 A1-2): one line, the number centred vertically, the size beside it
    // when it fits.
    if rect.height() < mono.size * 1.15 + small.size + inset {
        let y = rect.center().y;
        ap.text(egui::pos2(left, y), egui::Align2::LEFT_CENTER, &id, egui::FontId::new(mono.size, mono.family.clone()), fg);
        let id_w = width(&id, mono) + 6.0;
        if id_w + width(&size_t, small) <= w {
            ap.text(egui::pos2(left + id_w, y), egui::Align2::LEFT_CENTER, &size_t, small.clone(), fg);
        }
        return;
    }
    let mut y = rect.top() + inset;
    ap.text(egui::pos2(left, y), egui::Align2::LEFT_TOP, &id, egui::FontId::new(mono.size, mono.family.clone()), fg);
    y += mono.size * 1.15;
    if width(&size_t, small) > w || y + small.size > rect.bottom() {
        return;
    }
    ap.text(egui::pos2(left, y), egui::Align2::LEFT_TOP, &size_t, small.clone(), fg);
    y += small.size * 1.15;
    let k = format!("{} … {}", f.smallest_preview, f.largest_preview);
    if y + small.size <= rect.bottom() + 1.0 && w > 40.0 {
        let g = ui.fonts_mut(|x| x.layout_job(egui::text::LayoutJob { wrap: egui::text::TextWrapping::truncate_at_width(w), ..egui::text::LayoutJob::simple_singleline(k, small.clone(), fg) }));
        ap.galley(egui::pos2(left, y), g, fg);
    }
}

/// The focus ring of a box: inset past its border, or the box itself when it is too narrow
/// for the inset (an inverted rect would draw nothing; D4 reviewer nit).
fn focus_ring(rect: egui::Rect) -> egui::Rect {
    let inset = LAYOUT.map_focus_gap + 1.0;
    if rect.width() > 2.0 * inset + 2.0 && rect.height() > 2.0 * inset + 2.0 { rect.shrink(inset) } else { rect }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D4: a focused box at the 3-pt minimum width still gets a visible (non-inverted) ring;
    /// a wide box gets the inset ring.
    #[test]
    fn focus_ring_of_a_narrow_box_is_not_inverted() {
        let narrow = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(LAYOUT.map_min_box_w, 40.0));
        let r = focus_ring(narrow);
        assert!(r.width() > 0.0 && r.height() > 0.0 && r.is_positive(), "{r:?}");
        assert_eq!(r, narrow);
        let wide = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 40.0));
        assert_eq!(focus_ring(wide), wide.shrink(LAYOUT.map_focus_gap + 1.0));
    }

    /// 0030 A3: a density bar shows "600 files", else "600", else nothing (the zoom-to-bar rule
    /// is tested in `app::lsm_map`).
    #[test]
    fn density_count_and_tier_heights() {
        let m = |t: &str| t.chars().count() as f32 * 6.0;
        assert_eq!(density_label(600, 80.0, &m).as_deref(), Some("600 files"));
        assert_eq!(density_label(600, 30.0, &m).as_deref(), Some("600"));
        assert_eq!(density_label(600, 20.0, &m), None);
        // Tier heights: Full > Compact > Bar ≥ the LAYOUT floor.
        let [f, c, b] = tier_heights(14.0, 11.0);
        assert!(f > c && c > b && b >= LAYOUT.map_lane_min_h);
    }

    /// 0030 A5: a glyph is never cut; the rest is cut with `…`.
    #[test]
    fn wal_card_glyph_is_kept() {
        let m = |t: &str| t.chars().count() as f32;
        assert_eq!(glyph_line("✔ cf_users", 20.0, &m), "✔ cf_users");
        assert_eq!(glyph_line("✔ cf_users", 7.0, &m), "✔ cf_u…");
        assert_eq!(glyph_line("? cf_users", 2.0, &m), "?");
    }

    /// D2 F3: WAL box lines are cut at word boundaries with ` …`, never inside a number.
    #[test]
    fn wal_lines_cut_at_word_boundaries() {
        let m = |t: &str| t.chars().count() as f32; // one point per char
        assert_eq!(fit_words("✔ 3 rec · seq 115..136", 30.0, &m), "✔ 3 rec · seq 115..136");
        assert_eq!(fit_words("✔ 3 rec · seq 115..136", 14.0, &m), "✔ 3 rec …", "no trailing ·, no cut number");
        assert_eq!(fit_words("default: ✔ seen", 12.0, &m), "default: ✔ …");
        assert_eq!(fit_words("≈ partial: first 167 rec", 2.0, &m), "…");
    }
}
