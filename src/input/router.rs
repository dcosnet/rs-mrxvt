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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputAction {
    /// Keep running; redraw.
    Continue,
    /// App should quit.
    Quit,
    /// Open the command palette overlay.
    OpenPalette,
    /// Input was a command (palette etc.); the manager doesn't need raw bytes.
    Handled,
    /// A resolved binding the router cannot execute itself (clipboard
    /// commands need the App's clipboard backend and session selection).
    /// The App must run it via `App::run_command`.
    Command(Command),
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
                // Clipboard commands need the App layer (clipboard backend +
                // session selection); hand them up instead of dropping them.
                Command::CopySelection | Command::PasteClipboard | Command::PastePrimary => {
                    return InputAction::Command(cmd.clone())
                }
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
            if !bytes.is_empty() {
                if let Err(e) = manager.route_input(&bytes) {
                    log::debug!("route_input failed: {e}");
                }
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
        KeyCode::Enter => {
            if alt { b"\x1b\r".to_vec() } else { b"\r".to_vec() }
        }
        KeyCode::Tab => {
            if alt { b"\x1b\t".to_vec() } else { b"\t".to_vec() }
        }
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => {
            // DEL (^?) — matches what xterm sends with default config.
            if alt { b"\x1b\x7f".to_vec() } else { b"\x7f".to_vec() }
        }
        KeyCode::Esc => {
            // Alt+Esc = ESC ESC (the standard meta prefix applied to Esc).
            if alt { b"\x1b\x1b".to_vec() } else { b"\x1b".to_vec() }
        }
        KeyCode::Left  => csi_arrow("D", ctrl, shift, alt),
        KeyCode::Right => csi_arrow("C", ctrl, shift, alt),
        KeyCode::Up    => csi_arrow("A", ctrl, shift, alt),
        KeyCode::Down  => csi_arrow("B", ctrl, shift, alt),
        KeyCode::Home => mod_aware_csi("H", ctrl, shift, alt),
        KeyCode::End => mod_aware_csi("F", ctrl, shift, alt),
        KeyCode::PageUp => mod_aware_tilde(5, ctrl, shift, alt),
        KeyCode::PageDown => mod_aware_tilde(6, ctrl, shift, alt),
        KeyCode::Delete => mod_aware_tilde(3, ctrl, shift, alt),
        KeyCode::Insert => mod_aware_tilde(2, ctrl, shift, alt),
        KeyCode::F(n) => f_key_seq(n),
        _ => return None,
    };
    Some(bytes)
}

/// Build a CSI arrow-key sequence with xterm's modifier parameter
/// (1 = none, 2 = shift, 3 = alt, 4 = shift+alt, 5 = ctrl, 6 = shift+ctrl,
/// 7 = ctrl+alt, 8 = shift+ctrl+alt).
fn csi_arrow(direction: &str, ctrl: bool, shift: bool, alt: bool) -> Vec<u8> {
    mod_aware_csi(direction, ctrl, shift, alt)
}

/// Shared builder for `CSI 1;<mods><final>` sequences (arrows, Home, End).
fn mod_aware_csi(final_byte: &str, ctrl: bool, shift: bool, alt: bool) -> Vec<u8> {
    let code = modifier_code(shift, alt, ctrl);
    let seq = if code == 1 {
        format!("\x1b[{final_byte}")
    } else {
        format!("\x1b[1;{code}{final_byte}")
    };
    seq.into_bytes()
}

/// Shared builder for `CSI <n>;<mods>~` sequences (PageUp/PageDown/Delete/Insert).
fn mod_aware_tilde(n: u8, ctrl: bool, shift: bool, alt: bool) -> Vec<u8> {
    let code = modifier_code(shift, alt, ctrl);
    if code == 1 {
        format!("\x1b[{n}~").into_bytes()
    } else {
        format!("\x1b[{n};{code}~").into_bytes()
    }
}

/// xterm's numeric modifier encoding for CSI sequences.
fn modifier_code(shift: bool, alt: bool, ctrl: bool) -> u8 {
    1 + (shift as u8) + ((alt as u8) << 1) + ((ctrl as u8) << 2)
}

