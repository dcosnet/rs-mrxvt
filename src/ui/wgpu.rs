// SPDX-License-Identifier: GPL-2.0-only
//
// rs-mrxvt — a modernized, distro-agnostic mrxvt-inspired terminal emulator.
//
// Copyright (C) 2024 rs-mrxvt contributors
//
// This program is free software; you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation; either version 2 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License along
// with this program; if not, see <https://www.gnu.org/licenses/>.


//! wgpu renderer — full GPU acceleration via Vulkan or GL.
//!
//! Top tier of the backend fallback chain. Renders the terminal grid using:
//! - An instanced quad pipeline (one instance per cell)
//! - A glyph atlas texture (uploaded from [`glyph::GlyphCache`])
//! - WGSL shaders (see [`shaders::SHADER_SOURCE`])
//!
//! ## Status: functional (text rendering works on Vulkan/GL)
//!
//! Compiles only with `--features gpu`. Each frame:
//! 1. Reads the active tab's `alacritty_terminal::Term` grid
//! 2. Builds an instance buffer (one instance per visible cell)
//! 3. Re-uploads the glyph atlas texture if new glyphs were rasterized
//! 4. Draws the instances with the WGSL pipeline
//!
//! ## Build requirements
//!
//! Debian/Ubuntu: `apt install libvulkan-dev libwayland-dev libxkbcommon-dev`
//! Arch: `pacman -S vulkan-headers wayland-protocols libxkbcommon`
//! Fedora: `dnf install vulkan-headers wayland-devel libxkbcommon-devel`

use std::sync::Arc;

use anyhow::Result;
use winit::event_loop::EventLoop;
use winit::window::WindowBuilder;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line as ALine, Point};
use alacritty_terminal::term::cell::{Cell, Flags as CellFlags};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor};

use wgpu::util::DeviceExt;

use crate::app::App;
use crate::ui::backend::BackendFactory;
use crate::ui::glyph::GlyphCache;
use crate::ui::shaders::SHADER_SOURCE;
use crate::ui::Renderer;

const CELL_WIDTH: u32 = 8;
const CELL_HEIGHT: u32 = 16;
const FONT_PIXEL_SIZE: f32 = 14.0;
const ATLAS_SIZE: u32 = 1024;

/// Per-cell instance data. Matches the `VertexInput` struct in the WGSL shader.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct CellInstance {
    position: [f32; 2],
    size: [f32; 2],
    uv_offset: [f32; 2],
    uv_size: [f32; 2],
    bg_color: [f32; 4],
    fg_color: [f32; 4],
    flags: u32,
}

impl CellInstance {
    const ATTRS: [wgpu::VertexAttribute; 7] = wgpu::vertex_attr_array![
        0 => Float32x2,
        1 => Float32x2,
        2 => Float32x2,
        3 => Float32x2,
        4 => Float32x4,
        5 => Float32x4,
        6 => Uint32,
    ];

    fn desc() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<CellInstance>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &Self::ATTRS,
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    resolution: [f32; 2],
    time: f32,
    padding: f32,
}

pub struct WgpuRenderer {
    window: Arc<winit::window::Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    render_pipeline: wgpu::RenderPipeline,
    uniform_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    glyph_cache: GlyphCache,
    glyph_atlas_texture: wgpu::Texture,
    #[allow(dead_code)]
    glyph_atlas_view: wgpu::TextureView,
    #[allow(dead_code)]
    glyph_sampler: wgpu::Sampler,
    /// Tracks how many glyphs are currently in the atlas. If this changes
    /// between frames, we re-upload the texture.
    cached_glyph_count: usize,
    pending_events: Vec<crate::ui::event::AppEvent>,
    start_time: std::time::Instant,
}

