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


//! Softbuffer renderer — CPU rasterization, the modern VESA mode.
//!
//! When wgpu isn't available (no Vulkan, no GL, ancient hardware), this
//! backend opens a window via `winit` and renders to a CPU pixel buffer
//! via `softbuffer`. Text is rasterized through the shared [`glyph::GlyphCache`]
//! and composited into a `tiny-skia::Pixmap`, then blitted to the softbuffer
//! surface.
//!
//! ## Status: functional (text rendering works)
//!
//! Compiles only with `--features gpu`. Renders the active terminal grid:
//! - Tab bar at the top
//! - Terminal cells (text + colors)
//! - Status bar at the bottom (broadcast indicator)
//!
//! ## Why this exists
//!
//! Classic VESA VBE was a CPU-driven linear framebuffer with no GPU
//! acceleration. It worked on any VGA card because the CPU did everything.
//! `softbuffer` + `tiny-skia` + `ab_glyph` is the modern equivalent: works
//! on any compositor that speaks Wayland or X11, no GPU driver required.

use std::num::NonZeroU32;
use std::sync::Arc;

use anyhow::Result;
use winit::event_loop::EventLoop;
use winit::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
use winit::window::WindowBuilder;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line as ALine, Point};
use alacritty_terminal::term::cell::Cell;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor};

use crate::app::App;
use crate::terminal::manager::BroadcastTarget;
use crate::ui::backend::BackendFactory;
use crate::ui::glyph::GlyphCache;
use crate::ui::Renderer;

/// Cell dimensions in pixels.
const CELL_WIDTH: u32 = 8;
const CELL_HEIGHT: u32 = 16;
const FONT_PIXEL_SIZE: f32 = 14.0;

/// CPU-rasterized renderer.
pub struct SoftRenderer {
    window: Arc<winit::window::Window>,
    /// The winit event loop. Must be kept alive and pumped every tick —
    /// without pumping, the window never receives configure/expose events
    /// and appears as a blank form. winit's `pump_events` lets us drive it
    /// from our own main loop instead of ceding control to `EventLoop::run`.
    event_loop: EventLoop<()>,
    /// Tracks keyboard modifier state (updated by ModifiersChanged events).
    mod_tracker: crate::ui::event::winit_translate::ModifierTracker,
    /// Tracks mouse state (last cursor position, held button) so button
    /// events can be translated into grid-cell coordinates.
    mouse_tracker: crate::ui::event::winit_translate::MouseTracker,
    #[allow(dead_code)]
    context: softbuffer::Context<Arc<winit::window::Window>>,
    surface: softbuffer::Surface<Arc<winit::window::Window>, Arc<winit::window::Window>>,
    glyph_cache: GlyphCache,
    pending_events: Vec<crate::ui::event::AppEvent>,
}

impl SoftRenderer {
    pub fn new() -> Result<Self> {
        let event_loop = EventLoop::<()>::new()
            .map_err(|e| anyhow::anyhow!("creating event loop: {e}"))?;
        let window = Arc::new(
            WindowBuilder::new()
                .with_title("rs-mrxvt (softbuffer)")
                // Give the window a real initial size. Without this, the WM
                // may pick a tiny default (sometimes 1×1 or just title-bar-
                // tall), which leaves the surface too small to render any
                // cells and the user sees a blank form.
                .with_inner_size(winit::dpi::LogicalSize::<u32>::new(1024, 768))
                .build(&event_loop)
                .map_err(|e| anyhow::anyhow!("creating winit window: {e}"))?,
        );

        let context = softbuffer::Context::new(window.clone())
            .map_err(|e| anyhow::anyhow!("creating softbuffer context: {e}"))?;
        let mut surface = softbuffer::Surface::new(&context, window.clone())
            .map_err(|e| anyhow::anyhow!("creating softbuffer surface: {e}"))?;

        let size = window.inner_size();
        surface
            .resize(
                size.width.max(1).try_into().unwrap_or(NonZeroU32::MIN),
                size.height.max(1).try_into().unwrap_or(NonZeroU32::MIN),
            )
            .map_err(|e| anyhow::anyhow!("resizing softbuffer surface: {e}"))?;

        let glyph_cache = GlyphCache::new(FONT_PIXEL_SIZE)?;

        Ok(Self {
            window,
            event_loop,
            mod_tracker: Default::default(),
            mouse_tracker: Default::default(),
            context,
            surface,
            glyph_cache,
            pending_events: Vec::new(),
        })
    }
}

