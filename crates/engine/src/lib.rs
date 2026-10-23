//! The engine: the command registry through which every user-facing action runs, the job engine for long,
//! cancellable work, the undo history, and the orchestration that ties the other crates together. It may depend on
//! every FloraStack library crate except `florastack-metrics`, and it re-exports the types the apps need, so that the
//! apps depend on it alone.
//!
//! - [`Engine`] ([`engine`]): the API the desktop app, the CLI, the control channel and the MCP server use: dispatch
//!   a command by name with JSON parameters, follow jobs, read the open project, subscribe to [`Event`]s.
//! - [`registry`]: the commands, with their JSON schemas, help texts and flags ([`Registry::list`]), and how they are
//!   made and named; [`commands`]: FloraStack's own.
//! - [`session`]: what commands see ([`Session`]): the open project, which changes only through
//!   [`Session::apply`], as one undo step per command ([`history`]).
//! - [`jobs`]: long commands as jobs on workers, in lanes, with progress, ETAs and cancellation at checkpoints.
//! - [`frames`]: reading a stack's frames, each file sent to the crate that reads it (camera RAW and DNG files to
//!   florastack-raw, developed alike; 16-bit TIFF and other image files to florastack-codecs, unchanged).
//! - [`project`]: the project with the other crates' types: the develop settings and their resolution, the alignment
//!   settings and runs with their reports (each frame's flags and motion), each frame's EXIF summary, which
//!   florastack-project stores as data, and the cache's images (lossless TIFF: the defect mask, outputs) and previews
//!   (JPEG: the filmstrip's, as shot and aligned), which it stores as bytes.

pub mod commands;
pub mod engine;
pub mod error;
pub mod events;
pub mod frames;
pub mod history;
pub mod jobs;
pub mod project;
pub mod registry;
pub mod schema;
pub mod session;

pub use engine::{DispatchOptions, Dispatched, Engine, EngineConfig, MAX_PARAMS_BYTES};
pub use error::{CommandError, ErrorInfo, Invalid};
pub use events::{Event, ProjectEvent};
pub use jobs::{Clock, Executor, InlineExecutor, JobContext, JobId, JobInfo, JobPlan, JobState, Lane, ManualClock, Progress, Waker, commit};
#[cfg(not(target_arch = "wasm32"))]
pub use jobs::{SystemClock, ThreadExecutor};
pub use registry::{CommandInfo, CommandSpec, Effects, Kind, Meta, NoParams, Params, Registry, RegistryBuilder, RegistryError, Requires};
pub use session::Session;

/// JSON values, which commands take and return.
pub use serde_json::{Value, json};

// The product identity.
pub use florastack_core::branding;
// Images and geometry, for viewers and for whatever builds images to hand to the engine.
pub use florastack_core::{Affine, AnyImage, Cancelled, ColorSpace, Encoding, Image, Monitor, Orientation, Rect, Sample, SampleFormat};
// Reading frames and writing outputs.
pub use florastack_codecs::{
    BitDepth, ChromaSubsampling, DngCompression, DngLook, ExifSummary, ExrCompression, Format, OrientationPolicy, OutputFormat, OutputProfile,
    OutputSpec, ReadOptions, TiffCompression, WideSpace,
};
// The alignment settings, and what alignment reports about frames and stacks.
pub use florastack_align::{AlignSettings, End, Flag, Framing, Term};
// The RAW develop settings.
pub use florastack_raw::{ChromaticAberration, Demosaic, DevelopSettings, Highlights, NoiseReduction, Quality, RawDevelop, Resolved, WhiteBalance};

/// The project model, as the apps show it (read from [`Engine::project`]; changed only through commands). The
/// retouch crate has no types yet; its brushes and strokes join these in roadmap 5.3.
pub mod model {
    #[cfg(not(target_arch = "wasm32"))]
    pub use florastack_project::SystemTrash;
    pub use florastack_project::{
        AlignedFrame, Alignment, AlignmentRun, Aspects, CacheRef, ContentHash, FolderTrash, Frame, FrameId, FrameState, Group, Input, LayerId,
        MapName, Orphan, Output, OutputId, Preferences, Preset, Project, RetouchLayer, Timestamp, Trash,
    };
}
