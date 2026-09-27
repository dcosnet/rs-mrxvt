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


//! Image protocol support (Sixel + iTerm2 inline images).
//!
//! Compiles only with `--features images`. Adds the ability to display
//! inline images in the terminal, as used by `ranger`, `neofetch`, `chafa`,
//! `viu`, and other TUI image viewers.
//!
//! ## Supported protocols
//!
//! - **iTerm2 inline images**: `ESC ] 1337 ; File = ... : <base64> ST`
//!   The most common modern protocol, supported by iTerm2, WezTerm, Kitty,
//!   and others.
//! - **Sixel**: `DCS q ... ST` — the older DEC protocol, still used by
//!   `mlterm`, `xterm` (with `-ti vt340`), and some embedded terminals.
//!
//! ## Implementation
//!
//! Image data is parsed out of the PTY stream and stored in an [`ImageStore`]
//! keyed by the cell range it occupies. The renderer queries the store when
//! drawing cells; if a cell has an image, the renderer composites the image
//! pixels instead of (or in addition to) the cell's text.
//!
//! For the MVP, images are stored as RGBA buffers and rendered as cell-sized
//! quads. Proper aspect-ratio preservation and scrolling are deferred.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Mutex;

use image::ImageDecoder;

/// A decoded inline image.
#[derive(Debug, Clone)]
pub struct InlineImage {
    /// RGBA pixel data.
    pub pixels: Vec<u8>,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// The cell column where the image starts.
    pub start_col: u32,
    /// The cell row where the image starts.
    pub start_row: u32,
    /// How many cells wide the image occupies (rounded up).
    pub cell_width: u32,
    /// How many cells tall the image occupies (rounded up).
    pub cell_height: u32,
}

impl InlineImage {
    /// Decode an image from raw bytes (PNG, JPEG, GIF, etc.) and compute
    /// the cell footprint based on the given cell dimensions.
    pub fn from_bytes(
        bytes: &[u8],
        start_col: u32,
        start_row: u32,
        cell_w_px: u32,
        cell_h_px: u32,
    ) -> Result<Self, ImageError> {
        let format = image::guess_format(bytes).map_err(ImageError::Format)?;
        let cursor = Cursor::new(bytes);
        let decoder: Box<dyn ImageDecoder> = match format {
            image::ImageFormat::Png => Box::new(
                image::codecs::png::PngDecoder::new(cursor).map_err(ImageError::Decode)?,
            ),
            image::ImageFormat::Jpeg => Box::new(
                image::codecs::jpeg::JpegDecoder::new(cursor).map_err(ImageError::Decode)?,
            ),
            image::ImageFormat::Gif => Box::new(
                image::codecs::gif::GifDecoder::new(cursor).map_err(ImageError::Decode)?,
            ),
            image::ImageFormat::WebP => Box::new(
                image::codecs::webp::WebPDecoder::new(cursor).map_err(ImageError::Decode)?,
            ),
            image::ImageFormat::Bmp => Box::new(
                image::codecs::bmp::BmpDecoder::new(cursor).map_err(ImageError::Decode)?,
            ),
            _ => {
                return Err(ImageError::Unsupported(format!(
                    "unsupported image format: {format:?}"
                )))
            }
        };
        let (w, h) = decoder.dimensions();
        let buf_size = (w as usize)
            .checked_mul(h as usize)
            .and_then(|s| s.checked_mul(4))
            .ok_or_else(|| ImageError::InvalidArgs("image dimensions too large".into()))?;
        let mut pixels = vec![0u8; buf_size];
        decoder
            .read_image(&mut pixels)
            .map_err(ImageError::Decode)?;

        let cell_width = w.div_ceil(cell_w_px);
        let cell_height = h.div_ceil(cell_h_px);

        Ok(Self {
            pixels,
            width: w,
            height: h,
            start_col,
            start_row,
            cell_width,
            cell_height,
        })
    }
}

/// Error type for image decoding.
#[derive(Debug)]
pub enum ImageError {
    Format(image::ImageError),
    Decode(image::ImageError),
    Base64(String),
    InvalidArgs(String),
    Unsupported(String),
}

impl std::fmt::Display for ImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImageError::Format(e) => write!(f, "format detection: {e}"),
            ImageError::Decode(e) => write!(f, "decode: {e}"),
            ImageError::Base64(s) => write!(f, "base64: {s}"),
            ImageError::InvalidArgs(s) => write!(f, "invalid args: {s}"),
            ImageError::Unsupported(s) => write!(f, "unsupported: {s}"),
        }
    }
}

impl std::error::Error for ImageError {}

/// Store of inline images, keyed by an arbitrary ID.
///
/// Thread-safe via `Mutex` because the PTY reader thread writes to it while
/// the render thread reads. The mutex is held only briefly during insert
/// and lookup.
#[derive(Default)]
pub struct ImageStore {
    inner: Mutex<HashMap<u32, InlineImage>>,
    next_id: std::sync::atomic::AtomicU32,
}

impl ImageStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            next_id: std::sync::atomic::AtomicU32::new(1),
        }
    }

    /// Insert an image, returning its ID.
    pub fn insert(&self, img: InlineImage) -> u32 {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut guard = self.inner.lock().expect("image store mutex poisoned");
        guard.insert(id, img);
        id
    }

    /// Look up an image by ID.
    pub fn get(&self, id: u32) -> Option<InlineImage> {
        let guard = self.inner.lock().expect("image store mutex poisoned");
        guard.get(&id).cloned()
    }

    /// Remove an image by ID.
    pub fn remove(&self, id: u32) -> bool {
        let mut guard = self.inner.lock().expect("image store mutex poisoned");
        guard.remove(&id).is_some()
    }

    /// Number of stored images.
    pub fn len(&self) -> usize {
        let guard = self.inner.lock().expect("image store mutex poisoned");
        guard.len()
    }

    /// Is the store empty?
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Clear all images.
    pub fn clear(&self) {
        let mut guard = self.inner.lock().expect("image store mutex poisoned");
        guard.clear();
    }

    /// Iterate over all images (clones them; use sparingly).
    pub fn all(&self) -> Vec<(u32, InlineImage)> {
        let guard = self.inner.lock().expect("image store mutex poisoned");
        guard.iter().map(|(k, v)| (*k, v.clone())).collect()
    }
}