impl Renderer for SoftRenderer {
    fn init(&mut self) -> Result<()> {
        Ok(())
    }

    fn fini(&mut self) -> Result<()> {
        Ok(())
    }

    fn poll_event(&mut self, timeout_ms: u64) -> Result<Option<crate::ui::event::AppEvent>> {
        // If we have buffered events from a previous pump, return the oldest.
        if !self.pending_events.is_empty() {
            return Ok(self.pending_events.drain(..).next());
        }

        // Pump the winit event loop. This processes window events (resize,
        // keyboard, mouse, close) for up to `timeout_ms`, translating each
        // into an `AppEvent` via the winit_translate module. Without this
        // pump call, the window never gets its initial configure event and
        // appears as a blank form.
        let mut collected: Vec<crate::ui::event::AppEvent> = Vec::new();
        let mut tracker = self.mod_tracker;
        let mut mouse = self.mouse_tracker;
        let (cols, rows) = self.size();
        let timeout = Some(std::time::Duration::from_millis(timeout_ms));

        let status = self.event_loop.pump_events(timeout, |event, elwt| {
            if let Some(app_ev) = crate::ui::event::winit_translate::translate_event(
                &event, &mut tracker, &mut mouse, elwt,
                CELL_WIDTH, CELL_HEIGHT, cols, rows,
            ) {
                collected.push(app_ev);
            }
        });

        self.mod_tracker = tracker;
        self.mouse_tracker = mouse;

        if let PumpStatus::Exit(_) = status {
            collected.push(crate::ui::event::AppEvent::Quit);
        }

        if collected.is_empty() {
            Ok(None)
        } else {
            let first = collected.remove(0);
            self.pending_events.extend(collected);
            Ok(Some(first))
        }
    }

    fn render(&mut self, app: &mut App) -> Result<()> {
        // Resize the softbuffer surface if the window changed.
        let size = self.window.inner_size();
        if size.width > 0 && size.height > 0 {
            self.surface
                .resize(
                    size.width.max(1).try_into().unwrap_or(NonZeroU32::MIN),
                    size.height.max(1).try_into().unwrap_or(NonZeroU32::MIN),
                )
                .map_err(|e| anyhow::anyhow!("resizing softbuffer surface: {e}"))?;
        }

        // Resolve the configured theme once per frame (hot-reload picks it
        // up on the next frame automatically).
        let theme = crate::theme::resolve(&app.config.ui.theme, &app.config.custom_themes)
            .unwrap_or_else(crate::theme::default_theme);

        // Build a tiny-skia Pixmap to render into.
        let mut pixmap = tiny_skia::Pixmap::new(size.width, size.height)
            .ok_or_else(|| anyhow::anyhow!("creating pixmap of {}x{}", size.width, size.height))?;

        // Fill background with the theme's background color.
        pixmap.fill(tiny_skia::Color::from_rgba8(theme.bg.r, theme.bg.g, theme.bg.b, 255));

        // Draw the terminal grid + chrome.
        let manager = &app.manager;
        let active_idx = manager.active;

        // Compute layout in pixels.
        let tabbar_h: u32 = CELL_HEIGHT;
        let statusbar_h: u32 = CELL_HEIGHT;
        let term_y: u32 = tabbar_h;
        let term_h: u32 = size.height.saturating_sub(tabbar_h + statusbar_h);

        // Render the active terminal grid.
        if let Some(tab) = manager.tabs.get(active_idx) {
            render_terminal_grid(
                &mut pixmap,
                &mut self.glyph_cache,
                tab,
                &app.session.selection,
                &theme,
                0,
                term_y,
                size.width,
                term_h,
            );
        }

        // Render tab bar (just text labels for now).
        render_tab_bar(&mut pixmap, &mut self.glyph_cache, manager, size.width, tabbar_h);

        // Render status bar.
        render_status_bar(&mut pixmap, &mut self.glyph_cache, &manager.broadcast, manager.tabs.len(), active_idx, size.width, statusbar_h, size.height);

        // Blit the pixmap to the softbuffer surface.
        let mut buffer = self.surface
            .buffer_mut()
            .map_err(|e| anyhow::anyhow!("locking softbuffer buffer: {e}"))?;

        // tiny-skia stores pixels as PremultipliedColorU8 (RGBA, premultiplied).
        // softbuffer wants u32 in 0xAARRGGBB (non-premultiplied). Convert.
        let pixels = pixmap.pixels();
        for (i, px) in pixels.iter().enumerate() {
            let r = px.red() as u32;
            let g = px.green() as u32;
            let b = px.blue() as u32;
            let a = px.alpha() as u32;
            // Un-premultiply: divide by alpha. `checked_div` + `unwrap_or`
            // handles the alpha == 0 case without a branch.
            let r = (r * 255).checked_div(a).unwrap_or(0);
            let g = (g * 255).checked_div(a).unwrap_or(0);
            let b = (b * 255).checked_div(a).unwrap_or(0);
            buffer[i] = (a << 24) | (r << 16) | (g << 8) | b;
        }

        buffer
            .present()
            .map_err(|e| anyhow::anyhow!("presenting softbuffer buffer: {e}"))?;

        Ok(())
    }

