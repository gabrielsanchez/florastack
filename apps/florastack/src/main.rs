//! The FloraStack desktop app: a thin egui layer that turns user actions into `florastack-engine` commands and shows
//! project state and job progress. It contains no processing logic, and it may depend on no FloraStack crate other than
//! `florastack-engine`, which re-exports everything it needs.

use florastack_engine::branding;

fn main() {
    println!("{} {}: the desktop UI is not built yet", branding::DISPLAY_NAME, env!("CARGO_PKG_VERSION"));
}
