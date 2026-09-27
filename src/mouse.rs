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


//! Mouse event types and SGR mouse encoding.
//!
//! The classic mrxvt supported several mouse modes (X10, X11 normal, X11
//! SGR-1006). This module defines the backend-agnostic mouse event type and
//! the SGR encoder that translates mouse events into escape sequences for
//! the child process.
//!
//! ## Mouse modes
//!
//! Programs request mouse reporting via DECSET escape sequences:
//! - `?9h` — X10 (click only, no modifiers, no release)
//! - `?1000h` — X11 normal (press/release + motion-with-button)
//! - `?1002h` — X11 motion (all motion events, even with no button)
//! - `?1003h` — all motion (no button needed)
//! - `?1006h` — SGR-1006 encoding (extends the above with bigger coords
//!   and explicit press/release markers)
//!
//! The terminal keeps a `MouseMode` bitfield; the renderer translates raw
//! mouse events into [`MouseEvent`]s and asks the encoder whether to send
//! them to the child.
//!
//! ## Selection
//!
//! When mouse reporting is OFF, mouse events are interpreted locally as
//! text selection: click-drag selects, release copies to clipboard. This
//! module exposes a [`Selection`] state machine that renderers can drive.

use std::fmt;

/// Bitflags for active mouse modes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MouseMode {
    /// X10 (button press only).
    pub x10: bool,
    /// X11 normal (press + release + motion-with-button).
    pub x11: bool,
    /// Motion reporting (even with no button held).
    pub motion: bool,
    /// SGR-1006 encoding.
    pub sgr: bool,
}

impl MouseMode {
    /// Is any mouse reporting active?
    pub fn any_reporting(self) -> bool {
        self.x10 || self.x11 || self.motion
    }

    /// Should a button-press event be reported?
    pub fn reports_press(self) -> bool {
        self.x10 || self.x11 || self.motion
    }

    /// Should a button-release event be reported?
    pub fn reports_release(self) -> bool {
        self.x11 || self.motion
    }

    /// Should a motion event be reported?
    pub fn reports_motion(self, button_held: bool) -> bool {
        self.motion || (self.x11 && button_held)
    }
}

/// A mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    /// Wheel up (one notch).
    WheelUp,
    /// Wheel down (one notch).
    WheelDown,
    /// No button (used for motion events with no button held).
    None,
}

/// A mouse event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    pub button: MouseButton,
    /// Cell column (0-indexed).
    pub col: u32,
    /// Cell row (0-indexed).
    pub row: u32,
    pub mods: MouseMods,
    pub kind: MouseEventKind,
}

/// Modifier flags on a mouse event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MouseMods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEventKind {
    Press,
    Release,
    Motion,
}

/// Encode a mouse event using SGR-1006 format.
///
/// Returns `None` if the event shouldn't be reported (e.g. release in X10 mode).
///
/// SGR-1006 format:
/// - Press:   `ESC [ < button ; col ; row M`
/// - Release: `ESC [ < button ; col ; row m`
///
/// Where `button` is the button code + modifier bits (lower 3 bits = button,
/// bit 2 = shift, bit 3 = meta, bit 4 = ctrl, bit 5 = wheel, bit 6 = motion
/// flag — actually for SGR we just encode button+mods, motion is a separate
/// M vs m signal).
pub fn encode_sgr(ev: MouseEvent, mode: MouseMode) -> Option<Vec<u8>> {
    if !mode.sgr {
        // SGR encoding was not negotiated; fall back to legacy elsewhere.
        return None;
    }
    let code = button_code(ev.button, ev.mods);
    let suffix = match ev.kind {
        MouseEventKind::Press => {
            if !mode.reports_press() {
                return None;
            }
            'M'
        }
        MouseEventKind::Release => {
            if !mode.reports_release() {
                return None;
            }
            'm'
        }
        MouseEventKind::Motion => {
            if !mode.reports_motion(ev.button != MouseButton::None) {
                return None;
            }
            // SGR-1006 uses M for motion-with-button, m for motion-without.
            // Convention: motion events always use M; the button field
            // encodes which button (or 35 = no button) is held.
            'M'
        }
    };
    // SGR is 1-indexed.
    let col = ev.col + 1;
    let row = ev.row + 1;
    let s = format!("\x1b[<{code};{col};{row}{suffix}");
    Some(s.into_bytes())
}

