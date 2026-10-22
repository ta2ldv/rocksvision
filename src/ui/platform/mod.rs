//! Platform boundary for native services: file/folder dialogs and the native menu bar. The app
//! requests dialogs as `Effect`s; menu items come back as `MenuCmd`s mapped to `Action`s. Only
//! this module talks to rfd or OS menu APIs.

#[cfg(target_os = "macos")]
mod macos;

use super::menu::MenuCmd;
use crate::app::AppState;
use eframe::egui;
use std::path::{Path, PathBuf};

pub trait Platform {
    /// Modal folder picker; None when cancelled.
    fn pick_folder(&self, start: Option<&Path>) -> Option<PathBuf>;
    /// Modal file picker; None when cancelled.
    fn pick_file(&self, start: Option<&Path>) -> Option<PathBuf>;
    /// True when a native menu bar exists and owns the menu accelerators.
    fn has_native_menu(&self) -> bool {
        false
    }
    /// Installs (first call) and updates the native menu from the app state. UI thread only.
    fn sync_menu(&mut self, _ctx: &egui::Context, _st: &AppState) {}
    /// Native menu items chosen since the last call.
    fn take_menu_commands(&mut self) -> Vec<MenuCmd> {
        Vec::new()
    }
}

/// Native dialogs through rfd; on macOS also the AppKit menu bar.
#[derive(Default)]
pub struct NativePlatform {
    #[cfg(target_os = "macos")]
    menu: Option<macos::NativeMenu>,
}

impl Platform for NativePlatform {
    fn pick_folder(&self, start: Option<&Path>) -> Option<PathBuf> {
        let mut d = rfd::FileDialog::new().set_title("Open a RocksDB data folder (read-only)");
        if let Some(dir) = start {
            d = d.set_directory(dir);
        }
        d.pick_folder()
    }

    fn pick_file(&self, start: Option<&Path>) -> Option<PathBuf> {
        let mut d = rfd::FileDialog::new().set_title("Open a WAL, SST, MANIFEST or CURRENT file (read-only)");
        if let Some(dir) = start {
            d = d.set_directory(dir);
        }
        d.pick_file()
    }

    #[cfg(target_os = "macos")]
    fn has_native_menu(&self) -> bool {
        self.menu.is_some()
    }

    #[cfg(target_os = "macos")]
    fn sync_menu(&mut self, ctx: &egui::Context, st: &AppState) {
        // Installed on the first frame, after winit has set up NSApplication and its default menu.
        if self.menu.is_none() {
            self.menu = macos::NativeMenu::install(ctx);
        }
        if let Some(m) = &mut self.menu {
            m.sync(st);
        }
    }

    #[cfg(target_os = "macos")]
    fn take_menu_commands(&mut self) -> Vec<MenuCmd> {
        self.menu.as_mut().map(|m| m.take_commands()).unwrap_or_default()
    }
}

/// The platform implementation for this build target.
pub fn native() -> Box<dyn Platform> {
    Box::new(NativePlatform::default())
}