impl WgpuRenderer {
    pub fn new() -> Result<Self> {
        let event_loop = EventLoop::<()>::new()
            .map_err(|e| anyhow::anyhow!("creating event loop: {e}"))?;
        let window = Arc::new(
            WindowBuilder::new()
                .with_title("rs-mrxvt")
                .build(&event_loop)
                .map_err(|e| anyhow::anyhow!("creating winit window: {e}"))?,
        );

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN | wgpu::Backends::GL,
            flags: wgpu::InstanceFlags::default(),
            dx12_shader_compiler: wgpu::Dx12Compiler::default(),
            gles_minor_version: wgpu::Gles3MinorVersion::default(),
        });

        let surface = instance
            .create_surface(window.clone())
            .map_err(|e| anyhow::anyhow!("creating wgpu surface: {e}"))?;

        let adapter = pollster::block_on(async {
            instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: Some(&surface),
                    force_fallback_adapter: false,
                })
                .await
        })
        .ok_or_else(|| anyhow::anyhow!("no suitable wgpu adapter found"))?;

        // Log the adapter we're actually using, cross-referencing with
        // the cached GPU probe result from auto_detect().
        {
            let info = adapter.get_info();
            log::info!(
                "WgpuRenderer: using adapter '{}' (backend={:?}, device_type={:?}, vendor=0x{:04X})",
                info.name,
                info.backend,
                info.device_type,
                info.vendor,
            );
            if let Some(probe) = crate::ui::backend::LAST_GPU_PROBE.get() {
                if let (Some(ref probe_name), Some(ref probe_backend)) =
                    (&probe.adapter_name, &probe.backend)
                {
                    if probe_name != &info.name {
                        log::warn!(
                            "WgpuRenderer adapter '{}' differs from probe adapter '{}' — \
                             the probe ran without a surface target",
                            info.name, probe_name,
                        );
                    } else {
                        log::info!(
                            "WgpuRenderer adapter matches probe ({} via {})",
                            probe_name, probe_backend,
                        );
                    }
                }
            }
        }

        let (device, queue) = pollster::block_on(async {
            adapter
                .request_device(
                    &wgpu::DeviceDescriptor {
                        label: Some("rs-mrxvt device"),
                        required_features: wgpu::Features::empty(),
                        required_limits: wgpu::Limits::downlevel_defaults(),
                        memory_hints: wgpu::MemoryHints::Performance,
                    },
                    None,
                )
                .await
        })
        .map_err(|e| anyhow::anyhow!("requesting wgpu device: {e}"))?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        // Create the glyph atlas texture (R8Unorm, ATLAS_SIZE x ATLAS_SIZE).
        let glyph_atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyph atlas"),
            size: wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let glyph_atlas_view = glyph_atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let glyph_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("glyph sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // Uniform buffer.
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniforms"),
            size: std::mem::size_of::<Uniforms>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Bind group.
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
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

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bind group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&glyph_atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&glyph_sampler),
                },
            ],
        });

        // Pipeline layout.
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pipeline layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        // Shader.
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("glyph shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER_SOURCE.into()),
        });

        // Render pipeline.
        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("render pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[CellInstance::desc()],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
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

        let glyph_cache = GlyphCache::new(FONT_PIXEL_SIZE)?;

        Ok(Self {
            window,
            surface,
            device,
            queue,
            config,
            render_pipeline,
            uniform_buffer,
            bind_group,
            glyph_cache,
            glyph_atlas_texture,
            glyph_atlas_view,
            glyph_sampler,
            cached_glyph_count: 0,
            pending_events: Vec::new(),
            start_time: std::time::Instant::now(),
        })
    }

    /// Build the instance buffer for one frame: one CellInstance per visible cell.
    fn build_instances(&mut self, app: &App) -> Vec<CellInstance> {
        let manager = &app.manager;
        let active_idx = manager.active;
        let tab = match manager.tabs.get(active_idx) {
            Some(t) => t,
            None => return Vec::new(),
        };

        let grid = tab.term.grid();
        let cols = (self.config.width / CELL_WIDTH) as usize;
        let rows = (self.config.height / CELL_HEIGHT) as usize;
        let screen_lines = grid.screen_lines();
        let display_offset = grid.display_offset();
        let start_line = -(display_offset as i32);
        let render_rows = rows.min(screen_lines);

        let mut instances = Vec::with_capacity(cols * render_rows);

        for row_idx in 0..render_rows as i32 {
            let line = ALine(start_line + row_idx);
            for col_idx in 0..cols {
                let point = Point { line, column: Column(col_idx) };
                let cell: &Cell = &grid[point];

                let bg = ansi_to_linear(cell.bg);
                let fg = ansi_to_linear(cell.fg);

                let has_glyph = cell.c != ' ' && cell.c != '\0';
                let mut flags = 0u32;
                if has_glyph {
                    flags |= 1;
                }
                if cell.flags.contains(CellFlags::BOLD) {
                    flags |= 2;
                }
                if cell.flags.contains(CellFlags::ITALIC) {
                    flags |= 4;
                }

                // For the atlas UV: we use a simple layout where each glyph
                // occupies a fixed-size slot. This is suboptimal (wastes
                // space) but simple. A future version can pack more tightly.
                let (uv_offset, uv_size) = if has_glyph {
                    // Force-rasterize to ensure the glyph is in the cache.
                    self.glyph_cache.get(cell.c, flags & 2 != 0, flags & 4 != 0);
                    // Each glyph gets a CELL_WIDTH x CELL_HEIGHT slot in the atlas.
                    // We index by the char's Unicode scalar value mod (ATLAS_SIZE / CELL_WIDTH).
                    let slot_w = ATLAS_SIZE / CELL_WIDTH;
                    let slot_h = ATLAS_SIZE / CELL_HEIGHT;
                    let char_idx = cell.c as u32;
                    let sx = (char_idx % slot_w) * CELL_WIDTH;
                    let sy = ((char_idx / slot_w) % slot_h) * CELL_HEIGHT;
                    (
                        [sx as f32 / ATLAS_SIZE as f32, sy as f32 / ATLAS_SIZE as f32],
                        [CELL_WIDTH as f32 / ATLAS_SIZE as f32, CELL_HEIGHT as f32 / ATLAS_SIZE as f32],
                    )
                } else {
                    ([0.0, 0.0], [0.0, 0.0])
                };

                instances.push(CellInstance {
                    position: [(col_idx as u32 * CELL_WIDTH) as f32, (row_idx as u32 * CELL_HEIGHT) as f32],
                    size: [CELL_WIDTH as f32, CELL_HEIGHT as f32],
                    uv_offset,
                    uv_size,
                    bg_color: bg,
                    fg_color: fg,
                    flags,
                });
            }
        }

        instances
    }

    /// If new glyphs have been rasterized since the last upload, re-upload
    /// the atlas texture. This is O(glyph_count) per upload; we only upload
    /// when the count changed.
    fn maybe_upload_atlas(&mut self) {
        let count = self.glyph_cache.len();
        if count == self.cached_glyph_count {
            return;
        }
        self.cached_glyph_count = count;

        // Build the atlas as a single R8 buffer. For simplicity, we use the
        // fixed-slot layout: glyph for char C lives at
        // (C % slot_w) * CELL_WIDTH, (C / slot_w) % slot_h) * CELL_HEIGHT.
        let mut atlas = vec![0u8; (ATLAS_SIZE * ATLAS_SIZE) as usize];

        // We don't have direct access to the cache's internals here, so we
        // re-rasterize every glyph into the atlas. This is wasteful but
        // correct; a future version will expose an iterator over the cache.
        // For the MVP, this only runs when the cache size changes (rare).
        for codepoint in 0u32..0x80 {
            // Only ASCII for the MVP; full Unicode would iterate the cache.
            let c = char::from_u32(codepoint).unwrap_or('?');
            if c == ' ' || c == '\0' {
                continue;
            }
            let g = self.glyph_cache.get(c, false, false);
            let slot_w = ATLAS_SIZE / CELL_WIDTH;
            let sx = (codepoint % slot_w) * CELL_WIDTH;
            let sy = ((codepoint / slot_w) % (ATLAS_SIZE / CELL_HEIGHT)) * CELL_HEIGHT;

            for gy in 0..g.height {
                for gx in 0..g.width {
                    let px = sx as usize + gx;
                    let py = sy as usize + gy;
                    if px < ATLAS_SIZE as usize && py < ATLAS_SIZE as usize {
                        let gidx = (gy * g.width + gx) * 4 + 3; // alpha channel
                        let alpha = g.pixels[gidx];
                        let aidx = py * ATLAS_SIZE as usize + px;
                        atlas[aidx] = alpha;
                    }
                }
            }
        }

        self.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.glyph_atlas_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &atlas,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(ATLAS_SIZE),
                rows_per_image: Some(ATLAS_SIZE),
            },
            wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
        );
    }
}

