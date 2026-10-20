//! The FloraStack command-line tool: it parses arguments and dispatches `florastack-engine` commands, with no logic of
//! its own, and prints progress to stderr. It may depend on no FloraStack crate other than `florastack-engine`, which
//! re-exports everything it needs.

use florastack_engine::branding;

fn main() {
    println!("{} {}: no commands yet", branding::CLI_BINARY, env!("CARGO_PKG_VERSION"));
}
