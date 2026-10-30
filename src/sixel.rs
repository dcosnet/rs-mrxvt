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


//! Sixel image protocol parser.
//!
//! Sixel is an old DEC image format that pre-dates the modern PNG/JPEG era.
//! It's still used by `mlterm`, `xterm -ti vt340`, and various embedded
//! terminals. The format is a sequence of escape codes that describe a
//! raster image as runs of six-pixel-tall vertical strips ("sixels").
//!
//! ## Format
//!
//! Sixel data is sent inside a DCS (Device Control String):
//! ```text
//! DCS q <params> <sixel-data> ST
//! ```
//! - `DCS` = `ESC P` (0x1b 0x50)
//! - `q` is the Sixel command introducer
//! - `<params>` are semicolon-separated `key=val` pairs
//! - `<sixel-data>` is a mix of:
//!   - `#` color register selection (e.g. `#0` = use color 0)
//!   - `#N;r;g;b` define color N as r,g,b (0..100)
//!   - `!N<char>` repeat char N times (e.g. `!10?` = ten `?` sixels)
//!   - `$` carriage return (move to start of current line)
//!   - `-` new line of sixels
//!   - chars `?` (0x3f, 0b000000) to `~` (0x7e, 0b111111): 6 pixels high
//! - `ST` = `ESC \` (0x1b 0x5c)
//!
//! ## Status
//!
//! This module parses Sixel data into an RGBA buffer. It compiles only with
//! `--features images` (reuses [`crate::images::InlineImage`] for storage).

use crate::images::{ImageError, InlineImage};

/// Parse a Sixel data stream into an [`InlineImage`].
///
/// `data` is the bytes between `DCS q` and `ST` (exclusive). The caller is
/// responsible for extracting this from the PTY stream; the DCS handler in
/// `alacritty_terminal` would normally intercept this and we'd hook it.
///
/// For the MVP we expose the parser as a pure function so it can be tested
/// without a PTY.
pub fn parse_sixel(
    data: &[u8],
    start_col: u32,
    start_row: u32,
    cell_w_px: u32,
    cell_h_px: u32,
) -> Result<InlineImage, ImageError> {
    let mut parser = SixelParser::new();
    parser.feed(data)?;
    let (pixels, w, h) = parser.finalize();

    let cell_width = w.div_ceil(cell_w_px.max(1));
    let cell_height = h.div_ceil(cell_h_px.max(1));

    Ok(InlineImage {
        pixels,
        width: w,
        height: h,
        start_col,
        start_row,
        cell_width,
        cell_height,
    })
}

/// Color register: RGB in 0..255.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ColorReg {
    r: u8,
    g: u8,
    b: u8,
}

/// Sixel parser state machine.
///
/// Pixels are collected sparsely and packed row-major only at `finalize` —
/// the image width grows as columns arrive, and repacking an existing
/// row-major buffer on every width change is where stride corruption
/// (sheared/garbled output) comes from.
struct SixelParser {
    /// Painted pixels keyed by (x, y).
    painted: std::collections::HashMap<(u32, u32), ColorReg>,
    /// Width in pixels (grows as we see more sixels in a row).
    width: u32,
    /// Height in pixels (grows as we see more rows).
    height: u32,
    /// Current X position (in sixel columns).
    x: u32,
    /// Current "row" of sixels (each sixel is 6 pixels tall; the row counter
    /// advances by 6 when we see `-`).
    y_base: u32,
    /// Color registers (index → RGB).
    color_regs: Vec<ColorReg>,
    /// Currently selected color register.
    current_color: u32,
    /// Parser state for parameter parsing (after `#` or `!`).
    state: SixelState,
    /// Buffer for accumulating multi-digit parameter values.
    param_buf: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SixelState {
    Ground,
    /// Saw `#`, accumulating color register index (or `;r;g;b` definition).
    ColorIntroducer,
    /// Saw `!`, accumulating repeat count.
    RepeatCount,
}

impl SixelParser {
    fn new() -> Self {
        Self {
            painted: std::collections::HashMap::new(),
            width: 0,
            height: 0,
            x: 0,
            y_base: 0,
            color_regs: vec![ColorReg { r: 255, g: 255, b: 255 }; 256],
            current_color: 0,
            state: SixelState::Ground,
            param_buf: String::new(),
        }
    }

    fn feed(&mut self, data: &[u8]) -> Result<(), ImageError> {
        for &b in data {
            let c = b as char;
            match self.state {
                SixelState::Ground => self.handle_ground(c)?,
                SixelState::ColorIntroducer => self.handle_color_param(c)?,
                SixelState::RepeatCount => self.handle_repeat(c)?,
            }
        }
        Ok(())
    }

