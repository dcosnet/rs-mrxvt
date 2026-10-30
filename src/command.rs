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


//! Commands that can be triggered from the command palette or keybindings.
//!
//! Every user-facing action is a variant of [`Command`]. The palette lists
//! them by display name, and the keybinding dispatcher maps key chords to
//! the same enum. Adding a new feature = adding a variant + a match arm in
//! [`App::handle_command`].

use std::fmt;

/// A user-invocable action.
///
/// Kept deliberately small for the MVP — broadcasting variants cover the
/// classic mrxvt "input to all tabs / input to tagged group" use cases.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Command {
    /// Create a new tab with the default profile.
    NewTab,
    /// Create a new tab with a named profile.
    NewTabProfile(String),
    /// Close the currently focused tab.
    CloseTab,
    /// Switch to the next tab (wraps around).
    NextTab,
    /// Switch to the previous tab (wraps around).
    PrevTab,
    /// Switch to tab by 0-indexed position.
    GotoTab(usize),
    /// Toggle broadcasting to *all* tabs.
    ToggleBroadcastAll,
    /// Toggle broadcasting to a tagged group.
    ToggleBroadcastGroup(String),
    /// Tag the active tab with a group name.
    TagActiveTab(String),
    /// Copy the active selection to the system clipboard.
    CopySelection,
    /// Paste the system clipboard into the terminal.
    PasteClipboard,
    /// Paste the X11 primary (middle-click) selection into the terminal.
    PastePrimary,
    /// Scroll the viewport one line up (into scrollback).
    ScrollLineUp,
    /// Scroll the viewport one line down (toward the live bottom).
    ScrollLineDown,
    /// Scroll the viewport one page up.
    ScrollPageUp,
    /// Scroll the viewport one page down.
    ScrollPageDown,
    /// Jump the viewport to the top of the scrollback.
    ScrollToTop,
    /// Jump the viewport back to the live bottom.
    ScrollToBottom,
    /// Open the command palette.
    OpenPalette,
    /// Reset the active terminal (clear + send RIS).
    ResetTerminal,
    /// Quit the application.
    Quit,
}

/// A `(Command, display name)` pair for the palette.
pub struct CommandEntry {
    pub command: Command,
    pub name: &'static str,
    pub category: &'static str,
}

impl Command {
    /// Parse a macro-spec string into a [`Command`].
    ///
    /// Bare variant names (`"ResetTerminal"`, `"Quit"`, …) cover the
    /// parameterless commands; parameterized variants take the argument
    /// after a colon (`"NewTabProfile:zsh"`, `"ToggleBroadcastGroup:web"`,
    /// `"TagActiveTab:web"`, `"GotoTab:3"`). This is what turns the
    /// config `[macros]` table into real keybindings.
    pub fn parse_spec(spec: &str) -> Option<Command> {
        let (head, arg) = match spec.split_once(':') {
            Some((h, a)) => (h.trim(), Some(a.trim())),
            None => (spec.trim(), None),
        };
        // Parameterized variants consume the colon argument; parameterless
        // ones reject it so a stray `"Quit:"` isn't silently accepted.
        match (head, arg) {
            ("GotoTab", Some(a)) => Some(Command::GotoTab(a.parse().ok()?)),
            ("NewTabProfile", Some(a)) => Some(Command::NewTabProfile(a.to_string())),
            ("ToggleBroadcastGroup", Some(a)) => Some(Command::ToggleBroadcastGroup(a.to_string())),
            ("TagActiveTab", Some(a)) => Some(Command::TagActiveTab(a.to_string())),
            ("NewTab", None) => Some(Command::NewTab),
            ("CloseTab", None) => Some(Command::CloseTab),
            ("NextTab", None) => Some(Command::NextTab),
            ("PrevTab", None) => Some(Command::PrevTab),
            ("ToggleBroadcastAll", None) => Some(Command::ToggleBroadcastAll),
            ("CopySelection", None) => Some(Command::CopySelection),
            ("PasteClipboard", None) => Some(Command::PasteClipboard),
            ("PastePrimary", None) => Some(Command::PastePrimary),
            ("ScrollLineUp", None) => Some(Command::ScrollLineUp),
            ("ScrollLineDown", None) => Some(Command::ScrollLineDown),
            ("ScrollPageUp", None) => Some(Command::ScrollPageUp),
            ("ScrollPageDown", None) => Some(Command::ScrollPageDown),
            ("ScrollToTop", None) => Some(Command::ScrollToTop),
            ("ScrollToBottom", None) => Some(Command::ScrollToBottom),
            ("OpenPalette", None) => Some(Command::OpenPalette),
            ("ResetTerminal", None) => Some(Command::ResetTerminal),
            ("Quit", None) => Some(Command::Quit),
            _ => None,
        }
    }

