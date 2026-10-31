//! The WGSL kernels (`src/shaders/`): one compute entry point, `main`, per file, each with its own parameters and
//! bindings, and the helpers of `common.wgsl` prepended. Each file's comment names its CPU reference.

/// The shared helpers and constants, prepended to every kernel.
const COMMON: &str = include_str!("shaders/common.wgsl");

/// A compute kernel. See the module documentation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Kernel {
    Fill,
    FillU32,
    CopyRect,
    Combine,
    Unpack,
    Transfer,
    Blur,
    Laplacian,
    Warp,
    Luma,
    Energy,
    Sharpen,
    Accumulate,
    Normalize,
    Clean,
    Filter5,
    Expand,
    Coverage,
    BandEnergy,
    Select,
    Bounds,
    Coefficients,
    Residual,
    Limit,
    FocusNormalize,
    Mark,
    Record,
    Stats,
    Composite,
    CompositeResult,
}

impl Kernel {
    /// The kernel's name, its file's.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Kernel::Fill => "fill",
            Kernel::FillU32 => "fill_u32",
            Kernel::CopyRect => "copy_rect",
            Kernel::Combine => "combine",
            Kernel::Unpack => "unpack",
            Kernel::Transfer => "transfer",
            Kernel::Blur => "blur",
            Kernel::Laplacian => "laplacian",
            Kernel::Warp => "warp",
            Kernel::Luma => "luma",
            Kernel::Energy => "energy",
            Kernel::Sharpen => "sharpen",
            Kernel::Accumulate => "accumulate",
            Kernel::Normalize => "normalize",
            Kernel::Clean => "clean",
            Kernel::Filter5 => "filter5",
            Kernel::Expand => "expand",
            Kernel::Coverage => "coverage",
            Kernel::BandEnergy => "band_energy",
            Kernel::Select => "select",
            Kernel::Bounds => "bounds",
            Kernel::Coefficients => "coefficients",
            Kernel::Residual => "residual",
            Kernel::Limit => "limit",
            Kernel::FocusNormalize => "focus_normalize",
            Kernel::Mark => "mark",
            Kernel::Record => "record",
            Kernel::Stats => "stats",
            Kernel::Composite => "composite",
            Kernel::CompositeResult => "composite_result",
        }
    }

    fn body(self) -> &'static str {
        match self {
            Kernel::Fill => include_str!("shaders/fill.wgsl"),
            Kernel::FillU32 => include_str!("shaders/fill_u32.wgsl"),
            Kernel::CopyRect => include_str!("shaders/copy_rect.wgsl"),
            Kernel::Combine => include_str!("shaders/combine.wgsl"),
            Kernel::Unpack => include_str!("shaders/unpack.wgsl"),
            Kernel::Transfer => include_str!("shaders/transfer.wgsl"),
            Kernel::Blur => include_str!("shaders/blur.wgsl"),
            Kernel::Laplacian => include_str!("shaders/laplacian.wgsl"),
            Kernel::Warp => include_str!("shaders/warp.wgsl"),
            Kernel::Luma => include_str!("shaders/luma.wgsl"),
            Kernel::Energy => include_str!("shaders/energy.wgsl"),
            Kernel::Sharpen => include_str!("shaders/sharpen.wgsl"),
            Kernel::Accumulate => include_str!("shaders/accumulate.wgsl"),
            Kernel::Normalize => include_str!("shaders/normalize.wgsl"),
            Kernel::Clean => include_str!("shaders/clean.wgsl"),
            Kernel::Filter5 => include_str!("shaders/filter5.wgsl"),
            Kernel::Expand => include_str!("shaders/expand.wgsl"),
            Kernel::Coverage => include_str!("shaders/coverage.wgsl"),
            Kernel::BandEnergy => include_str!("shaders/band_energy.wgsl"),
            Kernel::Select => include_str!("shaders/select.wgsl"),
            Kernel::Bounds => include_str!("shaders/bounds.wgsl"),
            Kernel::Coefficients => include_str!("shaders/coefficients.wgsl"),
            Kernel::Residual => include_str!("shaders/residual.wgsl"),
            Kernel::Limit => include_str!("shaders/limit.wgsl"),
            Kernel::FocusNormalize => include_str!("shaders/focus_normalize.wgsl"),
            Kernel::Mark => include_str!("shaders/mark.wgsl"),
            Kernel::Record => include_str!("shaders/record.wgsl"),
            Kernel::Stats => include_str!("shaders/stats.wgsl"),
            Kernel::Composite => include_str!("shaders/composite.wgsl"),
            Kernel::CompositeResult => include_str!("shaders/composite_result.wgsl"),
        }
    }

    /// The kernel's whole source: the common helpers, then its own.
    pub(crate) fn source(self) -> String {
        format!("{COMMON}\n{}", self.body())
    }

    /// Every kernel, for the test that compiles them all.
    pub(crate) const ALL: [Kernel; 30] = [
        Kernel::Fill,
        Kernel::FillU32,
        Kernel::CopyRect,
        Kernel::Combine,
        Kernel::Unpack,
        Kernel::Transfer,
        Kernel::Blur,
        Kernel::Laplacian,
        Kernel::Warp,
        Kernel::Luma,
        Kernel::Energy,
        Kernel::Sharpen,
        Kernel::Accumulate,
        Kernel::Normalize,
        Kernel::Clean,
        Kernel::Filter5,
        Kernel::Expand,
        Kernel::Coverage,
        Kernel::BandEnergy,
        Kernel::Select,
        Kernel::Bounds,
        Kernel::Coefficients,
        Kernel::Residual,
        Kernel::Limit,
        Kernel::FocusNormalize,
        Kernel::Mark,
        Kernel::Record,
        Kernel::Stats,
        Kernel::Composite,
        Kernel::CompositeResult,
    ];
}
