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


//! The application state and main loop.
//!
//! Owns:
//! - the [`TerminalManager`]
//! - the [`InputRouter`] with keybindings
//! - the [`PaletteState`] for the command palette
//! - the [`SessionState`] for mouse/selection/hyperlinks/images
//! - a reference to the loaded [`Config`]
//!
//! The actual render / event-poll is delegated to a [`Renderer`] impl, so the
//! same `App` can drive a TUI or a future wgpu GUI. Events arrive as
//! [`AppEvent`], a backend-agnostic enum — every renderer translates its
//! native events into this type at the boundary.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::cli::Cli;
use crate::command::Command;
use crate::config::Config;
use crate::input::router::{InputAction, InputRouter};
use crate::input::bindings::KeyBindingTable;
use crate::mouse::{MouseButton, MouseEvent, MouseEventKind};
use crate::session::SessionState;
use crate::terminal::manager::{Action, BroadcastTarget, TerminalManager};
use crate::ui::event::{AppEvent, AppKey, AppKeyEvent};
#[cfg(test)]
use crate::ui::event::AppModifiers;
use crate::ui::palette::PaletteState;

/// The app. Construct with [`App::new`], then run with a [`Renderer`].
pub struct App {
    pub manager: TerminalManager,
    pub router: InputRouter,
    pub palette: PaletteState,
    pub session: SessionState,
    pub config: Config,
    pub cli: Cli,
    pub quit: bool,
}

impl App {
    pub fn new(cli: Cli, config: Config) -> anyhow::Result<Self> {
        let bindings = KeyBindingTable::defaults();
        let router = InputRouter::new(bindings);

        let mut manager = TerminalManager::new(&config);

        // If the user passed `--broadcast`, enable broadcast-to-all from the start.
        if cli.broadcast {
            manager.broadcast = BroadcastTarget::All;
        }

        // Open the initial tabs.
        // Determine cols/rows — we don't know the actual terminal size yet,
        // so use the config defaults. The renderer will resize on first frame.
        let cols = config.terminal.cols;
        let rows = config.terminal.rows;

        let n_initial = cli.tabs.max(1);
        for i in 0..n_initial {
            let mut profile = if let Some(argv) = &cli.exec {
                crate::config::Profile {
                    command: argv.clone(),
                    ..crate::config::Profile::default()
                }
            } else {
                config.profile(&config.default_profile)
            };

            // Apply CLI overrides.
            if let Some(d) = &cli.dir {
                profile.cwd = Some(d.clone());
            }
            if let Some(t) = &cli.tag {
                profile.tag = Some(t.clone());
            }

            let title = if n_initial == 1 {
                Some(cli.title.clone())
            } else {
                Some(format!("{} #{}", cli.title, i + 1))
            };
            manager.open_tab(&profile, title, cols, rows)?;
        }

        Ok(Self {
            manager,
            router,
            palette: PaletteState::new(),
            session: SessionState::new(),
            config,
            cli,
            quit: false,
        })
    }

    /// Apply a one-shot [`Command`] (from the palette, a macro, etc.).
    pub fn run_command(&mut self, cmd: &Command, cols: u16, rows: u16) -> InputAction {
        match cmd {
            Command::OpenPalette => {
                self.palette.toggle();
                InputAction::Handled
            }
            Command::Quit => {
                self.quit = true;
                InputAction::Quit
            }
            other => {
                let action = self.manager.execute(other, cols, rows, &self.config);
                if action == Action::Quit {
                    self.quit = true;
                    InputAction::Quit
                } else {
                    InputAction::Handled
                }
            }
        }
    }

    /// Handle a backend-agnostic [`AppEvent`].
    ///
    /// This is the entry point used by the main loop; it dispatches to
    /// either `handle_key` (for key events) or specialized handlers
    /// (resize, paste, quit).
    pub fn handle_event(&mut self, ev: AppEvent, cols: u16, rows: u16) -> InputAction {
        match ev {
            AppEvent::Key(k) => self.handle_app_key(k, cols, rows),
            AppEvent::Resize(_c, _r) => {
                // The renderer reports its own size via `Renderer::size()`,
                // which is called by the main loop. Nothing to do here beyond
                // acknowledging the event.
                InputAction::Continue
            }
            AppEvent::FocusGained | AppEvent::FocusLost => InputAction::Continue,
            AppEvent::Paste(s) => {
                // Paste as raw bytes into the active tab (or broadcast group).
                let bytes = s.into_bytes();
                if let Err(e) = self.manager.route_input(&bytes) {
                    log::warn!("input routing failed: {e}");
                }
                InputAction::Continue
            }
            AppEvent::Mouse(m) => self.handle_mouse(m),
            AppEvent::Quit => {
                self.quit = true;
                InputAction::Quit
            }
        }
    }

