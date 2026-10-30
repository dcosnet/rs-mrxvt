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


//! True-color theme presets.
//!
//! Each theme defines 19 colors: the 16 ANSI colors (0-15), plus a
//! foreground, background, and cursor color. Themes are applied by the
//! renderers when translating `alacritty_terminal`'s `NamedColor` enum
//! into actual RGB values; the TUI renderer resolves `ui.theme` once per
//! frame, so config hot-reload switches themes live.
//!
//! ## Built-in presets
//!
//! - `mrxvt` — classic green-on-black (the default).
//! - `tokyo-night` — Tokyo Night, a popular dark theme.
//! - `gruvbox` — Gruvbox, warm retro colors.
//! - `dracula` — Dracula, dark purple.
//! - `solarized-dark` — Solarized Dark.
//! - `solarized-light` — Solarized Light.
//!
//! ## Custom themes
//!
//! Define a `[custom_themes.<name>]` table in config.toml with the full
//! palette (colors as `#RRGGBB`):
//!
//! ```toml
//! [ui]
//! theme = "midnight"
//!
//! [custom_themes.midnight]
//! bg = "#1a1b26"
//! fg = "#a9b1d6"
//! cursor = "#c0caf5"
//! black = "#15161e"
//! red = "#f7768e"
//! green = "#9ece6a"
//! yellow = "#e0af68"
//! blue = "#7aa2f7"
//! magenta = "#bb9af7"
//! cyan = "#7dcfff"
//! white = "#a9b1d6"
//! bright_black = "#414868"
//! bright_red = "#ff7a93"
//! bright_green = "#b9f27c"
//! bright_yellow = "#ffc777"
//! bright_blue = "#7daeff"
//! bright_magenta = "#bb9af7"
//! bright_cyan = "#0db9d7"
//! bright_white = "#acb0d0"
//! ```

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A complete theme: 16 ANSI colors + bg/fg/cursor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Theme {
    /// Display name. Optional for custom themes parsed from config (the
    /// table name is authoritative); filled in by [`resolve`].
    #[serde(default)]
    pub name: String,
    pub bg: Color,
    pub fg: Color,
    pub cursor: Color,
    pub black: Color,
    pub red: Color,
    pub green: Color,
    pub yellow: Color,
    pub blue: Color,
    pub magenta: Color,
    pub cyan: Color,
    pub white: Color,
    pub bright_black: Color,
    pub bright_red: Color,
    pub bright_green: Color,
    pub bright_yellow: Color,
    pub bright_blue: Color,
    pub bright_magenta: Color,
    pub bright_cyan: Color,
    pub bright_white: Color,
}

/// An RGB color.
///
/// Deserializes from either a `#RRGGBB` hex string (the config-file form)
/// or a `{ r = .., g = .., b = .. }` table.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Hash)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Hex(String),
            Rgb { r: u8, g: u8, b: u8 },
        }
        match Raw::deserialize(d)? {
            Raw::Hex(s) => Color::parse(&s)
                .ok_or_else(|| serde::de::Error::custom(format!("invalid hex color {s:?}"))),
            Raw::Rgb { r, g, b } => Ok(Color::new(r, g, b)),
        }
    }
}

impl Color {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Parse a `#RRGGBB` hex string.
    pub fn parse(hex: &str) -> Option<Self> {
        let hex = hex.strip_prefix('#').unwrap_or(hex);
        if hex.len() != 6 {
            return None;
        }
        let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
        let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
        let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
        Some(Self { r, g, b })
    }

    /// Convert to a `#RRGGBB` string.
    pub fn to_hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }
}

impl Theme {
    /// Look up a color by its ANSI index (0..15) or special name.
    pub fn ansi(&self, idx: u8) -> Color {
        match idx {
            0 => self.black,
            1 => self.red,
            2 => self.green,
            3 => self.yellow,
            4 => self.blue,
            5 => self.magenta,
            6 => self.cyan,
            7 => self.white,
            8 => self.bright_black,
            9 => self.bright_red,
            10 => self.bright_green,
            11 => self.bright_yellow,
            12 => self.bright_blue,
            13 => self.bright_magenta,
            14 => self.bright_cyan,
            15 => self.bright_white,
            _ => self.fg,
        }
    }
}

/// Get a built-in theme by name. Returns `None` for unknown names.
pub fn builtin(name: &str) -> Option<Theme> {
    match name {
        "mrxvt" => Some(mrxvt()),
        "tokyo-night" | "tokyo_night" => Some(tokyo_night()),
        "gruvbox" => Some(gruvbox()),
        "dracula" => Some(dracula()),
        "solarized-dark" | "solarized_dark" => Some(solarized_dark()),
        "solarized-light" | "solarized_light" => Some(solarized_light()),
        _ => None,
    }
}

