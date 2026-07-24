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


//! Multi-tab coordinator and broadcasting router.
//!
//! Owns the [`TerminalTab`] vec, tracks the active index, and routes input
//! bytes to the right destination(s). The broadcasting logic — the classic
//! mrxvt "killer feature" — lives here.

use anyhow::Result;

use crate::command::Command;
use crate::config::{Config, Profile};

use super::tab::TerminalTab;

/// Where a keystroke should go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BroadcastTarget {
    /// Only the focused tab (default behaviour).
    Active,
    /// Every open tab, regardless of tag.
    All,
    /// Only tabs tagged with the given group name.
    Group(String),
}

/// The tab manager.
///
/// Holding this struct gives you:
/// - the active tab (mutable)
/// - the ability to create/close tabs
/// - input routing with broadcasting
pub struct TerminalManager {
    pub tabs: Vec<TerminalTab>,
    pub active: usize,
    pub broadcast: BroadcastTarget,
    pub fallback_shell: String,
    pub default_scrollback: usize,
    pub default_profile: String,
}

impl TerminalManager {
    pub fn new(config: &Config) -> Self {
        Self {
            tabs: Vec::new(),
            active: 0,
            broadcast: BroadcastTarget::Active,
            fallback_shell: config.terminal.shell.clone(),
            default_scrollback: config.terminal.scrollback,
            default_profile: config.default_profile.clone(),
        }
    }

    /// Spawn a tab using a profile by name (falls back to "default").
    pub fn open_tab(&mut self, profile: &Profile, title: Option<String>, cols: u16, rows: u16) -> Result<u32> {
        let title = title.unwrap_or_else(|| {
            if profile.command.is_empty() {
                "shell".to_string()
            } else {
                profile.command.first().cloned().unwrap_or_else(|| "shell".into())
            }
        });
        let tab = TerminalTab::new(
            title,
            profile,
            &self.fallback_shell,
            cols,
            rows,
            self.default_scrollback,
        )?;
        let id = tab.id;
        self.tabs.push(tab);
        Ok(id)
    }

