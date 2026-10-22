//! The command registry: every user-facing action as a named command with typed, schema-described parameters and
//! results, help text and an undoable flag. [`Registry::list`] is what the CLI's `commands`, the command palette, the
//! MCP server's tool list and the generated command reference show; [`crate::Engine::dispatch`] runs a command by
//! name with JSON parameters.
//!
//! # Commands
//!
//! A command is a [`CommandSpec`], made from its [`Meta`] (name, title, help, flags) and a typed function by one of
//! three constructors:
//!
//! - [`CommandSpec::query`]: reads the session and returns a result; changes nothing.
//! - [`CommandSpec::instant`]: runs on the engine's thread and may change the project through
//!   [`Session::apply`](crate::Session::apply), which records one undo step for the whole command.
//! - [`CommandSpec::job`]: long work, run as a cancellable job on a worker ([`crate::jobs`]); its function only plans
//!   it, from a snapshot of what it reads, and the plan's commit, run back on the engine's thread, changes the project.
//!
//! Parameters are a struct that implements [`Params`]: serde with `deny_unknown_fields`, so that a misspelled key is an
//! error rather than a default, a doc comment on every field (it becomes the field's description in the schema), and
//! [`Params::check`] for what types can't say. Results are structs too, so that both schemas are JSON objects, as MCP
//! tool definitions want.
//!
//! # Names
//!
//! Names are `area.verbObject` (`project.addFrames`, `develop.set`, `edit.undo`): lower-case areas, a lower camel-case
//! action, at most 64 characters, matching `^[a-z]+(\.[a-z][a-zA-Z0-9]*)+$`. They are an interface: scripts, presets
//! of the CLI and MCP clients use them, so a test pins the list (`crates/engine/tests/commands.txt`), and a renamed
//! command keeps its old name as an alias.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::{CommandError, Invalid};
use crate::jobs::{JobPlan, Lane};
use crate::schema::schema_for;
use crate::session::Session;

/// A command's parameters. See the module documentation.
pub trait Params: DeserializeOwned + JsonSchema {
    /// Checks what the types can't: ranges, lengths, contradictions.
    fn check(&self) -> Result<(), Invalid> {
        Ok(())
    }
}

/// The parameters of a command that takes none: `{}` (or `null`).
#[derive(Debug, Default, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoParams {}

impl Params for NoParams {}

/// What a command needs before it can run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Requires {
    Nothing,
    /// An open project.
    Project,
    /// A project open for editing, not read-only.
    WritableProject,
}

/// What a command may change, for clients that ask before running something (MCP's tool annotations).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Effects {
    /// Nothing: a query.
    None,
    /// The open project or the session, and the project's own folder.
    Project,
    /// Files outside the project folder, such as an exported image, a preset or the trash.
    Files,
}

/// How a command runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    /// It reads, and returns at once.
    Query,
    /// It runs to its end on the engine's thread.
    Instant,
    /// It starts a job and returns its id; the job's result comes later.
    Job,
}

/// A command's name, texts and flags, from which its [`CommandSpec`] is made.
#[derive(Clone, Copy, Debug)]
pub struct Meta {
    pub name: &'static str,
    /// A short label for menus and the command palette, in sentence case ("Add frames").
    pub title: &'static str,
    /// One sentence that says what it does.
    pub summary: &'static str,
    /// More: when to use it, what it changes, what its result holds.
    pub help: &'static str,
    /// Whether what it changes is one undo step.
    pub undoable: bool,
    pub requires: Requires,
    pub effects: Effects,
    /// For a job: a newer call cancels the older ones that haven't finished (previews of changed settings).
    pub supersede: bool,
    /// Parameters it can be called with, as JSON text: the reference shows them, and a test checks them.
    pub examples: &'static [&'static str],
}

impl Meta {
    /// A command that needs nothing, changes nothing and is not undoable; the builder methods below say otherwise.
    pub const fn new(name: &'static str, title: &'static str, summary: &'static str) -> Meta {
        Meta {
            name,
            title,
            summary,
            help: "",
            undoable: false,
            requires: Requires::Nothing,
            effects: Effects::None,
            supersede: false,
            examples: &["{}"],
        }
    }

    #[must_use]
    pub const fn help(mut self, help: &'static str) -> Meta {
        self.help = help;
        self
    }

