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


//! CLI argument parsing.
//!
//! Honors a subset of the classic mrxvt flags so existing muscle memory
//! (and existing `.desktop` files / shell wrappers) keeps working.

use std::path::PathBuf;

use clap::{Parser, ValueEnum};

use crate::ui::backend::Backend;

/// Config file format selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum ConfigFormat {
    /// Auto-detect from file extension (`.lua` → Lua, else TOML).
    #[default]
    Auto,
    /// Force TOML parsing.
    Toml,
    /// Force Lua parsing (requires `--features lua` at build time).
    Lua,
}

/// rs-mrxvt — modernized mrxvt-inspired terminal emulator.
///
/// Run with no arguments to launch the interactive TUI.
/// Classic mrxvt flags are honored where they map cleanly to the new engine.
#[derive(Debug, Clone, Parser)]
#[command(name = "rs-mrxvt", version, about, long_about = None)]
pub struct Cli {
    /// Program (and args) to run inside the first tab instead of $SHELL.
    ///
    /// Mirrors the classic `mrxvt -e` / `xterm -e` flag: it takes the rest
    /// of the command line verbatim, so put it last —
    /// `rs-mrxvt -e vim -c 'startinsert' file` works, but anything after
    /// `-e` belongs to the child program.
    #[arg(
        short = 'e',
        long = "exec",
        num_args = 1..,
        trailing_var_arg = true,
        allow_hyphen_values = true,
        value_name = "COMMAND [ARGS]..."
    )]
    pub exec: Option<Vec<String>>,

    /// Window title (also used as the initial tab title).
    #[arg(short = 't', long = "title", default_value = "rs-mrxvt")]
    pub title: String,

    /// Number of tabs to open at startup.
    #[arg(short = 'n', long = "tabs", default_value_t = 1)]
    pub tabs: usize,

    /// Toggle broadcasting mode at startup (classic mrxvt `-j`).
    #[arg(short = 'j', long = "broadcast", default_value_t = false)]
    pub broadcast: bool,

    /// Tag name to assign to all startup tabs (for grouped broadcasting).
    #[arg(short = 'g', long = "tag", value_name = "TAG")]
    pub tag: Option<String>,

    /// Path to a config file. Defaults to `~/.config/rs-mrxvt/config.toml`.
    ///
    /// When built with `--features lua`, the file format is auto-detected
    /// from the extension: `.lua` → Lua script, anything else → TOML.
    /// Use `--config-format` to force a specific format.
    #[arg(short = 'c', long = "config", value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Force a config format regardless of file extension.
    ///
    /// `auto` (default) picks based on extension. `toml` and `lua` force
    /// that parser. The `lua` option requires building with `--features lua`.
    #[arg(long = "config-format", value_enum, default_value = "auto")]
    pub config_format: ConfigFormat,

    /// Working directory for startup tabs. Defaults to current dir.
    #[arg(short = 'd', long = "dir", value_name = "PATH")]
    pub dir: Option<PathBuf>,

    /// Which rendering backend to use.
    ///
    /// `auto` (default) probes in order: wgpu (Vulkan) → wgpu (GL) → softbuffer
    /// (CPU raster, the "VESA mode") → TUI (always available).
    /// Useful overrides: `tui` for SSH/headless; `soft` to force the CPU
    /// rasterizer on a machine with no GPU.
    #[arg(short = 'b', long = "backend", value_enum, default_value_t = Backend::Auto)]
    pub backend: Backend,

    /// Increase verbosity (repeat for trace-level logging).
    #[arg(short = 'v', long = "verbose", action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Print GPU detection info and exit.
    ///
    /// Runs the full structured probe (adapter name, backend, device type,
    /// limits, failure reasons) and prints a human-readable report.
    /// Useful for debugging GPU detection issues or verifying that a
    /// specific adapter is visible to wgpu.
    #[arg(long = "gpu-info")]
    pub gpu_info: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn exec_takes_trailing_args_verbatim() {
        // xterm-style -e: everything after it is the child command, even
        // tokens that look like flags. Regression: clap used to stop at
        // "-c", silently running a bare interactive shell.
        let cli = Cli::try_parse_from([
            "rs-mrxvt", "-e", "sh", "-c", "echo SMOKE-TEST-OK; sleep 0.5",
        ])
        .unwrap();
        assert_eq!(
            cli.exec.unwrap(),
            vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo SMOKE-TEST-OK; sleep 0.5".to_string()
            ]
        );
    }

    #[test]
    fn exec_long_form_also_greedy() {
        let cli = Cli::try_parse_from([
            "rs-mrxvt", "--exec", "vim", "-c", "set nu?", "/etc/motd",
        ])
        .unwrap();
        assert_eq!(cli.exec.unwrap().len(), 4);
    }

    #[test]
    fn exec_optional() {
        let cli = Cli::try_parse_from(["rs-mrxvt"]).unwrap();
        assert!(cli.exec.is_none());
    }

    #[test]
    fn options_before_exec_still_parse() {
        let cli = Cli::try_parse_from(["rs-mrxvt", "-n", "3", "-j", "-e", "htop"])
            .unwrap();
        assert_eq!(cli.tabs, 3);
        assert!(cli.broadcast);
        assert_eq!(cli.exec.unwrap(), vec!["htop".to_string()]);
    }
}
