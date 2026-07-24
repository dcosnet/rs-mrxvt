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


//! OSC 8 hyperlink support.
//!
//! Implements the [OSC 8 escape sequence](https://gist.github.com/egmontkov/eb114294efbcd5adb1944c9842f0ec18)
//! for inline hyperlinks. Programs like `ls` (with `--hyperlink=auto`),
//! `gcc`, and various TUI file managers use this to make URLs and file
//! paths clickable.
//!
//! ## Format
//!
//! ```text
//! ESC ] 8 ; <params> ; <URI> ST   ← start hyperlink
//! ... clickable text ...
//! ESC ] 8 ; ; ST                  ← end hyperlink
//! ```
//!
//! `<params>` is a semicolon-separated list of `key=value` pairs. The
//! most common are:
//! - `id=<id>` — group consecutive cells into one logical link
//! - `title=<text>` — tooltip text
//!
//! ## How it works
//!
//! The PTY stream is scanned for OSC 8 sequences. When found, we record
//! the `(start_cell, end_cell, uri, id)` tuple in a [`HyperlinkStore`].
//! The renderer queries the store when drawing cells; if a cell has a
//! hyperlink, it can be styled differently (underline, hover color) and
//! mouse clicks can resolve to the URI.
//!
//! ## Status
//!
//! Pure parser module. The renderer integration (drawing underlines on
//! hyperlinked cells, mouse-click hit-testing) is the next step.

use std::collections::HashMap;

/// A hyperlink annotation covering a range of cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hyperlink {
    /// The URI the link points to.
    pub uri: String,
    /// Optional group ID — cells with the same ID are treated as one link.
    pub id: Option<String>,
    /// Optional tooltip / title.
    pub title: Option<String>,
    /// Starting cell (col, row) — inclusive.
    pub start: (u32, u32),
    /// Ending cell (col, row) — inclusive.
    pub end: (u32, u32),
}

impl Hyperlink {
    /// Does this hyperlink contain the given cell?
    pub fn contains(&self, col: u32, row: u32) -> bool {
        let (sx, sy) = self.start;
        let (ex, ey) = self.end;
        let (x1, y1) = (sx.min(ex), sy.min(ey));
        let (x2, y2) = (sx.max(ex), sy.max(ey));
        col >= x1 && col <= x2 && row >= y1 && row <= y2
    }
}

/// Store of hyperlinks, keyed by link ID.
#[derive(Debug, Default)]
pub struct HyperlinkStore {
    links: Vec<Hyperlink>,
    /// Index from cell → link index, built lazily.
    cell_index: HashMap<(u32, u32), Vec<usize>>,
    index_dirty: bool,
}

impl HyperlinkStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a hyperlink. Returns its index.
    pub fn add(&mut self, link: Hyperlink) -> usize {
        let idx = self.links.len();
        self.links.push(link);
        self.index_dirty = true;
        idx
    }

    /// Remove a hyperlink by index.
    pub fn remove(&mut self, idx: usize) -> Option<Hyperlink> {
        if idx < self.links.len() {
            self.index_dirty = true;
            Some(self.links.remove(idx))
        } else {
            None
        }
    }

    /// Clear all hyperlinks.
    pub fn clear(&mut self) {
        self.links.clear();
        self.cell_index.clear();
        self.index_dirty = false;
    }

    /// Number of stored hyperlinks.
    pub fn len(&self) -> usize {
        self.links.len()
    }

    /// Is the store empty?
    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }

    /// Get all hyperlinks covering a given cell.
    pub fn at(&mut self, col: u32, row: u32) -> Vec<&Hyperlink> {
        if self.index_dirty {
            self.rebuild_index();
        }
        self.cell_index
            .get(&(col, row))
            .map_or_else(Vec::new, |indices| {
                indices.iter().filter_map(|&i| self.links.get(i)).collect()
            })
    }

    /// Get all hyperlinks (immutable).
    pub fn all(&self) -> &[Hyperlink] {
        &self.links
    }

    fn rebuild_index(&mut self) {
        self.cell_index.clear();
        for (i, link) in self.links.iter().enumerate() {
            let (sx, sy) = link.start;
            let (ex, ey) = link.end;
            let (x1, y1) = (sx.min(ex), sy.min(ey));
            let (x2, y2) = (sx.max(ex), sy.max(ey));
            for y in y1..=y2 {
                for x in x1..=x2 {
                    self.cell_index.entry((x, y)).or_default().push(i);
                }
            }
        }
        self.index_dirty = false;
    }
}

/// Parse the params portion of an OSC 8 sequence.
///
/// Input format: `key1=val1;key2=val2;...` (possibly empty).
pub fn parse_osc8_params(params: &str) -> HashMap<String, String> {
    params
        .split(';')
        .filter(|kv| !kv.is_empty())
        .filter_map(|kv| {
            let eq = kv.find('=')?;
            Some((kv[..eq].to_string(), kv[eq + 1..].to_string()))
        })
        .collect()
}

