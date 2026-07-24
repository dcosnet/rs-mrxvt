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


//! TUI renderer using `ratatui` + `crossterm`.
//!
//! This is the distro-agnostic default backend. It runs in any terminal
//! (xterm, foot, alacritty, kitty, konsole, …) and on any distro, with no
//! system graphics dependencies. A future `wgpu` backend can implement the
//! same [`Renderer`] trait and slot in behind a feature flag.
//!
//! ## Layout
//! ```text
//! ┌──────────────────────────────────────┐
//! │ [1 bash] [2 ssh]* [3 logs]           │ ← tab bar (1 row)
//! ├──────────────────────────────────────┤
//! │                                      │
//! │   active terminal grid               │ ← terminal area (rows-2)
//! │                                      │
//! ├──────────────────────────────────────┤
//! │ ● BROADCAST:All                      │ ← status bar (1 row)
//! └──────────────────────────────────────┘
//! ```
//! When the palette is open, it overlays a centered box on top.

use std::io::{self, stdout, Write};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::cursor::{Hide, Show};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Terminal as RataTerminal;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line as ALine, Point};
use alacritty_terminal::term::cell::{Cell, Flags as CellFlags};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor};

use crate::app::App;
use crate::mouse::{MouseButton, MouseEventKind, MouseMods};
use crate::terminal::manager::BroadcastTarget;
use crate::ui::backend::BackendFactory;
use crate::ui::event::{AppEvent, AppMouseEvent};
use crate::ui::Renderer;

pub struct TuiRenderer {
    terminal: RataTerminal<CrosstermBackend<io::Stdout>>,
}

impl TuiRenderer {
    pub fn new() -> Result<Self> {
        let backend = CrosstermBackend::new(stdout());
        let terminal = RataTerminal::new(backend).context("creating ratatui terminal")?;
        Ok(Self { terminal })
    }

    fn draw(&mut self, app: &mut App) -> Result<()> {
        let manager = &app.manager;
        let active_idx = manager.active;
        let palette_open = app.palette.is_open();

        self.terminal.draw(|f| {
            let total = f.area();

            // Top: tab bar (1 row) + terminal + status (1 row).
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),
                    Constraint::Min(1),
                    Constraint::Length(1),
                ])
                .split(total);

            // ---- Tab bar ----
            let titles: Vec<Span> = manager.tabs.iter().enumerate().flat_map(|(i, t)| {
                let marker = if i == active_idx { "*" } else { " " };
                let tag_str = t.tag.as_ref().map(|g| format!("[{}]", g)).unwrap_or_default();
                let label = format!("{} {}: {}{}", marker, i + 1, t.title, tag_str);
                let style = if i == active_idx {
                    Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Gray)
                };
                vec![
                    Span::styled(label, style),
                    Span::raw(" "),
                ]
            }).collect();
            let tabbar = Paragraph::new(Line::from(titles));
            f.render_widget(tabbar, chunks[0]);

            // ---- Terminal area ----
            if let Some(tab) = manager.tabs.get(active_idx) {
                render_terminal_grid(f, tab, chunks[1]);
            } else {
                let empty = Paragraph::new("No tabs. Press Ctrl+Shift+T to create one.")
                    .alignment(Alignment::Center);
                f.render_widget(empty, chunks[1]);
            }

            // ---- Status bar ----
            let (status_text, status_style) = match &manager.broadcast {
                BroadcastTarget::Active => (
                    format!(" rs-mrxvt  •  tabs={}  active={}  ", manager.tabs.len(), active_idx + 1),
                    Style::default().fg(Color::Black).bg(Color::DarkGray),
                ),
                BroadcastTarget::All => (
                    format!(" ● BROADCAST:All  •  tabs={}  ", manager.tabs.len()),
                    Style::default().fg(Color::White).bg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                BroadcastTarget::Group(g) => (
                    format!(" ● BROADCAST:{}  •  tabs={}  ", g, manager.tabs.len()),
                    Style::default().fg(Color::White).bg(Color::Magenta).add_modifier(Modifier::BOLD),
                ),
            };
            f.render_widget(Paragraph::new(status_text).style(status_style), chunks[2]);

            // ---- Command palette overlay ----
            if palette_open {
                render_palette(f, total, app);
            }
        })?;

        Ok(())
    }
}

