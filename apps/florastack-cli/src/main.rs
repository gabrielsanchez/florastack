//! The CLI parses arguments, dispatches registry commands and presents their results and progress.

mod args;
mod client;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use florastack_engine::{BitDepth, Format, OutputSpec, Registry, Value, branding, json};

use args::{AlignTerm, Cli, Command, Depth, Method, ProjectCommand, RunArgs, StackArgs};
use client::{Client, Failure, path_text, print_json, report_error};

fn main() -> ExitCode {
    let arguments = args::normalize_drop(std::env::args_os().collect());
    let json_progress = arguments.iter().take_while(|arg| *arg != "--").any(|arg| arg == "--json-progress");
    let cli = match Cli::try_parse_from(arguments) {
        Ok(cli) => cli,
        Err(error) => {
            if error.use_stderr() && json_progress {
                report_error(&Failure::usage(error.to_string()), true);
            } else {
                let _ = error.print();
            }
            return if error.use_stderr() { ExitCode::from(2) } else { ExitCode::SUCCESS };
        }
    };
    let json_progress = cli.json_progress;
    match execute(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            report_error(&failure, json_progress);
            ExitCode::from(failure.exit)
        }
    }
}

fn execute(cli: Cli) -> Result<(), Failure> {
    if matches!(cli.command, Command::Commands) {
        return print_json(&Registry::standard().list());
    }
    let root = std::path::absolute(cli.temporary_projects.unwrap_or_else(|| std::env::temp_dir().join(branding::APP_ID).join("projects")))?;
    path_text(&root)?;
    let mut client = Client::new(root, cli.json_progress, cli.device.device());
    let result = match cli.command {
        Command::Stack(args) => stack(&mut client, &args),
        Command::Batch(args) => queue(&mut client, "batch.run", &args.input, &args.out, &args.processing, json!({})),
        Command::Watch(args) => {
            let input = std::path::absolute(&args.folder)?;
            let out = args.out.unwrap_or_else(|| {
                let mut name = input.file_name().unwrap_or_default().to_os_string();
                name.push("-stacked");
                input.with_file_name(name)
            });
            queue(
                &mut client,
                "watch.run",
                &input,
                &out,
                &args.processing,
                json!({"idleSeconds": args.idle, "settleMs": args.settle_ms, "pollMs": args.poll_ms}),
            )
        }
        Command::Run(args) => run(&mut client, &args),
        Command::Project { command } => project(&mut client, command),
        Command::Commands => Ok(()),
    };
    client.reporter.clear_bar()?;
    result
}

fn queue(client: &mut Client, command: &str, input: &Path, output: &Path, options: &args::QueueOptions, mut params: Value) -> Result<(), Failure> {
    params["input"] = json!(path_text(input)?);
    params["output"] = json!(path_text(output)?);
    params["preset"] = json!(path_text(&options.preset)?);
    params["method"] = json!(options.method.name());
    params["template"] = json!(options.template);
    let result = client.run(command, params)?;
    print_json(&result)?;
    if result.get("failed").and_then(Value::as_u64).is_some_and(|n| n > 0) {
        return Err(Failure::new(1, "batch", "some stacks failed; see summary.csv for details"));
    }
    Ok(())
}