/// Encode a mouse event using legacy X11 format (for programs that don't
/// support SGR-1006).
///
/// Returns `None` if the event shouldn't be reported.
///
/// Legacy format: `ESC [ M <button-byte> <col-byte> <row-byte>`
/// where each byte is the value + 32 (to keep it in the printable range).
/// Coordinates are clamped to 1..227 (bytes 33..255).
pub fn encode_x11(ev: MouseEvent, mode: MouseMode) -> Option<Vec<u8>> {
    let _should = match ev.kind {
        MouseEventKind::Press => mode.reports_press(),
        MouseEventKind::Release => mode.reports_release(),
        MouseEventKind::Motion => mode.reports_motion(ev.button != MouseButton::None),
    };
    if !_should {
        return None;
    }
    // Legacy protocol encodes release as button 3 ("no button") and motion
    // with bit 5 set, regardless of the actual button.
    let mod_bits = {
        let mut bits = 0u32;
        if ev.mods.shift { bits |= 4; }
        if ev.mods.alt { bits |= 8; }
        if ev.mods.ctrl { bits |= 16; }
        bits
    };
    let mut code = match ev.kind {
        MouseEventKind::Release => 3 | mod_bits,
        MouseEventKind::Motion => 32 | button_code(ev.button, MouseMods::default()) | mod_bits,
        MouseEventKind::Press => button_code(ev.button, MouseMods::default()) | mod_bits,
    };
    if code > 255 - 32 {
        code = 255 - 32;
    }
    let b = (code + 32) as u8;
    // Coords: 1-indexed, clamped to 1..223 (bytes 33..255 after +32).
    // Clamp to 223 so that 223 + 32 = 255 = u8::MAX (no overflow).
    let col_clamped = (ev.col.saturating_add(1)).clamp(1, 223);
    let row_clamped = (ev.row.saturating_add(1)).clamp(1, 223);
    let c = (col_clamped + 32) as u8;
    let r = (row_clamped + 32) as u8;
    Some(vec![0x1b, b'[', b'M', b, c, r])
}

/// Compute the button code (lower 3 bits + modifier bits).
fn button_code(button: MouseButton, mods: MouseMods) -> u32 {
    let base = match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::WheelUp => 64,
        MouseButton::WheelDown => 65,
        MouseButton::None => 3,
    };
    let mod_bits: [(bool, u32); 3] =
        [(mods.shift, 4), (mods.alt, 8), (mods.ctrl, 16)];
    base | mod_bits.iter().filter(|(f, _)| *f).map(|(_, b)| b).fold(0, |a, b| a | b)
}

// ─── Selection state machine ─────────────────────────────────────────────────

/// A point in the terminal grid (not the viewport). `line` uses the
/// alacritty grid convention: `Line(0)` is the top visible row when the
/// display offset is 0, and negative values reach into scrollback history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SelPoint {
    pub line: i32,
    pub col: usize,
}

/// How the selection was made — decides how it renders and copies.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SelectionKind {
    /// Click-drag: character granularity, line-based span.
    #[default]
    Simple,
    /// Double-click: expanded to word boundaries.
    Word,
    /// Triple-click: whole lines.
    Line,
}

/// A text selection in grid coordinates.
///
/// `anchor` is where the gesture began; `head` is the current extent. The
/// pair is normalized on demand so the selection is direction-independent.
/// The span is **line-based** (like every mainstream terminal): the first
/// and last lines are partially selected between the two points, and every
/// line between them is fully selected.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    pub kind: SelectionKind,
    /// Where the gesture began. `None` = no active selection.
    pub anchor: Option<SelPoint>,
    /// Current extent (`None` while the gesture hasn't moved).
    pub head: Option<SelPoint>,
}