fn ctrl_char_to_byte(c: char) -> Option<u8> {
    let b = c as u32;
    if b >= 0x80 {
        return None;
    }
    // Follow xterm's table exactly: letters and @[\]^_ fold with & 0x1f,
    // space and the shifted-punctuation digits map to their C0 counterparts.
    match b {
        0x20 | 0x32 => Some(0x00), // Ctrl+Space / Ctrl+2 → NUL
        0x40..=0x5f => Some((b & 0x1f) as u8), // @A-Z[\]^_ → classic C0
        0x61..=0x7a => Some((b & 0x1f) as u8), // a-z → same as A-Z
        0x3f | 0x38 => Some(0x7f), // Ctrl+? / Ctrl+8 → DEL
        0x2f | 0x37 => Some(0x1f), // Ctrl+/ / Ctrl+7 → US
        0x33 => Some(0x1b),        // Ctrl+3 → ESC
        0x34 => Some(0x1c),        // Ctrl+4 → FS
        0x35 => Some(0x1d),        // Ctrl+5 → GS
        0x36 => Some(0x1e),        // Ctrl+6 → RS
        _ => None,                 // no control form (digits/punctuation) → drop
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
    // F(0) isn't a real key; `get` handles it via the checked subtract below.
    if n == 0 {
        return Vec::new();
    }
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

    #[test]
    fn clipboard_chords_return_command_for_app_layer() {
        let mut manager = TerminalManager::new(&Config::default());
        let config = Config::default();
        let mut router = InputRouter::new(KeyBindingTable::defaults());

        // Ctrl+Shift+C must come back as a Command for App::run_command —
        // the router has no clipboard access. Regression: these used to be
        // silently swallowed by manager.execute (which ignores clipboard
        // commands), so the copy/paste bindings did nothing.
        let ev = KeyEvent::new(KeyCode::Char('C'), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        assert_eq!(
            router.handle(ev, &mut manager, &config, 40, 10),
            InputAction::Command(Command::CopySelection)
        );
        let ev = KeyEvent::new(KeyCode::Char('V'), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        assert_eq!(
            router.handle(ev, &mut manager, &config, 40, 10),
            InputAction::Command(Command::PasteClipboard)
        );
    }

    #[test]
    fn alt_arrow_uses_modifier_parameter() {
        // Alt+Up → CSI 1;3A (xterm's modifier 3 = alt).
        let ev = KeyEvent::new(KeyCode::Up, KeyModifiers::ALT);
        assert_eq!(key_to_bytes(ev).unwrap(), b"\x1b[1;3A");
        // Ctrl+Shift+Left → CSI 1;6D.
        let ev = KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        assert_eq!(key_to_bytes(ev).unwrap(), b"\x1b[1;6D");
    }

    #[test]
    fn alt_enter_prepends_esc() {
        let ev = KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT);
        assert_eq!(key_to_bytes(ev).unwrap(), b"\x1b\r");
    }

    #[test]
    fn modified_home_end_page_keys() {
        // Alt+Home → CSI 1;3H.
        let ev = KeyEvent::new(KeyCode::Home, KeyModifiers::ALT);
        assert_eq!(key_to_bytes(ev).unwrap(), b"\x1b[1;3H");
        // Ctrl+PageUp → CSI 5;5~.
        let ev = KeyEvent::new(KeyCode::PageUp, KeyModifiers::CONTROL);
        assert_eq!(key_to_bytes(ev).unwrap(), b"\x1b[5;5~");
        // Unmodified forms stay compact.
        let ev = KeyEvent::new(KeyCode::PageUp, KeyModifiers::empty());
        assert_eq!(key_to_bytes(ev).unwrap(), b"\x1b[5~");
        let ev = KeyEvent::new(KeyCode::Home, KeyModifiers::empty());
        assert_eq!(key_to_bytes(ev).unwrap(), b"\x1b[H");
    }

    #[test]
    fn ctrl_digits_match_xterm_table() {
        // Ctrl+2 → NUL (the old blanket `& 0x1f` produced 0x12 = Ctrl+R).
        let ev = KeyEvent::new(KeyCode::Char('2'), KeyModifiers::CONTROL);
        assert_eq!(key_to_bytes(ev).unwrap(), vec![0x00]);
        // Ctrl+3 → ESC.
        let ev = KeyEvent::new(KeyCode::Char('3'), KeyModifiers::CONTROL);
        assert_eq!(key_to_bytes(ev).unwrap(), vec![0x1b]);
        // Ctrl+7 → US.
        let ev = KeyEvent::new(KeyCode::Char('7'), KeyModifiers::CONTROL);
        assert_eq!(key_to_bytes(ev).unwrap(), vec![0x1f]);
        // Ctrl+8 → DEL.
        let ev = KeyEvent::new(KeyCode::Char('8'), KeyModifiers::CONTROL);
        assert_eq!(key_to_bytes(ev).unwrap(), vec![0x7f]);
        // Ctrl+9: no control form in xterm's table → dropped.
        let ev = KeyEvent::new(KeyCode::Char('9'), KeyModifiers::CONTROL);
        assert_eq!(key_to_bytes(ev), None);
    }

    #[test]
    fn unknown_function_keys_do_not_route_empty_writes() {
        let ev = KeyEvent::new(KeyCode::F(0), KeyModifiers::empty());
        assert_eq!(key_to_bytes(ev).unwrap(), Vec::<u8>::new());
        let ev = KeyEvent::new(KeyCode::F(13), KeyModifiers::empty());
        assert_eq!(key_to_bytes(ev).unwrap(), Vec::<u8>::new());
    }
}
