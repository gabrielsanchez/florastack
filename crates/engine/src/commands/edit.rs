//! Undo and redo (the history module says how they work).

use schemars::JsonSchema;
use serde::Serialize;

use crate::error::CommandError;
use crate::registry::{CommandSpec, Effects, Meta, NoParams, Requires};
use crate::session::Session;

pub(crate) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::instant(
            Meta::new("edit.undo", "Undo", "Undoes the last change to the project.")
                .help(
                    "Every undoable command is one step; a retouch stroke is one stroke. Refused while a running job \
                     depends on what the step would change.",
                )
                .requires(Requires::WritableProject)
                .effects(Effects::Project),
            undo,
        ),
        CommandSpec::instant(
            Meta::new("edit.redo", "Redo", "Redoes the last change undone.").requires(Requires::WritableProject).effects(Effects::Project),
            redo,
        ),
        CommandSpec::query(
            Meta::new("edit.history", "Undo history", "Lists what undo and redo would do, and whether the project has unsaved changes.")
                .requires(Requires::Project),
            history,
        ),
    ]
}

/// The step undone or redone.
#[derive(Debug, Serialize, JsonSchema)]
struct Step {
    /// Its label: the title of the command that made it.
    label: String,
}

fn undo(session: &mut Session, _: NoParams) -> Result<Step, CommandError> {
    Ok(Step { label: session.undo()? })
}

fn redo(session: &mut Session, _: NoParams) -> Result<Step, CommandError> {
    Ok(Step { label: session.redo()? })
}

/// A step of the undo history.
#[derive(Debug, Serialize, JsonSchema)]
struct HistoryStep {
    /// What the user sees: the title of the command that made it.
    label: String,
    /// That command.
    command: &'static str,
}

/// The undo history.
#[derive(Debug, Serialize, JsonSchema)]
struct History {
    /// The undo steps, the next to undo first.
    undo: Vec<HistoryStep>,
    /// The redo steps, the next to redo first.
    redo: Vec<HistoryStep>,
    /// Whether the project has unsaved changes.
    dirty: bool,
}

fn history(session: &Session, _: NoParams) -> Result<History, CommandError> {
    let (undo, redo) = session.history_steps();
    let list = |steps: Vec<(String, &'static str)>| steps.into_iter().map(|(label, command)| HistoryStep { label, command }).collect();
    Ok(History { undo: list(undo), redo: list(redo), dirty: session.is_dirty() })
}