impl std::fmt::Debug for ImageStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let len = self.len();
        f.debug_struct("ImageStore").field("count", &len).finish()
    }
}

// ─── iTerm2 protocol parsing ─────────────────────────────────────────────────

/// Parse an iTerm2 inline image escape sequence.
///
/// Format: `ESC ] 1337 ; File = <args> : <base64-data> ST`
///
/// Where `<args>` is a semicolon-separated list of `key=value` pairs.
/// Important keys:
/// - `name`: display name (ignored)
/// - `size`: byte size of the original image (informational)
/// - `width`: cell width in columns (e.g. "10" or "auto")
/// - `height`: cell height in rows
/// - `inline`: 1 = display inline, 0 = download only
/// - `preserveAspectRatio`: 0 or 1
pub fn parse_iterm2_sequence(
    args_and_data: &str,
    start_col: u32,
    start_row: u32,
    cell_w_px: u32,
    cell_h_px: u32,
) -> Result<InlineImage, ImageError> {
    // Split into args and base64 data on the first colon.
    let colon = args_and_data
        .find(':')
        .ok_or_else(|| ImageError::InvalidArgs("missing ':' separator".into()))?;
    let args_str = &args_and_data[..colon];
    let b64 = &args_and_data[colon + 1..];

    // Parse args.
    let mut width_cells: Option<u32> = None;
    let mut height_cells: Option<u32> = None;
    for kv in args_str.split(';') {
        if let Some(eq) = kv.find('=') {
            let key = &kv[..eq];
            let value = &kv[eq + 1..];
            match key {
                "width" if value != "auto" => width_cells = value.parse().ok(),
                "height" if value != "auto" => height_cells = value.parse().ok(),
                _ => {}
            }
        }
    }

    // Decode base64.
    let raw = base64_decode(b64).ok_or_else(|| ImageError::Base64("invalid base64".into()))?;

    // Decode the image.
    let mut img = InlineImage::from_bytes(&raw, start_col, start_row, cell_w_px, cell_h_px)?;

    // Override cell footprint if explicit width/height were given.
    if let Some(w) = width_cells {
        img.cell_width = w;
    }
    if let Some(h) = height_cells {
        img.cell_height = h;
    }

    Ok(img)
}

/// Tiny base64 decoder (avoids pulling in a base64 crate just for this).
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    for c in s.chars() {
        if c.is_whitespace() || c == '=' {
            continue;
        }
        let val: u32 = match c {
            'A'..='Z' => (c as u32) - ('A' as u32),
            'a'..='z' => (c as u32) - ('a' as u32) + 26,
            '0'..='9' => (c as u32) - ('0' as u32) + 52,
            '+' | '-' => 62,
            '/' | '_' => 63,
            _ => return None,
        };
        buf = (buf << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_insert_and_get() {
        let store = ImageStore::new();
        let img = InlineImage {
            pixels: vec![0; 4],
            width: 1,
            height: 1,
            start_col: 0,
            start_row: 0,
            cell_width: 1,
            cell_height: 1,
        };
        let id = store.insert(img);
        assert!(store.get(id).is_some());
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn store_remove() {
        let store = ImageStore::new();
        let img = InlineImage {
            pixels: vec![0; 4],
            width: 1,
            height: 1,
            start_col: 0,
            start_row: 0,
            cell_width: 1,
            cell_height: 1,
        };
        let id = store.insert(img);
        assert!(store.remove(id));
        assert!(!store.remove(id));
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn store_clear() {
        let store = ImageStore::new();
        for _ in 0..3 {
            store.insert(InlineImage {
                pixels: vec![0; 4],
                width: 1,
                height: 1,
                start_col: 0,
                start_row: 0,
                cell_width: 1,
                cell_height: 1,
            });
        }
        assert_eq!(store.len(), 3);
        store.clear();
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn store_all_returns_clones() {
        let store = ImageStore::new();
        store.insert(InlineImage {
            pixels: vec![0; 4],
            width: 1,
            height: 1,
            start_col: 0,
            start_row: 0,
            cell_width: 1,
            cell_height: 1,
        });
        let v = store.all();
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn base64_decodes_simple() {
        // "hello" → base64 → "aGVsbG8="
        let decoded = base64_decode("aGVsbG8=").unwrap();
        assert_eq!(decoded, b"hello");
    }

    #[test]
    fn base64_rejects_garbage() {
        assert!(base64_decode("@#$%").is_none());
    }

    #[test]
    fn base64_handles_whitespace() {
        let decoded = base64_decode("aGV sbG 8=").unwrap();
        assert_eq!(decoded, b"hello");
    }

    #[test]
    fn parse_iterm2_missing_colon_errors() {
        let result = parse_iterm2_sequence("no_colon_here", 0, 0, 8, 16);
        assert!(result.is_err());
    }

    #[test]
    fn parse_iterm2_invalid_base64_errors() {
        let result = parse_iterm2_sequence("name=test:@#$%", 0, 0, 8, 16);
        assert!(result.is_err());
    }

    #[test]
    fn image_error_display_works() {
        let e = ImageError::Base64("test".into());
        assert!(format!("{e}").contains("base64"));
    }
}
