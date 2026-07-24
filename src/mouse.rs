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
    let mut code = button_code(ev.button, ev.mods);
    if ev.kind == MouseEventKind::Motion {
        code |= 32; // motion flag
    }
    let b = (code.min(255 - 32) + 32) as u8;
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

/// A text selection. Used by renderers for click-drag-to-select.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    /// Start point (cell coords). `None` = no active selection.
    pub start: Option<(u32, u32)>,
    /// End point (cell coords). `None` = single-click.
    pub end: Option<(u32, u32)>,
}

impl Selection {
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin a selection at the given cell.
    pub fn begin(&mut self, col: u32, row: u32) {
        self.start = Some((col, row));
        self.end = Some((col, row));
    }

    /// Extend the selection to the given cell.
    pub fn extend(&mut self, col: u32, row: u32) {
        if self.start.is_some() {
            self.end = Some((col, row));
        }
    }

    /// Clear the selection.
    pub fn clear(&mut self) {
        self.start = None;
        self.end = None;
    }

    /// Is there an active selection?
    pub fn is_active(&self) -> bool {
        self.start.is_some() && self.end.is_some()
    }

    /// Iterate over the selected cells in row-major order.
    pub fn cells(&self) -> Vec<(u32, u32)> {
        let Some((sx, sy)) = self.start else { return Vec::new(); };
        let Some((ex, ey)) = self.end else { return Vec::new(); };
        // Normalize: top-left to bottom-right.
        let (x1, y1) = (sx.min(ex), sy.min(ey));
        let (x2, y2) = (sx.max(ex), sy.max(ey));
        (y1..=y2).flat_map(|y| (x1..=x2).map(move |x| (x, y))).collect()
    }

    /// True if the selection spans more than one cell.
    pub fn is_multi_cell(&self) -> bool {
        if let (Some(s), Some(e)) = (self.start, self.end) {
            s != e
        } else {
            false
        }
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
        s.begin(0, 0);
        assert!(s.is_active());
        s.extend(5, 2);
        assert!(s.is_multi_cell());
    }

    #[test]
    fn selection_cells_row_major() {
        let mut s = Selection::new();
        s.begin(0, 0);
        s.extend(2, 1);
        let cells = s.cells();
        // 3 cols × 2 rows = 6 cells.
        assert_eq!(cells.len(), 6);
        assert!(cells.contains(&(0, 0)));
        assert!(cells.contains(&(2, 1)));
    }

    #[test]
    fn selection_normalizes_swapped_points() {
        let mut s = Selection::new();
        s.begin(5, 5);
        s.extend(1, 1);
        let cells = s.cells();
        // Should still cover the same rectangle regardless of direction.
        assert_eq!(cells.len(), 5 * 5);
        assert!(cells.contains(&(1, 1)));
        assert!(cells.contains(&(5, 5)));
    }

    #[test]
    fn selection_clear() {
        let mut s = Selection::new();
        s.begin(0, 0);
        s.clear();
        assert!(!s.is_active());
    }

    #[test]
    fn selection_single_cell_not_multi() {
        let mut s = Selection::new();
        s.begin(3, 3);
        assert!(!s.is_multi_cell());
    }
}
