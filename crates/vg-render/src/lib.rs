//! GPU compositor: draws a frame of the avatar track into an offscreen canvas.
//!
//! The canvas is premultiplied RGBA with gamma-encoded values (like egui's own
//! textures), so the GUI shows it directly; the exporter runs an un-premultiply pass and reads straight-alpha RGBA back.

use anyhow::{Context, Result, bail};
use std::path::Path;
use vg_core::{PoseSet, Preset, Timeline, avatar_at, sprite_quad};

const CANVAS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// A device + queue for headless use (CLI export).
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl Gpu {
    pub fn headless() -> Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .context("no GPU adapter found")?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("videogen"),
            ..Default::default()
        }))
        .context("failed to open the GPU device")?;
        Ok(Gpu { device, queue })
    }
}

/// Everything needed to draw one frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameDesc {
    /// Pose index and its canvas-pixel corners (tl, tr, bl, br).
    pub avatar: Option<(usize, [[f32; 2]; 4])>,
}

/// The frame at time `t` for this timeline and preset.
pub fn frame_at(tl: &Timeline, preset: &Preset, poses: &PoseSet, t: f64) -> FrameDesc {
    let avatar = avatar_at(tl, &preset.motion, t).and_then(|st| {
        let pose = poses.poses.get(st.pose)?;
        Some((st.pose, sprite_quad(&preset.avatar, (preset.width, preset.height), pose, &st)))
    });
    FrameDesc { avatar }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SpriteUniform {
    corners: [[f32; 4]; 4],
    canvas: [f32; 4],
}

struct PoseTexture {
    bind_group: wgpu::BindGroup,
}

pub struct Compositor {
    device: wgpu::Device,
    queue: wgpu::Queue,
    width: u32,
    height: u32,
    sprite_pipeline: wgpu::RenderPipeline,
    sprite_bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform: wgpu::Buffer,
    poses: Vec<PoseTexture>,
    unpremul_pipeline: wgpu::RenderPipeline,
    unpremul_bgl: wgpu::BindGroupLayout,
    targets: Targets,
}

/// Size-dependent resources.
struct Targets {
    /// Held with its view; only the view is used.
    _canvas: wgpu::Texture,
    canvas_view: wgpu::TextureView,
    output_view: wgpu::TextureView,
    output: wgpu::Texture,
    unpremul_bg: wgpu::BindGroup,
    readback: wgpu::Buffer,
    padded_bpr: u32,
}

impl Compositor {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, width: u32, height: u32) -> Self {
        let sprite_mod = device.create_shader_module(wgpu::include_wgsl!("sprite.wgsl"));
        let unpremul_mod = device.create_shader_module(wgpu::include_wgsl!("unpremultiply.wgsl"));

        let sprite_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sprite bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let unpremul_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("unpremultiply bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let pipeline = |label: &str, bgl: &wgpu::BindGroupLayout, module: &wgpu::ShaderModule, vs: &str, fs: &str, topology, blend| {
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &[Some(bgl)],
                immediate_size: 0,
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState { module, entry_point: Some(vs), buffers: &[], compilation_options: Default::default() },
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some(fs),
                    targets: &[Some(wgpu::ColorTargetState { format: CANVAS_FORMAT, blend, write_mask: wgpu::ColorWrites::ALL })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState { topology, ..Default::default() },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let sprite_pipeline = pipeline(
            "sprite",
            &sprite_bgl,
            &sprite_mod,
            "vs_sprite",
            "fs_sprite",
            wgpu::PrimitiveTopology::TriangleStrip,
            Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
        );
        let unpremul_pipeline = pipeline(
            "unpremultiply",
            &unpremul_bgl,
            &unpremul_mod,
            "vs_full",
            "fs_unpremultiply",
            wgpu::PrimitiveTopology::TriangleList,
            None,
        );
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("pose sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 4,
            ..Default::default()
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sprite uniform"),
            size: std::mem::size_of::<SpriteUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let targets = Targets::new(device, &unpremul_bgl, width, height);
        Compositor {
            device: device.clone(),
            queue: queue.clone(),
            width,
            height,
            sprite_pipeline,
            sprite_bgl,
            sampler,
            uniform,
            poses: Vec::new(),
            unpremul_pipeline,
            unpremul_bgl,
            targets,
        }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if (width, height) != (self.width, self.height) {
            self.width = width;
            self.height = height;
            self.targets = Targets::new(&self.device, &self.unpremul_bgl, width, height);
        }
    }

    /// (Re)load every pose image onto the GPU. Decoding runs on all cores.
    pub fn load_poses(&mut self, poses: &PoseSet) -> Result<()> {
        let paths: Vec<_> = (0..poses.poses.len()).map(|i| poses.path_of(i)).collect();
        let images: Vec<Result<Vec<image::RgbaImage>>> = std::thread::scope(|s| {
            let handles: Vec<_> = paths.iter().map(|p| s.spawn(move || load_mip_chain(p))).collect();
            handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err(anyhow::anyhow!("image loader panicked")))).collect()
        });
        let mut loaded = Vec::with_capacity(images.len());
        for (mips, path) in images.into_iter().zip(&paths) {
            let mips = mips.with_context(|| format!("loading pose image {}", path.display()))?;
            loaded.push(self.upload(&mips));
        }
        self.poses = loaded;
        Ok(())
    }

    fn upload(&self, mips: &[image::RgbaImage]) -> PoseTexture {
        let (w, h) = mips[0].dimensions();
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pose"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: mips.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: CANVAS_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (level, img) in mips.iter().enumerate() {
            let (lw, lh) = img.dimensions();
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: level as u32, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                img.as_raw(),
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * lw), rows_per_image: Some(lh) },
                wgpu::Extent3d { width: lw, height: lh, depth_or_array_layers: 1 },
            );
        }
        let view = texture.create_view(&Default::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pose bg"),
            layout: &self.sprite_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        });
        PoseTexture { bind_group }
    }

    /// Render `frame` into the canvas.
    pub fn draw(&self, frame: &FrameDesc) {
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("canvas"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.targets.canvas_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let Some((pose, quad)) = frame.avatar
                && let Some(tex) = self.poses.get(pose) {
                    let uniform = SpriteUniform {
                        corners: quad.map(|[x, y]| [x, y, 0.0, 0.0]),
                        canvas: [self.width as f32, self.height as f32, 0.0, 0.0],
                    };
                    self.queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&uniform));
                    pass.set_pipeline(&self.sprite_pipeline);
                    pass.set_bind_group(0, &tex.bind_group, &[]);
                    pass.draw(0..4, 0..1);
                }
        }
        self.queue.submit(Some(enc.finish()));
    }

    /// The canvas view, for display in egui (premultiplied, gamma-encoded like egui's textures).
    pub fn canvas_view(&self) -> &wgpu::TextureView {
        &self.targets.canvas_view
    }

    /// Un-premultiply the canvas and read it back as tightly packed straight-alpha RGBA.
    pub fn read_rgba(&self, out: &mut Vec<u8>) -> Result<()> {
        let t = &self.targets;
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("readback") });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("unpremultiply"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &t.output_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.unpremul_pipeline);
            pass.set_bind_group(0, &t.unpremul_bg, &[]);
            pass.draw(0..3, 0..1);
        }
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &t.output, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo {
                buffer: &t.readback,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(t.padded_bpr), rows_per_image: Some(self.height) },
            },
            wgpu::Extent3d { width: self.width, height: self.height, depth_or_array_layers: 1 },
        );
        self.queue.submit(Some(enc.finish()));
        let (tx, rx) = std::sync::mpsc::channel();
        t.readback.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r.is_ok());
        });
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        if !rx.recv().unwrap_or(false) {
            bail!("GPU read-back failed");
        }
        let row = (4 * self.width) as usize;
        out.clear();
        out.reserve(row * self.height as usize);
        {
            let data = t.readback.slice(..).get_mapped_range();
            for y in 0..self.height as usize {
                let off = y * t.padded_bpr as usize;
                out.extend_from_slice(&data[off..off + row]);
            }
        }
        t.readback.unmap();
        Ok(())
    }
}

