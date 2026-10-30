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
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::cursor::{Hide, Show};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Terminal as RataTerminal;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line as ALine, Point};
use alacritty_terminal::term::cell::{Cell, Flags as CellFlags};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor};

use crate::app::App;
use crate::mouse::{MouseButton, MouseEventKind, MouseMods, Selection};
use crate::terminal::manager::BroadcastTarget;
use crate::theme::{self, Theme};
use crate::ui::backend::BackendFactory;
use crate::ui::event::{AppEvent, AppKeyEvent, AppMouseEvent};
use crate::ui::Renderer;

pub struct TuiRenderer {
    terminal: RataTerminal<CrosstermBackend<io::Stdout>>,
    /// Height of the tab bar in rows, from `ui.tabbar_height`. Kept on the
    /// renderer because mouse events and `size()` need the chrome geometry
    /// outside of the draw pass. Updated on each frame; 1 until then.
    tabbar_height: u16,
}

impl TuiRenderer {
    pub fn new() -> Result<Self> {
        let backend = CrosstermBackend::new(stdout());
        let terminal = RataTerminal::new(backend).context("creating ratatui terminal")?;
        Ok(Self { terminal, tabbar_height: 1 })
    }

    fn draw(&mut self, app: &mut App) -> Result<()> {
        let manager = &app.manager;
        let active_idx = manager.active;
        let palette_open = app.palette.is_open();

        // Resolve the configured theme once per frame. Hot-reload swaps
        // `app.config` wholesale, so the next frame picks up a new theme.
        let theme = theme::resolve(&app.config.ui.theme, &app.config.custom_themes)
            .unwrap_or_else(theme::default_theme);

        // Tab bar height is configurable (`ui.tabbar_height`); keep the
        // renderer's copy in sync so mouse mapping and `size()` agree with
        // the layout actually drawn this frame.
        let tabbar_h = app.config.ui.tabbar_height.max(1);

        self.terminal.draw(|f| {
            let total = f.area();

            // Top: tab bar + terminal + status (1 row).
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(tabbar_h),
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
                render_terminal_grid(
                    f,
                    tab,
                    chunks[1],
                    &app.session.selection,
                    &theme,
                );
            } else {
                let empty = Paragraph::new("No tabs. Press Ctrl+Shift+T to create one.")
                    .alignment(Alignment::Center);
                f.render_widget(empty, chunks[1]);
            }

            // ---- Status bar ----
            let mut status_left = match &manager.broadcast {
                BroadcastTarget::Active => format!(
                    " rs-mrxvt  •  tabs={}  active={}  ",
                    manager.tabs.len(),
                    active_idx + 1
                ),
                BroadcastTarget::All => {
                    format!(" ● BROADCAST:All  •  tabs={}  ", manager.tabs.len())
                }
                BroadcastTarget::Group(g) => {
                    format!(" ● BROADCAST:{}  •  tabs={}  ", g, manager.tabs.len())
                }
            };
            let mut status_style = match &manager.broadcast {
                BroadcastTarget::Active => {
                    Style::default().fg(Color::Black).bg(Color::DarkGray)
                }
                BroadcastTarget::All => Style::default()
                    .fg(Color::White)
                    .bg(Color::Red)
                    .add_modifier(Modifier::BOLD),
                BroadcastTarget::Group(_) => Style::default()
                    .fg(Color::White)
                    .bg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            };

            // Scrollback indicator when the viewport is raised.
            if let Some(tab) = manager.tabs.get(active_idx) {
                let off = tab.display_offset();
                if off > 0 {
                    status_left.push_str(&format!("↑{off}  "));
                }
            }

            // Transient notice (copy/paste feedback, bell, …).
            if let Some(notice) = app.active_notice() {
                status_style = Style::default().fg(Color::Black).bg(Color::Yellow);
                status_left = format!(" {notice}  •  {status_left}");
            }
            f.render_widget(Paragraph::new(status_left).style(status_style), chunks[2]);

            // ---- Command palette overlay ----
            if palette_open {
                render_palette(f, total, app);
            }
        })?;
        self.tabbar_height = tabbar_h;

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
        // capture, bracketed paste, cursor hide) and flush them to the
        // outer terminal. Bracketed paste means the host terminal delivers
        // its own paste (Ctrl+Shift+V / menu) as a Paste event instead of
        // synthesizing keystrokes.
        //
        // The keyboard-enhancement push enables the kitty keyboard protocol
        // on terminals that support it (kitty, foot, WezTerm, ghostty, …).
        // Without it, a legacy terminal maps Ctrl+Shift+T and Ctrl+T to the
        // same byte and crossterm can't report the SHIFT bit, making every
        // Ctrl+Shift+* binding unreachable. Terminals that don't know the
        // sequence ignore it, so this degrades gracefully to legacy input.
        let mut out = stdout();
        execute!(
            out,
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
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
        if let Err(e) = execute!(
            out,
            LeaveAlternateScreen,
            DisableMouseCapture,
            DisableBracketedPaste,
            PopKeyboardEnhancementFlags,
            Show
        ) {
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
                let ev = event::read()?;
                if let Some(app_ev) = self.translate_native_event(ev) {
                    return Ok(Some(app_ev));
                }
                // Event dropped (chrome-area mouse click, unknown key) —
                // keep polling until the deadline expires.
            }
        }
        Ok(None)
    }

    fn render(&mut self, app: &mut App) -> Result<()> {
        self.draw(app)
    }

    fn size(&self) -> (u16, u16) {
        let area = self.terminal.size().unwrap_or_else(|_| Rect::new(0, 0, 80, 24).into());
        // Subtract chrome: tab bar (`ui.tabbar_height`) + status (1).
        let cols = area.width;
        let rows = area
            .height
            .saturating_sub(self.tabbar_height)
            .saturating_sub(1);
        (cols, rows)
    }
}

