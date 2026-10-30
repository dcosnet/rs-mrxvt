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


//! Clipboard integration: copy, paste, OSC 52.
//!
//! rs-mrxvt keeps its "no mandatory system dependencies" promise here too:
//! instead of linking a clipboard library, it shells out to the standard
//! clipboard tools that are already installed on virtually every desktop:
//!
//! | Environment | Copy                    | Paste                     |
//! |-------------|-------------------------|---------------------------|
//! | X11         | `xclip` / `xsel`        | `xclip` / `xsel`          |
//! | Wayland     | `wl-copy`               | `wl-paste`                |
//! | macOS       | `pbcopy`                | `pbpaste`                 |
//! | Android     | `termux-clipboard-set`  | `termux-clipboard-get`    |
//!
//! ## OSC 52
//!
//! On copy, rs-mrxvt *also* emits an [OSC 52] escape sequence to its host
//! terminal. This is the same mechanism terminal-aware programs (tmux, ssh
//! through intermediate hosts, neovim) use, and it means copy works even
//! when rs-mrxvt runs over SSH with no local clipboard tool: the *client's*
//! terminal performs the clipboard write.
//!
//! The payload is base64-encoded and capped at [`OSC52_MAX_BYTES`] raw
//! bytes (the de-facto limit accepted by xterm-class terminals); larger
//! copies still go to the system clipboard via the external tool, only the
//! OSC 52 mirror is skipped.
//!
//! [OSC 52]: https://invisible-island.net/xterm/ctlseqs/ctlseqs.html#h3-Operating-System-Commands

use std::collections::HashMap;
use std::io::Write;
use std::process::{Command as StdCommand, Stdio};
use std::sync::Mutex;

/// Which clipboard to address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClipboardKind {
    /// The system clipboard (Ctrl+C/V clipboard).
    Clipboard,
    /// The X11 "primary" selection (middle-click paste).
    Primary,
}

impl From<alacritty_terminal::term::ClipboardType> for ClipboardKind {
    fn from(ty: alacritty_terminal::term::ClipboardType) -> Self {
        match ty {
            alacritty_terminal::term::ClipboardType::Clipboard => ClipboardKind::Clipboard,
            alacritty_terminal::term::ClipboardType::Selection => ClipboardKind::Primary,
        }
    }
}

/// Raw payload cap for the OSC 52 mirror (xterm-class terminals accept
/// roughly 100 KiB of base64; we cap the *raw* bytes at 75 KiB to stay
/// comfortably inside that).
pub const OSC52_MAX_BYTES: usize = 75 * 1024;

/// A clipboard backend.
///
/// Production code uses [`SystemClipboard`]; tests use [`MockClipboard`].
pub trait ClipboardBackend: Send {
    /// Store `text` into the given clipboard.
    fn store(&mut self, kind: ClipboardKind, text: &str) -> anyhow::Result<()>;

    /// Load the current contents of the given clipboard.
    fn load(&mut self, kind: ClipboardKind) -> anyhow::Result<String>;

    /// Human-readable name for status messages / logging.
    fn describe(&self) -> &'static str {
        "clipboard"
    }

    /// Downcast hook so tests can inspect a [`MockClipboard`] behind the
    /// trait object. Production backends return `None`.
    fn as_any(&mut self) -> Option<&mut dyn std::any::Any> {
        None
    }
}

// ─── external tool discovery ─────────────────────────────────────────────────

/// A clipboard tool pair: (copy argv, paste argv), with `{primary}` support.
#[derive(Clone)]
struct ToolSpec {
    name: &'static str,
    copy: Vec<String>,
    paste: Vec<String>,
    /// Extra argv fragment inserted for the primary selection.
    primary_flag: Vec<String>,
}