impl Renderer for TuiRenderer {
    fn init(&mut self) -> Result<()> {
        // Step 1: Enter raw mode.  This changes how the kernel's line
        // discipline processes input (no echo, no line editing, no
        // signal generation), but it does NOT flush bytes already queued
        // in the kernel input buffer.  Those stale bytes — typically the
        // Enter that launched rs-mrxvt — will be delivered to crossterm
        // as real key events unless we explicitly discard them.
        enable_raw_mode().context("enabling raw mode")?;

        // Step 2: Nuke everything the kernel has already received from the
        // outer terminal (xfce4-term, etc.) but that our process hasn't
        // read yet.  This is the "launch residue" — the Enter (or other
        // bytes) used to start rs-mrxvt from a shell prompt.
        //
        // We do this BEFORE crossterm's internal reader touches stdin,
        // so crossterm never sees these bytes.
        flush_stdin("after enable_raw_mode");

        // Step 3: Send our init escape sequences (alternate screen, mouse
        // capture, cursor hide) and flush them to the outer terminal.
        let mut out = stdout();
        execute!(
            out,
            EnterAlternateScreen,
            EnableMouseCapture,
            Hide
        )
        .context("entering alt screen")?;
        out.flush().context("flushing init sequences")?;
        self.terminal.clear()?;

        // Step 4: The outer terminal will respond to the escape sequences we
        // just sent (DECRPM reports, focus event capabilities, etc.).
        // Give the terminal a moment to process and send its responses,
        // then flush them so they never reach crossterm.
        std::thread::sleep(std::time::Duration::from_millis(30));
        flush_stdin("after init sequences (terminal responses)");

        // Step 5: Belt-and-suspenders drain via crossterm's own parser.
        // This clears anything that slipped into crossterm's internal
        // buffer between steps 2 and now (unlikely, but possible if
        // another thread or signal handler triggered a read).
        drain_stale_input();

        // Step 6: Final kernel-buffer flush.  During the drain loop above,
        // the outer terminal may have sent additional delayed responses.
        // Flush them now so the main loop starts with a perfectly clean
        // stdin.
        flush_stdin("final pre-loop flush");

        log::debug!("TUI init complete — stdin buffer flushed at 3 points");
        Ok(())
    }

    fn fini(&mut self) -> Result<()> {
        let mut out = stdout();
        if let Err(e) = execute!(out, LeaveAlternateScreen, DisableMouseCapture, Show) {
            log::error!("failed to restore terminal: {e}");
        }
        if let Err(e) = disable_raw_mode() {
            log::error!("failed to disable raw mode: {e}");
        }
        Ok(())
    }

    fn poll_event(&mut self, timeout_ms: u64) -> Result<Option<AppEvent>> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        while Instant::now() < deadline {
            if event::poll(Duration::from_millis(20))? {
                let ev = match event::read()? {
                    Event::Key(k) => AppEvent::from(k),
                    Event::Resize(cols, rows) => AppEvent::Resize(cols, rows),
                    Event::FocusGained => AppEvent::FocusGained,
                    Event::FocusLost => AppEvent::FocusLost,
                    Event::Paste(s) => AppEvent::Paste(s),
                    Event::Mouse(m) => AppEvent::Mouse(crossterm_mouse_to_app(m)),
                };
                return Ok(Some(ev));
            }
        }
        Ok(None)
    }

    fn render(&mut self, app: &mut App) -> Result<()> {
        self.draw(app)
    }

    fn size(&self) -> (u16, u16) {
        let area = self.terminal.size().unwrap_or_else(|_| Rect::new(0, 0, 80, 24).into());
        // Subtract chrome: tab bar (1) + status (1).
        let cols = area.width;
        let rows = area.height.saturating_sub(2);
        (cols, rows)
    }
}

