//! LSM Map model (egui-free): the map's own view state (`MapState`: Table/Map mode, CF, pan,
//! map zoom, focused file, selected WAL, conditional navigation `NavState` from the painter's
//! zoom-1 `MapMeasure`), the availability of the map for the open file/folder, the pure
//! `layout` of one CF of an `LsmInventory` into level rows and boxes (positions from the rank
//! slots and lanes the inventory computed once), and keyboard neighbours. Range and order
//! semantics live in the inventory (`model::LsmInventory`, built by `present::manifest`); this
//! module only scales them. Map pan/zoom never touch the app zoom.

use crate::limits::LIMITS;
use crate::model::{KeyOrder, LsmCf, LsmFile, LsmInventory, LsmLevel, Status, files_word};
use std::path::PathBuf;

/// Levels table or LSM Map (both show the Observed levels view).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MapMode {
    #[default]
    Table,
    Map,
}

/// Map zoom bounds: ×1 = Fit (the whole CF in the map width).
pub const ZOOM_MIN: f32 = 1.0;
/// Absolute cap; the painter also caps at "one rank ≥ 24 pt" (`max_zoom`).
pub const ZOOM_CAP: f32 = 100_000.0;
/// One zoom step of the − / + buttons and keys.
pub const ZOOM_STEP: f32 = 1.5;
/// Lanes drawn per level; overlapping files beyond them are one summary lane.
pub const MAX_LANES: u32 = 16;

/// What Back restores of the map (mode, CF, pan, zoom, focused box, selected WAL).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct MapSnapshot {
    pub mode: MapMode,
    pub cf: Option<u32>,
    pub pan: (f32, f32),
    pub zoom: f32,
    pub focus: Option<u64>,
    /// The picked WAL / older-WAL summary / level (exclusive with a selected levels row).
    pub pick: Option<MapPick>,
    /// The lane-detail tier (0030 A1).
    pub tier: LaneTier,
}

/// A map object that is not a levels row (0026 §1.7, §4): selected by a click or the keys,
/// described by the Explanation's map card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapPick {
    /// A WAL of the band (by number).
    Wal(u64),
    /// The band's "+N older WAL files" summary, by the newest WAL number folded into it (the
    /// files up to it): the same object after a resize changes N (D3 N6).
    OlderWals(u64),
    /// A level frame of the CF shown (its title bar).
    Level(u32),
}

/// Conditional navigation (0026 item 6): the scene fits readably at zoom 1 (stationary, no
/// controls, no wheel capture) or overflows (local zoom/pan with visible controls).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum NavState {
    Fits,
    /// Also the state until the painter has measured the scene (navigation stays available).
    #[default]
    Overflow,
}

/// What the painter measured of the CF shown at zoom 1 (`measure`), plus the room it had: the
/// scroll region under the strip, axis row and WAL band (the strip's height never changes, so
/// showing the controls cannot change the result; 0026 §6.3), with the vertical scrollbar
/// width always subtracted from `view_w` (§6.1).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct MapMeasure {
    /// Height of all level frames (points).
    pub scene_h: f32,
    /// Height and width of the scroll region / box area (points).
    pub view_h: f32,
    pub view_w: f32,
    /// Narrowest file box at zoom 1 (points; `f32::INFINITY` without files).
    pub narrowest: f32,
    /// Smallest readable box width (`map_readable_w`: "000000" in Monospace + 8 pt).
    pub readable_w: f32,
    /// One lane's height (the height margin to leave navigation).
    pub lane_h: f32,
    /// At zoom 1 some lane needs a density bar, a summary lane, or `LIMITS.map_boxes` would be
    /// exceeded.
    pub merged: bool,
    /// What the decision depends on besides the area: the CF shown, its file count and the
    /// app zoom.
    pub cf: Option<u32>,
    pub files: usize,
    pub app_zoom: f32,
}

/// Width margin (points) the narrowest box must have to leave navigation (0026 §6.4).
pub const NAV_MARGIN_W: f32 = 24.0;
/// An area change up to this (points) does not re-decide Fits / Overflow (§6.4).
pub const NAV_AREA_EPS: f32 = 2.0;

impl MapMeasure {
    fn fits(&self, margin_w: f32, margin_h: f32) -> bool {
        !self.merged && self.narrowest >= self.readable_w + margin_w && self.scene_h <= self.view_h - margin_h
    }
    /// The next navigation state from `prev` (hysteresis): Fits → Overflow as soon as the scene
    /// does not fit readably; Overflow → Fits only with `NAV_MARGIN_W` width and one lane of
    /// height to spare.
    pub fn nav(&self, prev: NavState) -> NavState {
        let fits = match prev {
            NavState::Fits => self.fits(0.0, 0.0),
            NavState::Overflow => self.fits(NAV_MARGIN_W, self.lane_h),
        };
        if fits { NavState::Fits } else { NavState::Overflow }
    }
    /// Fits / Overflow is decided again only when the area changed by more than
    /// `NAV_AREA_EPS`, or the CF, the inventory (files, scene height) or the app zoom changed.
    pub fn decides_after(&self, last: &MapMeasure) -> bool {
        (self.view_w - last.view_w).abs() > NAV_AREA_EPS
            || (self.view_h - last.view_h).abs() > NAV_AREA_EPS
            || self.cf != last.cf
            || self.files != last.files
            || (self.scene_h - last.scene_h).abs() > 0.5
            || (self.app_zoom - last.app_zoom).abs() > 1e-4
            || self.readable_w != last.readable_w
            || self.narrowest != last.narrowest && self.view_w == last.view_w
            || self.merged != last.merged
    }
}

/// The map's view state, owned by `DocumentSession` next to the view it describes.
/// `pan.0` is the left edge as a fraction of the CF's full width (0 ..= 1 − 1/zoom), so a zoom
/// about the centre needs no pixel geometry; `pan.1` is the vertical scroll in points.
#[derive(Clone, Debug, PartialEq)]
pub struct MapState {
    mode: MapMode,
    cf: Option<u32>,
    pan: (f32, f32),
    zoom: f32,
    focus: Option<u64>,
    /// One-line strip message (e.g. "000099.sst is not a live file in this MANIFEST").
    note: Option<String>,
    /// The focused box should be brought into view (one-shot, cleared by the painter's
    /// `MapView`).
    reveal: bool,
    /// The map widget should take keyboard focus (one-shot; the LSM Map entry asked for it).
    take_focus: bool,
    /// Upper bound of `+` / `MapZoom::In` (the painter reports "one rank ≥ 24 pt" for the CF
    /// shown, `max_zoom()`); never below `ZOOM_FLOOR_MAX`.
    max_zoom: f32,
    /// The picked WAL / older-WAL summary / level (exclusive with a selected levels row).
    pick: Option<MapPick>,
    /// Fits / Overflow (0026 item 6), the measure it was decided from and the latest one.
    nav: NavState,
    decided: Option<MapMeasure>,
    measure: Option<MapMeasure>,
    /// Lane-detail tier (0030 A1) and the scene heights per tier at ×1 (the painter's).
    tier: LaneTier,
    heights: Option<TierHeights>,
    /// The next decision that lands in Overflow applies Fit once (a new map, a CF switch); a
    /// restored map (Back) keeps its view.
    fit_on_overflow: bool,
}

impl Default for MapState {
    fn default() -> Self {
        MapState {
            mode: MapMode::Table,
            cf: None,
            pan: (0.0, 0.0),
            zoom: ZOOM_MIN,
            focus: None,
            note: None,
            reveal: false,
            take_focus: false,
            max_zoom: ZOOM_FLOOR_MAX,
            pick: None,
            nav: NavState::Overflow,
            decided: None,
            measure: None,
            tier: LaneTier::Full,
            heights: None,
            fit_on_overflow: true,
        }
    }
}