    fn size(&self) -> (u16, u16) {
        let size = self.window.inner_size();
        let cols = (size.width / CELL_WIDTH).max(2) as u16;
        let rows = (size.height / CELL_HEIGHT).saturating_sub(2).max(1) as u16;
        (cols, rows)
    }
}

/// Factory for [`SoftRenderer`].
pub struct SoftFactory;

impl BackendFactory for SoftFactory {
    fn available(&self) -> bool {
        super::backend::probe_softbuffer_available_pub()
    }

    fn create(&self) -> Result<Box<dyn Renderer>> {
        Ok(Box::new(SoftRenderer::new()?))
    }
}

// ─── Rendering helpers ───────────────────────────────────────────────────────

/// Selection highlight tint (matches the TUI backend's Rgb(60, 71, 100)).
const SELECTION_TINT: (u8, u8, u8, u8) = (60, 71, 100, 255);

#[allow(clippy::too_many_arguments)] // mirrors the wgpu builder's state bundle
fn render_terminal_grid(
    pixmap: &mut tiny_skia::Pixmap,
    cache: &mut GlyphCache,
    tab: &crate::terminal::TerminalTab,
    selection: &crate::mouse::Selection,
    theme: &crate::theme::Theme,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) {
    use alacritty_terminal::term::TermMode;

    let grid = tab.term.grid();
    let cols = (width / CELL_WIDTH) as usize;
    let rows = (height / CELL_HEIGHT) as usize;
    let screen_lines = grid.screen_lines();
    let display_offset = grid.display_offset();
    let start_line = -(display_offset as i32);
    let render_rows = rows.min(screen_lines);

    // Block cursor position (viewport row), if visible.
    let show_cursor = tab.term.mode().contains(TermMode::SHOW_CURSOR);
    let cursor_pos: Option<(usize, usize)> = show_cursor
        .then(|| {
            let c = &grid.cursor.point;
            let row = c.line.0 + start_line;
            let col = c.column.0;
            (row >= 0 && (row as usize) < render_rows && col < cols).then_some((row as usize, col))
        })
        .flatten();

    for row_idx in 0..render_rows as i32 {
        let line = ALine(start_line + row_idx);
        let sel_span = if selection.is_active() {
            selection.line_span(line.0, cols)
        } else {
            None
        };
        for col_idx in 0..cols {
            let point = Point { line, column: Column(col_idx) };
            let cell: &Cell = &grid[point];

            let cell_x = x + (col_idx as u32) * CELL_WIDTH;
            let cell_y = y + (row_idx as u32) * CELL_HEIGHT;

            // The INVERSE flag means the renderer swaps fg/bg; the DIM flag
            // scales the foreground down. Selection replaces the background
            // tint (inverse suppressed), and the cursor cell reverses.
            let (mut fg, mut bg) = (ansi_to_rgba(cell.fg, theme), ansi_to_rgba(cell.bg, theme));
            if cell.flags.contains(alacritty_terminal::term::cell::Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if cell.flags.contains(alacritty_terminal::term::cell::Flags::DIM) {
                fg = (
                    (fg.0 as u32 * 6 / 10) as u8,
                    (fg.1 as u32 * 6 / 10) as u8,
                    (fg.2 as u32 * 6 / 10) as u8,
                    255,
                );
            }
            let selected = sel_span
                .map(|(a, b)| col_idx >= a && col_idx < b)
                .unwrap_or(false);
            if selected {
                bg = SELECTION_TINT;
            }
            if cursor_pos == Some((row_idx as usize, col_idx)) {
                std::mem::swap(&mut fg, &mut bg);
            }

            // Fill cell background.
            fill_rect(pixmap, cell_x, cell_y, CELL_WIDTH, CELL_HEIGHT, bg);

            // Render the glyph.
            if cell.c != ' ' && cell.c != '\0' {
                draw_glyph(
                    pixmap,
                    cache,
                    cell.c,
                    cell_x,
                    cell_y,
                    fg,
                    cell.flags.contains(alacritty_terminal::term::cell::Flags::BOLD),
                    cell.flags.contains(alacritty_terminal::term::cell::Flags::ITALIC),
                );
            }
        }
    }
}

fn render_tab_bar(
    pixmap: &mut tiny_skia::Pixmap,
    cache: &mut GlyphCache,
    manager: &crate::terminal::manager::TerminalManager,
    width: u32,
    height: u32,
) {
    // Fill background.
    fill_rect(pixmap, 0, 0, width, height, (30, 30, 30, 255));

    let mut x: u32 = 4;
    for (i, tab) in manager.tabs.iter().enumerate() {
        let active = i == manager.active;
        let marker = if active { "*" } else { " " };
        let tag_str = tab.tag.as_ref().map(|g| format!("[{g}]")).unwrap_or_default();
        let label = format!("{marker} {}: {}{}", i + 1, tab.title, tag_str);
        let fg = if active { (255, 255, 255, 255) } else { (150, 150, 150, 255) };

        // Draw a separator.
        if i > 0 {
            fill_rect(pixmap, x, 2, 1, height - 4, (60, 60, 60, 255));
            x += 4;
        }

        // Draw the label.
        for c in label.chars() {
            draw_glyph(pixmap, cache, c, x, 0, fg, active, false);
            x += CELL_WIDTH;
            if x >= width {
                return;
            }
        }
        x += 4;
    }
}

/// Render the bottom status bar (tabs, broadcast indicator).
///
/// This is a private render-pass helper; the argument list mirrors the
/// per-call render state and grouping it into a struct would hurt locality.
#[allow(clippy::too_many_arguments)]
fn render_status_bar(
    pixmap: &mut tiny_skia::Pixmap,
    cache: &mut GlyphCache,
    broadcast: &BroadcastTarget,
    tab_count: usize,
    active_idx: usize,
    width: u32,
    height: u32,
    window_height: u32,
) {
    let y = window_height.saturating_sub(height);
    let (text, color) = match broadcast {
        BroadcastTarget::Active => (
            format!(" rs-mrxvt  •  tabs={}  active={}", tab_count, active_idx + 1),
            (180, 180, 180, 255),
        ),
        BroadcastTarget::All => (
            format!(" ● BROADCAST:All  •  tabs={}", tab_count),
            (255, 80, 80, 255),
        ),
        BroadcastTarget::Group(g) => (
            format!(" ● BROADCAST:{}  •  tabs={}", g, tab_count),
            (255, 100, 255, 255),
        ),
    };
    fill_rect(pixmap, 0, y, width, height, (30, 30, 30, 255));
    let mut x: u32 = 4;
    for c in text.chars() {
        draw_glyph(pixmap, cache, c, x, y, color, false, false);
        x += CELL_WIDTH;
        if x >= width {
            break;
        }
    }
}

/// Composite a single cached glyph into the pixmap at the given cell.
///
/// Private render-pass helper; see `render_status_bar` for the rationale on
/// the argument count.
#[allow(clippy::too_many_arguments)]
fn draw_glyph(
    pixmap: &mut tiny_skia::Pixmap,
    cache: &mut GlyphCache,
    c: char,
    cell_x: u32,
    cell_y: u32,
    color: (u8, u8, u8, u8),
    bold: bool,
    italic: bool,
) {
    let g = cache.get(c, bold, italic);
    let (r, gr, b, _a) = color;

    // Position the glyph on the shared baseline (same rule as the wgpu
    // backend) so both rasterizers draw identical layouts. Centering the
    // bitmap in the cell ignores the font's bearings and makes tall glyphs
    // (|, brackets) sit visibly higher than short ones.
    let baseline = CELL_HEIGHT as f32 - 3.0;
    let start_x = cell_x + g.bearing_x.max(0.0) as u32;
    let start_y = cell_y + (baseline + g.bearing_y).max(0.0) as u32;

    // Composite the glyph onto the pixmap (alpha-blend over background).
    let pm_w = pixmap.width();
    let pm_h = pixmap.height();
    let pixels = pixmap.pixels_mut();

    for gy in 0..g.height {
        for gx in 0..g.width {
            let px = start_x + gx as u32;
            let py = start_y + gy as u32;
            if px >= pm_w || py >= pm_h {
                continue;
            }
            let gidx = (gy * g.width + gx) * 4;
            let alpha = g.pixels[gidx + 3] as u32;
            if alpha == 0 {
                continue;
            }
            let pidx = (py * pm_w + px) as usize;
            let dst = pixels[pidx];
            let dst_r = dst.red() as u32;
            let dst_g = dst.green() as u32;
            let dst_b = dst.blue() as u32;
            let a = alpha;
            let inv_a = 255 - a;
            // Source is fully opaque color, so premultiplication is just r*a/255.
            let src_r = r as u32 * a / 255;
            let src_g = gr as u32 * a / 255;
            let src_b = b as u32 * a / 255;
            // Result premultiplied = src + dst * (1 - alpha_a/255).
            let out_r = (src_r + dst_r * inv_a / 255) as u8;
            let out_g = (src_g + dst_g * inv_a / 255) as u8;
            let out_b = (src_b + dst_b * inv_a / 255) as u8;
            // Final alpha is 255 (we drew over a fully-opaque background).
            if let Some(c) = tiny_skia::PremultipliedColorU8::from_rgba(out_r, out_g, out_b, 255) {
                pixels[pidx] = c;
            }
        }
    }
}

fn fill_rect(
    pixmap: &mut tiny_skia::Pixmap,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    color: (u8, u8, u8, u8),
) {
    let pm_w = pixmap.width();
    let pm_h = pixmap.height();
    let pixels = pixmap.pixels_mut();
    let (r, g, b, a) = color;
    // Premultiply the source color.
    let (pr, pg, pb) = if a == 255 {
        (r, g, b)
    } else if a == 0 {
        (0, 0, 0)
    } else {
        (
            (r as u32 * a as u32 / 255) as u8,
            (g as u32 * a as u32 / 255) as u8,
            (b as u32 * a as u32 / 255) as u8,
        )
    };
    // from_rgba returns None if any component > alpha; we've already premultiplied,
    // so it should always be Some.
    let c = tiny_skia::PremultipliedColorU8::from_rgba(pr, pg, pb, a)
        .unwrap_or(tiny_skia::PremultipliedColorU8::TRANSPARENT);

    for ry in 0..h {
        for rx in 0..w {
            let px = x + rx;
            let py = y + ry;
            if px < pm_w && py < pm_h {
                pixels[(py * pm_w + px) as usize] = c;
            }
        }
    }
}

fn ansi_to_rgba(c: AnsiColor, theme: &crate::theme::Theme) -> (u8, u8, u8, u8) {
    match c {
        AnsiColor::Named(n) => named_to_rgba(n, theme),
        AnsiColor::Spec(rgb) => (rgb.r, rgb.g, rgb.b, 255),
        AnsiColor::Indexed(i) => {
            // First 16 indices come from the theme; 216-cube + grayscale are
            // fixed hardware palettes.
            if i < 16 {
                let t = theme.ansi(i);
                (t.r, t.g, t.b, 255)
            } else if (16..232).contains(&i) {
                let j = i - 16;
                let r = (j / 36) % 6;
                let g = (j / 6) % 6;
                let b = j % 6;
                let to_byte = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                (to_byte(r), to_byte(g), to_byte(b), 255)
            } else {
                let v = 8 + (i - 232) * 10;
                (v, v, v, 255)
            }
        }
    }
}

fn named_to_rgba(n: NamedColor, theme: &crate::theme::Theme) -> (u8, u8, u8, u8) {
    let rgb = |c: crate::theme::Color| (c.r, c.g, c.b, 255u8);
    use NamedColor::*;
    match n {
        Black => rgb(theme.black),
        Red => rgb(theme.red),
        Green => rgb(theme.green),
        Yellow => rgb(theme.yellow),
        Blue => rgb(theme.blue),
        Magenta => rgb(theme.magenta),
        Cyan => rgb(theme.cyan),
        White => rgb(theme.white),
        BrightBlack => rgb(theme.bright_black),
        BrightRed => rgb(theme.bright_red),
        BrightGreen => rgb(theme.bright_green),
        BrightYellow => rgb(theme.bright_yellow),
        BrightBlue => rgb(theme.bright_blue),
        BrightMagenta => rgb(theme.bright_magenta),
        BrightCyan => rgb(theme.bright_cyan),
        BrightWhite => rgb(theme.bright_white),
        BrightForeground => rgb(theme.fg),
        Foreground => rgb(theme.fg),
        Background => rgb(theme.bg),
        Cursor => rgb(theme.cursor),
        // Dim* variants map to their base color (dimming is applied to the
        // fg in render_terminal_grid).
        _ => (200, 200, 200, 255),
    }
}
