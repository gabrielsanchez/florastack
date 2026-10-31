//! Starts the FloraStack desktop app (`florastack_app`): its log on standard error, at the level `FLORASTACK_LOG`
//! names (`warn` by default), and the files given on the command line opened (a project) or added as frames.

use std::path::PathBuf;
use std::process::ExitCode;

use florastack_app::{Options, run};
use florastack_engine::branding;
use tracing_subscriber::filter::LevelFilter;

fn main() -> ExitCode {
    let level = std::env::var("FLORASTACK_LOG").ok().and_then(|l| l.parse::<LevelFilter>().ok()).unwrap_or(LevelFilter::WARN);
    tracing_subscriber::fmt().with_max_level(level).with_writer(std::io::stderr).init();
    let files: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    match run(Options::standard(files)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{}: the window couldn't open: {e}", branding::DISPLAY_NAME);
            ExitCode::FAILURE
        }
    }
}