impl MapState {
    pub fn mode(&self) -> MapMode {
        self.mode
    }
    pub fn cf(&self) -> Option<u32> {
        self.cf
    }
    pub fn pan(&self) -> (f32, f32) {
        self.pan
    }
    pub fn zoom(&self) -> f32 {
        self.zoom
    }
    pub fn focus(&self) -> Option<u64> {
        self.focus
    }
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }
    pub fn reveal(&self) -> bool {
        self.reveal
    }
    pub fn snapshot(&self) -> MapSnapshot {
        MapSnapshot { mode: self.mode, cf: self.cf, pan: self.pan, zoom: self.zoom, focus: self.focus, pick: self.pick, tier: self.tier }
    }
    /// The lane-detail tier the map is drawn at (0030 A1).
    pub fn tier(&self) -> LaneTier {
        self.tier
    }
    /// The painter's scene heights per tier (×1) for the CF shown.
    pub fn heights(&self) -> Option<TierHeights> {
        self.heights
    }
    /// The scene height of the CF shown at the current tier (×1), when measured.
    pub fn scene_h(&self) -> Option<f32> {
        self.heights.map(|h| h[self.tier.index()]).or(self.measure.map(|m| m.scene_h))
    }
    /// The picked map object (WAL, older-WAL summary, level), if any.
    pub fn pick(&self) -> Option<MapPick> {
        self.pick
    }
    /// The selected WAL (number) of the WAL band.
    pub fn wal(&self) -> Option<u64> {
        match self.pick {
            Some(MapPick::Wal(n)) => Some(n),
            _ => None,
        }
    }
    pub fn nav(&self) -> NavState {
        self.nav
    }
    /// Wheel, drag and the zoom keys act on the map (only while it overflows).
    pub fn navigable(&self) -> bool {
        self.nav == NavState::Overflow
    }
    pub fn measure(&self) -> Option<MapMeasure> {
        self.measure
    }
    /// The CF shown: the chosen one if it exists, else the focused file's, else the first.
    pub fn shown_cf(&self, inv: &LsmInventory) -> Option<u32> {
        let has = |id: u32| inv.cf(id).is_some();
        self.cf.filter(|c| has(*c)).or_else(|| self.focus.and_then(|n| inv.by_number(n)).map(|(c, _, _)| c.id)).or_else(|| inv.cfs.first().map(|c| c.id))
    }

    pub(super) fn restore(&mut self, s: &MapSnapshot) {
        self.mode = s.mode;
        self.cf = s.cf;
        // Same bounds as `set_view` (non-finite values become Fit / top).
        self.zoom = if s.zoom.is_finite() { s.zoom.clamp(ZOOM_MIN, ZOOM_CAP) } else { ZOOM_MIN };
        self.pan = (clamp_pan_x(s.pan.0, self.zoom), if s.pan.1.is_finite() { s.pan.1.max(0.0) } else { 0.0 });
        self.focus = s.focus;
        self.pick = s.pick;
        self.tier = if self.zoom > ZOOM_MIN { LaneTier::Full } else { s.tier };
        self.note = None;
        self.reveal = false;
        // A restored view is kept (Back); a new entry asks for Fit (`fit_on_next_overflow`).
        self.fit_on_overflow = false;
        // A stationary map stays at ×1 (§6.9: if it fits, ×1 overrides the restored zoom); the
        // next measure decides again.
        if self.nav == NavState::Fits {
            self.fit();
        }
        self.clamp_pan_y();
    }
    /// The next decision that lands in Overflow applies Fit once (a new map entry).
    pub(super) fn fit_on_next_overflow(&mut self) {
        self.fit_on_overflow = true;
    }
    /// The painter's scene heights per tier for the CF shown (0030 A1).
    pub(super) fn set_heights(&mut self, h: TierHeights) {
        if h.iter().all(|x| x.is_finite() && *x >= 0.0) {
            self.heights = Some(h);
            self.clamp_pan_y();
        }
    }
    /// Picks a WAL / older-WAL summary / level (None: none); the levels focus is kept.
    pub(super) fn set_pick(&mut self, p: Option<MapPick>) {
        self.pick = p;
    }
    /// The painter's measure of this frame (0026 §6.4): decides Fits / Overflow with hysteresis
    /// only when the area, CF, inventory or app zoom changed (`decides_after`); a stationary
    /// scene is at zoom 1, top; the pan always stays inside the scene. Returns whether it
    /// decided. User actions (Fit, Center, zoom, pan) never switch the mode.
    pub(super) fn set_measure(&mut self, m: MapMeasure) -> bool {
        let ok = |v: f32| v.is_finite() && v >= 0.0;
        if !(ok(m.scene_h) && ok(m.view_h) && ok(m.view_w) && ok(m.readable_w) && ok(m.lane_h)) || m.narrowest.is_nan() {
            return false;
        }
        let decide = self.decided.is_none_or(|d| m.decides_after(&d));
        let before = self.nav;
        let fitted = self.measure.is_some() && self.heights.is_some() && self.is_fitted();
        if decide {
            self.nav = m.nav(self.nav);
            self.decided = Some(m);
        }
        self.measure = Some(m);
        if self.nav == NavState::Fits {
            self.fit();
        } else if (decide && (self.fit_on_overflow || before == NavState::Fits)) || fitted {
            // 0030 A1-9: entering an overflowing map (new map, CF switch, a resize that changed
            // the decision) applies Fit once; a view the user has not moved since Fit stays
            // fitted to the new area; other resizes keep the tier and clamp the pan.
            self.fit();
        }
        if decide {
            self.fit_on_overflow = false;
        }
        self.clamp_pan_y();
        decide
    }
    /// Largest vertical pan of the scene at the current tier (0 when unmeasured or fitting).
    fn max_pan_y(&self) -> Option<f32> {
        let view_h = self.measure?.view_h;
        self.scene_h().map(|h| (h - view_h).max(0.0))
    }
    /// A view can never be stranded below the scene.
    fn clamp_pan_y(&mut self) {
        if let Some(max) = self.max_pan_y() {
            self.pan.1 = self.pan.1.clamp(0.0, max);
        }
    }
    /// Center: the scene's centre in the middle of the box area at the CURRENT zoom and tier
    /// (Fit changes them; Center does not), in both axes.
    pub(super) fn center(&mut self) {
        self.pan.0 = clamp_pan_x((1.0 - 1.0 / self.zoom.max(ZOOM_MIN)) / 2.0, self.zoom);
        self.pan.1 = self.max_pan_y().map(|m| m / 2.0).unwrap_or(0.0);
    }
    pub(super) fn set_mode(&mut self, m: MapMode) {
        self.mode = m;
    }
    /// The map widget takes keyboard focus when it is next drawn (Shift+⌘M, toolbar, menu).
    pub(super) fn request_focus(&mut self) {
        self.take_focus = true;
    }
    pub(super) fn take_focus_request(&mut self) -> bool {
        std::mem::take(&mut self.take_focus)
    }
    /// A keyboard-focus request is pending (tests).
    pub fn focus_requested(&self) -> bool {
        self.take_focus
    }
    pub(super) fn set_note(&mut self, n: Option<String>) {
        self.note = n;
    }
    /// Another CF: its whole width, no focus carried over.
    /// `shown`: the CF the map shows now (`shown_cf`). Tester P4/P5 (A3, D2–D4): before a CF was
    /// chosen, `cf` is None while the map shows the first CF; selecting a box of that CF must not
    /// count as a CF switch (it used to Fit to ×1 and drop the focus after a bar zoom).
    pub(super) fn set_cf(&mut self, cf: u32, shown: Option<u32>) {
        if self.cf.or(shown) == Some(cf) {
            self.cf = Some(cf);
        } else {
            self.cf = Some(cf);
            // The new CF's heights arrive with its first measure, which applies Fit once.
            self.heights = None;
            self.fit_on_overflow = true;
            self.fit();
            self.focus = None;
            self.reveal = false;
            // A level pick belongs to its CF (D3 N6); WAL picks are CF-independent.
            if matches!(self.pick, Some(MapPick::Level(_))) {
                self.pick = None;
            }
        }
    }
    pub(super) fn set_focus(&mut self, f: Option<u64>, reveal: bool) {
        self.focus = f;
        self.reveal = reveal && f.is_some();
    }
    /// Fit (0030 A1-4): zoom ×1, the whole key range across the width, and the largest lane
    /// tier at which every level fits the region height, top-aligned; when even Bar overflows,
    /// Bar centred vertically (the scrollbar stays).
    pub(super) fn fit(&mut self) {
        let (tier, pan_y) = self.fit_view(self.measure.map(|m| m.view_h));
        self.zoom = ZOOM_MIN;
        self.tier = tier;
        self.pan = (0.0, pan_y);
    }
    /// Fit's tier and vertical pan for a region `view_h` high: centred only when the tier heights
    /// are known and even Bar overflows (before the painter measured them, Fit is the plain ×1 /
    /// top of 0026).
    fn fit_view(&self, view_h: Option<f32>) -> (LaneTier, f32) {
        match (self.heights, view_h) {
            (Some(h), Some(v)) => {
                let t = fit_tier(&h, v);
                (t, (h[t.index()] - v).max(0.0) / 2.0)
            }
            _ => (LaneTier::Full, 0.0),
        }
    }
    /// The view is still exactly Fit's result for the measured area (no zoom, pan or Center
    /// since): it stays fitted when the area settles or changes (tester P3 / reviewer N2: the
    /// first measures of a new map come from a layout that is still settling).
    fn is_fitted(&self) -> bool {
        let (t, y) = self.fit_view(self.measure.map(|m| m.view_h));
        self.zoom == ZOOM_MIN && self.pan.0 == 0.0 && self.tier == t && (self.pan.1 - y).abs() < 0.01
    }
    /// Zoom by `factor` about the fraction `at` of the visible width (0.5 = centre), clamped to
    /// [ZOOM_MIN, max]. Zooming in asks for detail: the Full tier (0030 A1-10).
    pub(super) fn zoom_about(&mut self, factor: f32, at: f32, max: f32) {
        let (pan_x, z) = zoom_about(self.pan.0, self.zoom, factor, at, max);
        self.tier = tier_after_zoom(self.tier, self.zoom, z);
        self.zoom = z;
        self.pan.0 = pan_x;
        self.clamp_pan_y();
    }
    /// The painter's measured map maximum (one rank ≥ 24 pt); `+` from the keys/strip uses it.
    pub(super) fn set_max_zoom(&mut self, max: f32) {
        self.max_zoom = max.clamp(ZOOM_FLOOR_MAX, ZOOM_CAP);
    }
    pub fn zoom_max(&self) -> f32 {
        self.max_zoom
    }
    /// The focused box was not laid out (hidden lane, over the cap): nothing to reveal.
    pub(super) fn clear_reveal(&mut self) {
        self.reveal = false;
    }
    /// Pan/zoom set by the painter (drag, wheel, pinch, reveal), clamped.
    pub(super) fn set_view(&mut self, pan: (f32, f32), zoom: f32) {
        let z = if zoom.is_finite() { zoom.clamp(ZOOM_MIN, ZOOM_CAP) } else { ZOOM_MIN };
        self.tier = tier_after_zoom(self.tier, self.zoom, z);
        self.zoom = z;
        self.pan = (clamp_pan_x(pan.0, self.zoom), if pan.1.is_finite() { pan.1.max(0.0) } else { 0.0 });
        self.clamp_pan_y();
        self.reveal = false;
    }
    /// Horizontal auto-pan so that the fraction range [a, b) is visible.
    pub(super) fn reveal_x(&mut self, a: f32, b: f32) {
        let w = 1.0 / self.zoom;
        if a < self.pan.0 {
            self.pan.0 = a;
        } else if b > self.pan.0 + w {
            self.pan.0 = (b - w).min(a);
        }
        self.pan.0 = clamp_pan_x(self.pan.0, self.zoom);
    }
}

