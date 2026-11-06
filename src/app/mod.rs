//! Application layer, independent of egui. `AppState` (`state`) coordinates components with
//! private fields: `DocumentSession` (`session`; owns `Navigation` (`navigation`) and the byte
//! view `HexState` (`hex`)), `SelectionState` (`selection`), `SearchState` (`search`), `JobState`
//! (`job_state`, the only holder of the worker; owns the copy gate `ClipboardState`
//! (`clipboard`)) and `ViewPreferences` (`view_prefs`). Pure services: `provenance` (ranges,
//! where), `diagnosis` (typed findings), `explanation` (the one selection classifier and the
//! Explanation cards). Also: actions/effects, persisted preferences (`prefs`) and background
//! jobs (`jobs`). The UI renders `&AppState` and sends `Action`s; platform effects come back as
//! `Effect`s. Never depends on `crate::ui`.

pub mod actions;
pub mod clipboard;
pub mod diagnosis;
pub mod explanation;
pub mod hex;
pub mod job_state;
pub mod jobs;
pub mod lsm_map;
pub mod map_facts;
pub mod navigation;
pub mod prefs;
pub mod provenance;
pub mod search;
pub mod selection;
pub mod session;
pub mod state;
pub mod view_prefs;

pub use actions::{Action, CopyKind, Effect, MapMeasure, MapMode, MapMove, MapPick, MapZoom, Panel, RowMove};
pub use job_state::JobState;
pub use search::SearchState;
pub use selection::SelectionState;
pub use session::{DocumentSession, TreeNode};
pub use state::{AppState, CENTER_SPLIT_DEFAULT, FILE_SPACE_TITLE, ScrollRequest, ZoomFit, ZoomPolicy};
pub use view_prefs::ViewPreferences;