impl Selection {
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin a selection of the given kind at a grid point.
    pub fn begin(&mut self, kind: SelectionKind, p: SelPoint) {
        self.kind = kind;
        self.anchor = Some(p);
        self.head = Some(p);
    }

    /// Extend the selection to a grid point (mouse drag / motion).
    pub fn extend(&mut self, p: SelPoint) {
        if self.anchor.is_some() {
            self.head = Some(p);
        }
    }

    /// Replace both endpoints (used for word/line re-selection on drag).
    pub fn set_span(&mut self, kind: SelectionKind, a: SelPoint, b: SelPoint) {
        self.kind = kind;
        self.anchor = Some(a);
        self.head = Some(b);
    }

    /// Clear the selection.
    pub fn clear(&mut self) {
        self.anchor = None;
        self.head = None;
    }

    /// Is there an active selection?
    pub fn is_active(&self) -> bool {
        self.anchor.is_some() && self.head.is_some()
    }

    /// Anchor/head ordered top-left → bottom-right.
    pub fn normalized(&self) -> Option<(SelPoint, SelPoint)> {
        let a = self.anchor?;
        let b = self.head?;
        if (b.line, b.col) < (a.line, a.col) {
            Some((b, a))
        } else {
            Some((a, b))
        }
    }

    /// True if the selection spans more than one cell.
    pub fn is_multi_cell(&self) -> bool {
        self.normalized().map(|(a, b)| a != b).unwrap_or(false)
    }

    /// For a given grid line, the selected column range `[start, end)`
    /// (half-open), or `None` if the line isn't selected. `cols` is the
    /// grid width, used for fully-selected interior lines.
    pub fn line_span(&self, line: i32, cols: usize) -> Option<(usize, usize)> {
        let (a, b) = self.normalized()?;
        if line < a.line || line > b.line {
            return None;
        }
        let (start, end) = match self.kind {
            SelectionKind::Line => (0, cols),
            SelectionKind::Simple | SelectionKind::Word => {
                if a.line == b.line {
                    (a.col, b.col + 1)
                } else if line == a.line {
                    (a.col, cols)
                } else if line == b.line {
                    (0, b.col + 1)
                } else {
                    (0, cols)
                }
            }
        };
        Some((start, end.min(cols)))
    }

    /// Iterate over the selected grid lines (inclusive range).
    pub fn lines(&self) -> Vec<i32> {
        match self.normalized() {
            Some((a, b)) => (a.line..=b.line).collect(),
            None => Vec::new(),
        }
    }

    /// Does this line fall inside the selection at all? (Cheap check for
    /// renderers deciding whether to inspect a row.)
    pub fn includes_line(&self, line: i32) -> bool {
        self.normalized().map(|(a, b)| line >= a.line && line <= b.line).unwrap_or(false)
    }
}

impl fmt::Display for MouseButton {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_mode_no_reporting_by_default() {
        let m = MouseMode::default();
        assert!(!m.any_reporting());
        assert!(!m.reports_press());
        assert!(!m.reports_release());
        assert!(!m.reports_motion(false));
    }

    #[test]
    fn x11_mode_reports_press_and_release() {
        let m = MouseMode { x11: true, ..Default::default() };
        assert!(m.reports_press());
        assert!(m.reports_release());
        assert!(!m.reports_motion(false));
        assert!(m.reports_motion(true));
    }

    #[test]
    fn motion_mode_reports_all() {
        let m = MouseMode { motion: true, ..Default::default() };
        assert!(m.reports_motion(false));
        assert!(m.reports_motion(true));
    }

    #[test]
    fn x10_only_reports_press() {
        let m = MouseMode { x10: true, ..Default::default() };
        assert!(m.reports_press());
        assert!(!m.reports_release());
        assert!(!m.reports_motion(true));
    }