    /// The default, parameterless commands shown in the palette.
    pub fn defaults() -> Vec<CommandEntry> {
        vec![
            CommandEntry { command: Command::NewTab,             name: "New Tab",                    category: "Tabs" },
            CommandEntry { command: Command::CloseTab,           name: "Close Tab",                  category: "Tabs" },
            CommandEntry { command: Command::NextTab,            name: "Next Tab",                   category: "Tabs" },
            CommandEntry { command: Command::PrevTab,            name: "Previous Tab",               category: "Tabs" },
            CommandEntry { command: Command::GotoTab(0),         name: "Go to Tab 1",                category: "Tabs" },
            CommandEntry { command: Command::GotoTab(1),         name: "Go to Tab 2",                category: "Tabs" },
            CommandEntry { command: Command::GotoTab(2),         name: "Go to Tab 3",                category: "Tabs" },
            CommandEntry { command: Command::GotoTab(3),         name: "Go to Tab 4",                category: "Tabs" },
            CommandEntry { command: Command::GotoTab(4),         name: "Go to Tab 5",                category: "Tabs" },
            CommandEntry { command: Command::ToggleBroadcastAll, name: "Toggle Broadcast (All)",     category: "Broadcast" },
            CommandEntry { command: Command::CopySelection,      name: "Copy Selection",             category: "Clipboard" },
            CommandEntry { command: Command::PasteClipboard,     name: "Paste from Clipboard",       category: "Clipboard" },
            CommandEntry { command: Command::PastePrimary,       name: "Paste Primary Selection",    category: "Clipboard" },
            CommandEntry { command: Command::ScrollLineUp,       name: "Scroll Line Up",             category: "Scrollback" },
            CommandEntry { command: Command::ScrollLineDown,     name: "Scroll Line Down",           category: "Scrollback" },
            CommandEntry { command: Command::ScrollPageUp,       name: "Scroll Page Up",             category: "Scrollback" },
            CommandEntry { command: Command::ScrollPageDown,     name: "Scroll Page Down",           category: "Scrollback" },
            CommandEntry { command: Command::ScrollToTop,        name: "Scroll to Top",              category: "Scrollback" },
            CommandEntry { command: Command::ScrollToBottom,     name: "Scroll to Bottom",           category: "Scrollback" },
            CommandEntry { command: Command::ResetTerminal,      name: "Reset Terminal",             category: "Terminal" },
            CommandEntry { command: Command::OpenPalette,        name: "Open Command Palette",       category: "UI" },
            CommandEntry { command: Command::Quit,               name: "Quit",                       category: "App" },
        ]
    }
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Command::NewTab => write!(f, "New Tab"),
            Command::NewTabProfile(p) => write!(f, "New Tab ({p})"),
            Command::CloseTab => write!(f, "Close Tab"),
            Command::NextTab => write!(f, "Next Tab"),
            Command::PrevTab => write!(f, "Previous Tab"),
            Command::GotoTab(i) => write!(f, "Go to Tab {}", i + 1),
            Command::ToggleBroadcastAll => write!(f, "Toggle Broadcast (All)"),
            Command::ToggleBroadcastGroup(g) => write!(f, "Toggle Broadcast ({g})"),
            Command::TagActiveTab(g) => write!(f, "Tag Tab → {g}"),
            Command::CopySelection => write!(f, "Copy Selection"),
            Command::PasteClipboard => write!(f, "Paste from Clipboard"),
            Command::PastePrimary => write!(f, "Paste Primary Selection"),
            Command::ScrollLineUp => write!(f, "Scroll Line Up"),
            Command::ScrollLineDown => write!(f, "Scroll Line Down"),
            Command::ScrollPageUp => write!(f, "Scroll Page Up"),
            Command::ScrollPageDown => write!(f, "Scroll Page Down"),
            Command::ScrollToTop => write!(f, "Scroll to Top"),
            Command::ScrollToBottom => write!(f, "Scroll to Bottom"),
            Command::OpenPalette => write!(f, "Open Command Palette"),
            Command::ResetTerminal => write!(f, "Reset Terminal"),
            Command::Quit => write!(f, "Quit"),
        }
    }
}