/// Factory that constructs [`TuiRenderer`]s. Always available.
pub struct TuiFactory;

impl BackendFactory for TuiFactory {
    fn available(&self) -> bool {
        // TUI always works — even over SSH with no display.
        true
    }
    fn create(&self) -> Result<Box<dyn Renderer>> {
        Ok(Box::new(TuiRenderer::new()?))
    }
}

fn render_terminal_grid(
    f: &mut ratatui::Frame<'_>,
    tab: &crate::terminal::TerminalTab,
    area: Rect,
) {
    let grid = tab.term.grid();

    // Visible viewport = `screen_lines()` starting from `topmost_line() + display_offset`.
    // We iterate row-by-row using grid indexing.
    let cols = area.width as usize;
    let rows = area.height as usize;
    let screen_lines = grid.screen_lines();
    let display_offset = grid.display_offset();

    // Compute the starting line in the grid's coordinate system.
    // grid.display_offset() == 0 means top of visible viewport == Line(0).
    // > 0 means we've scrolled back into history.
    let start_line = -i32::try_from(display_offset).unwrap_or(i32::MAX);
    let render_rows = rows.min(screen_lines);

    let mut lines: Vec<Line> = Vec::with_capacity(render_rows);
    for row_idx in 0..render_rows as i32 {
        let line = ALine(start_line + row_idx);
        let mut spans: Vec<Span> = Vec::with_capacity(cols);

        for col_idx in 0..cols {
            let point = Point { line, column: Column(col_idx) };
            let cell: &Cell = &grid[point];

            if cell.c == ' ' || cell.c == '\0' {
                let style = cell_to_style(cell);
                spans.push(Span::styled(" ", style));
                continue;
            }

            let style = cell_to_style(cell);
            spans.push(Span::styled(cell.c.to_string(), style));
        }

        spans = merge_spans(spans);
        lines.push(Line::from(spans));
    }

    let block = Block::default().borders(Borders::NONE);
    let p = Paragraph::new(lines).block(block);
    f.render_widget(p, area);
}

