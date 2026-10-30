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


//! Keybinding definitions and matching.
//!
//! Chords are expressed as a string like `"Ctrl+Shift+T"` for readability
//! in config files, and parsed into a [`KeyChord`] struct for matching
//! against raw crossterm events.

use std::collections::HashMap;
use std::fmt;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::command::Command;

/// Modifier flags. Matches `KeyModifiers` but `Copy + Eq + Hash`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

impl Modifiers {
    pub fn from_crossterm(m: KeyModifiers) -> Self {
        Self {
            shift: m.contains(KeyModifiers::SHIFT),
            ctrl: m.contains(KeyModifiers::CONTROL),
            alt: m.contains(KeyModifiers::ALT),
        }
    }
}

impl fmt::Display for Modifiers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::new();
        if self.ctrl { parts.push("Ctrl"); }
        if self.alt { parts.push("Alt"); }
        if self.shift { parts.push("Shift"); }
        if parts.is_empty() {
            write!(f, "")
        } else {
            write!(f, "{}", parts.join("+"))
        }
    }
}

/// A normalized key chord.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub mods: Modifiers,
    pub key: KeyName,
}

/// A small, hashable key enum covering the keys we actually bind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyName {
    Char(char),
    Enter,
    Tab,
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
}

impl KeyChord {
    pub fn from_crossterm(ev: KeyEvent) -> Option<Self> {
        let mods = Modifiers::from_crossterm(ev.modifiers);
        let key = match ev.code {
            KeyCode::Char(c) => {
                // Normalize: when shift is held (or the char is already uppercase),
                // store as uppercase so it matches `parse_chord` output.
                let normalized = if mods.shift || c.is_ascii_uppercase() {
                    c.to_ascii_uppercase()
                } else if c.is_ascii_lowercase() {
                    // Even without shift, store lowercase letters as uppercase
                    // in the chord — the shift modifier distinguishes them.
                    // This matches parse_chord which uppercases the trailing char.
                    c.to_ascii_uppercase()
                } else {
                    c
                };
                KeyName::Char(normalized)
            }
            KeyCode::Enter => KeyName::Enter,
            KeyCode::Tab => KeyName::Tab,
            KeyCode::BackTab => KeyName::Tab,
            KeyCode::Backspace => KeyName::Backspace,
            KeyCode::Esc => KeyName::Esc,
            KeyCode::Left => KeyName::Left,
            KeyCode::Right => KeyName::Right,
            KeyCode::Up => KeyName::Up,
            KeyCode::Down => KeyName::Down,
            KeyCode::Home => KeyName::Home,
            KeyCode::End => KeyName::End,
            KeyCode::PageUp => KeyName::PageUp,
            KeyCode::PageDown => KeyName::PageDown,
            KeyCode::Delete => KeyName::Delete,
            KeyCode::Insert => KeyName::Insert,
            KeyCode::F(n) => KeyName::F(n),
            _ => return None,
        };
        Some(Self { mods, key })
    }
}

impl fmt::Display for KeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = self.mods.to_string();
        let k = match self.key {
            KeyName::Char(c) => {
                let s: String = c.to_uppercase().collect();
                s
            }
            KeyName::Enter => "Enter".into(),
            KeyName::Tab => "Tab".into(),
            KeyName::Backspace => "Backspace".into(),
            KeyName::Esc => "Esc".into(),
            KeyName::Left => "Left".into(),
            KeyName::Right => "Right".into(),
            KeyName::Up => "Up".into(),
            KeyName::Down => "Down".into(),
            KeyName::Home => "Home".into(),
            KeyName::End => "End".into(),
            KeyName::PageUp => "PageUp".into(),
            KeyName::PageDown => "PageDown".into(),
            KeyName::Delete => "Delete".into(),
            KeyName::Insert => "Insert".into(),
            KeyName::F(n) => format!("F{n}"),
        };
        if m.is_empty() {
            write!(f, "{k}")
        } else {
            write!(f, "{m}+{k}")
        }
    }
}

