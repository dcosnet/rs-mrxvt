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


//! Pseudo-transparency + tinting state.
//!
//! Implements the classic mrxvt `-tint` and `-sh` (shading) flags in a
//! modern, shader-driven way. The state lives here; the wgpu backend
//! consumes it via the [`TransparencyUniforms`] struct and the softbuffer
//! backend reads it directly when compositing.
//!
//! ## Classic mrxvt semantics
//!
//! - `-tint <color>`: tint the background with the given color (multiply).
//! - `-sh <value>`: shading amount, 0..100 (higher = darker).
//! - `-bg <image>`: load a background image (root-pixmap in old mrxvt;
//!   compositor screenshot or static image in 2026).
//!
//! ## Modern implementation
//!
//! True transparency requires the compositor to blend the window with what's
//! behind it. On Wayland this means using the `wlr-layer-shell` protocol
//! (which the winit backend doesn't currently expose). For the MVP, we
//! support:
//!
//! - **Static background image**: the user provides a path via `-bg`; we
//!   load it, upload it as a wgpu texture, and the shader samples it for
//!   each cell's background instead of using the cell's bg color directly.
//! - **Tinting**: the shader multiplies the sampled background by the tint
//!   color before drawing text on top.
//! - **Shading (opacity)**: the shader reduces the alpha of the final
//!   pixel, letting the desktop show through where the compositor supports it.
//!
//! ## Config
//!
//! In `config.toml`:
//!
//! ```toml
//! [transparency]
//! enabled = true
//! tint = "#004080"      # blue tint
//! opacity = 0.85         # 1.0 = opaque, 0.0 = fully transparent
//! background_image = "/path/to/wallpaper.png"
//! ```
//!
//! In Lua config:
//!
//! ```lua
//! return {
//!   transparency = {
//!     enabled = true,
//!     tint = "#004080",
//!     opacity = 0.85,
//!   },
//! }
//! ```

use serde::{Deserialize, Serialize};

/// Transparency / tinting configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransparencyConfig {
    /// Master toggle. When false, the terminal is fully opaque.
    #[serde(default)]
    pub enabled: bool,
    /// Tint color as `#RRGGBB` hex string. Applied multiplicatively.
    #[serde(default = "default_tint")]
    pub tint: String,
    /// Window opacity: 0.0 (invisible) to 1.0 (opaque).
    #[serde(default = "default_opacity")]
    pub opacity: f32,
    /// Optional path to a background image. If set, the shader samples this
    /// instead of the cell's bg color.
    #[serde(default)]
    pub background_image: Option<String>,
}

impl Default for TransparencyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            tint: default_tint(),
            opacity: default_opacity(),
            background_image: None,
        }
    }
}

fn default_tint() -> String {
    "#000000".into()
}

fn default_opacity() -> f32 {
    1.0
}

/// Parsed tint color as linear RGB.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TintColor {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

impl TintColor {
    /// Parse a `#RRGGBB` hex string into linear RGB (0..1).
    pub fn parse(hex: &str) -> Option<Self> {
        let hex = hex.strip_prefix('#').unwrap_or(hex);
        if hex.len() != 6 {
            return None;
        }
        let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
        let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
        let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
        Some(Self {
            r: r as f32 / 255.0,
            g: g as f32 / 255.0,
            b: b as f32 / 255.0,
        })
    }

    /// Black tint (no effect).
    pub fn black() -> Self {
        Self { r: 0.0, g: 0.0, b: 0.0 }
    }

    /// White tint (full brightening).
    pub fn white() -> Self {
        Self { r: 1.0, g: 1.0, b: 1.0 }
    }
}

/// Runtime transparency state. Held by the app; sampled by renderers each frame.
#[derive(Debug, Clone)]
pub struct TransparencyState {
    pub config: TransparencyConfig,
    pub tint: TintColor,
}

impl TransparencyState {
    pub fn new(config: TransparencyConfig) -> Self {
        let tint = TintColor::parse(&config.tint).unwrap_or_else(TintColor::black);
        Self { config, tint }
    }

    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    pub fn opacity(&self) -> f32 {
        self.config.opacity.clamp(0.0, 1.0)
    }

    pub fn tint(&self) -> TintColor {
        self.tint
    }
}

impl Default for TransparencyState {
    fn default() -> Self {
        Self::new(TransparencyConfig::default())
    }
}

/// WGSL-friendly uniforms for the transparency shader.
///
/// Pass this to the shader as a `uniform` block. The shader uses it to
/// tint and shade the sampled background.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable, Default)]
pub struct TransparencyUniforms {
    /// Tint color (linear RGB, 0..1).
    pub tint_color: [f32; 3],
    /// Tint strength: 0.0 = no tint, 1.0 = full tint color.
    pub tint_strength: f32,
    /// Opacity: 0.0 (invisible) to 1.0 (opaque).
    pub opacity: f32,
    /// Padding to align to 16 bytes.
    pub _pad: [f32; 3],
}

