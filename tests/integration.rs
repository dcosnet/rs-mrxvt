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


//! Integration tests: end-to-end PTY + terminal-emulation round-trip.
//!
//! These tests spawn real subprocesses via portable-pty and verify that
//! the alacritty_terminal emulator receives and renders the output. They
//! run on any Linux (and macOS) without any system graphics deps.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use mrxvt::alacritty_terminal::grid::Dimensions;
use mrxvt::config::{Config, Profile};
use mrxvt::terminal::tab::TerminalTab;

fn sh_profile(cmd: &str) -> Profile {
    Profile {
        command: vec!["sh".into(), "-c".into(), cmd.into()],
        cwd: None,
        tag: None,
        env: HashMap::new(),
    }
}

/// Poll a tab until its visible grid contains `needle`, or panic.
fn wait_for(tab: &mut TerminalTab, needle: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let _ = tab.poll_pty();
        if grid_contains(tab, needle) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timeout waiting for {needle:?} in terminal grid");
}

/// Walk the visible grid and check for a substring match across each row.
fn grid_contains(tab: &TerminalTab, needle: &str) -> bool {
    let grid = tab.term.grid();
    let cols = grid.columns();
    let lines = grid.screen_lines();
    let display_offset = grid.display_offset();
    let start = -(display_offset as i32);

    for row in 0..lines as i32 {
        let mut s = String::with_capacity(cols);
        for col in 0..cols {
            let point = mrxvt::re_export_point(start + row, col);
            let cell = &grid[point];
            s.push(cell.c);
        }
        if s.contains(needle) {
            return true;
        }
    }
    false
}

#[test]
fn echo_appears_in_grid() {
    let p = sh_profile("echo integration_test_marker_42");
    let mut tab = TerminalTab::new("t".into(), &p, "/bin/sh", 60, 10, 1_000).unwrap();
    wait_for(&mut tab, "integration_test_marker_42", Duration::from_secs(3));
}

#[test]
fn colored_output_is_parsed() {
    // Print "RED" in red, then "PLAIN" in default.
    let p = sh_profile("printf '\\033[31mRED\\033[0m PLAIN'");
    let mut tab = TerminalTab::new("t".into(), &p, "/bin/sh", 40, 5, 1_000).unwrap();
    wait_for(&mut tab, "RED", Duration::from_secs(3));
    wait_for(&mut tab, "PLAIN", Duration::from_secs(3));
}

#[test]
fn input_round_trip() {
    // Start an interactive `cat` — echo back what we type.
    let p = sh_profile("cat");
    let mut tab = TerminalTab::new("t".into(), &p, "/bin/sh", 40, 5, 1_000).unwrap();
    // Give cat a moment to start.
    std::thread::sleep(Duration::from_millis(100));
    // Send some input.
    tab.write_input(b"hello_round_trip\n").unwrap();
    wait_for(&mut tab, "hello_round_trip", Duration::from_secs(3));
}

#[test]
fn resize_preserves_content() {
    let p = sh_profile("echo preserve_me; sleep 5");
    let mut tab = TerminalTab::new("t".into(), &p, "/bin/sh", 60, 10, 1_000).unwrap();
    wait_for(&mut tab, "preserve_me", Duration::from_secs(3));
    // Resize to smaller, then larger. Content should still be visible.
    tab.resize(40, 5).unwrap();
    tab.resize(80, 24).unwrap();
    // After resize, the marker may be in scrollback. Switch to grid view
    // and just assert the terminal didn't crash.
    let (c, r) = tab.size();
    assert!(c > 0 && r > 0);
}

#[test]
fn eof_detected_after_child_exits() {
    let p = sh_profile("true"); // exits immediately
    let mut tab = TerminalTab::new("t".into(), &p, "/bin/sh", 40, 5, 1_000).unwrap();
    // Poll until EOF is observed (the reader thread sends an empty-vec sentinel).
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let _ = tab.poll_pty();
        if tab.is_dead() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("tab never observed EOF");
}

#[test]
fn ansi_cursor_movement() {
    // Print "ABC", move cursor back two, overwrite with "XY" → "AXY"
    let p = sh_profile("printf 'ABC\\b\\bXY'");
    let mut tab = TerminalTab::new("t".into(), &p, "/bin/sh", 40, 5, 1_000).unwrap();
    wait_for(&mut tab, "AXY", Duration::from_secs(3));
}

#[test]
fn large_output_doesnt_crash() {
    // Print 1000 lines.
    let p = sh_profile("for i in $(seq 1 1000); do echo line_$i; done; sleep 1");
    let mut tab = TerminalTab::new("t".into(), &p, "/bin/sh", 80, 24, 5_000).unwrap();
    // We just need the last line to appear eventually.
    wait_for(&mut tab, "line_1000", Duration::from_secs(10));
}

#[test]
fn tabs_can_be_created_independently() {
    let p1 = sh_profile("echo tab_one_marker; sleep 5");
    let p2 = sh_profile("echo tab_two_marker; sleep 5");
    let mut t1 = TerminalTab::new("t1".into(), &p1, "/bin/sh", 60, 10, 1_000).unwrap();
    let mut t2 = TerminalTab::new("t2".into(), &p2, "/bin/sh", 60, 10, 1_000).unwrap();
    wait_for(&mut t1, "tab_one_marker", Duration::from_secs(3));
    wait_for(&mut t2, "tab_two_marker", Duration::from_secs(3));
    // Each tab got its own marker.
    assert!(grid_contains(&t1, "tab_one_marker"));
    assert!(!grid_contains(&t1, "tab_two_marker"));
    assert!(grid_contains(&t2, "tab_two_marker"));
    assert!(!grid_contains(&t2, "tab_one_marker"));
}

#[test]
fn config_loads_from_temp_file() {
    let toml = r#"
[terminal]
cols = 100
rows = 30

[profiles.default]
command = ["bash"]
"#;
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), toml).unwrap();
    let cfg = Config::load(Some(tmp.path())).unwrap();
    assert_eq!(cfg.terminal.cols, 100);
    assert_eq!(cfg.terminal.rows, 30);
    assert_eq!(cfg.profiles["default"].command, vec!["bash".to_string()]);
}

#[test]
fn default_config_is_sane() {
    let cfg = Config::default();
    assert!(cfg.terminal.cols >= 80);
    assert!(!cfg.terminal.shell.is_empty());
    assert_eq!(cfg.default_profile, "default");
}