    #[test]
    fn sgr_press_encoding() {
        let ev = MouseEvent {
            button: MouseButton::Left,
            col: 5,
            row: 10,
            mods: MouseMods::default(),
            kind: MouseEventKind::Press,
        };
        let mode = MouseMode { x11: true, sgr: true, ..Default::default() };
        let bytes = encode_sgr(ev, mode).unwrap();
        let s = String::from_utf8(bytes).unwrap();
        // SGR is 1-indexed: col 5 → 6, row 10 → 11.
        assert_eq!(s, "\x1b[<0;6;11M");
    }

    #[test]
    fn sgr_release_encoding() {
        let ev = MouseEvent {
            button: MouseButton::Right,
            col: 0,
            row: 0,
            mods: MouseMods::default(),
            kind: MouseEventKind::Release,
        };
        let mode = MouseMode { x11: true, sgr: true, ..Default::default() };
        let bytes = encode_sgr(ev, mode).unwrap();
        let s = String::from_utf8(bytes).unwrap();
        assert_eq!(s, "\x1b[<2;1;1m");
    }

    #[test]
    fn sgr_with_modifiers() {
        let ev = MouseEvent {
            button: MouseButton::Left,
            col: 0,
            row: 0,
            mods: MouseMods { shift: true, ctrl: true, alt: true },
            kind: MouseEventKind::Press,
        };
        let mode = MouseMode { x11: true, sgr: true, ..Default::default() };
        let bytes = encode_sgr(ev, mode).unwrap();
        let s = String::from_utf8(bytes).unwrap();
        // shift=4, alt=8, ctrl=16, left=0 → 28
        assert_eq!(s, "\x1b[<28;1;1M");
    }

    #[test]
    fn sgr_wheel_events() {
        let up = MouseEvent {
            button: MouseButton::WheelUp,
            col: 3, row: 4,
            mods: MouseMods::default(),
            kind: MouseEventKind::Press,
        };
        let mode = MouseMode { x11: true, sgr: true, ..Default::default() };
        let s = String::from_utf8(encode_sgr(up, mode).unwrap()).unwrap();
        // WheelUp = 64
        assert_eq!(s, "\x1b[<64;4;5M");
    }

    #[test]
    fn sgr_skipped_when_mode_off() {
        let ev = MouseEvent {
            button: MouseButton::Left,
            col: 0, row: 0,
            mods: MouseMods::default(),
            kind: MouseEventKind::Press,
        };
        let mode = MouseMode::default();
        assert!(encode_sgr(ev, mode).is_none());
    }

    #[test]
    fn sgr_skipped_when_sgr_not_negotiated() {
        // X11 reporting on but SGR off → SGR encoder must not fire; the
        // caller falls back to the legacy encoder.
        let ev = MouseEvent {
            button: MouseButton::Left,
            col: 0, row: 0,
            mods: MouseMods::default(),
            kind: MouseEventKind::Press,
        };
        let mode = MouseMode { x11: true, ..Default::default() };
        assert!(encode_sgr(ev, mode).is_none());
        assert!(encode_x11(ev, mode).is_some());
    }

    #[test]
    fn x11_legacy_encoding() {
        let ev = MouseEvent {
            button: MouseButton::Left,
            col: 0, row: 0,
            mods: MouseMods::default(),
            kind: MouseEventKind::Press,
        };
        let mode = MouseMode { x11: true, ..Default::default() };
        let bytes = encode_x11(ev, mode).unwrap();
        // ESC [ M <32> <33> <33>
        assert_eq!(bytes, vec![0x1b, b'[', b'M', 32, 33, 33]);
    }

    #[test]
    fn x11_legacy_clamps_coords() {
        let ev = MouseEvent {
            button: MouseButton::Left,
            col: 500, row: 500,
            mods: MouseMods::default(),
            kind: MouseEventKind::Press,
        };
        let mode = MouseMode { x11: true, ..Default::default() };
        let bytes = encode_x11(ev, mode).unwrap();
        // Coords clamped to 223 before +32, so byte = 255 (no u8 overflow).
        assert_eq!(bytes.len(), 6);
        assert_eq!(bytes[4], 255); // col byte
        assert_eq!(bytes[5], 255); // row byte
    }