impl TransparencyUniforms {
    pub fn from_state(state: &TransparencyState) -> Self {
        let tint = state.tint();
        Self {
            tint_color: [tint.r, tint.g, tint.b],
            tint_strength: if state.enabled() { 1.0 } else { 0.0 },
            opacity: if state.enabled() { state.opacity() } else { 1.0 },
            _pad: [0.0; 3],
        }
    }
}

/// WGSL snippet that applies tinting and shading to a sampled background color.
///
/// Drop this into the fragment shader after sampling the background:
///
/// ```wgsl
/// var bg = textureSample(background_atlas, background_sampler, uv);
/// // Apply tint + opacity:
/// bg = apply_transparency(bg, transparency_uniforms);
/// ```
pub const TRANSPARENCY_SHADER_SNIPPET: &str = r#"
fn apply_transparency(bg: vec4<f32>, t: TransparencyUniforms) -> vec4<f32> {
    // Tint: mix background toward tint color.
    let tinted = mix(bg.rgb, t.tint_color, t.tint_strength);
    // Opacity: scale alpha.
    return vec4<f32>(tinted, bg.a * t.opacity);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_tint() {
        let c = TintColor::parse("#FF8040").unwrap();
        assert!((c.r - 1.0).abs() < 0.01);
        assert!((c.g - 0.5).abs() < 0.01);
        assert!((c.b - 0.25).abs() < 0.01);
    }

    #[test]
    fn parses_tint_without_hash() {
        let c = TintColor::parse("00FF00").unwrap();
        assert!((c.g - 1.0).abs() < 0.01);
    }

    #[test]
    fn rejects_bad_hex() {
        assert!(TintColor::parse("#XYZ").is_none());
        assert!(TintColor::parse("#12345").is_none());
        assert!(TintColor::parse("").is_none());
    }

    #[test]
    fn default_config_is_disabled() {
        let cfg = TransparencyConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.opacity, 1.0);
    }

    #[test]
    fn state_reflects_config() {
        let cfg = TransparencyConfig {
            enabled: true,
            tint: "#800080".into(),
            opacity: 0.5,
            background_image: None,
        };
        let state = TransparencyState::new(cfg);
        assert!(state.enabled());
        assert!((state.opacity() - 0.5).abs() < 0.01);
        assert!((state.tint().r - 0.5).abs() < 0.01);
    }

    #[test]
    fn opacity_clamps() {
        let cfg = TransparencyConfig {
            enabled: true,
            tint: "#000000".into(),
            opacity: 1.5,
            background_image: None,
        };
        let state = TransparencyState::new(cfg);
        assert!((state.opacity() - 1.0).abs() < 0.01);
    }

    #[test]
    fn uniforms_reflect_state() {
        let cfg = TransparencyConfig {
            enabled: true,
            tint: "#FF0000".into(),
            opacity: 0.7,
            background_image: None,
        };
        let state = TransparencyState::new(cfg);
        let u = TransparencyUniforms::from_state(&state);
        assert!((u.tint_color[0] - 1.0).abs() < 0.01);
        assert!((u.opacity - 0.7).abs() < 0.01);
        assert!((u.tint_strength - 1.0).abs() < 0.01);
    }

    #[test]
    fn disabled_state_has_no_tint() {
        let cfg = TransparencyConfig {
            enabled: false,
            tint: "#FF0000".into(),
            opacity: 0.5,
            background_image: None,
        };
        let state = TransparencyState::new(cfg);
        let u = TransparencyUniforms::from_state(&state);
        assert!((u.tint_strength - 0.0).abs() < 0.01);
        assert!((u.opacity - 1.0).abs() < 0.01);
    }

    #[test]
    fn bad_tint_falls_back_to_black() {
        let cfg = TransparencyConfig {
            enabled: true,
            tint: "garbage".into(),
            opacity: 1.0,
            background_image: None,
        };
        let state = TransparencyState::new(cfg);
        assert_eq!(state.tint(), TintColor::black());
    }

    #[test]
    fn config_round_trips_toml() {
        let cfg = TransparencyConfig {
            enabled: true,
            tint: "#0080FF".into(),
            opacity: 0.85,
            background_image: Some("/path/to/bg.png".into()),
        };
        let s = toml::to_string(&cfg).unwrap();
        let parsed: TransparencyConfig = toml::from_str(&s).unwrap();
        assert_eq!(cfg.enabled, parsed.enabled);
        assert_eq!(cfg.tint, parsed.tint);
        assert_eq!(cfg.opacity, parsed.opacity);
        assert_eq!(cfg.background_image, parsed.background_image);
    }
}