/// The one zoom formula (painter wheel/pinch/density click, strip buttons and keys): zoom by
/// `factor` about the fraction `at` of the visible width (0.5 = centre), clamped to
/// [ZOOM_MIN, max]; returns (left-edge fraction, zoom).
pub fn zoom_about(pan_x: f32, zoom: f32, factor: f32, at: f32, max: f32) -> (f32, f32) {
    let max = max.clamp(ZOOM_MIN, ZOOM_CAP);
    let at = at.clamp(0.0, 1.0);
    let z = (zoom * factor).clamp(ZOOM_MIN, max);
    let anchor = pan_x + at / zoom.max(ZOOM_MIN);
    (clamp_pan_x(anchor - at / z, z), z)
}

/// The lane tier after a zoom change (0030 A1-10): zooming in asks for detail (Full); zooming
/// out or keeping the zoom keeps the tier (only Fit reduces it).
pub fn tier_after_zoom(t: LaneTier, old_z: f32, new_z: f32) -> LaneTier {
    if new_z > old_z + 1e-6 { LaneTier::Full } else { t }
}

/// Zoom into a density bar of `count` files spanning box-area x `[x0, x1)` at the current
/// pan/zoom (0030 A3-3, tester P2 fix): the smallest zoom at which each of its files is
/// `readable_w` wide (`zoom · count · readable_w / bar_w`), at least twice the current zoom, at
/// most `zmax`; the bar's centre at the view centre. A click therefore always reveals more
/// (also for a bar across the whole width at ×1); files still merged after it form smaller bars
/// with their own counts, so repeated clicks drill down. At `zmax` nothing changes. Returns
/// (left-edge fraction, zoom).
pub fn zoom_to_bar(pan_x: f32, zoom: f32, (x0, x1): (f32, f32), count: usize, area_w: f32, readable_w: f32, zmax: f32) -> (f32, f32) {
    let zoom = zoom.max(ZOOM_MIN);
    let full = area_w * zoom;
    let bar_w = (x1 - x0).max(1e-3);
    let zmax = zmax.clamp(ZOOM_MIN, ZOOM_CAP);
    let want = zoom * count.max(1) as f32 * readable_w / bar_w;
    let z = want.max(zoom * 2.0).min(zmax).max(zoom);
    let centre = ((x0 + x1) / 2.0 + pan_x * full) / full;
    (clamp_pan_x(centre - 0.5 / z, z), z)
}

/// The reveal of a file at key-rank fraction `[a, b)` that is merged into a density bar at
/// `zoom` (narrower than `min_w` in an `area_w`-wide area; 0030 A3-5): the smallest zoom at which
/// it is `readable_w` wide (at least the current zoom, at most `zmax`) with the file centred;
/// None when it is already drawn as a box.
pub fn reveal_zoom(a: f32, b: f32, area_w: f32, zoom: f32, readable_w: f32, min_w: f32, zmax: f32) -> Option<(f32, f32)> {
    if (b - a) * area_w * zoom >= min_w {
        return None;
    }
    let z = (readable_w / ((b - a).max(1e-9) * area_w)).clamp(zoom.max(ZOOM_MIN), zmax.max(ZOOM_MIN));
    Some((clamp_pan_x((a + b) / 2.0 - 0.5 / z, z), z))
}

/// Left-edge fraction bounds at zoom `z`.
pub fn clamp_pan_x(p: f32, z: f32) -> f32 {
    let max = (1.0 - 1.0 / z.max(ZOOM_MIN)).max(0.0);
    if p.is_finite() { p.clamp(0.0, max) } else { 0.0 }
}

/// Largest useful map zoom: one rank slot at least `slot_pt` wide in a map `width` points wide.
/// Never below `ZOOM_FLOOR_MAX`, so − / + / Fit always act on a small inventory too.
pub fn max_zoom(scale: u32, width: f32, slot_pt: f32) -> f32 {
    if width <= 0.0 { ZOOM_FLOOR_MAX } else { (slot_pt * scale.max(1) as f32 / width).clamp(ZOOM_FLOOR_MAX, ZOOM_CAP) }
}

/// Smallest upper zoom bound (a few files already have ranks wider than 24 pt at ×1).
pub const ZOOM_FLOOR_MAX: f32 = 4.0;

/// Where the LSM Map entry leads (`DocumentSession::lsm_map_availability`).
#[derive(Clone, Debug, PartialEq)]
pub enum MapEntry {
    /// The Observed levels view is shown: switch its mode.
    Shown,
    /// The open file is a CURRENT or MANIFEST: request its Observed levels.
    InFile,
    /// Open this CURRENT (else MANIFEST) of the folder first; focus the file number if known.
    Via { path: PathBuf, focus: Option<u64>, from: Option<String> },
}

/// Why the LSM Map is unavailable (the one text for the disabled tooltip and Explanation).
#[derive(Clone, Debug, PartialEq)]
pub struct Unavailable(pub String);

pub const NEED_FOLDER: &str = "LSM Map needs a RocksDB folder. Open Folder…, or open its CURRENT or MANIFEST file.";
/// A reason with the platform's Open Folder shortcut after "Open Folder…" (the UI knows the key
/// text): "… Open Folder… (Shift+⌘O), or …". Other reasons are returned unchanged.
pub fn with_open_folder_key(reason: &str, key: &str) -> String {
    if key.is_empty() { reason.to_string() } else { reason.replacen("Open Folder…,", &format!("Open Folder… ({key}),"), 1) }
}

pub const NO_CURRENT: &str = "This folder has no CURRENT or MANIFEST, so its live SST files are unknown. Open the RocksDB data folder that contains CURRENT.";

pub const LISTING: &str = "Looking for CURRENT or MANIFEST next to this file (listing its folder)…";

pub fn lone_sst(name: &str) -> String {
    format!("{name} was opened without its database folder (no CURRENT next to it). Level and neighbours are unknown. Open the RocksDB folder to see the LSM map.")
}

// ---------------- layout ----------------

/// Point sizes the painter measured (the model knows no fonts).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    /// Width of the box area (the scroll region minus the frame padding and the scrollbar).
    pub width: f32,
    /// Height of one lane (box height) and the gap between lanes.
    pub lane_h: f32,
    pub lane_gap: f32,
    /// Height of a level frame's title bar, the frame's inner padding and the gap between
    /// frames (0026 §1.2–1.3).
    pub title_h: f32,
    pub pad: f32,
    pub row_gap: f32,
    /// Minimum drawn box width; narrower neighbours in a lane merge into a density bar.
    pub min_w: f32,
    /// Lowest lane height of a level that holds a density bar (0030 A1-6: the Compact tier,
    /// so the bar's count stays readable); `lane_h` when it is not lower.
    pub dense_lane_h: f32,
    /// Visible part of the box area, in box-area coordinates after pan (x0, y0, x1, y1).
    pub visible: [f32; 4],
}

/// The map's lane-detail tier (0030 A1): Fit chooses the largest tier at which every level fits
/// the region height; zooming in returns to Full. Horizontal zoom (`zoom`) is independent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub enum LaneTier {
    /// Number, size and key range (three lines).
    #[default]
    Full,
    /// One line: the number (and the size when it fits).
    Compact,
    /// No text: present/missing stroke, selection, focus and the tooltip only.
    Bar,
}

impl LaneTier {
    pub const ALL: [LaneTier; 3] = [LaneTier::Full, LaneTier::Compact, LaneTier::Bar];
    pub fn index(self) -> usize {
        match self {
            LaneTier::Full => 0,
            LaneTier::Compact => 1,
            LaneTier::Bar => 2,
        }
    }
}

/// Scene heights at zoom 1 per tier (`LaneTier::index` order), measured by the painter.
pub type TierHeights = [f32; 3];

/// The largest tier whose scene fits `view_h` (Bar when none does).
pub fn fit_tier(h: &TierHeights, view_h: f32) -> LaneTier {
    LaneTier::ALL.into_iter().find(|t| h[t.index()] <= view_h + 0.5).unwrap_or(LaneTier::Bar)
}

/// One level frame of the map (y in box-area coordinates after the vertical pan).
#[derive(Clone, Debug, PartialEq)]
pub struct MapRow {
    pub level: u32,
    /// Frame top / bottom.
    pub y0: f32,
    pub y1: f32,
    /// Top of the title bar and of the first lane.
    pub title_y: f32,
    pub lanes_top: f32,
    /// Lane bands drawn (lanes up to `MAX_LANES` plus the summary lane); 0 for an empty level.
    pub lane_rows: u32,
    pub files: usize,
    pub bytes: u64,
    pub on_disk: usize,
    pub lanes: u32,
    /// Overlapping files on L1+ (ordered keys only).
    pub overlaps: bool,
    /// Files in lanes beyond `MAX_LANES` (one summary lane).
    pub hidden: usize,
    /// This level's lane height (the tier's, or `dense_lane_h` for a level with a density bar).
    pub lane_h: f32,
}

