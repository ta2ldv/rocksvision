//! Everything the toolbar, menus, panels and keyboard can request. Panels only emit these;
//! `AppState::apply` is the single owner of the resulting state changes.

use super::state::ZoomFit;
pub use super::view_prefs::Panel;
use crate::model::{Space, Span};
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CopyKind {
    Value,
    /// Exact key bytes of an entry row.
    Key,
    Hex,
    Offset,
    RowTsv,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RowMove {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    // ---- opening ----
    /// Native folder dialog (executed by the platform layer, see `Effect`).
    PickFolder,
    PickFile,
    /// Folder or file path (resolved by a background stat).
    OpenPath(PathBuf),
    /// File from the folder list or a link.
    OpenFile(PathBuf),
    SetPathInput(String),
    /// Open the path typed in the path field.
    GoPathInput,
    // ---- search ----
    SetSearch(String),
    SetSearchHex(bool),
    SearchNext,
    SearchPrev,
    /// Clear search text, row matches and byte-search hit (keeps the selection).
    ClearSearch,
    /// Focus the search field (View › Find…).
    FocusSearch,
    CancelSearch,
    // ---- copy ----
    Copy(CopyKind),
    /// Select row `0` then copy it (row context menu).
    CopyRow(usize, CopyKind),
    CopyFilePath,
    // ---- zoom / layout / theme ----
    ZoomIn,
    ZoomOut,
    ZoomReset,
    /// Restore a persisted zoom (clamped by the policy).
    SetZoom(f32),
    /// Per-frame layout measurements for the zoom ceiling.
    LayoutFit(ZoomFit),
    /// Records share of the center column.
    SetCenterSplit(f32),
    ResetCenterSplit,
    SetShowLeft(bool),
    SetShowRight(bool),
    SetShowHex(bool),
    /// Collapse/expand one region: chevrons, strips, View menu items and Option+⌘1/2/3.
    TogglePanel(Panel),
    ResetView,
    SetTheme(&'static str),
    ShowAbout,
    CloseAbout,
    ShowShortcuts,
    CloseShortcuts,
    ShowHelp,
    CloseHelp,
    Quit,
    // ---- navigation ----
    SelectRoot,
    SelectNode(usize),
    ToggleNode(usize),
    ExpandSelectedNode,
    CollapseSelectedNode,
    Back,
    SelectRow(usize),
    /// Follow the link / referenced file of a row.
    FollowRow(usize),
    FollowSelected,
    MoveRow(RowMove),
    // ---- byte view ----
    HexPage(u64),
    HexShowSpace(Space),
    SetGoto(String),
    /// Focus the Go-to-offset field (File › Go to Offset…); ignored without a file.
    FocusGoto,
    GotoOffset,
    HexClick(u64),
    HexShiftClick(u64),
    /// Select a byte span in its own offset space (Diagnosis "Bytes" link): the row with exactly
    /// this span if there is one, otherwise a raw range; the byte view switches space/page.
    SelectSpan(Span),
    HexRetry,
    // ---- jobs ----
    CancelJobs,
}

/// Side effects the application asks the platform layer to perform.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// Show a native folder picker starting at the given folder; answer with `Action::OpenPath`.
    PickFolder(Option<PathBuf>),
    /// Show a native file picker; answer with `Action::OpenPath`.
    PickFile(Option<PathBuf>),
    FocusSearch,
    FocusGoto,
    /// Close the window through the windowing layer (preferences are saved).
    Quit,
}
