//! Keyboard shortcuts → `Action`s. Accelerators that the native menu bar owns (macOS) are not
//! handled here, so every key press triggers its action exactly once. Enter belongs to the
//! focused context: in the search field it steps through matches (Shift = previous); in another
//! text field (path, offset) that field handles it; otherwise it follows the selected row.

use crate::app::{Action, AppState, Panel, RowMove};
use eframe::egui;

/// Keys pressed this frame (egui-independent, so the mapping is unit-testable).
#[derive(Clone, Copy, Debug, Default)]
pub struct Keys {
    /// Table/tree navigation allowed (no text field focused, no modal open).
    pub nav: bool,
    /// The search field had keyboard focus when the key was pressed.
    pub search_focused: bool,
    pub up: bool,
    pub down: bool,
    pub pgup: bool,
    pub pgdn: bool,
    pub home: bool,
    pub end: bool,
    pub left: bool,
    pub right: bool,
    pub enter: bool,
    pub shift: bool,
    pub f3: bool,
    pub shift_f3: bool,
    pub alt_left: bool,
    pub esc: bool,
    /// `]` / `[`: next / previous field; with Shift: next / previous structure.
    pub close_bracket: bool,
    pub open_bracket: bool,
    // ---- accelerators that a native menu owns ----
    pub cmd_o: bool,
    pub shift_cmd_o: bool,
    pub cmd_l: bool,
    pub cmd_f: bool,
    pub cmd_g: bool,
    pub shift_cmd_g: bool,
    pub cmd_plus: bool,
    pub cmd_minus: bool,
    pub cmd_0: bool,
    pub shift_cmd_r: bool,
    /// Option+⌘1/2/3: toggle Navigation / Explanation / Byte View.
    pub alt_cmd_digit: Option<u8>,
}

pub fn read(ctx: &egui::Context, st: &AppState, menu_owns_accelerators: bool, search_id: egui::Id) -> Vec<Action> {
    let nav = !ctx.egui_wants_keyboard_input() && !st.prefs().modal_open();
    let search_focused = ctx.memory(|m| m.has_focus(search_id)) && !st.prefs().modal_open();
    let k = ctx.input(|i| {
        let c = i.modifiers.command;
        let s = i.modifiers.shift;
        let a = i.modifiers.alt;
        let p = |k| i.key_pressed(k);
        use egui::Key as K;
        Keys {
            nav,
            search_focused,
            up: p(K::ArrowUp),
            down: p(K::ArrowDown),
            pgup: p(K::PageUp),
            pgdn: p(K::PageDown),
            home: p(K::Home),
            end: p(K::End),
            left: p(K::ArrowLeft) && !a,
            right: p(K::ArrowRight) && !a,
            enter: p(K::Enter),
            shift: s,
            f3: p(K::F3) && !s,
            shift_f3: p(K::F3) && s,
            alt_left: a && p(K::ArrowLeft),
            esc: p(K::Escape),
            // Shift+] arrives as "}" on most layouts; both mean the same key here.
            close_bracket: !c && !a && (p(K::CloseBracket) || p(K::CloseCurlyBracket)),
            open_bracket: !c && !a && (p(K::OpenBracket) || p(K::OpenCurlyBracket)),
            cmd_o: c && !s && p(K::O),
            shift_cmd_o: c && s && p(K::O),
            cmd_l: c && p(K::L),
            cmd_f: c && p(K::F),
            cmd_g: c && !s && p(K::G),
            shift_cmd_g: c && s && p(K::G),
            cmd_plus: c && (p(K::Plus) || p(K::Equals)),
            cmd_minus: c && p(K::Minus),
            cmd_0: c && !a && p(K::Num0),
            shift_cmd_r: c && s && p(K::R),
            alt_cmd_digit: if c && a && !s { [(K::Num1, 1), (K::Num2, 2), (K::Num3, 3)].into_iter().find(|(k, _)| p(*k)).map(|(_, d)| d) } else { None },
        }
    });
    map(&k, st, menu_owns_accelerators)
}

/// Enter in the focused search field: one step per keypress (Shift = previous); an empty query
/// does nothing. Returns None when Enter is not the search field's.
pub fn search_enter(k: &Keys, query_empty: bool) -> Option<Option<Action>> {
    if !(k.enter && k.search_focused) {
        return None;
    }
    Some(if query_empty {
        None
    } else if k.shift {
        Some(Action::SearchPrev)
    } else {
        Some(Action::SearchNext)
    })
}

