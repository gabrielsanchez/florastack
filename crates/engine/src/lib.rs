//! The engine: the command registry through which every user-facing action runs, the job system for long, cancellable
//! work, and the orchestration that ties the other crates together. It may depend on every FloraStack library crate
//! except `florastack-metrics`, and it re-exports the types the apps need, so that the apps depend on it alone.

pub use florastack_core::branding;
