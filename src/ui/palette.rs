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


//! Command palette overlay.
//!
//! A fuzzy-search overlay over the terminal. Triggered by `Ctrl+Shift+P`
//! or the `OpenPalette` command. Filters [`Command::defaults()`] by
//! the user's query using `fuzzy-matcher`'s SkimMatcher.

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

use crate::command::{Command, CommandEntry};

/// One entry in the visible palette list.
#[derive(Debug, Clone)]
pub struct PaletteEntry {
    pub command: Command,
    pub name: String,
    pub category: String,
    pub score: i64,
}

/// State for the palette overlay.
pub struct PaletteState {
    pub query: String,
    pub entries: Vec<PaletteEntry>,
    pub selected: usize,
    pub open: bool,
    matcher: SkimMatcherV2,
}

impl std::fmt::Debug for PaletteState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaletteState")
            .field("query", &self.query)
            .field("entries_len", &self.entries.len())
            .field("selected", &self.selected)
            .field("open", &self.open)
            .finish()
    }
}

impl PaletteState {
    pub fn new() -> Self {
        let matcher = SkimMatcherV2::default();
        let entries = Self::compute_entries("", &matcher);
        Self {
            query: String::new(),
            entries,
            selected: 0,
            open: false,
            matcher,
        }
    }

    pub fn open(&mut self) {
        self.query.clear();
        self.selected = 0;
        self.entries = Self::compute_entries("", &self.matcher);
        self.open = true;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn toggle(&mut self) {
        if self.open {
            self.close();
        } else {
            self.open();
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Append a character to the query and re-filter.
    pub fn push_char(&mut self, c: char) {
        self.query.push(c);
        self.entries = Self::compute_entries(&self.query, &self.matcher);
        self.selected = 0;
    }

    /// Remove the last character of the query.
    pub fn backspace(&mut self) {
        self.query.pop();
        self.entries = Self::compute_entries(&self.query, &self.matcher);
        self.selected = 0;
    }

    pub fn move_up(&mut self) {
        if !self.entries.is_empty() {
            self.selected = if self.selected == 0 {
                self.entries.len() - 1
            } else {
                self.selected - 1
            };
        }
    }

    pub fn move_down(&mut self) {
        if !self.entries.is_empty() {
            self.selected = (self.selected + 1) % self.entries.len();
        }
    }

    /// Returns the currently-selected command, if any.
    pub fn selected_command(&self) -> Option<Command> {
        self.entries.get(self.selected).map(|e| e.command.clone())
    }

    fn compute_entries(query: &str, matcher: &SkimMatcherV2) -> Vec<PaletteEntry> {
        let mut entries: Vec<PaletteEntry> = Command::defaults()
            .into_iter()
            .filter_map(|CommandEntry { command, name, category }| {
                if query.is_empty() {
                    return Some(PaletteEntry {
                        command,
                        name: name.into(),
                        category: category.into(),
                        score: 0,
                    });
                }
                let score = matcher.fuzzy_match(name, query)?;
                Some(PaletteEntry {
                    command,
                    name: name.into(),
                    category: category.into(),
                    score,
                })
            })
            .collect();

        if !query.is_empty() {
            entries.sort_by_key(|b| std::cmp::Reverse(b.score));
        }
        entries
    }
}

impl Default for PaletteState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_lists_all_defaults() {
        let p = PaletteState::new();
        assert!(p.entries.len() >= 5);
    }

    #[test]
    fn filtering_narrows_results() {
        let mut p = PaletteState::new();
        let all = p.entries.len();
        for c in "broadcast".chars() {
            p.push_char(c);
        }
        assert!(p.entries.len() <= all, "filter should narrow");
        assert!(p.entries.iter().any(|e| e.name.contains("Broadcast")));
    }

    #[test]
    fn backspace_restores_results() {
        let mut p = PaletteState::new();
        let original = p.entries.len();
        p.push_char('q');
        let narrowed = p.entries.len();
        p.backspace();
        assert_eq!(p.entries.len(), original);
        assert_ne!(p.entries.len(), narrowed);
    }

    #[test]
    fn move_down_wraps() {
        let mut p = PaletteState::new();
        let len = p.entries.len();
        // Go down `len` times — should wrap back to 0.
        for _ in 0..len {
            p.move_down();
        }
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn move_up_wraps() {
        let mut p = PaletteState::new();
        p.move_up();
        assert_eq!(p.selected, p.entries.len() - 1);
    }

    #[test]
    fn selected_command_returns_some_when_open() {
        let mut p = PaletteState::new();
        p.open();
        assert!(p.selected_command().is_some());
    }

    #[test]
    fn open_close_toggle() {
        let mut p = PaletteState::new();
        assert!(!p.is_open());
        p.toggle();
        assert!(p.is_open());
        p.toggle();
        assert!(!p.is_open());
    }
}