fn cell_to_style(cell: &Cell) -> Style {
    let fg = color_to_ratatui(cell.fg);
    let bg = color_to_ratatui(cell.bg);
    let mut style = Style::default().fg(fg).bg(bg);
    let flags: CellFlags = cell.flags;
    if flags.contains(CellFlags::BOLD) {
        style = style.add_modifier(Modifier::BOLD);
    }
    if flags.contains(CellFlags::ITALIC) {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if flags.contains(CellFlags::UNDERLINE) {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if flags.contains(CellFlags::INVERSE) {
        style = style.add_modifier(Modifier::REVERSED);
    }
    if flags.contains(CellFlags::DIM) {
        style = style.add_modifier(Modifier::DIM);
    }
    style
}

fn color_to_ratatui(c: AnsiColor) -> Color {
    match c {
        AnsiColor::Named(n) => named_to_ratatui(n),
        AnsiColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
        AnsiColor::Indexed(i) => {
            // 16-color palette + 216-cube + 24-grayscale.
            match i {
                0 => Color::Black,
                1 => Color::Red,
                2 => Color::Green,
                3 => Color::Yellow,
                4 => Color::Blue,
                5 => Color::Magenta,
                6 => Color::Cyan,
                7 => Color::Gray,
                8 => Color::DarkGray,
                9 => Color::LightRed,
                10 => Color::LightGreen,
                11 => Color::LightYellow,
                12 => Color::LightBlue,
                13 => Color::LightMagenta,
                14 => Color::LightCyan,
                15 => Color::White,
                _ => {
                    // Best-effort: 216-cube → RGB
                    if (16..232).contains(&i) {
                        let j = i - 16;
                        let r = (j / 36) % 6;
                        let g = (j / 6) % 6;
                        let b = j % 6;
                        let to_byte = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                        Color::Rgb(to_byte(r), to_byte(g), to_byte(b))
                    } else {
                        // grayscale ramp
                        let v = 8 + (i - 232) * 10;
                        Color::Rgb(v, v, v)
                    }
                }
            }
        }
    }
}

fn named_to_ratatui(n: NamedColor) -> Color {
    // ratatui has 16 named colors + a few extras. Map everything else to
    // the closest semantic equivalent.
    match n {
        NamedColor::Black => Color::Black,
        NamedColor::Red => Color::Red,
        NamedColor::Green => Color::Green,
        NamedColor::Yellow => Color::Yellow,
        NamedColor::Blue => Color::Blue,
        NamedColor::Magenta => Color::Magenta,
        NamedColor::Cyan => Color::Cyan,
        NamedColor::White => Color::Gray,
        NamedColor::BrightBlack => Color::DarkGray,
        NamedColor::BrightRed => Color::LightRed,
        NamedColor::BrightGreen => Color::LightGreen,
        NamedColor::BrightYellow => Color::LightYellow,
        NamedColor::BrightBlue => Color::LightBlue,
        NamedColor::BrightMagenta => Color::LightMagenta,
        NamedColor::BrightCyan => Color::LightCyan,
        NamedColor::BrightWhite => Color::White,
        NamedColor::BrightForeground => Color::White,
        // Cursor / foreground / background variants — fall back to defaults.
        NamedColor::Foreground => Color::Gray,
        NamedColor::Background => Color::Black,
        NamedColor::Cursor => Color::White,
        // Dim* variants — ratatui has no Dim colors; map to the base color
        // (the renderer's DIM modifier handles the actual dimming).
        NamedColor::DimBlack => Color::Black,
        NamedColor::DimRed => Color::Red,
        NamedColor::DimGreen => Color::Green,
        NamedColor::DimYellow => Color::Yellow,
        NamedColor::DimBlue => Color::Blue,
        NamedColor::DimMagenta => Color::Magenta,
        NamedColor::DimCyan => Color::Cyan,
        NamedColor::DimWhite => Color::Gray,
        NamedColor::DimForeground => Color::Gray,
    }
}

fn merge_spans(spans: Vec<Span>) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for s in spans {
        if let Some(last) = out.last_mut() {
            // ratatui::Span doesn't expose PartialEq on Style, so we merge by
            // formatting both to debug strings. Cheap enough for terminal use.
            if format!("{:?}", last.style) == format!("{:?}", s.style) {
                last.content = format!("{}{}", last.content, s.content).into();
                continue;
            }
        }
        out.push(s);
    }
    out
}

/// Translate a crossterm mouse event into an [`AppMouseEvent`].
///
/// Crossterm reports mouse positions in both pixel and cell coordinates;
/// we use the cell coordinates (column, row) which crossterm computes
/// from the terminal's reported cell size.
fn crossterm_mouse_to_app(m: crossterm::event::MouseEvent) -> AppMouseEvent {
    use crossterm::event::{MouseButton as CButton, MouseEventKind as CKind};

    let button = match m.kind {
        CKind::Down(b) | CKind::Drag(b) => match b {
            CButton::Left => MouseButton::Left,
            CButton::Right => MouseButton::Right,
            CButton::Middle => MouseButton::Middle,
        },
        CKind::Up(_) => MouseButton::None,
        CKind::ScrollDown => MouseButton::WheelDown,
        CKind::ScrollUp => MouseButton::WheelUp,
        // Horizontal scroll: treat as no button (rarely used by terminal apps).
        CKind::ScrollLeft | CKind::Moved => MouseButton::None,
        CKind::ScrollRight => MouseButton::None,
    };
    let kind = match m.kind {
        CKind::Down(_) => MouseEventKind::Press,
        CKind::Up(_) => MouseEventKind::Release,
        CKind::Drag(_) | CKind::Moved => MouseEventKind::Motion,
        CKind::ScrollDown | CKind::ScrollUp
        | CKind::ScrollLeft | CKind::ScrollRight => MouseEventKind::Press,
    };

    AppMouseEvent {
        button,
        col: m.column as u32,
        row: m.row as u32,
        mods: MouseMods {
            shift: m.modifiers.contains(crossterm::event::KeyModifiers::SHIFT),
            ctrl: m.modifiers.contains(crossterm::event::KeyModifiers::CONTROL),
            alt: m.modifiers.contains(crossterm::event::KeyModifiers::ALT),
        },
        kind,
    }
}

fn render_palette(f: &mut ratatui::Frame<'_>, total: Rect, app: &App) {
    let width = (total.width as f32 * 0.6).round() as u16;
    let height = (total.height as f32 * 0.5).round() as u16;
    let x = total.x + (total.width.saturating_sub(width)) / 2;
    let y = total.y + (total.height.saturating_sub(height)) / 2;
    let area = Rect::new(x, y, width.max(20), height.max(5));

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            " Command Palette ",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(Color::Black));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(inner);

    // Query line.
    let query_line = Paragraph::new(format!("> {}", app.palette.query))
        .style(Style::default().fg(Color::Yellow));
    f.render_widget(query_line, chunks[0]);

    // Results.
    let items: Vec<ListItem> = app
        .palette
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let style = if i == app.palette.selected {
                Style::default().bg(Color::DarkGray).fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            ListItem::new(format!("[{}] {}", e.category, e.name)).style(style)
        })
        .collect();

    let list = List::new(items)
        .highlight_style(Style::default().bg(Color::Cyan).fg(Color::Black));
    f.render_widget(list, chunks[1]);
}

