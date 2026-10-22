//! macOS native menu bar bridge (AppKit through objc2, the crates winit/rfd already use).
//! Builds the menu from `ui::menu::tree()`, replaces winit's default menu (so there is no Cocoa
//! default About panel), and forwards chosen items as `MenuCmd`s. Item tags index `items`.
//! Hide / Hide Others / Show All use the standard NSApplication selectors.

use super::super::menu::{self, tree, MenuCmd, MenuNode};
use crate::app::AppState;
use eframe::egui;
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, Sel};
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem, NSWindow};
use objc2_foundation::NSString;
use std::sync::{Mutex, OnceLock};

/// Tags of chosen items, drained once per frame.
static QUEUE: Mutex<Vec<isize>> = Mutex::new(Vec::new());
/// Wakes the egui loop when an item is chosen.
static CTX: OnceLock<egui::Context> = OnceLock::new();

define_class!(
    // SAFETY: NSObject has no subclassing requirements; MenuTarget has no Drop impl.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RocksVisionMenuTarget"]
    struct MenuTarget;

    impl MenuTarget {
        #[unsafe(method(menuAction:))]
        fn menu_action(&self, sender: &NSMenuItem) {
            if let Ok(mut q) = QUEUE.lock() {
                q.push(sender.tag());
            }
            if let Some(c) = CTX.get() {
                c.request_repaint();
            }
        }
    }

    unsafe impl NSObjectProtocol for MenuTarget {}
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: plain NSObject initializer.
        unsafe { msg_send![super(this), init] }
    }
}

pub struct NativeMenu {
    _target: Retained<MenuTarget>,
    items: Vec<(MenuCmd, Retained<NSMenuItem>)>,
    /// Last applied (enabled, checked) per item.
    applied: Vec<(bool, bool)>,
}

impl NativeMenu {
    /// Installs the menu bar; None off the main thread.
    pub fn install(ctx: &egui::Context) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        let _ = CTX.set(ctx.clone());
        // No automatic "Show Tab Bar / Show All Tabs" items in the View menu (single window).
        NSWindow::setAllowsAutomaticWindowTabbing(false, mtm);
        let target = MenuTarget::new(mtm);
        let main = NSMenu::new(mtm);
        let mut items = Vec::new();
        for node in tree() {
            if let MenuNode::Submenu { title, items: sub } = node {
                main.addItem(&submenu(mtm, &target, title, &sub, &mut items));
            }
        }
        NSApplication::sharedApplication(mtm).setMainMenu(Some(&main));
        Some(NativeMenu { _target: target, applied: vec![(true, false); items.len()], items })
    }

    /// Mirrors enabled/checked state from the app (only changed items are touched).
    pub fn sync(&mut self, st: &AppState) {
        for (i, (cmd, item)) in self.items.iter().enumerate() {
            let s = cmd.state(st);
            if self.applied[i] != s {
                item.setEnabled(s.0);
                item.setState(if s.1 { 1 } else { 0 });
                self.applied[i] = s;
            }
        }
    }

    pub fn take_commands(&mut self) -> Vec<MenuCmd> {
        let tags: Vec<isize> = QUEUE.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
        tags.into_iter().filter_map(|t| usize::try_from(t).ok().and_then(|i| self.items.get(i)).map(|x| x.0)).collect()
    }
}

fn submenu(mtm: MainThreadMarker, target: &MenuTarget, title: &str, nodes: &[MenuNode], items: &mut Vec<(MenuCmd, Retained<NSMenuItem>)>) -> Retained<NSMenuItem> {
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
    // Enabled state is driven by `sync`, not by AppKit's responder-chain validation.
    menu.setAutoenablesItems(false);
    for n in nodes {
        match n {
            MenuNode::Item { cmd, title, shortcut, hidden, tip } => {
                let (action, native): (Sel, bool) = match cmd {
                    MenuCmd::Hide => (sel!(hide:), true),
                    MenuCmd::HideOthers => (sel!(hideOtherApplications:), true),
                    MenuCmd::ShowAll => (sel!(unhideAllApplications:), true),
                    _ => (sel!(menuAction:), false),
                };
                let key = NSString::from_str(shortcut.map(|s| s.key).unwrap_or(""));
                // SAFETY: the selector exists on the target (our class) or on NSApplication
                // (nil target → responder chain).
                let it = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &NSString::from_str(title), Some(action), &key) };
                if let Some(s) = shortcut {
                    let mut m = NSEventModifierFlags::empty();
                    if s.cmd {
                        m |= NSEventModifierFlags::Command;
                    }
                    if s.shift {
                        m |= NSEventModifierFlags::Shift;
                    }
                    if s.alt {
                        m |= NSEventModifierFlags::Option;
                    }
                    it.setKeyEquivalentModifierMask(m);
                }
                if !native {
                    // SAFETY: MenuTarget implements menuAction: and outlives the menu (kept in NativeMenu).
                    unsafe { it.setTarget(Some(target)) };
                }
                if let Some(t) = tip {
                    it.setToolTip(Some(&NSString::from_str(&menu::tip(t, *cmd))));
                }
                if *hidden {
                    it.setHidden(true);
                    it.setAllowsKeyEquivalentWhenHidden(true);
                }
                it.setTag(items.len() as isize);
                menu.addItem(&it);
                items.push((*cmd, it));
            }
            MenuNode::Submenu { title, items: sub } => menu.addItem(&submenu(mtm, target, title, sub, items)),
            MenuNode::Separator => menu.addItem(&NSMenuItem::separatorItem(mtm)),
        }
    }
    let holder = NSMenuItem::new(mtm);
    holder.setTitle(&NSString::from_str(title));
    holder.setSubmenu(Some(&menu));
    holder
}
