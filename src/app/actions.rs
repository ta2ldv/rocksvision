//! Everything the toolbar, menus, panels and keyboard can request. Panels only emit these;
//! `AppState::apply` is the single owner of the resulting state changes.

pub use super::lsm_map::{LaneTier, MapMeasure, MapMode, MapMove, MapPick, TierHeights};
use super::state::ZoomFit;
pub use super::view_prefs::Panel;
use crate::model::{NodeKind, Space, Span};
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CopyKind {
    Value,
    /// Exact key bytes of an entry row.
    Key,
    Hex,
    Offset,
    RowTsv,
    /// The open file's path (`Action::CopyFilePath`).
    FilePath,
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
    /// Go to a canonical structure (every link, reference node and location link): expand its
    /// ancestors, select and reveal its tree node, show its view, select `span` (or the
    /// target's row) there, move the byte view; Back returns.
    Navigate {
        target: NodeKind,
        span: Option<Span>,
    },
    /// `]` / `[`: next / previous field of the current table in span order (one step).
    StepField(bool),
    /// `Shift+]` / `Shift+[`: next / previous sibling structure in tree order (one step).
    StepStructure(bool),
    /// The records table's vertical scroll offset (points), reported by the records panel when it
    /// changes (Back restores it, 0024 D1).
    SetTableScroll(f32),
    /// Show / hide the table columns hidden for lack of width ("+N columns").
    ToggleAllColumns,
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
    // ---- LSM Map (0024) ----
    /// Toolbar button / View › LSM Map / Shift+⌘M: show the Observed levels as a map (opening the
    /// folder's CURRENT, else MANIFEST, first when needed; focuses the open SST); while the map
    /// is shown, back to the table.
    ShowLsmMap,
    /// The map is shown but does not have keyboard focus: give it focus, keep CF, pick and pan
    /// (0030 A2-4; the shell turns `ShowLsmMap` into this, it knows egui's focus).
    FocusLsmMap,
    /// Table | Map segment of the levels view (no history entry).
    SetMapMode(MapMode),
    /// Column family shown on the map.
    MapSelectCf(u32),
    /// Map − / + / Fit (about the centre); map zoom only, never the app zoom.
    MapZoom(MapZoom),
    /// Pan (left-edge fraction, vertical points) and map zoom set by the painter (drag, wheel,
    /// pinch, reveal); clamped by `MapState`.
    MapView {
        pan: (f32, f32),
        zoom: f32,
    },
    /// The map's measured zoom maximum for the CF shown (one rank ≥ 24 pt), reported by the
    /// painter when it changes; bounds `MapZoom::In`.
    MapMaxZoom(f32),
    /// The focused box is not laid out (hidden lane / over the cap): the reveal is dropped.
    MapRevealDone,
    /// Keyboard move of the map focus (selects the file's row).
    MapMove(MapMove),
    /// The painter's zoom-1 measure of the map (when it changes): decides Fits / Overflow
    /// (0026 item 6) and bounds the pan.
    MapMeasure(MapMeasure),
    /// The painter's scene heights at ×1 per lane tier (Full, Compact, Bar) for the CF shown,
    /// when they change; sent before `MapMeasure` so Fit can choose the tier (0030 A1).
    MapFitHeights(TierHeights),
    /// Select a WAL of the WAL band by number (single click): the Explanation describes it; a
    /// selected levels row is deselected.
    SelectWal(u64),
    /// Open that WAL (double click / Enter): a file switch, Back restores the map.
    OpenWal(u64),
    /// Pick another map object: a level title bar, the older-WAL summary (or a WAL).
    SelectMapPick(MapPick),
    // ---- preferences ----
    /// View › Show Tooltips (persisted).
    SetTooltips(bool),
    // ---- jobs ----
    CancelJobs,
}

/// Map zoom commands. In / Out / Center act only while the map overflows; Fit always.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MapZoom {
    In,
    Out,
    /// Zoom 1 (the whole CF width), top of the scene.
    Fit,
    /// The scene's centre in the middle at the current zoom.
    Center,
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
