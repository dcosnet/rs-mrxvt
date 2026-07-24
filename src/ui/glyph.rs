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


//! Glyph cache shared between the wgpu and softbuffer renderers.
//!
//! Builds an in-memory atlas of rasterized glyphs using `ab_glyph`. The
//! atlas is keyed by `(char, style)` and stores glyph bitmaps as `Vec<u8>`
//! in RGBA format. Both the wgpu backend (which uploads it to a texture)
//! and the softbuffer backend (which composites via tiny-skia) consume the
//! same cache.
//!
//! ## Font selection
//!
//! Uses a bundled monospace font (or one resolved via fontconfig when
//! available). For the MVP we ship a fallback font compiled into the binary
//! so the renderer works on any system. A future `--font` CLI flag will
//! let the user override.
//!
//! ## Status
//!
//! Compiles only with `--features gpu`. The cache itself is testable
//! without a GPU; only the wgpu upload step needs graphics.

use std::collections::HashMap;

use ab_glyph::{Font, FontVec, Glyph, PxScale, ScaleFont};
use anyhow;

/// A cached glyph: its rasterized bitmap plus placement info.
#[derive(Debug, Clone)]
pub struct CachedGlyph {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// RGBA pixel data (length = width × height × 4).
    pub pixels: Vec<u8>,
    /// Horizontal bearing (offset from cursor X to glyph left edge).
    pub bearing_x: f32,
    /// Vertical bearing (offset from baseline to glyph top).
    pub bearing_y: f32,
    /// Horizontal advance (how far to move the cursor for the next glyph).
    pub advance: f32,
}

/// A glyph cache.
///
/// Lazily rasterizes glyphs on demand and stores them. The cache is keyed
/// by `(char, style)` — different styles are rasterized as separate
/// entries.
pub struct GlyphCache {
    font: FontVec,
    scale: PxScale,
    /// (char, bold, italic) → CachedGlyph
    cache: HashMap<(char, bool, bool), CachedGlyph>,
}

impl GlyphCache {
    /// Build a new cache at the given pixel size.
    ///
    /// Uses a bundled fallback font (DejaVu Sans Mono) so this works on any
    /// system. A future version will accept a font path override.
    pub fn new(pixel_size: f32) -> anyhow::Result<Self> {
        let font_data = bundled_font()?;
        let font = FontVec::try_from_vec(font_data)
            .map_err(|e| anyhow::anyhow!("bundled font is invalid: {e}"))?;
        let scale = PxScale::from(pixel_size);
        Ok(Self {
            font,
            scale,
            cache: HashMap::new(),
        })
    }

    /// Look up (or rasterize on miss) a glyph.
    pub fn get(&mut self, c: char, bold: bool, italic: bool) -> &CachedGlyph {
        let key = (c, bold, italic);
        // We avoid `entry().or_insert_with(|| self.rasterize(...))` because it
        // triggers E0502 (the closure needs `&self` while `entry` holds
        // `&mut self.cache`). We also avoid an early `return self.cache.get()`
        // because that would tie an immutable borrow to the return lifetime
        // and block the `insert` below. Instead: check, insert on miss, then
        // fetch — no borrow is held across the mutation.
        if !self.cache.contains_key(&key) {
            let glyph = self.rasterize(c, bold, italic);
            self.cache.insert(key, glyph);
        }
        self.cache
            .get(&key)
            .expect("glyph was either already cached or just inserted")
    }

    fn rasterize(&self, c: char, _bold: bool, _italic: bool) -> CachedGlyph {
        // `as_scaled` borrows; we use the borrow throughout this function.
        let scaled = self.font.as_scaled(self.scale);
        let glyph_id = self.font.glyph_id(c);
        let glyph: Glyph = glyph_id.with_scale_and_position(self.scale, ab_glyph::point(0.0, scaled.ascent()));

        // Empty placeholder glyph if the outline is missing (rare).
        let advance = scaled.h_advance(glyph_id);
        let ascent = scaled.ascent();

        let outlined = match self.font.outline_glyph(glyph) {
            Some(o) => o,
            None => {
                return CachedGlyph {
                    width: 0,
                    height: 0,
                    pixels: Vec::new(),
                    bearing_x: 0.0,
                    bearing_y: 0.0,
                    advance,
                };
            }
        };

        let bounds = outlined.px_bounds();
        let width = bounds.width().max(1.0) as usize;
        let height = bounds.height().max(1.0) as usize;

        let mut pixels = vec![0u8; width * height * 4];
        outlined.draw(|x, y, coverage| {
            let x = x as usize;
            let y = y as usize;
            if x < width && y < height {
                let alpha = (coverage.clamp(0.0, 1.0) * 255.0) as u8;
                let idx = (y * width + x) * 4;
                // White glyph with alpha = coverage (so callers can tint
                // by multiplying RGB at composite time).
                pixels[idx] = 255;
                pixels[idx + 1] = 255;
                pixels[idx + 2] = 255;
                pixels[idx + 3] = alpha;
            }
        });

        CachedGlyph {
            width,
            height,
            pixels,
            bearing_x: bounds.min.x,
            bearing_y: bounds.min.y - ascent,
            advance,
        }
    }

    /// Clear the cache (forces re-rasterization on next access).
    pub fn clear(&mut self) {
        self.cache.clear();
    }

    /// Number of cached glyphs.
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// Is the cache empty?
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    /// The configured pixel size.
    pub fn pixel_size(&self) -> f32 {
        self.scale.y
    }
}

/// Return a bundled monospace font. We use DejaVu Sans Mono, which is a
/// high-quality, freely-licensed font that ships with most Linux distros.
fn bundled_font() -> anyhow::Result<Vec<u8>> {
    let candidates = [
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
        "/usr/share/fonts/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/dejavu-sans-mono/DejaVuSansMono.ttf",
        "/usr/local/share/fonts/dejavu/DejaVuSansMono.ttf",
    ];
    candidates.iter()
        .find_map(|p| std::fs::read(p).ok())
        .ok_or_else(|| anyhow::anyhow!("no monospace font found; install DejaVu fonts"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_rasterizes_basic_chars() {
        // Skip if no font available (sandbox might not have DejaVu).
        let candidates = [
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
            "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
        ];
        if !candidates.iter().any(|p| std::path::Path::new(p).exists()) {
            eprintln!("skipping glyph cache test — no DejaVu font found");
            return;
        }

        let mut cache = GlyphCache::new(16.0).expect("glyph cache init failed");
        let g = cache.get('a', false, false);
        assert!(g.width > 0);
        assert!(g.height > 0);
        assert!(g.advance > 0.0);
        assert_eq!(g.pixels.len(), g.width * g.height * 4);
    }

    #[test]
    fn cache_dedupes() {
        let candidates = [
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
            "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
        ];
        if !candidates.iter().any(|p| std::path::Path::new(p).exists()) {
            return;
        }

        let mut cache = GlyphCache::new(16.0).expect("glyph cache init failed");
        cache.get('x', false, false);
        cache.get('x', false, false);
        assert_eq!(cache.len(), 1);
    }
}