fn tool_specs() -> Vec<ToolSpec> {
    fn s(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }
    vec![
        ToolSpec {
            name: "xclip",
            copy: s(&["xclip", "-in", "-selection", "{sel}"]),
            paste: s(&["xclip", "-out", "-selection", "{sel}"]),
            primary_flag: s(&["primary"]),
        },
        ToolSpec {
            name: "xsel",
            copy: s(&["xsel", "--input", "{sel}"]),
            paste: s(&["xsel", "--output", "{sel}"]),
            primary_flag: s(&["primary"]),
        },
        ToolSpec {
            name: "wl-clipboard",
            copy: s(&["wl-copy", "{primary}"]),
            paste: s(&["wl-paste", "{primary}", "--no-newline"]),
            primary_flag: s(&["--primary"]),
        },
        ToolSpec {
            name: "pbcopy/pbpaste",
            copy: s(&["pbcopy"]),
            paste: s(&["pbpaste"]),
            primary_flag: s(&[]),
        },
        ToolSpec {
            name: "termux-clipboard",
            copy: s(&["termux-clipboard-set"]),
            paste: s(&["termux-clipboard-get"]),
            primary_flag: s(&[]),
        },
    ]
}

fn which(program: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else { return false };
    path.split(':').any(|dir| {
        let p = std::path::Path::new(dir).join(program);
        p.is_file()
    })
}

/// The tool families available on this machine, in priority order.
/// Detected once, then cached.
fn available_tools() -> Vec<ToolSpec> {
    static CACHE: Mutex<Option<Vec<ToolSpec>>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(tools) = cache.clone() {
        return tools;
    }
    let found: Vec<ToolSpec> = tool_specs()
        .into_iter()
        .filter(|t| {
            let prog = t
                .copy
                .first()
                .map(|c| c.split(' ').next().unwrap_or("").to_string())
                .unwrap_or_default();
            which(&prog)
        })
        .collect();
    *cache = Some(found.clone());
    found
}

fn expand_argv(argv: &[String], kind: ClipboardKind, primary_flag: &[String]) -> Vec<String> {
    let sel_word = match kind {
        ClipboardKind::Clipboard => "clipboard",
        ClipboardKind::Primary => "primary",
    };
    let mut out = Vec::with_capacity(argv.len() + 1);
    for a in argv {
        if a == "{sel}" {
            out.push(sel_word.to_string());
        } else if a == "{primary}" {
            if kind == ClipboardKind::Primary {
                out.extend(primary_flag.iter().cloned());
            }
        } else {
            out.push(a.clone());
        }
    }
    out.retain(|a| !a.is_empty());
    out
}

// ─── system clipboard ────────────────────────────────────────────────────────

/// The production clipboard backend.
///
/// Writes go to the first available external tool *and* (optionally) to the
/// host terminal via OSC 52. Reads come from the external tool; there is no
/// reliable synchronous way to read a host terminal's clipboard, so over
/// bare SSH without a tool, paste is simply unavailable and reports an
/// error the caller can surface.
pub struct SystemClipboard {
    /// Explicit tool override from config (`clipboard.tool = "xclip"`).
    tool: Option<String>,
    /// Mirror copies to the host terminal via OSC 52.
    osc52: bool,
    /// Suppresses OSC 52 in tests / when stdout is not a terminal.
    osc52_enabled: bool,
}

impl SystemClipboard {
    pub fn new(tool: Option<String>, osc52: bool) -> Self {
        Self {
            tool,
            osc52,
            osc52_enabled: true,
        }
    }

    /// Build from the TOML `[clipboard]` section.
    pub fn from_config(cfg: &crate::config::ClipboardConfig) -> Self {
        let tool = cfg.tool.as_ref().filter(|t| !t.trim().is_empty()).cloned();
        Self::new(tool, cfg.osc52)
    }

    /// Disable OSC 52 emission entirely (used by tests).
    pub fn disable_osc52(&mut self) {
        self.osc52_enabled = false;
    }

