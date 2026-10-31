//! Planes on the GPU ([`Plane`]): a storage buffer of `width` x `height` pixels of `channels` 32-bit samples, laid out
//! as florastack-core's [`Image`] is (rows from the top, channels interleaved), and their uploads and readbacks.
//!
//! A plane's samples are `f32`, or the raw 32-bit words of a few kernels (the pyramid's range bounds); a readback gives
//! the words, which [`Gpu::download`] reads as `f32`. Buffers are zero-initialized by wgpu. A plane never exceeds the
//! device's binding limit ([`Gpu::max_binding`]): the stackers tile their work so that none has to.

use florastack_core::{ColorSpace, Image, Rect};

use crate::device::Gpu;
use crate::error::GpuError;

/// A plane of samples on the GPU. See the module documentation.
#[derive(Debug)]
pub struct Plane {
    pub(crate) buffer: wgpu::Buffer,
    width: usize,
    height: usize,
    channels: usize,
}

impl Plane {
    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The number of samples.
    pub fn len(&self) -> usize {
        self.width * self.height * self.channels
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The bytes the plane takes on the GPU.
    pub fn bytes(&self) -> u64 {
        self.len() as u64 * 4
    }
}

/// The bytes of `len` 32-bit samples, at least 4 (wgpu buffers aren't empty), or `None` on overflow.
fn sample_bytes(len: usize) -> Option<u64> {
    u64::try_from(len).ok()?.checked_mul(4).map(|b| b.max(4))
}

impl Gpu {
    /// A buffer of `bytes` bytes for `what`, within the device's limits.
    pub(crate) fn buffer(&self, what: &'static str, bytes: u64, usage: wgpu::BufferUsages) -> Result<wgpu::Buffer, GpuError> {
        let limit = self.max_binding();
        if bytes > limit {
            return Err(GpuError::TooLarge { what, bytes, limit });
        }
        let buffer = self.device().create_buffer(&wgpu::BufferDescriptor { label: Some(what), size: bytes, usage, mapped_at_creation: false });
        Ok(buffer)
    }

    /// A plane of zeros.
    pub fn plane(&self, what: &'static str, width: usize, height: usize, channels: usize) -> Result<Plane, GpuError> {
        let len = width.checked_mul(height).and_then(|n| n.checked_mul(channels));
        let bytes = len.and_then(sample_bytes).ok_or(GpuError::TooLarge { what, bytes: u64::MAX, limit: self.max_binding() })?;
        let usage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST;
        Ok(Plane { buffer: self.buffer(what, bytes, usage)?, width, height, channels })
    }

    /// `image` on the GPU.
    pub fn upload(&self, image: &Image<f32>) -> Result<Plane, GpuError> {
        self.upload_rect(image, Rect::new(0, 0, image.width(), image.height()))
    }

    /// The pixels of `rect` of `image` on the GPU.
    pub fn upload_rect(&self, image: &Image<f32>, rect: Rect) -> Result<Plane, GpuError> {
        let plane = self.plane("block", rect.width, rect.height, image.channels())?;
        self.write_rect(&plane, image, rect)?;
        Ok(plane)
    }

    /// The pixels of `rect` of `image` written into `plane`, which has the rectangle's size and the image's channels.
    pub(crate) fn write_rect(&self, plane: &Plane, image: &Image<f32>, rect: Rect) -> Result<(), GpuError> {
        let c = image.channels();
        if !rect.fits_in(image.width(), image.height()) || (plane.width, plane.height, plane.channels) != (rect.width, rect.height, c) {
            return Err(GpuError::shape(
                "a block of an image",
                format!("{}x{} at {},{} of {}x{}", rect.width, rect.height, rect.x, rect.y, image.width(), image.height()),
                format!("{}x{} with {} channels inside the image", plane.width, plane.height, plane.channels),
            ));
        }
        if rect.x == 0 && rect.width == image.width() && image.stride() == rect.width * c {
            // Whole rows without padding are contiguous in the image.
            let start = rect.y * rect.width * c;
            if let Some(samples) = image.as_raw().get(start..start + rect.height * rect.width * c) {
                self.queue().write_buffer(&plane.buffer, 0, bytemuck::cast_slice(samples));
                return Ok(());
            }
        }
        let mut block = Vec::with_capacity(rect.width * rect.height * c);
        for y in rect.y..rect.y + rect.height {
            if let Some(row) = image.row(y).and_then(|r| r.get(rect.x * c..(rect.x + rect.width) * c)) {
                block.extend_from_slice(row);
            }
        }
        self.queue().write_buffer(&plane.buffer, 0, bytemuck::cast_slice(&block));
        Ok(())
    }

    /// Words written into `plane` from its start.
    pub(crate) fn write_words(&self, plane: &Plane, words: &[u32]) {
        self.queue().write_buffer(&plane.buffer, 0, bytemuck::cast_slice(words));
    }

    /// `plane`'s raw words, once the GPU has done everything submitted before.
    pub(crate) fn read_words(&self, plane: &Plane, what: &'static str) -> Result<Vec<u32>, GpuError> {
        let len = plane.len();
        let Some(bytes) = sample_bytes(len) else { return Err(GpuError::Readback { what }) };
        let staging = self.buffer(what, bytes, wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST)?;
        let mut encoder = self.device().create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(what) });
        encoder.copy_buffer_to_buffer(&plane.buffer, 0, &staging, 0, bytes);
        self.queue().submit([encoder.finish()]);
        let mapped = std::sync::Arc::new(std::sync::Mutex::new(None));
        let done = std::sync::Arc::clone(&mapped);
        staging.slice(..).map_async(wgpu::MapMode::Read, move |result| {
            *done.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result.is_ok());
        });
        self.wait()?;
        if *mapped.lock().unwrap_or_else(std::sync::PoisonError::into_inner) != Some(true) {
            return Err(GpuError::Readback { what });
        }
        let words = {
            let view = staging.slice(..).get_mapped_range().map_err(|_| GpuError::Readback { what })?;
            let mut words: Vec<u32> = match bytemuck::try_cast_slice::<u8, u32>(&view) {
                Ok(words) => words.to_vec(),
                Err(_) => view.as_chunks::<4>().0.iter().map(|b| u32::from_le_bytes(*b)).collect(),
            };
            words.truncate(len);
            words
        };
        staging.unmap();
        Ok(words)
    }

    /// `plane` read back as a linear `f32` image.
    pub fn download(&self, plane: &Plane) -> Result<Image<f32>, GpuError> {
        let words = self.read_words(plane, "a plane")?;
        let samples = words.into_iter().map(f32::from_bits).collect();
        Ok(Image::from_vec(plane.width, plane.height, plane.channels, ColorSpace::LINEAR, samples)?)
    }
}
