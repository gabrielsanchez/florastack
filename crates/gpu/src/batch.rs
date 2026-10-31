//! Recording GPU work ([`Batch`]): kernel dispatches and copies into a command encoder, submitted together, each
//! dispatch with its parameters in a small uniform buffer ([`Params`]) and its planes bound in order from binding 1.

use wgpu::util::DeviceExt;

use crate::device::Gpu;
use crate::error::GpuError;
use crate::plane::Plane;
use crate::shaders::Kernel;

/// A kernel's parameters: 32-bit words in the order its `Params` struct declares them.
#[derive(Clone, Debug, Default)]
pub(crate) struct Params(Vec<u32>);

/// `v` as a 32-bit word; sizes and offsets beyond it never reach a kernel (planes stay within the binding limit).
fn word(v: usize) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
}

impl Params {
    pub(crate) fn new() -> Params {
        Params(Vec::new())
    }

    #[must_use]
    pub(crate) fn u(mut self, v: usize) -> Params {
        self.0.push(word(v));
        self
    }

    #[must_use]
    pub(crate) fn bits(mut self, v: u32) -> Params {
        self.0.push(v);
        self
    }

    #[must_use]
    pub(crate) fn i(mut self, v: i64) -> Params {
        let v = i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX });
        self.0.push(u32::from_ne_bytes(v.to_ne_bytes()));
        self
    }

    #[must_use]
    pub(crate) fn f(mut self, v: f32) -> Params {
        self.0.push(v.to_bits());
        self
    }

    #[must_use]
    pub(crate) fn flag(self, v: bool) -> Params {
        self.u(usize::from(v))
    }

    /// The words as bytes, padded to a multiple of 16 as uniform buffers are.
    fn bytes(&self) -> Vec<u8> {
        let mut words = self.0.clone();
        while words.is_empty() || !words.len().is_multiple_of(4) {
            words.push(0);
        }
        bytemuck::cast_slice(&words).to_vec()
    }
}

/// The shape of a dispatch: one invocation per pixel of a plane (16 x 16 workgroups), or per item of a flat list
/// (workgroups of 256, in two dimensions when one can't hold them).
#[derive(Clone, Copy, Debug)]
pub(crate) enum Groups {
    Plane { width: usize, height: usize },
    Flat(usize),
}

/// What a kernel binds: a plane, or a raw buffer (the dummy for a plane it doesn't read, a kernel's weights).
#[derive(Clone, Copy)]
pub(crate) enum Bind<'a> {
    Plane(&'a Plane),
    Buffer(&'a wgpu::Buffer),
}

impl<'a> From<&'a Plane> for Bind<'a> {
    fn from(p: &'a Plane) -> Bind<'a> {
        Bind::Plane(p)
    }
}

impl<'a> Bind<'a> {
    fn buffer(self) -> &'a wgpu::Buffer {
        match self {
            Bind::Plane(p) => &p.buffer,
            Bind::Buffer(b) => b,
        }
    }
}

/// GPU work being recorded. See the module documentation.
pub(crate) struct Batch<'g> {
    gpu: &'g Gpu,
    encoder: wgpu::CommandEncoder,
    recorded: usize,
}

impl<'g> Batch<'g> {
    pub(crate) fn new(gpu: &'g Gpu) -> Batch<'g> {
        Batch { gpu, encoder: Batch::encoder(gpu), recorded: 0 }
    }

    fn encoder(gpu: &Gpu) -> wgpu::CommandEncoder {
        gpu.device().create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("florastack") })
    }

    pub(crate) fn gpu(&self) -> &'g Gpu {
        self.gpu
    }

    /// The workgroup counts of `groups`.
    fn counts(&self, groups: Groups) -> Result<(u32, u32), GpuError> {
        let max = self.gpu.max_groups() as usize;
        let (x, y) = match groups {
            Groups::Plane { width, height } => (width.div_ceil(16), height.div_ceil(16)),
            Groups::Flat(n) => {
                let total = n.div_ceil(256).max(1);
                if total <= max { (total, 1) } else { (max, total.div_ceil(max)) }
            }
        };
        if x > max || y > max {
            return Err(GpuError::TooLarge { what: "a dispatch", bytes: (x.max(y)) as u64, limit: max as u64 });
        }
        Ok((u32::try_from(x.max(1)).unwrap_or(u32::MAX), u32::try_from(y.max(1)).unwrap_or(u32::MAX)))
    }

    /// Records `kernel` over `groups` with `params` and `binds` at bindings 1, 2, ...
    pub(crate) fn run(&mut self, kernel: Kernel, params: &Params, binds: &[Bind<'_>], groups: Groups) -> Result<(), GpuError> {
        if matches!(groups, Groups::Plane { width: 0, .. } | Groups::Plane { height: 0, .. } | Groups::Flat(0)) {
            return Ok(());
        }
        let (x, y) = self.counts(groups)?;
        let pipeline = self.gpu.pipeline(kernel)?;
        let device = self.gpu.device();
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(kernel.name()),
            contents: &params.bytes(),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let mut entries = vec![wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() }];
        for (binding, bind) in (1u32..).zip(binds) {
            entries.push(wgpu::BindGroupEntry { binding, resource: bind.buffer().as_entire_binding() });
        }
        let layout = pipeline.get_bind_group_layout(0);
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some(kernel.name()), layout: &layout, entries: &entries });
        {
            let mut pass = self.encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some(kernel.name()), timestamp_writes: None });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(x, y, 1);
        }
        self.recorded += 1;
        Ok(())
    }

    /// Records a copy of `bytes` bytes of `src` to `dst`, both from their start.
    pub(crate) fn copy(&mut self, src: &Plane, dst: &Plane) -> Result<(), GpuError> {
        if src.len() != dst.len() {
            return Err(GpuError::shape("a plane copied", src.len(), format!("{} samples", dst.len())));
        }
        self.encoder.copy_buffer_to_buffer(&src.buffer, 0, &dst.buffer, 0, src.bytes().max(4));
        self.recorded += 1;
        Ok(())
    }

    /// Submits what is recorded and starts a new recording.
    pub(crate) fn submit(&mut self) {
        if self.recorded == 0 {
            return;
        }
        let encoder = std::mem::replace(&mut self.encoder, Batch::encoder(self.gpu));
        self.gpu.queue().submit([encoder.finish()]);
        self.recorded = 0;
    }

    /// Submits what is recorded and waits for the GPU to finish it.
    pub(crate) fn finish(mut self) -> Result<(), GpuError> {
        self.submit();
        self.gpu.wait()
    }
}