    fn tool_copy_argv(&self, kind: ClipboardKind) -> Option<Vec<String>> {
        if let Some(name) = &self.tool {
            // User-forced tool: find its spec by name prefix.
            let spec = tool_specs().into_iter().find(|t| {
                t.copy
                    .first()
                    .map(|c| c.starts_with(name.as_str()))
                    .unwrap_or(false)
                    || t.name.starts_with(name.as_str())
            })?;
            return Some(expand_argv(&spec.copy, kind, &spec.primary_flag));
        }
        available_tools()
            .first()
            .map(|t| expand_argv(&t.copy, kind, &t.primary_flag))
    }

    fn tool_paste_argv(&self, kind: ClipboardKind) -> Option<Vec<String>> {
        if let Some(name) = &self.tool {
            let spec = tool_specs().into_iter().find(|t| {
                t.copy
                    .first()
                    .map(|c| c.starts_with(name.as_str()))
                    .unwrap_or(false)
                    || t.name.starts_with(name.as_str())
            })?;
            return Some(expand_argv(&spec.paste, kind, &spec.primary_flag));
        }
        available_tools()
            .first()
            .map(|t| expand_argv(&t.paste, kind, &t.primary_flag))
    }

    fn tool_store(&self, kind: ClipboardKind, text: &str) -> anyhow::Result<()> {
        let argv = self
            .tool_copy_argv(kind)
            .ok_or_else(|| anyhow::anyhow!("no clipboard tool found (install xclip, xsel, or wl-clipboard)"))?;
        let mut child = StdCommand::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| anyhow::anyhow!("failed to spawn {}: {e}", argv[0]))?;
        if let Some(stdin) = child.stdin.as_mut() {
            stdin.write_all(text.as_bytes())?;
        }
        // Dropping stdin closes the pipe; the tool then flushes.
        let status = child.wait()?;
        if !status.success() {
            anyhow::bail!("{} exited with {status}", argv[0]);
        }
        Ok(())
    }

    fn tool_load(&self, kind: ClipboardKind) -> anyhow::Result<String> {
        let argv = self
            .tool_paste_argv(kind)
            .ok_or_else(|| anyhow::anyhow!("no clipboard tool found (install xclip, xsel, or wl-clipboard)"))?;
        let out = StdCommand::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|e| anyhow::anyhow!("failed to spawn {}: {e}", argv[0]))?;
        if !out.status.success() {
            anyhow::bail!("{} exited with {}", argv[0], out.status);
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Emit an OSC 52 "set clipboard" sequence to the host terminal.
    ///
    /// `ESC ] 52 ; <selection> ; <base64> BEL` where `<selection>` is `c`
    /// for the clipboard and `p` for the primary selection. Terminals that
    /// don't implement OSC 52 ignore unknown OSC sequences, so this is
    /// always safe to write.
    fn emit_osc52(&self, kind: ClipboardKind, text: &str) {
        if !self.osc52 || !self.osc52_enabled {
            return;
        }
        let raw = text.as_bytes();
        if raw.len() > OSC52_MAX_BYTES {
            // Too large for OSC 52; the tool path already handled the copy.
            log::debug!(
                "OSC 52 mirror skipped: {} bytes > cap {OSC52_MAX_BYTES}",
                raw.len()
            );
            return;
        }
        let sel = match kind {
            ClipboardKind::Clipboard => "c",
            ClipboardKind::Primary => "p",
        };
        let seq = format!("\x1b]52;{sel};{}\x07", base64_encode(raw));
        // Write outside of ratatui's frame: we're between draws here (event
        // handling), so a direct write + flush cannot corrupt the frame.
        let mut out = std::io::stdout();
        let _ = out.write_all(seq.as_bytes());
        let _ = out.flush();
    }
}

impl Default for SystemClipboard {
    fn default() -> Self {
        Self::new(None, true)
    }
}