impl MapRow {
    /// Top of lane `k` of this frame.
    pub fn lane_y(&self, k: u32, g: &Geometry) -> f32 {
        self.lanes_top + k as f32 * (self.lane_h + g.lane_gap)
    }
    /// The lane band under y (box-area coordinates), if any.
    pub fn lane_at(&self, y: f32, g: &Geometry) -> Option<u32> {
        (0..self.lane_rows).find(|&k| {
            let t = self.lane_y(k, g);
            y >= t && y < t + self.lane_h
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum BoxKind {
    /// One file; `li`/`fi` index it directly (`LsmCf::levels[li].files[fi]`), so the painter
    /// never searches the inventory per box.
    File { number: u64, row: usize, on_disk: bool, size: u64, li: usize, fi: usize },
    /// Several files of one lane narrower than `min_w` together: zoom in to select.
    Density { count: usize, bytes: u64 },
    /// "+N overlapping files — see Table".
    Summary { count: usize },
}

/// One box in box-area coordinates after pan (x0, y0, x1, y1).
#[derive(Clone, Debug, PartialEq)]
pub struct MapBox {
    pub rect: [f32; 4],
    pub level: u32,
    pub kind: BoxKind,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct MapLayout {
    pub cf: Option<u32>,
    pub rows: Vec<MapRow>,
    pub boxes: Vec<MapBox>,
    /// Full height of all frames (points).
    pub height: f32,
    /// Boxes not produced because of `LIMITS.map_boxes`.
    pub capped: usize,
    pub legend: String,
    pub short_legend: &'static str,
}

/// Lane bands of a level (drawn lanes + the summary lane) and its hidden-file count.
fn lane_rows(l: &LsmLevel) -> (u32, usize) {
    let hidden = l.files.iter().filter(|f| f.lane >= MAX_LANES).count();
    if l.files.is_empty() { (0, 0) } else { (l.lanes.clamp(1, MAX_LANES) + u32::from(hidden > 0), hidden) }
}

/// Height of a level frame with lanes `lane_h` high: padding, title bar, (padding, lanes,)
/// padding.
fn frame_h(rows: u32, lane_h: f32, g: &Geometry) -> f32 {
    let lanes = if rows == 0 { 0.0 } else { g.pad + rows as f32 * (lane_h + g.lane_gap) - g.lane_gap };
    g.pad + g.title_h + lanes + g.pad
}

/// A level at `zoom` holds a density bar (some drawn file narrower than `min_w`).
fn level_dense(l: &LsmLevel, g: &Geometry, zoom: f32) -> bool {
    let scale = l.scale.max(1) as f32;
    let full = g.width * zoom.max(ZOOM_MIN);
    l.files.iter().filter(|f| f.lane < MAX_LANES).any(|f| (f.slots.1.saturating_sub(f.slots.0)) as f32 / scale * full < g.min_w)
}

/// The lane height of a level: the geometry's, raised to `dense_lane_h` when the level holds a
/// density bar (0030 A1-6).
fn level_lane_h(l: &LsmLevel, g: &Geometry, zoom: f32) -> f32 {
    if g.dense_lane_h > g.lane_h && level_dense(l, g, zoom) { g.dense_lane_h } else { g.lane_h }
}

/// The scene height of CF `cf` at zoom 1 with this geometry (equals `layout(..).height`), for
/// the tier choice of Fit (0030 A1).
pub fn scene_height(inv: &LsmInventory, cf: Option<u32>, g: &Geometry) -> f32 {
    cf.and_then(|id| inv.cf(id)).map(|c| c.levels.iter().map(|l| frame_h(lane_rows(l).0, level_lane_h(l, g, ZOOM_MIN), g) + g.row_gap).sum()).unwrap_or(0.0)
}

/// Lays out CF `cf` of `inv` for the map: one frame per level (title bar, then lanes × lane
/// height; an empty level is its title bar only), file boxes at their rank slots × width ×
/// zoom − pan (mirroring is already in the slots), culled to `g.visible`, adjacent sub-`min_w`
/// boxes merged into density bars, at most `MAX_LANES` lanes and `LIMITS.map_boxes` boxes.
pub fn layout(inv: &LsmInventory, cf: Option<u32>, pan: (f32, f32), zoom: f32, g: &Geometry) -> MapLayout {
    let Some(c) = cf.and_then(|id| inv.cf(id)) else { return MapLayout::default() };
    let full = g.width * zoom.max(ZOOM_MIN);
    let px = pan.0 * full;
    let [vx0, vy0, vx1, vy1] = g.visible;
    let mut out = MapLayout { cf: Some(c.id), legend: c.order.legend(), short_legend: c.order.short_legend(), ..Default::default() };
    let mut y = -pan.1;
    for (li, l) in c.levels.iter().enumerate() {
        let (rows, hidden) = lane_rows(l);
        let lane_h = level_lane_h(l, g, zoom);
        let h = frame_h(rows, lane_h, g);
        let row = MapRow {
            level: l.level,
            y0: y,
            y1: y + h,
            title_y: y + g.pad,
            lanes_top: y + g.pad + g.title_h + g.pad,
            lane_rows: rows,
            files: l.files.len(),
            bytes: l.bytes,
            on_disk: l.on_disk,
            lanes: l.lanes,
            overlaps: l.overlaps,
            hidden,
            lane_h,
        };
        if row.y1 >= vy0 && row.y0 <= vy1 {
            lay_level(li, l, &row, (full, px), g, [vx0, vx1], &mut out);
        }
        if hidden > 0 {
            let ly = row.lane_y(rows - 1, g);
            out.boxes.push(MapBox { rect: [vx0.max(-px), ly, vx1.min(full - px), ly + lane_h], level: l.level, kind: BoxKind::Summary { count: hidden } });
        }
        y += h + g.row_gap;
        out.rows.push(row);
    }
    out.height = y + pan.1;
    out
}

/// What the measure needs besides the geometry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeasureInput {
    pub view_h: f32,
    pub readable_w: f32,
    pub app_zoom: f32,
}

/// The fitting measure of CF `cf` at zoom 1 in a box area `g.width` wide and `input.view_h`
/// high (0026 §6.1): full scene height (as `layout().height`), the narrowest file box, and
/// whether a density bar, a summary lane or the box cap would be needed. One pass over the CF's
/// files, no allocation.
pub fn measure(inv: &LsmInventory, cf: Option<u32>, g: &Geometry, input: MeasureInput) -> MapMeasure {
    let mut m = MapMeasure {
        scene_h: 0.0,
        view_h: input.view_h,
        view_w: g.width,
        narrowest: f32::INFINITY,
        readable_w: input.readable_w,
        lane_h: g.lane_h,
        merged: false,
        cf,
        files: 0,
        app_zoom: input.app_zoom,
    };
    let Some(c) = cf.and_then(|id| inv.cf(id)) else { return m };
    let mut boxes = 0usize;
    for l in &c.levels {
        let (rows, hidden) = lane_rows(l);
        m.scene_h += frame_h(rows, level_lane_h(l, g, ZOOM_MIN), g) + g.row_gap;
        let scale = l.scale.max(1) as f32;
        for f in l.files.iter().filter(|f| f.lane < MAX_LANES) {
            let w = (f.slots.1.saturating_sub(f.slots.0)) as f32 / scale * g.width;
            m.narrowest = m.narrowest.min(w);
            m.merged |= w < g.min_w;
            boxes += 1;
        }
        m.files += l.files.len();
        // Files beyond the drawn lanes are a summary lane: not every box is shown.
        m.merged |= hidden > 0;
    }
    m.merged |= boxes > LIMITS.map_boxes;
    m
}

// ---------------- level titles, axis, empty lane space (0026 §1–§2) ----------------

/// "4.1 KiB" (the short part of `human_size`).
pub fn short_size(n: u64) -> String {
    let h = crate::util::human_size(n);
    h.split(" (").next().unwrap_or(&h).to_string()
}

/// How a part of a level title is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TitleRole {
    /// "L0": Body strong, `heading`.
    Level,
    /// Small `text_muted`.
    Muted,
    /// Small, in this status colour.
    Status(Status),
}

/// Compaction steps of a level title (§1.3 hiding order): 0 full, 1 without the level note,
/// 2 also without the bytes, 3 the status shortened to `✔` / `○N`. `L0` and the file count
/// are never hidden.
pub const TITLE_STEPS: u8 = 4;

/// The parts of a level title at compaction `step` (0..TITLE_STEPS).
pub fn title_parts(r: &MapRow, ordered: bool, step: u8) -> Vec<(String, TitleRole)> {
    let mut v = vec![(format!("L{}", r.level), TitleRole::Level)];
    if r.files == 0 {
        v.push((" · empty (0 files in MANIFEST)".into(), TitleRole::Muted));
        return v;
    }
    v.push((format!(" · {}", files_word(r.files)), TitleRole::Muted));
    if step < 2 {
        v.push((format!(" · {}", short_size(r.bytes)), TitleRole::Muted));
    }
    let missing = r.files - r.on_disk;
    v.push(match (missing, step >= 3) {
        (0, false) => (format!(" · ✔ {} on disk", r.on_disk), TitleRole::Status(Status::Present)),
        (0, true) => (" · ✔".into(), TitleRole::Status(Status::Present)),
        (m, false) => (format!(" · ✔ {} · ○ {m} missing", r.on_disk), TitleRole::Status(Status::NotPresent)),
        (m, true) => (format!(" · ○{m}"), TitleRole::Status(Status::NotPresent)),
    });
    if step == 0 && ordered {
        if r.level == 0 && r.lanes > 1 {
            v.push((format!(" · {} overlap lanes", r.lanes), TitleRole::Muted));
        } else if r.overlaps {
            v.push((" · ⚠ overlap".into(), TitleRole::Status(Status::Unsupported)));
        }
    }
    v
}

/// The axis row ends: the first and last boundary key of the CF in comparator order (observed
/// smallest/largest keys of its files only), as previews; None for an unknown order (§2.2).
pub fn axis_ends(c: &LsmCf) -> Option<(String, String)> {
    let (a, b) = c.order.span(c.levels.iter().flat_map(|l| l.files.iter()))?;
    Some((crate::util::bytes_preview(a, 40), crate::util::bytes_preview(b, 40)))
}

/// The strip's short legend when the full sentence has no room (D4): marked with ` …` so it
/// never reads as the whole statement; the full sentence is in its tooltip and the
/// Explanation.
pub fn short_legend_cut(order: &KeyOrder) -> String {
    format!("{} …", order.short_legend())
}

/// The axis row's middle text (§2.2).
pub fn axis_middle(order: &KeyOrder) -> &'static str {
    if order.ordered() { "key rank, not to scale ->" } else { "file number order ->" }
}

/// The caption line under the legend (§2.3): only when the CF shown has a level with more than
/// one lane (ordered keys only).
pub fn lane_caption(c: &LsmCf) -> Option<&'static str> {
    (c.order.ordered() && c.levels.iter().any(|l| l.lanes > 1)).then_some("Stacked lanes: overlapping L0 files. Empty lane space: outside that file's key range.")
}

/// Files whose rank range covers the fraction `x` (0..1 of the CF's rank scale), the level at
/// index `first` first, then the others in level order: at most `max` (number, level), and the
/// total count. One bounded pass; nothing for an unknown order.
pub fn cover_at(c: &LsmCf, x: f32, first: usize, max: usize) -> (Vec<(u64, u32)>, usize) {
    let mut out = Vec::new();
    let mut total = 0usize;
    if !c.order.ordered() {
        return (out, 0);
    }
    let order = std::iter::once(first).chain((0..c.levels.len()).filter(move |&i| i != first));
    for li in order {
        let Some(l) = c.levels.get(li) else { continue };
        let s = l.scale.max(1) as f32;
        for f in &l.files {
            if (f.slots.0 as f32 / s) <= x && x < (f.slots.1 as f32 / s) {
                total += 1;
                if out.len() < max {
                    out.push((f.number, l.level));
                }
            }
        }
    }
    (out, total)
}

/// The empty-lane-space tooltip (§2.5): exact sentences, at most three names, never a key.
pub fn empty_lane_text(c: &LsmCf, li: usize, lane: u32, x: f32) -> Option<String> {
    let l = c.levels.get(li)?;
    if !c.order.ordered() {
        return None;
    }
    let (hits, total) = cover_at(c, x, li, 3);
    let mut lines = vec![format!("L{} lane {}: no file in this lane here.", l.level, lane + 1)];
    if hits.is_empty() {
        lines.push(format!("No live SST range of {} covers this position.", c.name));
    } else {
        let names: Vec<String> = hits.iter().map(|(n, lv)| format!("{n:06} (L{lv})")).collect();
        let more = if total > hits.len() { format!(" +{} more", total - hits.len()) } else { String::new() };
        lines.push(format!("Covered at this position by: {}{more}.", names.join(", ")));
    }
    lines.push(if l.level == 0 { "Lanes only stack overlapping L0 files.".into() } else { "Gaps are between file key ranges.".into() });
    Some(lines.join("\n"))
}

/// Narrow files of one lane being merged: x range, count, bytes and the first file's index.
#[derive(Clone, Copy)]
struct Pending {
    x0: f32,
    x1: f32,
    count: usize,
    bytes: u64,
    first: usize,
}

fn file_kind(li: usize, fi: usize, f: &LsmFile) -> BoxKind {
    BoxKind::File { number: f.number, row: f.row, on_disk: f.on_disk(), size: f.size, li, fi }
}

/// `(full, px)`: the CF's full width at this zoom and the horizontal pan in points.
fn lay_level(li: usize, l: &LsmLevel, row: &MapRow, (full, px): (f32, f32), g: &Geometry, vx: [f32; 2], out: &mut MapLayout) {
    let scale = l.scale.max(1) as f32;
    let mut pending: Vec<Option<Pending>> = vec![None; MAX_LANES as usize];
    let lane_y = |lane: usize| row.lane_y(lane as u32, g);
    // A single narrow file stays selectable at the minimum width; several are a density bar.
    let flush = |out: &mut MapLayout, lane: usize, p: Pending| {
        let y = lane_y(lane);
        let rect = [p.x0, y, p.x1.max(p.x0 + g.min_w), y + row.lane_h];
        let kind = if p.count == 1 { file_kind(li, p.first, &l.files[p.first]) } else { BoxKind::Density { count: p.count, bytes: p.bytes } };
        out.boxes.push(MapBox { rect, level: l.level, kind });
    };
    for (fi, f) in l.files.iter().enumerate().filter(|(_, f)| f.lane < MAX_LANES) {
        let x0 = f.slots.0 as f32 / scale * full - px;
        let x1 = f.slots.1 as f32 / scale * full - px;
        if x1 < vx[0] || x0 > vx[1] {
            continue;
        }
        if out.boxes.len() >= LIMITS.map_boxes {
            out.capped += 1;
            continue;
        }
        let lane = f.lane as usize;
        if x1 - x0 < g.min_w {
            // Joins the lane's pending bar if it touches it, else starts one.
            match pending[lane] {
                Some(p) if x0 <= p.x1 + g.min_w => pending[lane] = Some(Pending { x1: p.x1.max(x1), count: p.count + 1, bytes: p.bytes + f.size, ..p }),
                Some(p) => {
                    flush(out, lane, p);
                    pending[lane] = Some(Pending { x0, x1, count: 1, bytes: f.size, first: fi });
                }
                None => pending[lane] = Some(Pending { x0, x1, count: 1, bytes: f.size, first: fi }),
            }
            continue;
        }
        if let Some(p) = pending[lane].take() {
            flush(out, lane, p);
        }
        let y = lane_y(lane);
        out.boxes.push(MapBox { rect: [x0, y, x1, y + row.lane_h], level: l.level, kind: file_kind(li, fi, f) });
    }
    for (lane, p) in pending.into_iter().enumerate() {
        if let Some(p) = p {
            flush(out, lane, p);
        }
    }
}

// ---------------- keyboard neighbours ----------------

/// Keyboard moves on the map (arrows, Home, End).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapMove {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}

fn centre(l: &LsmLevel, f: &LsmFile) -> f32 {
    (f.slots.0 + f.slots.1) as f32 / 2.0 / l.scale.max(1) as f32
}

/// The file a move from `from` (file number; None = start at the first file) reaches in CF
/// `c`: Left/Right previous/next in the same lane, Up/Down nearest by x centre in the nearest
/// non-empty level above/below, Home/End first/last of the level. Files in hidden lanes are
/// reached through the table.
pub fn neighbour(c: &LsmCf, from: Option<u64>, m: MapMove) -> Option<&LsmFile> {
    let visible = |f: &&LsmFile| f.lane < MAX_LANES;
    let Some((li, f)) = from.and_then(|n| c.levels.iter().enumerate().find_map(|(i, l)| l.files.iter().find(|f| f.number == n).map(|f| (i, f)))) else {
        return c.levels.iter().flat_map(|l| l.files.iter()).find(visible);
    };
    let l = &c.levels[li];
    let mut lane: Vec<&LsmFile> = l.files.iter().filter(|g| g.lane == f.lane).collect();
    lane.sort_by_key(|g| (g.slots.0, g.number));
    let pos = lane.iter().position(|g| g.number == f.number)?;
    match m {
        MapMove::Left => pos.checked_sub(1).map(|p| lane[p]),
        MapMove::Right => lane.get(pos + 1).copied(),
        MapMove::Home => l.files.iter().filter(visible).min_by_key(|g| (g.slots.0, g.lane, g.number)),
        MapMove::End => l.files.iter().filter(visible).max_by_key(|g| (g.slots.1, g.lane, g.number)),
        MapMove::Up => {
            // Lane above in the same level first, then the level above.
            if f.lane > 0 {
                let x = centre(l, f);
                return l.files.iter().filter(|g| g.lane == f.lane - 1).min_by(|a, b| (centre(l, a) - x).abs().total_cmp(&(centre(l, b) - x).abs()));
            }
            nearest(c.levels[..li].iter().rev(), centre(l, f))
        }
        MapMove::Down => {
            let x = centre(l, f);
            if let Some(g) = l.files.iter().filter(|g| g.lane == f.lane + 1 && g.lane < MAX_LANES).min_by(|a, b| (centre(l, a) - x).abs().total_cmp(&(centre(l, b) - x).abs())) {
                return Some(g);
            }
            nearest(c.levels[li + 1..].iter(), centre(l, f))
        }
    }
}

/// In the first level of `levels` with a visible file: the file nearest to x centre `x`.
fn nearest<'a>(mut levels: impl Iterator<Item = &'a LsmLevel>, x: f32) -> Option<&'a LsmFile> {
    let k = levels.find(|k| k.files.iter().any(|g| g.lane < MAX_LANES))?;
    k.files.iter().filter(|g| g.lane < MAX_LANES).min_by(|a, b| (centre(k, a) - x).abs().total_cmp(&(centre(k, b) - x).abs()).then(a.lane.cmp(&b.lane)))
}