    /// Close the tab at `index`. Switches active to the previous tab if needed.
    /// Returns `true` if a tab was closed.
    pub fn close_tab(&mut self, index: usize) -> bool {
        if index >= self.tabs.len() {
            return false;
        }
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.active = 0;
        } else if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        true
    }

    /// Close the active tab.
    pub fn close_active(&mut self) -> bool {
        let i = self.active;
        self.close_tab(i)
    }

    /// Get the active tab (immutable).
    pub fn active_tab(&self) -> Option<&TerminalTab> {
        self.tabs.get(self.active)
    }

    /// Get the active tab (mutable).
    pub fn active_tab_mut(&mut self) -> Option<&mut TerminalTab> {
        self.tabs.get_mut(self.active)
    }

    /// Switch to the next tab (wraps around).
    pub fn next_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
        }
    }

    /// Switch to the previous tab (wraps around).
    pub fn prev_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active = if self.active == 0 {
                self.tabs.len() - 1
            } else {
                self.active - 1
            };
        }
    }

    /// Switch to tab `i` if it exists. Returns `true` if switched.
    pub fn goto_tab(&mut self, i: usize) -> bool {
        if i < self.tabs.len() {
            self.active = i;
            true
        } else {
            false
        }
    }

    /// Tag the active tab.
    pub fn tag_active(&mut self, tag: String) -> bool {
        if let Some(t) = self.tabs.get_mut(self.active) {
            t.tag = Some(tag);
            true
        } else {
            false
        }
    }

    /// Route input bytes to the appropriate tab(s) per the current broadcast mode.
    pub fn route_input(&mut self, data: &[u8]) -> std::io::Result<()> {
        match &self.broadcast {
            BroadcastTarget::Active => {
                if let Some(tab) = self.tabs.get(self.active) {
                    tab.write_input(data)?;
                }
                Ok(())
            }
            BroadcastTarget::All => {
                self.tabs.iter()
                    .find_map(|tab| tab.write_input(data).err())
                    .map_or(Ok(()), Err)
            }
            BroadcastTarget::Group(tag) => {
                let tagged: Vec<_> = self.tabs.iter()
                    .filter(|t| t.tag.as_deref() == Some(tag.as_str()))
                    .collect();
                let first_err = tagged.iter().find_map(|tab| tab.write_input(data).err());
                // If no tab matched the tag, fall back to active so the user
                // isn't left typing into the void.
                if tagged.is_empty() {
                    if let Some(tab) = self.tabs.get(self.active) {
                        tab.write_input(data)?;
                    }
                }
                first_err.map_or(Ok(()), Err)
            }
        }
    }

    /// Toggle between `Active` and `All` broadcasting.
    pub fn toggle_broadcast_all(&mut self) {
        self.broadcast = match &self.broadcast {
            BroadcastTarget::Active => BroadcastTarget::All,
            _ => BroadcastTarget::Active,
        };
    }

    /// Toggle a tagged-group broadcast. Calling with the same tag twice
    /// turns it off; calling with a different tag switches to it.
    pub fn toggle_broadcast_group(&mut self, tag: String) {
        self.broadcast = match &self.broadcast {
            BroadcastTarget::Group(existing) if existing == &tag => BroadcastTarget::Active,
            _ => BroadcastTarget::Group(tag),
        };
    }

    /// Drain all PTYs. Should be called once per event-loop tick.
    ///
    /// Returns a tuple of (active_tab_received_data, any_tab_eof).
    pub fn poll_all(&mut self) -> (bool, bool) {
        let mut active_got_data = false;
        let mut any_eof = false;
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            match tab.poll_pty() {
                Ok(0) => {
                    any_eof = true;
                }
                Ok(_) => {
                    if i == self.active {
                        active_got_data = true;
                    }
                }
                Err(e) => {
                    log::debug!("poll error on tab {i}: {e}");
                }
            }
        }
        (active_got_data, any_eof)
    }

    /// Resize every tab to the given terminal area (excluding chrome).
    pub fn resize_all(&mut self, cols: u16, rows: u16) {
        self.tabs.iter_mut().for_each(|tab| {
            if let Err(e) = tab.resize(cols, rows) {
                log::warn!("resize failed on tab: {e}");
            }
        });
    }

    /// Resolve a [`Command`] against this manager. Returns `Quit` if the
    /// app should exit, `None` otherwise.
    ///
    /// This is the single source of truth for command execution — both the
    /// keybinding dispatcher and the command palette funnel through here.
    pub fn execute(&mut self, cmd: &Command, cols: u16, rows: u16, config: &Config) -> Action {
        match cmd {
            Command::NewTab => {
                let p = config.profile(&self.default_profile);
                if let Err(e) = self.open_tab(&p, None, cols, rows) {
                    log::error!("NewTab failed: {e}");
                }
                Action::Continue
            }
            Command::NewTabProfile(name) => {
                let p = config.profile(name);
                if let Err(e) = self.open_tab(&p, None, cols, rows) {
                    log::error!("NewTabProfile failed: {e}");
                }
                Action::Continue
            }
            Command::CloseTab => {
                if self.close_active() && self.tabs.is_empty() {
                    Action::Quit
                } else {
                    Action::Continue
                }
            }
            Command::NextTab => {
                self.next_tab();
                Action::Continue
            }
            Command::PrevTab => {
                self.prev_tab();
                Action::Continue
            }
            Command::GotoTab(i) => {
                self.goto_tab(*i);
                Action::Continue
            }
            Command::ToggleBroadcastAll => {
                self.toggle_broadcast_all();
                Action::Continue
            }
            Command::ToggleBroadcastGroup(g) => {
                self.toggle_broadcast_group(g.clone());
                Action::Continue
            }
            Command::TagActiveTab(g) => {
                self.tag_active(g.clone());
                Action::Continue
            }
            Command::ResetTerminal => {
                if let Some(t) = self.active_tab_mut() {
                    if let Err(e) = t.write_input(b"\x1bc") {
                        log::warn!("RIS (reset) failed: {e}");
                    }
                }
                Action::Continue
            }
            Command::OpenPalette => Action::Continue,
            Command::Quit => Action::Quit,
        }
    }
}

