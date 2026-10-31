//! Files that runs leave outside projects: removing the tile folders of runs that crashed (`cache.removeAbandoned`),
//! which the desktop app does at startup.

use schemars::JsonSchema;
use serde::Serialize;

use super::display;
use crate::error::CommandError;
use crate::registry::{CommandSpec, Effects, Meta, NoParams};
use crate::session::Session;

pub(crate) fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec::instant(
        Meta::new(
            "cache.removeAbandoned",
            "Remove abandoned tile files",
            "Removes the tile folders that crashed runs left in the temporary directory.",
        )
        .help(
            "Stacks keep their accumulators' tiles in a folder of the system's temporary directory when they exceed \
                 their memory budget, and remove it when they end; a run that crashes leaves it behind. This removes \
                 every such folder that no run holds and that hasn't changed for a minute (a folder from before tile \
                 folders had locks, once it is a day old). They hold nothing a run can use again, so they are deleted, \
                 not moved to the trash. The desktop app runs this at startup.",
        )
        .effects(Effects::Files),
        remove_abandoned,
    )]
}

/// The tile folders removed.
#[derive(Debug, Serialize, JsonSchema)]
struct Removed {
    /// The folder looked in: the system's temporary directory.
    folder: String,
    removed: Vec<String>,
}

fn remove_abandoned(_: &mut Session, _: NoParams) -> Result<Removed, CommandError> {
    let folder = std::env::temp_dir();
    let removed = florastack_core::TempFileStore::remove_abandoned(&folder).map_err(florastack_stack::StackError::from)?;
    Ok(Removed { folder: display(&folder), removed: removed.iter().map(|p| display(p)).collect() })
}
