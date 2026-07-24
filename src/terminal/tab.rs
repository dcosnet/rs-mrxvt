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

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::vte::ansi::Processor;

use crate::config::Profile;

use super::pty::PtySession;

/// Event listener that absorbs all VT events. We drive redraws ourselves,
/// so we don't need to react to bell/OSC etc. in the MVP.
#[derive(Clone, Default)]
pub struct NoopListener;

impl EventListener for NoopListener {
    fn send_event(&self, _event: Event) {}
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
    pub tag: Option<String>,
    pub pty: PtySession,
    pub term: Term<NoopListener>,
    parser: Processor,
    /// Channel fed by the reader thread.
    rx: Receiver<Vec<u8>>,
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
        let term = Term::new(term_cfg, &size, NoopListener);

        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

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
            tag: profile.tag.clone(),
            pty,
            term,
            parser: Processor::new(),
            rx,
            _reader_thread: reader_thread,
            eof_seen: false,
        })
    }

    /// Drain whatever the reader thread has buffered and feed it to the VT emulator.
    ///
    /// Non-blocking: returns immediately with `Ok(0)` if no data is available.
    /// Returns `Ok(0)` with `eof_seen = true` after the child process exits.
    pub fn poll_pty(&mut self) -> anyhow::Result<usize> {
        let mut total = 0;
        loop {
            match self.rx.try_recv() {
                Ok(bytes) => {
                    if bytes.is_empty() {
                        // EOF sentinel from the reader thread.
                        self.eof_seen = true;
                        return Ok(total);
                    }
                    self.parser.advance(&mut self.term, &bytes);
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Profile;
    use std::collections::HashMap;

    fn make_test_tab(command: &str) -> TerminalTab {
        let p = Profile {
            command: vec!["sh".into(), "-c".into(), command.into()],
            cwd: None,
            tag: None,
            env: HashMap::new(),
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
}