/// Names of all built-in themes.
pub fn builtin_names() -> &'static [&'static str] {
    &[
        "mrxvt",
        "tokyo-night",
        "gruvbox",
        "dracula",
        "solarized-dark",
        "solarized-light",
    ]
}

// ─── Preset definitions ──────────────────────────────────────────────────────

fn mrxvt() -> Theme {
    // Classic mrxvt: green-on-black with the standard 16 ANSI colors.
    Theme {
        name: "mrxvt".into(),
        bg: Color::new(5, 10, 5),
        fg: Color::new(178, 238, 138),
        cursor: Color::new(178, 238, 138),
        black: Color::new(0, 0, 0),
        red: Color::new(205, 0, 0),
        green: Color::new(0, 205, 0),
        yellow: Color::new(205, 205, 0),
        blue: Color::new(0, 0, 238),
        magenta: Color::new(205, 0, 205),
        cyan: Color::new(0, 205, 205),
        white: Color::new(229, 229, 229),
        bright_black: Color::new(127, 127, 127),
        bright_red: Color::new(255, 0, 0),
        bright_green: Color::new(0, 255, 0),
        bright_yellow: Color::new(255, 255, 0),
        bright_blue: Color::new(92, 92, 255),
        bright_magenta: Color::new(255, 0, 255),
        bright_cyan: Color::new(0, 255, 255),
        bright_white: Color::new(255, 255, 255),
    }
}

fn tokyo_night() -> Theme {
    // https://github.com/enkia/tokyo-night-vscode-style
    Theme {
        name: "tokyo-night".into(),
        bg: Color::new(0x1a, 0x1b, 0x26),
        fg: Color::new(0xa9, 0xb1, 0xd6),
        cursor: Color::new(0xc0, 0xca, 0xf5),
        black: Color::new(0x15, 0x16, 0x1e),
        red: Color::new(0xf7, 0x76, 0x8e),
        green: Color::new(0x9e, 0xce, 0x6a),
        yellow: Color::new(0xe0, 0xaf, 0x68),
        blue: Color::new(0x7a, 0xa2, 0xf7),
        magenta: Color::new(0xbb, 0x9a, 0xf7),
        cyan: Color::new(0x7d, 0xcf, 0xff),
        white: Color::new(0xa9, 0xb1, 0xd6),
        bright_black: Color::new(0x41, 0x48, 0x68),
        bright_red: Color::new(0xff, 0x7a, 0x93),
        bright_green: Color::new(0xb9, 0xf2, 0x7c),
        bright_yellow: Color::new(0xff, 0xc7, 0x77),
        bright_blue: Color::new(0x7d, 0xae, 0xff),
        bright_magenta: Color::new(0xbb, 0x9a, 0xf7),
        bright_cyan: Color::new(0x0d, 0xb9, 0xd7),
        bright_white: Color::new(0xac, 0xb0, 0xd0),
    }
}

fn gruvbox() -> Theme {
    // https://github.com/morhetz/gruvbox
    Theme {
        name: "gruvbox".into(),
        bg: Color::new(0x28, 0x28, 0x28),
        fg: Color::new(0xeb, 0xdb, 0xb2),
        cursor: Color::new(0xeb, 0xdb, 0xb2),
        black: Color::new(0x28, 0x28, 0x28),
        red: Color::new(0xcc, 0x24, 0x1d),
        green: Color::new(0x98, 0x97, 0x1a),
        yellow: Color::new(0xd7, 0x99, 0x21),
        blue: Color::new(0x45, 0x85, 0x88),
        magenta: Color::new(0xb1, 0x62, 0x86),
        cyan: Color::new(0x68, 0x9d, 0x6a),
        white: Color::new(0xa8, 0x99, 0x84),
        bright_black: Color::new(0x92, 0x83, 0x74),
        bright_red: Color::new(0xfb, 0x49, 0x34),
        bright_green: Color::new(0xb8, 0xbb, 0x26),
        bright_yellow: Color::new(0xfa, 0xbd, 0x2f),
        bright_blue: Color::new(0x83, 0xa5, 0x98),
        bright_magenta: Color::new(0xd3, 0x86, 0x9b),
        bright_cyan: Color::new(0x8e, 0xc0, 0x7c),
        bright_white: Color::new(0xeb, 0xdb, 0xb2),
    }
}