/// Result of parsing one OSC 8 sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Osc8Event {
    /// Start of a hyperlink. `(id, uri)` — both may be empty.
    Start { id: Option<String>, uri: String },
    /// End of a hyperlink.
    End,
}

/// Parse a single OSC 8 payload (the text between `ESC ] 8 ;` and `ST`).
///
/// Returns `None` if the payload is not a valid OSC 8 sequence.
pub fn parse_osc8_payload(payload: &str) -> Option<Osc8Event> {
    // Payload format: `<params> ; <uri>` for start, or `;` (empty) for end.
    let semi = payload.find(';')?;
    let params_str = &payload[..semi];
    let uri = &payload[semi + 1..];

    if uri.is_empty() && params_str.is_empty() {
        return Some(Osc8Event::End);
    }

    let params = parse_osc8_params(params_str);
    let id = params.get("id").cloned();
    Some(Osc8Event::Start {
        id,
        uri: uri.to_string(),
    })
}

/// A stateful scanner that walks a byte stream and emits OSC 8 events.
///
/// Use this in the PTY reader thread to extract hyperlinks from the
/// terminal output. Bytes that aren't part of an OSC 8 sequence are
/// passed through untouched (the scanner only consumes the escape
/// sequences, not the text between them).
pub struct Osc8Scanner {
    state: ScanState,
    buf: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanState {
    /// Looking for `ESC ]`.
    Ground,
    /// Saw `ESC`, expecting `]`.
    EscSeen,
    /// Inside an OSC sequence, accumulating until `ST` (`ESC \` or `BEL`).
    InOsc,
    /// Saw `ESC` inside OSC, expecting `\` (ST terminator).
    OscEscSeen,
}

impl Osc8Scanner {
    pub fn new() -> Self {
        Self {
            state: ScanState::Ground,
            buf: Vec::new(),
        }
    }

    /// Feed one byte. Returns `Some(Osc8Event)` if a complete OSC 8 sequence
    /// was recognized, `None` otherwise.
    ///
    /// Note: this only emits events for OSC 8 (`ESC ] 8 ; ... ST`). Other
    /// OSC sequences are silently consumed (the bytes are not emitted as
    /// pass-through; that's the caller's responsibility if needed).
    pub fn feed(&mut self, b: u8) -> Option<Osc8Event> {
        match self.state {
            ScanState::Ground => {
                if b == 0x1b {
                    self.state = ScanState::EscSeen;
                }
                None
            }
            ScanState::EscSeen => {
                if b == b']' {
                    self.state = ScanState::InOsc;
                    self.buf.clear();
                } else {
                    self.state = ScanState::Ground;
                }
                None
            }
            ScanState::InOsc => {
                if b == 0x1b {
                    self.state = ScanState::OscEscSeen;
                    None
                } else if b == 0x07 {
                    // BEL terminator.
                    let event = self.take_event();
                    self.state = ScanState::Ground;
                    event
                } else {
                    self.buf.push(b);
                    None
                }
            }
            ScanState::OscEscSeen => {
                if b == b'\\' {
                    // ST terminator.
                    let event = self.take_event();
                    self.state = ScanState::Ground;
                    event
                } else {
                    // Not ST — was a different ESC inside OSC. Treat as
                    // a new escape sequence start (best-effort).
                    self.state = ScanState::EscSeen;
                    None
                }
            }
        }
    }

    fn take_event(&mut self) -> Option<Osc8Event> {
        let payload = String::from_utf8_lossy(&self.buf).to_string();
        self.buf.clear();
        if let Some(rest) = payload.strip_prefix("8;") {
            parse_osc8_payload(rest)
        } else {
            None
        }
    }
}

impl Default for Osc8Scanner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_start_with_uri() {
        let ev = parse_osc8_payload("id=42;https://example.com").unwrap();
        match ev {
            Osc8Event::Start { id, uri } => {
                assert_eq!(id.as_deref(), Some("42"));
                assert_eq!(uri, "https://example.com");
            }
            _ => panic!("expected Start"),
        }
    }

    #[test]
    fn parse_start_without_id() {
        let ev = parse_osc8_payload(";file:///etc/passwd").unwrap();
        match ev {
            Osc8Event::Start { id, uri } => {
                assert!(id.is_none());
                assert_eq!(uri, "file:///etc/passwd");
            }
            _ => panic!("expected Start"),
        }
    }

    #[test]
    fn parse_end_sequence() {
        let ev = parse_osc8_payload(";").unwrap();
        assert_eq!(ev, Osc8Event::End);
    }

    #[test]
    fn parse_empty_payload_returns_none() {
        assert!(parse_osc8_payload("").is_none());
    }

    #[test]
    fn parse_params_with_multiple_keys() {
        let params = parse_osc8_params("id=link1;title=Click me;color=blue");
        assert_eq!(params.get("id").unwrap(), "link1");
        assert_eq!(params.get("title").unwrap(), "Click me");
        assert_eq!(params.get("color").unwrap(), "blue");
    }

    #[test]
    fn parse_params_handles_empty() {
        let params = parse_osc8_params("");
        assert!(params.is_empty());
    }