impl Renderer for WgpuRenderer {
    fn init(&mut self) -> Result<()> {
        Ok(())
    }

    fn fini(&mut self) -> Result<()> {
        Ok(())
    }

    fn poll_event(&mut self, _timeout_ms: u64) -> Result<Option<crate::ui::event::AppEvent>> {
        // winit event loop integration is deferred (see soft.rs for the same
        // caveat). For now we return None; the user can close the window
        // via the WM.
        if self.pending_events.is_empty() {
            Ok(None)
        } else {
            Ok(self.pending_events.drain(..).next())
        }
    }

    fn render(&mut self, app: &mut App) -> Result<()> {
        // Resize surface if the window changed.
        let size = self.window.inner_size();
        if size.width > 0 && size.height > 0
            && (size.width != self.config.width || size.height != self.config.height)
        {
            self.config.width = size.width;
            self.config.height = size.height;
            self.surface.configure(&self.device, &self.config);
        }

        // Update uniforms.
        let elapsed = self.start_time.elapsed().as_secs_f32();
        let uniforms = Uniforms {
            resolution: [self.config.width as f32, self.config.height as f32],
            time: elapsed,
            padding: 0.0,
        };
        self.queue.write_buffer(&self.uniform_buffer, 0, bytemuck::cast_slice(&[uniforms]));

        // Build instances for this frame.
        let instances = self.build_instances(app);

        // Maybe upload the atlas (only if new glyphs were rasterized).
        self.maybe_upload_atlas();

        // Create the instance buffer.
        let instance_buffer = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("instance buffer"),
            contents: bytemuck::cast_slice(&instances),
            usage: wgpu::BufferUsages::VERTEX,
        });

        // Get the surface texture.
        let output = match self.surface.get_current_texture() {
            Ok(t) => t,
            Err(e) => {
                log::debug!("get_current_texture failed, skipping frame: {e}");
                return Ok(());
            }
        };
        let view = output.texture.create_view(&wgpu::TextureViewDescriptor::default());

        // Encode + submit.
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("frame encoder"),
        });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("frame pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.02,
                            g: 0.04,
                            b: 0.02,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            pass.set_pipeline(&self.render_pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, instance_buffer.slice(..));
            // 4 vertices per quad (we generate them in the vertex shader),
            // `instances.len()` instances.
            pass.draw(0..4, 0..instances.len() as u32);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        output.present();

        Ok(())
    }

    fn size(&self) -> (u16, u16) {
        let cols = (self.config.width / CELL_WIDTH).max(2) as u16;
        let rows = (self.config.height / CELL_HEIGHT).saturating_sub(2).max(1) as u16;
        (cols, rows)
    }
}

