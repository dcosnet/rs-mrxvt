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


//! rs-mrxvt binary entry point.
//!
//! Loads config, parses CLI (including `--backend` and `--config-format`),
//! builds the [`App`], and runs it with the chosen [`Renderer`].

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;
use log::LevelFilter;

use mrxvt::app::App;
use mrxvt::cli::{Cli, ConfigFormat};
use mrxvt::config::Config;
use mrxvt::config::ConfigSource;
use mrxvt::ui::backend::{run_with_backend, print_gpu_info_and_exit};

fn main() -> ExitCode {
    let cli = Cli::parse();

    // Initialize logging.
    let level = match cli.verbose {
        0 => LevelFilter::Warn,
        1 => LevelFilter::Info,
        2 => LevelFilter::Debug,
        _ => LevelFilter::Trace,
    };
    if let Err(e) = env_logger::Builder::new()
        .filter_level(level)
        .format_timestamp(None)
        .try_init()
    {
        eprintln!("rs-mrxvt: warning: could not init logger: {e}");
    }

    // --gpu-info: print probe report and exit (no UI).
    if cli.gpu_info {
        print_gpu_info_and_exit();
        return ExitCode::from(0);
    }

    if let Err(e) = run(cli) {
        eprintln!("rs-mrxvt: {e:#}");
        ExitCode::from(1)
    } else {
        ExitCode::from(0)
    }
}

fn run(cli: Cli) -> Result<i32> {
    let config_path: Option<PathBuf> = cli.config.clone();
    let config = load_config(config_path.as_deref(), cli.config_format)?;

    let backend = cli.backend;
    let mut app = App::new(cli, config)?;

    // Config hot-reload: poll the config file for edits and apply updates
    // in the main loop. 2s polling is ~0% CPU for a single small file.
    // A failure here (unreadable path, etc.) disables hot reload only.
    if let Err(e) = app.start_config_watcher(2000) {
        log::warn!("config hot-reload unavailable: {e}");
    }

    run_with_backend(&mut app, backend)
}

fn load_config(path: Option<&std::path::Path>, format: ConfigFormat) -> Result<Config> {
    // If the user forced a format, honor it. Otherwise let Config::load
    // auto-detect (which uses extension when lua feature is enabled).
    match format {
        ConfigFormat::Auto => Config::load(path),
        ConfigFormat::Toml => {
            let path = path.map(|p| p.to_path_buf()).unwrap_or_else(Config::default_path);
            let expanded = PathBuf::from(shellexpand::tilde(&path.to_string_lossy()).to_string());
            mrxvt::config::FileConfigSource { path: expanded }.load()
        }
        ConfigFormat::Lua => {
            #[cfg(feature = "lua")]
            {
                let path = path.map(|p| p.to_path_buf()).unwrap_or_else(|| {
                    let base = Config::default_path();
                    base.with_extension("lua")
                });
                let expanded = PathBuf::from(shellexpand::tilde(&path.to_string_lossy()).to_string());
                mrxvt::config_lua::LuaConfigSource::new(expanded).load()
            }
            #[cfg(not(feature = "lua"))]
            {
                anyhow::bail!("Lua config support requires building with --features lua");
            }
        }
    }
}