    #[test]
    fn parse_params_ignores_no_eq() {
        let params = parse_osc8_params("id=link1;garbage;key=val");
        assert_eq!(params.get("id").unwrap(), "link1");
        assert_eq!(params.get("key").unwrap(), "val");
        assert!(!params.contains_key("garbage"));
    }

    #[test]
    fn store_add_and_lookup() {
        let mut store = HyperlinkStore::new();
        store.add(Hyperlink {
            uri: "https://example.com".into(),
            id: None,
            title: None,
            start: (0, 0),
            end: (5, 0),
        });
        let links = store.at(3, 0);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].uri, "https://example.com");
    }

    #[test]
    fn store_outside_range_returns_empty() {
        let mut store = HyperlinkStore::new();
        store.add(Hyperlink {
            uri: "x".into(),
            id: None,
            title: None,
            start: (0, 0),
            end: (5, 0),
        });
        assert!(store.at(6, 0).is_empty());
        assert!(store.at(0, 1).is_empty());
    }

    #[test]
    fn store_handles_multiple_links_per_cell() {
        let mut store = HyperlinkStore::new();
        store.add(Hyperlink {
            uri: "first".into(),
            id: None,
            title: None,
            start: (0, 0),
            end: (5, 0),
        });
        store.add(Hyperlink {
            uri: "second".into(),
            id: None,
            title: None,
            start: (3, 0),
            end: (8, 0),
        });
        let links = store.at(4, 0);
        assert_eq!(links.len(), 2);
    }

    #[test]
    fn store_clear_resets_index() {
        let mut store = HyperlinkStore::new();
        store.add(Hyperlink {
            uri: "x".into(),
            id: None,
            title: None,
            start: (0, 0),
            end: (5, 0),
        });
        store.clear();
        assert!(store.is_empty());
        assert!(store.at(0, 0).is_empty());
    }

    #[test]
    fn store_remove_by_index() {
        let mut store = HyperlinkStore::new();
        let idx = store.add(Hyperlink {
            uri: "x".into(),
            id: None,
            title: None,
            start: (0, 0),
            end: (5, 0),
        });
        assert_eq!(store.len(), 1);
        store.remove(idx);
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn hyperlink_contains_works() {
        let link = Hyperlink {
            uri: "x".into(),
            id: None,
            title: None,
            start: (5, 5),
            end: (10, 7),
        };
        assert!(link.contains(5, 5));
        assert!(link.contains(10, 7));
        assert!(link.contains(7, 6));
        assert!(!link.contains(4, 5));
        assert!(!link.contains(11, 7));
    }

    #[test]
    fn scanner_emits_start_event() {
        let mut s = Osc8Scanner::new();
        // ESC ] 8 ; id=1 ; https://example.com ST
        let bytes = b"\x1b]8;id=1;https://example.com\x1b\\";
        let mut events = Vec::new();
        for &b in bytes {
            if let Some(ev) = s.feed(b) {
                events.push(ev);
            }
        }
        assert_eq!(events.len(), 1);
        match &events[0] {
            Osc8Event::Start { id, uri } => {
                assert_eq!(id.as_deref(), Some("1"));
                assert_eq!(uri, "https://example.com");
            }
            _ => panic!("expected Start"),
        }
    }

    #[test]
    fn scanner_emits_end_event() {
        let mut s = Osc8Scanner::new();
        // ESC ] 8 ; ; ST
        let bytes = b"\x1b]8;;\x1b\\";
        let mut events = Vec::new();
        for &b in bytes {
            if let Some(ev) = s.feed(b) {
                events.push(ev);
            }
        }
        assert_eq!(events, vec![Osc8Event::End]);
    }

    #[test]
    fn scanner_ignores_non_osc8_sequences() {
        let mut s = Osc8Scanner::new();
        // ESC ] 0 ; title ST (OSC 0 — set window title, not 8)
        let bytes = b"\x1b]0;my title\x1b\\";
        let mut events = Vec::new();
        for &b in bytes {
            if let Some(ev) = s.feed(b) {
                events.push(ev);
            }
        }
        assert!(events.is_empty());
    }

    #[test]
    fn scanner_handles_bel_terminator() {
        let mut s = Osc8Scanner::new();
        let bytes = b"\x1b]8;;http://x\x07";
        let mut events = Vec::new();
        for &b in bytes {
            if let Some(ev) = s.feed(b) {
                events.push(ev);
            }
        }
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn scanner_resets_after_non_osc_escape() {
        let mut s = Osc8Scanner::new();
        // ESC [ A (cursor up) — not OSC at all.
        let bytes = b"\x1b[A";
        let mut events = Vec::new();
        for &b in bytes {
            if let Some(ev) = s.feed(b) {
                events.push(ev);
            }
        }
        assert!(events.is_empty());
        // Scanner should be back in ground state, ready for next sequence.
        // Verify by feeding a real OSC 8.
        let bytes = b"\x1b]8;;http://x\x07";
        for &b in bytes {
            if let Some(ev) = s.feed(b) {
                events.push(ev);
            }
        }
        assert_eq!(events.len(), 1);
    }
}