fn dracula() -> Theme {
    // https://github.com/dracula/dracula-theme
    Theme {
        name: "dracula".into(),
        bg: Color::new(0x28, 0x2a, 0x36),
        fg: Color::new(0xf8, 0xf8, 0xf2),
        cursor: Color::new(0xbb, 0xc5, 0xff),
        black: Color::new(0x00, 0x00, 0x00),
        red: Color::new(0xff, 0x55, 0x55),
        green: Color::new(0x50, 0xfa, 0x7b),
        yellow: Color::new(0xf1, 0xfa, 0x8c),
        blue: Color::new(0xbd, 0x93, 0xf9),
        magenta: Color::new(0xff, 0x79, 0xc6),
        cyan: Color::new(0x8b, 0xe9, 0xfd),
        white: Color::new(0xbf, 0xbf, 0xbf),
        bright_black: Color::new(0x4d, 0x4d, 0x4d),
        bright_red: Color::new(0xff, 0x6e, 0x67),
        bright_green: Color::new(0x5a, 0xf7, 0x8e),
        bright_yellow: Color::new(0xf4, 0xf9, 0x9d),
        bright_blue: Color::new(0xca, 0xa9, 0xfa),
        bright_magenta: Color::new(0xff, 0x92, 0xd0),
        bright_cyan: Color::new(0x9a, 0xed, 0xfe),
        bright_white: Color::new(0xe6, 0xe6, 0xe6),
    }
}

fn solarized_dark() -> Theme {
    // https://github.com/altercation/solarized
    Theme {
        name: "solarized-dark".into(),
        bg: Color::new(0x00, 0x2b, 0x36),
        fg: Color::new(0x83, 0x94, 0x96),
        cursor: Color::new(0x83, 0x94, 0x96),
        black: Color::new(0x07, 0x36, 0x42),
        red: Color::new(0xdc, 0x32, 0x2f),
        green: Color::new(0x85, 0x99, 0x00),
        yellow: Color::new(0xb5, 0x89, 0x00),
        blue: Color::new(0x26, 0x8b, 0xd2),
        magenta: Color::new(0xd3, 0x36, 0x82),
        cyan: Color::new(0x2a, 0xa1, 0x98),
        white: Color::new(0xee, 0xe8, 0xd5),
        bright_black: Color::new(0x00, 0x29, 0x4d),
        bright_red: Color::new(0xcb, 0x4b, 0x16),
        bright_green: Color::new(0x58, 0x6e, 0x75),
        bright_yellow: Color::new(0x65, 0x7b, 0x83),
        bright_blue: Color::new(0x83, 0x94, 0x96),
        bright_magenta: Color::new(0x6c, 0x71, 0xc4),
        bright_cyan: Color::new(0x93, 0xa1, 0xa1),
        bright_white: Color::new(0xfd, 0xf6, 0xe3),
    }
}

fn solarized_light() -> Theme {
    Theme {
        name: "solarized-light".into(),
        bg: Color::new(0xfd, 0xf6, 0xe3),
        fg: Color::new(0x65, 0x7b, 0x83),
        cursor: Color::new(0x65, 0x7b, 0x83),
        black: Color::new(0x07, 0x36, 0x42),
        red: Color::new(0xdc, 0x32, 0x2f),
        green: Color::new(0x85, 0x99, 0x00),
        yellow: Color::new(0xb5, 0x89, 0x00),
        blue: Color::new(0x26, 0x8b, 0xd2),
        magenta: Color::new(0xd3, 0x36, 0x82),
        cyan: Color::new(0x2a, 0xa1, 0x98),
        white: Color::new(0xee, 0xe8, 0xd5),
        bright_black: Color::new(0x00, 0x29, 0x4d),
        bright_red: Color::new(0xcb, 0x4b, 0x16),
        bright_green: Color::new(0x58, 0x6e, 0x75),
        bright_yellow: Color::new(0x65, 0x7b, 0x83),
        bright_blue: Color::new(0x83, 0x94, 0x96),
        bright_magenta: Color::new(0x6c, 0x71, 0xc4),
        bright_cyan: Color::new(0x93, 0xa1, 0xa1),
        bright_white: Color::new(0xfd, 0xf6, 0xe3),
    }
}

/// Resolve a theme by name, with optional custom themes from config.
///
/// Custom themes take precedence over built-ins of the same name; a custom
/// theme without an explicit `name` field inherits the key it was defined
/// under.
pub fn resolve(name: &str, custom: &HashMap<String, Theme>) -> Option<Theme> {
    if let Some(t) = custom.get(name) {
        let mut t = t.clone();
        if t.name.is_empty() {
            t.name = name.to_string();
        }
        return Some(t);
    }
    builtin(name)
}