/// Flush pending bytes from the OS-level stdin buffer.
///
/// Calls `tcflush(fd, TCIFLUSH)` which discards everything the kernel has
/// received from the terminal but that our process hasn't `read()` yet.
/// This is the only reliable way to clear stale input — crossterm's
/// `event::read()` can leave partial sequences in its internal buffer, and
/// a poll-based drain can miss bytes that arrive just after the last poll.
///
/// The `label` parameter is only for debug logging.
#[cfg(unix)]
fn flush_stdin(label: &str) {
    use std::os::unix::io::AsRawFd;
    use std::io::stdin;
    let fd = stdin().as_raw_fd();
    let rc = unsafe { libc::tcflush(fd, libc::TCIFLUSH) };
    if rc == 0 {
        log::debug!("tcflush(TCIFLUSH) OK — {label}");
    } else {
        log::warn!(
            "tcflush(TCIFLUSH) failed (errno={}) — {label}",
            std::io::Error::last_os_error()
        );
    }
}

/// Drain stale events from crossterm's internal parser buffer.
///
/// This is a belt-and-suspenders measure after `flush_stdin()`.  It handles
/// the edge case where crossterm's internal reader has already pulled bytes
/// from the kernel into its own buffer (e.g. from a prior `event::poll()`
/// call).  We call this AFTER `flush_stdin()` so it only needs to clear
/// crossterm's buffer, not the kernel's.
fn drain_stale_input() {
    let deadline = Instant::now() + Duration::from_millis(50);
    let mut count = 0u32;
    while Instant::now() < deadline {
        match event::poll(Duration::from_millis(10)) {
            Ok(true) => {
                if event::read().is_err() {
                    break;
                }
                count += 1;
            }
            Ok(false) => break,
            Err(_) => break,
        }
    }
    if count > 0 {
        log::debug!("drain_stale_input: discarded {count} crossterm events");
    }
}

