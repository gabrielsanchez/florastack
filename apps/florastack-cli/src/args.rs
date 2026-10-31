//! Argument syntax and its translation to command parameters.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand, ValueEnum};
use florastack_engine::{Device, branding};

pub const EXIT_CODES: &str =
    "Exit codes: 0 success (including help/version); 1 processing or I/O failure; 2 invalid arguments or command parameters; 130 cancelled.";

fn examples(lines: &[&str]) -> String {
    let mut text = String::from("Examples:\n");
    for line in lines {
        let line = line.replace("{project}", branding::PROJECT_EXTENSION);
        let _ = writeln!(text, "  {} {line}", branding::CLI_BINARY);
    }
    text.push('\n');
    text.push_str(EXIT_CODES);
    text
}

#[derive(Debug, Parser)]
#[command(name = branding::CLI_BINARY, version, about = "Align and focus-stack photos.",
    after_help = examples(&["stack photos --out result.tif", "commands", "project --help"]))]
pub struct Cli {
    /// Emit engine events and diagnostics as JSON lines on stderr.
    #[arg(long, global = true)]
    pub json_progress: bool,
    /// Folder for unnamed projects (default: the system temporary folder / app ID / projects).
    #[arg(long, global = true, value_name = "FOLDER")]
    pub temporary_projects: Option<PathBuf>,
    /// Which device stacks: auto (the GPU when a hardware adapter passes its self-test, else the CPU), cpu, or gpu
    /// (software adapters such as lavapipe and WARP too).
    #[arg(long, global = true, value_enum, default_value = "auto")]
    pub device: DeviceArg,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DeviceArg {
    Auto,
    Cpu,
    Gpu,
}

impl DeviceArg {
    pub fn device(self) -> Device {
        match self {
            Self::Auto => Device::Auto,
            Self::Cpu => Device::Cpu,
            Self::Gpu => Device::Gpu,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Align, stack and export files or the image files directly in a folder.
    #[command(after_help = examples(&[
        "stack photos --method pyramid --out result.tif",
        "stack frame1.tif frame2.tif --method both --out result.tif --crop common",
        "stack photos --slab 15:5 --align shift,scale --preset macro.toml --out result.tif"
    ]))]
    Stack(StackArgs),
    /// Queue stack folders, or auto-split a folder of frames, with one saved project per stack.
    #[command(after_help = examples(&["batch --in stacks --out results --preset macro.toml", "batch --in photos --out results --preset macro.toml --method depthmap"]))]
    Batch(BatchArgs),
    /// Capture arriving files, update a preview and finalize after a quiet period.
    #[command(after_help = examples(&["watch capture --preset macro.toml", "watch capture --out results --preset macro.toml --idle 10"]))]
    Watch(WatchArgs),
    /// Open a project and dispatch commands in order, waiting for each job.
    #[command(after_help = examples(&[
        "run Bee{project} --cmd align.run --params '{}' --cmd stack.pyramid --params '{}'",
        "run Bee{project} --cmd project.info"
    ]))]
    Run(RunArgs),
    /// Print every registered command, help text and JSON parameter/result schema as JSON.
    #[command(after_help = examples(&["commands > commands.json"]))]
    Commands,
    /// Create, inspect and clean up projects.
    #[command(after_help = examples(&["project new Bee --folder .", "project info Bee{project}", "project clean-orphans --dry-run"]))]
    Project {
        #[command(subcommand)]
        command: ProjectCommand,
    },
}

#[derive(Debug, Args)]
pub struct BatchArgs {
    /// Folder of stack folders, or one flat folder to auto-split.
    #[arg(long = "in", value_name = "FOLDER")]
    pub input: PathBuf,
    /// Destination for TIFFs, saved projects and summary.csv; existing entries are refused.
    #[arg(long, value_name = "FOLDER")]
    pub out: PathBuf,
    #[command(flatten)]
    pub processing: QueueOptions,
}

#[derive(Debug, Args)]
pub struct WatchArgs {
    /// Folder where the tethering application writes frames.
    pub folder: PathBuf,
    /// Destination (default: a sibling folder named <capture-folder>-stacked).
    #[arg(long, value_name = "FOLDER")]
    pub out: Option<PathBuf>,
    /// Finalize after this many seconds without new or changed frames.
    #[arg(long, alias = "timeout", default_value = "5")]
    pub idle: f64,
    /// Require unchanged file size and modification time for this many milliseconds before decoding.
    #[arg(long, default_value = "1000")]
    pub settle_ms: u64,
    /// Maximum milliseconds between directory scans, in addition to filesystem events.
    #[arg(long, default_value = "250")]
    pub poll_ms: u64,
    #[command(flatten)]
    pub processing: QueueOptions,
}

#[derive(Debug, Args)]
pub struct QueueOptions {
    /// TOML preset, including depth-map contrast thresholds.
    #[arg(long, value_name = "FILE")]
    pub preset: PathBuf,
    /// Stacking method (one output per stack).
    #[arg(long, default_value = "pyramid", value_parser = parse_queue_method)]
    pub method: Method,
    /// TIFF filename: {stack}, {method}, {range}, {first}, {last}, {date} (UTC), {params}.
    #[arg(long, default_value = "{stack}-{method}-{range}.tif")]
    pub template: String,
}

fn parse_queue_method(value: &str) -> Result<Method, String> {
    match value {
        "pyramid" => Ok(Method::Pyramid),
        "depthmap" => Ok(Method::Depthmap),
        "average" => Ok(Method::Average),
        _ => Err("expected pyramid, depthmap or average".into()),
    }
}

