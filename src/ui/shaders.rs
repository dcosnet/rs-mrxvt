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
//! One instanced quad pipeline. Each instance = one terminal cell. The quad
//! covers the full cell (for the background fill). The glyph is sampled from
//! a sub-rect of the atlas that corresponds to the glyph's actual rasterized
//! pixels — NOT stretched to fill the cell. This keeps baselines aligned and
//! prevents the jagged/mirrored look that stretching caused.
//!
//! ## Instance layout (must match `CellInstance` in wgpu.rs)
//!
//! For each instance:
//! - `position`: vec2<f32> — cell top-left in pixels
//! - `size`: vec2<f32> — cell size in pixels (CELL_WIDTH × CELL_HEIGHT)
//! - `uv_offset`: vec2<f32> — top-left of glyph rect in atlas (0..1 UV)
//! - `uv_size`: vec2<f32> — size of glyph rect in atlas (0..1 UV)
//! - `bg_color`: vec4<f32> — background color (linear RGBA)
//! - `fg_color`: vec4<f32> — foreground color (linear RGBA)
//! - `flags`: u32 — bit 0: has_glyph, bit 1: bold, bit 2: italic

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
    @location(2) glyph_offset: vec2<f32>,
    @location(3) glyph_size: vec2<f32>,
    @location(4) uv_offset: vec2<f32>,
    @location(5) uv_size: vec2<f32>,
    @location(6) bg_color: vec4<f32>,
    @location(7) fg_color: vec4<f32>,
    @location(8) flags: u32,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    /// Pixel coordinate within the cell, (0,0) = top-left of cell.
    /// Used by the fragment shader to determine if this fragment is inside
    /// the glyph rect.
    @location(0) cell_pixel: vec2<f32>,
    @location(1) uv_offset: vec2<f32>,
    @location(2) uv_size: vec2<f32>,
    @location(3) glyph_offset: vec2<f32>,
    @location(4) glyph_size: vec2<f32>,
    @location(5) fg_color: vec4<f32>,
    @location(6) bg_color: vec4<f32>,
    @location(7) flags: u32,
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
    //
    // We compute the corner position arithmetically from the two low bits of
    // `vertex_index` instead of building a corner table and indexing it with
    // `vid`. naga (wgpu's shader validator) rejects dynamic indexing into
    // non-`const` array values on several backends (GL/Vulkan) with
    // "Expression may only be indexed by a constant". The bit pattern maps
    // identically to the original corner table:
    //   vid=0 → (0,0)   vid=1 → (1,0)   vid=2 → (0,1)   vid=3 → (1,1)
    // and works on every wgpu backend because it's pure arithmetic.
    let corner = vec2<f32>(f32(vid & 1u), f32((vid >> 1u) & 1u));

    // Cell rect in screen-space pixels. The quad always covers the full cell
    // so the background fills correctly.
    let cell_min = in.position;
    let cell_max = in.position + in.size;
    let p = mix(cell_min, cell_max, corner);

    // Pixel coordinate within this cell (0,0 = top-left). Passed to the
    // fragment shader so it can test whether this fragment is inside the
    // glyph rect and compute the correct atlas UV.
    let cell_pixel = corner * in.size;

    var out: VertexOutput;
    out.clip_position = vec4<f32>(screen_to_ndc(p), 0.0, 1.0);
    out.cell_pixel = cell_pixel;
    out.uv_offset = in.uv_offset;
    out.uv_size = in.uv_size;
    out.glyph_offset = in.glyph_offset;
    out.glyph_size = in.glyph_size;
    out.fg_color = in.fg_color;
    out.bg_color = in.bg_color;
    out.flags = in.flags;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Always draw the background color first.
    var color = in.bg_color;

    // If the cell has a glyph, check whether this fragment falls inside the
    // glyph rect. If it does, compute the atlas UV and sample; if not, leave
    // the background. This prevents the glyph from being stretched to fill
    // the entire cell — instead it renders at its natural size, positioned
    // at glyph_offset within the cell.
    let has_glyph = (in.flags & 1u) != 0u;
    if has_glyph {
        let glyph_min = in.glyph_offset;
        let glyph_max = in.glyph_offset + in.glyph_size;

        if all(in.cell_pixel >= glyph_min) && all(in.cell_pixel < glyph_max) {
            // Fragment is inside the glyph rect. Map cell-pixel to atlas UV.
            let local = (in.cell_pixel - in.glyph_offset) / in.glyph_size;
            let atlas_uv = in.uv_offset + local * in.uv_size;

            // The atlas is an R8Unorm texture (single-channel alpha mask).
            // Sampling it yields vec4(r, 0, 0, 1), so we read the R channel
            // (not the alpha channel, which is always 1.0 for R8 textures)
            // to get the glyph coverage.
            let glyph_alpha = textureSample(glyph_atlas, glyph_sampler, atlas_uv).r;
            color = mix(color, in.fg_color, glyph_alpha);
        }
    }

    return color;
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for the "naga rejects dynamic array indexing" crash.
    ///
    /// The old vertex shader built a `corners` array inline and indexed it
    /// with the vertex_index builtin. naga rejected this on GL/Vulkan
    /// backends with "Expression may only be indexed by a constant", crashing
    /// the app at shader-module creation. The fix replaced the array+index
    /// with bit arithmetic on `vid`.
    ///
    /// This test guards against any future regression that reintroduces
    /// dynamic indexing into a non-`const` array.
    #[test]
    fn vertex_shader_does_not_use_dynamic_array_indexing() {
        // The rejected pattern: indexing a non-const array with vertex_index.
        assert!(
            !SHADER_SOURCE.contains("corners[vid]"),
            "shader must not index a non-const array with vertex_index — \
             naga rejects this on GL/Vulkan. Use bit arithmetic on vid instead."
        );

        // The corner array literal itself is a smell — if it's back, the
        // fix has been reverted.
        assert!(
            !SHADER_SOURCE.contains("array<vec2<f32>, 4>"),
            "shader should compute corners arithmetically, not via an array literal"
        );
    }

    /// The vertex shader must produce the correct corner mapping for
    /// TriangleStrip order: vid 0..3 → (0,0), (1,0), (0,1), (1,1).
    #[test]
    fn vertex_shader_corner_mapping_is_present() {
        assert!(
            SHADER_SOURCE.contains("vid & 1u") && SHADER_SOURCE.contains("(vid >> 1u) & 1u"),
            "vertex shader should compute corner via `vec2<f32>(f32(vid & 1u), f32((vid >> 1u) & 1u))`"
        );
    }

    /// Regression test for the "blank window with no text" bug.
    ///
    /// The atlas texture is R8Unorm — a single-channel format. Sampling it
    /// returns vec4(r, 0, 0, 1), so reading the alpha channel always returns
    /// 1.0. The fix reads the R channel instead.
    #[test]
    fn fragment_shader_reads_r_channel_not_alpha() {
        // The shader must sample the atlas's R channel, not the alpha channel.
        // We check that every textureSample call on glyph_atlas reads .r
        // and never .a. (The exact UV expression has changed over time —
        // `in.uv` became `atlas_uv` — so we match on the call pattern, not
        // the full expression.)
        assert!(
            SHADER_SOURCE.contains("textureSample(glyph_atlas, glyph_sampler,") ,
            "fragment shader must call textureSample on glyph_atlas"
        );
        assert!(
            SHADER_SOURCE.contains("textureSample(glyph_atlas, glyph_sampler, atlas_uv).r"),
            "fragment shader must sample the atlas's .r channel (R8Unorm texture, \
             not .a which is always 1.0 for single-channel textures)"
        );
        assert!(
            !SHADER_SOURCE.contains(").a;\n"),
            "fragment shader must not read .a from the R8 atlas — it's always 1.0"
        );
    }
}
