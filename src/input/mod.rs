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


//! Input routing and keybinding dispatch.
//!
//! Two responsibilities:
//! 1. Parse raw key events into [`KeyEvent`] chords.
//! 2. Resolve chords to [`Command`]s via a configurable binding table.
//!
//! The actual byte-routing (which PTY receives the keystrokes) is delegated
//! to [`crate::terminal::manager::TerminalManager::route_input`].

pub mod bindings;
pub mod router;

pub use bindings::{KeyChord, KeyBindingTable, Modifiers};
pub use router::InputRouter;
