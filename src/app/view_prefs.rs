//! View preferences: theme id, zoom (policy + layout ceiling + notice), panel visibility (manual
//! only: nothing is hidden automatically on resize or zoom), layout epoch, center split and the
//! three dialogs. `persisted()` is the subset stored per user (theme id + zoom). Mutators are
//! visible to `app` only.

use super::prefs::{DEFAULT_THEME, Prefs};
use std::time::{Duration, Instant};

/// How far the measured fit may fall below the current zoom before it is lowered (measurement
/// rounding between zoom levels; ≤ 2 % of the window, inside the layout margins).
pub const ZOOM_FIT_SLACK: f32 = 0.02;

/// Default records share of the center column (records above, bytes below).
pub const CENTER_SPLIT_DEFAULT: f32 = 0.42;

/// Layout measurements the UI reports each frame, in logical points at the current zoom:
/// available window size and the size the minimum layout needs (never data-dependent).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZoomFit {
    /// Zoom factor in effect when the sizes were measured.
    pub at_zoom: f32,
    pub avail_w: f32,
    pub avail_h: f32,
    pub need_w: f32,
    pub need_h: f32,
}

/// Zoom policy: bounds and step of the user zoom factor and the layout-aware ceiling. One
/// authoritative definition used by toolbar buttons, menu items, keyboard shortcuts and prefs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZoomPolicy {
    pub min: f32,
    /// Absolute cap; the effective ceiling also depends on the window (`ceiling`).
    pub max: f32,
    pub step: f32,
}

impl ZoomPolicy {
    pub const DEFAULT: ZoomPolicy = ZoomPolicy { min: 0.6, max: 2.0, step: 0.1 };

    /// Rounds to the step grid (avoids float drift of repeated ±step).
    fn snap(&self, z: f32) -> f32 {
        (z / self.step).round() * self.step
    }
    pub fn clamp(&self, z: f32) -> f32 {
        z.clamp(self.min, self.max)
    }
    pub fn zoom_in(&self, z: f32) -> f32 {
        self.snap(z + self.step).min(self.max)
    }
    pub fn zoom_out(&self, z: f32) -> f32 {
        self.snap(z - self.step).max(self.min)
    }
    /// Largest zoom (on the step grid, never below 1.0) at which the minimum layout fits:
    /// floor_to_step(min(zoom·avail_w/need_w, zoom·avail_h/need_h, max)).
    pub fn ceiling(&self, fit: ZoomFit) -> f32 {
        ((self.raw_ceiling(fit) / self.step + 1e-3).floor() * self.step).max(1.0)
    }
    /// The unrounded ratio behind `ceiling`.
    pub fn raw_ceiling(&self, fit: ZoomFit) -> f32 {
        let zoom = fit.at_zoom;
        let ratio = |avail: f32, need: f32| if need > 0.0 && avail > 0.0 && zoom > 0.0 { zoom * avail / need } else { self.max };
        ratio(fit.avail_w, fit.need_w).min(ratio(fit.avail_h, fit.need_h)).min(self.max)
    }
}

/// The three collapsible regions (side panels and the byte region of the center column).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Panel {
    Navigation,
    Explanation,
    Bytes,
}

/// Status message shown after Reset View (for `RESET_NOTICE_SECS`).
pub const RESET_NOTICE: &str = "View reset: panels, split and zoom restored (window size unchanged)";
pub const RESET_NOTICE_SECS: u64 = 4;
/// A copy confirmation / copy message is timed like the Reset View notice (0020 UI ruling F1).
pub const COPY_NOTICE_SECS: u64 = RESET_NOTICE_SECS;

/// Owner of the one transient status-bar notice (0020 UI ruling F1: one at a time, the newest
/// replaces the older one).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeOwner {
    /// "Zoom reduced to …% to fit window": lives while the clamp is current (untimed).
    Zoom,
    /// "Zoom at layout limit (…)": stale after a resize or any zoom command (untimed).
    ZoomLimit,
    /// Reset View confirmation (timed).
    Reset,
    /// Copy confirmation or copy message ("Copied …", "Copying…", "Nothing selected…"; timed).
    Copy,
    /// Navigation message: Back with an empty history ("No previous view"; timed).
    Nav,
}

