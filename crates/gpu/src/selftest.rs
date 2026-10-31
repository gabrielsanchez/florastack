//! The self-test the engine runs before it stacks on a device ([`Gpu::self_test`]): a few kernels on a small image,
//! compared with their CPU references in florastack-core, so that a driver that compiles the shaders but computes
//! wrong results (or none) is caught before a stack depends on it. It takes a few milliseconds on a hardware adapter.

use florastack_core::ops::{BlurMethod, Border, Interpolation, gaussian_blur_in_place, laplacian, warp_into};
use florastack_core::{Affine, ColorSpace, Image, Rect};

use crate::device::Gpu;
use crate::error::GpuError;
use crate::ops::{self, WarpGeometry};

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

fn compare(kernel: &'static str, gpu: &Image<f32>, cpu: &Image<f32>) -> Result<(), GpuError> {
    let d = difference(gpu, cpu);
    if d <= TOLERANCE { Ok(()) } else { Err(GpuError::SelfTest { kernel, difference: d, tolerance: TOLERANCE }) }
}

impl Gpu {
    /// Runs the blur (exact and boxes), the Laplacian and the bicubic warp on a 37 x 23 image and compares them with
    /// florastack-core's. See the module documentation.
    pub fn self_test(&self) -> Result<(), GpuError> {
        let image = Image::from_fn(37, 23, 3, ColorSpace::LINEAR, |x, y, c| ((x * 7 + y * 13 + c * 5) % 17) as f32 / 16.0)?;
        for (sigma, method) in [(1.5, BlurMethod::Exact), (6.0, BlurMethod::Auto)] {
            let mut cpu = image.clone();
            gaussian_blur_in_place(&mut cpu, sigma, method)?;
            compare("Gaussian blur", &ops::gaussian_blur(self, &image, sigma, method)?, &cpu)?;
        }
        compare("Laplacian", &ops::laplacian(self, &image)?, &laplacian(&image)?)?;
        // A small rotation and shift, inside the frame, where the CPU's clamped warp needs no coverage.
        let to_frame = Affine::similarity((18.0, 11.0), 1.02, 0.05, (0.3, -0.4));
        let rect = Rect::new(4, 4, 29, 15);
        let geometry = WarpGeometry { to_frame, frame_width: 37, frame_height: 23, feather: 0.0, gain: 1.0 };
        let (warped, coverage) = ops::warp_to_canvas(self, &image, &geometry, rect)?;
        let mut cpu = Image::new(rect.width, rect.height, 3, ColorSpace::LINEAR)?;
        let to_source = Affine::translation(rect.x as f64, rect.y as f64).then(&to_frame);
        warp_into(&image, &to_source, Interpolation::Bicubic, Border::Clamp, &mut cpu)?;
        compare("warp", &warped, &cpu)?;
        // Inside the frame with no feather, every weight is exactly 1.
        #[allow(clippy::float_cmp)]
        let partial = coverage.as_raw().iter().any(|&k| k != 1.0);
        if partial {
            return Err(GpuError::SelfTest { kernel: "warp coverage", difference: 1.0, tolerance: 0.0 });
        }
        self.check()
    }
}
