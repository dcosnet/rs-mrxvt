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


//! rs-mrxvt — a modernized, distro-agnostic mrxvt-inspired terminal emulator.
//!
//! This crate exposes the core terminal, input-routing, and UI-agnostic logic
//! so it can be reused by alternative frontends (TUI today, wgpu tomorrow).
//!
//! ## Modules
//! - [`terminal`] — PTY + VT emulation (built on `alacritty_terminal` + `portable_pty`).
//! - [`input`]    — Input routing: active tab, broadcast-to-all, broadcast-to-tagged-group.
//! - [`ui`]       — Renderer trait + the default TUI backend (`ratatui` + `crossterm`).
//! - [`config`]   — TOML config loader with profiles, macros, keybindings.
//! - [`command`]  — The `Command` enum consumed by the command palette and keybindings.

pub mod cli;
pub mod command;
pub mod config;
pub mod config_watch;
#[cfg(feature = "lua")]
pub mod config_lua;
pub mod app;
pub mod hyperlinks;
pub mod input;
pub mod mouse;
pub mod session;
pub mod terminal;
pub mod theme;
pub mod ui;
#[cfg(feature = "images")]
pub mod images;
#[cfg(feature = "images")]
pub mod sixel;

pub use app::App;
pub use command::Command;
pub use config::Config;

// Re-export alacritty_terminal types used in our public API so downstream
// code (and integration tests) can construct them without depending on
// alacritty_terminal directly.
pub use alacritty_terminal;
pub use alacritty_terminal::index::{Column, Line, Point};

/// Convenience constructor for `Point` (the field is `column`, not `col`,
/// which trips up callers). Used by integration tests.
pub fn re_export_point(line: i32, col: usize) -> Point {
    Point {
        line: Line(line),
        column: Column(col),
    }
}
