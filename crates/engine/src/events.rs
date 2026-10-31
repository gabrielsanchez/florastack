//! What the engine tells its clients when something happens ([`Event`]): a UI redraws what changed, the CLI prints
//! job progress (`--json-progress` writes each event as a line of JSON), the MCP server sends progress
//! notifications. [`crate::Engine::subscribe`] hands out a channel of them.

use florastack_project::Aspects;
use schemars::JsonSchema;
use serde::Serialize;

use crate::jobs::JobInfo;

/// Something that happened. Serialized with its kind in `event`: `{"event": "job", "job": {...}}`.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum Event {
    /// A project was opened, saved or closed.
    Project {
        state: ProjectEvent,
        /// The project folder.
        path: String,
    },
    /// The project changed: these parts of it. `revision` counts the changes since the engine started.
    Changed { revision: u64, aspects: Aspects },
    /// The undo history changed: what an undo and a redo would do now, and whether the project has unsaved changes.
    History {
        #[serde(skip_serializing_if = "Option::is_none")]
        undo: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        redo: Option<String>,
        dirty: bool,
    },
    /// A job was queued, started, made progress or ended.
    Job { job: Box<JobInfo> },
}

/// What happened to a project.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ProjectEvent {
    Opened,
    Saved,
    Closed,
    /// Saved, then moved or copied to another folder, where it is open now (`project.saveAs`).
    Moved,
}
