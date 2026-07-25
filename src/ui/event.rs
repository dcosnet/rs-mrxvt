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

// ─── winit translations (gpu feature: wgpu + softbuffer backends) ───────────

#[cfg(feature = "gpu")]
pub mod winit_translate {
    use winit::event::{ElementState, Event, KeyEvent, WindowEvent};
    use winit::event_loop::EventLoopWindowTarget;
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    use super::{AppEvent, AppKey, AppKeyEvent, AppModifiers};

    /// Tracks the current keyboard modifier state.
    ///
    /// Updated by `WindowEvent::ModifiersChanged` events, read when
    /// translating `WindowEvent::KeyboardInput` events.
    #[derive(Clone, Copy, Default)]
    pub struct ModifierTracker {
        mods: ModifiersState,
    }

    impl ModifierTracker {
        pub fn update(&mut self, mods: ModifiersState) {
            self.mods = mods;
        }

        pub fn to_app_modifiers(&self) -> AppModifiers {
            AppModifiers {
                shift: self.mods.shift_key(),
                ctrl: self.mods.control_key(),
                alt: self.mods.alt_key(),
                super_key: self.mods.super_key(),
            }
        }
    }

    /// Translate a winit `KeyEvent` into an `AppKeyEvent`.
    ///
    /// Returns `None` for keys we don't recognise (rare hardware keys, dead
    /// keys, etc.) — the caller silently drops them.
    pub fn winit_key_to_app_key(ev: &KeyEvent, mods: AppModifiers) -> Option<AppKeyEvent> {
        let key = match &ev.logical_key {
            Key::Character(s) => {
                // Take the first character. For most keypresses this is a
                // single-char string; for edge cases (e.g. dead-key sequences
                // producing multi-char strings) we take the first.
                AppKey::Char(s.chars().next().unwrap_or(' '))
            }
            Key::Named(n) => named_to_app_key(n)?,
            _ => return None,
        };
        Some(AppKeyEvent {
            mods,
            key,
            released: ev.state == ElementState::Released,
        })
    }

    fn named_to_app_key(n: &NamedKey) -> Option<AppKey> {
        Some(match n {
            NamedKey::Enter => AppKey::Enter,
            NamedKey::Tab => AppKey::Tab,
            NamedKey::Space => AppKey::Space,
            NamedKey::Backspace => AppKey::Backspace,
            NamedKey::Escape => AppKey::Esc,
            NamedKey::ArrowUp => AppKey::Up,
            NamedKey::ArrowDown => AppKey::Down,
            NamedKey::ArrowLeft => AppKey::Left,
            NamedKey::ArrowRight => AppKey::Right,
            NamedKey::Home => AppKey::Home,
            NamedKey::End => AppKey::End,
            NamedKey::PageUp => AppKey::PageUp,
            NamedKey::PageDown => AppKey::PageDown,
            NamedKey::Insert => AppKey::Insert,
            NamedKey::Delete => AppKey::Delete,
            NamedKey::F1 => AppKey::F(1),
            NamedKey::F2 => AppKey::F(2),
            NamedKey::F3 => AppKey::F(3),
            NamedKey::F4 => AppKey::F(4),
            NamedKey::F5 => AppKey::F(5),
            NamedKey::F6 => AppKey::F(6),
            NamedKey::F7 => AppKey::F(7),
            NamedKey::F8 => AppKey::F(8),
            NamedKey::F9 => AppKey::F(9),
            NamedKey::F10 => AppKey::F(10),
            NamedKey::F11 => AppKey::F(11),
            NamedKey::F12 => AppKey::F(12),
            _ => return None,
        })
    }

    /// Translate a top-level winit `Event` into an `AppEvent`.
    ///
    /// `tracker` is updated in place when modifier-state events arrive.
    /// Returns `None` for events that don't map to an `AppEvent` (most
    /// events — winit emits a lot of internal events we don't care about).
    pub fn translate_event(
        event: &Event<()>,
        tracker: &mut ModifierTracker,
        _elwt: &EventLoopWindowTarget<()>,
    ) -> Option<AppEvent> {
        match event {
            Event::WindowEvent { event: win_ev, .. } => {
                translate_window_event(win_ev, tracker)
            }
            _ => None,
        }
    }

    fn translate_window_event(
        win_ev: &WindowEvent,
        tracker: &mut ModifierTracker,
    ) -> Option<AppEvent> {
        match win_ev {
            WindowEvent::CloseRequested => Some(AppEvent::Quit),

            WindowEvent::Resized(size) => {
                Some(AppEvent::Resize(size.width as u16, size.height as u16))
            }

            WindowEvent::Focused(focused) => {
                if *focused {
                    Some(AppEvent::FocusGained)
                } else {
                    Some(AppEvent::FocusLost)
                }
            }

            WindowEvent::ModifiersChanged(mods) => {
                tracker.update(mods.state());
                None // modifier changes are state, not actions
            }

            WindowEvent::KeyboardInput { event, .. } => {
                let mut mods = tracker.to_app_modifiers();

                // Fallback: if the tracker hasn't seen a ModifiersChanged
                // event yet (which can happen on the very first key press
                // on some X11/Wayland compositors), infer shift from the
                // character's case. winit sends `Key::Character("T")` when
                // shift is held, so an uppercase letter implies shift.
                // This catches the common Ctrl+Shift+<letter> hotkeys even
                // when modifier tracking hasn't warmed up.
                if !mods.shift {
                    if let winit::keyboard::Key::Character(s) = &event.logical_key {
                        if s.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false) {
                            mods.shift = true;
                        }
                    }
                }

                winit_key_to_app_key(event, mods).map(AppEvent::Key)
            }

            _ => None,
        }
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