/// Parse a chord string like `"Ctrl+Shift+T"` into a [`KeyChord`].
///
/// Modifiers are case-insensitive; the trailing key char is uppercased.
/// Returns `None` if the string is malformed.
pub fn parse_chord(s: &str) -> Option<KeyChord> {
    let parts: Vec<&str> = s.split('+').map(|p| p.trim()).collect();
    if parts.is_empty() {
        return None;
    }
    let mut mods = Modifiers::default();
    for p in &parts[..parts.len() - 1] {
        match p.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods.ctrl = true,
            "alt" => mods.alt = true,
            "shift" => mods.shift = true,
            _ => return None,
        }
    }
    let last = parts.last()?;
    let key = match last.to_ascii_lowercase().as_str() {
        "enter" | "return" => KeyName::Enter,
        "tab" => KeyName::Tab,
        "backspace" => KeyName::Backspace,
        "esc" | "escape" => KeyName::Esc,
        "left" => KeyName::Left,
        "right" => KeyName::Right,
        "up" => KeyName::Up,
        "down" => KeyName::Down,
        "home" => KeyName::Home,
        "end" => KeyName::End,
        "pageup" | "pgup" => KeyName::PageUp,
        "pagedown" | "pgdn" => KeyName::PageDown,
        "delete" | "del" => KeyName::Delete,
        "insert" | "ins" => KeyName::Insert,
        "space" => KeyName::Char(' '),
        s if s.starts_with('f') && s.len() >= 2 => {
            let n: u8 = s[1..].parse().ok()?;
            KeyName::F(n)
        }
        s if s.len() == 1 => {
            KeyName::Char(s.chars().next()?.to_ascii_uppercase())
        }
        _ => return None,
    };
    Some(KeyChord { mods, key })
}

/// A binding table maps chords to commands.
#[derive(Debug, Clone, Default)]
pub struct KeyBindingTable {
    map: HashMap<KeyChord, Command>,
}

impl KeyBindingTable {
    /// Build the default mrxvt-style bindings.
    pub fn defaults() -> Self {
        let mut t = Self::default();
        // Classic mrxvt-style Ctrl+Shift+* bindings.
        t.bind("Ctrl+Shift+T", Command::NewTab);
        t.bind("Ctrl+Shift+W", Command::CloseTab);
        t.bind("Ctrl+Shift+I", Command::ToggleBroadcastAll);
        t.bind("Ctrl+Shift+P", Command::OpenPalette);
        t.bind("Ctrl+Tab", Command::NextTab);
        t.bind("Ctrl+Shift+Tab", Command::PrevTab);

        // Alt+1..9 → GotoTab(0..8). Alt+0 → GotoTab(9).
        // "Get" a virtual terminal by index — direct jump.
        for i in 1..=9u8 {
            let chord = KeyChord {
                mods: Modifiers { alt: true, ctrl: false, shift: false },
                key: KeyName::Char((b'0' + i) as char),
            };
            t.map.insert(chord, Command::GotoTab((i - 1) as usize));
        }
        let chord = KeyChord {
            mods: Modifiers { alt: true, ctrl: false, shift: false },
            key: KeyName::Char('0'),
        };
        t.map.insert(chord, Command::GotoTab(9));

        // Alt+N → New terminal (alias for Ctrl+Shift+T).
        t.bind("Alt+N", Command::NewTab);

        // Alt+Shift+X → Close the currently-focused tab.
        t.bind("Alt+Shift+X", Command::CloseTab);

        // Alt+Z → Launch a zsh tab (secondary shell).
        t.bind("Alt+Z", Command::NewTabProfile("zsh".into()));

        // Alt+Left / Alt+Right → shuffle backward / forward through tabs.
        // (Wraps around at the ends.)
        t.bind("Alt+Left", Command::PrevTab);
        t.bind("Alt+Right", Command::NextTab);

        // Clipboard: the modern xterm/ghostty-class chords, plus the
        // classic Insert-key aliases.
        t.bind("Ctrl+Shift+C", Command::CopySelection);
        t.bind("Ctrl+Shift+V", Command::PasteClipboard);
        t.bind("Ctrl+Insert", Command::CopySelection);
        t.bind("Shift+Insert", Command::PasteClipboard);

        // Scrollback navigation (xterm-style Shift-modified navigation keys).
        t.bind("Shift+PageUp", Command::ScrollPageUp);
        t.bind("Shift+PageDown", Command::ScrollPageDown);
        t.bind("Shift+Home", Command::ScrollToTop);
        t.bind("Shift+End", Command::ScrollToBottom);

        t
    }

