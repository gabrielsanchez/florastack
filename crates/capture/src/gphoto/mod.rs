//! System libgphoto2, dynamically loaded. No static link or LGPL Rust dependency.
mod ffi;
pub use ffi::{Connection, discover};