    /// One undo step, and it changes the project, which must be open for editing.
    #[must_use]
    pub const fn undoable(mut self) -> Meta {
        self.undoable = true;
        self.requires = Requires::WritableProject;
        self.effects = Effects::Project;
        self
    }

    #[must_use]
    pub const fn requires(mut self, requires: Requires) -> Meta {
        self.requires = requires;
        self
    }

    #[must_use]
    pub const fn effects(mut self, effects: Effects) -> Meta {
        self.effects = effects;
        self
    }

    #[must_use]
    pub const fn supersede(mut self) -> Meta {
        self.supersede = true;
        self
    }

    #[must_use]
    pub const fn examples(mut self, examples: &'static [&'static str]) -> Meta {
        self.examples = examples;
        self
    }
}

/// A command as [`Registry::list`] describes it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandInfo {
    pub name: &'static str,
    pub title: &'static str,
    pub summary: &'static str,
    pub help: &'static str,
    pub kind: Kind,
    /// The job lane, for a job.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lane: Option<Lane>,
    pub undoable: bool,
    pub requires: Requires,
    pub effects: Effects,
    /// The JSON schema of its parameters (draft 2020-12, an object).
    pub params: Value,
    /// The JSON schema of its result (an object); for a job, of the result the job finishes with.
    pub result: Value,
    pub examples: Vec<Value>,
}

type Check = Box<dyn Fn(Value) -> Result<(), CommandError> + Send + Sync>;
type QueryFn = Box<dyn Fn(&Session, Value) -> Result<Value, CommandError> + Send + Sync>;
type InstantFn = Box<dyn Fn(&mut Session, Value) -> Result<Value, CommandError> + Send + Sync>;
type JobFn = Box<dyn Fn(&Session, Value) -> Result<JobPlan<Value>, CommandError> + Send + Sync>;

pub(crate) enum Runner {
    Query(QueryFn),
    Instant(InstantFn),
    Job(JobFn),
}

/// A command: its description and its implementation.
pub struct CommandSpec {
    pub(crate) info: CommandInfo,
    pub(crate) supersede: bool,
    pub(crate) check: Check,
    pub(crate) run: Runner,
}

/// Reads parameters as `P` and checks them.
pub(crate) fn parse<P: Params>(command: &str, params: Value) -> Result<P, CommandError> {
    let params: P = serde_json::from_value(params).map_err(|source| CommandError::Params { command: command.to_owned(), source })?;
    params.check().map_err(|source| CommandError::Invalid { command: command.to_owned(), source })?;
    Ok(params)
}

/// A result as JSON.
pub(crate) fn to_json<R: Serialize>(command: &str, result: &R) -> Result<Value, CommandError> {
    serde_json::to_value(result).map_err(|e| {
        tracing::error!("the result of `{command}` can't be written as JSON: {e}");
        CommandError::Internal { command: command.to_owned() }
    })
}

impl CommandSpec {
    fn info<P: Params, R: JsonSchema>(meta: &Meta, kind: Kind, lane: Option<Lane>) -> CommandInfo {
        CommandInfo {
            name: meta.name,
            title: meta.title,
            summary: meta.summary,
            help: meta.help,
            kind,
            lane,
            undoable: meta.undoable,
            requires: meta.requires,
            effects: meta.effects,
            params: schema_for::<P>(),
            result: schema_for::<R>(),
            examples: meta.examples.iter().map(|e| serde_json::from_str(e).unwrap_or(Value::Null)).collect(),
        }
    }

    fn checker<P: Params>(name: &'static str) -> Check {
        Box::new(move |params| parse::<P>(name, params).map(drop))
    }

