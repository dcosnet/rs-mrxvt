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


//! Session-level state shared between the App, the PTY reader, and the
//! renderers.
//!
//! Bundles the moving parts that the renderers and input router need to
//! share but that don't belong on `TerminalManager` (which is purely about
//! PTY/tab lifecycle):
//!
//! - [`MouseState`] — current mouse mode (set by the child program via
//!   DECSET escape sequences) and the active selection.
//! - [`HyperlinkStore`] — OSC 8 inline hyperlinks, populated by the PTY
//!   reader as it scans the byte stream.
//! - [`ImageStore`] — inline images (iTerm2 + Sixel), populated by the
//!   PTY reader. Gated on the `images` feature.
//!
//! The renderers read this state each frame to draw selection highlights,
//! underline hyperlinks, and composite inline images.

use std::time::{Duration, Instant};

use crate::hyperlinks::HyperlinkStore;
use crate::mouse::{MouseMode, Selection, SelectionKind};

#[cfg(feature = "images")]
use crate::images::ImageStore;

/// Multi-click window: clicks within this gap count as double/triple clicks.
pub const MULTI_CLICK_WINDOW: Duration = Duration::from_millis(400);

/// Tracks click cadence to distinguish single / double / triple clicks.
#[derive(Debug, Default)]
pub struct ClickTracker {
    last_at: Option<Instant>,
    count: u8,
}

impl ClickTracker {
    /// Register a click; returns the selection kind it implies.
    pub fn register(&mut self, now: Instant) -> SelectionKind {
        let within = self
            .last_at
            .map(|t| now.duration_since(t) < MULTI_CLICK_WINDOW)
            .unwrap_or(false);
        self.count = if within { self.count.saturating_add(1) } else { 1 };
        self.last_at = Some(now);
        match self.count % 3 {
            1 => SelectionKind::Simple,
            2 => SelectionKind::Word,
            _ => SelectionKind::Line,
        }
    }

    /// The current click count in the active cadence window.
    pub fn count(&self) -> u8 {
        self.count
    }
}

/// Bundle of session state shared between App, PTY reader, and renderers.
pub struct SessionState {
    /// Current mouse reporting mode (set by the child via DECSET).
    ///
    /// Note: the authoritative source is the active tab's `TermMode`
    /// (see `TerminalTab::mouse_mode`); this copy exists for renderers
    /// and headless use.
    pub mouse_mode: MouseMode,
    /// Active text selection (when mouse reporting is off).
    pub selection: Selection,
    /// Click-cadence tracker for double/triple-click selection.
    pub clicks: ClickTracker,
    /// OSC 8 hyperlinks seen in the PTY stream.
    pub hyperlinks: HyperlinkStore,
    /// Inline images (iTerm2 + Sixel). Only available with `--features images`.
    #[cfg(feature = "images")]
    pub images: ImageStore,
}

impl Default for SessionState {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionState {
    pub fn new() -> Self {
        Self {
            mouse_mode: MouseMode::default(),
            selection: Selection::new(),
            clicks: ClickTracker::default(),
            hyperlinks: HyperlinkStore::new(),
            #[cfg(feature = "images")]
            images: ImageStore::new(),
        }
    }

    /// Reset all state (e.g. when the active tab changes).
    pub fn reset(&mut self) {
        self.selection.clear();
        self.hyperlinks.clear();
        #[cfg(feature = "images")]
        self.images.clear();
        // Note: mouse_mode is per-program, not per-tab; we keep it across
        // tab switches. A more sophisticated impl would track per-tab.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_is_empty() {
        let s = SessionState::new();
        assert!(!s.mouse_mode.any_reporting());
        assert!(!s.selection.is_active());
        assert!(s.hyperlinks.is_empty());
        #[cfg(feature = "images")]
        assert!(s.images.is_empty());
    }

    #[test]
    fn reset_clears_everything() {
        let mut s = SessionState::new();
        s.selection.begin(crate::mouse::SelectionKind::Simple, crate::mouse::SelPoint { line: 0, col: 0 });
        s.hyperlinks.add(crate::hyperlinks::Hyperlink {
            uri: "x".into(),
            id: None,
            title: None,
            start: (0, 0),
            end: (1, 1),
        });
        s.reset();
        assert!(!s.selection.is_active());
        assert!(s.hyperlinks.is_empty());
    }

    #[test]
    fn mouse_mode_can_be_toggled() {
        let mut s = SessionState::new();
        s.mouse_mode.x11 = true;
        assert!(s.mouse_mode.any_reporting());
        s.mouse_mode.x11 = false;
        assert!(!s.mouse_mode.any_reporting());
    }

    #[test]
    fn click_tracker_cycles_kinds() {
        let mut t = ClickTracker::default();
        let now = Instant::now();
        assert_eq!(t.register(now), crate::mouse::SelectionKind::Simple);
        assert_eq!(t.register(now + Duration::from_millis(100)), crate::mouse::SelectionKind::Word);
        assert_eq!(t.register(now + Duration::from_millis(200)), crate::mouse::SelectionKind::Line);
        // Fourth click restarts the cycle.
        assert_eq!(t.register(now + Duration::from_millis(300)), crate::mouse::SelectionKind::Simple);
    }

    #[test]
    fn click_tracker_resets_after_window() {
        let mut t = ClickTracker::default();
        let now = Instant::now();
        assert_eq!(t.register(now), crate::mouse::SelectionKind::Simple);
        // Beyond the 400ms window → single click again.
        assert_eq!(
            t.register(now + Duration::from_millis(500)),
            crate::mouse::SelectionKind::Simple
        );
    }
}