impl ClipboardBackend for SystemClipboard {
    fn store(&mut self, kind: ClipboardKind, text: &str) -> anyhow::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        // OSC 52 first (never fails), then the tool (may fail, e.g. no
        // display over SSH — that's fine, the host terminal got it).
        self.emit_osc52(kind, text);
        match self.tool_store(kind, text) {
            Ok(()) => Ok(()),
            Err(e) => {
                // A tool failure is only tolerable when OSC 52 actually
                // delivered the copy to the host terminal. When OSC 52 is
                // disabled (or the payload exceeded its cap), nothing was
                // delivered anywhere — surface the failure so the UI can
                // say "Copy failed" instead of lying with "Copied N chars".
                let delivered_by_osc52 =
                    self.osc52 && self.osc52_enabled && text.len() <= OSC52_MAX_BYTES;
                if delivered_by_osc52 {
                    log::debug!("clipboard tool store failed (OSC 52 delivered instead): {e}");
                    Ok(())
                } else {
                    Err(anyhow::anyhow!("clipboard tool failed and OSC 52 disabled: {e}"))
                }
            }
        }
    }

    fn load(&mut self, kind: ClipboardKind) -> anyhow::Result<String> {
        self.tool_load(kind)
    }

    fn describe(&self) -> &'static str {
        if available_tools().is_empty() {
            "osc52-only"
        } else {
            "system"
        }
    }
}

// ─── mock ────────────────────────────────────────────────────────────────────

/// In-memory clipboard for tests.
#[derive(Default)]
pub struct MockClipboard {
    pub stores: Vec<(ClipboardKind, String)>,
    pub loads: Vec<ClipboardKind>,
    pub storage: HashMap<ClipboardKind, String>,
    /// When set, `load` returns this error.
    pub fail_load: bool,
}

impl MockClipboard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed the mock so `load` returns `text`.
    pub fn with_content(kind: ClipboardKind, text: &str) -> Self {
        let mut m = Self::new();
        m.storage.insert(kind, text.to_string());
        m
    }
}

impl ClipboardBackend for MockClipboard {
    fn store(&mut self, kind: ClipboardKind, text: &str) -> anyhow::Result<()> {
        self.stores.push((kind, text.to_string()));
        self.storage.insert(kind, text.to_string());
        Ok(())
    }

    fn load(&mut self, kind: ClipboardKind) -> anyhow::Result<String> {
        self.loads.push(kind);
        if self.fail_load {
            anyhow::bail!("mock clipboard read failure");
        }
        Ok(self.storage.get(&kind).cloned().unwrap_or_default())
    }

    fn describe(&self) -> &'static str {
        "mock"
    }

    fn as_any(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

// ─── base64 ──────────────────────────────────────────────────────────────────

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Minimal standard base64 encoder (with padding). Enough for OSC 52; no
/// need to pull in a crate for ~20 lines.
pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(B64[(n >> 6) as usize & 63] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(B64[n as usize & 63] as char);
        } else {
            out.push('=');
        }
    }
    out
}

// ─── paste sanitation ────────────────────────────────────────────────────────

/// Sanitize text before pasting it into a PTY.
///
/// - C0 control characters other than `\t`, `\n` are stripped (they could
///   inject escape sequences or signals into the child).
/// - C1 control characters (0x80–0x9F as raw bytes in UTF-8 can't occur;
///   as decoded chars they can) are stripped for the same reason.
/// - `\r\n` and lone `\r` are normalized to `\n` — shells treat a raw `\r`
///   as a cursor return, which corrupts the command line.
/// - ONE trailing `\n` is dropped (together with the spaces/tabs directly
///   before it) so a paste doesn't auto-submit the command. Whitespace
///   *without* a trailing newline is preserved — trailing spaces in pasted
///   code are legitimate content.
pub fn sanitize_paste(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next(); // swallow the \n of \r\n
                }
                out.push('\n');
            }
            '\t' | '\n' => out.push(c),
            c if (c as u32) < 0x20 => { /* strip other C0 */ }
            c if ('\u{7f}'..'\u{a0}').contains(&c) => { /* strip DEL + C1 */ }
            c => out.push(c),
        }
    }
    // Drop ONE trailing newline so paste doesn't auto-submit; keep the rest.
    // Only the whitespace immediately before that newline is trimmed.
    match out.strip_suffix('\n') {
        Some(body) => body.trim_end_matches([' ', '\t']).to_string(),
        None => out,
    }
}

