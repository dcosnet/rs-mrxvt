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
use winit::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
use winit::window::WindowBuilder;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line as ALine, Point};
use alacritty_terminal::term::cell::{Cell, Flags as CellFlags};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor};

use wgpu::util::DeviceExt;

use crate::app::App;
use crate::terminal::manager::BroadcastTarget;
use crate::ui::backend::BackendFactory;
use crate::ui::glyph::GlyphCache;
use crate::ui::shaders::SHADER_SOURCE;
use crate::ui::Renderer;

const CELL_WIDTH: u32 = 8;
const CELL_HEIGHT: u32 = 16;
const FONT_PIXEL_SIZE: f32 = 14.0;
const ATLAS_SIZE: u32 = 1024;
/// Padding between glyphs in the atlas (pixels). Prevents linear-filter
/// sampling from bleeding into neighbouring glyphs at the edges.
const ATLAS_PADDING: u32 = 1;

/// Height of the tab bar in pixels (one cell row).
const TAB_BAR_HEIGHT: u32 = CELL_HEIGHT;
/// Height of the status bar in pixels (one cell row). Documented here for
/// layout clarity; the status bar Y is computed from `total_rows` at render
/// time, but this constant keeps the chrome-height intent explicit.
#[allow(dead_code)]
const STATUS_BAR_HEIGHT: u32 = CELL_HEIGHT;

// ─── Modern color palette (linear RGB, 0..1) ─────────────────────────────
//
// A clean dark theme with a cyan accent — modern without being garish.
const COLOR_BG: [f32; 4] = [0.05, 0.05, 0.07, 1.0];          // window background
const COLOR_TABBAR_BG: [f32; 4] = [0.10, 0.10, 0.13, 1.0];   // tab bar bg
const COLOR_TAB_INACTIVE_BG: [f32; 4] = [0.12, 0.12, 0.16, 1.0];
const COLOR_TAB_INACTIVE_FG: [f32; 4] = [0.55, 0.55, 0.62, 1.0];
const COLOR_TAB_ACTIVE_BG: [f32; 4] = [0.15, 0.20, 0.28, 1.0]; // highlighted
#[allow(dead_code)]
const COLOR_TAB_ACTIVE_FG: [f32; 4] = [0.95, 0.95, 1.0, 1.0];
const COLOR_TAB_ACCENT: [f32; 4] = [0.20, 0.65, 0.85, 1.0];  // cyan accent (active tab indicator)
#[allow(dead_code)]
const COLOR_TAB_SEPARATOR: [f32; 4] = [0.20, 0.20, 0.25, 1.0];
const COLOR_STATUS_BG: [f32; 4] = [0.08, 0.08, 0.11, 1.0];
const COLOR_STATUS_FG: [f32; 4] = [0.60, 0.60, 0.67, 1.0];
const COLOR_BROADCAST_ALL: [f32; 4] = [0.90, 0.25, 0.25, 1.0]; // red
const COLOR_BROADCAST_GROUP: [f32; 4] = [0.85, 0.40, 0.85, 1.0]; // magenta

/// Per-cell instance data. Matches the `VertexInput` struct in the WGSL shader.
///
/// The quad covers the full cell (for the background fill). The glyph is
/// positioned within the cell via `glyph_offset` and `glyph_size` so it
/// renders at its natural aspect ratio instead of being stretched to fill
/// the cell. The atlas UV rect (`uv_offset`, `uv_size`) maps to the glyph's
/// actual pixels in the atlas.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct CellInstance {
    position: [f32; 2],
    size: [f32; 2],
    /// Top-left of the glyph rect within the cell, in pixels relative to
    /// the cell's top-left. Used to position the glyph at the right baseline.
    glyph_offset: [f32; 2],
    /// Size of the glyph rect in pixels. The shader uses this to determine
    /// whether a fragment falls inside the glyph.
    glyph_size: [f32; 2],
    uv_offset: [f32; 2],
    uv_size: [f32; 2],
    bg_color: [f32; 4],
    fg_color: [f32; 4],
    flags: u32,
}