    /// Handle a mouse event.
    ///
    /// If the active program has enabled mouse reporting (via DECSET), the
    /// event is encoded and sent to the PTY. Otherwise, the event drives
    /// local text selection: press begins a selection, motion extends it,
    /// release copies to clipboard (deferred — clipboard access requires
    /// platform-specific code).
    pub fn handle_mouse(&mut self, ev: crate::ui::event::AppMouseEvent) -> InputAction {
        let mouse_ev: MouseEvent = ev.into();
        let mode = self.session.mouse_mode;

        // If the program is reporting mouse events, encode and forward.
        if mode.any_reporting() {
            // Try SGR-1006 first (preferred), then legacy X11.
            if let Some(bytes) = crate::mouse::encode_sgr(mouse_ev, mode)
                .or_else(|| crate::mouse::encode_x11(mouse_ev, mode))
            {
                if let Err(e) = self.manager.route_input(&bytes) {
                    log::warn!("input routing failed: {e}");
                }
                return InputAction::Handled;
            }
        }

        // Otherwise, drive local selection.
        match mouse_ev.kind {
            MouseEventKind::Press => match mouse_ev.button {
                MouseButton::Left => {
                    self.session.selection.begin(mouse_ev.col, mouse_ev.row);
                }
                MouseButton::Right => {
                    // Right-click: paste (placeholder — would call into clipboard).
                    self.session.selection.clear();
                }
                _ => {}
            },
            MouseEventKind::Motion => {
                if self.session.selection.is_active() {
                    self.session.selection.extend(mouse_ev.col, mouse_ev.row);
                }
            }
            MouseEventKind::Release => {
                if self.session.selection.is_multi_cell() {
                    // Selection complete — would copy to clipboard here.
                    // For now we just leave the selection active so the
                    // renderer can highlight it; the user can clear it
                    // with a single click.
                } else {
                    self.session.selection.clear();
                }
            }
        }
        InputAction::Handled
    }

    /// Handle an [`AppKeyEvent`] (translated from crossterm or winit).
    pub fn handle_app_key(&mut self, ev: AppKeyEvent, cols: u16, rows: u16) -> InputAction {
        // Ignore key-release events (only key presses trigger commands).
        if ev.released {
            return InputAction::Continue;
        }

        // If the palette is open, intercept most keys.
        if self.palette.is_open() {
            return self.handle_palette_app_key(ev, cols, rows);
        }

        // Translate AppKeyEvent → crossterm KeyEvent for the existing router.
        // (The router's internals still use crossterm types; we bridge here.)
        let ck = app_key_to_crossterm(ev);
        let action = self.router.handle(ck, &mut self.manager, &self.config, cols, rows);
        // The router returns `OpenPalette` as a signal; act on it here by
        // actually opening the palette. (The router doesn't touch UI state.)
        if matches!(action, InputAction::OpenPalette) {
            self.palette.open();
        }
        action
    }

    /// Handle a crossterm `KeyEvent` directly. Kept for backward compat
    /// with existing tests and the legacy `handle_key` API.
    pub fn handle_key(&mut self, ev: KeyEvent, cols: u16, rows: u16) -> InputAction {
        // If the palette is open, intercept most keys.
        if self.palette.is_open() {
            return self.handle_palette_key(ev, cols, rows);
        }

        let action = self.router.handle(ev, &mut self.manager, &self.config, cols, rows);
        if matches!(action, InputAction::OpenPalette) {
            self.palette.open();
        }
        action
    }

    fn handle_palette_key(&mut self, ev: KeyEvent, cols: u16, rows: u16) -> InputAction {
        let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);