    fn handle_ground(&mut self, c: char) -> Result<(), ImageError> {
        match c {
            // Color register selection / definition.
            '#' => {
                self.state = SixelState::ColorIntroducer;
                self.param_buf.clear();
            }
            // Repeat introducer.
            '!' => {
                self.state = SixelState::RepeatCount;
                self.param_buf.clear();
            }
            // Carriage return (within the current sixel row).
            '$' => {
                self.x = 0;
            }
            // New sixel row (advances by 6 pixels vertically).
            '-' => {
                self.x = 0;
                self.y_base += 6;
                self.height = self.height.max(self.y_base + 6);
            }
            // Sixel character: 6 pixels tall, encoded as ?..~ (0x3f..0x7e).
            '?'..='~' => {
                let sixel = (c as u8) - 0x3f; // 0b000000..0b111111
                self.draw_sixel(sixel);
                self.x += 1;
                self.width = self.width.max(self.x);
            }
            // Whitespace and unknown chars are ignored.
            _ => {}
        }
        Ok(())
    }

    fn handle_color_param(&mut self, c: char) -> Result<(), ImageError> {
        if c.is_ascii_digit() || c == ';' {
            self.param_buf.push(c);
            return Ok(());
        }
        // End of param: parse what we have.
        let parts: Vec<&str> = self.param_buf.split(';').collect();
        if parts.len() == 4 {
            // #N;r;g;b — define color N.
            let n: u32 = parts[0].parse().unwrap_or(0);
            let r: u32 = parts[1].parse().unwrap_or(0).min(100);
            let g: u32 = parts[2].parse().unwrap_or(0).min(100);
            let b: u32 = parts[3].parse().unwrap_or(0).min(100);
            let idx = n as usize;
            if idx < self.color_regs.len() {
                // Sixel colors are 0..100; scale to 0..255.
                self.color_regs[idx] = ColorReg {
                    r: (r * 255 / 100) as u8,
                    g: (g * 255 / 100) as u8,
                    b: (b * 255 / 100) as u8,
                };
            }
            self.current_color = n;
        } else if parts.len() == 1 && !parts[0].is_empty() {
            // #N — select color N.
            self.current_color = parts[0].parse().unwrap_or(0);
        }
        self.state = SixelState::Ground;
        // Re-process the current char in Ground state.
        self.handle_ground(c)
    }

    fn handle_repeat(&mut self, c: char) -> Result<(), ImageError> {
        if c.is_ascii_digit() {
            self.param_buf.push(c);
            return Ok(());
        }
        // End of count: parse and repeat the next char.
        let count: u32 = self.param_buf.parse().unwrap_or(1).max(1);
        self.state = SixelState::Ground;
        if ('?'..='~').contains(&c) {
            let sixel = (c as u8) - 0x3f;
            for _ in 0..count {
                self.draw_sixel(sixel);
                self.x += 1;
                self.width = self.width.max(self.x);
            }
        }
        // Other chars: ignore (the spec says the char after !N must be a sixel).
        Ok(())
    }

    /// Draw one sixel (6 vertical pixels) at the current (x, y_base).
    fn draw_sixel(&mut self, sixel: u8) {
        let color = self.color_regs.get(self.current_color as usize)
            .copied()
            .unwrap_or(ColorReg { r: 255, g: 255, b: 255 });
        for bit in 0..6 {
            if (sixel >> bit) & 1 == 1 {
                let py = self.y_base + bit;
                self.set_pixel(self.x, py, color);
            }
        }
    }

    /// Record one painted pixel; the row-major buffer is packed at finalize.
    fn set_pixel(&mut self, x: u32, y: u32, color: ColorReg) {
        self.painted.insert((x, y), color);
        self.width = self.width.max(x + 1);
        self.height = self.height.max(y + 1);
    }