/// Maps pressed keys to actions. With a native menu, its accelerators are left to the menu
/// entirely (verified live: AppKit also consumes ⌘= while Zoom In is disabled at the layout
/// ceiling, so the disabled item + A+ tooltip carry that state).
pub fn map(k: &Keys, st: &AppState, menu_owns_accelerators: bool) -> Vec<Action> {
    let mut acts = Vec::new();
    // Enter consumed by the search field this frame: never also FollowSelected.
    let search_enter = search_enter(k, st.search().query().is_empty());
    if let Some(a) = search_enter.clone().flatten() {
        acts.push(a);
    }
    if k.nav {
        let has_rows = st.session().view().map(|v| !v.rows.is_empty()).unwrap_or(false);
        if has_rows {
            let m = [(k.up, RowMove::Up), (k.down, RowMove::Down), (k.pgup, RowMove::PageUp), (k.pgdn, RowMove::PageDown), (k.home, RowMove::Home), (k.end, RowMove::End)]
                .into_iter()
                .find(|x| x.0)
                .map(|x| x.1);
            if let Some(m) = m {
                acts.push(Action::MoveRow(m));
            }
            if k.enter && search_enter.is_none() {
                acts.push(Action::FollowSelected);
            }
        }
        if k.right {
            acts.push(Action::ExpandSelectedNode);
        } else if k.left {
            acts.push(Action::CollapseSelectedNode);
        }
        // One step per press; inactive while a text field has focus (`nav`).
        if k.close_bracket || k.open_bracket {
            let forward = k.close_bracket;
            acts.push(if k.shift { Action::StepStructure(forward) } else { Action::StepField(forward) });
        }
    }
    if k.f3 {
        acts.push(Action::SearchNext);
    }
    if k.shift_f3 {
        acts.push(Action::SearchPrev);
    }
    if k.alt_left {
        acts.push(Action::Back);
    }
    // Esc closes a modal (handled by the modal) instead of cancelling jobs.
    if k.esc && !st.prefs().modal_open() {
        acts.push(Action::CancelJobs);
    }
    if !menu_owns_accelerators {
        for (pressed, a) in [
            (k.cmd_o, Action::PickFile),
            (k.shift_cmd_o, Action::PickFolder),
            (k.cmd_l, Action::FocusGoto),
            (k.cmd_f, Action::FocusSearch),
            (k.cmd_g, Action::SearchNext),
            (k.shift_cmd_g, Action::SearchPrev),
            // Gated like the native menu item (review N1): a Zoom In that cannot zoom never
            // rewrites the chosen zoom.
            (k.cmd_plus && st.can_zoom_in(), Action::ZoomIn),
            (k.cmd_minus, Action::ZoomOut),
            (k.cmd_0, Action::ZoomReset),
            (k.shift_cmd_r, Action::ResetView),
        ] {
            if pressed {
                acts.push(a);
            }
        }
        if let Some(d) = k.alt_cmd_digit {
            acts.push(Action::TogglePanel(match d {
                1 => Panel::Navigation,
                2 => Panel::Explanation,
                _ => Panel::Bytes,
            }));
        }
    }
    acts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::jobs::Worker;
    use std::sync::Arc;

    fn st() -> AppState {
        AppState::new(Worker::spawn(Arc::new(|| {})))
    }

    /// One ⌘= press is one Zoom In: with a native menu the key belongs to the menu only.
    #[test]
    fn cmd_plus_is_handled_once() {
        let s = st();
        let k = Keys { cmd_plus: true, ..Default::default() };
        assert!(map(&k, &s, true).is_empty(), "menu-owned ⌘= also handled by keys");
        assert_eq!(map(&k, &s, false), vec![Action::ZoomIn]);
        let mut a = st();
        a.apply(Action::ZoomIn);
        assert!((a.prefs().zoom() - 1.1).abs() < 1e-4, "one press = +10 %: {}", a.prefs().zoom());
    }

    /// Review N1: chosen 160 % clamped to 100 % by a small window; Ctrl+= (no native menu) is
    /// gated by `can_zoom_in` like the menu item, so the chosen zoom (and app.ron) stays 160 %
    /// and the window's return still restores it.
    #[test]
    fn zoom_in_key_at_the_layout_limit_keeps_the_chosen_zoom() {
        use crate::app::ZoomFit;
        let at = |w: f32, h: f32, z: f32| Action::LayoutFit(ZoomFit { at_zoom: z, avail_w: w / z, avail_h: h / z, need_w: 875.0, need_h: 560.0 });
        let mut s = st();
        s.apply(Action::SetZoom(1.6));
        s.apply(at(900.0, 700.0, 1.6));
        s.apply(at(900.0, 700.0, 1.6));
        assert_eq!((s.prefs().zoom(), s.prefs().chosen_zoom(), s.can_zoom_in()), (1.0, 1.6, false));
        let k = Keys { cmd_plus: true, ..Default::default() };
        let acts = map(&k, &s, false);
        assert!(acts.is_empty(), "Zoom In at the limit is not dispatched: {acts:?}");
        for a in acts {
            s.apply(a);
        }
        assert_eq!((s.prefs().chosen_zoom(), s.prefs().persisted().zoom), (1.6, 1.6));
        s.apply(at(1400.0, 900.0, 1.0));
        assert!((s.prefs().zoom() - 1.6).abs() < 1e-3, "the window's return restores 160 %: {}", s.prefs().zoom());
    }

    /// `]`/`[` step fields, Shift steps structures; one action per press; nothing while a text
    /// field has focus.
    #[test]
    fn bracket_keys_step_once_and_not_in_text_fields() {
        let s = st();
        assert_eq!(map(&Keys { nav: true, close_bracket: true, ..Default::default() }, &s, true), vec![Action::StepField(true)]);
        assert_eq!(map(&Keys { nav: true, open_bracket: true, ..Default::default() }, &s, true), vec![Action::StepField(false)]);
        assert_eq!(map(&Keys { nav: true, close_bracket: true, shift: true, ..Default::default() }, &s, true), vec![Action::StepStructure(true)]);
        assert_eq!(map(&Keys { nav: true, open_bracket: true, shift: true, ..Default::default() }, &s, false), vec![Action::StepStructure(false)]);
        assert!(map(&Keys { nav: false, close_bracket: true, ..Default::default() }, &s, true).is_empty(), "text field focused");
    }

    #[test]
    fn non_menu_keys_always_mapped() {
        let s = st();
        let k = Keys { f3: true, alt_left: true, esc: true, ..Default::default() };
        assert_eq!(map(&k, &s, true), vec![Action::SearchNext, Action::Back, Action::CancelJobs]);
    }

    #[test]
    fn panel_toggles_once_per_press() {
        let s = st();
        for (d, p) in [(1, Panel::Navigation), (2, Panel::Explanation), (3, Panel::Bytes)] {
            let k = Keys { alt_cmd_digit: Some(d), ..Default::default() };
            assert!(map(&k, &s, true).is_empty(), "menu-owned Option+⌘{d}");
            assert_eq!(map(&k, &s, false), vec![Action::TogglePanel(p)]);
        }
    }

    /// Search field Enter: exactly one SearchNext/SearchPrev per keypress, never also
    /// FollowSelected; empty query is a no-op; after focus moved away, Enter follows the row.
    #[test]
    fn search_enter_is_one_action_per_keypress() {
        let mut s = st();
        s.apply(Action::SetSearch("MAN".into()));
        // `nav` is false while a text field owns the keyboard; also checked with nav = true
        // (focus surrendered in the same frame) — the search flag still wins.
        for nav in [false, true] {
            let k = Keys { enter: true, search_focused: true, nav, ..Default::default() };
            assert_eq!(map(&k, &s, true), vec![Action::SearchNext]);
            let k = Keys { enter: true, shift: true, search_focused: true, nav, ..Default::default() };
            assert_eq!(map(&k, &s, true), vec![Action::SearchPrev]);
        }
        // No Enter → nothing (repeated frames without a keypress do not repeat the step).
        assert!(map(&Keys { search_focused: true, ..Default::default() }, &s, true).is_empty());
        // Empty query: no-op.
        let e = st();
        assert!(map(&Keys { enter: true, search_focused: true, nav: true, ..Default::default() }, &e, true).is_empty());
        // Focus lost (clicked elsewhere): Enter is the table's again (needs rows; none here).
        assert_eq!(search_enter(&Keys { enter: true, ..Default::default() }, false), None);
    }
}