/// The one transient status-bar notice and its owner and expiry.
#[derive(Debug, Default)]
struct TransientNotice {
    text: String,
    /// When a timed notice disappears (None: until replaced or cleared by its owner's rule).
    until: Option<Instant>,
    owner: Option<NoticeOwner>,
}

impl TransientNotice {
    fn set(&mut self, owner: NoticeOwner, text: String, until: Option<Instant>) {
        *self = TransientNotice { text, until, owner: Some(owner) };
    }
    /// Clears the notice if `owner` set it (an older notice of another owner was replaced).
    fn clear_if(&mut self, owner: NoticeOwner) {
        if self.owner == Some(owner) {
            *self = TransientNotice::default();
        }
    }
    fn expire(&mut self, now: Instant) {
        if self.until.is_some_and(|t| now >= t) {
            *self = TransientNotice::default();
        }
    }
}

/// A window size change above measurement rounding (points).
const RESIZE_EPS: f32 = 1.0;

/// What one layout measurement does to the effective zoom (`ZoomFitter::observe`).
#[derive(Clone, Copy, Debug, PartialEq)]
enum FitOutcome {
    Keep,
    /// The window cannot hold the zoom (two consecutive over-fit frames): lower it to this.
    Reduce(f32),
    /// The window holds more than the clamped zoom again: raise it (never above the chosen).
    Raise(f32),
    /// The window was resized and nothing is clamped or pending.
    Resized,
}

/// Transient zoom-fit state: the layout ceiling, the over-fit frame counter and the last
/// window size. Never persisted.
#[derive(Debug)]
struct ZoomFitter {
    policy: ZoomPolicy,
    /// Current layout-aware zoom ceiling (recomputed from the UI's `ZoomFit` each frame).
    ceiling: f32,
    /// Consecutive frames in which the zoom was clearly above the measured fit (0015 C3: a
    /// reduction needs two, so one transient frame — resize step, re-measure after a zoom
    /// change — cannot snap the zoom back).
    over_frames: u8,
    /// Window size (points at 100 %, `avail × at_zoom`) of the last layout fit: a change is a
    /// resize.
    last_window: Option<(f32, f32)>,
    /// The zoom the last reduction lowered from, until the window is resized: a Raise back to
    /// it (or above) is not tried in the same window (review N2: a measurement that differs
    /// between two zoom levels by more than `ZOOM_FIT_SLACK` would otherwise Raise → Reduce →
    /// Raise every few frames).
    reduced_from: Option<f32>,
}

impl ZoomFitter {
    fn new(policy: ZoomPolicy) -> Self {
        ZoomFitter { policy, ceiling: policy.max, over_frames: 0, last_window: None, reduced_from: None }
    }
    /// One frame's measurement at effective zoom `zoom` (user's chosen zoom `chosen`). The
    /// same rule for the first frame and every later one: a restored zoom in a small first
    /// window is lowered exactly like a resize to that window at that zoom.
    fn observe(&mut self, zoom: f32, chosen: f32, fit: ZoomFit) -> FitOutcome {
        let window = (fit.avail_w * fit.at_zoom, fit.avail_h * fit.at_zoom);
        let resized = self.last_window.is_some_and(|(w, h)| (w - window.0).abs() > RESIZE_EPS || (h - window.1).abs() > RESIZE_EPS);
        self.last_window = Some(window);
        if resized {
            self.reduced_from = None;
        }
        self.ceiling = self.policy.ceiling(fit);
        let over = zoom > self.ceiling + 1e-3 && self.policy.raw_ceiling(fit) < zoom - ZOOM_FIT_SLACK;
        self.over_frames = if over { self.over_frames.saturating_add(1) } else { 0 };
        if self.over_frames >= 2 {
            self.over_frames = 0;
            self.reduced_from = Some(zoom);
            return FitOutcome::Reduce(self.ceiling);
        }
        let target = chosen.min(self.ceiling);
        let retry = self.reduced_from.is_some_and(|r| target >= r - 1e-3);
        if self.over_frames == 0 && zoom < chosen - 1e-3 && self.ceiling > zoom + 1e-3 && !retry {
            // 0020 UI ruling (zoom return): effective = min(chosen, ceiling).
            return FitOutcome::Raise(target);
        }
        if resized && self.over_frames == 0 { FitOutcome::Resized } else { FitOutcome::Keep }
    }
}