impl Targets {
    fn new(device: &wgpu::Device, unpremul_bgl: &wgpu::BindGroupLayout, width: u32, height: u32) -> Self {
        let size = wgpu::Extent3d { width: width.max(1), height: height.max(1), depth_or_array_layers: 1 };
        let canvas = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("canvas"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: CANVAS_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let canvas_view = canvas.create_view(&Default::default());
        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("output"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: CANVAS_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let output_view = output.create_view(&Default::default());
        let unpremul_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("unpremultiply bg"),
            layout: unpremul_bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&canvas_view) }],
        });
        let padded_bpr = (4 * size.width).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (padded_bpr * size.height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Targets { _canvas: canvas, canvas_view, output, output_view, unpremul_bg, readback, padded_bpr }
    }
}

/// Decode, premultiply, and build a full mip chain (so small presets and GUI zoom stay smooth).
fn load_mip_chain(path: &Path) -> Result<Vec<image::RgbaImage>> {
    let mut img = image::open(path)?.to_rgba8();
    for px in img.pixels_mut() {
        let a = px[3] as u32;
        for c in 0..3 {
            px[c] = ((px[c] as u32 * a + 127) / 255) as u8;
        }
    }
    let mut mips = vec![img];
    loop {
        let (w, h) = mips.last().unwrap().dimensions();
        if w == 1 && h == 1 {
            break;
        }
        let next = image::imageops::resize(mips.last().unwrap(), (w / 2).max(1), (h / 2).max(1), image::imageops::FilterType::Triangle);
        mips.push(next);
    }
    Ok(mips)
}
