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


//! Terminal core: PTY + VT emulation.
//!
//! Built on [`alacritty_terminal`] for the VT state machine and
//! [`portable_pty`] for pseudo-terminal management. Both are pure Rust
//! on Linux, so this module has no GUI or distro-specific dependencies.

pub mod manager;
pub mod pty;
pub mod tab;

pub use manager::{BroadcastTarget, TerminalManager};
pub use pty::PtySession;
pub use tab::TerminalTab;