    /// A query: reads the session and returns `R`.
    pub fn query<P, R>(meta: Meta, f: impl Fn(&Session, P) -> Result<R, CommandError> + Send + Sync + 'static) -> CommandSpec
    where
        P: Params + 'static,
        R: Serialize + JsonSchema + 'static,
    {
        let name = meta.name;
        CommandSpec {
            info: CommandSpec::info::<P, R>(&meta, Kind::Query, None),
            supersede: false,
            check: CommandSpec::checker::<P>(name),
            run: Runner::Query(Box::new(move |session, params| to_json(name, &f(session, parse(name, params)?)?))),
        }
    }

    /// A command that runs on the engine's thread and may change the session and the project.
    pub fn instant<P, R>(meta: Meta, f: impl Fn(&mut Session, P) -> Result<R, CommandError> + Send + Sync + 'static) -> CommandSpec
    where
        P: Params + 'static,
        R: Serialize + JsonSchema + 'static,
    {
        let name = meta.name;
        CommandSpec {
            info: CommandSpec::info::<P, R>(&meta, Kind::Instant, None),
            supersede: false,
            check: CommandSpec::checker::<P>(name),
            run: Runner::Instant(Box::new(move |session, params| to_json(name, &f(session, parse(name, params)?)?))),
        }
    }

    /// A command that runs as a job in `lane`: `f` plans it (on the engine's thread, from the session as it is when
    /// the job starts), and the plan's work and commit do it.
    pub fn job<P, R>(meta: Meta, lane: Lane, f: impl Fn(&Session, P) -> Result<JobPlan<R>, CommandError> + Send + Sync + 'static) -> CommandSpec
    where
        P: Params + 'static,
        R: Serialize + JsonSchema + 'static,
    {
        let name = meta.name;
        CommandSpec {
            info: CommandSpec::info::<P, R>(&meta, Kind::Job, Some(lane)),
            supersede: meta.supersede,
            check: CommandSpec::checker::<P>(name),
            run: Runner::Job(Box::new(move |session, params| Ok(f(session, parse(name, params)?)?.map_result(move |r| to_json(name, &r))))),
        }
    }

    pub fn name(&self) -> &'static str {
        self.info.name
    }

    pub fn describe(&self) -> &CommandInfo {
        &self.info
    }

    pub(crate) fn lane(&self) -> Option<Lane> {
        self.info.lane
    }
}

/// Why a set of commands can't make a registry.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    #[error("`{name}` is registered twice")]
    Duplicate { name: String },
    #[error("`{name}` isn't a valid command name: use `area.verbObject`, lower camel case, at most 64 characters")]
    BadName { name: String },
}

/// Whether `name` is a valid command name (see the module documentation).
pub fn valid_name(name: &str) -> bool {
    let mut parts = name.split('.');
    let area_ok = parts.next().is_some_and(|area| !area.is_empty() && area.bytes().all(|b| b.is_ascii_lowercase()));
    let rest: Vec<&str> = parts.collect();
    name.len() <= 64
        && area_ok
        && !rest.is_empty()
        && rest.iter().all(|p| p.bytes().next().is_some_and(|b| b.is_ascii_lowercase()) && p.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// The commands, by name. See the module documentation.
pub struct Registry {
    specs: Vec<Arc<CommandSpec>>,
    by_name: HashMap<&'static str, usize>,
    aliases: HashMap<&'static str, &'static str>,
}

/// Collects commands into a [`Registry`].
#[derive(Default)]
pub struct RegistryBuilder {
    specs: Vec<CommandSpec>,
    aliases: Vec<(&'static str, &'static str)>,
}

impl RegistryBuilder {
    /// Adds FloraStack's own commands.
    #[must_use]
    pub fn standard(mut self) -> RegistryBuilder {
        self.specs.extend(crate::commands::all());
        self.aliases.extend_from_slice(crate::commands::ALIASES);
        self
    }

    /// Adds a command (later phases' commands, or a test's).
    #[must_use]
    pub fn command(mut self, spec: CommandSpec) -> RegistryBuilder {
        self.specs.push(spec);
        self
    }

    pub fn build(self) -> Result<Registry, RegistryError> {
        let mut by_name = HashMap::with_capacity(self.specs.len());
        let mut specs = Vec::with_capacity(self.specs.len());
        for spec in self.specs {
            let name = spec.name();
            if !valid_name(name) {
                return Err(RegistryError::BadName { name: name.to_owned() });
            }
            if by_name.insert(name, specs.len()).is_some() {
                return Err(RegistryError::Duplicate { name: name.to_owned() });
            }
            specs.push(Arc::new(spec));
        }
        let mut aliases = HashMap::new();
        for (old, new) in self.aliases {
            if by_name.contains_key(old) || !by_name.contains_key(new) {
                return Err(RegistryError::Duplicate { name: old.to_owned() });
            }
            aliases.insert(old, new);
        }
        Ok(Registry { specs, by_name, aliases })
    }
}

impl Registry {
    pub fn builder() -> RegistryBuilder {
        RegistryBuilder::default()
    }

    /// FloraStack's own commands, built once.
    pub fn standard() -> Arc<Registry> {
        static STANDARD: OnceLock<Arc<Registry>> = OnceLock::new();
        STANDARD
            .get_or_init(|| {
                Arc::new(Registry::builder().standard().build().unwrap_or_else(|e| {
                    tracing::error!("the built-in commands don't make a registry: {e}");
                    Registry { specs: Vec::new(), by_name: HashMap::new(), aliases: HashMap::new() }
                }))
            })
            .clone()
    }

    /// Every command, sorted by name.
    pub fn list(&self) -> Vec<&CommandInfo> {
        let mut all: Vec<&CommandInfo> = self.specs.iter().map(|s| &s.info).collect();
        all.sort_by_key(|i| i.name);
        all
    }

    /// Every command name, sorted.
    pub fn names(&self) -> Vec<&'static str> {
        self.list().into_iter().map(|i| i.name).collect()
    }

    /// The command called `name` (or once called so).
    pub fn get(&self, name: &str) -> Option<&CommandInfo> {
        self.spec(name).ok().map(|s| &s.info)
    }

    /// Checks parameters for a command without running it.
    pub fn check(&self, name: &str, params: Value) -> Result<(), CommandError> {
        let spec = self.spec(name)?;
        (spec.check)(normalize(spec.name(), params)?)
    }

    pub(crate) fn spec(&self, name: &str) -> Result<&Arc<CommandSpec>, CommandError> {
        let name = self.aliases.get(name).copied().unwrap_or(name);
        self.by_name
            .get(name)
            .and_then(|&i| self.specs.get(i))
            .ok_or_else(|| CommandError::UnknownCommand { name: name.chars().take(100).collect(), suggestion: self.suggest(name) })
    }

    /// The registered name closest to a mistyped one, if one is close.
    fn suggest(&self, name: &str) -> Option<&'static str> {
        if name.len() > 100 {
            return None;
        }
        let lower = name.to_ascii_lowercase();
        self.specs.iter().map(|s| (distance(&lower, &s.name().to_ascii_lowercase()), s.name())).filter(|&(d, _)| d <= 3).min().map(|(_, n)| n)
    }
}

/// Parameters as a command reads them: an object, `null` meaning `{}`.
pub(crate) fn normalize(command: &str, params: Value) -> Result<Value, CommandError> {
    match params {
        Value::Null => Ok(Value::Object(serde_json::Map::new())),
        Value::Object(_) => Ok(params),
        _ => Err(CommandError::Invalid { command: command.to_owned(), source: Invalid::new("params", "must be a JSON object") }),
    }
}

/// The edit distance between two short strings.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut previous = row.first().copied().unwrap_or(0);
        if let Some(first) = row.first_mut() {
            *first = i + 1;
        }
        for (j, cb) in b.iter().enumerate() {
            let above = row.get(j + 1).copied().unwrap_or(0);
            let left = row.get(j).copied().unwrap_or(0);
            let value = (previous + usize::from(ca != *cb)).min(above + 1).min(left + 1);
            previous = above;
            if let Some(cell) = row.get_mut(j + 1) {
                *cell = value;
            }
        }
    }
    row.last().copied().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_scheme() {
        for good in ["project.new", "project.addFrames", "stack.depthMap.composite", "edit.undo", "job.cancel"] {
            assert!(valid_name(good), "{good}");
        }
        for bad in ["project", "Project.new", "project.AddFrames", "project.add_frames", "project.", ".new", "project..x", "pro ject.new", "x.1a"] {
            assert!(!valid_name(bad), "{bad}");
        }
        assert!(!valid_name(&format!("a.{}", "b".repeat(63))));
    }

    #[test]
    fn edit_distance() {
        assert_eq!(distance("project.ad", "project.new"), 3);
        assert_eq!(distance("", "abc"), 3);
        assert_eq!(distance("edit.undo", "edit.undo"), 0);
        assert_eq!(distance("edit.redo", "edit.undo"), 2);
    }
}
