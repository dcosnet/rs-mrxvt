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


//! WGSL shaders for the wgpu backend.
//!
//! These are embedded as string constants so they're compiled into the binary
//! at build time. No external shader files needed.
//!
//! ## Shader design
//!
//! Two pipelines:
//! 1. **Background pipeline**: fills each cell with its bg color. Simple
//!    instanced quad shader.
//! 2. **Glyph pipeline**: samples the glyph atlas texture at the right UV
//!    coordinates and tints with the cell's fg color.
//!
//! Both pipelines share the same instance buffer layout for efficiency.

/// Shader source for the glyph + background rendering pipeline.
///
/// # Uniforms (group 0):
/// - binding 0: `uniforms` — global uniforms (resolution, time, etc.)
/// - binding 1: `glyph_atlas` — texture_2d<f32> with all rasterized glyphs
/// - binding 2: `glyph_sampler` — sampler (linear filter)
///
/// # Vertex format
/// For each instance:
/// - `position`: vec2<f32> — cell position in pixels (top-left corner)
/// - `size`: vec2<f32> — cell size in pixels
/// - `uv_offset`: vec2<f32> — offset into the glyph atlas (in texels)
/// - `uv_size`: vec2<f32> — size of the glyph in the atlas (in texels)
/// - `bg_color`: vec4<f32> — background color (linear RGBA)
/// - `fg_color`: vec4<f32> — foreground color (linear RGBA)
/// - `flags`: u32 — bit 0: has_glyph, bit 1: bold, bit 2: italic
pub const SHADER_SOURCE: &str = r#"
struct Uniforms {
    resolution: vec2<f32>,
    time: f32,
    padding: f32,
};

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var glyph_atlas: texture_2d<f32>;
@group(0) @binding(2) var glyph_sampler: sampler;

struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) uv_offset: vec2<f32>,
    @location(3) uv_size: vec2<f32>,
    @location(4) bg_color: vec4<f32>,
    @location(5) fg_color: vec4<f32>,
    @location(6) flags: u32,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) fg_color: vec4<f32>,
    @location(2) bg_color: vec4<f32>,
    @location(3) flags: u32,
};

// Convert screen-space pixels to NDC.
// Screen origin is top-left; NDC origin is center, Y up.
fn screen_to_ndc(p: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(
        (p.x / uniforms.resolution.x) * 2.0 - 1.0,
        1.0 - (p.y / uniforms.resolution.y) * 2.0,
    );
}

@vertex
fn vs_main(in: VertexInput, @builtin(vertex_index) vid: u32) -> VertexOutput {
    // Generate a unit quad (0,0)-(1,1) from vertex_index.
    let corners = array<vec2<f32>, 4>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 1.0),
    );
    let corner = corners[vid];

    // Cell rect in screen-space pixels.
    let cell_min = in.position;
    let cell_max = in.position + in.size;
    let p = mix(cell_min, cell_max, corner);

    // UV into the glyph atlas (in 0..1 range).
    let atlas_uv = in.uv_offset + corner * in.uv_size;

    var out: VertexOutput;
    out.clip_position = vec4<f32>(screen_to_ndc(p), 0.0, 1.0);
    out.uv = atlas_uv;
    out.fg_color = in.fg_color;
    out.bg_color = in.bg_color;
    out.flags = in.flags;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Always draw the background color first.
    var color = in.bg_color;

    // If the cell has a glyph, sample the atlas and blend over the bg.
    let has_glyph = (in.flags & 1u) != 0u;
    if has_glyph {
        let glyph_alpha = textureSample(glyph_atlas, glyph_sampler, in.uv).a;
        color = mix(color, in.fg_color, glyph_alpha);
    }

    return color;
}
"#;
