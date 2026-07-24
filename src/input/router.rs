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


//! Input router.
//!
//! The router sits between crossterm events and the terminal manager. It:
//! 1. Checks if a key event matches a binding → dispatches the `Command`.
//! 2. Otherwise translates the key event into raw bytes (ANSI escape sequences
//!    for special keys, UTF-8 for printable chars) and sends them via the
//!    manager's `route_input`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::command::Command;
use crate::input::bindings::KeyBindingTable;
use crate::terminal::manager::{Action, BroadcastTarget, TerminalManager};

use crate::config::Config;

/// What should the app do after handling this input?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAction {
    /// Keep running; redraw.
    Continue,
    /// App should quit.
    Quit,
    /// Open the command palette overlay.
    OpenPalette,
    /// Input was a command (palette etc.); the manager doesn't need raw bytes.
    Handled,
}

pub struct InputRouter {
    pub bindings: KeyBindingTable,
}

impl InputRouter {
    pub fn new(bindings: KeyBindingTable) -> Self {
        Self { bindings }
    }

    /// Dispatch a key event. Returns the recommended app action.
    pub fn handle(
        &mut self,
        ev: KeyEvent,
        manager: &mut TerminalManager,
        config: &Config,
        cols: u16,
        rows: u16,
    ) -> InputAction {
        // 1. Check bindings first.
        if let Some(cmd) = self.bindings.resolve(ev) {
            match &cmd {
                Command::OpenPalette => return InputAction::OpenPalette,
                Command::Quit => return InputAction::Quit,
                other => {
                    let action = manager.execute(other, cols, rows, config);
                    if action == Action::Quit {
                        return InputAction::Quit;
                    }
                    return InputAction::Handled;
                }
            }
        }

        // 2. Translate to bytes and route to PTY(s).
        if let Some(bytes) = key_to_bytes(ev) {
            if let Err(e) = manager.route_input(&bytes) {
                log::debug!("route_input failed: {e}");
            }
        }
        InputAction::Continue
    }
}

/// Translate a crossterm key event into the bytes a Unix terminal expects.
///
/// Honors the current broadcast mode implicitly — the caller's `route_input`
/// will fan out if needed.
pub fn key_to_bytes(ev: KeyEvent) -> Option<Vec<u8>> {
    let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
    let alt = ev.modifiers.contains(KeyModifiers::ALT);
    let shift = ev.modifiers.contains(KeyModifiers::SHIFT);

    let bytes = match ev.code {
        KeyCode::Char(c) => {
            let c = if c.is_ascii_alphabetic() {
                if shift { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() }
            } else {
                c
            };

            if ctrl {
                // Ctrl+letter → 0x01..0x1A; Ctrl+@ → 0x00, Ctrl+[ → 0x1b, etc.
                let b = ctrl_char_to_byte(c)?;
                let mut v = vec![b];
                if alt {
                    v.insert(0, 0x1b);
                }
                v
            } else {
                let mut s = c.to_string();
                if alt {
                    s = format!("\x1b{}", s);
                }
                s.into_bytes()
            }
        }
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => {
            // DEL (^?) — matches what xterm sends with default config.
            if alt { b"\x1b\x7f".to_vec() } else { b"\x7f".to_vec() }
        }
        KeyCode::Esc => b"\x1b".to_vec(),
        KeyCode::Left  => csi_arrow("D", ctrl, shift),
        KeyCode::Right => csi_arrow("C", ctrl, shift),
        KeyCode::Up    => csi_arrow("A", ctrl, shift),
        KeyCode::Down  => csi_arrow("B", ctrl, shift),
        KeyCode::Home => {
            let seq = if ctrl { "\x1b[1;5H" } else { "\x1b[H" };
            seq.as_bytes().to_vec()
        }
        KeyCode::End => {
            let seq = if ctrl { "\x1b[1;5F" } else { "\x1b[F" };
            seq.as_bytes().to_vec()
        }
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::F(n) => f_key_seq(n),
        _ => return None,
    };
    Some(bytes)
}

/// Build a CSI arrow-key sequence with optional Ctrl/Shift modifiers.
fn csi_arrow(direction: &str, ctrl: bool, shift: bool) -> Vec<u8> {
    let seq = match (ctrl, shift) {
        (true, _)    => format!("\x1b[1;5{direction}"),
        (false, true) => format!("\x1b[1;2{direction}"),
        (false, false) => format!("\x1b[{direction}"),
    };
    seq.into_bytes()
}

fn ctrl_char_to_byte(c: char) -> Option<u8> {
    let b = c as u32;
    if b < 0x80 {
        let mask = b & 0x1f;
        Some(mask as u8)
    } else {
        None
    }
}

