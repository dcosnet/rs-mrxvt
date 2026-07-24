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


//! Per-tab fading state.
//!
//! Implements the classic mrxvt feature where inactive tabs are dimmed to
//! keep focus on the active tab. Each tab has a `target_fading` (0.0 = active,
//! 1.0 = fully faded) and a `current_fading` that lerps toward the target
//! each frame, producing a smooth ~150ms transition instead of a snap.
//!
//! ## Usage
//!
//! The renderers (wgpu, softbuffer) read this state per-tab and apply it as
//! a brightness multiplier on the cell colors before drawing.
//!
//! ## Lerp math
//!
//! ```text
//! current_fading += (target_fading - current_fading) * lerp_speed * dt
//! ```
//!
//! With `lerp_speed = 10.0` and `dt = 1/60`, the fade completes in ~150ms.

use std::collections::HashMap;

/// Per-tab fade state.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TabFadeState {
    /// 0.0 = fully visible (active), 1.0 = fully faded (inactive).
    pub current: f32,
    /// Where current is heading.
    pub target: f32,
}

impl TabFadeState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark this tab as the active one (target = 0.0).
    pub fn activate(&mut self) {
        self.target = 0.0;
    }

    /// Mark this tab as inactive (target = 1.0).
    pub fn deactivate(&mut self) {
        self.target = 1.0;
    }

    /// Advance the fade by `dt` seconds.
    pub fn update(&mut self, dt: f32, lerp_speed: f32) {
        let delta = (self.target - self.current) * lerp_speed * dt;
        self.current += delta;
        // Clamp to target to avoid overshoot.
        if (self.current - self.target).abs() < 0.001 {
            self.current = self.target;
        }
    }

    /// Returns true once the fade has settled on its target.
    pub fn settled(&self) -> bool {
        (self.current - self.target).abs() < 0.001
    }

    /// Brightness multiplier: 1.0 = full brightness, 0.5 = half (fully faded).
    pub fn brightness(&self, fade_amount: f32) -> f32 {
        1.0 - self.current * fade_amount
    }
}

/// Tracks fade state for all tabs, keyed by tab ID.
#[derive(Debug, Clone, Default)]
pub struct FadeState {
    /// Tab ID → fade state.
    pub tabs: HashMap<u32, TabFadeState>,
    /// Lerp speed (higher = snappier).
    pub lerp_speed: f32,
    /// How much to dim inactive tabs (0.0 = no dim, 1.0 = full dim).
    pub fade_amount: f32,
}

impl FadeState {
    pub fn new() -> Self {
        Self {
            tabs: HashMap::new(),
            lerp_speed: 10.0,
            fade_amount: 0.5,
        }
    }

    /// Configure the lerp speed and fade amount.
    pub fn with_params(mut self, lerp_speed: f32, fade_amount: f32) -> Self {
        self.lerp_speed = lerp_speed;
        self.fade_amount = fade_amount;
        self
    }

    /// Register a new tab.
    pub fn add_tab(&mut self, id: u32) {
        self.tabs.insert(id, TabFadeState::new());
    }

    /// Remove a tab.
    pub fn remove_tab(&mut self, id: u32) {
        self.tabs.remove(&id);
    }

    /// Mark a tab as active; all others become inactive.
    pub fn set_active(&mut self, active_id: u32, all_ids: &[u32]) {
        for id in all_ids {
            let state = self.tabs.entry(*id).or_default();
            if *id == active_id {
                state.activate();
            } else {
                state.deactivate();
            }
        }
    }

    /// Advance all fades by `dt` seconds.
    pub fn update(&mut self, dt: f32) {
        let lerp = self.lerp_speed;
        for state in self.tabs.values_mut() {
            state.update(dt, lerp);
        }
    }

    /// Get the brightness multiplier for a tab.
    pub fn brightness_for(&self, id: u32) -> f32 {
        self.tabs
            .get(&id)
            .map(|s| s.brightness(self.fade_amount))
            .unwrap_or(1.0)
    }

