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


//! Backend-agnostic event types.
//!
//! Both `crossterm` (TUI backend) and `winit` (GPU/softbuffer backends) emit
//! their own key event types. To keep the rest of the app backend-agnostic,
//! we translate every input into [`AppEvent`] at the renderer boundary.
//!
//! This is the swap point that lets the same `App::handle_event` drive a TUI,
//! a wgpu window, or a CPU-rasterized softbuffer window.

use crossterm::event::{KeyCode as CrosstermCode, KeyEvent as CrosstermKey, KeyModifiers as CrosstermMods};

/// An input event from any backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppEvent {
    /// A key was pressed (or released, on backends that distinguish).
    Key(AppKeyEvent),
    /// The window/drawing area was resized (cols, rows).
    Resize(u16, u16),
    /// The window gained focus.
    FocusGained,
    /// The window lost focus.
    FocusLost,
    /// The user pasted text.
    Paste(String),
    /// A mouse event (translated from the backend's native type).
    Mouse(AppMouseEvent),
    /// The backend asked us to quit (window close, Ctrl+C in TUI, etc.).
    Quit,
}

/// A backend-agnostic mouse event.
///
/// Both crossterm and winit mouse events are translated into this type at
/// the renderer boundary. Cell coordinates are 0-indexed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppMouseEvent {
    pub button: crate::mouse::MouseButton,
    pub col: u32,
    pub row: u32,
    pub mods: crate::mouse::MouseMods,
    pub kind: crate::mouse::MouseEventKind,
}

impl From<crate::mouse::MouseEvent> for AppMouseEvent {
    fn from(ev: crate::mouse::MouseEvent) -> Self {
        Self {
            button: ev.button,
            col: ev.col,
            row: ev.row,
            mods: ev.mods,
            kind: ev.kind,
        }
    }
}

impl From<AppMouseEvent> for crate::mouse::MouseEvent {
    fn from(ev: AppMouseEvent) -> Self {
        Self {
            button: ev.button,
            col: ev.col,
            row: ev.row,
            mods: ev.mods,
            kind: ev.kind,
        }
    }
}

/// Modifier flags — `Copy + Eq + Hash` so we can use them in binding tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct AppModifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub super_key: bool,
}

impl AppModifiers {
    pub fn empty() -> Self {
        Self::default()
    }
}

impl std::fmt::Display for AppModifiers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = Vec::new();
        if self.ctrl { parts.push("Ctrl"); }
        if self.alt { parts.push("Alt"); }
        if self.shift { parts.push("Shift"); }
        if self.super_key { parts.push("Super"); }
        if parts.is_empty() {
            write!(f, "")
        } else {
            write!(f, "{}", parts.join("+"))
        }
    }
}

/// A normalized key (independent of any windowing library).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppKey {
    Char(char),
    Enter,
    Tab,
    BackTab,
    Backspace,
    Esc,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Delete,
    Insert,
    F(u8),
    Space,
}

/// A key event the app can act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AppKeyEvent {
    pub mods: AppModifiers,
    pub key: AppKey,
    /// True if this is a key-release event (some backends report these).
    /// TUI backends never set this; winit does.
    pub released: bool,
}

impl AppKeyEvent {
    pub fn new(mods: AppModifiers, key: AppKey) -> Self {
        Self { mods, key, released: false }
    }
}

// ─── crossterm translations ──────────────────────────────────────────────────

impl From<CrosstermMods> for AppModifiers {
    fn from(m: CrosstermMods) -> Self {
        Self {
            shift: m.contains(CrosstermMods::SHIFT),
            ctrl: m.contains(CrosstermMods::CONTROL),
            alt: m.contains(CrosstermMods::ALT),
            super_key: m.contains(CrosstermMods::SUPER),
        }
    }
}

impl From<CrosstermCode> for AppKey {
    fn from(c: CrosstermCode) -> Self {
        match c {
            CrosstermCode::Char(c) => AppKey::Char(c),
            CrosstermCode::Enter => AppKey::Enter,
            CrosstermCode::Tab => AppKey::Tab,
            CrosstermCode::BackTab => AppKey::BackTab,
            CrosstermCode::Backspace => AppKey::Backspace,
            CrosstermCode::Esc => AppKey::Esc,
            CrosstermCode::Left => AppKey::Left,
            CrosstermCode::Right => AppKey::Right,
            CrosstermCode::Up => AppKey::Up,
            CrosstermCode::Down => AppKey::Down,
            CrosstermCode::Home => AppKey::Home,
            CrosstermCode::End => AppKey::End,
            CrosstermCode::PageUp => AppKey::PageUp,
            CrosstermCode::PageDown => AppKey::PageDown,
            CrosstermCode::Delete => AppKey::Delete,
            CrosstermCode::Insert => AppKey::Insert,
            CrosstermCode::F(n) => AppKey::F(n),
            _ => AppKey::Char(' '), // unknown → space (rare)
        }
    }
}

impl From<CrosstermKey> for AppKeyEvent {
    fn from(ev: CrosstermKey) -> Self {
        Self {
            mods: AppModifiers::from(ev.modifiers),
            key: AppKey::from(ev.code),
            released: false,
        }
    }
}

impl From<CrosstermKey> for AppEvent {
    fn from(ev: CrosstermKey) -> Self {
        AppEvent::Key(ev.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crossterm_char_translates() {
        let ev = CrosstermKey::new(CrosstermCode::Char('a'), CrosstermMods::CONTROL);
        let app_ev: AppKeyEvent = ev.into();
        assert_eq!(app_ev.key, AppKey::Char('a'));
        assert!(app_ev.mods.ctrl);
        assert!(!app_ev.released);
    }

    #[test]
    fn crossterm_special_keys_translate() {
        let ev = CrosstermKey::new(CrosstermCode::Up, CrosstermMods::SHIFT);
        let app_ev: AppKeyEvent = ev.into();
        assert_eq!(app_ev.key, AppKey::Up);
        assert!(app_ev.mods.shift);
    }

    #[test]
    fn function_keys_translate() {
        let ev = CrosstermKey::new(CrosstermCode::F(11), CrosstermMods::empty());
        let app_ev: AppKeyEvent = ev.into();
        assert_eq!(app_ev.key, AppKey::F(11));
    }

    #[test]
    fn modifiers_display() {
        let m = AppModifiers { ctrl: true, alt: true, shift: false, super_key: false };
        assert_eq!(m.to_string(), "Ctrl+Alt");
        let m = AppModifiers::empty();
        assert_eq!(m.to_string(), "");
    }

    #[test]
    fn app_event_equality() {
        let a = AppEvent::Key(AppKeyEvent::new(AppModifiers::empty(), AppKey::Enter));
        let b = AppEvent::Key(AppKeyEvent::new(AppModifiers::empty(), AppKey::Enter));
        assert_eq!(a, b);
    }
}
