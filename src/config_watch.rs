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


//! Hot-reload config watcher.
//!
//! Watches the config file for changes (via polling — no `inotify` dependency
//! for distro-agnosticism) and re-evaluates it on save. The new [`Config`]
//! is delivered through a channel; the main loop picks it up on the next
//! tick.
//!
//! ## Why polling?
//!
//! `notify` crate pulls in `inotify` on Linux, `kqueue` on BSD, and
//! ReadDirectoryChangesW on Windows. Each adds platform-specific deps.
//! For a single config file, polling every 2s is cheap (~0% CPU) and works
//! everywhere.
//!
//! ## What gets applied
//!
//! - `ui.*` (theme, tabbar height, focus model)
//! - `terminal.scrolling_history` (affects new tabs only)
//! - `profiles.*` (used when opening new tabs)
//! - `macros.*`
//! - `transparency.*`
//!
//! What does NOT get applied mid-run:
//! - `terminal.cols/rows` — would require resizing the active terminal
//! - `default_profile` — affects only new tabs anyway
//! - Currently open tabs keep their original shells/cwds

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, SystemTime};

use anyhow::Result;

use crate::config::Config;
/// A handle to a running config watcher. Drop to stop.
pub struct ConfigWatcher {
    pub config_rx: Receiver<Config>,
    pub running: Arc<AtomicBool>,
    pub handle: Option<thread::JoinHandle<()>>,
}

impl ConfigWatcher {
    /// Start a watcher that polls `path` every `interval_ms` and re-evaluates
    /// it on change. The latest [`Config`] is delivered through the returned
    /// channel.
    ///
    /// The watcher resolves Lua vs TOML based on the file extension (when
    /// the `lua` feature is enabled).
    pub fn spawn(path: PathBuf, interval_ms: u64) -> Result<Self> {
        let (tx, rx): (Sender<Config>, Receiver<Config>) = mpsc::channel();
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();

        // Initial load.
        let initial = load_config_at(&path)?;
        if tx.send(initial).is_err() {
            log::warn!("config watcher: receiver dropped before initial config sent");
        }
        let last_mtime = mtime_of(&path);

        let handle = thread::Builder::new()
            .name("mrxvt-config-watcher".into())
            .spawn(move || {
                let mut last_mtime = last_mtime;
                while running_clone.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(interval_ms));
                    let current = mtime_of(&path);
                    if current != last_mtime {
                        last_mtime = current;
                        match load_config_at(&path) {
                            Ok(cfg) => {
                                log::info!("config reloaded: {}", path.display());
                                if tx.send(cfg).is_err() {
                                    // Receiver dropped — stop watching.
                                    break;
                                }
                            }
                            Err(e) => {
                                log::warn!("config reload failed for {}: {e}", path.display());
                            }
                        }
                    }
                }
            })?;

        Ok(Self {
            config_rx: rx,
            running,
            handle: Some(handle),
        })
    }

    /// Try to receive an updated config without blocking. Returns `None` if
    /// no update is available.
    pub fn try_recv(&self) -> Option<Config> {
        self.config_rx.try_recv().ok()
    }

    /// Stop the watcher. Joins the polling thread.
    pub fn stop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            if let Err(e) = h.join() {
                log::error!("config watcher thread panicked: {e:?}");
            }
        }
    }
}

impl Drop for ConfigWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Load the config at the given path, picking the parser by extension.
fn load_config_at(path: &Path) -> Result<Config> {
    #[cfg(feature = "lua")]
    {
        return crate::config_lua::load_config_smart(Some(path));
    }
    #[cfg(not(feature = "lua"))]
    {
        let raw = if path.exists() {
            std::fs::read_to_string(path)?
        } else {
            String::new()
        };
        let cfg: Config = toml::from_str(&raw)?;
        Ok(cfg)
    }
}

/// Get the modification time of a file (or epoch zero if missing).
fn mtime_of(path: &Path) -> SystemTime {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watcher_fires_on_change() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let initial = "[terminal]\ncols = 80\n";
        std::fs::write(tmp.path(), initial).unwrap();

        let mut watcher = ConfigWatcher::spawn(tmp.path().to_path_buf(), 50).unwrap();

        // Receive the initial load.
        let cfg1 = watcher.config_rx.recv().unwrap();
        assert_eq!(cfg1.terminal.cols, 80);

        // Modify the file.
        std::thread::sleep(Duration::from_millis(100));
        let updated = "[terminal]\ncols = 120\n";
        std::fs::write(tmp.path(), updated).unwrap();

        // Wait for the watcher to pick it up.
        let cfg2 = watcher
            .config_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("watcher should deliver updated config");
        assert_eq!(cfg2.terminal.cols, 120);

        watcher.stop();
    }

    #[test]
    fn watcher_survives_bad_config() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "[terminal]\ncols = 80\n").unwrap();

        let mut watcher = ConfigWatcher::spawn(tmp.path().to_path_buf(), 50).unwrap();
        let _ = watcher.config_rx.recv().unwrap();

        // Write garbage.
        std::thread::sleep(Duration::from_millis(100));
        std::fs::write(tmp.path(), "this is not toml {{{{").unwrap();

        // Wait a bit; the watcher should log a warning but NOT send anything.
        std::thread::sleep(Duration::from_millis(200));
        assert!(watcher.try_recv().is_none(), "bad config should not be sent");

        // Fix the file — should send.
        std::fs::write(tmp.path(), "[terminal]\ncols = 99\n").unwrap();
        let cfg = watcher
            .config_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("watcher should deliver fixed config");
        assert_eq!(cfg.terminal.cols, 99);

        watcher.stop();
    }

    #[test]
    fn try_recv_returns_none_when_empty() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "[terminal]\ncols = 80\n").unwrap();

        let mut watcher = ConfigWatcher::spawn(tmp.path().to_path_buf(), 50).unwrap();
        // Drain the initial load.
        let _ = watcher.config_rx.recv().unwrap();
        // Now the queue should be empty.
        assert!(watcher.try_recv().is_none());
        watcher.stop();
    }

    #[test]
    fn stop_terminates_thread() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "[terminal]\ncols = 80\n").unwrap();

        let mut watcher = ConfigWatcher::spawn(tmp.path().to_path_buf(), 50).unwrap();
        let _ = watcher.config_rx.recv().unwrap();
        watcher.stop();
        // After stop, the handle is taken.
        assert!(watcher.handle.is_none());
    }

    #[test]
    fn drop_stops_watcher() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "[terminal]\ncols = 80\n").unwrap();

        let mut watcher = ConfigWatcher::spawn(tmp.path().to_path_buf(), 50).unwrap();
        let _ = watcher.config_rx.recv().unwrap();
        // Take the handle out so we can verify it finishes after drop.
        let handle = watcher.handle.take().unwrap();
        drop(watcher);
        // Joining should succeed quickly (within 1s).
        handle.join().expect("watcher thread should exit cleanly");
    }
}
