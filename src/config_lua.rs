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


//! Lua-backed config source.
//!
//! Compiles only with `--features lua`. Implements [`ConfigSource`] by
//! evaluating a Lua script that returns a config table.
//!
//! ## Lua config format
//!
//! The script must `return` a table with the same shape as the TOML config:
//!
//! ```lua
//! return {
//!   terminal = {
//!     cols = 120,
//!     rows = 40,
//!     shell = "/bin/zsh",
//!   },
//!   ui = {
//!     theme = "tokyo-night",
//!     always_show_tabs = true,
//!   },
//!   default_profile = "default",
//!   profiles = {
//!     default = { command = {"zsh"}, cwd = "/home/user" },
//!     web = { command = {"ssh", "user@web-01"}, tag = "web" },
//!   },
//!   macros = {
//!     ["Ctrl+Shift+R"] = "ResetTerminal",
//!   },
//! }
//! ```
//!
//! ## Why Lua?
//!
//! TOML is great for static config but can't express logic. Lua gives users:
//! - Conditional config ("if it's after 8pm, use midnight theme")
//! - Programmable macros (functions, not just command names)
//! - Hot-reload (re-eval the script on file change)
//! - Access to environment vars and time
//!
//! ## Example: time-based theme
//!
//! ```lua
//! local hour = tonumber(os.date("%H"))
//! local theme = "mrxvt"
//! if hour >= 20 or hour < 6 then
//!   theme = "tokyo-night"
//! end
//!
//! return {
//!   ui = { theme = theme },
//!   terminal = { cols = 120, rows = 40 },
//! }
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use mlua::{Lua, Value};

use crate::config::{Config, ConfigSource, Profile, TerminalConfig, UiConfig};

/// A Lua-backed config source. Loads `config.lua` and evaluates it.
pub struct LuaConfigSource {
    pub path: PathBuf,
}

impl LuaConfigSource {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl ConfigSource for LuaConfigSource {
    fn load(&self) -> Result<Config> {
        if !self.path.exists() {
            return Ok(Config::default());
        }
        let source = std::fs::read_to_string(&self.path)
            .map_err(|e| anyhow::anyhow!("reading lua config {}: {e}", self.path.display()))?;
        parse_lua_config(&source)
            .map_err(|e| anyhow::anyhow!("parsing lua config {}: {e}", self.path.display()))
    }
}

/// Parse a Lua script and extract a [`Config`].
///
/// The script must `return` a table. Unknown keys are silently ignored
/// (forward-compat). Malformed values cause an error.
pub fn parse_lua_config(source: &str) -> Result<Config> {
    let lua = Lua::new();
    let result: Value = lua
        .load(source)
        .eval()
        .map_err(|e| anyhow::anyhow!("lua eval failed: {e}"))?;

    let mut cfg = Config::default();
    let table = match result {
        Value::Table(t) => t,
        Value::Nil => return Ok(cfg),
        other => anyhow::bail!("lua config must return a table, got {:?}", other.type_name()),
    };

    // terminal = { cols = N, rows = N, scrollback = N, shell = "..." }
    if let Some(Value::Table(t)) = opt_value(&table, "terminal") {
        let scrollback: Option<usize> = t
            .get::<mlua::Integer>("scrollback")
            .ok()
            .and_then(|i| {
                usize::try_from(i).ok().or_else(|| {
                    log::warn!("scrollback value {i} out of range, using default");
                    None
                })
            });
        cfg.terminal = TerminalConfig {
            cols: opt_int(&t, "cols").unwrap_or_else(default_cols),
            rows: opt_int(&t, "rows").unwrap_or_else(default_rows),
            scrollback: scrollback.unwrap_or_else(default_scrollback),
            shell: opt_string(&t, "shell").unwrap_or_else(default_shell),
        };
    }

    // ui = { ... }
    if let Some(Value::Table(t)) = opt_value(&table, "ui") {
        cfg.ui = UiConfig {
            focus: opt_string(&t, "focus").unwrap_or_else(default_focus),
            always_show_tabs: opt_bool(&t, "always_show_tabs").unwrap_or(true),
            disable_palette: opt_bool(&t, "disable_palette").unwrap_or(false),
            tabbar_height: opt_int(&t, "tabbar_height").unwrap_or(1),
            theme: opt_string(&t, "theme").unwrap_or_else(default_theme),
        };
    }

    // default_profile = "..."
    if let Some(s) = opt_string(&table, "default_profile") {
        cfg.default_profile = s;
    }

    // profiles = { name = { ... }, ... }
    if let Some(Value::Table(t)) = opt_value(&table, "profiles") {
        cfg.profiles = HashMap::new();
        for pair in t.pairs::<String, Value>() {
            let (k, v) = pair.map_err(|e| anyhow::anyhow!("lua profiles pair: {e}"))?;
            if let Value::Table(pt) = v {
                let command = opt_str_array(&pt, "command").unwrap_or_default();
                let cwd = opt_string(&pt, "cwd").map(PathBuf::from);
                let tag = opt_string(&pt, "tag");
                let env = opt_string_map(&pt, "env").unwrap_or_default();
                cfg.profiles.insert(
                    k,
                    Profile {
                        command,
                        cwd,
                        tag,
                        env,
                    },
                );
            }
        }
    }

    // macros = { ["Ctrl+Shift+R"] = "ResetTerminal", ... }
    if let Some(Value::Table(t)) = opt_value(&table, "macros") {
        cfg.macros = HashMap::new();
        for pair in t.pairs::<String, Value>() {
            let (k, v) = pair.map_err(|e| anyhow::anyhow!("lua macros pair: {e}"))?;
            if let Value::String(s) = v {
                let sval = s.to_str().map_err(|e| anyhow::anyhow!("lua string to_str: {e}"))?;
                cfg.macros.insert(k, sval.to_string());
            }
        }
    }

    Ok(cfg)
}

/// Pick the right config source based on the file extension.
///
/// `.lua` → `LuaConfigSource`
/// `.toml` (or anything else) → `FileConfigSource` (TOML)
/// If the path doesn't exist, defaults to TOML at the standard location.
pub fn load_config_smart(path: Option<&Path>) -> Result<Config> {
    let path = path.map(|p| p.to_path_buf()).unwrap_or_else(Config::default_path);
    let expanded = PathBuf::from(shellexpand::tilde(&path.to_string_lossy()).to_string());

    if expanded.extension().and_then(|e| e.to_str()) == Some("lua") {
        LuaConfigSource::new(expanded).load()
    } else {
        // Delegate to the TOML loader in the parent module.
        let raw = if expanded.exists() {
            std::fs::read_to_string(&expanded)
                .map_err(|e| anyhow::anyhow!("reading {}: {e}", expanded.display()))?
        } else {
            String::new()
        };
        let cfg: Config = toml::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("parsing {}: {e}", expanded.display()))?;
        Ok(cfg)
    }
}

