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

    /// Drop the super key (which mouse reporting has no bit for) and map
    /// to the backend-agnostic [`crate::mouse::MouseMods`].
    pub fn into_mouse_mods(self) -> crate::mouse::MouseMods {
        crate::mouse::MouseMods {
            shift: self.shift,
            ctrl: self.ctrl,
            alt: self.alt,
        }
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

/// Translate a crossterm `KeyCode` into an [`AppKey`], returning `None` for
/// keys this app doesn't understand (the caller drops them instead of
/// injecting a bogus keystroke into the shell).
pub fn app_key_from_crossterm(c: CrosstermCode) -> Option<AppKey> {
    match c {
        CrosstermCode::Char(c) => Some(AppKey::Char(c)),
        CrosstermCode::Enter => Some(AppKey::Enter),
        CrosstermCode::Tab => Some(AppKey::Tab),
        CrosstermCode::BackTab => Some(AppKey::BackTab),
        CrosstermCode::Backspace => Some(AppKey::Backspace),
        CrosstermCode::Esc => Some(AppKey::Esc),
        CrosstermCode::Left => Some(AppKey::Left),
        CrosstermCode::Right => Some(AppKey::Right),
        CrosstermCode::Up => Some(AppKey::Up),
        CrosstermCode::Down => Some(AppKey::Down),
        CrosstermCode::Home => Some(AppKey::Home),
        CrosstermCode::End => Some(AppKey::End),
        CrosstermCode::PageUp => Some(AppKey::PageUp),
        CrosstermCode::PageDown => Some(AppKey::PageDown),
        CrosstermCode::Delete => Some(AppKey::Delete),
        CrosstermCode::Insert => Some(AppKey::Insert),
        CrosstermCode::F(n) => Some(AppKey::F(n)),
        _ => None,
    }
}

impl From<CrosstermCode> for AppKey {
    fn from(c: CrosstermCode) -> Self {
        app_key_from_crossterm(c).unwrap_or(AppKey::Char(' '))
    }
}

impl AppKeyEvent {
    /// Like `From<CrosstermKey>`, but drops unrecognised keys instead of
    /// mapping them to a space (which would type into the shell).
    pub fn try_from_crossterm(ev: CrosstermKey) -> Option<Self> {
        Some(Self {
            mods: AppModifiers::from(ev.modifiers),
            key: app_key_from_crossterm(ev.code)?,
            released: false,
        })
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

    use super::{AppEvent, AppKey, AppKeyEvent, AppModifiers, AppMouseEvent};

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

    /// Tracks mouse state across winit events.
    ///
    /// winit's `MouseInput` events carry no cursor position, so the last
    /// `CursorMoved` position is remembered here and used for button events.
    /// `left_down` distinguishes drag motion from hover motion.
    #[derive(Clone, Copy, Default)]
    pub struct MouseTracker {
        last_pos: winit::dpi::PhysicalPosition<f64>,
        left_down: bool,
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
    /// `mouse` is updated by cursor/button events and used to synthesize
    /// `AppEvent::Mouse` in *grid* coordinates (the 1-cell tab bar at the top
    /// and 1-cell status bar at the bottom of the GUI layout are stripped,
    /// matching what `App::handle_mouse` and the child program expect).
    /// `cols`/`rows` are the grid area dimensions from `Renderer::size()`.
    /// Returns `None` for events that don't map to an `AppEvent` (most
    /// events — winit emits a lot of internal events we don't care about).
    #[allow(clippy::too_many_arguments)] // trackers + geometry; grouping would obscure the call sites
    pub fn translate_event(
        event: &Event<()>,
        tracker: &mut ModifierTracker,
        mouse: &mut MouseTracker,
        elwt: &EventLoopWindowTarget<()>,
        cell_w: u32,
        cell_h: u32,
        cols: u16,
        rows: u16,
    ) -> Option<AppEvent> {
        match event {
            Event::WindowEvent { event: win_ev, .. } => {
                translate_window_event(win_ev, tracker, mouse, elwt, cell_w, cell_h, cols, rows)
            }
            _ => None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn translate_window_event(
        win_ev: &WindowEvent,
        tracker: &mut ModifierTracker,
        mouse: &mut MouseTracker,
        _elwt: &EventLoopWindowTarget<()>,
        cell_w: u32,
        cell_h: u32,
        cols: u16,
        rows: u16,
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

            WindowEvent::CursorMoved { position, .. } => {
                mouse.last_pos = *position;
                let button = if mouse.left_down {
                    crate::mouse::MouseButton::Left
                } else {
                    crate::mouse::MouseButton::None
                };
                mouse_app_event(mouse, button, crate::mouse::MouseEventKind::Motion,
                                tracker, cell_w, cell_h, cols, rows)
            }

            WindowEvent::MouseInput { state, button, .. } => {
                let app_button = match button {
                    winit::event::MouseButton::Left => crate::mouse::MouseButton::Left,
                    winit::event::MouseButton::Middle => crate::mouse::MouseButton::Middle,
                    winit::event::MouseButton::Right => crate::mouse::MouseButton::Right,
                    // Back/Forward/Other buttons have no terminal meaning.
                    _ => crate::mouse::MouseButton::None,
                };
                if app_button == crate::mouse::MouseButton::Left {
                    mouse.left_down = *state == ElementState::Pressed;
                }
                let kind = match state {
                    ElementState::Pressed => crate::mouse::MouseEventKind::Press,
                    ElementState::Released => crate::mouse::MouseEventKind::Release,
                };
                mouse_app_event(mouse, app_button, kind, tracker, cell_w, cell_h, cols, rows)
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let up = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => *y > 0.0,
                    winit::event::MouseScrollDelta::PixelDelta(p) => p.y > 0.0,
                };
                let button = if up {
                    crate::mouse::MouseButton::WheelUp
                } else {
                    crate::mouse::MouseButton::WheelDown
                };
                mouse_app_event(mouse, button, crate::mouse::MouseEventKind::Press,
                                tracker, cell_w, cell_h, cols, rows)
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

    /// Build an [`AppMouseEvent`] from the tracker's last cursor position,
    /// converting physical pixels to grid cells (tab bar stripped) and
    /// dropping events outside the grid area.
    #[allow(clippy::too_many_arguments)]
    fn mouse_app_event(
        mouse: &MouseTracker,
        button: crate::mouse::MouseButton,
        kind: crate::mouse::MouseEventKind,
        tracker: &ModifierTracker,
        cell_w: u32,
        cell_h: u32,
        cols: u16,
        rows: u16,
    ) -> Option<AppEvent> {
        if cell_w == 0 || cell_h == 0 || cols == 0 || rows == 0 {
            return None;
        }
        let col = (mouse.last_pos.x / cell_w as f64) as i64;
        // Strip the tab bar row, then index the remaining area.
        let row = ((mouse.last_pos.y - cell_h as f64) / cell_h as f64) as i64;
        if col < 0 || row < 0 {
            return None; // above the grid (tab bar / title bar)
        }
        let col = (col as u32).min(cols as u32 - 1);
        let row = (row as u32).min(rows as u32 - 1);
        Some(AppEvent::Mouse(AppMouseEvent {
            button,
            col,
            row,
            mods: tracker.to_app_modifiers().into_mouse_mods(),
            kind,
        }))
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
    fn unknown_keys_are_dropped_not_typed_as_space() {
        // KeyCode::Null (and other unrecognised codes) used to become
        // AppKey::Char(' ') — injecting a space keystroke into the shell.
        let ev = CrosstermKey::new(CrosstermCode::Null, CrosstermMods::empty());
        assert!(AppKeyEvent::try_from_crossterm(ev).is_none());
        let ev = CrosstermKey::new(CrosstermCode::CapsLock, CrosstermMods::empty());
        assert!(AppKeyEvent::try_from_crossterm(ev).is_none());
        // Known keys still translate.
        let ev = CrosstermKey::new(CrosstermCode::Char('x'), CrosstermMods::empty());
        let app_ev = AppKeyEvent::try_from_crossterm(ev).unwrap();
        assert_eq!(app_ev.key, AppKey::Char('x'));
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