/// Horizontal fraction range of a file on its level's scale.
pub fn fraction(l: &LsmLevel, f: &LsmFile) -> (f32, f32) {
    let s = l.scale.max(1) as f32;
    (f.slots.0 as f32 / s, f.slots.1 as f32 / s)
}

/// `KeyOrder` of the CF shown (for the strip/legend), Unknown without one.
pub fn order_of(inv: &LsmInventory, cf: Option<u32>) -> KeyOrder {
    cf.and_then(|c| inv.cf(c)).map(|c| c.order.clone()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LsmCf, Status};

    fn file(number: u64, slots: (u32, u32), lane: u32, row: usize) -> LsmFile {
        LsmFile {
            number,
            size: 100,
            smallest: vec![],
            largest: vec![],
            smallest_preview: String::new(),
            largest_preview: String::new(),
            seq: None,
            overlap_same: Some(0),
            overlap_next: Some(0),
            status: Status::Present,
            row,
            path: None,
            slots,
            lane,
        }
    }
    fn inv(order: KeyOrder) -> LsmInventory {
        let l0 = LsmLevel { level: 0, files: vec![file(1, (0, 3), 0, 0), file(2, (1, 4), 1, 1), file(3, (4, 6), 0, 2)], bytes: 300, on_disk: 3, scale: 6, lanes: 2, overlaps: true };
        let l1 = LsmLevel { level: 1, files: vec![], bytes: 0, on_disk: 0, scale: 6, lanes: 0, overlaps: false };
        let l2 = LsmLevel { level: 2, files: vec![file(4, (0, 2), 0, 3), file(5, (2, 6), 0, 4)], bytes: 200, on_disk: 2, scale: 6, lanes: 1, overlaps: false };
        LsmInventory { cfs: vec![LsmCf { id: 0, name: "default".into(), levels: vec![l0, l1, l2], comparator: None, order }], truncated: false, ..Default::default() }
    }
    fn geo(width: f32) -> Geometry {
        Geometry { width, lane_h: 20.0, lane_gap: 2.0, title_h: 10.0, pad: 5.0, row_gap: 4.0, min_w: 3.0, dense_lane_h: 12.0, visible: [0.0, 0.0, width, 1000.0] }
    }
    fn file_boxes(l: &MapLayout) -> Vec<(u64, [f32; 4])> {
        l.boxes
            .iter()
            .filter_map(|b| match b.kind {
                BoxKind::File { number, .. } => Some((number, b.rect)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn layout_places_by_rank_with_lanes_and_thin_empty_levels() {
        let i = inv(KeyOrder::Bytewise);
        let st = MapState::default();
        let l = layout(&i, Some(0), st.pan(), st.zoom(), &geo(600.0));
        let b = file_boxes(&l);
        // Frame: pad 5, title 10, pad 5, then the lanes (top 20).
        assert_eq!(b[0], (1, [0.0, 20.0, 300.0, 40.0]));
        assert_eq!(b[1], (2, [100.0, 42.0, 400.0, 62.0]), "overlapping L0 file in lane 1");
        assert_eq!(b[2], (3, [400.0, 20.0, 600.0, 40.0]));
        assert_eq!((l.rows[0].y1, l.rows[0].lane_rows, l.rows[1].y1 - l.rows[1].y0, l.rows[1].lane_rows), (67.0, 2, 20.0, 0), "two lanes, then an empty L1 (title bar only)");
        assert_eq!((l.rows[0].lane_at(45.0, &geo(600.0)), l.rows[0].lane_at(41.0, &geo(600.0))), (Some(1), None), "lane bands with a gap");
        assert!(l.legend.contains("bytewise") && l.legend.contains("not to scale"));
        // No two boxes of one level overlap on screen.
        for (n, a) in &b {
            for (m, c) in &b {
                if n < m && (a[1] - c[1]).abs() < 1e-3 {
                    assert!(a[2] <= c[0] || c[2] <= a[0], "{n} and {m} overlap");
                }
            }
        }
        // Unsupported: the legend says that neither order nor width means anything.
        let l = layout(&inv(KeyOrder::Unsupported("x.Cmp".into())), Some(0), st.pan(), st.zoom(), &geo(600.0));
        assert!(l.legend.contains("file number order") && l.legend.contains("'x.Cmp'"));
    }

    #[test]
    fn narrow_boxes_merge_into_density_bars_and_culling() {
        let mut files = Vec::new();
        for i in 0..1000u32 {
            files.push(file(i as u64, (i, i + 1), 0, i as usize));
        }
        let l0 = LsmLevel { level: 0, files, bytes: 100_000, on_disk: 1000, scale: 1000, lanes: 1, overlaps: false };
        let i = LsmInventory { cfs: vec![LsmCf { id: 0, name: "d".into(), levels: vec![l0], comparator: None, order: KeyOrder::Bytewise }], truncated: false, ..Default::default() };
        let st = MapState::default();
        let l = layout(&i, Some(0), st.pan(), st.zoom(), &geo(500.0));
        assert!(l.boxes.iter().all(|b| matches!(b.kind, BoxKind::Density { .. })), "0.5 pt per file: density only");
        let n: usize = l.boxes.iter().map(|b| if let BoxKind::Density { count, .. } = b.kind { count } else { 0 }).sum();
        assert_eq!(n, 1000);
        // Zoomed ×100 at the left: 50 pt per file, ~10 visible boxes only (culled).
        let mut z = MapState::default();
        z.set_view((0.0, 0.0), 100.0);
        let l = layout(&i, Some(0), z.pan(), z.zoom(), &geo(500.0));
        assert!(l.boxes.len() <= 12 && file_boxes(&l).len() >= 10, "{}", l.boxes.len());
    }

    #[test]
    fn hidden_lanes_are_one_summary_lane() {
        let files: Vec<LsmFile> = (0..20u32).map(|i| file(i as u64, (0, 10), i, i as usize)).collect();
        let l0 = LsmLevel { level: 0, files, bytes: 2000, on_disk: 20, scale: 10, lanes: 20, overlaps: true };
        let i = LsmInventory { cfs: vec![LsmCf { id: 0, name: "d".into(), levels: vec![l0], comparator: None, order: KeyOrder::Bytewise }], truncated: false, ..Default::default() };
        let l = layout(&i, Some(0), (0.0, 0.0), 1.0, &geo(100.0));
        assert_eq!(file_boxes(&l).len(), 16);
        assert!(l.boxes.iter().any(|b| b.kind == BoxKind::Summary { count: 4 }));
        assert_eq!(l.rows[0].hidden, 4);
    }

    #[test]
    fn map_state_fit_zoom_bounds_and_pan() {
        let mut s = MapState::default();
        assert_eq!((s.zoom(), s.pan()), (1.0, (0.0, 0.0)));
        s.zoom_about(0.5, 0.5, 10.0);
        assert_eq!(s.zoom(), 1.0, "never below Fit");
        s.zoom_about(4.0, 0.5, 10.0);
        assert_eq!(s.zoom(), 4.0);
        assert!((s.pan().0 - 0.375).abs() < 1e-5, "zoom about the centre: {:?}", s.pan());
        s.zoom_about(100.0, 0.5, 10.0);
        assert_eq!(s.zoom(), 10.0, "capped by max");
        s.set_view((5.0, -3.0), 10.0);
        assert_eq!(s.pan(), (0.9, 0.0), "pan clamped");
        s.reveal_x(0.1, 0.15);
        assert!((s.pan().0 - 0.1).abs() < 1e-5);
        s.fit();
        assert_eq!((s.zoom(), s.pan()), (1.0, (0.0, 0.0)));
        assert_eq!(max_zoom(1000, 500.0, 24.0), 48.0);
        assert_eq!(max_zoom(16, 700.0, 24.0), ZOOM_FLOOR_MAX, "small inventories still zoom");
        let snap = MapSnapshot { mode: MapMode::Map, cf: Some(2), pan: (0.5, 10.0), zoom: 4.0, focus: Some(7), pick: None, tier: LaneTier::Full };
        s.restore(&snap);
        assert_eq!(s.snapshot(), snap);
        // D3 (reviewer nit 3): non-finite values are rejected like `set_view`.
        s.restore(&MapSnapshot { pan: (f32::NAN, f32::INFINITY), zoom: f32::NAN, ..snap });
        assert_eq!((s.zoom(), s.pan()), (ZOOM_MIN, (0.0, 0.0)));
    }

    fn input(view_h: f32) -> MeasureInput {
        MeasureInput { view_h, readable_w: 40.0, app_zoom: 1.0 }
    }

    /// 0030 A3-3/A3-5/A1-10 (architect S1): the camera rules the painter applies — zoom to a
    /// density bar, the reveal zoom of a merged file, the tier after a zoom.
    #[test]
    fn zoom_to_bar_reveal_zoom_and_tier_after_zoom() {
        // Tester P2 (A3 FAIL on D1): a bar of 600 files across the whole 900-pt area at ×1 —
        // the click zooms (each file readable: 600 · 56 / 900 ≈ ×37), centred on the bar.
        let (px, z) = zoom_to_bar(0.0, 1.0, (0.0, 900.0), 600, 900.0, 56.0, 1000.0);
        assert!((z - 600.0 * 56.0 / 900.0).abs() < 1e-3 && (px - (0.5 - 0.5 / z)).abs() < 1e-4, "{px} {z}");
        // Capped by the map maximum, but always more than the current zoom (×2 at least).
        assert_eq!(zoom_to_bar(0.0, 1.0, (0.0, 900.0), 600, 900.0, 56.0, 16.0).1, 16.0);
        assert_eq!(zoom_to_bar(0.0, 3.0, (100.0, 900.0), 2, 900.0, 56.0, 100.0).1, 6.0, "few files: at least ×2 of the current zoom");
        assert_eq!(zoom_to_bar(0.0, 16.0, (100.0, 900.0), 600, 900.0, 56.0, 16.0).1, 16.0, "at the maximum: nothing more");
        // Drill-down: 10 000 files in 900 pt, the map maximum ×4: the first click leaves smaller
        // bars with their own counts; a click on one of them zooms further, centred on it.
        let files: Vec<LsmFile> = (0..10_000u32).map(|k| file(k as u64, (k, k + 1), 0, k as usize)).collect();
        let many = LsmInventory {
            cfs: vec![LsmCf {
                id: 0,
                name: "d".into(),
                levels: vec![LsmLevel { level: 0, files, bytes: 1, on_disk: 10_000, scale: 10_000, lanes: 1, overlaps: false }],
                comparator: None,
                order: KeyOrder::Bytewise,
            }],
            ..Default::default()
        };
        let g = Geometry { visible: [0.0, 0.0, 900.0, 1000.0], ..geo(900.0) };
        let l1 = layout(&many, Some(0), (0.0, 0.0), 1.0, &g);
        let bar = l1.boxes.iter().find_map(|b| if let BoxKind::Density { count, .. } = b.kind { Some((b.rect, count)) } else { None }).unwrap();
        let (px, z) = zoom_to_bar(0.0, 1.0, (bar.0[0], bar.0[2]), bar.1, 900.0, 56.0, 4.0);
        assert_eq!(z, 4.0);
        let l2 = layout(&many, Some(0), (px, 0.0), z, &g);
        let subs: Vec<_> = l2.boxes.iter().filter_map(|b| if let BoxKind::Density { count, .. } = b.kind { Some((b.rect, count)) } else { None }).collect();
        assert!(!subs.is_empty() && subs.iter().all(|s| s.1 < bar.1), "smaller bars with their counts: {:?}", subs.iter().map(|s| s.1).collect::<Vec<_>>());
        let sub = subs[subs.len() / 2];
        let (px2, z2) = zoom_to_bar(px, z, (sub.0[0], sub.0[2]), sub.1, 900.0, 56.0, 1000.0);
        assert!(z2 > z, "a second click zooms further: {z} -> {z2}");
        let full = 900.0 * z;
        let centre = ((sub.0[0] + sub.0[2]) / 2.0 + px * full) / full;
        assert!((px2 + 0.5 / z2 - centre).abs() < 1e-3, "centred on the sub-bar");
        // A file 1/1000 wide in 500 pt (0.5 pt < 3 pt): readable 50 pt needs ×100, centred.
        let (px, z) = reveal_zoom(0.5, 0.501, 500.0, 1.0, 50.0, 3.0, 1000.0).expect("merged");
        assert!((z - 100.0).abs() < 0.1 && (px - (0.5005 - 0.5 / z)).abs() < 1e-4, "{px} {z}");
        assert_eq!(reveal_zoom(0.5, 0.501, 500.0, 1.0, 50.0, 3.0, 40.0).map(|x| x.1), Some(40.0), "capped");
        assert_eq!(reveal_zoom(0.0, 0.5, 500.0, 1.0, 50.0, 3.0, 40.0), None, "already a box");
        assert_eq!(tier_after_zoom(LaneTier::Bar, 1.0, 1.5), LaneTier::Full);
        assert_eq!(tier_after_zoom(LaneTier::Compact, 2.0, 1.0), LaneTier::Compact);
        assert_eq!(tier_after_zoom(LaneTier::Bar, 1.0, 1.0), LaneTier::Bar);
    }

    /// 0030 A1: lane tiers. Scene heights per tier equal the layout's; Fit picks the largest tier
    /// that fits the region (top-aligned), Bar centred when even Bar overflows; zoom-in returns
    /// to Full, Center keeps zoom and tier and centres both axes; the measure (Full, ×1) decides
    /// Fits / Overflow, so the tier never feeds back; entering an overflowing map applies Fit
    /// once, a restored snapshot (Back) keeps its tier; a level with a density bar is never
    /// lower than Compact.
    #[test]
    fn lane_tiers_fit_both_dimensions_center_and_zoom() {
        let i = inv(KeyOrder::Bytewise);
        let tier_geo = |t: LaneTier| Geometry { lane_h: [20.0, 12.0, 8.0][t.index()], dense_lane_h: 12.0, ..geo(600.0) };
        let h = LaneTier::ALL.map(|t| scene_height(&i, Some(0), &tier_geo(t)));
        for t in LaneTier::ALL {
            assert_eq!(h[t.index()], layout(&i, Some(0), (0.0, 0.0), 1.0, &tier_geo(t)).height, "{t:?}");
        }
        assert_eq!(h, [144.0, 120.0, 108.0]);
        assert_eq!((fit_tier(&h, 200.0), fit_tier(&h, 130.0), fit_tier(&h, 110.0), fit_tier(&h, 50.0)), (LaneTier::Full, LaneTier::Compact, LaneTier::Bar, LaneTier::Bar));
        // A 130-pt region: Overflow (Full 144 does not fit); the first decision fits Compact.
        let mut s = MapState::default();
        s.set_heights(h);
        let m = measure(&i, Some(0), &tier_geo(LaneTier::Full), input(130.0));
        assert_eq!(m.scene_h, 144.0, "the measure is at Full");
        s.set_measure(m);
        assert!(s.navigable());
        assert_eq!((s.tier(), s.zoom(), s.pan()), (LaneTier::Compact, 1.0, (0.0, 0.0)), "Fit on entering: Compact, top-aligned");
        // An untouched fitted view follows a settling area (110 pt: Bar, top; then back).
        s.set_measure(MapMeasure { view_h: 110.0, ..m });
        assert_eq!((s.tier(), s.pan()), (LaneTier::Bar, (0.0, 0.0)), "still fitted");
        s.set_measure(MapMeasure { view_h: 130.0, ..m });
        assert_eq!(s.tier(), LaneTier::Compact);
        s.zoom_about(2.0, 0.5, 10.0);
        assert_eq!(s.tier(), LaneTier::Full, "zoom-in asks for detail");
        s.center();
        assert_eq!((s.zoom(), s.tier()), (2.0, LaneTier::Full), "Center keeps zoom and tier");
        assert_eq!(s.pan(), (0.25, 7.0), "and centres both axes ((144 − 130) / 2)");
        s.zoom_about(0.5, 0.5, 10.0);
        assert_eq!(s.tier(), LaneTier::Full, "zoom-out keeps Full; only Fit reduces");
        s.fit();
        assert_eq!((s.tier(), s.zoom(), s.pan()), (LaneTier::Compact, 1.0, (0.0, 0.0)));
        // A resize that does not change the decision keeps the tier (no re-Fit under the user).
        s.zoom_about(2.0, 0.5, 10.0);
        s.set_measure(MapMeasure { view_h: 120.0, ..m });
        assert_eq!(s.tier(), LaneTier::Full);
        // Even Bar overflows (100 pt): Bar, centred vertically ((108 − 100) / 2).
        s.set_measure(MapMeasure { view_h: 100.0, ..m });
        s.fit();
        assert_eq!((s.tier(), s.pan()), (LaneTier::Bar, (0.0, 4.0)));
        // Back keeps a restored tier and view; the next decision does not re-Fit.
        let snap = s.snapshot();
        let mut b = MapState::default();
        b.restore(&snap);
        b.set_heights(h);
        b.set_measure(MapMeasure { view_h: 100.0, ..m });
        assert_eq!((b.tier(), b.pan()), (LaneTier::Bar, (0.0, 4.0)));
        // The tier never decides Fits / Overflow: a fitting scene is Full, zoom 1, top.
        let mut f = MapState::default();
        f.set_heights(h);
        f.set_measure(MapMeasure { view_h: 400.0, ..m });
        assert_eq!((f.navigable(), f.tier(), f.pan()), (false, LaneTier::Full, (0.0, 0.0)));
        // A level with a density bar keeps the Compact lane height at the Bar tier.
        let files: Vec<LsmFile> = (0..1000u32).map(|k| file(k as u64, (k, k + 1), 0, k as usize)).collect();
        let dense = LsmInventory {
            cfs: vec![LsmCf {
                id: 0,
                name: "d".into(),
                levels: vec![LsmLevel { level: 0, files, bytes: 1, on_disk: 1000, scale: 1000, lanes: 1, overlaps: false }],
                comparator: None,
                order: KeyOrder::Bytewise,
            }],
            ..Default::default()
        };
        let l = layout(&dense, Some(0), (0.0, 0.0), 1.0, &tier_geo(LaneTier::Bar));
        assert_eq!(l.rows[0].lane_h, 12.0);
        assert!(l.boxes.iter().all(|b| b.rect[3] - b.rect[1] == 12.0));
    }

    /// 0026 §6: fits/overflow from the zoom-1 measure; leaving navigation needs 24 pt width and
    /// one lane of height to spare; decided only when the area changes by more than 2 pt (or
    /// CF / inventory / app zoom); Center (current zoom) vs Fit (zoom 1); pan clamped.
    #[test]
    fn conditional_navigation_fits_overflow_center_fit_and_pan_clamp() {
        let i = inv(KeyOrder::Bytewise);
        let g = geo(600.0);
        // L0 frame 67 + 4, empty L1 20 + 4, L2 45 + 4 = 144; narrowest box 200 pt.
        let m = measure(&i, Some(0), &g, input(200.0));
        assert_eq!((m.scene_h, m.narrowest, m.merged, m.files), (144.0, 200.0, false, 5));
        let l = layout(&i, Some(0), (0.0, 0.0), 1.0, &g);
        assert_eq!(m.scene_h, l.height, "measure and layout agree on the scene height");
        let mut s = MapState::default();
        assert_eq!(s.nav(), NavState::Overflow, "unmeasured: navigation stays available");
        s.set_view((0.2, 5.0), 3.0);
        assert!(s.set_measure(m));
        assert_eq!((s.nav(), s.zoom(), s.pan()), (NavState::Fits, 1.0, (0.0, 0.0)), "a fitting scene is stationary at zoom 1");
        // Too low by 1 pt: overflow.
        s.set_measure(MapMeasure { view_h: 143.0, ..m });
        assert_eq!(s.nav(), NavState::Overflow);
        // Taller again, but within the one-lane margin (144 > 160 − 20): still navigation.
        s.set_measure(MapMeasure { view_h: 160.0, ..m });
        assert_eq!(s.nav(), NavState::Overflow);
        // With the margin (165 − 20 ≥ 144): Fits; then 160 does not switch back (no flicker).
        s.set_measure(MapMeasure { view_h: 165.0, ..m });
        assert_eq!(s.nav(), NavState::Fits);
        for h in [160.0, 165.0, 158.0, 165.0] {
            s.set_measure(MapMeasure { view_h: h, ..m });
            assert_eq!(s.nav(), NavState::Fits, "{h}");
        }
        // Width: leaving navigation needs readable + 24 (200 < 180 + 24).
        s.set_measure(MapMeasure { readable_w: 201.0, ..m });
        assert_eq!(s.nav(), NavState::Overflow);
        s.set_measure(MapMeasure { readable_w: 180.0, ..m });
        assert_eq!(s.nav(), NavState::Overflow, "within the width margin");
        s.set_measure(MapMeasure { readable_w: 170.0, ..m });
        assert_eq!(s.nav(), NavState::Fits);
        // An area change up to 2 pt is not a decision point.
        s.set_measure(MapMeasure { view_h: 145.0, ..m });
        assert_eq!(s.nav(), NavState::Fits);
        assert!(!s.set_measure(MapMeasure { view_h: 143.5, ..m }), "1.5 pt: no decision");
        assert_eq!(s.nav(), NavState::Fits);
        assert!(s.set_measure(MapMeasure { view_h: 142.0, ..m }));
        assert_eq!(s.nav(), NavState::Overflow);
        // A density bar at zoom 1: navigation.
        s.set_measure(MapMeasure { merged: true, ..m });
        assert!(s.navigable());
        // Overflowing scene 144 in 50 pt: pan clamped to 94; Center at ×4 keeps the zoom; Fit
        // keeps navigation (user actions never switch the mode).
        s.set_measure(MapMeasure { view_h: 50.0, ..m });
        s.set_view((0.0, 1000.0), 4.0);
        assert_eq!(s.pan().1, 94.0, "never stranded below the scene");
        s.center();
        assert_eq!((s.zoom(), s.pan()), (4.0, (0.375, 47.0)));
        s.fit();
        // 0030 A1-4: without tier heights (not yet measured) Fit is ×1 at the top.
        assert_eq!((s.zoom(), s.pan(), s.nav()), (1.0, (0.0, 0.0), NavState::Overflow));
        // Non-finite measures are ignored.
        assert!(!s.set_measure(MapMeasure { scene_h: f32::NAN, ..m }));
        assert_eq!(s.measure().map(|x| x.view_h), Some(50.0));
        // A restored snapshot is clamped too, and carries the pick.
        s.restore(&MapSnapshot { pan: (0.0, 500.0), zoom: 2.0, pick: Some(MapPick::Wal(25)), ..Default::default() });
        assert_eq!((s.pan().1, s.wal(), s.zoom()), (94.0, Some(25), 2.0));
        // Fits: a restored zoom gives way to ×1 (§6.9).
        s.set_measure(MapMeasure { view_h: 400.0, ..m });
        s.restore(&MapSnapshot { pan: (0.3, 0.0), zoom: 3.0, ..Default::default() });
        assert_eq!((s.nav(), s.zoom()), (NavState::Fits, 1.0));
    }

    /// Many files: the measure is one bounded pass and reports the merge.
    #[test]
    fn measure_of_many_files_reports_merging() {
        let files: Vec<LsmFile> = (0..10_000u32).map(|i| file(i as u64, (i, i + 1), 0, i as usize)).collect();
        let l0 = LsmLevel { level: 0, files, bytes: 0, on_disk: 0, scale: 10_000, lanes: 1, overlaps: false };
        let i = LsmInventory { cfs: vec![LsmCf { id: 0, name: "d".into(), levels: vec![l0], comparator: None, order: KeyOrder::Bytewise }], ..Default::default() };
        let m = measure(&i, Some(0), &geo(800.0), input(600.0));
        assert!(m.merged && m.narrowest < 1.0);
        assert_eq!(m.nav(NavState::Fits), NavState::Overflow);
        assert_eq!(measure(&i, Some(9), &geo(800.0), input(600.0)).scene_h, 0.0, "unknown CF: empty measure");
    }

    /// 0026 §1.3 / §2: title hiding order, axis ends from observed boundary keys, empty lane
    /// sentences (≤ 3 names, never a key), and no forbidden phrase in any map text.
    #[test]
    fn titles_axis_and_empty_lane_text() {
        let i = inv(KeyOrder::Bytewise);
        let l = layout(&i, Some(0), (0.0, 0.0), 1.0, &geo(600.0));
        let text = |step| title_parts(&l.rows[0], true, step).into_iter().map(|p| p.0).collect::<String>();
        assert_eq!(text(0), "L0 · 3 files · 300 B · ✔ 3 on disk · 2 overlap lanes");
        assert_eq!(text(1), "L0 · 3 files · 300 B · ✔ 3 on disk");
        assert_eq!(text(2), "L0 · 3 files · ✔ 3 on disk");
        assert_eq!(text(3), "L0 · 3 files · ✔");
        let e = title_parts(&l.rows[1], true, 0);
        assert_eq!((e[0].clone(), e[1].0.as_str()), (("L1".to_string(), TitleRole::Level), " · empty (0 files in MANIFEST)"));
        let mut i2 = inv(KeyOrder::Bytewise);
        for (n, f) in i2.cfs[0].levels.iter_mut().flat_map(|l| l.files.iter_mut()).enumerate() {
            f.smallest = format!("u-{:03}", (n % 3) * 10).into_bytes();
            f.largest = format!("u-{:03}", (n % 3) * 10 + 14).into_bytes();
        }
        let c = &i2.cfs[0];
        assert_eq!(axis_ends(c), Some(("\"u-000\"".into(), "\"u-034\"".into())));
        assert_eq!(axis_middle(&c.order), "key rank, not to scale ->");
        assert_eq!(axis_middle(&KeyOrder::Unknown), "file number order ->");
        // D4: the strip's short legend is marked as cut.
        assert_eq!(short_legend_cut(&KeyOrder::Unsupported("x".into())), "x: file number order …");
        assert_eq!(short_legend_cut(&KeyOrder::Bytewise), "x: key order (bytewise) …");
        assert!(axis_ends(&inv(KeyOrder::Unknown).cfs[0]).is_none(), "no keys for an unknown order");
        assert!(lane_caption(c).is_some() && lane_caption(&inv(KeyOrder::Unsupported("x".into())).cfs[0]).is_none());
        // Lane 2 of L0 at x = 0.05 (left of file 2 at slots 1..4 of 6): covered by file 1.
        let t = empty_lane_text(c, 0, 1, 0.05).unwrap();
        assert_eq!(t, "L0 lane 2: no file in this lane here.\nCovered at this position by: 000001 (L0), 000004 (L2).\nLanes only stack overlapping L0 files.");
        let t2 = empty_lane_text(c, 2, 0, 0.99).unwrap();
        assert!(t2.contains("000003 (L0)") && t2.ends_with("Gaps are between file key ranges."), "{t2}");
        assert!(empty_lane_text(&inv(KeyOrder::Unknown).cfs[0], 0, 1, 0.5).is_none(), "no gap tooltip without key order");
        let (names, total) = cover_at(c, 0.5, 0, 1);
        assert_eq!((names.len(), total), (1, 2), "bounded names, full count");
        for s in [t, t2, text(0), c.order.legend(), lane_caption(c).unwrap().to_string()] {
            let s = s.to_lowercase();
            for bad in ["u-009", "missing keys", "no data", "lacks", "every key", "% of"] {
                assert!(!s.contains(bad), "{bad} in {s}");
            }
        }
    }

    #[test]
    fn keyboard_neighbours() {
        let i = inv(KeyOrder::Bytewise);
        let c = &i.cfs[0];
        let n = |from, m| neighbour(c, from, m).map(|f| f.number);
        assert_eq!(n(None, MapMove::Right), Some(1));
        assert_eq!(n(Some(1), MapMove::Right), Some(3), "same lane");
        assert_eq!(n(Some(3), MapMove::Left), Some(1));
        assert_eq!(n(Some(1), MapMove::Down), Some(2), "lane below in L0");
        assert_eq!(n(Some(2), MapMove::Down), Some(4), "skips the empty L1; nearest by x");
        assert_eq!(n(Some(5), MapMove::Up), Some(3));
        assert_eq!(n(Some(4), MapMove::End), Some(5));
        assert_eq!(n(Some(5), MapMove::Home), Some(4));
        assert_eq!(n(Some(4), MapMove::Down), None);
    }
}