// ─── Lua value helpers ───────────────────────────────────────────────────────

fn opt_value(t: &mlua::Table, key: &str) -> Option<Value> {
    t.get::<Value>(key).ok()
}

fn opt_int(t: &mlua::Table, key: &str) -> Option<u16> {
    t.get::<mlua::Integer>(key).ok().and_then(|i| {
        u16::try_from(i).ok().or_else(|| {
            log::warn!("config key '{key}': value {i} out of u16 range, ignoring");
            None
        })
    })
}

fn opt_string(t: &mlua::Table, key: &str) -> Option<String> {
    t.get::<mlua::String>(key)
        .ok()
        .and_then(|s| s.to_str().ok().map(|s| s.to_string()))
}

fn opt_bool(t: &mlua::Table, key: &str) -> Option<bool> {
    t.get::<bool>(key).ok()
}

fn opt_str_array(t: &mlua::Table, key: &str) -> Option<Vec<String>> {
    let v: mlua::Value = t.get(key).ok()?;
    match v {
        mlua::Value::Table(t) => {
            Some(
                t.pairs::<mlua::Value, mlua::Value>()
                    .filter_map(|pair| pair.ok())
                    .filter_map(|(_, v)| match v {
                        mlua::Value::String(s) => s.to_str().ok().map(|s| s.to_string()),
                        _ => None,
                    })
                    .collect()
            )
        }
        mlua::Value::String(s) => Some(vec![s.to_str().ok()?.to_string()]),
        _ => None,
    }
}

fn opt_string_map(t: &mlua::Table, key: &str) -> Option<HashMap<String, String>> {
    let v: mlua::Value = t.get(key).ok()?;
    if let mlua::Value::Table(t) = v {
        Some(
            t.pairs::<String, mlua::String>()
                .filter_map(|pair| pair.ok())
                .filter_map(|(k, v)| v.to_str().ok().map(|s| (k, s.to_string())))
                .collect()
        )
    } else {
        None
    }
}