/// Factory for the wgpu renderer.
pub struct WgpuFactory;

impl BackendFactory for WgpuFactory {
    fn available(&self) -> bool {
        super::backend::probe_wgpu_available_pub()
    }
    fn create(&self) -> Result<Box<dyn Renderer>> {
        Ok(Box::new(WgpuRenderer::new()?))
    }
}

// ─── Color conversion ────────────────────────────────────────────────────────

fn ansi_to_linear(c: AnsiColor) -> [f32; 4] {
    let (r, g, b, a) = ansi_to_rgba_u8(c);
    [
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        a as f32 / 255.0,
    ]
}

fn ansi_to_rgba_u8(c: AnsiColor) -> (u8, u8, u8, u8) {
    match c {
        AnsiColor::Named(n) => named_to_rgba(n),
        AnsiColor::Spec(rgb) => (rgb.r, rgb.g, rgb.b, 255),
        AnsiColor::Indexed(i) => match i {
            0 => (0, 0, 0, 255),
            1 => (205, 0, 0, 255),
            2 => (0, 205, 0, 255),
            3 => (205, 205, 0, 255),
            4 => (0, 0, 238, 255),
            5 => (205, 0, 205, 255),
            6 => (0, 205, 205, 255),
            7 => (229, 229, 229, 255),
            8 => (127, 127, 127, 255),
            9 => (255, 0, 0, 255),
            10 => (0, 255, 0, 255),
            11 => (255, 255, 0, 255),
            12 => (92, 92, 255, 255),
            13 => (255, 0, 255, 255),
            14 => (0, 255, 255, 255),
            15 => (255, 255, 255, 255),
            _ => (200, 200, 200, 255),
        },
    }
}

fn named_to_rgba(n: NamedColor) -> (u8, u8, u8, u8) {
    use NamedColor::*;
    match n {
        Black => (0, 0, 0, 255),
        Red => (205, 0, 0, 255),
        Green => (0, 205, 0, 255),
        Yellow => (205, 205, 0, 255),
        Blue => (0, 0, 238, 255),
        Magenta => (205, 0, 205, 255),
        Cyan => (0, 205, 205, 255),
        White => (229, 229, 229, 255),
        BrightBlack => (127, 127, 127, 255),
        BrightRed => (255, 0, 0, 255),
        BrightGreen => (0, 255, 0, 255),
        BrightYellow => (255, 255, 0, 255),
        BrightBlue => (92, 92, 255, 255),
        BrightMagenta => (255, 0, 255, 255),
        BrightCyan => (0, 255, 255, 255),
        BrightWhite => (255, 255, 255, 255),
        BrightForeground => (255, 255, 255, 255),
        Foreground => (229, 229, 229, 255),
        Background => (5, 10, 5, 255),
        Cursor => (255, 255, 255, 255),
        _ => (200, 200, 200, 255),
    }
}