/// Which records / bytes split applies to the center column (0030 A8): the file views' table,
/// the levels view's Table mode, or the LSM Map. Each keeps its own user split.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitRole {
    Records,
    LevelsTable,
    Map,
}

#[derive(Debug)]
pub struct ViewPreferences {
    /// Stable id of the active theme (resolved against the UI registry).
    theme_id: &'static str,
    /// Effective zoom: min(chosen, layout ceiling), lowered only after two over-fit frames.
    zoom: f32,
    /// The user's chosen zoom (persisted). A clamp to the window never changes it; Zoom In /
    /// Out / Actual Size / SetZoom / Reset View set it (0020 UI ruling, zoom return).
    chosen_zoom: f32,
    fitter: ZoomFitter,
    /// The user's records share of the file views (bytes below; None: `CENTER_SPLIT_DEFAULT`
    /// with the byte-pane content cap, 0030 A8-2); session-only.
    center_split: Option<f32>,
    /// The user's split while the LSM Map is shown (0026 D2 F2); None: the map takes all but
    /// the byte pane's minimum. Session-only; Table / other views keep `center_split`.
    map_split: Option<f32>,
    /// The user's split of the levels view in Table mode (0030 A8); None: the table gets what
    /// all its rows need, the bytes keeping what they keep under the Map. Session-only.
    levels_split: Option<f32>,
    /// The one transient status-bar notice (zoom, Reset View, copy).
    notice: TransientNotice,
    show_shortcuts: bool,
    show_about: bool,
    show_help: bool,
    show_left: bool,
    show_right: bool,
    show_hex: bool,
    /// Id salt of panels/tables; Reset View increments it so the UI forgets remembered sizes.
    layout_epoch: u32,
    /// Show the table columns that are hidden for lack of width ("+N columns"); session-only.
    all_columns: bool,
    /// View › Show Tooltips (persisted; default on).
    tooltips: bool,
}

impl ViewPreferences {
    pub(super) fn new() -> Self {
        ViewPreferences {
            theme_id: DEFAULT_THEME,
            zoom: 1.0,
            chosen_zoom: 1.0,
            fitter: ZoomFitter::new(ZoomPolicy::DEFAULT),
            center_split: None,
            map_split: None,
            levels_split: None,
            notice: TransientNotice::default(),
            show_shortcuts: false,
            show_about: false,
            show_help: false,
            show_left: true,
            show_right: true,
            show_hex: true,
            layout_epoch: 0,
            all_columns: false,
            tooltips: true,
        }
    }

    /// App-owned contextual tooltips are shown (View › Show Tooltips).
    pub fn tooltips(&self) -> bool {
        self.tooltips
    }
    pub(super) fn set_tooltips(&mut self, on: bool) {
        self.tooltips = on;
    }

    /// The user asked to show every table column even when they do not fit.
    pub fn all_columns(&self) -> bool {
        self.all_columns
    }
    pub(super) fn toggle_all_columns(&mut self) {
        self.all_columns = !self.all_columns;
    }