    #[test]
    fn selection_begin_and_extend() {
        let mut s = Selection::new();
        assert!(!s.is_active());
        s.begin(SelectionKind::Simple, SelPoint { line: 0, col: 0 });
        assert!(s.is_active());
        s.extend(SelPoint { line: 2, col: 5 });
        assert!(s.is_multi_cell());
    }

    #[test]
    fn selection_line_span_single_row() {
        let mut s = Selection::new();
        s.begin(SelectionKind::Simple, SelPoint { line: 3, col: 2 });
        s.extend(SelPoint { line: 3, col: 5 });
        assert_eq!(s.line_span(3, 80), Some((2, 6)));
        assert_eq!(s.line_span(2, 80), None);
        assert_eq!(s.line_span(4, 80), None);
    }

    #[test]
    fn selection_line_span_multi_row_partial_ends() {
        let mut s = Selection::new();
        s.begin(SelectionKind::Simple, SelPoint { line: 1, col: 10 });
        s.extend(SelPoint { line: 3, col: 4 });
        // First row: from col 10 to end of line.
        assert_eq!(s.line_span(1, 80), Some((10, 80)));
        // Interior row: full width.
        assert_eq!(s.line_span(2, 80), Some((0, 80)));
        // Last row: start to col 4 inclusive.
        assert_eq!(s.line_span(3, 80), Some((0, 5)));
        assert_eq!(s.line_span(0, 80), None);
        assert_eq!(s.line_span(4, 80), None);
    }

    #[test]
    fn selection_direction_independent() {
        let mut s = Selection::new();
        // Drag bottom-right → top-left; spans must match the forward drag.
        s.begin(SelectionKind::Simple, SelPoint { line: 3, col: 4 });
        s.extend(SelPoint { line: 1, col: 10 });
        assert_eq!(s.line_span(1, 80), Some((10, 80)));
        assert_eq!(s.line_span(2, 80), Some((0, 80)));
        assert_eq!(s.line_span(3, 80), Some((0, 5)));
        let (a, b) = s.normalized().unwrap();
        assert_eq!((a.line, a.col), (1, 10));
        assert_eq!((b.line, b.col), (3, 4));
    }

    #[test]
    fn line_selection_spans_full_rows() {
        let mut s = Selection::new();
        s.begin(SelectionKind::Line, SelPoint { line: 2, col: 30 });
        s.extend(SelPoint { line: 4, col: 1 });
        assert_eq!(s.line_span(2, 80), Some((0, 80)));
        assert_eq!(s.line_span(3, 80), Some((0, 80)));
        assert_eq!(s.line_span(4, 80), Some((0, 80)));
    }

    #[test]
    fn word_selection_uses_point_bounds() {
        // Word selections store pre-expanded boundaries, so the span logic
        // is identical to Simple between those bounds.
        let mut s = Selection::new();
        s.set_span(SelectionKind::Word, SelPoint { line: 0, col: 4 }, SelPoint { line: 0, col: 9 });
        assert_eq!(s.line_span(0, 80), Some((4, 10)));
    }

    #[test]
    fn selection_lines_iterates_inclusive() {
        let mut s = Selection::new();
        s.begin(SelectionKind::Simple, SelPoint { line: -5, col: 0 });
        s.extend(SelPoint { line: -3, col: 0 });
        assert_eq!(s.lines(), vec![-5, -4, -3]);
        assert!(s.includes_line(-4));
        assert!(!s.includes_line(-2));
    }

    #[test]
    fn selection_clear() {
        let mut s = Selection::new();
        s.begin(SelectionKind::Simple, SelPoint { line: 0, col: 0 });
        s.clear();
        assert!(!s.is_active());
        assert!(!s.is_multi_cell());
        assert_eq!(s.line_span(0, 80), None);
    }

    #[test]
    fn selection_single_cell_not_multi() {
        let mut s = Selection::new();
        s.begin(SelectionKind::Simple, SelPoint { line: 3, col: 3 });
        assert!(!s.is_multi_cell());
    }
}
