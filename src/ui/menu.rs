//! Menu model shared by the native macOS menu bar (`platform::macos`), the in-window fallback
//! menu (other platforms), the Keyboard Shortcuts modal, the Help text and the tooltips. Every
//! item maps to one `app::Action`; the same action is used by the toolbar, the panel chevrons
//! and the keyboard. `shortcut_table()` is the single source of shortcut text.

use super::theme::THEMES;
use crate::app::{Action, AppState, Panel};
use eframe::egui;

/// Key equivalent: `key` is the macOS key-equivalent character ("o", "=", "-", "0", …).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shortcut {
    pub cmd: bool,
    pub shift: bool,
    pub alt: bool,
    pub key: &'static str,
}

const fn cmd(key: &'static str) -> Option<Shortcut> {
    Some(Shortcut { cmd: true, shift: false, alt: false, key })
}
const fn shift_cmd(key: &'static str) -> Option<Shortcut> {
    Some(Shortcut { cmd: true, shift: true, alt: false, key })
}
const fn alt_cmd(key: &'static str) -> Option<Shortcut> {
    Some(Shortcut { cmd: true, shift: false, alt: true, key })
}

/// Modifier words of every in-window shortcut label (menu fallback, tooltips, Help, modal).
/// macOS: "⌘" (the only modifier glyph the bundled fonts render; see theme::UI_GLYPHS),
/// "Option+", "Shift+", ordered Option+Shift+⌘ ("Shift+⌘O", "Option+⌘1"). Other platforms:
/// "Ctrl+", "Alt+", "Shift+", ordered Ctrl+Alt+Shift+ ("Ctrl+Shift+O", "Ctrl+Alt+1"); egui maps
/// `command` to Ctrl there (ui/keys.rs), so the labels name the keys that work. The native
/// macOS menu bar draws its own glyphs from the key equivalent and modifier mask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyLabels {
    Mac,
    Other,
}

impl KeyLabels {
    /// Labels of the platform this binary was built for.
    pub const HOST: KeyLabels = if cfg!(target_os = "macos") { KeyLabels::Mac } else { KeyLabels::Other };

    pub fn cmd(self) -> &'static str {
        match self {
            KeyLabels::Mac => "⌘",
            KeyLabels::Other => "Ctrl+",
        }
    }
    pub fn alt(self) -> &'static str {
        match self {
            KeyLabels::Mac => "Option+",
            KeyLabels::Other => "Alt+",
        }
    }
    pub fn shift(self) -> &'static str {
        "Shift+"
    }
    /// One key combination in this platform's wording and order.
    pub fn combo(self, cmd: bool, alt: bool, shift: bool, key: &str) -> String {
        let on = |b: bool, s: &'static str| if b { s } else { "" };
        match self {
            KeyLabels::Mac => format!("{}{}{}{key}", on(alt, self.alt()), on(shift, self.shift()), on(cmd, self.cmd())),
            KeyLabels::Other => format!("{}{}{}{key}", on(cmd, self.cmd()), on(alt, self.alt()), on(shift, self.shift())),
        }
    }
}