        match ev.code {
            KeyCode::Esc => {
                self.palette.close();
                InputAction::Handled
            }
            KeyCode::Up => {
                self.palette.move_up();
                InputAction::Handled
            }
            KeyCode::Down => {
                self.palette.move_down();
                InputAction::Handled
            }
            KeyCode::Enter => {
                if let Some(cmd) = self.palette.selected_command() {
                    self.palette.close();
                    self.run_command(&cmd, cols, rows)
                } else {
                    InputAction::Handled
                }
            }
            KeyCode::Backspace => {
                self.palette.backspace();
                InputAction::Handled
            }
            KeyCode::Char('c') if ctrl => {
                self.palette.close();
                InputAction::Handled
            }
            KeyCode::Char('u') if ctrl => {
                self.palette.query.clear();
                InputAction::Handled
            }
            KeyCode::Char(c) if !ctrl => {
                self.palette.push_char(c);
                InputAction::Handled
            }
            _ => InputAction::Handled,
        }
    }

    fn handle_palette_app_key(&mut self, ev: AppKeyEvent, cols: u16, rows: u16) -> InputAction {
        let ctrl = ev.mods.ctrl;
        let shift = ev.mods.shift;
        let alt = ev.mods.alt;

        match ev.key {
            AppKey::Esc => {
                self.palette.close();
                InputAction::Handled
            }
            AppKey::Up => {
                self.palette.move_up();
                InputAction::Handled
            }
            AppKey::Down => {
                self.palette.move_down();
                InputAction::Handled
            }
            AppKey::Enter => {
                if let Some(cmd) = self.palette.selected_command() {
                    self.palette.close();
                    self.run_command(&cmd, cols, rows)
                } else {
                    InputAction::Handled
                }
            }
            AppKey::Backspace => {
                self.palette.backspace();
                InputAction::Handled
            }
            AppKey::Char('c') if ctrl => {
                self.palette.close();
                InputAction::Handled
            }
            AppKey::Char('u') if ctrl => {
                self.palette.query.clear();
                InputAction::Handled
            }
            AppKey::Char(c) if !ctrl && !alt && !shift => {
                self.palette.push_char(c);
                InputAction::Handled
            }
            AppKey::Char(c) if shift && !ctrl && !alt => {
                // Shifted chars: push the shifted form.
                self.palette.push_char(c.to_ascii_uppercase());
                InputAction::Handled
            }
            _ => InputAction::Handled,
        }
    }

    /// Run the main loop with a given renderer. Returns process exit code.
    ///
    /// Accepts a `Box<dyn Renderer>` so the caller can pick the backend at
    /// runtime (TUI, wgpu, softbuffer) without monomorphizing the loop.
    pub fn run(&mut self, mut renderer: Box<dyn crate::ui::Renderer>) -> anyhow::Result<i32> {
        renderer.init()?;

        // Initial resize to actual terminal dimensions.
        let (cols, rows) = renderer.size();
        self.manager.resize_all(cols, rows);

        let result = self.main_loop(&mut *renderer);

        renderer.fini()?;
        result.map(|_| 0)
    }

    fn main_loop(&mut self, renderer: &mut dyn crate::ui::Renderer) -> anyhow::Result<()> {
        let poll_ms = 50; // 20 Hz input poll; PTY drain is per-tick.
        while !self.quit {
            // 1. Drain PTYs.
            let (_active_changed, any_eof) = self.manager.poll_all();

            // 2. Poll for input.
            while let Some(ev) = renderer.poll_event(poll_ms)? {
                let (cols, rows) = renderer.size();
                let action = self.handle_event(ev, cols, rows);
                if matches!(action, InputAction::Quit) {
                    self.quit = true;
                    break;
                }
            }

            // 3. Handle EOF on a tab — close it.
            if any_eof {
                // Heuristic: close active tab if its child has exited.
                // For the MVP we don't track which tab EOF'd; closing the
                // active one matches user expectation most of the time.
                // (A more robust approach tracks EOF per-tab.)
                // We avoid closing if there's still data being produced.
                // For now: don't auto-close; the user can press Ctrl+Shift+W.
            }

            // 4. Render.
            renderer.render(self)?;
        }
        Ok(())
    }
}

