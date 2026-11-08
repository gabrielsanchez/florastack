//! The self-test the engine runs before it stacks on a device ([`Gpu::self_test`]): a few kernels on a small image,
//! compared with their CPU references in florastack-core, so that a driver that compiles the shaders but computes
//! wrong results (or none) is caught before a stack depends on it. It takes a few milliseconds on a hardware adapter.

use florastack_core::ops::{BlurMethod, Border, Interpolation, gaussian_blur_in_place, laplacian, warp_into};
use florastack_core::{Affine, ColorSpace, Image, Rect};

use crate::device::Gpu;
use crate::error::GpuError;
use crate::ops::{self, WarpGeometry};
use crate::plane::Plane;

/// The largest difference allowed between a kernel's results on the GPU and on the CPU, for samples in `[0, 1]`: the
/// agreement tests bound the same kernels far more tightly; this only needs to tell results from garbage.
const TOLERANCE: f32 = 1e-3;

/// The largest absolute difference between two images' samples, infinite when their shapes differ.
fn difference(a: &Image<f32>, b: &Image<f32>) -> f32 {
    if (a.width(), a.height(), a.channels()) != (b.width(), b.height(), b.channels()) {
        return f32::INFINITY;
    }
    a.as_raw().iter().zip(b.as_raw()).map(|(x, y)| if x.is_finite() && y.is_finite() { (x - y).abs() } else { f32::INFINITY }).fold(0.0, f32::max)
}

/// A kernel of the self-test, run: its name, its result on the GPU, and florastack-core's.
type Run = (&'static str, Plane, Image<f32>);

fn compare(kernel: &'static str, gpu: &Image<f32>, cpu: &Image<f32>) -> Result<(), GpuError> {
    let d = difference(gpu, cpu);
    if d <= TOLERANCE { Ok(()) } else { Err(GpuError::SelfTest { kernel, difference: d, tolerance: TOLERANCE }) }
}

impl Gpu {
    /// Runs the blur (exact and boxes), the Laplacian and the bicubic warp on a 37 x 23 image and compares them with
    /// florastack-core's. See the module documentation.
    pub fn self_test(&self) -> Result<(), GpuError> {
        for (kernel, plane, cpu) in self.self_test_runs()? {
            compare(kernel, &self.download(&plane)?, &cpu)?;
        }
        self.check()
    }

    /// [`Gpu::self_test`] in a browser, where the results are read back by awaiting them.
    #[cfg(target_arch = "wasm32")]
    pub async fn self_test_async(&self) -> Result<(), GpuError> {
        for (kernel, plane, cpu) in self.self_test_runs()? {
            compare(kernel, &self.download_async(&plane).await?, &cpu)?;
        }
        self.check()
    }

    /// The self-test's kernels, run: each one's name, its result on the GPU, and florastack-core's.
    fn self_test_runs(&self) -> Result<Vec<Run>, GpuError> {
        let image = Image::from_fn(37, 23, 3, ColorSpace::LINEAR, |x, y, c| ((x * 7 + y * 13 + c * 5) % 17) as f32 / 16.0)?;
        let mut runs = Vec::new();
        for (sigma, method) in [(1.5, BlurMethod::Exact), (6.0, BlurMethod::Auto)] {
            let mut cpu = image.clone();
            gaussian_blur_in_place(&mut cpu, sigma, method)?;
            runs.push(("Gaussian blur", ops::gaussian_blur_plane(self, &image, sigma, method)?, cpu));
        }
        runs.push(("Laplacian", ops::laplacian_plane(self, &image)?, laplacian(&image)?));
        // A small rotation and shift, inside the frame, where the CPU's clamped warp needs no coverage.
        let to_frame = Affine::similarity((18.0, 11.0), 1.02, 0.05, (0.3, -0.4));
        let rect = Rect::new(4, 4, 29, 15);
        let geometry = WarpGeometry { to_frame, frame_width: 37, frame_height: 23, feather: 0.0, gain: 1.0, white_balance: [1.0; 3] };
        let (warped, coverage) = ops::warp_planes(self, &image, &geometry, rect)?;
        let mut cpu = Image::new(rect.width, rect.height, 3, ColorSpace::LINEAR)?;
        let to_source = Affine::translation(rect.x as f64, rect.y as f64).then(&to_frame);
        warp_into(&image, &to_source, Interpolation::Bicubic, Border::Clamp, &mut cpu)?;
        runs.push(("warp", warped, cpu));
        // Inside the frame with no feather, every weight is 1.
        runs.push(("warp coverage", coverage, Image::from_fn(rect.width, rect.height, 1, ColorSpace::LINEAR, |_, _, _| 1.0)?));
        Ok(runs)
    }
}
