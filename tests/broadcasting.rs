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


//! Multi-tab broadcasting integration tests.
//!
//! Verifies the classic mrxvt "killer feature": keystrokes typed in one tab
//! are mirrored to other tabs (or only tagged-group tabs) when broadcasting
//! is active.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use mrxvt::alacritty_terminal::grid::Dimensions;
use mrxvt::config::{Config, Profile};
use mrxvt::terminal::manager::{BroadcastTarget, TerminalManager};

fn cat_profile(tag: Option<String>) -> Profile {
    Profile {
        command: vec!["cat".into()],
        cwd: None,
        tag,
        env: HashMap::new(),
    }
}

fn wait_for_marker(
    manager: &mut TerminalManager,
    tab_idx: usize,
    needle: &str,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let _ = manager.poll_all();
        if let Some(tab) = manager.tabs.get(tab_idx) {
            let grid = tab.term.grid();
            let cols = grid.columns();
            let lines = grid.screen_lines();
            let display_offset = grid.display_offset();
            let start = -(display_offset as i32);
            for row in 0..lines as i32 {
                let mut s = String::with_capacity(cols);
                for col in 0..cols {
                    let point = mrxvt::re_export_point(start + row, col);
                    s.push(grid[point].c);
                }
                if s.contains(needle) {
                    return true;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn broadcast_all_sends_to_every_tab() {
    let cfg = Config::default();
    let mut mgr = TerminalManager::new(&cfg);

    // Three `cat` tabs.
    let p = cat_profile(None);
    let _ = mgr.open_tab(&p, Some("t1".into()), 60, 10);
    let _ = mgr.open_tab(&p, Some("t2".into()), 60, 10);
    let _ = mgr.open_tab(&p, Some("t3".into()), 60, 10);
    assert_eq!(mgr.tabs.len(), 3);

    // Give cat time to start.
    std::thread::sleep(Duration::from_millis(150));

    // Enable broadcast-all and send a line.
    mgr.broadcast = BroadcastTarget::All;
    mgr.route_input(b"broadcast_all_marker\n").unwrap();

    // Every tab should receive the marker (cat echoes it back).
    for i in 0..3 {
        assert!(
            wait_for_marker(&mut mgr, i, "broadcast_all_marker", Duration::from_secs(3)),
            "tab {i} did not receive broadcast_all_marker"
        );
    }
}

#[test]
fn broadcast_active_sends_only_to_focused() {
    let cfg = Config::default();
    let mut mgr = TerminalManager::new(&cfg);

    let p = cat_profile(None);
    let _ = mgr.open_tab(&p, Some("t1".into()), 60, 10);
    let _ = mgr.open_tab(&p, Some("t2".into()), 60, 10);

    std::thread::sleep(Duration::from_millis(150));

    // Active = tab 0 (default).
    assert_eq!(mgr.active, 0);
    mgr.route_input(b"only_active_marker\n").unwrap();

    assert!(
        wait_for_marker(&mut mgr, 0, "only_active_marker", Duration::from_secs(3)),
        "active tab should receive marker"
    );
    // Tab 1 should NOT have the marker.
    // (Poll tab 1 a few times to give it a chance to receive if it was going to.)
    std::thread::sleep(Duration::from_millis(300));
    let _ = mgr.poll_all();
    let has_marker = {
        let tab = &mgr.tabs[1];
        let grid = tab.term.grid();
        let cols = grid.columns();
        let lines = grid.screen_lines();
        let start = -(grid.display_offset() as i32);
        let mut found = false;
        for row in 0..lines as i32 {
            let mut s = String::with_capacity(cols);
            for col in 0..cols {
                let point = mrxvt::re_export_point(start + row, col);
                s.push(grid[point].c);
            }
            if s.contains("only_active_marker") {
                found = true;
                break;
            }
        }
        found
    };
    assert!(!has_marker, "inactive tab should not receive active-mode input");
}

#[test]
fn broadcast_group_sends_to_tagged_tabs_only() {
    let cfg = Config::default();
    let mut mgr = TerminalManager::new(&cfg);

    // 3 tabs: two tagged "web", one untagged.
    let p_web = cat_profile(Some("web".into()));
    let p_other = cat_profile(None);
    let _ = mgr.open_tab(&p_web, Some("web1".into()), 60, 10);
    let _ = mgr.open_tab(&p_web, Some("web2".into()), 60, 10);
    let _ = mgr.open_tab(&p_other, Some("db1".into()), 60, 10);

    std::thread::sleep(Duration::from_millis(150));

    // Broadcast to "web" tag.
    mgr.broadcast = BroadcastTarget::Group("web".into());
    mgr.route_input(b"group_web_marker\n").unwrap();

    assert!(
        wait_for_marker(&mut mgr, 0, "group_web_marker", Duration::from_secs(3)),
        "web1 should receive marker"
    );
    assert!(
        wait_for_marker(&mut mgr, 1, "group_web_marker", Duration::from_secs(3)),
        "web2 should receive marker"
    );

    // The untagged db1 tab should NOT have the marker.
    std::thread::sleep(Duration::from_millis(300));
    let _ = mgr.poll_all();
    let has_marker = {
        let tab = &mgr.tabs[2];
        let grid = tab.term.grid();
        let cols = grid.columns();
        let lines = grid.screen_lines();
        let start = -(grid.display_offset() as i32);
        let mut found = false;
        for row in 0..lines as i32 {
            let mut s = String::with_capacity(cols);
            for col in 0..cols {
                let point = mrxvt::re_export_point(start + row, col);
                s.push(grid[point].c);
            }
            if s.contains("group_web_marker") {
                found = true;
                break;
            }
        }
        found
    };
    assert!(!has_marker, "untagged tab should not receive group broadcast");
}

#[test]
fn broadcast_group_falls_back_to_active_when_no_match() {
    let cfg = Config::default();
    let mut mgr = TerminalManager::new(&cfg);

    let p = cat_profile(None);
    let _ = mgr.open_tab(&p, Some("t1".into()), 60, 10);

    std::thread::sleep(Duration::from_millis(150));

    // No tab has tag "ghost" — should fall back to active (tab 0).
    mgr.broadcast = BroadcastTarget::Group("ghost".into());
    mgr.route_input(b"fallback_marker\n").unwrap();

    assert!(
        wait_for_marker(&mut mgr, 0, "fallback_marker", Duration::from_secs(3)),
        "active tab should receive marker when group has no matches"
    );
}

#[test]
fn toggle_broadcast_all_round_trips() {
    let cfg = Config::default();
    let mut mgr = TerminalManager::new(&cfg);
    assert_eq!(mgr.broadcast, BroadcastTarget::Active);
    mgr.toggle_broadcast_all();
    assert_eq!(mgr.broadcast, BroadcastTarget::All);
    mgr.toggle_broadcast_all();
    assert_eq!(mgr.broadcast, BroadcastTarget::Active);
}

#[test]
fn toggle_broadcast_group_cycles() {
    let cfg = Config::default();
    let mut mgr = TerminalManager::new(&cfg);
    mgr.toggle_broadcast_group("web".into());
    assert_eq!(mgr.broadcast, BroadcastTarget::Group("web".into()));
    // Toggling same tag returns to Active.
    mgr.toggle_broadcast_group("web".into());
    assert_eq!(mgr.broadcast, BroadcastTarget::Active);
    // Different tag switches.
    mgr.toggle_broadcast_group("db".into());
    assert_eq!(mgr.broadcast, BroadcastTarget::Group("db".into()));
}

#[test]
fn tag_active_overrides_existing_tag() {
    let cfg = Config::default();
    let mut mgr = TerminalManager::new(&cfg);
    let p = cat_profile(Some("old".into()));
    let _ = mgr.open_tab(&p, Some("t1".into()), 60, 10);
    assert_eq!(mgr.active_tab().unwrap().tag.as_deref(), Some("old"));
    mgr.tag_active("new".into());
    assert_eq!(mgr.active_tab().unwrap().tag.as_deref(), Some("new"));
}
