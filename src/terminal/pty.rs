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

use std::cell::RefCell;
use std::io::{self, Read, Write};

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, MasterPty, PtySize};

use crate::config::Profile;

/// A live PTY session.
///
/// The master is held as a `Box<dyn MasterPty + Send>`. If you need shared
/// access across threads, wrap the whole `PtySession` in an `Arc<Mutex<_>>`
/// — the per-tab state is small enough that this is cheaper than trying to
/// share the master alone.
///
/// ## Writer lifetime
///
/// The PTY writer is taken **once** at spawn (via `MasterPty::take_writer()`)
/// and held for the lifetime of this `PtySession`. This is mandatory:
/// `portable_pty` explicitly forbids calling `take_writer()` more than once,
/// and dropping the writer sends EOF to the child process (its `Drop` impl
/// writes `\n` + VEOF to the PTY). If we took a fresh writer per write — as
/// an earlier version did — the first keystroke would kill the child shell
/// and every subsequent keystroke would silently fail.
pub struct PtySession {
    pub master: Box<dyn MasterPty + Send>,
    pub pid: u32,
    pub reader: Box<dyn Read + Send>,
    /// The single PTY writer, held for the session lifetime. `RefCell`
    /// because `Write::write` needs `&mut self` while the public API
    /// (`write_input`, `route_input`) threads `&self` through the
    /// broadcast fan-out. All access is from the main event-loop thread,
    /// so `RefCell` (not `Mutex`) is the right primitive.
    writer: RefCell<Box<dyn Write + Send>>,
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

        // Take the writer ONCE, now, and hold it for the session lifetime.
        // portable_pty forbids a second `take_writer()` call, and dropping
        // the writer sends EOF to the child — both of which would break
        // per-write writer acquisition.
        let writer = pair
            .master
            .take_writer()
            .context("taking pty writer")?;

        let master = pair.master;

        // Drop the slave handle in the parent. The child still has its FDs
        // (inherited via dup2 inside portable-pty) so it can keep talking.
        drop(pair.slave);

        Ok(Self { master, pid, reader, writer: RefCell::new(writer) })
    }

    /// Send raw bytes to the child process (e.g. keystrokes, paste).
    ///
    /// Borrows the session-long writer via `RefCell`; all writes go through
    /// the same `Box<dyn Write + Send>` taken at spawn.
    pub fn write_all(&self, data: &[u8]) -> io::Result<()> {
        let mut w = self.writer.borrow_mut();
        w.write_all(data)?;
        w.flush()
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

    /// Regression test for the "first keystroke kills the shell" bug.
    ///
    /// The old `write_all` called `take_writer()` on every write. portable_pty
    /// forbids that ("cannot take writer more than once") AND its writer's
    /// `Drop` sends EOF to the child. So the first write succeeded but killed
    /// the shell, and every subsequent write failed silently.
    ///
    /// This test writes multiple times through the same `PtySession` and
    /// verifies that (a) every write returns Ok and (b) the child `cat`
    /// process is still alive (we can read back the echo of the second write).
    #[test]
    fn multiple_writes_do_not_kill_child() {
        let p = Profile {
            command: vec!["cat".into()],
            cwd: None,
            tag: None,
            env: HashMap::new(),
        };
        let mut session = PtySession::spawn(&p, "/bin/sh", 40, 10).unwrap();

        // First write must succeed.
        session.write_all(b"first\n").expect("first write should succeed");

        // Give `cat` a moment to echo it back so we can drain it.
        std::thread::sleep(std::time::Duration::from_millis(30));
        let mut drain = [0u8; 128];
        let _ = session.reader.read(&mut drain);

        // Second write must ALSO succeed — this is the regression.
        // The old code failed here with "cannot take writer more than once".
        session.write_all(b"second\n").expect("second write should succeed");

        // Read back the echo of the second write. If the first write's
        // writer-drop had sent EOF, `cat` would have exited and we'd see
        // EOF (0 bytes) instead of "second".
        std::thread::sleep(std::time::Duration::from_millis(30));
        let mut buf = [0u8; 128];
        let n = session.reader.read(&mut buf).expect("read should not error");
        let out = String::from_utf8_lossy(&buf[..n]);
        assert!(
            out.contains("second"),
            "child should have echoed 'second' back, got {out:?} ({} bytes)",
            n
        );
    }

    /// The writer is taken exactly once at spawn; calling write_all many
    /// times must never error or panic.
    #[test]
    fn many_sequential_writes_all_succeed() {
        let p = Profile {
            command: vec!["cat".into()],
            cwd: None,
            tag: None,
            env: HashMap::new(),
        };
        let mut session = PtySession::spawn(&p, "/bin/sh", 40, 10).unwrap();

        for i in 0..50 {
            session.write_all(format!("line {i}\n").as_bytes())
                .expect("every write should succeed");
            // Drain the echo so the PTY buffer doesn't fill and block.
            std::thread::sleep(std::time::Duration::from_millis(5));
            let mut drain = [0u8; 256];
            let _ = session.reader.read(&mut drain);
        }
    }
}