    pub fn theme_id(&self) -> &'static str {
        self.theme_id
    }
    /// The effective zoom (what egui renders at): min(chosen, layout ceiling).
    pub fn zoom(&self) -> f32 {
        self.zoom
    }
    /// The user's chosen (persisted) zoom; above `zoom()` while the window clamps it.
    pub fn chosen_zoom(&self) -> f32 {
        self.chosen_zoom
    }
    pub fn zoom_policy(&self) -> ZoomPolicy {
        self.fitter.policy
    }
    pub fn zoom_ceiling(&self) -> f32 {
        self.fitter.ceiling
    }
    /// The file views' effective records ratio (kept for the integration tests; the read API of
    /// the splits is `split(role)`, architect N8).
    pub fn center_split(&self) -> f32 {
        self.center_split.unwrap_or(CENTER_SPLIT_DEFAULT)
    }
    /// The user's records share for a split role; None: that role's automatic split
    /// (None for every role the user has not dragged).
    pub fn split(&self, role: SplitRole) -> Option<f32> {
        match role {
            SplitRole::Records => self.center_split,
            SplitRole::LevelsTable => self.levels_split,
            SplitRole::Map => self.map_split,
        }
    }
    /// The one transient status-bar notice (zoom, Reset View or copy), or "".
    pub fn notice(&self) -> &str {
        &self.notice.text
    }
    pub fn notice_owner(&self) -> Option<NoticeOwner> {
        self.notice.owner
    }
    /// Time left of a timed notice (the UI repaints then to clear it).
    pub fn notice_remaining(&self) -> Option<Duration> {
        self.notice.until.map(|t| t.saturating_duration_since(Instant::now()))
    }
    pub fn show_shortcuts(&self) -> bool {
        self.show_shortcuts
    }
    pub fn show_about(&self) -> bool {
        self.show_about
    }
    pub fn show_help(&self) -> bool {
        self.show_help
    }
    /// A modal dialog (About, Keyboard Shortcuts, Help) is open.
    pub fn modal_open(&self) -> bool {
        self.show_about || self.show_shortcuts || self.show_help
    }
    pub fn show_left(&self) -> bool {
        self.show_left
    }
    pub fn show_right(&self) -> bool {
        self.show_right
    }
    pub fn show_hex(&self) -> bool {
        self.show_hex
    }
    pub fn shown(&self, p: Panel) -> bool {
        match p {
            Panel::Navigation => self.show_left,
            Panel::Explanation => self.show_right,
            Panel::Bytes => self.show_hex,
        }
    }
    pub fn layout_epoch(&self) -> u32 {
        self.layout_epoch
    }
    /// Visibility, split and zoom are at their Reset View defaults (panel widths are UI memory
    /// and checked by the shell).
    pub fn is_default_layout(&self) -> bool {
        self.show_left
            && self.show_right
            && self.show_hex
            && !self.all_columns
            && (self.chosen_zoom - 1.0).abs() < 1e-4
            && self.center_split.is_none()
            && self.map_split.is_none()
            && self.levels_split.is_none()
    }
    /// True if Zoom In would change the zoom (toolbar/menu enabled state).
    pub fn can_zoom_in(&self) -> bool {
        let p = self.fitter.policy;
        p.zoom_in(self.zoom) <= self.fitter.ceiling + 1e-3 && self.zoom < p.max - 1e-3
    }
    /// The persisted subset (theme id, the chosen zoom — never the clamped one — and tooltips).
    pub fn persisted(&self) -> Prefs {
        Prefs { theme: self.theme_id.to_string(), zoom: self.chosen_zoom, tooltips: self.tooltips }
    }

    /// Sets the one transient notice (replaces any older one). `secs`: timed, else untimed.
    pub(super) fn notify(&mut self, owner: NoticeOwner, text: impl Into<String>, now: Instant, secs: Option<u64>) {
        self.notice.set(owner, text.into(), secs.map(|s| now + Duration::from_secs(s)));
    }
    /// Clears the notice if `owner` set it.
    pub(super) fn clear_notice(&mut self, owner: NoticeOwner) {
        self.notice.clear_if(owner);
    }
    /// A zoom command or an unclamped zoom makes both zoom notices stale; a Reset View or
    /// copy notice keeps its timer.
    fn clear_zoom_notices(&mut self) {
        self.notice.clear_if(NoticeOwner::Zoom);
        self.notice.clear_if(NoticeOwner::ZoomLimit);
    }
    fn set_reduced_notice(&mut self) {
        self.notice.set(NoticeOwner::Zoom, format!("Zoom reduced to {:.0}% to fit window", self.zoom * 100.0), None);
    }
    /// Drops a timed notice whose time is up.
    pub(super) fn expire_notice(&mut self, now: Instant) {
        self.notice.expire(now);
    }
    /// A theme change re-measures the layout: a zoom reduced under the old theme may return.
    pub(super) fn set_theme(&mut self, id: &'static str) {
        self.theme_id = id;
        self.fitter.reduced_from = None;
    }
    /// A zoom command: the new chosen zoom, shown as is (the next fits clamp it if needed).
    fn choose_zoom(&mut self, z: f32) {
        self.fitter.reduced_from = None;
        self.clear_zoom_notices();
        self.chosen_zoom = z;
        self.zoom = z;
    }
    /// Zoom In through the layout-aware ceiling (from the effective zoom); at the ceiling it is a
    /// no-op with a notice, and the effective zoom becomes the chosen one.
    pub(super) fn zoom_in(&mut self) {
        let next = self.fitter.policy.zoom_in(self.zoom);
        if next > self.fitter.ceiling + 1e-3 {
            self.chosen_zoom = self.zoom;
            self.notice.set(NoticeOwner::ZoomLimit, format!("Zoom at layout limit ({:.0}%)", self.fitter.ceiling * 100.0), None);
        } else {
            self.choose_zoom(next);
        }
    }
    pub(super) fn zoom_out(&mut self) {
        self.choose_zoom(self.fitter.policy.zoom_out(self.zoom));
    }
    pub(super) fn zoom_reset(&mut self) {
        self.choose_zoom(1.0f32.min(self.fitter.ceiling));
    }
    pub(super) fn set_zoom(&mut self, z: f32) {
        self.choose_zoom(self.fitter.policy.clamp(if z.is_finite() { z } else { 1.0 }));
    }
    /// Recomputes the ceiling from this frame's layout measurements (`ZoomFitter::observe`).
    /// Effective zoom = min(chosen, ceiling): lowered when the window no longer fits it
    /// (resize, theme/font change, panel toggles; two consecutive clearly-over frames, see
    /// `ZOOM_FIT_SLACK`), raised back toward the chosen zoom as soon as the window holds more
    /// (0020 UI ruling, zoom return). Zoom In/A+ use the same ceiling.
    ///
    /// The "Zoom reduced to {z}% to fit window" notice lives while the clamp is current: set (or
    /// updated in place) by a reduction or a partial return, cleared when the chosen zoom is
    /// back; a resize also clears a "layout limit" notice.
    pub(super) fn fit(&mut self, fit: ZoomFit) {
        match self.fitter.observe(self.zoom, self.chosen_zoom, fit) {
            FitOutcome::Keep => self.keep_reduced_notice(),
            FitOutcome::Reduce(z) => {
                self.zoom = z;
                self.set_reduced_notice();
            }
            FitOutcome::Raise(z) => {
                self.zoom = z;
                self.notice.clear_if(NoticeOwner::ZoomLimit);
                if (z - self.chosen_zoom).abs() < 1e-3 {
                    self.notice.clear_if(NoticeOwner::Zoom);
                } else if self.notice.owner == Some(NoticeOwner::Zoom) {
                    self.set_reduced_notice();
                } else {
                    self.keep_reduced_notice();
                }
            }
            FitOutcome::Resized => {
                self.notice.clear_if(NoticeOwner::ZoomLimit);
                if self.zoom >= self.chosen_zoom - 1e-3 {
                    self.notice.clear_if(NoticeOwner::Zoom);
                } else {
                    self.keep_reduced_notice();
                }
            }
        }
    }
    /// Review S1: the reduced notice comes back when the slot is free (a copy or Reset View
    /// notice expired) while the clamp is still current; another owner's notice is kept.
    fn keep_reduced_notice(&mut self) {
        if self.notice.owner.is_none() && self.zoom < self.chosen_zoom - 1e-3 {
            self.set_reduced_notice();
        }
    }
    /// A reduction is pending (one over-fit frame seen): the UI repaints once more to confirm.
    pub fn zoom_fit_pending(&self) -> bool {
        self.fitter.over_frames > 0
    }
    /// A split drag: stored for the role shown (file views' table, levels table or LSM Map);
    /// the other roles keep theirs, so switching restores them (0026 D2 F2, 0030 A8).
    pub(super) fn set_split(&mut self, role: SplitRole, r: f32) {
        let r = r.clamp(0.0, 1.0);
        match role {
            SplitRole::Records => self.center_split = Some(r),
            SplitRole::LevelsTable => self.levels_split = Some(r),
            SplitRole::Map => self.map_split = Some(r),
        }
    }
    /// Double-click on the split handle (0030 A8-5): only the role shown goes back to its
    /// automatic split; the others (e.g. the Map's) are kept. Reset View resets all.
    pub(super) fn reset_split(&mut self, role: SplitRole) {
        match role {
            SplitRole::Records => self.center_split = None,
            SplitRole::LevelsTable => self.levels_split = None,
            SplitRole::Map => self.map_split = None,
        }
    }
    pub(super) fn set_show_shortcuts(&mut self, b: bool) {
        self.show_shortcuts = b;
    }
    pub(super) fn set_show_about(&mut self, b: bool) {
        self.show_about = b;
    }
    pub(super) fn set_show_help(&mut self, b: bool) {
        self.show_help = b;
    }
    pub(super) fn set_shown(&mut self, p: Panel, b: bool) {
        match p {
            Panel::Navigation => self.show_left = b,
            Panel::Explanation => self.show_right = b,
            Panel::Bytes => self.show_hex = b,
        }
    }
    /// Layout reset: panel visibility, center split, table columns (epoch) and 100 % zoom, then
    /// a timed confirmation notice. The OS window size is not part of the view.
    pub(super) fn reset_layout(&mut self, now: Instant) {
        self.layout_epoch = self.layout_epoch.wrapping_add(1);
        self.show_left = true;
        self.show_right = true;
        self.show_hex = true;
        self.center_split = None;
        self.map_split = None;
        self.levels_split = None;
        self.zoom = 1.0;
        self.chosen_zoom = 1.0;
        self.all_columns = false;
        self.notify(NoticeOwner::Reset, RESET_NOTICE, now, Some(RESET_NOTICE_SECS));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// need 875×560 pt at any zoom: 1400×900 fits 160 %, 900×700 fits 100 %.
    fn at(w: f32, h: f32, z: f32) -> ZoomFit {
        ZoomFit { at_zoom: z, avail_w: w / z, avail_h: h / z, need_w: 875.0, need_h: 560.0 }
    }

    /// 0020 §5 / UI ruling (zoom return): one rule for the first frame and every later one.
    /// A restored 160 % in a small first window behaves exactly like a resize to that window at
    /// 160 %: Keep after one over-fit frame, Reduce after two; a single transient frame is
    /// Keep; a larger window raises the effective zoom back to the chosen one.
    #[test]
    fn zoom_fitter_observe_table() {
        let fresh = || ZoomFitter::new(ZoomPolicy::DEFAULT);
        // (name, frames before (window, zoom), then the observed frames, expected outcomes)
        let restored: Vec<FitOutcome> = {
            let mut f = fresh();
            vec![f.observe(1.6, 1.6, at(900.0, 700.0, 1.6)), f.observe(1.6, 1.6, at(900.0, 700.0, 1.6))]
        };
        let resized: Vec<FitOutcome> = {
            let mut f = fresh();
            assert_eq!(f.observe(1.6, 1.6, at(1400.0, 900.0, 1.6)), FitOutcome::Keep);
            vec![f.observe(1.6, 1.6, at(900.0, 700.0, 1.6)), f.observe(1.6, 1.6, at(900.0, 700.0, 1.6))]
        };
        assert_eq!(restored, vec![FitOutcome::Keep, FitOutcome::Reduce(1.0)], "restored 160 % in a small first frame");
        assert_eq!(resized, restored, "≡ a resize to the same window at 160 %");
        let mut f = fresh();
        f.observe(1.6, 1.6, at(1400.0, 900.0, 1.6));
        assert_eq!(f.observe(1.6, 1.6, at(900.0, 700.0, 1.6)), FitOutcome::Keep, "one transient frame");
        assert_eq!(f.observe(1.6, 1.6, at(1400.0, 900.0, 1.6)), FitOutcome::Resized, "back before a second over frame: nothing changes");
        assert!(f.over_frames == 0);
        // Clamped at 100 % (chosen 160 %): a larger window raises the zoom back.
        let mut f = fresh();
        f.observe(1.0, 1.6, at(900.0, 700.0, 1.0));
        assert_eq!(f.observe(1.0, 1.6, at(1100.0, 700.0, 1.0)), FitOutcome::Raise(1.2), "partial return: min(chosen, ceiling)");
        assert_eq!(f.observe(1.2, 1.6, at(1400.0, 900.0, 1.2)), FitOutcome::Raise(1.6), "full return to the chosen zoom");
        assert_eq!(f.observe(1.6, 1.6, at(1400.0, 900.0, 1.6)), FitOutcome::Keep, "never above the chosen zoom");
        // Review N2: a steady 1000 pt window whose measured fit depends on the zoom it is
        // measured at (1.31 at ≤ 125 %, 1.27 above: more than ZOOM_FIT_SLACK apart). Chosen
        // 160 %: one Raise to 130 %, the Reduce back to 120 %, then no Raise/Reduce flip-flop.
        let steady = |z: f32| {
            let raw = if z <= 1.25 { 1.31 } else { 1.27 };
            ZoomFit { at_zoom: z, avail_w: 1000.0 / z, avail_h: 10_000.0 / z, need_w: 1000.0 / raw, need_h: 1.0 }
        };
        let (mut f, mut z) = (fresh(), 1.2f32);
        let mut seen = Vec::new();
        for _ in 0..12 {
            let o = f.observe(z, 1.6, steady(z));
            if let FitOutcome::Raise(n) | FitOutcome::Reduce(n) = o {
                z = n;
            }
            seen.push(match o {
                FitOutcome::Raise(n) => format!("Raise({n:.1})"),
                FitOutcome::Reduce(n) => format!("Reduce({n:.1})"),
                o => format!("{o:?}"),
            });
        }
        let mut want = vec!["Raise(1.3)", "Keep", "Reduce(1.2)"];
        want.extend(["Keep"; 9]);
        assert_eq!(seen, want, "steady window at a fractional fit");
        assert!((z - 1.2).abs() < 1e-3);
        // A resize (the window grows) allows the return again.
        assert_eq!(f.observe(1.2, 1.6, at(1400.0, 900.0, 1.2)), FitOutcome::Raise(1.6), "resized: return to the chosen zoom");
    }

    /// Chosen vs effective zoom and the one notice slot (newest replaces, owner-cleared,
    /// timed copy notice).
    #[test]
    fn chosen_zoom_survives_the_clamp_and_one_notice_slot() {
        let mut p = ViewPreferences::new();
        p.set_zoom(1.6);
        p.fit(at(1400.0, 900.0, 1.6));
        p.fit(at(900.0, 700.0, 1.6));
        p.fit(at(900.0, 700.0, 1.6));
        assert_eq!((p.zoom(), p.chosen_zoom(), p.persisted().zoom, p.notice()), (1.0, 1.6, 1.6, "Zoom reduced to 100% to fit window"));
        let now = Instant::now();
        p.notify(NoticeOwner::Copy, "Copied offset range", now, Some(COPY_NOTICE_SECS));
        assert_eq!((p.notice(), p.notice_owner()), ("Copied offset range", Some(NoticeOwner::Copy)), "newest replaces");
        // Review S1: the copy notice expires while the clamp is current → the reduced notice
        // returns with the next fit (same window), and stays while the clamp does.
        p.fit(at(900.0, 700.0, 1.0));
        assert_eq!(p.notice(), "Copied offset range", "a live copy notice is kept");
        p.expire_notice(now + Duration::from_secs(COPY_NOTICE_SECS));
        assert_eq!(p.notice(), "");
        p.fit(at(900.0, 700.0, 1.0));
        assert_eq!((p.notice(), p.notice_owner()), ("Zoom reduced to 100% to fit window", Some(NoticeOwner::Zoom)), "clamp still current");
        p.fit(at(900.0, 700.0, 1.0));
        assert_eq!(p.notice(), "Zoom reduced to 100% to fit window");
        p.notify(NoticeOwner::Copy, "Copied offset range", now, Some(COPY_NOTICE_SECS));
        p.fit(at(1400.0, 900.0, 1.0));
        assert_eq!((p.zoom(), p.notice()), (1.6, "Copied offset range"), "zoom returns; a copy notice is not the zoom's to clear");
        p.expire_notice(now + Duration::from_secs(COPY_NOTICE_SECS - 1));
        assert_eq!(p.notice(), "Copied offset range");
        p.expire_notice(now + Duration::from_secs(COPY_NOTICE_SECS));
        assert_eq!(p.notice(), "", "copy notice expires after 4 s");
        p.clear_notice(NoticeOwner::Zoom);
        assert_eq!(p.notice_owner(), None);
    }
}