    /// Pack the painted pixels into a row-major RGBA buffer.
    fn finalize(self) -> (Vec<u8>, u32, u32) {
        let mut pixels = vec![0u8; (self.width as usize) * (self.height as usize) * 4];
        for ((x, y), c) in &self.painted {
            let idx = ((y * self.width + x) * 4) as usize;
            pixels[idx] = c.r;
            pixels[idx + 1] = c.g;
            pixels[idx + 2] = c.b;
            pixels[idx + 3] = 255;
        }
        (pixels, self.width, self.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_sixel_yields_empty_image() {
        let img = parse_sixel(b"", 0, 0, 8, 16).unwrap();
        assert_eq!(img.width, 0);
        assert_eq!(img.height, 0);
    }

    #[test]
    fn single_sixel_draws_six_pixels() {
        // '?' = 0b000000 (no pixels). '~' = 0b111111 (all 6 pixels).
        // Draw one column of all-on sixels using the default color (white).
        let img = parse_sixel(b"~", 0, 0, 8, 16).unwrap();
        assert_eq!(img.width, 1);
        assert_eq!(img.height, 6);
        // Top pixel should be the default white.
        assert_eq!(img.pixels[0], 255); // R
        assert_eq!(img.pixels[1], 255); // G
        assert_eq!(img.pixels[2], 255); // B
        assert_eq!(img.pixels[3], 255); // A
    }

    #[test]
    fn color_register_definition() {
        // Define color 0 as red (100,0,0), then draw with it.
        // #0;1;0;0 means: color 0 = (100%, 0%, 0%)
        let img = parse_sixel(b"#0;100;0;0~", 0, 0, 8, 16).unwrap();
        assert_eq!(img.pixels[0], 255); // R = 100% → 255
        assert_eq!(img.pixels[1], 0);   // G = 0
        assert_eq!(img.pixels[2], 0);   // B = 0
    }

    #[test]
    fn color_register_selection() {
        // Define color 1 as green, select it, draw with it.
        let img = parse_sixel(b"#1;0;100;0#1~", 0, 0, 8, 16).unwrap();
        assert_eq!(img.pixels[0], 0);   // R
        assert_eq!(img.pixels[1], 255); // G
        assert_eq!(img.pixels[2], 0);   // B
    }

    #[test]
    fn repeat_count() {
        // !5~ = draw 5 columns of all-on sixels.
        let img = parse_sixel(b"!5~", 0, 0, 8, 16).unwrap();
        assert_eq!(img.width, 5);
        assert_eq!(img.height, 6);
    }

    #[test]
    fn newline_advances_by_six() {
        // ~ - ~ draws one sixel, then a new row, then another.
        let img = parse_sixel(b"~-~", 0, 0, 8, 16).unwrap();
        assert_eq!(img.width, 1);
        assert_eq!(img.height, 12); // two rows of 6
    }

    #[test]
    fn carriage_return_resets_x() {
        // ~ $ ~ draws one sixel, CR, then another in the same column.
        let img = parse_sixel(b"~$~", 0, 0, 8, 16).unwrap();
        assert_eq!(img.width, 1);
        assert_eq!(img.height, 6);
    }

    #[test]
    fn multiple_sixels_in_row_grow_width() {
        // ~~~ = three columns of all-on sixels.
        let img = parse_sixel(b"~~~", 0, 0, 8, 16).unwrap();
        assert_eq!(img.width, 3);
        assert_eq!(img.height, 6);
    }

    #[test]
    fn whitespace_ignored() {
        // Spaces and newlines (real ones, not '-') in the data are ignored.
        let img = parse_sixel(b"  ~  \n  ", 0, 0, 8, 16).unwrap();
        assert_eq!(img.width, 1);
    }

    #[test]
    fn cell_footprint_rounded_up() {
        // 9-pixel-wide image with 8px cells → 2 cells wide.
        let img = parse_sixel(b"!9~", 0, 0, 8, 16).unwrap();
        assert_eq!(img.width, 9);
        assert_eq!(img.cell_width, 2);
    }

    #[test]
    fn start_position_propagates() {
        let img = parse_sixel(b"~", 5, 3, 8, 16).unwrap();
        assert_eq!(img.start_col, 5);
        assert_eq!(img.start_row, 3);
    }

    #[test]
    fn width_growth_does_not_shear_earlier_pixels() {
        // Regression: the old incremental buffer never re-packed rows when
        // the width grew, so pixel (0,1) was reinterpreted as (1,0).
        // Column 0 red, then column 1 green:
        let data = b"#0;100;0;0~#1;0;100;0~";
        let img = parse_sixel(data, 0, 0, 8, 16).unwrap();
        assert_eq!(img.width, 2);
        assert_eq!(img.height, 6);
        let px = |x: u32, y: u32| -> (u8, u8, u8, u8) {
            let i = ((y * img.width + x) * 4) as usize;
            (img.pixels[i], img.pixels[i + 1], img.pixels[i + 2], img.pixels[i + 3])
        };
        // Column 0 stays red, column 1 green — no bleed-through either way.
        assert_eq!(px(0, 0), (255, 0, 0, 255));
        assert_eq!(px(0, 1), (255, 0, 0, 255), "(0,1) must keep column 0's color");
        assert_eq!(px(1, 0), (0, 255, 0, 255));
        assert_eq!(px(1, 5), (0, 255, 0, 255));
    }

    #[test]
    fn zero_cell_size_does_not_panic() {
        let img = parse_sixel(b"~", 0, 0, 0, 0).unwrap();
        assert_eq!(img.cell_width, 1);
        assert_eq!(img.cell_height, 6);
    }
}