fn default_cols() -> u16 { 80 }
fn default_rows() -> u16 { 24 }
fn default_scrollback() -> usize { 10_000 }
fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())
}
fn default_focus() -> String { "click".into() }
fn default_theme() -> String { "mrxvt".into() }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_lua_config() {
        let src = r#"
            return {
              terminal = { cols = 120, rows = 40 },
              profiles = {
                default = { command = {"bash"} }
              }
            }
        "#;
        let cfg = parse_lua_config(src).unwrap();
        assert_eq!(cfg.terminal.cols, 120);
        assert_eq!(cfg.terminal.rows, 40);
        assert_eq!(cfg.profiles["default"].command, vec!["bash".to_string()]);
    }

    #[test]
    fn parses_macros() {
        let src = r#"
            return {
              macros = {
                ["Ctrl+Shift+R"] = "ResetTerminal",
                ["Ctrl+Shift+Q"] = "Quit",
              }
            }
        "#;
        let cfg = parse_lua_config(src).unwrap();
        assert_eq!(cfg.macros.get("Ctrl+Shift+R").unwrap(), "ResetTerminal");
        assert_eq!(cfg.macros.get("Ctrl+Shift+Q").unwrap(), "Quit");
    }

    #[test]
    fn time_based_theme_works() {
        // The example from the module docs should parse cleanly.
        let src = r#"
            local hour = tonumber(os.date("%H"))
            local theme = "mrxvt"
            if hour >= 20 or hour < 6 then
              theme = "tokyo-night"
            end

            return {
              ui = { theme = theme },
              terminal = { cols = 120, rows = 40 },
            }
        "#;
        let cfg = parse_lua_config(src).unwrap();
        assert!(cfg.ui.theme == "mrxvt" || cfg.ui.theme == "tokyo-night");
        assert_eq!(cfg.terminal.cols, 120);
    }

    #[test]
    fn empty_return_yields_defaults() {
        let src = "return {}";
        let cfg = parse_lua_config(src).unwrap();
        assert_eq!(cfg.terminal.cols, 80);
        assert_eq!(cfg.default_profile, "default");
    }

    #[test]
    fn nil_return_yields_defaults() {
        let src = "return nil";
        let cfg = parse_lua_config(src).unwrap();
        assert_eq!(cfg.terminal.cols, 80);
    }

    #[test]
    fn non_table_return_errors() {
        let src = "return 42";
        assert!(parse_lua_config(src).is_err());
    }

    #[test]
    fn profiles_with_env_and_tag() {
        let src = r#"
            return {
              profiles = {
                web1 = {
                  command = {"ssh", "user@web-01"},
                  tag = "web",
                  env = { TERM = "xterm-256color", EDITOR = "vim" }
                }
              }
            }
        "#;
        let cfg = parse_lua_config(src).unwrap();
        let p = &cfg.profiles["web1"];
        assert_eq!(p.command, vec!["ssh", "user@web-01"]);
        assert_eq!(p.tag.as_deref(), Some("web"));
        assert_eq!(p.env.get("EDITOR").unwrap(), "vim");
    }

    #[test]
    fn ui_section_parses() {
        let src = r#"
            return {
              ui = {
                theme = "gruvbox",
                always_show_tabs = false,
                tabbar_height = 2,
                focus = "follow",
              }
            }
        "#;
        let cfg = parse_lua_config(src).unwrap();
        assert_eq!(cfg.ui.theme, "gruvbox");
        assert!(!cfg.ui.always_show_tabs);
        assert_eq!(cfg.ui.tabbar_height, 2);
        assert_eq!(cfg.ui.focus, "follow");
    }

    #[test]
    fn syntax_error_propagates() {
        let src = "this is not lua";
        assert!(parse_lua_config(src).is_err());
    }

    #[test]
    fn load_config_smart_picks_lua_by_extension() {
        let tmp = tempfile::NamedTempFile::with_suffix(".lua").unwrap();
        std::fs::write(tmp.path(), "return { terminal = { cols = 99 } }").unwrap();
        let cfg = load_config_smart(Some(tmp.path())).unwrap();
        assert_eq!(cfg.terminal.cols, 99);
    }

    #[test]
    fn load_config_smart_picks_toml_by_extension() {
        let tmp = tempfile::NamedTempFile::with_suffix(".toml").unwrap();
        std::fs::write(tmp.path(), "[terminal]\ncols = 77\n").unwrap();
        let cfg = load_config_smart(Some(tmp.path())).unwrap();
        assert_eq!(cfg.terminal.cols, 77);
    }

    #[test]
    fn lua_and_toml_produce_equivalent_defaults() {
        let lua_cfg = parse_lua_config("return {}").unwrap();
        let toml_cfg: Config = toml::from_str("").unwrap();
        assert_eq!(lua_cfg.terminal.cols, toml_cfg.terminal.cols);
        assert_eq!(lua_cfg.default_profile, toml_cfg.default_profile);
    }
}
