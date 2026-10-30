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


//! A single terminal tab = (PTY session) + (VT emulator state).
//!
//! [`TerminalTab`] owns the PTY, the VT emulator (an `alacritty_terminal::Term`),
//! and metadata like title, tag, and active broadcast state. Tabs are
//! managed by [`crate::terminal::manager::TerminalManager`].
//!
//! ## Threading model
//! Each tab spawns a dedicated reader thread that performs blocking `read()`
//! on the PTY master FD and forwards bytes through a bounded channel. The
//! main event loop drains the channel non-blockingly via `try_recv()` in
//! [`TerminalTab::poll_pty`]. This avoids one stalled tab blocking the
//! whole UI — a real-world failure mode of the original mrxvt.

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line as ALine, Point as APoint};
use alacritty_terminal::term::cell::{Cell, Flags as CellFlags};
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode};
use alacritty_terminal::vte::ansi::Processor;

use crate::config::Profile;
use crate::mouse::{MouseMode, SelPoint, Selection};

use super::pty::PtySession;

/// Default semantic escape chars for word (double-click) selection,
/// mirroring alacritty's default.
pub const SEMANTIC_ESCAPE_CHARS: &str = ",│`|:\"' ()[]{}<>\t";

/// Event listener that forwards VT events (title changes, OSC 52 clipboard
/// requests, bell, …) into a channel the main loop drains after every PTY
/// poll. This is what makes programs like tmux and nvim able to set the
/// tab title or the system clipboard.
#[derive(Clone, Default)]
pub struct EventForwarder(Option<mpsc::Sender<Event>>);

impl EventForwarder {
    fn channel() -> (Self, Receiver<Event>) {
        let (tx, rx) = mpsc::channel();
        (Self(Some(tx)), rx)
    }
}

impl EventListener for EventForwarder {
    fn send_event(&self, event: Event) {
        if let Some(tx) = &self.0 {
            let _ = tx.send(event);
        }
    }
}

/// A simple Dimensions impl used to size the Term on creation/resize.
#[derive(Copy, Clone, Debug)]
pub struct TermSize {
    pub cols: usize,
    pub rows: usize,
}

impl TermSize {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols: cols.max(2) as usize,
            rows: rows.max(1) as usize,
        }
    }
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
    fn history_size(&self) -> usize {
        0
    }
}

/// One tab. Cheap to keep alive; the heavy state is the `Term` grid.
pub struct TerminalTab {
    pub id: u32,
    pub title: String,
    /// Title to restore when the child sends `ResetTitle` (OSC 0/2 without
    /// an argument, or RIS).
    pub default_title: String,
    pub tag: Option<String>,
    pub pty: PtySession,
    pub term: Term<EventForwarder>,
    parser: Processor,
    /// Channel fed by the reader thread.
    rx: Receiver<Vec<u8>>,
    /// VT events forwarded by the [`EventForwarder`] during parsing.
    events: Receiver<Event>,
    /// Events drained from `events`, awaiting pickup by the main loop.
    pending_events: VecDeque<Event>,
    /// Reader thread handle (kept so we can join on drop if needed).
    _reader_thread: thread::JoinHandle<()>,
    /// Tracks whether the child has exited.
    pub eof_seen: bool,
}

static NEXT_ID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

