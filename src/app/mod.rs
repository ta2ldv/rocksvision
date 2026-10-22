//! Application layer, independent of egui. `AppState` (`state`) coordinates five components with
//! private fields: `DocumentSession` (`session`), `SelectionState` (`selection`), `SearchState`
//! (`search`), `JobState` (`job_state`, the only holder of the worker) and `ViewPreferences`
//! (`view_prefs`). Also: actions/effects, the hex page state machine (`hex`), persisted
//! preferences (`prefs`) and background jobs (`jobs`). The UI renders `&AppState` and sends
//! `Action`s; platform effects come back as `Effect`s. Never depends on `crate::ui`.

pub mod actions;
pub mod diagnosis;
pub mod hex;
pub mod job_state;
pub mod jobs;
pub mod prefs;
pub mod search;
pub mod selection;
pub mod session;
pub mod state;
pub mod view_prefs;

pub use actions::{Action, CopyKind, Effect, Panel, RowMove};
pub use job_state::JobState;
pub use search::SearchState;
pub use selection::SelectionState;
pub use session::{DocumentSession, TreeNode};
pub use state::{AppState, ScrollRequest, ZoomFit, ZoomPolicy, CENTER_SPLIT_DEFAULT, FILE_SPACE_TITLE};
pub use view_prefs::ViewPreferences;
