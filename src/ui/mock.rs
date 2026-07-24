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


//! A no-op renderer used by tests and as a stub.
//!
//! Records the events it would have processed and the frames it would have
//! rendered. Useful for:
//! - Unit-testing the auto-detect / registry logic.
//! - Verifying the app loop drives `Renderer` correctly without standing up
//!   a real terminal or window.

use std::sync::mpsc::{self, Receiver, TryRecvError};

use anyhow::Result;

use crate::app::App;

use super::event::AppEvent;
use super::Renderer;

pub struct MockRenderer {
    pub name: String,
    pub events: Receiver<AppEvent>,
    pub frames_rendered: usize,
    pub size: (u16, u16),
}

impl MockRenderer {
    pub fn new(name: &str) -> Self {
        // We don't actually feed events from anywhere by default; tests
        // construct the renderer and then call `run` on an App they've
        // pre-populated with a Quit command.
        let (_, rx) = mpsc::channel();
        Self {
            name: name.to_string(),
            events: rx,
            frames_rendered: 0,
            size: (80, 24),
        }
    }

    pub fn with_events(name: &str, events: Receiver<AppEvent>) -> Self {
        Self {
            name: name.to_string(),
            events,
            frames_rendered: 0,
            size: (80, 24),
        }
    }
}

impl Renderer for MockRenderer {
    fn init(&mut self) -> Result<()> {
        Ok(())
    }

    fn fini(&mut self) -> Result<()> {
        Ok(())
    }

    fn poll_event(&mut self, _timeout_ms: u64) -> Result<Option<AppEvent>> {
        match self.events.try_recv() {
            Ok(ev) => Ok(Some(ev)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Ok(Some(AppEvent::Quit)),
        }
    }

    fn render(&mut self, _app: &mut App) -> Result<()> {
        self.frames_rendered += 1;
        Ok(())
    }

    fn size(&self) -> (u16, u16) {
        self.size
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn mock_renderer_renders_and_sizes() {
        let mut r = MockRenderer::new("test");
        assert_eq!(r.size(), (80, 24));
        // We can't easily call render() without an App; just verify init/fini.
        r.init().unwrap();
        r.fini().unwrap();
    }

    #[test]
    fn mock_renderer_returns_quit_when_channel_closed() {
        let (tx, rx) = mpsc::channel();
        drop(tx);
        let mut r = MockRenderer::with_events("test", rx);
        let ev = r.poll_event(0).unwrap();
        assert_eq!(ev, Some(AppEvent::Quit));
    }

    #[test]
    fn mock_renderer_returns_none_when_empty() {
        let (_tx, rx) = mpsc::channel::<AppEvent>();
        let mut r = MockRenderer::with_events("test", rx);
        let ev = r.poll_event(0).unwrap();
        assert_eq!(ev, None);
    }
}