impl TerminalTab {
    /// Spawn a new tab.
    pub fn new(
        title: String,
        profile: &Profile,
        fallback_shell: &str,
        cols: u16,
        rows: u16,
        scrollback: usize,
    ) -> anyhow::Result<Self> {
        let mut pty = PtySession::spawn(profile, fallback_shell, cols, rows)?;

        let term_cfg = TermConfig { scrolling_history: scrollback, ..TermConfig::default() };

        let size = TermSize::new(cols, rows);
        let (listener, events) = EventForwarder::channel();
        let term = Term::new(term_cfg, &size, listener);

        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let default_title = title.clone();

        // Spawn a reader thread that does blocking reads and forwards bytes
        // through a channel. The main loop drains the channel without blocking.
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let mut reader = pty.reader_clone()?;
        let reader_thread = thread::Builder::new()
            .name(format!("mrxvt-tab-{id}-reader"))
            .spawn(move || {
                let mut buf = vec![0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => {
                            // EOF — child exited. Send an empty vec as a sentinel.
                            let _ = tx.send(Vec::new());
                            break;
                        }
                        Ok(n) => {
                            if tx.send(buf[..n].to_vec()).is_err() {
                                // Receiver dropped — tab was closed.
                                break;
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            // Spurious wakeup; retry.
                            thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(_) => {
                            let _ = tx.send(Vec::new());
                            break;
                        }
                    }
                }
            })?;

        Ok(Self {
            id,
            title,
            default_title,
            tag: profile.tag.clone(),
            pty,
            term,
            parser: Processor::new(),
            rx,
            events,
            pending_events: VecDeque::new(),
            _reader_thread: reader_thread,
            eof_seen: false,
        })
    }

    /// Drain whatever the reader thread has buffered and feed it to the VT emulator.
    ///
    /// Non-blocking: returns immediately with `Ok(0)` if no data is available.
    /// Returns `Ok(0)` with `eof_seen = true` after the child process exits.
    ///
    /// VT events emitted while parsing (title changes, OSC 52 clipboard
    /// requests, …) are collected and can be picked up with
    /// [`TerminalTab::drain_events`].
    pub fn poll_pty(&mut self) -> anyhow::Result<usize> {
        let mut total = 0;
        loop {
            match self.rx.try_recv() {
                Ok(bytes) => {
                    if bytes.is_empty() {
                        // EOF sentinel from the reader thread.
                        self.eof_seen = true;
                        self.drain_event_channel();
                        return Ok(total);
                    }
                    self.parser.advance(&mut self.term, &bytes);
                    self.drain_event_channel();
                    total += bytes.len();
                }
                Err(TryRecvError::Empty) => return Ok(total),
                Err(TryRecvError::Disconnected) => {
                    self.eof_seen = true;
                    return Ok(total);
                }
            }
        }
    }

    /// Move events from the channel into the pending queue (non-blocking).
    fn drain_event_channel(&mut self) {
        while let Ok(ev) = self.events.try_recv() {
            self.pending_events.push_back(ev);
        }
    }

    /// Take all VT events emitted since the last call.
    pub fn drain_events(&mut self) -> Vec<Event> {
        self.drain_event_channel();
        self.pending_events.drain(..).collect()
    }

    /// Send input to the child process.
    pub fn write_input(&self, data: &[u8]) -> std::io::Result<()> {
        self.pty.write_all(data)
    }

    /// Resize the underlying PTY and VT grid.
    pub fn resize(&mut self, cols: u16, rows: u16) -> anyhow::Result<()> {
        self.pty.resize(cols, rows)?;
        self.term.resize(TermSize::new(cols, rows));
        Ok(())
    }

    /// Currently-visible terminal size.
    pub fn size(&self) -> (u16, u16) {
        let cols = self.term.columns() as u16;
        let rows = self.term.screen_lines() as u16;
        (cols, rows)
    }

    /// Has the child process exited?
    pub fn is_dead(&self) -> bool {
        self.eof_seen
    }

    // ─── mode bridges ────────────────────────────────────────────────────────

    /// Reset the local emulator state (the RIS escape action): modes, colors,
    /// charsets, scroll regions, tab stops, alt screen, title stack. This is
    /// what the `reset`/ResetTerminal command should do to *our* parser —
    /// writing RIS bytes to the PTY would only feed them to the child's stdin.
    pub fn reset_state(&mut self) {
        use alacritty_terminal::vte::ansi::Handler;
        self.term.reset_state();
        self.drain_event_channel();
    }

    /// The child's negotiated mouse-reporting mode, as a [`MouseMode`].
    ///
    /// Bridges alacritty's `TermMode` bits:
    /// - `MOUSE_REPORT_CLICK` (DECSET 1000) → `x11`
    /// - `MOUSE_DRAG` (1002) → `x11`
    /// - `MOUSE_MOTION` (1003) → `motion`
    /// - `SGR_MOUSE` (1006) → `sgr`
    pub fn mouse_mode(&self) -> MouseMode {
        let mode = self.term.mode();
        MouseMode {
            x10: false,
            x11: mode.contains(TermMode::MOUSE_REPORT_CLICK)
                || mode.contains(TermMode::MOUSE_DRAG),
            motion: mode.contains(TermMode::MOUSE_MOTION),
            sgr: mode.contains(TermMode::SGR_MOUSE),
        }
    }

    /// Has the child enabled bracketed paste (DECSET 2004)?
    pub fn bracketed_paste(&self) -> bool {
        self.term.mode().contains(TermMode::BRACKETED_PASTE)
    }

    /// Is the alternate screen active (e.g. vim, less, htop)?
    pub fn alternate_screen(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    /// Does the child want wheel events translated to arrow keys while on
    /// the alternate screen (DECSET 1007, on by default)?
    pub fn alternate_scroll(&self) -> bool {
        self.term.mode().contains(TermMode::ALTERNATE_SCROLL)
    }

    // ─── scrolling ───────────────────────────────────────────────────────────

    /// Scroll the viewport within the scrollback buffer.
    pub fn scroll_display(&mut self, scroll: Scroll) {
        self.term.scroll_display(scroll);
    }

    /// Lines of scrollback the viewport is currently raised by.
    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// True when the viewport is sitting at the live bottom.
    pub fn at_bottom(&self) -> bool {
        self.display_offset() == 0
    }

    /// Wheel scroll: one notch. On the alternate screen with alternate-scroll
    /// enabled this is forwarded as arrow keys instead (so `less`/`vim`
    /// scroll); otherwise it moves the scrollback viewport.
    pub fn wheel_scroll(&mut self, up: bool) {
        if self.alternate_screen() && self.alternate_scroll() {
            // 3 arrows per notch, matching the common xterm/mrxvt default.
            let seq: &[u8] = if up { b"\x1b[A" } else { b"\x1b[B" };
            let mut bytes = Vec::with_capacity(seq.len() * 3);
            for _ in 0..3 {
                bytes.extend_from_slice(seq);
            }
            let _ = self.write_input(&bytes);
        } else if up {
            self.scroll_display(Scroll::Delta(3));
        } else {
            self.scroll_display(Scroll::Delta(-3));
        }
    }

    // ─── selection ───────────────────────────────────────────────────────────

    /// Convert viewport (mouse) coordinates into a grid point.
    ///
    /// `row` is the visible-area row (0 = top visible row); the returned
    /// `line` is in grid coordinates (`0` = top visible line at display
    /// offset 0, negative = scrollback history).
    pub fn grid_point(&self, row: u32, col: u32) -> SelPoint {
        let offset = self.term.grid().display_offset() as i32;
        SelPoint {
            line: -offset + row as i32,
            col: col as usize,
        }
    }

    /// Extract the selected text from the grid.
    ///
    /// Line-based semantics matching mainstream terminals: the first and
    /// last selected lines are cut at the selection points, interior lines
    /// are taken whole. Trailing whitespace is trimmed per line, and lines
    /// are joined with `\n`. Returns `None` if the selection is inactive.
    pub fn selection_text(&self, sel: &Selection) -> Option<String> {
        if !sel.is_active() {
            return None;
        }
        let grid = self.term.grid();
        let cols = grid.columns();
        let mut out = String::new();
        for line in sel.lines() {
            let (start, end) = sel.line_span(line, cols)?;
            let mut row = String::with_capacity(end - start);
            for col in start..end {
                let point = APoint { line: ALine(line), column: Column(col) };
                let cell: &Cell = &grid[point];
                // Skip wide-char spacer placeholders; the wide char itself
                // is stored in the leading cell.
                if cell
                    .flags
                    .intersects(CellFlags::WIDE_CHAR_SPACER | CellFlags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                row.push(if cell.c == '\0' { ' ' } else { cell.c });
            }
            out.push_str(row.trim_end());
            out.push('\n');
        }
        // Every line got a trailing \n; drop the last one. A selection that
        // ends mid-line shouldn't end with a newline; one that ends at the
        // far right edge of a line conceptually includes it, which matches
        // what users expect when triple-clicking or selecting full lines.
        Some(out.strip_suffix('\n').unwrap_or(&out).to_string())
    }

    /// Expand a grid point to word boundaries (double-click selection).
    ///
    /// Words are delimited by whitespace and [`SEMANTIC_ESCAPE_CHARS`].
    pub fn expand_word(&self, p: SelPoint) -> (SelPoint, SelPoint) {
        let grid = self.term.grid();
        let cols = grid.columns();
        let escape: Vec<char> = SEMANTIC_ESCAPE_CHARS.chars().collect();

        let cell_char = |line: i32, col: usize| -> char {
            let point = APoint { line: ALine(line), column: Column(col) };
            let c = grid[point].c;
            if c == '\0' { ' ' } else { c }
        };

        // If we landed on a separator, select just the separator cell run.
        let c0 = cell_char(p.line, p.col);
        let is_sep = |c: char| c.is_whitespace() || escape.contains(&c);

        let mut start = p.col;
        let mut end = p.col;
        if is_sep(c0) {
            while start > 0 && is_sep(cell_char(p.line, start - 1)) {
                start -= 1;
            }
            while end + 1 < cols && is_sep(cell_char(p.line, end + 1)) {
                end += 1;
            }
        } else {
            while start > 0 && !is_sep(cell_char(p.line, start - 1)) {
                start -= 1;
            }
            while end + 1 < cols && !is_sep(cell_char(p.line, end + 1)) {
                end += 1;
            }
        }
        (
            SelPoint { line: p.line, col: start },
            SelPoint { line: p.line, col: end },
        )
    }

    /// Expand a grid point to full-line bounds (triple-click selection).
    pub fn expand_line(&self, p: SelPoint) -> (SelPoint, SelPoint) {
        let cols = self.term.grid().columns();
        (
            SelPoint { line: p.line, col: 0 },
            SelPoint { line: p.line, col: cols.saturating_sub(1) },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Profile;
    use crate::mouse::SelectionKind;
    use std::collections::HashMap;

    fn make_test_tab(command: &str) -> TerminalTab {
        let p = Profile {
            command: vec!["sh".into(), "-c".into(), command.into()],
            cwd: None,
            tag: None,
            env: HashMap::new(),
            login_shell: false,
        };
        TerminalTab::new("test".into(), &p, "/bin/sh", 40, 10, 1_000).unwrap()
    }

    #[test]
    fn tab_can_be_created_and_polled() {
        let mut tab = make_test_tab("echo hello_world; sleep 0.1");
        let mut total = 0;
        for _ in 0..50 {
            match tab.poll_pty() {
                Ok(n) => total += n,
                Err(_) => break,
            }
            if total > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(total > 0, "should have read at least some bytes");
    }

    #[test]
    fn poll_pty_returns_zero_without_data() {
        // A long-sleeping command produces no output — poll should return
        // 0 immediately (non-blocking).
        let mut tab = make_test_tab("sleep 10");
        let start = std::time::Instant::now();
        let n = tab.poll_pty().unwrap();
        assert_eq!(n, 0);
        assert!(
            start.elapsed() < std::time::Duration::from_millis(100),
            "poll_pty should be non-blocking"
        );
    }

    #[test]
    fn resize_doesnt_panic() {
        let mut tab = make_test_tab("sleep 0.5");
        tab.resize(60, 20).unwrap();
        let (c, r) = tab.size();
        assert_eq!(c, 60);
        assert_eq!(r, 20);
    }

    #[test]
    fn ids_are_unique() {
        let a = make_test_tab("true");
        let b = make_test_tab("true");
        assert_ne!(a.id, b.id, "tab ids must be unique");
    }

    #[test]
    fn eof_marks_tab_dead() {
        let mut tab = make_test_tab("true"); // exits immediately
        for _ in 0..100 {
            let _ = tab.poll_pty();
            if tab.is_dead() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // It's possible (rare) that the reader thread hasn't yet delivered
        // the EOF; assert with a final drain.
        let _ = tab.poll_pty();
        assert!(tab.is_dead(), "tab should have observed EOF");
    }

    // ─── selection / clipboard / mode tests ─────────────────────────────

    fn wait_for_events(tab: &mut TerminalTab, secs: u64) -> Vec<Event> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        let mut seen = Vec::new();
        while std::time::Instant::now() < deadline {
            let _ = tab.poll_pty();
            seen.extend(tab.drain_events());
            if !seen.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        seen
    }

    #[test]
    fn selection_text_extracts_lines() {
        let mut tab = make_test_tab("printf 'alpha beta\\ngamma delta\\nepsilon'");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            let _ = tab.poll_pty();
            let _ = tab.drain_events();
            // Wait until the grid actually contains the text.
            let grid = tab.term.grid();
            let got = (0..grid.screen_lines()).any(|r| {
                (0..grid.columns())
                    .map(|c| grid[APoint {
                        line: ALine(-(grid.display_offset() as i32) + r as i32),
                        column: Column(c),
                    }].c)
                    .collect::<String>()
                    .contains("epsilon")
            });
            if got {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        // Select "beta" on row 0 through "delta" on row 1 (cols 6..=15).
        let mut sel = Selection::new();
        sel.begin(
            SelectionKind::Simple,
            SelPoint { line: 0, col: 6 },
        );
        sel.extend(SelPoint { line: 1, col: 10 });
        let text = tab.selection_text(&sel).unwrap();
        assert_eq!(text, "beta\ngamma delta");
    }

    #[test]
    fn selection_text_word_and_line_kinds() {
        let mut tab = make_test_tab("printf 'foo bar baz'");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            let _ = tab.poll_pty();
            let _ = tab.drain_events();
            let grid = tab.term.grid();
            let got = (0..grid.columns())
                .map(|c| grid[APoint { line: ALine(0), column: Column(c) }].c)
                .collect::<String>()
                .contains("baz");
            if got {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        // Word selection over "bar" (cols 4..6).
        let (a, b) = tab.expand_word(SelPoint { line: 0, col: 5 });
        assert_eq!((a.col, b.col), (4, 6));
        let mut sel = Selection::new();
        sel.set_span(SelectionKind::Word, a, b);
        assert_eq!(tab.selection_text(&sel).unwrap(), "bar");

        // Line selection grabs the whole row.
        let (a, b) = tab.expand_line(SelPoint { line: 0, col: 2 });
        let mut sel = Selection::new();
        sel.set_span(SelectionKind::Line, a, b);
        assert_eq!(tab.selection_text(&sel).unwrap(), "foo bar baz");
    }

    #[test]
    fn mouse_mode_bridges_from_decset() {
        let mut tab = make_test_tab("printf '\\033[?1000h\\033[?1006h'; sleep 0.3");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            let _ = tab.poll_pty();
            if tab.mouse_mode().x11 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let m = tab.mouse_mode();
        assert!(m.x11, "DECSET 1000 should enable x11 reporting");
        assert!(m.sgr, "DECSET 1006 should enable SGR");
        assert!(!m.motion);
    }

    #[test]
    fn bracketed_paste_bridges_from_decset() {
        let mut tab = make_test_tab("printf '\\033[?2004h'; sleep 0.3");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            let _ = tab.poll_pty();
            if tab.bracketed_paste() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(tab.bracketed_paste(), "DECSET 2004 should enable bracketed paste");
    }

    #[test]
    fn title_event_forwarded() {
        let mut tab = make_test_tab("printf '\\033]0;my-title\\007'; sleep 0.3");
        let events = wait_for_events(&mut tab, 3);
        assert!(
            events.iter().any(|e| matches!(e, Event::Title(t) if t == "my-title")),
            "expected Title event, got {events:?}"
        );
    }

    #[test]
    fn osc52_store_event_forwarded() {
        let mut tab = make_test_tab("printf '\\033]52;c;aGVsbG8=\\007'; sleep 0.3");
        let events = wait_for_events(&mut tab, 3);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::ClipboardStore(ty, text)
                    if matches!(ty, alacritty_terminal::term::ClipboardType::Clipboard)
                        && text == "hello")),
            "expected ClipboardStore event, got {events:?}"
        );
    }

    #[test]
    fn scroll_display_moves_viewport() {
        let mut tab = make_test_tab("seq 1 200; sleep 0.3");
        // Wait for the output to land.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            let _ = tab.poll_pty();
            if tab.term.grid().history_size() > 100 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(tab.at_bottom());
        tab.scroll_display(Scroll::Delta(10));
        assert_eq!(tab.display_offset(), 10);
        tab.scroll_display(Scroll::Top);
        assert_eq!(tab.display_offset(), tab.term.grid().history_size());
        tab.scroll_display(Scroll::Bottom);
        assert!(tab.at_bottom());
    }

    #[test]
    fn wheel_scrolls_scrollback_on_main_screen() {
        let mut tab = make_test_tab("seq 1 200; sleep 0.3");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            let _ = tab.poll_pty();
            if tab.term.grid().history_size() > 100 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        tab.wheel_scroll(true);
        assert_eq!(tab.display_offset(), 3);
        tab.wheel_scroll(false);
        assert_eq!(tab.display_offset(), 0);
    }

    #[test]
    fn grid_point_maps_viewport_into_scrollback() {
        let mut tab = make_test_tab("seq 1 200; sleep 0.3");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            let _ = tab.poll_pty();
            if tab.term.grid().history_size() > 100 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        tab.scroll_display(Scroll::Delta(20));
        // Top visible row maps to Line(-20); col passthrough.
        let p = tab.grid_point(0, 7);
        assert_eq!(p.line, -20);
        assert_eq!(p.col, 7);
        // Row 3 → Line(-17).
        assert_eq!(tab.grid_point(3, 0).line, -17);
    }
}