    /// True if all tabs have settled.
    pub fn all_settled(&self) -> bool {
        self.tabs.values().all(|s| s.settled())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_tab_starts_unfadted() {
        let s = TabFadeState::new();
        assert_eq!(s.current, 0.0);
        assert_eq!(s.target, 0.0);
        assert!(s.settled());
    }

    #[test]
    fn activate_sets_target_zero() {
        let mut s = TabFadeState::new();
        s.deactivate();
        s.activate();
        assert_eq!(s.target, 0.0);
    }

    #[test]
    fn deactivate_sets_target_one() {
        let mut s = TabFadeState::new();
        s.deactivate();
        assert_eq!(s.target, 1.0);
    }

    #[test]
    fn lerp_moves_toward_target() {
        let mut s = TabFadeState::new();
        s.deactivate(); // target = 1.0
        s.update(0.016, 10.0); // 60fps, lerp_speed=10
        assert!(s.current > 0.0, "current should have moved up");
        assert!(s.current < 1.0, "current should not have reached target yet");
    }

    #[test]
    fn lerp_settles_eventually() {
        let mut s = TabFadeState::new();
        s.deactivate();
        // Simulate ~1 second of frames.
        for _ in 0..120 {
            s.update(0.016, 10.0);
            if s.settled() {
                break;
            }
        }
        assert!(s.settled());
        assert!((s.current - 1.0).abs() < 0.01);
    }

    #[test]
    fn brightness_at_full_when_active() {
        let s = TabFadeState::new();
        assert!((s.brightness(0.5) - 1.0).abs() < 0.001);
    }

    #[test]
    fn brightness_dimmed_when_inactive() {
        let mut s = TabFadeState::new();
        s.current = 1.0;
        assert!((s.brightness(0.5) - 0.5).abs() < 0.001);
    }

    #[test]
    fn fade_state_manages_multiple_tabs() {
        let mut fs = FadeState::new();
        fs.add_tab(1);
        fs.add_tab(2);
        fs.add_tab(3);
        fs.set_active(2, &[1, 2, 3]);

        assert_eq!(fs.tabs[&1].target, 1.0); // inactive
        assert_eq!(fs.tabs[&2].target, 0.0); // active
        assert_eq!(fs.tabs[&3].target, 1.0); // inactive
    }

    #[test]
    fn fade_state_removes_tabs() {
        let mut fs = FadeState::new();
        fs.add_tab(1);
        fs.add_tab(2);
        fs.remove_tab(1);
        assert!(!fs.tabs.contains_key(&1));
        assert!(fs.tabs.contains_key(&2));
    }

    #[test]
    fn fade_state_update_advances_all() {
        let mut fs = FadeState::new();
        fs.add_tab(1);
        fs.add_tab(2);
        fs.set_active(1, &[1, 2]);
        fs.update(0.016);
        // Tab 2 should have moved toward 1.0.
        assert!(fs.tabs[&2].current > 0.0);
    }

    #[test]
    fn brightness_for_unknown_tab_is_one() {
        let fs = FadeState::new();
        assert_eq!(fs.brightness_for(999), 1.0);
    }

    #[test]
    fn all_settled_after_enough_updates() {
        let mut fs = FadeState::new();
        fs.add_tab(1);
        fs.add_tab(2);
        fs.set_active(1, &[1, 2]);
        // Initially not settled (target changed).
        assert!(!fs.all_settled());
        // Update until settled.
        for _ in 0..120 {
            fs.update(0.016);
            if fs.all_settled() {
                break;
            }
        }
        assert!(fs.all_settled());
    }

    #[test]
    fn with_params_sets_speed_and_amount() {
        let fs = FadeState::new().with_params(20.0, 0.7);
        assert!((fs.lerp_speed - 20.0).abs() < 0.001);
        assert!((fs.fade_amount - 0.7).abs() < 0.001);
    }
}