#[derive(Debug, Args)]
pub struct StackArgs {
    /// Input files in focus order, or folders (files directly inside, sorted by name).
    #[arg(required = true, num_args = 1.., value_name = "FILES_OR_FOLDER")]
    pub files: Vec<PathBuf>,
    /// Both writes <stem>-pyramid.tif and <stem>-depthmap.tif.
    #[arg(long, value_enum, default_value = "pyramid")]
    pub method: Method,
    /// Destination TIFF file; existing files are refused.
    #[arg(long, value_name = "FILE")]
    pub out: PathBuf,
    /// Stack overlapping slabs, then their results, using the selected method at both stages.
    #[arg(long, value_parser = parse_slab, value_name = "SIZE:OVERLAP")]
    pub slab: Option<Slab>,
    /// Terms to correct (replaces the preset's switches); omit to keep preset/engine defaults.
    #[arg(long, value_enum, value_delimiter = ',', num_args = 1, value_name = "TERMS")]
    pub align: Option<Vec<AlignTerm>>,
    /// Output framing; omit to keep the preset/engine default.
    #[arg(long, value_enum)]
    pub crop: Option<Crop>,
    /// Layer a TOML preset before explicit options.
    #[arg(long, value_name = "FILE")]
    pub preset: Option<PathBuf>,
    /// Use the camera's white balance for the whole RAW stack.
    #[arg(long, value_enum)]
    pub raw_wb: Option<RawWb>,
    /// TIFF sample depth.
    #[arg(long, value_enum, default_value = "16")]
    pub depth: Depth,
    /// Sensor defect mask: black spots on white, at the input frames' size.
    #[arg(long, value_name = "FILE")]
    pub dust_mask: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Method {
    Pyramid,
    Depthmap,
    Average,
    Both,
}

impl Method {
    pub fn name(self) -> &'static str {
        match self {
            Self::Pyramid => "pyramid",
            Self::Depthmap => "depth-map",
            Self::Average => "average",
            Self::Both => "both",
        }
    }

    pub fn command(self) -> &'static str {
        match self {
            Self::Pyramid | Self::Both => "stack.pyramid",
            Self::Depthmap => "stack.depthMap",
            Self::Average => "stack.average",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AlignTerm {
    Shift,
    Scale,
    Rotation,
    Brightness,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Crop {
    Narrow,
    Wide,
    Common,
}

impl Crop {
    pub fn name(self) -> &'static str {
        match self {
            Self::Narrow => "narrow",
            Self::Wide => "wide",
            Self::Common => "common",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum RawWb {
    Asshot,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Depth {
    #[value(name = "16")]
    U16,
    #[value(name = "32f")]
    F32,
}

#[derive(Debug, Clone, Copy)]
pub struct Slab {
    pub size: usize,
    pub overlap: usize,
}

fn parse_slab(text: &str) -> Result<Slab, String> {
    let bad = || "expected SIZE:OVERLAP, with 2 <= SIZE <= 100000 and 0 <= OVERLAP < SIZE".to_owned();
    let (size, overlap) = text.split_once(':').ok_or_else(bad)?;
    let slab = Slab { size: size.parse().map_err(|_| bad())?, overlap: overlap.parse().map_err(|_| bad())? };
    if !(2..=100_000).contains(&slab.size) || slab.overlap >= slab.size {
        return Err(bad());
    }
    Ok(slab)
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Project folder or its project.json.
    pub project: PathBuf,
    /// Registered command, repeatable; commands run in the order given.
    #[arg(long = "cmd", required = true, action = clap::ArgAction::Append)]
    pub commands: Vec<String>,
    /// JSON object for the corresponding --cmd; give one per command, or omit all for {}.
    #[arg(long, action = clap::ArgAction::Append)]
    pub params: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub enum ProjectCommand {
    /// Create a named project, or an unnamed temporary project when NAME is omitted.
    #[command(after_help = examples(&["project new Bee --folder photos", "project new"]))]
    New {
        /// Project name (the engine appends the project-folder extension).
        name: Option<String>,
        /// Parent directory for a named project.
        #[arg(long, requires = "name")]
        folder: Option<PathBuf>,
    },
    /// Describe a project without taking its editing lock.
    #[command(after_help = examples(&["project info Bee{project}"]))]
    Info { project: PathBuf },
    /// Move old, closed, unnamed projects to the operating system's trash.
    #[command(after_help = examples(&["project clean-orphans --dry-run", "project clean-orphans --days 30"]))]
    CleanOrphans {
        /// Minimum age in days since last use.
        #[arg(long, default_value = "30", value_parser = clap::value_parser!(u32).range(0..=36_500))]
        days: u32,
        /// List candidates without moving them.
        #[arg(long)]
        dry_run: bool,
    },
}

/// File-only invocations (including Windows Explorer drops) use the normal stack parser and defaults.
pub fn normalize_drop(mut args: Vec<OsString>) -> Vec<OsString> {
    let paths = args.get(1..).unwrap_or_default();
    if paths.is_empty() || !paths.iter().all(|p| Path::new(p).is_file()) {
        return args;
    }
    let Some(first) = paths.first() else { return args };
    let first = Path::new(first);
    let mut name = first.file_stem().unwrap_or_default().to_os_string();
    name.push("-stacked.tif");
    let out = first.with_file_name(name);
    args.insert(1, OsString::from("stack"));
    // End option parsing before dropped filenames, which can themselves begin with a hyphen.
    let mut output_arg = OsString::from("--out=");
    output_arg.push(out);
    args.splice(2..2, [output_arg, OsString::from("--")]);
    args
}
