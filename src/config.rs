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


//! Configuration loader.
//!
//! Format: TOML. Stored at `~/.config/rs-mrxvt/config.toml` by default
//! (overridable with `--config` or `$MRXVT_CONFIG`).
//!
//! The original mrxvt used X-resources; we deliberately use TOML because
//! (a) it's distro-agnostic and doesn't require an X server, and
//! (b) it round-trips cleanly with serde.
//!
//! A Lua layer (`config.lua`) is planned as a future enhancement — see
//! `ARCHITECTURE.md` for how the trait-based [`ConfigSource`] design
//! accommodates that without forcing a hard dependency on `mlua`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Top-level config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub terminal: TerminalConfig,
    #[serde(default)]
    pub profiles: HashMap<String, Profile>,
    /// Default profile name used by `--exec`-less tabs.
    #[serde(default = "default_profile")]
    pub default_profile: String,
    /// Macro table: keybinding chord → command name.
    ///
    /// Example: `"Ctrl+Shift+R" = "ResetTerminal"`
    #[serde(default)]
    pub macros: HashMap<String, String>,
    /// User-defined themes: `[custom_themes.<name>]` tables with the full
    /// palette (16 ANSI colors + bg/fg/cursor). `ui.theme` references them
    /// by name; a custom theme overrides a built-in of the same name.
    #[serde(default)]
    pub custom_themes: HashMap<String, crate::theme::Theme>,
    /// Pseudo-transparency + tinting configuration.
    ///
    /// Only effective when built with `--features gpu` and using a GUI
    /// backend (wgpu or softbuffer). Ignored by the TUI backend.
    #[serde(default)]
    pub transparency: TransparencyConfig,
    /// Clipboard (copy/paste) configuration.
    #[serde(default)]
    pub clipboard: ClipboardConfig,
    /// GPU detection and rendering configuration.
    ///
    /// Controls how the wgpu backend probes for hardware and what
    /// happens when no suitable GPU is found. Only effective when built
    /// with `--features gpu`.
    #[serde(default)]
    pub gpu: GpuConfig,
}

/// Clipboard behavior configuration (`[clipboard]` in config.toml).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardConfig {
    /// Copy the selection to the clipboard as soon as the mouse button is
    /// released (classic xterm/mrxvt behavior).
    #[serde(default = "default_true")]
    pub copy_on_select: bool,
    /// Mirror every copy to the host terminal via OSC 52. This is what
    /// makes copy work over SSH — the *client's* terminal performs the
    /// clipboard write.
    #[serde(default = "default_true")]
    pub osc52: bool,
    /// Allow programs inside the terminal to *read* the clipboard via
    /// OSC 52 queries (DECSET-style paste requests from tmux/nvim).
    /// Disabled by default, matching xterm's secure default; copying
    /// *out* is always allowed.
    #[serde(default)]
    pub osc52_read: bool,
    /// Explicit clipboard tool to use (`"xclip"`, `"xsel"`, `"wl-copy"`).
    /// Empty = auto-detect in priority order.
    #[serde(default)]
    pub tool: Option<String>,
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self {
            copy_on_select: true,
            osc52: true,
            osc52_read: false,
            tool: None,
        }
    }
}

/// Transparency config (re-exported here so users don't need to import from
/// the ui module when writing config files).
#[cfg(feature = "gpu")]
pub type TransparencyConfig = crate::ui::transparency::TransparencyConfig;

/// Stub type when the gpu feature is off — keeps the config schema the same.
#[cfg(not(feature = "gpu"))]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransparencyConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_tint_stub")]
    pub tint: String,
    #[serde(default = "default_opacity_stub")]
    pub opacity: f32,
    #[serde(default)]
    pub background_image: Option<String>,
}

#[cfg(not(feature = "gpu"))]
fn default_tint_stub() -> String { "#000000".into() }

#[cfg(not(feature = "gpu"))]
fn default_opacity_stub() -> f32 { 1.0 }

/// GPU detection and rendering configuration.
///
/// All fields have sensible defaults. Users only need to set these if
/// they want to override the auto-detection behavior.
///
/// Example config.toml:
///
/// ```toml
/// [gpu]
/// preferred_backend = "vulkan,gl"
/// accept_software_rasterizer = false
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GpuConfig {
    /// Comma-separated list of preferred wgpu backends, tried in order.
    ///
    /// Valid values: "vulkan", "metal", "dx12", "gl".
    /// Empty string (default) = use the built-in order: vulkan → metal → dx12 → gl.
    #[serde(default)]
    pub preferred_backend: Option<String>,

    /// Accept CPU software rasterizers (llvmpipe, swiftshader) as valid GPUs.
    ///
    /// When `false` (default), a software rasterizer adapter is rejected
    /// and the probe continues to the next backend. Set to `true` if you
    /// want to use the GPU pipeline even on headless/VM systems where the
    /// only "GPU" is a CPU-based Vulkan implementation.
    #[serde(default)]
    pub accept_software_rasterizer: bool,

    /// Force wgpu to use its built-in software fallback adapter.
    ///
    /// This bypasses ALL hardware probes and renders via CPU. Useful for
    /// debugging the wgpu pipeline without a real GPU. Implies
    /// `accept_software_rasterizer = true`.
    #[serde(default)]
    pub force_fallback_adapter: bool,

    /// Require a minimum maximum texture dimension (2D).
    ///
    /// If the detected adapter's max_texture_dimension_2d is below this
    /// value, the adapter is rejected. The glyph atlas needs at least
    /// the atlas size. 0 = no minimum (default).
    #[serde(default)]
    pub min_texture_size: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ui: UiConfig::default(),
            terminal: TerminalConfig::default(),
            profiles: HashMap::new(),
            default_profile: default_profile(),
            macros: HashMap::new(),
            custom_themes: HashMap::new(),
            transparency: TransparencyConfig::default(),
            clipboard: ClipboardConfig::default(),
            gpu: GpuConfig::default(),
        }
    }
}

