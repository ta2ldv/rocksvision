//! View preferences: theme id, zoom (policy + layout ceiling + notice), panel visibility (manual
//! only: nothing is hidden automatically on resize or zoom), layout epoch, center split and the
//! three dialogs. `persisted()` is the subset stored per user (theme id + zoom). Mutators are
//! visible to `app` only.

use super::prefs::{Prefs, DEFAULT_THEME};
use std::time::{Duration, Instant};

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
        let zoom = fit.at_zoom;
        let ratio = |avail: f32, need: f32| if need > 0.0 && avail > 0.0 && zoom > 0.0 { zoom * avail / need } else { self.max };
        let raw = ratio(fit.avail_w, fit.need_w).min(ratio(fit.avail_h, fit.need_h)).min(self.max);
        ((raw / self.step + 1e-3).floor() * self.step).max(1.0)
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

#[derive(Debug)]
pub struct ViewPreferences {
    /// Stable id of the active theme (resolved against the UI registry).
    theme_id: &'static str,
    zoom: f32,
    zoom_policy: ZoomPolicy,
    /// Current layout-aware zoom ceiling (recomputed from the UI's `ZoomFit` each frame).
    zoom_ceiling: f32,
    /// Records share of the center column (bytes below); session-only.
    center_split: f32,
    /// Transient status-bar notice (zoom limit messages, Reset View confirmation).
    notice: String,
    /// When a timed notice disappears (None: until replaced).
    notice_until: Option<Instant>,
    show_shortcuts: bool,
    show_about: bool,
    show_help: bool,
    show_left: bool,
    show_right: bool,
    show_hex: bool,
    /// Id salt of panels/tables; Reset View increments it so the UI forgets remembered sizes.
    layout_epoch: u32,
}

impl ViewPreferences {
    pub(super) fn new() -> Self {
        ViewPreferences {
            theme_id: DEFAULT_THEME,
            zoom: 1.0,
            zoom_policy: ZoomPolicy::DEFAULT,
            zoom_ceiling: ZoomPolicy::DEFAULT.max,
            center_split: CENTER_SPLIT_DEFAULT,
            notice: String::new(),
            notice_until: None,
            show_shortcuts: false,
            show_about: false,
            show_help: false,
            show_left: true,
            show_right: true,
            show_hex: true,
            layout_epoch: 0,
        }
    }

    pub fn theme_id(&self) -> &'static str {
        self.theme_id
    }
    pub fn zoom(&self) -> f32 {
        self.zoom
    }
    pub fn zoom_policy(&self) -> ZoomPolicy {
        self.zoom_policy
    }
    pub fn zoom_ceiling(&self) -> f32 {
        self.zoom_ceiling
    }
    pub fn center_split(&self) -> f32 {
        self.center_split
    }
    pub fn notice(&self) -> &str {
        &self.notice
    }
    /// Time left of a timed notice (the UI repaints then to clear it).
    pub fn notice_remaining(&self) -> Option<Duration> {
        self.notice_until.map(|t| t.saturating_duration_since(Instant::now()))
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
        self.show_left && self.show_right && self.show_hex && (self.zoom - 1.0).abs() < 1e-4 && (self.center_split - CENTER_SPLIT_DEFAULT).abs() < 1e-4
    }
    /// True if Zoom In would change the zoom (toolbar/menu enabled state).
    pub fn can_zoom_in(&self) -> bool {
        self.zoom_policy.zoom_in(self.zoom) <= self.zoom_ceiling + 1e-3 && self.zoom < self.zoom_policy.max - 1e-3
    }
    /// The persisted subset (theme id + zoom).
    pub fn persisted(&self) -> Prefs {
        Prefs { theme: self.theme_id.to_string(), zoom: self.zoom }
    }

    fn set_notice(&mut self, s: String) {
        self.notice = s;
        self.notice_until = None;
    }
    fn clear_notice(&mut self) {
        self.notice.clear();
        self.notice_until = None;
    }
    /// Drops a timed notice whose time is up.
    pub(super) fn expire_notice(&mut self, now: Instant) {
        if self.notice_until.map(|t| now >= t).unwrap_or(false) {
            self.clear_notice();
        }
    }
    pub(super) fn set_theme(&mut self, id: &'static str) {
        self.theme_id = id;
    }
    /// Zoom In through the layout-aware ceiling; at the ceiling it is a no-op with a notice.
    pub(super) fn zoom_in(&mut self) {
        let next = self.zoom_policy.zoom_in(self.zoom);
        if next > self.zoom_ceiling + 1e-3 {
            self.set_notice(format!("Zoom at layout limit ({:.0}%)", self.zoom_ceiling * 100.0));
        } else {
            self.clear_notice();
            self.zoom = next;
        }
    }
    pub(super) fn zoom_out(&mut self) {
        self.clear_notice();
        self.zoom = self.zoom_policy.zoom_out(self.zoom);
    }
    pub(super) fn zoom_reset(&mut self) {
        self.clear_notice();
        self.zoom = 1.0f32.min(self.zoom_ceiling);
    }
    pub(super) fn set_zoom(&mut self, z: f32) {
        self.zoom = self.zoom_policy.clamp(if z.is_finite() { z } else { 1.0 });
    }
    /// Recomputes the ceiling from this frame's layout measurements; lowers the zoom if the
    /// window no longer fits it (resize, theme/font change, panel toggles).
    pub(super) fn fit(&mut self, fit: ZoomFit) {
        self.zoom_ceiling = self.zoom_policy.ceiling(fit);
        if self.zoom > self.zoom_ceiling + 1e-3 {
            self.zoom = self.zoom_ceiling;
            self.set_notice(format!("Zoom reduced to {:.0}% to fit window", self.zoom * 100.0));
        }
    }
    pub(super) fn set_center_split(&mut self, r: f32) {
        self.center_split = r.clamp(0.0, 1.0);
    }
    pub(super) fn reset_center_split(&mut self) {
        self.center_split = CENTER_SPLIT_DEFAULT;
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
        self.center_split = CENTER_SPLIT_DEFAULT;
        self.zoom = 1.0;
        self.notice = RESET_NOTICE.into();
        self.notice_until = Some(now + Duration::from_secs(RESET_NOTICE_SECS));
    }
}