impl Shortcut {
    /// Label for the host platform (`KeyLabels::HOST`).
    pub fn label(&self) -> String {
        self.label_in(KeyLabels::HOST)
    }
    pub fn label_in(&self, l: KeyLabels) -> String {
        let key = match (self.key, l) {
            ("-", _) => "−".to_string(),
            ("+", KeyLabels::Other) => "Plus".to_string(),
            (k, _) => k.to_uppercase(),
        };
        l.combo(self.cmd, self.alt, self.shift, &key)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MenuCmd {
    About,
    /// Native-only items (hide:, hideOtherApplications:, unhideAllApplications:).
    Hide,
    HideOthers,
    ShowAll,
    Quit,
    OpenFile,
    OpenFolder,
    GotoOffset,
    Find,
    FindNext,
    FindPrev,
    ClearSearch,
    ZoomIn,
    /// Hidden twin of Zoom In for ⌘+ (shift variant), same action.
    ZoomInPlus,
    ZoomOut,
    ActualSize,
    Theme(usize),
    TogglePanel(Panel),
    ResetView,
    Help,
    Shortcuts,
}

pub enum MenuNode {
    Item { cmd: MenuCmd, title: &'static str, shortcut: Option<Shortcut>, hidden: bool, tip: Option<&'static str> },
    Submenu { title: &'static str, items: Vec<MenuNode> },
    Separator,
}

fn item(cmd: MenuCmd, title: &'static str, shortcut: Option<Shortcut>) -> MenuNode {
    MenuNode::Item { cmd, title, shortcut, hidden: false, tip: None }
}

/// Reset View contract (toolbar tooltip and native menu item tooltip); shown through `tip`,
/// which appends the shortcut ("… (Shift+⌘R)" / "… (Ctrl+Shift+R)").
pub const RESET_VIEW_TIP: &str = "Restore default panel widths, the records/bytes split, panel visibility and 100% zoom. Keeps the open file, selection, search and theme. Does not resize the window.";
/// Reset View tooltip while the view is already at its defaults (button disabled).
pub const RESET_VIEW_DEFAULT_TIP: &str = "View is already at defaults (window size is not part of the view)";

/// The menu tree (spec §6.3). The first submenu is the application menu.
pub fn tree() -> Vec<MenuNode> {
    use MenuCmd as C;
    let themes = THEMES.iter().enumerate().map(|(i, t)| item(C::Theme(i), t.name, None)).collect();
    vec![
        MenuNode::Submenu {
            title: crate::branding::NAME,
            items: vec![
                item(C::About, "About RocksVision", None),
                MenuNode::Separator,
                item(C::Hide, "Hide RocksVision", cmd("h")),
                item(C::HideOthers, "Hide Others", alt_cmd("h")),
                item(C::ShowAll, "Show All", None),
                MenuNode::Separator,
                item(C::Quit, "Quit RocksVision", cmd("q")),
            ],
        },
        MenuNode::Submenu {
            title: "File",
            items: vec![item(C::OpenFile, "Open File…", cmd("o")), item(C::OpenFolder, "Open Folder…", shift_cmd("o")), MenuNode::Separator, item(C::GotoOffset, "Go to Offset…", cmd("l"))],
        },
        MenuNode::Submenu {
            title: "View",
            items: vec![
                item(C::Find, "Find…", cmd("f")),
                item(C::FindNext, "Find Next", cmd("g")),
                item(C::FindPrev, "Find Previous", shift_cmd("g")),
                item(C::ClearSearch, "Clear Search", None),
                MenuNode::Separator,
                item(C::ZoomIn, "Zoom In", cmd("=")),
                MenuNode::Item { cmd: C::ZoomInPlus, title: "Zoom In", shortcut: cmd("+"), hidden: true, tip: None },
                item(C::ZoomOut, "Zoom Out", cmd("-")),
                item(C::ActualSize, "Actual Size", cmd("0")),
                MenuNode::Separator,
                MenuNode::Submenu { title: "Theme", items: themes },
                MenuNode::Separator,
                item(C::TogglePanel(Panel::Navigation), "Navigation Panel", alt_cmd("1")),
                item(C::TogglePanel(Panel::Explanation), "Explanation Panel", alt_cmd("2")),
                item(C::TogglePanel(Panel::Bytes), "Byte View", alt_cmd("3")),
                MenuNode::Separator,
                MenuNode::Item { cmd: C::ResetView, title: "Reset View", shortcut: shift_cmd("r"), hidden: false, tip: Some(RESET_VIEW_TIP) },
            ],
        },
        MenuNode::Submenu { title: "Help", items: vec![item(C::Help, "RocksVision Help", None), item(C::Shortcuts, "Keyboard Shortcuts", None)] },
    ]
}

impl MenuCmd {
    /// The application action of this item (None for items handled natively).
    pub fn action(self, _st: &AppState) -> Option<Action> {
        use MenuCmd as C;
        Some(match self {
            C::About => Action::ShowAbout,
            C::Hide | C::HideOthers | C::ShowAll => return None,
            C::Quit => Action::Quit,
            C::OpenFile => Action::PickFile,
            C::OpenFolder => Action::PickFolder,
            C::GotoOffset => Action::FocusGoto,
            C::Find => Action::FocusSearch,
            C::FindNext => Action::SearchNext,
            C::FindPrev => Action::SearchPrev,
            C::ClearSearch => Action::ClearSearch,
            C::ZoomIn | C::ZoomInPlus => Action::ZoomIn,
            C::ZoomOut => Action::ZoomOut,
            C::ActualSize => Action::ZoomReset,
            C::Theme(i) => Action::SetTheme(THEMES.get(i)?.id),
            C::TogglePanel(p) => Action::TogglePanel(p),
            C::ResetView => Action::ResetView,
            C::Help => Action::ShowHelp,
            C::Shortcuts => Action::ShowShortcuts,
        })
    }

    /// (enabled, checked) from the application state (spec §6.5); synced every frame.
    pub fn state(self, st: &AppState) -> (bool, bool) {
        use MenuCmd as C;
        match self {
            C::GotoOffset => (st.session().file().is_some(), false),
            C::FindNext | C::FindPrev => (!st.search().query().is_empty(), false),
            C::ZoomIn | C::ZoomInPlus => (st.can_zoom_in(), false),
            C::Theme(i) => (true, THEMES.get(i).map(|t| t.id == st.prefs().theme_id()).unwrap_or(false)),
            C::TogglePanel(p) => (true, st.prefs().shown(p)),
            _ => (true, false),
        }
    }
}

/// Every item of the tree with its shortcut (for the shortcut table and tests).
pub fn items(nodes: &[MenuNode], out: &mut Vec<(MenuCmd, &'static str, Option<Shortcut>, bool)>) {
    for n in nodes {
        match n {
            MenuNode::Item { cmd, title, shortcut, hidden, .. } => out.push((*cmd, *title, *shortcut, *hidden)),
            MenuNode::Submenu { items: sub, .. } => items(sub, out),
            MenuNode::Separator => {}
        }
    }
}

/// Shortcut text of a menu command for the host platform, e.g. "Shift+⌘R"; Zoom In includes
/// its ⌘+ alias.
pub fn shortcut_text(c: MenuCmd) -> Option<String> {
    shortcut_text_in(c, KeyLabels::HOST)
}
pub fn shortcut_text_in(c: MenuCmd, l: KeyLabels) -> Option<String> {
    let mut all = Vec::new();
    items(&tree(), &mut all);
    let main = all.iter().find(|x| x.0 == c && !x.3).and_then(|x| x.2)?.label_in(l);
    let twin = all.iter().find(|x| x.3 && x.1 == all.iter().find(|y| y.0 == c).map(|y| y.1).unwrap_or("")).and_then(|x| x.2);
    Some(match twin {
        Some(t) => format!("{main} (also {})", t.label_in(l)),
        None => main,
    })
}

/// "Option+Left" / "Alt+Left": Back (ui/keys.rs `alt_left`).
pub fn back_key(l: KeyLabels) -> String {
    format!("{}Left", l.alt())
}

/// "Option+⌘1/2/3" / "Ctrl+Alt+1/2/3": the panel toggles.
pub fn panel_keys(l: KeyLabels) -> String {
    l.combo(true, true, false, "1/2/3")
}

/// Keys handled in the window (not menu accelerators).
fn window_keys(l: KeyLabels) -> Vec<(String, &'static str)> {
    [
        ("Enter / Shift+Enter".to_string(), "In the search field: next / previous match"),
        ("F3 / Shift+F3".to_string(), "Find next / previous"),
        (back_key(l), "Back"),
        ("Esc".to_string(), "Cancel background jobs"),
        ("Up Down PgUp PgDn Home End".to_string(), "Move through table rows"),
        ("Left / Right".to_string(), "Fold / unfold the selected tree node"),
        ("Enter".to_string(), "Follow the selected row's link"),
        ("Shift+click".to_string(), "Extend the byte selection in the byte view"),
    ]
    .into()
}

/// The one shortcut table (spec §7.4): menu accelerators (with aliases folded in) then window
/// keys. The Keyboard Shortcuts modal renders it; tooltips use `shortcut_text`.
pub fn shortcut_table() -> Vec<(String, &'static str)> {
    shortcut_table_in(KeyLabels::HOST)
}
pub fn shortcut_table_in(l: KeyLabels) -> Vec<(String, &'static str)> {
    let mut all = Vec::new();
    items(&tree(), &mut all);
    let mut out: Vec<(String, &'static str)> = all.iter().filter(|x| !x.3 && x.2.is_some() && !matches!(x.0, MenuCmd::Hide | MenuCmd::HideOthers)).filter_map(|x| Some((shortcut_text_in(x.0, l)?, x.1))).collect();
    out.extend(window_keys(l));
    out
}

/// "Title (Shortcut)" for tooltips.
pub fn tip(text: &str, c: MenuCmd) -> String {
    tip_in(text, c, KeyLabels::HOST)
}
pub fn tip_in(text: &str, c: MenuCmd, l: KeyLabels) -> String {
    match shortcut_text_in(c, l) {
        Some(s) => format!("{text} ({s})"),
        None => text.to_string(),
    }
}

/// In-window menu for platforms without a native menu bar: the same tree, minus the
/// application-menu items that only exist natively. Returns the chosen command.
pub fn show_fallback(ui: &mut egui::Ui, st: &AppState, nodes: &[MenuNode]) -> Option<MenuCmd> {
    let mut chosen = None;
    for n in nodes {
        match n {
            MenuNode::Item { cmd, title, shortcut, hidden, tip } => {
                if *hidden || matches!(cmd, MenuCmd::Hide | MenuCmd::HideOthers | MenuCmd::ShowAll) {
                    continue;
                }
                let (enabled, checked) = cmd.state(st);
                let text = format!("{}{title}", if checked { "✔ " } else { "" });
                let mut b = egui::Button::new(text);
                if let Some(s) = shortcut {
                    b = b.shortcut_text(s.label());
                }
                let mut r = ui.add_enabled(enabled, b);
                if let Some(t) = tip {
                    r = r.on_hover_text(self::tip(t, *cmd));
                }
                if r.clicked() {
                    chosen = Some(*cmd);
                    ui.close();
                }
            }
            MenuNode::Submenu { title, items: sub } => {
                ui.menu_button(*title, |ui| {
                    if let Some(c) = show_fallback(ui, st, sub) {
                        chosen = Some(c);
                    }
                });
            }
            MenuNode::Separator => {
                ui.separator();
            }
        }
    }
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each accelerator is registered once in the menu tree.
    #[test]
    fn shortcuts_are_unique() {
        let mut all = Vec::new();
        items(&tree(), &mut all);
        let keys: Vec<Shortcut> = all.iter().filter_map(|x| x.2).collect();
        for (i, a) in keys.iter().enumerate() {
            assert!(!keys[i + 1..].contains(a), "duplicate shortcut {}", a.label());
        }
        assert!(all.iter().any(|x| x.0 == MenuCmd::About), "About item missing");
        assert!(all.iter().any(|x| x.0 == MenuCmd::Help), "Help item missing");
        assert_eq!(all.iter().filter(|x| matches!(x.0, MenuCmd::Theme(_))).count(), THEMES.len());
        assert!(!all.iter().any(|x| x.1.contains("Narrow")), "the pin item is gone");
    }

    /// One table: plain-text modifiers, the ⌘+ alias folded into Zoom In, panel toggles (macOS
    /// wording, independent of the host).
    #[test]
    fn shortcut_table_texts() {
        let t = shortcut_table_in(KeyLabels::Mac);
        let find = |w: &str| t.iter().find(|x| x.1 == w).map(|x| x.0.clone()).unwrap_or_default();
        assert_eq!(find("Zoom In"), "⌘= (also ⌘+)");
        assert_eq!(find("Open Folder…"), "Shift+⌘O");
        assert_eq!(find("Reset View"), "Shift+⌘R");
        assert_eq!(find("Navigation Panel"), "Option+⌘1");
        assert_eq!(find("Explanation Panel"), "Option+⌘2");
        assert_eq!(find("Byte View"), "Option+⌘3");
        assert_eq!(find("Zoom Out"), "⌘−");
        assert_eq!(t.iter().filter(|x| x.1 == "Zoom In").count(), 1);
        assert_eq!(tip_in("Hide Navigation", MenuCmd::TogglePanel(Panel::Navigation), KeyLabels::Mac), "Hide Navigation (Option+⌘1)");
        assert_eq!(find("Back"), "Option+Left");
        assert_eq!(tip_in(RESET_VIEW_TIP, MenuCmd::ResetView, KeyLabels::Mac), format!("{RESET_VIEW_TIP} (Shift+⌘R)"));
        assert_eq!(panel_keys(KeyLabels::Mac), "Option+⌘1/2/3");
        for (k, _) in &t {
            assert!(!k.contains('⇧') && !k.contains('⌥'), "{k}");
        }
    }

    /// Windows/Linux wording: Ctrl/Alt words, Ctrl+Alt+Shift order, no ⌘ or Option anywhere.
    #[test]
    fn shortcut_table_texts_other_platforms() {
        let l = KeyLabels::Other;
        let t = shortcut_table_in(l);
        let find = |w: &str| t.iter().find(|x| x.1 == w).map(|x| x.0.clone()).unwrap_or_default();
        assert_eq!(find("Zoom In"), "Ctrl+= (also Ctrl+Plus)");
        assert_eq!(find("Open Folder…"), "Ctrl+Shift+O");
        assert_eq!(find("Open File…"), "Ctrl+O");
        assert_eq!(find("Reset View"), "Ctrl+Shift+R");
        assert_eq!(find("Navigation Panel"), "Ctrl+Alt+1");
        assert_eq!(find("Zoom Out"), "Ctrl+−");
        assert_eq!(find("Back"), "Alt+Left");
        assert_eq!(tip_in("Hide Navigation", MenuCmd::TogglePanel(Panel::Navigation), l), "Hide Navigation (Ctrl+Alt+1)");
        assert_eq!(panel_keys(l), "Ctrl+Alt+1/2/3");
        assert_eq!(Shortcut { cmd: true, shift: true, alt: true, key: "x" }.label_in(l), "Ctrl+Alt+Shift+X");
        assert_eq!(Shortcut { cmd: true, shift: true, alt: true, key: "x" }.label_in(KeyLabels::Mac), "Option+Shift+⌘X");
        for (k, _) in &t {
            assert!(!k.contains('⌘') && !k.contains("Option"), "{k}");
        }
        assert_eq!(KeyLabels::HOST, if cfg!(target_os = "macos") { KeyLabels::Mac } else { KeyLabels::Other });
    }
}