fn default_profile() -> String {
    "default".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    /// Mouse focus model: "click" (default) or "follow".
    #[serde(default = "default_focus")]
    pub focus: String,
    /// Show the tab bar even when only one tab is open.
    #[serde(default = "default_true")]
    pub always_show_tabs: bool,
    /// Disable the command palette overlay entirely.
    #[serde(default)]
    pub disable_palette: bool,
    /// Tab bar height in terminal rows.
    #[serde(default = "default_tabbar_height")]
    pub tabbar_height: u16,
    /// Theme: "mrxvt" (classic green-on-black), "tokyo-night", "gruvbox".
    #[serde(default = "default_theme")]
    pub theme: String,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            focus: default_focus(),
            always_show_tabs: default_true(),
            disable_palette: false,
            tabbar_height: default_tabbar_height(),
            theme: default_theme(),
        }
    }
}

fn default_focus() -> String { "click".into() }
fn default_true() -> bool { true }
pub(crate) fn default_tabbar_height() -> u16 { 1 }
fn default_theme() -> String { "mrxvt".into() }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalConfig {
    /// Initial columns.
    #[serde(default = "default_cols")]
    pub cols: u16,
    /// Initial rows.
    #[serde(default = "default_rows")]
    pub rows: u16,
    /// Scrollback lines kept in memory per tab.
    #[serde(default = "default_scrollback")]
    pub scrollback: usize,
    /// Shell to launch when no profile overrides it.
    #[serde(default = "default_shell")]
    pub shell: String,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            cols: default_cols(),
            rows: default_rows(),
            scrollback: default_scrollback(),
            shell: default_shell(),
        }
    }
}

fn default_cols() -> u16 { 80 }
fn default_rows() -> u16 { 24 }
fn default_scrollback() -> usize { 10_000 }
pub(crate) fn default_shell() -> String {
    // Prefer bash explicitly — it's the most common default shell across
    // distros. Fall back to $SHELL, then /bin/sh.
    if let Some(candidate) = ["/bin/bash", "/usr/bin/bash"]
        .iter()
        .find(|c| std::path::Path::new(c).exists())
    {
        return candidate.to_string();
    }
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())
}

/// A named profile. The `default` profile is consulted when no `--exec` is given.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    /// Command line (split on whitespace; no shell semantics).
    /// If empty, falls back to `terminal.shell`.
    #[serde(default)]
    pub command: Vec<String>,
    /// Working directory.
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    /// Optional tag for grouped broadcasting.
    #[serde(default)]
    pub tag: Option<String>,
    /// Environment overrides (`KEY=value`). Applied after the inherited
    /// environment and after the built-in `TERM`/`COLORTERM` defaults, so
    /// profiles can override them.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// When `command` is empty (default shell), run it as a **login shell**
    /// (argv0 `-bash`). Login shells source `/etc/profile` and
    /// `~/.bash_profile`, which is where distros export the GUI session
    /// environment (D-Bus address, flatpak/snap paths, XDG data dirs).
    /// This is the classic mrxvt default (`loginShell: True`) and it is
    /// what makes GUI apps launched from rs-mrxvt find their display
    /// environment. Only meaningful for the default shell.
    #[serde(default = "default_true")]
    pub login_shell: bool,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            command: Vec::new(),
            cwd: None,
            tag: None,
            env: HashMap::new(),
            login_shell: true,
        }
    }
}

/// Abstraction over config sources so we can later swap TOML for Lua without
/// touching the rest of the codebase.
pub trait ConfigSource {
    fn load(&self) -> Result<Config>;
}

/// Filesystem-backed TOML config.
pub struct FileConfigSource {
    pub path: PathBuf,
}

impl ConfigSource for FileConfigSource {
    fn load(&self) -> Result<Config> {
        if !self.path.exists() {
            return Ok(Config::default());
        }
        let raw = std::fs::read_to_string(&self.path)
            .with_context(|| format!("reading config {}", self.path.display()))?;
        let cfg: Config = toml::from_str(&raw)
            .with_context(|| format!("parsing config {}", self.path.display()))?;
        Ok(cfg)
    }
}