/// The fallback theme used when a configured name doesn't resolve.
pub fn default_theme() -> Theme {
    builtin("mrxvt").expect("built-in mrxvt theme must exist")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_themes_load() {
        for &name in builtin_names() {
            let t = builtin(name).unwrap_or_else(|| panic!("theme {name} should load"));
            assert_eq!(t.name, name);
            // Sanity: bg and fg should be different.
            assert_ne!(t.bg, t.fg);
        }
    }

    #[test]
    fn unknown_theme_returns_none() {
        assert!(builtin("nonexistent").is_none());
    }

    #[test]
    fn ansi_lookup_works() {
        let t = tokyo_night();
        assert_eq!(t.ansi(0), t.black);
        assert_eq!(t.ansi(1), t.red);
        assert_eq!(t.ansi(15), t.bright_white);
    }

    #[test]
    fn ansi_out_of_range_returns_fg() {
        let t = mrxvt();
        assert_eq!(t.ansi(99), t.fg);
    }

    #[test]
    fn color_parse_hex() {
        let c = Color::parse("#FF8040").unwrap();
        assert_eq!(c, Color::new(255, 128, 64));
    }

    #[test]
    fn color_parse_without_hash() {
        let c = Color::parse("00FF00").unwrap();
        assert_eq!(c, Color::new(0, 255, 0));
    }

    #[test]
    fn color_to_hex_roundtrip() {
        let c = Color::new(0xAB, 0xCD, 0xEF);
        let hex = c.to_hex();
        assert_eq!(hex, "#ABCDEF");
        let c2 = Color::parse(&hex).unwrap();
        assert_eq!(c, c2);
    }

    #[test]
    fn color_parse_rejects_garbage() {
        assert!(Color::parse("garbage").is_none());
        assert!(Color::parse("#XYZ").is_none());
        assert!(Color::parse("").is_none());
    }

    #[test]
    fn custom_theme_overrides_builtin() {
        let mut custom = HashMap::new();
        custom.insert(
            "mrxvt".into(),
            Theme {
                name: "mrxvt".into(),
                bg: Color::new(0, 0, 0),
                fg: Color::new(255, 255, 255),
                cursor: Color::new(255, 255, 255),
                black: Color::new(0, 0, 0),
                red: Color::new(255, 0, 0),
                green: Color::new(0, 255, 0),
                yellow: Color::new(255, 255, 0),
                blue: Color::new(0, 0, 255),
                magenta: Color::new(255, 0, 255),
                cyan: Color::new(0, 255, 255),
                white: Color::new(255, 255, 255),
                bright_black: Color::new(64, 64, 64),
                bright_red: Color::new(255, 64, 64),
                bright_green: Color::new(64, 255, 64),
                bright_yellow: Color::new(255, 255, 64),
                bright_blue: Color::new(64, 64, 255),
                bright_magenta: Color::new(255, 64, 255),
                bright_cyan: Color::new(64, 255, 255),
                bright_white: Color::new(255, 255, 255),
            },
        );
        let resolved = resolve("mrxvt", &custom).unwrap();
        assert_eq!(resolved.bg, Color::new(0, 0, 0));
        assert_ne!(resolved.bg, builtin("mrxvt").unwrap().bg);
    }

    #[test]
    fn resolve_falls_back_to_builtin() {
        let custom = HashMap::new();
        let resolved = resolve("gruvbox", &custom).unwrap();
        assert_eq!(resolved.name, "gruvbox");
    }

    #[test]
    fn resolve_unknown_returns_none() {
        let custom = HashMap::new();
        assert!(resolve("nonexistent", &custom).is_none());
    }

    #[test]
    fn theme_serializes_to_toml() {
        let t = dracula();
        let s = toml::to_string(&t).unwrap();
        assert!(s.contains("name = \"dracula\""));
        let parsed: Theme = toml::from_str(&s).unwrap();
        assert_eq!(parsed, t);
    }

    #[test]
    fn theme_alternate_name_normalization() {
        // Both "tokyo-night" and "tokyo_night" should resolve.
        assert!(builtin("tokyo-night").is_some());
        assert!(builtin("tokyo_night").is_some());
    }

    #[test]
    fn all_themes_have_distinct_bg_fg() {
        // A theme where bg == fg would be unusable.
        for &name in builtin_names() {
            let t = builtin(name).unwrap();
            assert_ne!(t.bg, t.fg, "theme {name} has bg == fg");
        }
    }
}
