//! A custom iced `shader` widget that draws a video frame from a persistent
//! wgpu texture.
//!
//! Why not the `image` widget: `image::Handle::from_rgba` mints a new handle id
//! every frame, so iced's image cache inserts+evicts a fresh texture each redraw
//! — that thrash renders black/flickers. Here each player owns ONE texture that
//! we `write_texture` into when a new frame arrives (keyed by player id in the
//! shared `VideoPipeline`). Frames are letterboxed (Contain) via a scale uniform.
//!
//! Ported from ~/code/finn's grid renderer, keyed per-player instead of per-cell.

use std::collections::HashMap;
use std::sync::Arc;

use iced::advanced::graphics::Viewport;
use iced::widget::shader::{self, Primitive};
use iced::{mouse, Rectangle};

use super::decoder::RgbaFrame;

/// The per-player program handed to the `shader` widget each `view()`.
pub struct VideoProgram {
    pub id: u64,
    pub version: u64,
    pub frame: Option<Arc<RgbaFrame>>,
}

impl<Message> shader::Program<Message> for VideoProgram {
    type State = ();
    type Primitive = VideoPrimitive;

    fn draw(&self, _state: &(), _cursor: mouse::Cursor, _bounds: Rectangle) -> VideoPrimitive {
        VideoPrimitive {
            id: self.id,
            version: self.version,
            frame: self.frame.clone(),
        }
    }
}

#[derive(Clone)]
pub struct VideoPrimitive {
    id: u64,
    version: u64,
    frame: Option<Arc<RgbaFrame>>,
}

impl std::fmt::Debug for VideoPrimitive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoPrimitive")
            .field("id", &self.id)
            .field("version", &self.version)
            .field("has_frame", &self.frame.is_some())
            .finish()
    }
}

impl Primitive for VideoPrimitive {
    type Pipeline = VideoPipeline;

    fn prepare(
        &self,
        pipeline: &mut VideoPipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        viewport: &Viewport,
    ) {
        let Some(frame) = &self.frame else {
            return;
        };
        // The widget's full bounds in physical pixels. `render` only receives
        // `clip_bounds` (the visible intersection), so we carry the full bounds
        // across here — the render-pass viewport maps the quad to them while the
        // scissor (clip_bounds) merely trims the scrolled-off part. Sizing the
        // viewport to clip_bounds instead would squish the frame into the strip.
        let phys = *bounds * viewport.scale_factor();
        pipeline.prepare(device, queue, self.id, self.version, frame, phys);
    }

    fn render(
        &self,
        pipeline: &VideoPipeline,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        clip_bounds: &Rectangle<u32>,
    ) {
        pipeline.render(self.id, encoder, target, clip_bounds);
    }
}

/// Shared GPU state for ALL video widgets: one render pipeline + sampler, plus a
/// persistent per-player target (texture/bind group/uniform) keyed by id.
#[derive(Debug)]
pub struct VideoPipeline {
    pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    layout: wgpu::BindGroupLayout,
    /// Format for uploaded frame textures, matched to iced's render target.
    /// sws gives us sRGB-encoded bytes; we want them to pass straight through.
    /// For an sRGB target, an sRGB texture (sample decodes, target re-encodes)
    /// is an identity round-trip; for a non-sRGB / 10-bit target a plain `Unorm`
    /// texture passes the bytes through. Mismatching tints the output (reddish).
    tex_format: wgpu::TextureFormat,
    targets: HashMap<u64, Target>,
}

#[derive(Debug)]
struct Target {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    width: u32,
    height: u32,
    version: u64,
    /// Full widget bounds in physical pixels (render-pass viewport).
    bounds: Rectangle,
}

impl shader::Pipeline for VideoPipeline {
    fn new(device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("video shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("video bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
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

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("video pl"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("video pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("video sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // Match the target's color space (see `tex_format` docs).
        let tex_format = if format.is_srgb() {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };

        VideoPipeline {
            pipeline,
            sampler,
            layout,
            tex_format,
            targets: HashMap::new(),
        }
    }
}

impl VideoPipeline {
    fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        id: u64,
        version: u64,
        frame: &RgbaFrame,
        bounds: Rectangle,
    ) {
        let needs_texture = match self.targets.get(&id) {
            Some(t) => t.width != frame.width || t.height != frame.height,
            None => true,
        };

        if needs_texture {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("video tex"),
                size: wgpu::Extent3d {
                    width: frame.width,
                    height: frame.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.tex_format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("video uniform"),
                size: 16,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("video bg"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            self.targets.insert(
                id,
                Target {
                    texture,
                    bind_group,
                    uniform,
                    width: frame.width,
                    height: frame.height,
                    version: u64::MAX, // force upload below
                    bounds,
                },
            );
        }

        let target = self.targets.get_mut(&id).unwrap();
        target.bounds = bounds;

        if target.version != version {
            target.version = version;
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &target.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &frame.data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(frame.width * 4),
                    rows_per_image: Some(frame.height),
                },
                wgpu::Extent3d {
                    width: frame.width,
                    height: frame.height,
                    depth_or_array_layers: 1,
                },
            );
        }

        // letterbox (Contain): shrink the NDC quad on the longer axis
        let cell_w = bounds.width.max(1.0);
        let cell_h = bounds.height.max(1.0);
        let cell_aspect = cell_w / cell_h;
        let tex_aspect = frame.width as f32 / frame.height as f32;
        let (sx, sy) = if tex_aspect > cell_aspect {
            (1.0, cell_aspect / tex_aspect)
        } else {
            (tex_aspect / cell_aspect, 1.0)
        };
        queue.write_buffer(
            &target.uniform,
            0,
            bytemuck::cast_slice(&[sx, sy, 0.0f32, 0.0f32]),
        );
    }

    fn render(
        &self,
        id: u64,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        clip_bounds: &Rectangle<u32>,
    ) {
        let Some(t) = self.targets.get(&id) else {
            return;
        };

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("video pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load, // preserve what other widgets drew
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        // Viewport = full widget bounds (the quad is laid out against these);
        // scissor = the visible clip region. Sizing the viewport to clip_bounds
        // would squeeze the whole frame into the visible strip as it scrolls off.
        pass.set_viewport(
            t.bounds.x,
            t.bounds.y,
            t.bounds.width,
            t.bounds.height,
            0.0,
            1.0,
        );
        pass.set_scissor_rect(
            clip_bounds.x,
            clip_bounds.y,
            clip_bounds.width,
            clip_bounds.height,
        );
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &t.bind_group, &[]);
        pass.draw(0..6, 0..1);
    }
}

const SHADER: &str = r#"
struct Uniforms { scale: vec2<f32>, _pad: vec2<f32> };
@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(-1.0, 1.0),
        vec2<f32>(-1.0, 1.0),  vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
    );
    let c = corners[vi];
    var out: VsOut;
    out.pos = vec4<f32>(c * u.scale, 0.0, 1.0);
    out.uv = vec2<f32>(c.x * 0.5 + 0.5, 1.0 - (c.y * 0.5 + 0.5));
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(tex, samp, in.uv);
}
"#;