/// Wrap text in bracketed-paste markers (`ESC[200~ … ESC[201~`).
pub fn bracket(text: &str) -> String {
    format!("\x1b[200~{text}\x1b[201~")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        // RFC 4648 test vectors.
        assert_eq!(base64_encode(b"hello world"), "aGVsbG8gd29ybGQ=");
    }

    #[test]
    fn mock_store_and_load() {
        let mut m = MockClipboard::new();
        m.store(ClipboardKind::Clipboard, "abc").unwrap();
        assert_eq!(m.load(ClipboardKind::Clipboard).unwrap(), "abc");
        assert_eq!(m.stores.len(), 1);
        // Primary is a different clipboard.
        assert_eq!(m.load(ClipboardKind::Primary).unwrap(), "");
    }

    #[test]
    fn sanitize_strips_control_chars() {
        // ESC from a pasted arrow-key sequence is stripped; the rest kept.
        assert_eq!(sanitize_paste("ls\x1b[Afoo"), "ls[Afoo");
        assert_eq!(sanitize_paste("echo\x07 hi"), "echo hi");
        assert_eq!(sanitize_paste("a\x00b"), "ab");
        // C1 controls are stripped too.
        assert_eq!(sanitize_paste("a\u{9c}b"), "ab");
    }

    #[test]
    fn sanitize_normalizes_newlines() {
        assert_eq!(sanitize_paste("one\r\ntwo"), "one\ntwo");
        assert_eq!(sanitize_paste("one\rtwo"), "one\ntwo");
    }

    #[test]
    fn sanitize_strips_single_trailing_newline_only() {
        assert_eq!(sanitize_paste("cmd\n"), "cmd");
        assert_eq!(sanitize_paste("cmd\n\n"), "cmd\n");
        assert_eq!(sanitize_paste("cmd"), "cmd");
    }

    #[test]
    fn sanitize_keeps_tabs_and_interior_newlines() {
        assert_eq!(sanitize_paste("a\tb\nc"), "a\tb\nc");
    }

    #[test]
    fn sanitize_keeps_trailing_whitespace_without_newline() {
        // Regression: trailing spaces/tabs used to be stripped even when
        // there was no trailing newline — deleting legitimate content.
        assert_eq!(sanitize_paste("foo "), "foo ");
        assert_eq!(sanitize_paste("let x = 1;   "), "let x = 1;   ");
        assert_eq!(sanitize_paste("indent\t"), "indent\t");
    }

    #[test]
    fn sanitize_trims_whitespace_before_dropped_newline() {
        // "cmd \n" should not leave a stray trailing space behind.
        assert_eq!(sanitize_paste("cmd \n"), "cmd");
        assert_eq!(sanitize_paste("cmd\t\n"), "cmd");
        assert_eq!(sanitize_paste("cmd \n\n"), "cmd \n");
    }

    #[test]
    fn bracket_wraps() {
        assert_eq!(bracket("x"), "\x1b[200~x\x1b[201~");
    }

    #[test]
    fn argv_expansion_for_primary() {
        let spec = &tool_specs()[0]; // xclip
        let cb = expand_argv(&spec.copy, ClipboardKind::Clipboard, &spec.primary_flag);
        assert_eq!(cb, vec!["xclip", "-in", "-selection", "clipboard"]);
        let pr = expand_argv(&spec.copy, ClipboardKind::Primary, &spec.primary_flag);
        assert_eq!(pr, vec!["xclip", "-in", "-selection", "primary"]);
    }

    #[test]
    fn osc52_sequence_format() {
        // "hi" → base64 "aGk="
        let text = "hi";
        let seq = format!("\x1b]52;c;{}\x07", base64_encode(text.as_bytes()));
        assert_eq!(seq, "\x1b]52;c;aGk=\x07");
    }
}