fn f_key_seq(n: u8) -> Vec<u8> {
    const F_KEY_SEQS: &[&[u8]] = &[
        b"\x1bOP",   // F1
        b"\x1bOQ",   // F2
        b"\x1bOR",   // F3
        b"\x1bOS",   // F4
        b"\x1b[15~", // F5
        b"\x1b[17~", // F6
        b"\x1b[18~", // F7
        b"\x1b[19~", // F8
        b"\x1b[20~", // F9
        b"\x1b[21~", // F10
        b"\x1b[23~", // F11
        b"\x1b[24~", // F12
    ];
    F_KEY_SEQS.get(n as usize - 1)
        .map(|s| s.to_vec())
        .unwrap_or_default()
}

/// True if the chord would start broadcasting (so the UI can show a HUD).
pub fn is_broadcast_active(target: &BroadcastTarget) -> bool {
    !matches!(target, BroadcastTarget::Active)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printable_char_lowercased_unless_shift() {
        let ev = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::empty());
        assert_eq!(key_to_bytes(ev).unwrap(), b"a");

        let ev = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SHIFT);
        assert_eq!(key_to_bytes(ev).unwrap(), b"A");
    }

    #[test]
    fn ctrl_c_yields_0x03() {
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(key_to_bytes(ev).unwrap(), vec![0x03]);
    }

    #[test]
    fn ctrl_c_with_alt_prepends_esc() {
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL | KeyModifiers::ALT);
        assert_eq!(key_to_bytes(ev).unwrap(), vec![0x1b, 0x03]);
    }

    #[test]
    fn alt_x_prepends_esc() {
        let ev = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT);
        assert_eq!(key_to_bytes(ev).unwrap(), b"\x1bx");
    }

    #[test]
    fn enter_yields_cr() {
        let ev = KeyEvent::new(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(key_to_bytes(ev).unwrap(), b"\r");
    }

    #[test]
    fn arrow_keys_emit_csi() {
        let ev = KeyEvent::new(KeyCode::Up, KeyModifiers::empty());
        assert_eq!(key_to_bytes(ev).unwrap(), b"\x1b[A");
        let ev = KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL);
        assert_eq!(key_to_bytes(ev).unwrap(), b"\x1b[1;5C");
    }

    #[test]
    fn backtab_emits_csi_z() {
        let ev = KeyEvent::new(KeyCode::BackTab, KeyModifiers::empty());
        assert_eq!(key_to_bytes(ev).unwrap(), b"\x1b[Z");
    }

    #[test]
    fn function_keys_emit_xterm_sequences() {
        let ev = KeyEvent::new(KeyCode::F(1), KeyModifiers::empty());
        assert_eq!(key_to_bytes(ev).unwrap(), b"\x1bOP");
        let ev = KeyEvent::new(KeyCode::F(11), KeyModifiers::empty());
        assert_eq!(key_to_bytes(ev).unwrap(), b"\x1b[23~");
    }

    #[test]
    fn router_handles_known_chord() {
        let mut manager = TerminalManager::new(&Config::default());
        let p = crate::config::Profile::default();
        let _ = manager.open_tab(&p, Some("t".into()), 40, 10);
        let config = Config::default();
        let mut router = InputRouter::new(KeyBindingTable::defaults());

        // Ctrl+Shift+T → NewTab
        let ev = KeyEvent::new(KeyCode::Char('T'), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        let action = router.handle(ev, &mut manager, &config, 40, 10);
        assert_eq!(action, InputAction::Handled);
        assert_eq!(manager.tabs.len(), 2);
    }

    #[test]
    fn router_routes_printable_bytes() {
        let mut manager = TerminalManager::new(&Config::default());
        let p = crate::config::Profile::default();
        let _ = manager.open_tab(&p, Some("t".into()), 40, 10);
        let config = Config::default();
        let mut router = InputRouter::new(KeyBindingTable::defaults());

        // Plain 'x' — should be routed as bytes (no command).
        let ev = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::empty());
        let action = router.handle(ev, &mut manager, &config, 40, 10);
        assert_eq!(action, InputAction::Continue);
    }

    #[test]
    fn router_returns_open_palette_for_palette_chord() {
        let mut manager = TerminalManager::new(&Config::default());
        let config = Config::default();
        let mut router = InputRouter::new(KeyBindingTable::defaults());
        let ev = KeyEvent::new(KeyCode::Char('P'), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        let action = router.handle(ev, &mut manager, &config, 40, 10);
        assert_eq!(action, InputAction::OpenPalette);
    }
}