impl CellInstance {
    const ATTRS: [wgpu::VertexAttribute; 9] = wgpu::vertex_attr_array![
        0 => Float32x2,  // position
        1 => Float32x2,  // size
        2 => Float32x2,  // glyph_offset
        3 => Float32x2,  // glyph_size
        4 => Float32x2,  // uv_offset
        5 => Float32x2,  // uv_size
        6 => Float32x4,  // bg_color
        7 => Float32x4,  // fg_color
        8 => Uint32,     // flags
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
    /// The winit event loop. Must be kept alive and pumped every tick —
    /// without pumping, the window never receives configure/expose events
    /// and appears as a blank form. winit's `pump_events` lets us drive it
    /// from our own main loop instead of ceding control to `EventLoop::run`.
    event_loop: EventLoop<()>,
    /// Tracks keyboard modifier state (updated by ModifiersChanged events).
    mod_tracker: crate::ui::event::winit_translate::ModifierTracker,
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
    /// Maps each cached glyph (char, bold, italic) to its pixel rect in the
    /// atlas. Built lazily by `rebuild_atlas` when new glyphs appear.
    /// Keyed the same way as `GlyphCache`'s internal map.
    atlas_map: std::collections::HashMap<(char, bool, bool), AtlasRect>,
    pending_events: Vec<crate::ui::event::AppEvent>,
    start_time: std::time::Instant,
}

/// A glyph's placement in the atlas texture (pixel coordinates).
#[derive(Copy, Clone, Debug)]
struct AtlasRect {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

impl WgpuRenderer {
    pub fn new() -> Result<Self> {
        let event_loop = EventLoop::<()>::new()
            .map_err(|e| anyhow::anyhow!("creating event loop: {e}"))?;
        let window = Arc::new(
            WindowBuilder::new()
                .with_title("rs-mrxvt")
                // Give the window a real initial size. Without this, the WM
                // may pick a tiny default (sometimes 1×1 or just title-bar-
                // tall), which leaves the surface too small to render any
                // cells and the user sees a blank form. 1024×768 is a
                // conservative default that every compositor will honour.
                .with_inner_size(winit::dpi::LogicalSize::<u32>::new(1024, 768))
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
            primitive: wgpu::PrimitiveState {
                // TriangleStrip: 4 vertices → 2 triangles forming one quad.
                // The default (TriangleList) would need 6 vertices for a quad
                // and would drop the 4th vertex, leaving half of every glyph
                // unrendered. The vertex shader's corner mapping
                // (vid 0..3 → TL,TR,BL,BR) is designed for strip order.
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let glyph_cache = GlyphCache::new(FONT_PIXEL_SIZE)?;

        Ok(Self {
            window,
            event_loop,
            mod_tracker: Default::default(),
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
            atlas_map: std::collections::HashMap::new(),
            pending_events: Vec::new(),
            start_time: std::time::Instant::now(),
        })
    }

    /// Build the full instance buffer for one frame: tab bar + terminal grid
    /// + status bar.
    ///
    /// All three are rendered with the same instanced quad pipeline — each
    /// character is just another cell instance.
    fn build_instances(&mut self, app: &App) -> Vec<CellInstance> {
        let manager = &app.manager;
        let active_idx = manager.active;

        let cols = (self.config.width / CELL_WIDTH) as usize;
        let total_rows = (self.config.height / CELL_HEIGHT) as usize;
        // Terminal area = total minus tab bar (top) and status bar (bottom).
        let term_rows = total_rows.saturating_sub(2);

        let mut instances = Vec::with_capacity(cols * total_rows);

        // ── Tab bar (row 0) ──────────────────────────────────────────────
        instances.extend(self.build_tab_bar_instances(app, cols));

        // ── Terminal grid (rows 1..=term_rows) ───────────────────────────
        if let Some(tab) = manager.tabs.get(active_idx) {
            let grid = tab.term.grid();
            let screen_lines = grid.screen_lines();
            let display_offset = grid.display_offset();
            let start_line = -(display_offset as i32);
            let render_rows = term_rows.min(screen_lines);

            for row_idx in 0..render_rows as i32 {
                let line = ALine(start_line + row_idx);
                for col_idx in 0..cols {
                    let point = Point { line, column: Column(col_idx) };
                    let cell: &Cell = &grid[point];

                    let bg = ansi_to_linear(cell.bg);
                    let fg = ansi_to_linear(cell.fg);

                    let bold = cell.flags.contains(CellFlags::BOLD);
                    let italic = cell.flags.contains(CellFlags::ITALIC);
                    let has_glyph = cell.c != ' ' && cell.c != '\0';

                    let mut flags = 0u32;
                    if has_glyph {
                        flags |= 1;
                    }
                    if bold {
                        flags |= 2;
                    }
                    if italic {
                        flags |= 4;
                    }

                    let (uv_offset, uv_size, glyph_offset, glyph_size) = if has_glyph {
                        let g = self.glyph_cache.get(cell.c, bold, italic);
                        match self.atlas_map.get(&(cell.c, bold, italic)) {
                            Some(rect) if rect.w > 0 && rect.h > 0 => {
                                let baseline = CELL_HEIGHT as f32 - 3.0;
                                let gx = g.bearing_x.max(0.0);
                                let gy = (baseline + g.bearing_y).max(0.0);
                                (
                                    [
                                        rect.x as f32 / ATLAS_SIZE as f32,
                                        rect.y as f32 / ATLAS_SIZE as f32,
                                    ],
                                    [
                                        rect.w as f32 / ATLAS_SIZE as f32,
                                        rect.h as f32 / ATLAS_SIZE as f32,
                                    ],
                                    [gx, gy],
                                    [rect.w as f32, rect.h as f32],
                                )
                            }
                            _ => ([0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]),
                        }
                    } else {
                        ([0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0])
                    };

                    // Offset by TAB_BAR_HEIGHT so the grid starts below the
                    // tab bar.
                    let y = (row_idx as u32 * CELL_HEIGHT + TAB_BAR_HEIGHT) as f32;
                    instances.push(CellInstance {
                        position: [(col_idx as u32 * CELL_WIDTH) as f32, y],
                        size: [CELL_WIDTH as f32, CELL_HEIGHT as f32],
                        glyph_offset,
                        glyph_size,
                        uv_offset,
                        uv_size,
                        bg_color: bg,
                        fg_color: fg,
                        flags,
                    });
                }
            }
        }

        // ── Status bar (bottom row) ──────────────────────────────────────
        instances.extend(self.build_status_bar_instances(app, cols, total_rows));

        instances
    }

    /// Build the tab bar instances: one row of cells at the top of the
    /// window showing each tab's index and title, with the active tab
    /// highlighted. Uses the same instanced-quad pipeline as the grid.
    fn build_tab_bar_instances(&mut self, app: &App, cols: usize) -> Vec<CellInstance> {
        let manager = &app.manager;
        let active_idx = manager.active;
        let total_cols = cols;

        // Start with the tab bar background — fill the entire row.
        let mut instances: Vec<CellInstance> = Vec::with_capacity(total_cols);
        for col_idx in 0..total_cols {
            instances.push(CellInstance {
                position: [(col_idx as u32 * CELL_WIDTH) as f32, 0.0],
                size: [CELL_WIDTH as f32, CELL_HEIGHT as f32],
                glyph_offset: [0.0, 0.0],
                glyph_size: [0.0, 0.0],
                uv_offset: [0.0, 0.0],
                uv_size: [0.0, 0.0],
                bg_color: COLOR_TABBAR_BG,
                fg_color: COLOR_TAB_INACTIVE_FG,
                flags: 0,
            });
        }

        // Render each tab's title starting at column 1 (leave 1-col margin).
        let mut col: usize = 1;
        for (i, tab) in manager.tabs.iter().enumerate() {
            let is_active = i == active_idx;

            // Build the tab label: " N: title " (space, index, colon, title, space)
            let tag_str = tab.tag.as_ref().map(|g| format!(" [{}]", g)).unwrap_or_default();
            let label = format!(" {}: {}{} ", i + 1, tab.title, tag_str);

            let bg = if is_active { COLOR_TAB_ACTIVE_BG } else { COLOR_TAB_INACTIVE_BG };
            // Active tab uses the cyan accent color for its text to make it
            // stand out; inactive tabs use a muted gray.
            let fg = if is_active { COLOR_TAB_ACCENT } else { COLOR_TAB_INACTIVE_FG };

            for ch in label.chars() {
                if col >= total_cols {
                    break;
                }

                let has_glyph = ch != ' ';
                let mut flags = 0u32;
                if has_glyph {
                    flags |= 1;
                }
                if is_active {
                    flags |= 2; // bold for active tab
                }

                let (uv_offset, uv_size, glyph_offset, glyph_size) = if has_glyph {
                    let bold = is_active; // active tab is bold
                    let g = self.glyph_cache.get(ch, bold, false);
                    match self.atlas_map.get(&(ch, bold, false)) {
                        Some(rect) if rect.w > 0 && rect.h > 0 => {
                            let baseline = CELL_HEIGHT as f32 - 3.0;
                            let gx = g.bearing_x.max(0.0);
                            let gy = (baseline + g.bearing_y).max(0.0);
                            (
                                [
                                    rect.x as f32 / ATLAS_SIZE as f32,
                                    rect.y as f32 / ATLAS_SIZE as f32,
                                ],
                                [
                                    rect.w as f32 / ATLAS_SIZE as f32,
                                    rect.h as f32 / ATLAS_SIZE as f32,
                                ],
                                [gx, gy],
                                [rect.w as f32, rect.h as f32],
                            )
                        }
                        _ => ([0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]),
                    }
                } else {
                    ([0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0])
                };

                // Overwrite the background instance at this column.
                instances[col] = CellInstance {
                    position: [(col as u32 * CELL_WIDTH) as f32, 0.0],
                    size: [CELL_WIDTH as f32, CELL_HEIGHT as f32],
                    glyph_offset,
                    glyph_size,
                    uv_offset,
                    uv_size,
                    bg_color: bg,
                    fg_color: fg,
                    flags,
                };
                col += 1;
            }

            // Separator between tabs (a thin dim column).
            if col < total_cols && i < manager.tabs.len() - 1 {
                instances[col] = CellInstance {
                    position: [(col as u32 * CELL_WIDTH) as f32, 0.0],
                    size: [CELL_WIDTH as f32, CELL_HEIGHT as f32],
                    glyph_offset: [0.0, 0.0],
                    glyph_size: [0.0, 0.0],
                    uv_offset: [0.0, 0.0],
                    uv_size: [0.0, 0.0],
                    bg_color: COLOR_TAB_SEPARATOR,
                    fg_color: COLOR_TAB_SEPARATOR,
                    flags: 0,
                };
                col += 1;
            }
        }

        instances
    }

    /// Build the status bar instances: broadcast indicator, tab count,
    /// active tab index. Rendered as the bottom row of the window.
    fn build_status_bar_instances(&mut self, app: &App, cols: usize, total_rows: usize) -> Vec<CellInstance> {
        let manager = &app.manager;
        let active_idx = manager.active;
        let y = (total_rows.saturating_sub(1) as u32 * CELL_HEIGHT) as f32;

        // Build the status text.
        let (status_text, text_color) = match &manager.broadcast {
            BroadcastTarget::Active => (
                format!(" rs-mrxvt  |  tabs={}  active={}/{}  ", manager.tabs.len(), active_idx + 1, manager.tabs.len()),
                COLOR_STATUS_FG,
            ),
            BroadcastTarget::All => (
                format!(" BROADCAST:ALL  |  tabs={}  ", manager.tabs.len()),
                COLOR_BROADCAST_ALL,
            ),
            BroadcastTarget::Group(g) => (
                format!(" BROADCAST:{}  |  tabs={}  ", g, manager.tabs.len()),
                COLOR_BROADCAST_GROUP,
            ),
        };

        let mut instances: Vec<CellInstance> = Vec::with_capacity(cols);

        // Fill the status bar background.
        for col_idx in 0..cols {
            instances.push(CellInstance {
                position: [(col_idx as u32 * CELL_WIDTH) as f32, y],
                size: [CELL_WIDTH as f32, CELL_HEIGHT as f32],
                glyph_offset: [0.0, 0.0],
                glyph_size: [0.0, 0.0],
                uv_offset: [0.0, 0.0],
                uv_size: [0.0, 0.0],
                bg_color: COLOR_STATUS_BG,
                fg_color: text_color,
                flags: 0,
            });
        }

        // Render the status text over the background.
        for (col, ch) in status_text.chars().enumerate() {
            if col >= cols {
                break;
            }

            let has_glyph = ch != ' ';
            let mut flags = 0u32;
            if has_glyph {
                flags |= 1;
            }

            let (uv_offset, uv_size, glyph_offset, glyph_size) = if has_glyph {
                let g = self.glyph_cache.get(ch, false, false);
                match self.atlas_map.get(&(ch, false, false)) {
                    Some(rect) if rect.w > 0 && rect.h > 0 => {
                        let baseline = CELL_HEIGHT as f32 - 3.0;
                        let gx = g.bearing_x.max(0.0);
                        let gy = (baseline + g.bearing_y).max(0.0);
                        (
                            [
                                rect.x as f32 / ATLAS_SIZE as f32,
                                rect.y as f32 / ATLAS_SIZE as f32,
                            ],
                            [
                                rect.w as f32 / ATLAS_SIZE as f32,
                                rect.h as f32 / ATLAS_SIZE as f32,
                            ],
                            [gx, gy],
                            [rect.w as f32, rect.h as f32],
                        )
                    }
                    _ => ([0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]),
                }
            } else {
                ([0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0])
            };

            instances[col] = CellInstance {
                position: [(col as u32 * CELL_WIDTH) as f32, y],
                size: [CELL_WIDTH as f32, CELL_HEIGHT as f32],
                glyph_offset,
                glyph_size,
                uv_offset,
                uv_size,
                bg_color: COLOR_STATUS_BG,
                fg_color: text_color,
                flags,
            };
        }

        instances
    }

    /// If new glyphs have been rasterized since the last upload, re-upload
    /// the atlas texture with all cached glyphs packed in. Rebuilds
    /// `atlas_map` so `build_instances` can find each glyph's UV rect.
    ///
    /// Uses a simple row-based shelf packing: glyphs are placed left-to-right
    /// in the current row; when a glyph won't fit, move to the next row.
    /// This is O(glyph_count) and only runs when the cache grows.
    fn maybe_upload_atlas(&mut self) {
        let count = self.glyph_cache.len();
        if count == self.cached_glyph_count {
            return;
        }
        self.cached_glyph_count = count;

        // Clear the atlas and the map — we rebuild both from scratch.
        let mut atlas = vec![0u8; (ATLAS_SIZE * ATLAS_SIZE) as usize];
        self.atlas_map.clear();

        // Shelf-packing state.
        let mut cur_x: u32 = 0;
        let mut cur_y: u32 = 0;
        let mut row_height: u32 = 0;

        // Iterate every cached glyph and blit it into the atlas.
        for ((c, bold, italic), g) in self.glyph_cache.iter() {
            let gw = g.width as u32;
            let gh = g.height as u32;

            // Skip empty glyphs (e.g. spaces, missing outlines). We still
            // record a zero-size rect so build_instances can look them up
            // without re-triggering the cache check.
            if gw == 0 || gh == 0 {
                self.atlas_map.insert((c, bold, italic), AtlasRect { x: 0, y: 0, w: 0, h: 0 });
                continue;
            }

            let padded_w = gw + ATLAS_PADDING;
            let padded_h = gh + ATLAS_PADDING;

            // Does it fit on the current row?
            if cur_x + padded_w > ATLAS_SIZE {
                // Move to the next row.
                cur_y += row_height;
                cur_x = 0;
                row_height = 0;
            }

            // Does it fit vertically? If not, the atlas is full. Skip the
            // glyph (it won't render, but we won't crash).
            if cur_y + padded_h > ATLAS_SIZE {
                log::warn!("glyph atlas full ({}x{}); skipping glyph {:?}", ATLAS_SIZE, ATLAS_SIZE, c);
                self.atlas_map.insert((c, bold, italic), AtlasRect { x: 0, y: 0, w: 0, h: 0 });
                continue;
            }

            let rect = AtlasRect { x: cur_x, y: cur_y, w: gw, h: gh };
            self.atlas_map.insert((c, bold, italic), rect);

            // Blit the glyph's alpha channel into the atlas.
            for gy in 0..gh {
                for gx in 0..gw {
                    let px = (cur_x + gx) as usize;
                    let py = (cur_y + gy) as usize;
                    let gidx = (gy as usize * gw as usize + gx as usize) * 4 + 3;
                    let alpha = g.pixels[gidx];
                    let aidx = py * ATLAS_SIZE as usize + px;
                    atlas[aidx] = alpha;
                }
            }

            cur_x += padded_w;
            row_height = row_height.max(padded_h);
        }

        log::debug!(
            "atlas rebuilt: {} glyphs, packed to row y={}",
            self.atlas_map.len(),
            cur_y + row_height
        );

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
        // Pump the event loop briefly so the window receives its initial
        // configure event and gets mapped by the compositor. Without this,
        // the first render() call may run before the surface is ready
        // (especially on Wayland), causing get_current_texture to fail.
        let mut tracker = self.mod_tracker;
        let _ = self.event_loop.pump_events(
            Some(std::time::Duration::from_millis(50)),
            |event, elwt| {
                let _ = crate::ui::event::winit_translate::translate_event(
                    &event, &mut tracker, elwt,
                );
            },
        );
        self.mod_tracker = tracker;

        // Now that the window has its initial size, configure the surface
        // to match. This avoids a wasted first frame.
        let size = self.window.inner_size();
        if size.width > 0 && size.height > 0 {
            self.config.width = size.width;
            self.config.height = size.height;
            self.surface.configure(&self.device, &self.config);
            log::debug!("wgpu surface configured on init: {}x{}", size.width, size.height);
        }

        Ok(())
    }

    fn fini(&mut self) -> Result<()> {
        Ok(())
    }

    fn poll_event(&mut self, timeout_ms: u64) -> Result<Option<crate::ui::event::AppEvent>> {
        // If we have buffered events from a previous pump, return the oldest.
        if !self.pending_events.is_empty() {
            return Ok(self.pending_events.drain(..).next());
        }

        // Pump the winit event loop. This processes window events (resize,
        // keyboard, mouse, close) for up to `timeout_ms`, translating each
        // into an `AppEvent` via the winit_translate module. Without this
        // pump call, the window never gets its initial configure event and
        // appears as a blank form.
        //
        // We collect into a local Vec because `pump_events` borrows
        // `self.event_loop` mutably, preventing the closure from capturing
        // `self.pending_events` directly.
        let mut collected: Vec<crate::ui::event::AppEvent> = Vec::new();
        let mut tracker = self.mod_tracker;
        let timeout = Some(std::time::Duration::from_millis(timeout_ms));

        let status = self.event_loop.pump_events(timeout, |event, elwt| {
            if let Some(app_ev) = crate::ui::event::winit_translate::translate_event(
                &event, &mut tracker, elwt,
            ) {
                collected.push(app_ev);
            }
        });

        self.mod_tracker = tracker;

        // If the event loop is exiting (window close button, etc.), signal quit.
        if let PumpStatus::Exit(_) = status {
            collected.push(crate::ui::event::AppEvent::Quit);
        }

        // Return the first event, if any. The rest stay buffered for next call.
        if collected.is_empty() {
            Ok(None)
        } else {
            let first = collected.remove(0);
            self.pending_events.extend(collected);
            Ok(Some(first))
        }
    }

    fn render(&mut self, app: &mut App) -> Result<()> {
        // Resize surface if the window changed. We also force a reconfigure
        // when width/height are still 0 (first frame, or the WM hasn't sent
        // the initial configure event yet — rare but possible on Wayland).
        let size = self.window.inner_size();
        let needs_configure = size.width > 0 && size.height > 0
            && (size.width != self.config.width || size.height != self.config.height);
        if needs_configure {
            self.config.width = size.width;
            self.config.height = size.height;
            self.surface.configure(&self.device, &self.config);
            log::debug!("wgpu surface reconfigured to {}x{}", size.width, size.height);
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
                // This happens when the surface hasn't been configured yet
                // (first frame on Wayland, before the initial configure event)
                // or after the window is minimized. Re-configure on next tick
                // and skip this frame. Log at info level the first time so the
                // user can see what's happening if they're watching logs.
                log::info!("get_current_texture failed (will retry): {e}");
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
                            r: COLOR_BG[0] as f64,
                            g: COLOR_BG[1] as f64,
                            b: COLOR_BG[2] as f64,
                            a: COLOR_BG[3] as f64,
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
        // The wgpu renderer draws a tab bar at the top and a status bar at
        // the bottom, each one cell row tall. The terminal grid gets the
        // space between them. The PTY is told exactly how many rows fit in
        // the terminal area so the shell's line wrapping matches what's
        // on screen.
        let cols = (self.config.width / CELL_WIDTH).max(2) as u16;
        let total_rows = self.config.height / CELL_HEIGHT;
        // Subtract 2 rows: tab bar (top) + status bar (bottom).
        let term_rows = total_rows.saturating_sub(2).max(1) as u16;
        (cols, term_rows)
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
