//! Clocks that work in a browser too: the standard library's [`Instant`] and [`SystemTime`] on native targets, and in
//! the browser build (`wasm32-unknown-unknown`, where the standard library's panic) the page's `performance.now()` and
//! `Date.now()`, through `web-time`. The crates that run in the browser take their clocks from here.

pub use std::time::Duration;
pub use web_time::{Instant, SystemTime, SystemTimeError, UNIX_EPOCH};