fn stack(client: &mut Client, args: &StackArgs) -> Result<(), Failure> {
    let paths = args.files.iter().map(|p| path_text(p)).collect::<Result<Vec<_>, _>>()?;
    path_text(&args.out)?;
    client.run("project.new", json!({}))?;
    if let Some(preset) = &args.preset {
        client.run("preset.apply", json!({ "path": path_text(preset)? }))?;
    }
    let mut align = json!({});
    if let Some(terms) = &args.align {
        align = json!({
            "shift_x": terms.contains(&AlignTerm::Shift), "shift_y": terms.contains(&AlignTerm::Shift),
            "scale": terms.contains(&AlignTerm::Scale), "rotation": terms.contains(&AlignTerm::Rotation),
            "brightness": terms.contains(&AlignTerm::Brightness)
        });
    }
    if let Some(crop) = args.crop {
        align["framing"] = json!(crop.name());
    }
    if align.as_object().is_some_and(|m| !m.is_empty()) {
        client.run("align.set", align)?;
    }
    if args.raw_wb.is_some() {
        client.run("develop.set", json!({ "white_balance": "as-shot" }))?;
    }
    let added = client.run("project.addFrames", json!({ "paths": paths }))?;
    if let Some(skipped) = added.get("skipped").and_then(Value::as_array) {
        for skip in skipped {
            client.reporter.warning(&format!(
                "{}: {}",
                skip.get("path").and_then(Value::as_str).unwrap_or("input"),
                skip.get("reason").and_then(Value::as_str).unwrap_or("not added")
            ))?;
        }
        if !skipped.is_empty() {
            return Err(Failure::new(1, "frame", "some inputs were not added; fix the reported inputs before stacking"));
        }
    }
    if added.get("added").and_then(Value::as_array).is_none_or(Vec::is_empty) {
        return Err(Failure::new(1, "frame", "no input frames were found"));
    }
    if let Some(mask) = &args.dust_mask {
        client.run("defects.import", json!({ "path": path_text(mask)? }))?;
    }
    client.run("align.run", json!({}))?;
    let methods: &[Method] = match args.method {
        Method::Both => &[Method::Pyramid, Method::Depthmap],
        Method::Pyramid => &[Method::Pyramid],
        Method::Depthmap => &[Method::Depthmap],
        Method::Average => &[Method::Average],
    };
    let mut written = Vec::new();
    for &method in methods {
        let stacked = if let Some(slab) = args.slab {
            client.run(
                "stack.slabbed",
                json!({
                    "slabSize": slab.size, "overlap": slab.overlap,
                    "slabs": { "method": method.name() }, "final": { "method": method.name() }
                }),
            )?
        } else {
            client.run(method.command(), json!({}))?
        };
        let output =
            stacked.get("output").and_then(Value::as_u64).ok_or_else(|| Failure::new(1, "internal", "the stacking command returned no output ID"))?;
        let path = if args.method == Method::Both { method_path(&args.out, method) } else { args.out.clone() };
        let mut spec = OutputSpec::new(Format::Tiff).map_err(|e| Failure::new(1, "codec", e.to_string()))?;
        spec.depth = match args.depth {
            Depth::U16 => BitDepth::U16,
            Depth::F32 => BitDepth::F32,
        };
        let saved = client.run("output.save", json!({ "output": output, "path": path_text(&path)?, "spec": spec }))?;
        if let Some(warnings) = saved.get("warnings").and_then(Value::as_array) {
            for warning in warnings.iter().filter_map(Value::as_str) {
                client.reporter.warning(warning)?;
            }
        }
        written.push(json!({ "method": method.name(), "stack": stacked, "file": saved }));
    }
    client.run("project.save", json!({}))?;
    let project = client.engine.project_path().map(Path::to_path_buf);
    client.run("project.close", json!({}))?;
    print_json(&json!({ "outputs": written, "project": project }))
}

fn method_path(path: &Path, method: Method) -> PathBuf {
    let mut stem = path.file_stem().unwrap_or_default().to_os_string();
    stem.push(match method {
        Method::Depthmap => "-depthmap",
        _ => "-pyramid",
    });
    let mut result = path.with_file_name(stem);
    result.set_extension(path.extension().unwrap_or_else(|| std::ffi::OsStr::new("tif")));
    result
}

fn run(client: &mut Client, args: &RunArgs) -> Result<(), Failure> {
    if !args.params.is_empty() && args.params.len() != args.commands.len() {
        return Err(Failure::usage("give one --params per --cmd, or omit all --params to use {} for every command"));
    }
    client.run("project.open", json!({ "path": path_text(&args.project)? }))?;
    for (i, command) in args.commands.iter().enumerate() {
        let params = args.params.get(i).map_or("{}", String::as_str);
        let result = client.run_json(command, params)?;
        print_json(&json!({ "command": command, "result": result }))?;
    }
    if client.engine.is_dirty() {
        client.run("project.save", json!({}))?;
    }
    client.run("project.close", json!({}))?;
    Ok(())
}

fn project(client: &mut Client, command: ProjectCommand) -> Result<(), Failure> {
    let result = match command {
        ProjectCommand::New { name, folder } => {
            let params = if let Some(name) = name {
                let folder = folder.unwrap_or(std::env::current_dir()?);
                json!({ "name": name, "folder": path_text(&folder)? })
            } else {
                json!({})
            };
            let result = client.run("project.new", params)?;
            client.run("project.close", json!({}))?;
            result
        }
        ProjectCommand::Info { project } => {
            client.run("project.open", json!({ "path": path_text(&project)?, "readOnly": true }))?;
            let result = client.run("project.info", json!({}))?;
            client.run("project.close", json!({}))?;
            result
        }
        ProjectCommand::CleanOrphans { days, dry_run } => {
            let found = client.run("project.findOrphans", json!({ "days": days }))?;
            if dry_run {
                found
            } else {
                let paths: Vec<&Value> = found.get("orphans").and_then(Value::as_array).into_iter().flatten().filter_map(|o| o.get("path")).collect();
                if paths.is_empty() {
                    json!({ "trashed": [], "failed": [] })
                } else {
                    let result = client.run("project.trashOrphans", json!({ "paths": paths, "days": days }))?;
                    if result.get("failed").and_then(Value::as_array).is_some_and(|f| !f.is_empty()) {
                        print_json(&result)?;
                        return Err(Failure::new(1, "project", "some orphaned projects could not be moved to the trash"));
                    }
                    result
                }
            }
        }
    };
    print_json(&result)
}
