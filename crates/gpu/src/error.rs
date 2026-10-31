//! What can go wrong on the GPU: no usable adapter, a device that refuses or is lost, an error the driver reports, a
//! buffer beyond the device's limits, or a failed readback. Cancellation is an error too, so that a caller can tell
//! it apart from a failure of the GPU and stop instead of falling back to the CPU.

use florastack_core::ops::OpError;
use florastack_core::{Cancelled, ImageError, StoreError, TileError};

/// Why GPU work failed. See the module documentation.
#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    #[error("no GPU adapter is usable: {reason}")]
    NoAdapter { reason: String },
    #[error("the GPU adapter {adapter} refused a device")]
    RequestDevice {
        adapter: String,
        #[source]
        source: wgpu::RequestDeviceError,
    },
    #[error("the GPU reported an error: {message}")]
    Device { message: String },
    #[error("the GPU device was lost: {message}")]
    Lost { message: String },
    #[error("reading {what} back from the GPU failed")]
    Readback { what: &'static str },
    #[error("a GPU buffer of {bytes} bytes for {what} is beyond the device's limit of {limit} bytes")]
    TooLarge { what: &'static str, bytes: u64, limit: u64 },
    #[error("{what} is {got}, but it must be {expected}")]
    Shape { what: &'static str, got: String, expected: String },
    #[error("{what} isn't done on the GPU")]
    Unsupported { what: &'static str },
    #[error("the GPU's {kernel} differs from the CPU's by {difference}, beyond the self-test's tolerance of {tolerance}")]
    SelfTest { kernel: &'static str, difference: f32, tolerance: f32 },
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
    #[error(transparent)]
    Image(#[from] ImageError),
    #[error(transparent)]
    Op(#[from] OpError),
    #[error(transparent)]
    Tile(#[from] TileError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl GpuError {
    /// Whether the work stopped because it was cancelled, rather than because the GPU failed.
    pub fn is_cancelled(&self) -> bool {
        matches!(self, GpuError::Cancelled(_))
    }

    #[allow(clippy::needless_pass_by_value)] // Callers pass numbers and fresh strings: references would only add `&`s.
    pub(crate) fn shape(what: &'static str, got: impl ToString, expected: impl ToString) -> GpuError {
        GpuError::Shape { what, got: got.to_string(), expected: expected.to_string() }
    }
}
