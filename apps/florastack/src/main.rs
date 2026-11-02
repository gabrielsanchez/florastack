//! Starts the FloraStack desktop app (`florastack_app`): its log on standard error, at the level `FLORASTACK_LOG`
//! names (`warn` by default), and the files given on the command line opened (a project) or added as frames.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use florastack_app::{Options, run};
use florastack_engine::branding;
use tracing_subscriber::filter::LevelFilter;

#[derive(Parser)]
#[command(name = branding::GUI_BINARY, version)]
struct Arguments {
    /// Enable authenticated loopback JSON control (off by default).
    #[arg(long, requires = "control_token_file")]
    control: bool,
    /// File containing a random 32-256 character token; keep it private.
    #[arg(long, requires = "control")]
    control_token_file: Option<PathBuf>,
    /// Loopback port (0 asks the OS for an unused port; printed to stderr).
    #[arg(long, default_value_t = 0, requires = "control")]
    control_port: u16,
    /// Render the full UI offscreen, without a window (requires a usable GPU or software adapter).
    #[arg(long, requires = "control")]
    headless: bool,
    /// Folder for unnamed projects (useful for isolated control sessions).
    #[arg(long)]
    temporary_projects: Option<PathBuf>,
    #[arg(long, default_value_t = 1200, value_parser = clap::value_parser!(u32).range(800..=4096))]
    width: u32,
    #[arg(long, default_value_t = 720, value_parser = clap::value_parser!(u32).range(500..=4096))]
    height: u32,
    files: Vec<PathBuf>,
}

fn main() -> ExitCode {
    let level = std::env::var("FLORASTACK_LOG").ok().and_then(|l| l.parse::<LevelFilter>().ok()).unwrap_or(LevelFilter::WARN);
    tracing_subscriber::fmt().with_max_level(level).with_writer(std::io::stderr).init();
    let args = Arguments::parse();
    let mut options = Options::standard(args.files);
    if let Some(folder) = args.temporary_projects {
        options.engine.temporary_projects = Some(folder);
    }
    let result = if args.control {
        control_run(options, args.control_port, args.control_token_file.as_deref(), args.headless, [args.width, args.height])
    } else {
        run(options).map_err(|e| e.to_string())
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{}: {e}", branding::DISPLAY_NAME);
            ExitCode::FAILURE
        }
    }
}

fn control_run(options: Options, port: u16, file: Option<&std::path::Path>, headless: bool, size: [u32; 2]) -> Result<(), String> {
    let file = file.ok_or("--control requires --control-token-file")?;
    let token = florastack_engine::control::read_token(file).map_err(|e| e.to_string())?;
    if headless {
        return florastack_app::offscreen::run(options, port, token, size);
    }
    eframe::run_native(
        branding::DISPLAY_NAME,
        florastack_app::native_options(),
        Box::new(move |cc| {
            let mut app = florastack_app::FloraStackApp::new(cc, options);
            let address = app.enable_control(&cc.egui_ctx, port, token)?;
            eprintln!("control: {address}");
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| e.to_string())
}
