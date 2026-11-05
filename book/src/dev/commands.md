# The command registry

Every user-facing action in FloraStack is a named command in `florastack-engine`'s registry, such as `align.run` or
`stack.pyramid`: buttons, menu items, shortcuts, the command-line tool's subcommands, the command palette, the desktop
control channel and MCP tools all dispatch them. The registry is the only way the project changes, which is what makes
every action scriptable, undoable and testable headless. The [engine commands](../reference/commands.md) page lists
them all, generated from the registry.

## What a command is

A command has:

- a **name**, `area.verbObject` (`project.addFrames`, `develop.set`): a lower-case area and a lower camel-case action,
  at most 64 characters. Names are an interface for scripts and MCP clients: a renamed command keeps its old name as an
  alias (`commands::ALIASES`), and `crates/engine/tests/commands.txt` pins the list;
- a **kind**: a **query** reads (`CommandSpec::query`), an **instant** command runs on the engine's thread
  (`CommandSpec::instant`), a **job** does long work on a worker (`CommandSpec::job`, on a lane);
- a **`Meta`**: a title for menus and the palette, a one-sentence summary, help text, examples, and flags: whether it is
  `undoable()`, what it `requires` (nothing, an open project, a writable one) and what it `effects` (nothing, the
  project, files outside it);
- **parameters**, a struct with `#[serde(deny_unknown_fields, rename_all = "camelCase")]` and `JsonSchema`, a doc
  comment on every field (it becomes the schema's description) and `Params::check` for what types can't say: ranges,
  lengths, paths without NUL;
- a **result**, a struct with `Serialize` and `JsonSchema`.

Commands live in `crates/engine/src/commands/<area>.rs`; `crates/engine/src/registry.rs` documents the details.

## An example: renaming an output

`output.rename`, in `crates/engine/src/commands/output.rs`, is a whole command:

```rust,ignore
CommandSpec::instant(
    Meta::new("output.rename", "Rename output", "Renames an output.")
        .undoable()
        .examples(&[r#"{ "output": 4, "name": "Pyramid, grit 0.5" }"#]),
    rename,
),

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RenameParams {
    /// The output's id, as `output.list` lists it.
    output: OutputId,
    /// Its new name: at most 200 characters, no control characters.
    name: String,
}

impl Params for RenameParams {
    fn check(&self) -> Result<(), Invalid> {
        check_name("name", &self.name)
    }
}

/// An output's name now.
#[derive(Debug, Serialize, JsonSchema)]
struct Renamed {
    output: OutputId,
    name: String,
}

fn rename(session: &mut Session, p: RenameParams) -> Result<Renamed, CommandError> {
    if find(session, p.output)?.name != p.name {
        session.apply(Edit::RenameOutput { output: p.output, name: p.name.clone() })?;
    }
    Ok(Renamed { output: p.output, name: p.name })
}
```

The project changes only through `Session::apply` with an `Edit` (florastack-project): a command's edits are one undo
step, undone by their inverses. A new kind of change is a new `Edit` variant, with a test that its inverse undoes it.

## Jobs

A job (`CommandSpec::job(meta, lane, plan)`) does long work without blocking the engine:

1. Its **plan** runs on the engine's thread, from a snapshot of what it reads, and locks the aspects of the project it
   depends on (`JobPlan::locks`), so that no other command changes them meanwhile.
2. Its **work** runs on a worker. It never touches the session; it calls `Monitor::step` after each unit (a frame, a
   tile), which reports progress and stops with `Cancelled` when the job is cancelled.
3. It returns a **commit**, which the engine runs on its thread to apply the result as edits.

Something irreversible outside the project, such as writing the user's file, comes after `JobContext::finalize`, past
the last point where it can be cancelled. Previews that a newer request makes obsolete use `Meta::supersede`.

## Tests

A new command comes with:

- a dispatch from JSON in `crates/engine/tests/dispatch.rs` (a test fails when a registered command is never
  dispatched there);
- for an undoable command, a case in `crates/engine/tests/undo.rs`, which checks that undo and redo restore the project;
- its hostile parameters in `crates/engine/tests/hostile.rs`: out of range, too long, missing files, ids that don't
  exist, each an error, never a panic;
- its name in `crates/engine/tests/commands.txt`.

`crates/engine/tests/registry.rs` checks every command's texts, schemas (draft 2020-12, inlined, every parameter
described, unknown parameters refused) and that its examples fit them. And the book's reference page must be written
again: `cargo xtask book --update-reference` (see [Writing the documentation](docs.md)).

## Using a command from the apps

The desktop app runs commands through its `Shell` (`app.shell.run("output.rename", json!({ ... }))`) and shows their
progress from the engine's events; the command-line tool's subcommands and `run` dispatch them the same way. When a
screen needs a computation, it becomes a command or a query in the engine, with tests, and the screen calls it: no
logic lives only in a front end.