    /// Bind a chord string to a command. No-op if the chord is malformed.
    pub fn bind(&mut self, chord_str: &str, cmd: Command) {
        if let Some(chord) = parse_chord(chord_str) {
            self.map.insert(chord, cmd);
        }
    }

    /// Apply the config `[macros]` table on top of the current bindings.
    ///
    /// Each entry maps a chord string to a command spec (see
    /// [`Command::parse_spec`]); later entries override earlier ones and
    /// defaults. Malformed entries are logged and skipped.
    pub fn apply_macros(&mut self, macros: &std::collections::HashMap<String, String>) {
        for (chord_str, spec) in macros {
            let Some(chord) = parse_chord(chord_str) else {
                log::warn!("ignoring macro with unparseable chord {chord_str:?}");
                continue;
            };
            let Some(cmd) = Command::parse_spec(spec) else {
                log::warn!("ignoring macro {chord_str:?}: unknown command spec {spec:?}");
                continue;
            };
            self.map.insert(chord, cmd);
        }
    }

    /// Resolve a crossterm key event to a command, if any.
    pub fn resolve(&self, ev: KeyEvent) -> Option<Command> {
        let chord = KeyChord::from_crossterm(ev)?;
        self.map.get(&chord).cloned()
    }

    /// Iterate all bindings (for display in the palette's help screen).
    pub fn iter(&self) -> impl Iterator<Item = (KeyChord, &Command)> {
        self.map.iter().map(|(k, v)| (*k, v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_chords() {
        let c = parse_chord("Ctrl+Shift+T").unwrap();
        assert!(c.mods.ctrl);
        assert!(c.mods.shift);
        assert!(!c.mods.alt);
        assert_eq!(c.key, KeyName::Char('T'));

        let c = parse_chord("Alt+1").unwrap();
        assert!(c.mods.alt);
        assert_eq!(c.key, KeyName::Char('1'));

        let c = parse_chord("F11").unwrap();
        assert!(!c.mods.ctrl && !c.mods.alt && !c.mods.shift);
        assert_eq!(c.key, KeyName::F(11));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_chord("").is_none());
        assert!(parse_chord("Foo+Bar").is_none());
        assert!(parse_chord("Ctrl+").is_none());
    }

    #[test]
    fn defaults_resolve_known_chords() {
        let t = KeyBindingTable::defaults();
        let ev = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        assert_eq!(t.resolve(ev), Some(Command::NewTab));
    }

    #[test]
    fn alt_number_switches_tab() {
        let t = KeyBindingTable::defaults();
        let ev = KeyEvent::new(KeyCode::Char('3'), KeyModifiers::ALT);
        assert_eq!(t.resolve(ev), Some(Command::GotoTab(2)));
    }

    #[test]
    fn bind_overrides_defaults() {
        let mut t = KeyBindingTable::defaults();
        t.bind("Ctrl+Shift+T", Command::Quit);
        let ev = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        assert_eq!(t.resolve(ev), Some(Command::Quit));
    }

    #[test]
    fn alt_n_creates_new_tab() {
        let t = KeyBindingTable::defaults();
        // 'n' or 'N' should both resolve (case-insensitive in chord parsing).
        let ev_lower = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::ALT);
        let ev_upper = KeyEvent::new(KeyCode::Char('N'), KeyModifiers::ALT);
        assert_eq!(t.resolve(ev_lower), Some(Command::NewTab));
        assert_eq!(t.resolve(ev_upper), Some(Command::NewTab));
    }

    #[test]
    fn alt_right_arrow_shuffles_forward() {
        let t = KeyBindingTable::defaults();
        let ev = KeyEvent::new(KeyCode::Right, KeyModifiers::ALT);
        assert_eq!(t.resolve(ev), Some(Command::NextTab));
    }

    #[test]
    fn alt_left_arrow_shuffles_backward() {
        let t = KeyBindingTable::defaults();
        let ev = KeyEvent::new(KeyCode::Left, KeyModifiers::ALT);
        assert_eq!(t.resolve(ev), Some(Command::PrevTab));
    }

    #[test]
    fn alt_zero_goes_to_tab_ten() {
        let t = KeyBindingTable::defaults();
        let ev = KeyEvent::new(KeyCode::Char('0'), KeyModifiers::ALT);
        assert_eq!(t.resolve(ev), Some(Command::GotoTab(9)));
    }

    #[test]
    fn plain_arrow_keys_not_bound_by_default() {
        // Without Alt, arrows should not match any binding (they pass through
        // to the shell as ANSI sequences).
        let t = KeyBindingTable::defaults();
        let ev = KeyEvent::new(KeyCode::Right, KeyModifiers::empty());
        assert_eq!(t.resolve(ev), None);
    }

    #[test]
    fn alt_shift_x_closes_tab() {
        let t = KeyBindingTable::defaults();
        // Both lowercase 'x' with Alt+Shift and uppercase 'X' should work,
        // since chord parsing normalizes to uppercase.
        let ev_lower = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT | KeyModifiers::SHIFT);
        let ev_upper = KeyEvent::new(KeyCode::Char('X'), KeyModifiers::ALT | KeyModifiers::SHIFT);
        assert_eq!(t.resolve(ev_lower), Some(Command::CloseTab));
        assert_eq!(t.resolve(ev_upper), Some(Command::CloseTab));
    }

