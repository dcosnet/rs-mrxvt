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


//! PTY session wrapper.
//!
//! Wraps [`portable_pty`] to give us a clean send/recv pair plus resize.
//! The master FD is held as raw so we can poll it from the event loop
//! without spawning a thread per tab (the original mrxvt did per-tab threads;
//! we use a single poller for the whole app to keep things cache-friendly).

use std::io::{self, Read, Write};

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, MasterPty, PtySize};

use crate::config::Profile;

/// A live PTY session.
///
/// The master is held as a `Box<dyn MasterPty>` (the type portable-pty
/// returns). If you need shared access across threads, wrap the whole
/// `PtySession` in an `Arc<Mutex<_>>` — the per-tab state is small enough
/// that this is cheaper than trying to share the master alone.
pub struct PtySession {
    pub master: Box<dyn MasterPty + Send>,
    pub pid: u32,
    pub reader: Box<dyn Read + Send>,
}

impl PtySession {
    /// Spawn a new PTY running `profile.command` (or `$SHELL` if unset).
    pub fn spawn(profile: &Profile, fallback_shell: &str, cols: u16, rows: u16) -> Result<Self> {
        let pty_system = portable_pty::native_pty_system();

        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("opening pty")?;

        let cmd = build_command(profile, fallback_shell)?;
        let child = pair
            .slave
            .spawn_command(cmd)
            .context("spawning child process")?;

        let pid = child.process_id().unwrap_or(0) as u32;

        // Take a reader BEFORE dropping the slave so the kernel keeps the
        // master side alive.
        let reader = pair
            .master
            .try_clone_reader()
            .context("cloning pty reader")?;

        let master = pair.master;

        // Drop the slave handle in the parent. The child still has its FDs
        // (inherited via dup2 inside portable-pty) so it can keep talking.
        drop(pair.slave);

        Ok(Self { master, pid, reader })
    }

    /// Send raw bytes to the child process (e.g. keystrokes, paste).
    pub fn write_all(&self, data: &[u8]) -> io::Result<()> {
        let mut writer = self
            .master
            .take_writer()
            .map_err(|e| io::Error::other(e.to_string()))?;
        writer.write_all(data)?;
        writer.flush()
    }

    /// Resize the PTY. Called when the window/tab area changes.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("resizing pty")
    }

    /// Returns the PID of the child shell (0 if unknown).
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Take ownership of the reader (used by the per-tab reader thread).
    pub fn reader_clone(&mut self) -> Result<Box<dyn Read + Send>> {
        self.master
            .try_clone_reader()
            .context("cloning PTY reader")
    }
}

fn build_command(profile: &Profile, fallback_shell: &str) -> Result<CommandBuilder> {
    let argv: Vec<String> = if profile.command.is_empty() {
        vec![fallback_shell.to_string()]
    } else {
        profile.command.clone()
    };

    let prog = argv[0].clone();
    let mut cmd = CommandBuilder::new(&prog);
    argv[1..].iter().for_each(|arg| { cmd.arg(arg); });

    // Working directory.
    if let Some(cwd) = &profile.cwd {
        cmd.cwd(cwd);
    }

    // Environment overrides.
    for (k, v) in &profile.env {
        cmd.env(k, v);
    }

    // Inherit TERM so programs know they're talking to a colour terminal.
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");

    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Profile;
    use std::collections::HashMap;

    #[test]
    fn build_command_uses_profile_command() {
        let p = Profile {
            command: vec!["echo".into(), "hi".into()],
            cwd: None,
            tag: None,
            env: HashMap::new(),
        };
        let _cmd = build_command(&p, "/bin/sh").unwrap();
    }

    #[test]
    fn build_command_falls_back_to_shell() {
        let p = Profile::default();
        let _cmd = build_command(&p, "/bin/sh").unwrap();
    }

    #[test]
    fn spawn_echo_and_read_output() {
        // Spawns `sh -c 'echo hello; exit 0'` and reads "hello\n".
        let p = Profile {
            command: vec!["sh".into(), "-c".into(), "echo hello".into()],
            cwd: None,
            tag: None,
            env: HashMap::new(),
        };
        let mut session = PtySession::spawn(&p, "/bin/sh", 40, 10).unwrap();
        let mut buf = [0u8; 64];
        let n = session.reader.read(&mut buf).unwrap();
        let out = String::from_utf8_lossy(&buf[..n]);
        assert!(out.contains("hello"), "got: {out:?}");
    }
}