impl TuiRenderer {
    /// Translate a native crossterm event into an [`AppEvent`].
    ///
    /// Returns `None` for events the app shouldn't see: unrecognised keys
    /// (which would otherwise be typed into the shell as a bogus space) and
    /// mouse events over the chrome (tab bar / status bar), whose screen
    /// coordinates are shifted into grid-relative coordinates here.
    fn translate_native_event(&self, ev: Event) -> Option<AppEvent> {
        match ev {
            Event::Key(k) => AppKeyEvent::try_from_crossterm(k).map(AppEvent::Key),
            Event::Resize(cols, rows) => Some(AppEvent::Resize(cols, rows)),
            Event::FocusGained => Some(AppEvent::FocusGained),
            Event::FocusLost => Some(AppEvent::FocusLost),
            Event::Paste(s) => Some(AppEvent::Paste(s)),
            Event::Mouse(m) => {
                let (cols, rows) = self.size();
                crossterm_mouse_to_app(m, cols, rows, self.tabbar_height).map(AppEvent::Mouse)
            }
        }
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
    selection: &Selection,
    theme: &Theme,
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

    // Block cursor position (viewport row), if visible.
    let show_cursor = tab.term.mode().contains(TermMode::SHOW_CURSOR);
    let cursor_pos: Option<(usize, usize)> = show_cursor
        .then(|| {
            let c = &grid.cursor.point;
            let row = c.line.0 + start_line; // grid Line → screen row
            let col = c.column.0;
            (row >= 0 && (row as usize) < render_rows && col < cols).then_some((row as usize, col))
        })
        .flatten();

    let mut lines: Vec<Line> = Vec::with_capacity(render_rows);
    for row_idx in 0..render_rows as i32 {
        let line = ALine(start_line + row_idx);
        let row = row_idx as usize;

        // Selected column span for this row, if any.
        let sel_span = if selection.is_active() {
            selection.line_span(line.0, cols)
        } else {
            None
        };

        let mut spans: Vec<Span> = Vec::with_capacity(cols);

        for col_idx in 0..cols {
            let point = Point { line, column: Column(col_idx) };
            let cell: &Cell = &grid[point];

            let mut style = cell_to_style(cell, theme);

            // Selection highlight: a solid tinted background. The cell's own
            // inverse flag is suppressed so the highlight stays uniform —
            // stacking REVERSED on top of the tint renders the tint as the
            // *text* color (the terminal swaps fg/bg at display time).
            if let Some((sel_start, sel_end)) = sel_span {
                if col_idx >= sel_start && col_idx < sel_end {
                    style = style
                        .remove_modifier(Modifier::REVERSED)
                        .bg(Color::Rgb(60, 71, 100));
                }
            }

            // Block cursor.
            if let Some((crow, ccol)) = cursor_pos {
                if crow == row && ccol == col_idx {
                    style = style.add_modifier(Modifier::REVERSED);
                }
            }

            if cell.c == ' ' || cell.c == '\0' {
                spans.push(Span::styled(" ", style));
                continue;
            }

            spans.push(Span::styled(cell.c.to_string(), style));
        }

        spans = merge_spans(spans);
        lines.push(Line::from(spans));
    }

    let block = Block::default().borders(Borders::NONE);
    let p = Paragraph::new(lines).block(block);
    f.render_widget(p, area);
}

fn cell_to_style(cell: &Cell, theme: &Theme) -> Style {
    let fg = color_to_ratatui(cell.fg, theme);
    let bg = color_to_ratatui(cell.bg, theme);
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

fn theme_rgb(c: crate::theme::Color) -> Color {
    Color::Rgb(c.r, c.g, c.b)
}

fn color_to_ratatui(c: AnsiColor, theme: &Theme) -> Color {
    match c {
        AnsiColor::Named(n) => named_to_ratatui(n, theme),
        AnsiColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
        AnsiColor::Indexed(i) => {
            // First 16 indices come from the theme's ANSI palette; the rest
            // are fixed hardware palettes (216-cube + 24-grayscale).
            if i < 16 {
                theme_rgb(theme.ansi(i))
            } else if (16..232).contains(&i) {
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

fn named_to_ratatui(n: NamedColor, theme: &Theme) -> Color {
    // Map alacritty's named colors onto the active theme's palette.
    match n {
        NamedColor::Black => theme_rgb(theme.black),
        NamedColor::Red => theme_rgb(theme.red),
        NamedColor::Green => theme_rgb(theme.green),
        NamedColor::Yellow => theme_rgb(theme.yellow),
        NamedColor::Blue => theme_rgb(theme.blue),
        NamedColor::Magenta => theme_rgb(theme.magenta),
        NamedColor::Cyan => theme_rgb(theme.cyan),
        NamedColor::White => theme_rgb(theme.white),
        NamedColor::BrightBlack => theme_rgb(theme.bright_black),
        NamedColor::BrightRed => theme_rgb(theme.bright_red),
        NamedColor::BrightGreen => theme_rgb(theme.bright_green),
        NamedColor::BrightYellow => theme_rgb(theme.bright_yellow),
        NamedColor::BrightBlue => theme_rgb(theme.bright_blue),
        NamedColor::BrightMagenta => theme_rgb(theme.bright_magenta),
        NamedColor::BrightCyan => theme_rgb(theme.bright_cyan),
        NamedColor::BrightWhite => theme_rgb(theme.bright_white),
        NamedColor::BrightForeground => theme_rgb(theme.fg),
        // Special colors.
        NamedColor::Foreground => theme_rgb(theme.fg),
        NamedColor::Background => theme_rgb(theme.bg),
        NamedColor::Cursor => theme_rgb(theme.cursor),
        // Dim* variants — the DIM modifier handles actual dimming; the base
        // color decides the hue.
        NamedColor::DimBlack => theme_rgb(theme.black),
        NamedColor::DimRed => theme_rgb(theme.red),
        NamedColor::DimGreen => theme_rgb(theme.green),
        NamedColor::DimYellow => theme_rgb(theme.yellow),
        NamedColor::DimBlue => theme_rgb(theme.blue),
        NamedColor::DimMagenta => theme_rgb(theme.magenta),
        NamedColor::DimCyan => theme_rgb(theme.cyan),
        NamedColor::DimWhite => theme_rgb(theme.white),
        NamedColor::DimForeground => theme_rgb(theme.fg),
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
/// Crossterm reports positions in *screen* cell coordinates, but the app
/// works in *grid* coordinates: the terminal area starts one row below the
/// tab bar and ends above the status bar (see the layout diagram at the top
/// of this file). Events over the chrome yield `None` — clicking the tab
/// bar must not start a selection in the grid, and the wheel over the
/// status bar must not scroll it.
///
/// Button releases keep their button identity (crossterm's `Up(b)` carries
/// the button) so local selection logic can distinguish left-release from
/// other releases; the mouse encoders ignore it (release is signaled by
/// the event kind).
fn crossterm_mouse_to_app(
    m: crossterm::event::MouseEvent,
    grid_cols: u16,
    grid_rows: u16,
    tabbar_height: u16,
) -> Option<AppMouseEvent> {
    use crossterm::event::{MouseButton as CButton, MouseEventKind as CKind};

    // Screen rows above the grid are the tab bar; row `tabbar_height +
    // grid_rows` and beyond are the status bar or outside the window. Grid
    // rows are screen rows tabbar_height..=tabbar_height + grid_rows - 1.
    if m.row < tabbar_height || m.row >= tabbar_height.saturating_add(grid_rows) {
        return None;
    }

    let map_button = |b: &CButton| match b {
        CButton::Left => MouseButton::Left,
        CButton::Right => MouseButton::Right,
        CButton::Middle => MouseButton::Middle,
    };

    let button = match &m.kind {
        CKind::Down(b) | CKind::Drag(b) => map_button(b),
        CKind::Up(b) => map_button(b),
        CKind::ScrollDown => MouseButton::WheelDown,
        CKind::ScrollUp => MouseButton::WheelUp,
        // Horizontal scroll / plain motion: no button.
        CKind::ScrollLeft | CKind::ScrollRight | CKind::Moved => MouseButton::None,
    };
    let kind = match m.kind {
        CKind::Down(_) => MouseEventKind::Press,
        CKind::Up(_) => MouseEventKind::Release,
        CKind::Drag(_) | CKind::Moved => MouseEventKind::Motion,
        CKind::ScrollDown | CKind::ScrollUp
        | CKind::ScrollLeft | CKind::ScrollRight => MouseEventKind::Press,
    };

    Some(AppMouseEvent {
        button,
        // Clamp the column to the grid; rows are already validated above.
        col: (m.column.min(grid_cols.saturating_sub(1))) as u32,
        row: (m.row - tabbar_height) as u32,
        mods: MouseMods {
            shift: m.modifiers.contains(crossterm::event::KeyModifiers::SHIFT),
            ctrl: m.modifiers.contains(crossterm::event::KeyModifiers::CONTROL),
            alt: m.modifiers.contains(crossterm::event::KeyModifiers::ALT),
        },
        kind,
    })
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

    // Results. Rendered statefully so ratatui applies `highlight_style` to
    // the selected row (a plain `render_widget` never applies it).
    let items: Vec<ListItem> = app
        .palette
        .entries
        .iter()
        .map(|e| ListItem::new(format!("[{}] {}", e.category, e.name)).style(Style::default().fg(Color::Gray)))
        .collect();

    let mut list_state = ListState::default();
    if !app.palette.entries.is_empty() {
        list_state.select(Some(app.palette.selected.min(app.palette.entries.len() - 1)));
    }
    let list = List::new(items)
        .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White).add_modifier(Modifier::BOLD));
    f.render_stateful_widget(list, chunks[1], &mut list_state);
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


#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{MouseButton as CButton, MouseEventKind as CKind};

    fn mouse_ev(row: u16, col: u16, kind: CKind) -> crossterm::event::MouseEvent {
        crossterm::event::MouseEvent {
            kind,
            column: col,
            row,
            modifiers: crossterm::event::KeyModifiers::empty(),
        }
    }

    #[test]
    fn mouse_rows_are_offset_by_tab_bar() {
        // Grid starts at screen row 1 (tab bar height 1): screen row 2 → row 1.
        let ev = crossterm_mouse_to_app(
            mouse_ev(2, 5, CKind::Down(CButton::Left)),
            80,
            10,
            1,
        )
        .unwrap();
        assert_eq!(ev.row, 1);
        assert_eq!(ev.col, 5);
    }

    #[test]
    fn mouse_events_on_chrome_are_dropped() {
        // Tab bar (row 0 with height 1).
        assert!(crossterm_mouse_to_app(mouse_ev(0, 3, CKind::Down(CButton::Left)), 80, 10, 1).is_none());
        // Status bar: grid occupies rows 1..=10, so row 11 is chrome.
        assert!(crossterm_mouse_to_app(mouse_ev(11, 3, CKind::Down(CButton::Left)), 80, 10, 1).is_none());
        // Same with a taller tab bar: grid rows 2..=11, row 12 is chrome.
        assert!(crossterm_mouse_to_app(mouse_ev(12, 3, CKind::Down(CButton::Left)), 80, 10, 2).is_none());
    }

    #[test]
    fn mouse_wheel_inside_grid_maps_to_grid_row() {
        let ev = crossterm_mouse_to_app(
            mouse_ev(4, 0, CKind::ScrollUp),
            80,
            10,
            1,
        )
        .unwrap();
        assert_eq!(ev.button, MouseButton::WheelUp);
        assert_eq!(ev.row, 3);
    }

    #[test]
    fn mouse_columns_are_clamped_to_grid() {
        // A click past the last column (transient resize race) clamps.
        let ev = crossterm_mouse_to_app(
            mouse_ev(1, 200, CKind::Down(CButton::Left)),
            80,
            10,
            1,
        )
        .unwrap();
        assert_eq!(ev.col, 79);
    }
}