impl Config {
    /// Resolve the default config path: `$MRXVT_CONFIG` or `~/.config/rs-mrxvt/config.toml`.
    pub fn default_path() -> PathBuf {
        if let Ok(p) = std::env::var("MRXVT_CONFIG") {
            return PathBuf::from(p);
        }
        let xdg = std::env::var("XDG_CONFIG_HOME").ok().filter(|s| !s.is_empty());
        let base = xdg.map(PathBuf::from).unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".config")
        });
        base.join("rs-mrxvt").join("config.toml")
    }

    /// Load config from a specific path (or the default if `path` is `None`).
    ///
    /// When the `lua` feature is enabled, this delegates to
    /// [`config_lua::load_config_smart`] which picks the parser based on
    /// file extension (`.lua` → Lua, `.toml` or anything else → TOML).
    pub fn load(path: Option<&Path>) -> Result<Self> {
        #[cfg(feature = "lua")]
        return crate::config_lua::load_config_smart(path);

        #[cfg(not(feature = "lua"))]
        {
            let path = path.map(|p| p.to_path_buf()).unwrap_or_else(Self::default_path);
            let expanded = PathBuf::from(shellexpand::tilde(&path.to_string_lossy()).to_string());
            FileConfigSource { path: expanded }.load()
        }
    }

    /// Look up a profile by name, falling back to `default` then a synthesized
    /// empty profile.
    pub fn profile(&self, name: &str) -> Profile {
        self.profiles.get(name).cloned().unwrap_or_else(|| match name {
            "default" => Profile::default(),
            _other => Profile {
                command: vec![],
                cwd: None,
                tag: None,
                env: HashMap::new(),
                login_shell: true,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_config() {
        let toml = r#"
[terminal]
cols = 120
rows = 40

[profiles.default]
command = ["bash"]
"#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.terminal.cols, 120);
        assert_eq!(cfg.terminal.rows, 40);
        assert_eq!(cfg.profiles["default"].command, vec!["bash".to_string()]);
    }

    #[test]
    fn defaults_are_sane() {
        let cfg = Config::default();
        assert!(cfg.terminal.cols >= 80);
        assert!(cfg.terminal.rows >= 24);
        assert!(cfg.terminal.scrollback > 0);
        assert!(!cfg.terminal.shell.is_empty());
        assert_eq!(cfg.default_profile, "default");
    }

    #[test]
    fn missing_file_yields_default() {
        let cfg = Config::load(Some(Path::new("/nonexistent/rs-mrxvt.toml"))).unwrap();
        assert_eq!(cfg.terminal.cols, default_cols());
    }

    #[test]
    fn profile_fallback_chain() {
        let cfg = Config::default();
        // Unknown profile → empty profile (does not panic).
        let p = cfg.profile("does-not-exist");
        assert!(p.command.is_empty());
        // "default" on an empty config → empty profile.
        let p = cfg.profile("default");
        assert!(p.command.is_empty());
    }

    #[test]
    fn macros_round_trip() {
        let toml = r#"
[macros]
"Ctrl+Shift+R" = "ResetTerminal"
"Ctrl+Shift+Q" = "Quit"
"#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.macros.get("Ctrl+Shift+R").unwrap(), "ResetTerminal");
    }

    #[test]
    fn custom_themes_parse_and_resolve() {
        let toml = r##"
[ui]
theme = "midnight"

[custom_themes.midnight]
bg = "#0a0e14"
fg = "#c5cdd7"
cursor = "#c5cdd7"
black = "#0a0e14"
red = "#f07178"
green = "#86b300"
yellow = "#f2ae49"
blue = "#399ee6"
magenta = "#a37acc"
cyan = "#4cbf99"
white = "#c5cdd7"
bright_black = "#565b66"
bright_red = "#f07178"
bright_green = "#86b300"
bright_yellow = "#f2ae49"
bright_blue = "#399ee6"
bright_magenta = "#a37acc"
bright_cyan = "#4cbf99"
bright_white = "#ffffff"
"##;
        let cfg: Config = toml::from_str(toml).unwrap();
        let resolved = crate::theme::resolve(&cfg.ui.theme, &cfg.custom_themes);
        assert!(resolved.is_some(), "custom theme must resolve");
        assert_eq!(resolved.unwrap().bg.to_hex(), "#0A0E14");
    }

    #[test]
    fn gpu_config_defaults() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.gpu.preferred_backend.is_none());
        assert!(!cfg.gpu.accept_software_rasterizer);
        assert!(!cfg.gpu.force_fallback_adapter);
        assert_eq!(cfg.gpu.min_texture_size, 0);
    }

    #[test]
    fn gpu_config_parses_all_fields() {
        let toml = r#"
[gpu]
preferred_backend = "gl,vulkan"
accept_software_rasterizer = true
force_fallback_adapter = true
min_texture_size = 4096
"#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.gpu.preferred_backend.as_deref(), Some("gl,vulkan"));
        assert!(cfg.gpu.accept_software_rasterizer);
        assert!(cfg.gpu.force_fallback_adapter);
        assert_eq!(cfg.gpu.min_texture_size, 4096);
    }
}
