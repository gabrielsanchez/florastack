//! What can go wrong when a command runs ([`CommandError`]), and how it is told to a JSON client ([`ErrorInfo`]).
//!
//! Every error has a stable [`code`](CommandError::code), which the CLI maps to its exit codes and the control
//! channel and MCP server pass on, and a message that says what failed and, where it can, what to do; the underlying
//! error is kept as its source.

use florastack_align::AlignError;
use florastack_codecs::CodecError;
use florastack_core::Cancelled;
use florastack_project::{Aspects, EditError, ProjectError};
use florastack_raw::RawError;
use florastack_stack::StackError;
use schemars::JsonSchema;
use serde::Serialize;

use crate::frames::FrameError;
use crate::project::ProjectDataError;

/// A parameter that is well-formed JSON of the right shape but can't be used: out of range, too long, contradictory.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{field}` {reason}")]
pub struct Invalid {
    /// The parameter, such as `paths` or `format.quality`.
    pub field: String,
    pub reason: String,
}

impl Invalid {
    pub fn new(field: impl Into<String>, reason: impl Into<String>) -> Invalid {
        Invalid { field: field.into(), reason: reason.into() }
    }
}

/// Why a command didn't run, or failed.
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("there is no command `{name}`{}", suggestion.map(|s| format!(" (did you mean `{s}`?)")).unwrap_or_default())]
    UnknownCommand { name: String, suggestion: Option<&'static str> },
    #[error("the parameters of `{command}` aren't valid")]
    Params {
        command: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("the parameters of `{command}` aren't valid")]
    Invalid {
        command: String,
        #[source]
        source: Invalid,
    },
    #[error("`{command}` can't run now: {reason}")]
    Unavailable { command: String, reason: String },
    #[error("`{command}` would change the {aspects} that “{job}” is working on: wait for it to finish, or cancel it")]
    Conflict { command: String, job: String, aspects: Aspects },
    #[error("there is no {what}")]
    NotFound { what: String },
    #[error("cancelled")]
    Cancelled,
    // The other crates' errors are boxed, so that every `Result` of the engine stays small.
    #[error(transparent)]
    Project(Box<ProjectError>),
    #[error(transparent)]
    Edit(#[from] EditError),
    #[error(transparent)]
    Data(Box<ProjectDataError>),
    #[error(transparent)]
    Frame(Box<FrameError>),
    #[error(transparent)]
    Codec(Box<CodecError>),
    #[error(transparent)]
    Raw(Box<RawError>),
    /// A stack that can't be aligned, ordered or framed (a frame that can't be matched is no error: it is flagged).
    #[error(transparent)]
    Align(Box<AlignError>),
    /// A stack that can't be made: settings out of range, frames that don't fit the canvas, accumulators that can't
    /// be spilled to disk.
    #[error(transparent)]
    Stack(Box<StackError>),
    #[error("cannot start a worker for `{command}`")]
    Spawn {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("`{command}` failed with an internal error, so nothing was changed; please report it")]
    Internal { command: String },
}

impl From<Cancelled> for CommandError {
    fn from(_: Cancelled) -> CommandError {
        CommandError::Cancelled
    }
}

macro_rules! boxed_from {
    ($($variant:ident($error:ty)),*) => {
        $(impl From<$error> for CommandError {
            fn from(e: $error) -> CommandError {
                CommandError::$variant(Box::new(e))
            }
        })*
    };
}

boxed_from!(Project(ProjectError), Data(ProjectDataError), Frame(FrameError), Codec(CodecError), Raw(RawError));

impl From<AlignError> for CommandError {
    fn from(e: AlignError) -> CommandError {
        match e {
            AlignError::Cancelled(_) => CommandError::Cancelled,
            other => CommandError::Align(Box::new(other)),
        }
    }
}

impl From<StackError> for CommandError {
    fn from(e: StackError) -> CommandError {
        match e {
            StackError::Cancelled(_) => CommandError::Cancelled,
            other => CommandError::Stack(Box::new(other)),
        }
    }
}

impl CommandError {
    /// A stable, machine-readable name for the kind of error: `unknownCommand`, `params`, `unavailable`, `conflict`,
    /// `notFound`, `cancelled`, `project`, `frame`, `codec`, `raw`, `align`, `stack`, `internal`.
    pub fn code(&self) -> &'static str {
        match self {
            CommandError::UnknownCommand { .. } => "unknownCommand",
            CommandError::Params { .. } | CommandError::Invalid { .. } => "params",
            CommandError::Unavailable { .. } => "unavailable",
            CommandError::Conflict { .. } => "conflict",
            CommandError::NotFound { .. } => "notFound",
            CommandError::Cancelled => "cancelled",
            CommandError::Project(_) | CommandError::Edit(_) => "project",
            CommandError::Data(e) => match &**e {
                ProjectDataError::Project(_) => "project",
                ProjectDataError::Codec(_)
                | ProjectDataError::Frame(FrameError::Codec(_))
                | ProjectDataError::ReadFrame { source: FrameError::Codec(_), .. } => "codec",
                ProjectDataError::Raw(_)
                | ProjectDataError::Frame(FrameError::Raw(_))
                | ProjectDataError::ReadFrame { source: FrameError::Raw(_), .. } => "raw",
                ProjectDataError::Op(_) => "internal",
                ProjectDataError::Frame(FrameError::Image(_)) | ProjectDataError::ReadFrame { source: FrameError::Image(_), .. } => "frame",
                ProjectDataError::Align(AlignError::Cancelled(_)) | ProjectDataError::Stack(StackError::Cancelled(_)) => "cancelled",
                ProjectDataError::Align(_) => "align",
                ProjectDataError::Stack(_) => "stack",
            },
            CommandError::Frame(e) => match &**e {
                FrameError::Raw(_) => "raw",
                FrameError::Codec(_) => "codec",
                FrameError::Image(_) => "frame",
            },
            CommandError::Codec(_) => "codec",
            CommandError::Raw(_) => "raw",
            CommandError::Align(_) => "align",
            CommandError::Stack(_) => "stack",
            CommandError::Spawn { .. } | CommandError::Internal { .. } => "internal",
        }
    }

    /// The command the error is about, when it names one.
    pub fn command(&self) -> Option<&str> {
        match self {
            CommandError::Params { command, .. }
            | CommandError::Invalid { command, .. }
            | CommandError::Unavailable { command, .. }
            | CommandError::Conflict { command, .. }
            | CommandError::Spawn { command, .. }
            | CommandError::Internal { command } => Some(command),
            CommandError::UnknownCommand { name, .. } => Some(name),
            _ => None,
        }
    }

    /// The error as a JSON client sees it.
    pub fn info(&self) -> ErrorInfo {
        let mut message = self.to_string();
        let mut source = std::error::Error::source(self);
        while let Some(cause) = source {
            message.push_str(": ");
            message.push_str(&cause.to_string());
            source = cause.source();
        }
        ErrorInfo { code: self.code().to_owned(), command: self.command().map(str::to_owned), message }
    }
}

/// An error as the JSON transports (the CLI's `--json` output, the control channel, MCP) report it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ErrorInfo {
    /// [`CommandError::code`].
    pub code: String,
    /// The command it is about.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// What happened, with its causes, in one line.
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_keep_their_causes() {
        let source = serde_json::from_str::<u8>("300").unwrap_err();
        let e = CommandError::Params { command: "develop.set".into(), source };
        let info = e.info();
        assert_eq!(info.code, "params");
        assert_eq!(info.command.as_deref(), Some("develop.set"));
        assert!(info.message.starts_with("the parameters of `develop.set` aren't valid: "), "{}", info.message);
        let unknown = CommandError::UnknownCommand { name: "project.ad".into(), suggestion: Some("project.new") };
        assert_eq!(unknown.to_string(), "there is no command `project.ad` (did you mean `project.new`?)");
        assert_eq!(CommandError::from(Cancelled).code(), "cancelled");
    }
}