/// Translate an [`AppKeyEvent`] back into a `crossterm::event::KeyEvent`
/// so the existing `InputRouter` (which uses crossterm types internally)
/// can handle it. This is a temporary bridge — the router will eventually
/// be rewritten to consume `AppKeyEvent` natively.
fn app_key_to_crossterm(ev: AppKeyEvent) -> KeyEvent {
    let mut mods = KeyModifiers::empty();
    if ev.mods.shift { mods |= KeyModifiers::SHIFT; }
    if ev.mods.ctrl { mods |= KeyModifiers::CONTROL; }
    if ev.mods.alt { mods |= KeyModifiers::ALT; }
    if ev.mods.super_key { mods |= KeyModifiers::SUPER; }

    let code = match ev.key {
        AppKey::Char(c) => KeyCode::Char(c),
        AppKey::Enter => KeyCode::Enter,
        AppKey::Tab => KeyCode::Tab,
        AppKey::BackTab => KeyCode::BackTab,
        AppKey::Backspace => KeyCode::Backspace,
        AppKey::Esc => KeyCode::Esc,
        AppKey::Left => KeyCode::Left,
        AppKey::Right => KeyCode::Right,
        AppKey::Up => KeyCode::Up,
        AppKey::Down => KeyCode::Down,
        AppKey::Home => KeyCode::Home,
        AppKey::End => KeyCode::End,
        AppKey::PageUp => KeyCode::PageUp,
        AppKey::PageDown => KeyCode::PageDown,
        AppKey::Delete => KeyCode::Delete,
        AppKey::Insert => KeyCode::Insert,
        AppKey::F(n) => KeyCode::F(n),
        AppKey::Space => KeyCode::Char(' '),
    };

    KeyEvent::new(code, mods)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use clap::Parser;

    fn default_cli() -> Cli {
        Cli::try_parse_from(["rs-mrxvt"]).unwrap()
    }

    #[test]
    fn app_creates_initial_tab() {
        let cli = default_cli();
        let cfg = Config::default();
        let app = App::new(cli, cfg).unwrap();
        assert_eq!(app.manager.tabs.len(), 1);
        assert!(!app.quit);
    }

    #[test]
    fn app_creates_n_tabs_from_cli() {
        let cli = Cli::try_parse_from(["rs-mrxvt", "--tabs", "3"]).unwrap();
        let cfg = Config::default();
        let app = App::new(cli, cfg).unwrap();
        assert_eq!(app.manager.tabs.len(), 3);
    }

    #[test]
    fn app_broadcast_flag_propagates() {
        let cli = Cli::try_parse_from(["rs-mrxvt", "-j"]).unwrap();
        let cfg = Config::default();
        let app = App::new(cli, cfg).unwrap();
        assert_eq!(app.manager.broadcast, BroadcastTarget::All);
    }

    #[test]
    fn app_run_command_palette_toggles() {
        let cli = default_cli();
        let cfg = Config::default();
        let mut app = App::new(cli, cfg).unwrap();
        assert!(!app.palette.is_open());
        app.run_command(&Command::OpenPalette, 40, 10);
        assert!(app.palette.is_open());
        app.run_command(&Command::OpenPalette, 40, 10);
        assert!(!app.palette.is_open());
    }

    #[test]
    fn app_quit_command_sets_quit_flag() {
        let cli = default_cli();
        let cfg = Config::default();
        let mut app = App::new(cli, cfg).unwrap();
        app.run_command(&Command::Quit, 40, 10);
        assert!(app.quit);
    }

    #[test]
    fn app_exec_flag_sets_command() {
        let cli = Cli::try_parse_from(["rs-mrxvt", "-e", "echo", "hi"]).unwrap();
        let cfg = Config::default();
        let app = App::new(cli, cfg).unwrap();
        assert_eq!(app.manager.tabs.len(), 1);
    }

    #[test]
    fn app_event_quit_terminates_loop() {
        let cli = default_cli();
        let cfg = Config::default();
        let mut app = App::new(cli, cfg).unwrap();
        let action = app.handle_event(AppEvent::Quit, 40, 10);
        assert_eq!(action, InputAction::Quit);
        assert!(app.quit);
    }

    #[test]
    fn app_event_paste_routes_bytes() {
        let cli = default_cli();
        let cfg = Config::default();
        let mut app = App::new(cli, cfg).unwrap();
        // Paste "hello" — should not panic and should be Continue.
        let action = app.handle_event(AppEvent::Paste("hello".into()), 40, 10);
        assert_eq!(action, InputAction::Continue);
    }

    #[test]
    fn app_event_key_release_is_ignored() {
        let cli = default_cli();
        let cfg = Config::default();
        let mut app = App::new(cli, cfg).unwrap();
        let ev = AppKeyEvent {
            mods: AppModifiers::empty(),
            key: AppKey::Char('a'),
            released: true,
        };
        let action = app.handle_event(AppEvent::Key(ev), 40, 10);
        assert_eq!(action, InputAction::Continue);
    }

    #[test]
    fn app_event_resize_is_ack_only() {
        let cli = default_cli();
        let cfg = Config::default();
        let mut app = App::new(cli, cfg).unwrap();
        let action = app.handle_event(AppEvent::Resize(120, 40), 40, 10);
        assert_eq!(action, InputAction::Continue);
    }

    #[test]
    fn app_event_focus_changes_are_no_ops() {
        let cli = default_cli();
        let cfg = Config::default();
        let mut app = App::new(cli, cfg).unwrap();
        assert_eq!(app.handle_event(AppEvent::FocusGained, 40, 10), InputAction::Continue);
        assert_eq!(app.handle_event(AppEvent::FocusLost, 40, 10), InputAction::Continue);
    }

    #[test]
    fn app_event_key_opens_palette_via_chord() {
        let cli = default_cli();
        let cfg = Config::default();
        let mut app = App::new(cli, cfg).unwrap();
        // Ctrl+Shift+P → OpenPalette
        let ev = AppKeyEvent {
            mods: AppModifiers { ctrl: true, shift: true, ..Default::default() },
            key: AppKey::Char('P'),
            released: false,
        };
        let action = app.handle_event(AppEvent::Key(ev), 40, 10);
        assert_eq!(action, InputAction::OpenPalette);
        assert!(app.palette.is_open());
    }
}