    #[test]
    fn alt_z_launches_zsh_profile() {
        let t = KeyBindingTable::defaults();
        let ev_lower = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT);
        let ev_upper = KeyEvent::new(KeyCode::Char('Z'), KeyModifiers::ALT);
        assert_eq!(
            t.resolve(ev_lower),
            Some(Command::NewTabProfile("zsh".into()))
        );
        assert_eq!(
            t.resolve(ev_upper),
            Some(Command::NewTabProfile("zsh".into()))
        );
    }

    #[test]
    fn plain_alt_x_not_bound() {
        // Alt+X (no shift) should not close the tab — would be too easy to hit
        // by accident. Only Alt+Shift+X closes.
        let t = KeyBindingTable::defaults();
        let ev = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT);
        assert_eq!(t.resolve(ev), None);
    }

    #[test]
    fn copy_paste_chords_bound() {
        let t = KeyBindingTable::defaults();
        // crossterm reports Ctrl+Shift+C with an uppercase char.
        let ev = KeyEvent::new(KeyCode::Char('C'), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        assert_eq!(t.resolve(ev), Some(Command::CopySelection));
        let ev = KeyEvent::new(KeyCode::Char('V'), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        assert_eq!(t.resolve(ev), Some(Command::PasteClipboard));
    }

    #[test]
    fn insert_key_clipboard_aliases_bound() {
        let t = KeyBindingTable::defaults();
        let ev = KeyEvent::new(KeyCode::Insert, KeyModifiers::CONTROL);
        assert_eq!(t.resolve(ev), Some(Command::CopySelection));
        let ev = KeyEvent::new(KeyCode::Insert, KeyModifiers::SHIFT);
        assert_eq!(t.resolve(ev), Some(Command::PasteClipboard));
    }

    #[test]
    fn shift_page_keys_scroll_scrollback() {
        let t = KeyBindingTable::defaults();
        let ev = KeyEvent::new(KeyCode::PageUp, KeyModifiers::SHIFT);
        assert_eq!(t.resolve(ev), Some(Command::ScrollPageUp));
        let ev = KeyEvent::new(KeyCode::PageDown, KeyModifiers::SHIFT);
        assert_eq!(t.resolve(ev), Some(Command::ScrollPageDown));
        // Unshifted PageUp passes through to the child.
        let ev = KeyEvent::new(KeyCode::PageUp, KeyModifiers::empty());
        assert_eq!(t.resolve(ev), None);
    }

    #[test]
    fn shift_home_end_jump_scrollback() {
        let t = KeyBindingTable::defaults();
        let ev = KeyEvent::new(KeyCode::Home, KeyModifiers::SHIFT);
        assert_eq!(t.resolve(ev), Some(Command::ScrollToTop));
        let ev = KeyEvent::new(KeyCode::End, KeyModifiers::SHIFT);
        assert_eq!(t.resolve(ev), Some(Command::ScrollToBottom));
    }

    #[test]
    fn macros_add_and_override_bindings() {
        use std::collections::HashMap;
        let mut macros = HashMap::new();
        macros.insert("Ctrl+Shift+R".to_string(), "ResetTerminal".to_string());
        macros.insert("Ctrl+Shift+G".to_string(), "ToggleBroadcastGroup:web".to_string());
        macros.insert("Alt+9".to_string(), "TagActiveTab:cluster".to_string());
        // Override a default.
        macros.insert("Ctrl+Shift+T".to_string(), "Quit".to_string());

        let mut t = KeyBindingTable::defaults();
        t.apply_macros(&macros);

        let ev = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        assert_eq!(t.resolve(ev), Some(Command::ResetTerminal));
        let ev = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        assert_eq!(
            t.resolve(ev),
            Some(Command::ToggleBroadcastGroup("web".into()))
        );
        let ev = KeyEvent::new(KeyCode::Char('9'), KeyModifiers::ALT);
        assert_eq!(t.resolve(ev), Some(Command::TagActiveTab("cluster".into())));
        let ev = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        assert_eq!(t.resolve(ev), Some(Command::Quit));
    }

    #[test]
    fn macros_with_garbage_are_skipped() {
        use std::collections::HashMap;
        let mut macros = HashMap::new();
        macros.insert("Not+A+Chord".to_string(), "Quit".to_string());
        macros.insert("Ctrl+Shift+R".to_string(), "NoSuchCommand".to_string());
        macros.insert("Ctrl+Shift+T".to_string(), "GotoTab:notanumber".to_string());
        macros.insert("Ctrl+Shift+Y".to_string(), "TagActiveTab".to_string()); // missing arg

        let mut t = KeyBindingTable::defaults();
        t.apply_macros(&macros);
        let ev = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        assert_eq!(t.resolve(ev), None);
        let ev = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        assert_eq!(t.resolve(ev), Some(Command::NewTab), "garbage macros must not clobber defaults");
    }

    #[test]
    fn parse_spec_handles_parameterized_forms() {
        assert_eq!(
            Command::parse_spec("NewTabProfile:zsh"),
            Some(Command::NewTabProfile("zsh".into()))
        );
        assert_eq!(
            Command::parse_spec("GotoTab:3"),
            Some(Command::GotoTab(3))
        );
        assert_eq!(Command::parse_spec("Quit"), Some(Command::Quit));
        assert_eq!(Command::parse_spec("Quit:"), None, "Quit takes no arg");
        assert_eq!(Command::parse_spec("TagActiveTab"), None, "missing arg");
        assert_eq!(Command::parse_spec("  ResetTerminal  "), Some(Command::ResetTerminal));
    }
}
