//! wgpu device setup and a minimal kernel launcher.
//!
//! Every kernel is a WGSL compute shader with a common uniform block (`Params`, bound at
//! binding 0) followed by its storage buffers in declaration order. Bind group layouts are
//! derived from the shaders, so adding a kernel only needs its WGSL source.

use std::collections::HashMap;
use std::sync::Mutex;

use wgpu::util::DeviceExt;

/// Uniform parameters shared by all kernels: sizes, integer and float arguments.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct Params {
    pub w: u32,
    pub h: u32,
    pub sw: u32,
    pub sh: u32,
    pub i: [u32; 4],
    pub f: [f32; 8],
}

/// WGSL declaration of [`Params`], prepended to every shader.
const PARAMS_WGSL: &str = "
struct Params { w: u32, h: u32, sw: u32, sh: u32, i0: u32, i1: u32, i2: u32, i3: u32,
                f0: f32, f1: f32, f2: f32, f3: f32, f4: f32, f5: f32, f6: f32, f7: f32 }
@group(0) @binding(0) var<uniform> P: Params;
";

const WORKGROUP: u32 = 16;

pub(crate) struct GpuContext {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub adapter_name: String,
    /// Largest storage buffer the device accepts, in bytes.
    pub max_buffer: u64,
    pipelines: Mutex<HashMap<&'static str, wgpu::ComputePipeline>>,
    /// Released storage buffers by size, for reuse. Fresh wgpu buffers are zero-filled
    /// on first use, which costs as much as a kernel pass for full-size images.
    pool: Mutex<HashMap<u64, Vec<wgpu::Buffer>>>,
}

impl GpuContext {
    /// Opens the default high-performance GPU, or `None` if there is no usable adapter.
    pub fn new() -> Option<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
            ..Default::default()
        }))
        .ok()?;
        let info = adapter.get_info();
        if info.device_type == wgpu::DeviceType::Cpu {
            return None;
        }
        let limits = adapter.limits();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("shroom"),
            required_features: wgpu::Features::empty(),
            required_limits: limits.clone(),
            experimental_features: Default::default(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: Default::default(),
        }))
        .ok()?;
        let max_buffer = (limits.max_storage_buffer_binding_size as u64).min(limits.max_buffer_size);
        Some(Self {
            device,
            queue,
            adapter_name: format!("{} ({:?})", info.name, info.backend),
            max_buffer,
            pipelines: Mutex::new(HashMap::new()),
            pool: Mutex::new(HashMap::new()),
        })
    }

    fn pipeline(&self, name: &'static str, source: &str) -> wgpu::ComputePipeline {
        let mut cache = self.pipelines.lock().unwrap();
        cache
            .entry(name)
            .or_insert_with(|| {
                let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some(name),
                    source: wgpu::ShaderSource::Wgsl(format!("{PARAMS_WGSL}\n{source}").into()),
                });
                self.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(name),
                    layout: None,
                    module: &module,
                    entry_point: Some("main"),
                    compilation_options: Default::default(),
                    cache: None,
                })
            })
            .clone()
    }

    /// Runs kernel `name` over a `w × h` grid (one invocation per pixel).
    pub fn run(&self, name: &'static str, source: &str, params: Params, buffers: &[&wgpu::Buffer], w: usize, h: usize) {
        let pipeline = self.pipeline(name, source);
        let uniform = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let mut entries = vec![wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() }];
        entries.extend(
            buffers
                .iter()
                .enumerate()
                .map(|(i, b)| wgpu::BindGroupEntry { binding: i as u32 + 1, resource: b.as_entire_binding() }),
        );
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(name),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &entries,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups((w as u32).div_ceil(WORKGROUP), (h as u32).div_ceil(WORKGROUP), 1);
        }
        self.queue.submit([encoder.finish()]);
        // Non-blocking: reclaims memory of buffers whose work has completed.
        let _ = self.device.poll(wgpu::PollType::Poll);
    }

    /// A storage buffer for `len` floats with undefined contents (possibly reused).
    pub fn storage(&self, len: usize) -> wgpu::Buffer {
        let size = (len.max(1) * 4) as u64;
        if let Some(buffer) = self.pool.lock().unwrap().get_mut(&size).and_then(Vec::pop) {
            return buffer;
        }
        self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (len.max(1) * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// A storage buffer filled with zeros.
    pub fn zeroed(&self, len: usize) -> wgpu::Buffer {
        let buffer = self.storage(len);
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.clear_buffer(&buffer, 0, None);
        self.queue.submit([encoder.finish()]);
        buffer
    }

    /// Returns a buffer to the pool. Queue order guarantees that work submitted later
    /// (which may reuse it) runs after all work already using it.
    pub fn release(&self, buffer: wgpu::Buffer) {
        self.pool.lock().unwrap().entry(buffer.size()).or_default().push(buffer);
    }

    /// Frees all pooled buffers.
    pub fn trim(&self) {
        self.pool.lock().unwrap().clear();
    }

    pub fn upload(&self, data: &[f32]) -> wgpu::Buffer {
        let buffer = self.storage(data.len());
        self.queue.write_buffer(&buffer, 0, bytemuck::cast_slice(data));
        buffer
    }

    pub fn download(&self, buffer: &wgpu::Buffer, len: usize) -> Vec<f32> {
        let size = (len * 4) as u64;
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("staging"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, size);
        self.queue.submit([encoder.finish()]);
        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |r| r.expect("GPU buffer mapping failed"));
        self.device.poll(wgpu::PollType::wait_indefinitely()).expect("GPU poll failed");
        let view = slice.get_mapped_range().expect("GPU buffer not mapped");
        let data = bytemuck::cast_slice(&view).to_vec();
        drop(view);
        staging.unmap();
        data
    }
}
