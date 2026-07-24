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


//! UI layer.
//!
//! Defines the [`Renderer`] trait — the swap point between TUI, wgpu, and
//! software-rasterizer backends. The default backend is [`TuiRenderer`]
//! (ratatui + crossterm), which is distro-agnostic and requires no system
//! graphics libraries.
//!
//! ## Backend selection
//!
//! See [`backend`] for the auto-detect chain. Order:
//! 1. **wgpu** (Vulkan) — full GPU acceleration.
//! 2. **wgpu** (GL) — older GPUs.
//! 3. **softbuffer + tiny-skia** — CPU rasterizer, the modern VESA mode.
//! 4. **ratatui + crossterm** — TUI, always available (even over SSH).
//!
//! ## Event abstraction
//!
//! Every backend translates its native events into [`event::AppEvent`] at
//! the renderer boundary. This lets `App::handle_event` stay backend-agnostic.

pub mod backend;
pub mod event;
pub mod fade;
pub mod mock;
pub mod palette;
pub mod tui;

#[cfg(feature = "gpu")]
pub mod glyph;
#[cfg(feature = "gpu")]
pub mod gpu_detect;
#[cfg(feature = "gpu")]
pub mod shaders;
#[cfg(feature = "gpu")]
pub mod transparency;
#[cfg(feature = "gpu")]
pub mod wgpu;
#[cfg(feature = "gpu")]
pub mod soft;

pub use backend::{Backend, BackendFactory, BackendRegistry, auto_detect, default_registry, print_gpu_info_and_exit, run_with_backend};
pub use event::{AppEvent, AppKey, AppKeyEvent, AppModifiers};
pub use mock::MockRenderer;
pub use palette::{PaletteEntry, PaletteState};
pub use tui::{TuiFactory, TuiRenderer};

use anyhow::Result;

use crate::app::App;

/// A renderer takes the app state and paints one frame.
///
/// All backends — TUI, wgpu, softbuffer — implement this trait. The app
/// loop is generic over `dyn Renderer` and doesn't care which is in use.
pub trait Renderer {
    /// Initialize (enter alt-screen, create window, etc.).
    fn init(&mut self) -> Result<()>;

    /// Tear down (leave alt-screen, destroy window, etc.).
    fn fini(&mut self) -> Result<()>;

    /// Poll for a single input event with the given timeout (in ms).
    /// Returns `None` if no event arrived in time.
    fn poll_event(&mut self, timeout_ms: u64) -> Result<Option<AppEvent>>;

    /// Render one frame.
    fn render(&mut self, app: &mut App) -> Result<()>;

    /// Return the current drawable terminal area (cols, rows).
    fn size(&self) -> (u16, u16);
}