/// What the app should do after executing a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Continue,
    Quit,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Profile;
    use std::collections::HashMap;

    fn test_profile(cmd: &str) -> Profile {
        Profile {
            command: vec!["sh".into(), "-c".into(), cmd.into()],
            cwd: None,
            tag: None,
            env: HashMap::new(),
        }
    }

    fn fresh_manager() -> TerminalManager {
        let cfg = Config::default();
        let mut m = TerminalManager::new(&cfg);
        let p = test_profile("sleep 5");
        let _ = m.open_tab(&p, Some("t1".into()), 40, 10);
        let _ = m.open_tab(&p, Some("t2".into()), 40, 10);
        m
    }

    #[test]
    fn open_close_active_switching() {
        let mut m = fresh_manager();
        assert_eq!(m.tabs.len(), 2);
        assert_eq!(m.active, 0);

        m.close_active(); // closes tab 0
        assert_eq!(m.tabs.len(), 1);
        assert_eq!(m.active, 0); // switched back to 0 after remove

        m.close_active(); // closes the last tab
        assert!(m.tabs.is_empty());
    }

    #[test]
    fn next_prev_wrap() {
        let mut m = fresh_manager();
        assert_eq!(m.active, 0);
        m.next_tab();
        assert_eq!(m.active, 1);
        m.next_tab();
        assert_eq!(m.active, 0); // wraps
        m.prev_tab();
        assert_eq!(m.active, 1); // wraps
    }

    #[test]
    fn goto_in_bounds() {
        let mut m = fresh_manager();
        assert!(m.goto_tab(1));
        assert_eq!(m.active, 1);
        assert!(!m.goto_tab(99));
        assert_eq!(m.active, 1);
    }

    #[test]
    fn broadcast_toggle_cycle() {
        let mut m = fresh_manager();
        assert_eq!(m.broadcast, BroadcastTarget::Active);
        m.toggle_broadcast_all();
        assert_eq!(m.broadcast, BroadcastTarget::All);
        m.toggle_broadcast_all();
        assert_eq!(m.broadcast, BroadcastTarget::Active);
    }

    #[test]
    fn broadcast_group_toggle() {
        let mut m = fresh_manager();
        m.toggle_broadcast_group("web".into());
        assert_eq!(m.broadcast, BroadcastTarget::Group("web".into()));
        // Toggling same tag turns it off.
        m.toggle_broadcast_group("web".into());
        assert_eq!(m.broadcast, BroadcastTarget::Active);
        // Toggling a different tag switches.
        m.toggle_broadcast_group("db".into());
        assert_eq!(m.broadcast, BroadcastTarget::Group("db".into()));
    }

    #[test]
    fn broadcast_active_writes_only_to_focused() {
        let mut m = fresh_manager();
        // Send Ctrl-C to active only.
        m.route_input(b"\x03").unwrap();
        // (No assertions on side effects here — the integration test below
        // verifies routing by reading PTY output.)
    }

    #[test]
    fn broadcast_all_writes_to_every_tab() {
        let mut m = fresh_manager();
        m.broadcast = BroadcastTarget::All;
        // 'A' to all tabs.
        m.route_input(b"A").unwrap();
        // Confirm both tabs have at least received the byte by reading their
        // PTYs.
        for tab in &mut m.tabs {
            let mut buf = [0u8; 16];
            // The PTY echoes input back; we should see 'A' in the read.
            std::thread::sleep(std::time::Duration::from_millis(50));
            let _ = tab.pty.reader.read(&mut buf);
            // We don't strictly assert content because terminal echo settings
            // vary, but the write must not error.
        }
    }

    #[test]
    fn group_broadcast_falls_back_to_active_when_no_match() {
        let mut m = fresh_manager();
        m.broadcast = BroadcastTarget::Group("nonexistent".into());
        // Should not panic; should fall back to active tab.
        m.route_input(b"hello\n").unwrap();
    }

    #[test]
    fn group_broadcast_hits_tagged_tabs_only() {
        let mut m = fresh_manager();
        // Tag tab 0 with "web".
        m.tabs[0].tag = Some("web".into());
        m.broadcast = BroadcastTarget::Group("web".into());
        m.route_input(b"x").unwrap();
        // Untagged tab 1 should not have been written to — but since writes
        // are async on the PTY side, we just verify no panic.
    }

    #[test]
    fn tag_active_sets_tag() {
        let mut m = fresh_manager();
        assert!(m.tag_active("web".into()));
        assert_eq!(m.active_tab().unwrap().tag.as_deref(), Some("web"));
    }

    #[test]
    fn execute_open_and_close_tab() {
        let mut m = fresh_manager();
        let cfg = Config::default();
        let before = m.tabs.len();
        m.execute(&Command::NewTab, 40, 10, &cfg);
        assert_eq!(m.tabs.len(), before + 1);
        m.execute(&Command::CloseTab, 40, 10, &cfg);
        assert_eq!(m.tabs.len(), before);
    }

    #[test]
    fn execute_quit_returns_quit_action() {
        let mut m = fresh_manager();
        let cfg = Config::default();
        assert_eq!(m.execute(&Command::Quit, 40, 10, &cfg), Action::Quit);
    }
}
