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

use std::time::{Duration, Instant};

use alacritty_terminal::event::Event as TermEvent;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::cli::Cli;
use crate::clipboard::{bracket, sanitize_paste, ClipboardBackend, ClipboardKind, SystemClipboard};
use crate::command::Command;
use crate::config::Config;
use crate::config_watch::ConfigWatcher;
use crate::input::router::{InputAction, InputRouter};
use crate::input::bindings::KeyBindingTable;
use crate::mouse::{MouseButton, MouseEvent, MouseEventKind, SelPoint};
use crate::session::SessionState;
use crate::terminal::manager::{Action, BroadcastTarget, TerminalManager};
use crate::ui::event::{AppEvent, AppKey, AppKeyEvent, AppMouseEvent};
#[cfg(test)]
use crate::ui::event::AppModifiers;
use crate::ui::palette::PaletteState;

/// How long a status-bar notice stays visible.
pub const NOTICE_TTL: Duration = Duration::from_millis(2500);

/// A transient status-bar message (e.g. "Copied 42 chars").
#[derive(Debug, Clone)]
pub struct Notice {
    pub text: String,
    pub expires: Instant,
}

/// The app. Construct with [`App::new`], then run with a [`Renderer`].
pub struct App {
    pub manager: TerminalManager,
    pub router: InputRouter,
    pub palette: PaletteState,
    pub session: SessionState,
    pub config: Config,
    pub cli: Cli,
    pub quit: bool,
    /// Clipboard backend (system tools + OSC 52 in production, mock in tests).
    pub clipboard: Box<dyn ClipboardBackend>,
    /// Transient status-bar message, if any.
    pub notice: Option<Notice>,
    /// Last (cols, rows) the tabs were resized to, so per-tick size checks
    /// only resizes on an actual change. `None` until the first frame.
    last_grid_size: Option<(u16, u16)>,
    /// Last observed alternate-screen state of the active tab. A transition
    /// (e.g. vim/less entering or leaving the alt screen) invalidates the
    /// selection, which lives in the primary grid's coordinates.
    watched_alt_screen: Option<bool>,
    /// Config hot-reload watcher, when one was started. Polled every tick.
    config_watcher: Option<ConfigWatcher>,
}

impl App {
    pub fn new(cli: Cli, config: Config) -> anyhow::Result<Self> {
        let clipboard: Box<dyn ClipboardBackend> =
            Box::new(SystemClipboard::from_config(&config.clipboard));
        Self::with_clipboard(cli, config, clipboard)
    }

    /// Constructor with an explicit clipboard backend (tests use a mock).
    pub fn with_clipboard(
        cli: Cli,
        config: Config,
        clipboard: Box<dyn ClipboardBackend>,
    ) -> anyhow::Result<Self> {
        let mut bindings = KeyBindingTable::defaults();
        bindings.apply_macros(&config.macros);
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
            clipboard,
            notice: None,
            last_grid_size: None,
            watched_alt_screen: None,
            config_watcher: None,
        })
    }

    /// Show a transient message in the status bar.
    pub fn notify(&mut self, text: impl Into<String>) {
        self.notice = Some(Notice {
            text: text.into(),
            expires: Instant::now() + NOTICE_TTL,
        });
    }

    /// The active notice, if it hasn't expired.
    pub fn active_notice(&self) -> Option<&str> {
        self.notice
            .as_ref()
            .filter(|n| Instant::now() < n.expires)
            .map(|n| n.text.as_str())
    }

    // ─── clipboard ───────────────────────────────────────────────────────────

    /// Copy the active selection to the system clipboard.
    pub fn copy_selection(&mut self) {
        let Some(tab) = self.manager.active_tab() else {
            return;
        };
        if !self.session.selection.is_active() {
            self.notify("No selection");
            return;
        }
        let Some(text) = tab.selection_text(&self.session.selection) else {
            self.notify("No selection");
            return;
        };
        if text.is_empty() {
            self.notify("Selection is empty");
            return;
        }
        let n = text.chars().count();
        match self.clipboard.store(ClipboardKind::Clipboard, &text) {
            Ok(()) => self.notify(format!("Copied {n} chars")),
            Err(e) => self.notify(format!("Copy failed: {e}")),
        }
    }

    /// Load the given clipboard and paste it into the terminal(s).
    pub fn paste(&mut self, kind: ClipboardKind) {
        match self.clipboard.load(kind) {
            Ok(text) => self.deliver_paste(&text),
            Err(e) => self.notify(format!("Paste failed: {e}")),
        }
    }

    /// Sanitize, optionally bracket, and route already-loaded paste text.
    ///
    /// Shared by the clipboard-tool path, OSC 52 paste requests, and host
    /// terminal paste events.
    pub fn deliver_paste(&mut self, text: &str) {
        let clean = sanitize_paste(text);
        if clean.is_empty() {
            self.notify("Clipboard is empty");
            return;
        }
        let n = clean.chars().count();
        let bracketed = self
            .manager
            .active_tab()
            .map(|t| t.bracketed_paste())
            .unwrap_or(false);
        let payload = if bracketed { bracket(&clean) } else { clean };
        if let Err(e) = self.manager.route_input(payload.as_bytes()) {
            log::warn!("paste routing failed: {e}");
        }
        self.notify(format!("Pasted {n} chars"));
    }

    // ─── commands ────────────────────────────────────────────────────────────

    /// Apply a one-shot [`Command`] (from the palette, a macro, etc.).
    pub fn run_command(&mut self, cmd: &Command, cols: u16, rows: u16) -> InputAction {
        match cmd {
            Command::OpenPalette => {
                if self.config.ui.disable_palette {
                    self.notify("Command palette is disabled (ui.disable_palette)");
                } else {
                    self.palette.toggle();
                }
                InputAction::Handled
            }
            Command::Quit => {
                self.quit = true;
                InputAction::Quit
            }
            Command::CopySelection => {
                self.copy_selection();
                InputAction::Handled
            }
            Command::PasteClipboard => {
                self.paste(ClipboardKind::Clipboard);
                InputAction::Handled
            }
            Command::PastePrimary => {
                self.paste(ClipboardKind::Primary);
                InputAction::Handled
            }
            other => {
                let active_before = self.manager.active;
                let action = self.manager.execute(other, cols, rows, &self.config);
                // Switching tabs invalidates the selection (it lives in the
                // previous tab's grid coordinates).
                if self.manager.active != active_before {
                    self.session.reset();
                }
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
                // The main loop compares `Renderer::size()` every tick and
                // calls `manager.resize_all` on an actual change, so the PTY
                // winsize and grid stay in sync with the window. Nothing to
                // do here beyond acknowledging the event.
                InputAction::Continue
            }
            AppEvent::FocusGained | AppEvent::FocusLost => InputAction::Continue,
            AppEvent::Paste(s) => {
                // Paste delivered by the host terminal (bracketed paste).
                self.deliver_paste(&s);
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
    /// Priority order (matching xterm/Ghostty behavior):
    /// 1. If the child enabled mouse reporting and Shift is not held,
    ///    encode the event (SGR-1006 when negotiated, else legacy X11) and
    ///    forward it to the PTY. Shift bypasses reporting so the user can
    ///    always select text.
    /// 2. Otherwise the event drives local interaction: click-drag selects
    ///    (with double/triple-click word/line modes), the wheel scrolls
    ///    the scrollback viewport (or sends arrows on the alternate
    ///    screen), middle-click pastes the primary selection, and
    ///    right-click pastes the clipboard.
    pub fn handle_mouse(&mut self, ev: AppMouseEvent) -> InputAction {
        let mouse_ev: MouseEvent = ev.into();
        let mode = self
            .manager
            .active_tab()
            .map(|t| t.mouse_mode())
            .unwrap_or_default();

        // 1. Mouse reporting for the child program.
        if mode.any_reporting() && !mouse_ev.mods.shift {
            let bytes = if mode.sgr {
                crate::mouse::encode_sgr(mouse_ev, mode)
            } else {
                crate::mouse::encode_x11(mouse_ev, mode)
            };
            if let Some(bytes) = bytes {
                if let Err(e) = self.manager.route_input(&bytes) {
                    log::warn!("input routing failed: {e}");
                }
                return InputAction::Handled;
            }
        }

        // 2. Local interaction.
        let now = Instant::now();
        match (mouse_ev.kind, mouse_ev.button) {
            (MouseEventKind::Press, MouseButton::WheelUp) => {
                if let Some(t) = self.manager.active_tab_mut() {
                    t.wheel_scroll(true);
                }
            }
            (MouseEventKind::Press, MouseButton::WheelDown) => {
                if let Some(t) = self.manager.active_tab_mut() {
                    t.wheel_scroll(false);
                }
            }
            (MouseEventKind::Press, MouseButton::Left) => {
                self.begin_selection(mouse_ev, now);
            }
            (MouseEventKind::Motion, _) if self.session.selection.is_active() => {
                self.extend_selection(mouse_ev);
            }
            (MouseEventKind::Release, MouseButton::Left) => {
                self.finish_selection();
            }
            (MouseEventKind::Press, MouseButton::Middle) => {
                self.paste(ClipboardKind::Primary);
            }
            (MouseEventKind::Press, MouseButton::Right) => {
                self.paste(ClipboardKind::Clipboard);
            }
            _ => {}
        }
        InputAction::Handled
    }

    /// Left-press: start a Simple/Word/Line selection based on click cadence.
    fn begin_selection(&mut self, ev: MouseEvent, now: Instant) {
        let Some(tab) = self.manager.active_tab() else { return };
        let p = tab.grid_point(ev.row, ev.col);
        let kind = self.session.clicks.register(now);
        match kind {
            crate::mouse::SelectionKind::Word => {
                let (a, b) = tab.expand_word(p);
                self.session.selection.set_span(kind, a, b);
            }
            crate::mouse::SelectionKind::Line => {
                let (a, b) = tab.expand_line(p);
                self.session.selection.set_span(kind, a, b);
            }
            crate::mouse::SelectionKind::Simple => {
                self.session.selection.begin(kind, p);
            }
        }
    }

    /// Drag: extend the selection, re-expanding word/line selections.
    fn extend_selection(&mut self, ev: MouseEvent) {
        let Some(tab) = self.manager.active_tab() else { return };
        let p = tab.grid_point(ev.row, ev.col);
        let kind = self.session.selection.kind;
        match kind {
            crate::mouse::SelectionKind::Simple => {
                self.session.selection.extend(p);
            }
            crate::mouse::SelectionKind::Word => {
                // Union of the anchor word and the word under the pointer.
                let origin = self
                    .session
                    .selection
                    .anchor
                    .or(Some(p))
                    .unwrap_or(p);
                let (aa, ab) = tab.expand_word(origin);
                let (ba, bb) = tab.expand_word(p);
                let lo = if (ba.line, ba.col) < (aa.line, aa.col) { ba } else { aa };
                let hi = if (bb.line, bb.col) > (ab.line, ab.col) { bb } else { ab };
                self.session.selection.set_span(kind, lo, hi);
            }
            crate::mouse::SelectionKind::Line => {
                let origin = self.session.selection.anchor.unwrap_or(p);
                let lo_line = origin.line.min(p.line);
                let hi_line = origin.line.max(p.line);
                let cols = tab.size().0 as usize;
                self.session.selection.set_span(
                    kind,
                    SelPoint { line: lo_line, col: 0 },
                    SelPoint { line: hi_line, col: cols.saturating_sub(1) },
                );
            }
        }
    }

    /// Left-release: copy-on-select (if enabled), collapse empty selections.
    fn finish_selection(&mut self) {
        let sel = &self.session.selection;
        if sel.is_active() && (sel.is_multi_cell() || sel.kind != crate::mouse::SelectionKind::Simple)
        {
            if self.config.clipboard.copy_on_select {
                if let Some(tab) = self.manager.active_tab() {
                    if let Some(text) = tab.selection_text(sel) {
                        if !text.is_empty() {
                            let n = text.chars().count();
                            // Copy-on-select targets the primary selection
                            // (X11 convention) and the clipboard (so it also
                            // works on Wayland/macOS setups).
                            if let Err(e) =
                                self.clipboard.store(ClipboardKind::Primary, &text)
                            {
                                log::debug!("primary store failed: {e}");
                            }
                            if let Err(e) =
                                self.clipboard.store(ClipboardKind::Clipboard, &text)
                            {
                                log::debug!("clipboard store failed: {e}");
                            }
                            self.notify(format!("Copied {n} chars"));
                        }
                    }
                }
            }
        } else {
            self.session.selection.clear();
        }
    }

    // ─── VT events from the child (title, OSC 52, bell, …) ───────────────────

    /// Process VT events drained from the tabs after a PTY poll.
    pub fn handle_tab_events(&mut self, events: Vec<(usize, TermEvent)>) {
        for (idx, ev) in events {
            self.handle_tab_event(idx, ev);
        }
    }

    fn handle_tab_event(&mut self, idx: usize, ev: TermEvent) {
        match ev {
            TermEvent::Title(title) => {
                self.manager.set_title(idx, title);
            }
            TermEvent::ResetTitle => {
                self.manager.reset_title(idx);
            }
            TermEvent::ClipboardStore(ty, text) => {
                // A program (tmux, nvim, …) set the clipboard via OSC 52.
                // Allow list-sized writes; refuse absurd ones.
                if text.len() > crate::clipboard::OSC52_MAX_BYTES {
                    log::warn!(
                        "ignoring OSC 52 store from tab {idx}: {} bytes exceeds cap",
                        text.len()
                    );
                    return;
                }
                let kind = ClipboardKind::from(ty);
                if let Err(e) = self.clipboard.store(kind, &text) {
                    log::debug!("OSC 52 store failed: {e}");
                }
            }
            TermEvent::ClipboardLoad(ty, fmt) => {
                // OSC 52 clipboard *query*. Off by default (security: don't
                // let arbitrary programs read the clipboard silently).
                if !self.config.clipboard.osc52_read {
                    return;
                }
                let kind = ClipboardKind::from(ty);
                if let Ok(text) = self.clipboard.load(kind) {
                    let seq = fmt(&text);
                    if let Some(tab) = self.manager.tabs.get_mut(idx) {
                        if let Err(e) = tab.write_input(seq.as_bytes()) {
                            log::debug!("OSC 52 paste write failed: {e}");
                        }
                    }
                }
            }
            TermEvent::PtyWrite(s) => {
                if let Some(tab) = self.manager.tabs.get_mut(idx) {
                    if let Err(e) = tab.write_input(s.as_bytes()) {
                        log::debug!("PtyWrite failed: {e}");
                    }
                }
            }
            TermEvent::Bell => {
                self.notify("Bell");
            }
            // Wakeup, MouseCursorDirty, … — the 20 Hz render loop already
            // covers redraws; nothing to do.
            _ => {}
        }
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
        self.absorb_router_action(action, cols, rows)
    }

    /// Handle a crossterm `KeyEvent` directly. Kept for backward compat
    /// with existing tests and the legacy `handle_key` API.
    pub fn handle_key(&mut self, ev: KeyEvent, cols: u16, rows: u16) -> InputAction {
        // If the palette is open, intercept most keys.
        if self.palette.is_open() {
            return self.handle_palette_key(ev, cols, rows);
        }

        let action = self.router.handle(ev, &mut self.manager, &self.config, cols, rows);
        self.absorb_router_action(action, cols, rows)
    }

    /// Act on the router's recommendation: open the palette or run a command
    /// the router can't execute itself (clipboard commands need the App's
    /// clipboard backend and session selection).
    fn absorb_router_action(&mut self, action: InputAction, cols: u16, rows: u16) -> InputAction {
        match action {
            InputAction::OpenPalette => {
                if self.config.ui.disable_palette {
                    self.notify("Command palette is disabled (ui.disable_palette)");
                    InputAction::Handled
                } else {
                    self.palette.open();
                    InputAction::OpenPalette
                }
            }
            InputAction::Command(cmd) => self.run_command(&cmd, cols, rows),
            other => other,
        }
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

    /// Start polling the config file for changes (hot reload). The watcher
    /// re-loads it on save and delivers updates into the main loop every
    /// tick. Call before `run`; a failed spawn just disables hot reload.
    pub fn start_config_watcher(&mut self, interval_ms: u64) -> anyhow::Result<()> {
        let path = self
            .cli
            .config
            .clone()
            .unwrap_or_else(Config::default_path);
        let watcher = ConfigWatcher::spawn(path, interval_ms)?;
        // Swallow the watcher's initial delivery: the app already loaded the
        // same file at startup (possibly through CLI overrides the watcher
        // can't reproduce), so re-applying it would be a no-op at best.
        while watcher.try_recv().is_some() {}
        self.config_watcher = Some(watcher);
        Ok(())
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
        self.last_grid_size = Some((cols, rows));

        let result = self.main_loop(&mut *renderer);

        if let Some(mut watcher) = self.config_watcher.take() {
            watcher.stop();
        }
        renderer.fini()?;
        result.map(|_| 0)
    }

    fn main_loop(&mut self, renderer: &mut dyn crate::ui::Renderer) -> anyhow::Result<()> {
        let poll_ms = 50; // 20 Hz input poll; PTY drain is per-tick.
        while !self.quit {
            // 0. Pick up config changes from disk (hot reload), then apply
            //    any window resize. Resizing before draining PTYs matters:
            //    the render pass indexes the grid with the renderer's size,
            //    so a stale grid would panic the moment the window grows.
            self.apply_reloaded_config();
            let (cols, rows) = renderer.size();
            if self.last_grid_size != Some((cols, rows)) {
                self.manager.resize_all(cols, rows);
                self.last_grid_size = Some((cols, rows));
                // A resize reflows the grid; selection coordinates recorded
                // against the old layout no longer match the content.
                self.session.selection.clear();
            }
            self.invalidate_selection_on_alt_screen_change();

            // 1. Drain PTYs, then act on VT events (titles, OSC 52, bells).
            let _poll = self.manager.poll_all();
            let events = self.manager.drain_events();
            if !events.is_empty() {
                self.handle_tab_events(events);
            }

            // 2. Poll for input.
            while let Some(ev) = renderer.poll_event(poll_ms)? {
                let (cols, rows) = renderer.size();
                let action = self.handle_event(ev, cols, rows);
                if matches!(action, InputAction::Quit) {
                    self.quit = true;
                    break;
                }
            }

            // 3. Close tabs whose child process has exited. If the last tab
            // closed, quit the app — classic terminal behaviour:
            //   `exit` in the last tab → app quits
            //   `exit` in any other tab → only that tab closes
            if self.manager.close_dead_tabs() {
                self.quit = true;
            }

            // 4. Render.
            renderer.render(self)?;
        }
        Ok(())
    }

    /// Apply config delivered by the hot-reload watcher, if any.
    fn apply_reloaded_config(&mut self) {
        let Some(watcher) = self.config_watcher.as_ref() else {
            return;
        };
        let mut newest: Option<Config> = None;
        while let Some(cfg) = watcher.try_recv() {
            newest = Some(cfg);
        }
        if let Some(cfg) = newest {
            log::info!("hot-reload: applying updated config");
            self.config = cfg;
        }
    }

    /// Clear the selection when the active tab swaps screens. Selection
    /// coordinates are grid lines on the *primary* screen; after vim/less
    /// enters or leaves the alternate screen they would highlight (and copy)
    /// unrelated cells.
    fn invalidate_selection_on_alt_screen_change(&mut self) {
        let alt = self
            .manager
            .active_tab()
            .map(|t| t.alternate_screen())
            .unwrap_or(false);
        match self.watched_alt_screen {
            Some(prev) if prev != alt => self.session.selection.clear(),
            _ => {}
        }
        self.watched_alt_screen = Some(alt);
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
    use alacritty_terminal::grid::Dimensions;
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

    // ─── clipboard + selection + scrolling tests ──────────────────────────

    use crate::clipboard::MockClipboard;
    use crate::ui::event::AppMouseEvent;

    fn mock_app() -> App {
        let cli = default_cli();
        let cfg = Config::default();
        App::with_clipboard(cli, cfg, Box::new(MockClipboard::new())).unwrap()
    }

    /// Replace the app's tabs with one child running `cmd`, then wait for
    /// `marker` (printed after `cmd`) to appear in its grid. Only suitable
    /// for one-shot commands; interactive children (`cat`) need a manual
    /// profile that prints the marker first.
    fn run_child(app: &mut App, cmd: &str, marker: &str) {
        let profile = crate::config::Profile {
            command: vec!["sh".into(), "-c".into(), format!("{cmd}; printf '{marker}'")],
            ..crate::config::Profile::default()
        };
        while !app.manager.tabs.is_empty() {
            app.manager.close_tab(0);
        }
        app.manager.open_tab(&profile, Some("test".into()), 40, 10).unwrap();
        wait_grid(app, marker);
    }

    /// Like [`run_child`], but the marker is printed *before* the (long
    /// running) command starts — for interactive children like `cat`.
    fn run_interactive_child(app: &mut App, cmd: &str, marker: &str) {
        let profile = crate::config::Profile {
            command: vec!["sh".into(), "-c".into(), format!("printf '{marker}'; {cmd}")],
            ..crate::config::Profile::default()
        };
        while !app.manager.tabs.is_empty() {
            app.manager.close_tab(0);
        }
        app.manager.open_tab(&profile, Some("test".into()), 40, 10).unwrap();
        wait_grid(app, marker);
    }

    /// Wait until the active tab's grid contains `needle`.
    ///
    /// VT events parsed along the way stay queued in the tab — tests pick
    /// them up with `manager.drain_events()` afterwards.
    fn wait_grid(app: &mut App, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            let _ = app.manager.poll_all();
            let tab = app.manager.active_tab().unwrap();
            let grid = tab.term.grid();
            let start = -(grid.display_offset() as i32);
            let found = (0..grid.screen_lines()).any(|r| {
                let s: String = (0..grid.columns())
                    .map(|c| {
                        grid[crate::Point {
                            line: crate::Line(start + r as i32),
                            column: crate::Column(c),
                        }].c
                    })
                    .collect();
                s.contains(needle)
            });
            if found {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("timeout waiting for {needle:?} in grid");
    }

    fn mouse_ev(
        button: crate::mouse::MouseButton,
        col: u32,
        row: u32,
        kind: MouseEventKind,
    ) -> AppMouseEvent {
        AppMouseEvent {
            button,
            col,
            row,
            mods: crate::mouse::MouseMods::default(),
            kind,
        }
    }

    #[test]
    fn drag_select_then_release_copies_on_select() {
        let mut app = mock_app();
        run_child(&mut app, "printf 'alpha beta gamma'", "MARK1");
        // Drag from col 6 to col 9 on row 0 → "beta".
        app.handle_event(AppEvent::Mouse(mouse_ev(
            crate::mouse::MouseButton::Left,
            6,
            0,
            MouseEventKind::Press,
        )), 40, 10);
        app.handle_event(AppEvent::Mouse(mouse_ev(
            crate::mouse::MouseButton::Left,
            9,
            0,
            MouseEventKind::Motion,
        )), 40, 10);
        app.handle_event(AppEvent::Mouse(mouse_ev(
            crate::mouse::MouseButton::Left,
            9,
            0,
            MouseEventKind::Release,
        )), 40, 10);

        let any = app.clipboard.as_any().unwrap();
        let mock = any.downcast_ref::<MockClipboard>().unwrap();
        let stored = mock
            .stores
            .iter()
            .find(|(k, _)| matches!(k, crate::clipboard::ClipboardKind::Clipboard))
            .map(|(_, t)| t.clone())
            .expect("copy-on-select should store to clipboard");
        assert_eq!(stored, "beta");
    }

    #[test]
    fn single_click_does_not_copy() {
        let mut app = mock_app();
        run_child(&mut app, "printf 'alpha beta gamma'", "MARK2");
        // Click and release on the same cell — no multi-cell selection.
        app.handle_event(AppEvent::Mouse(mouse_ev(
            crate::mouse::MouseButton::Left,
            3,
            0,
            MouseEventKind::Press,
        )), 40, 10);
        app.handle_event(AppEvent::Mouse(mouse_ev(
            crate::mouse::MouseButton::Left,
            3,
            0,
            MouseEventKind::Release,
        )), 40, 10);
        let any = app.clipboard.as_any().unwrap();
        let mock = any.downcast_ref::<MockClipboard>().unwrap();
        assert!(mock.stores.is_empty(), "single click must not copy");
    }

    #[test]
    fn right_click_pastes_clipboard() {
        let mut app = mock_app();
        run_interactive_child(&mut app, "cat", "MARK3");
        app.clipboard = Box::new(MockClipboard::with_content(
            crate::clipboard::ClipboardKind::Clipboard,
            "pasted-by-right-click",
        ));
        app.handle_event(
            AppEvent::Mouse(mouse_ev(
                crate::mouse::MouseButton::Right,
                1,
                1,
                MouseEventKind::Press,
            )),
            40,
            10,
        );
        // `cat` echoes the paste back into the grid.
        wait_grid(&mut app, "pasted-by-right");
        assert!(app.active_notice().is_some());
    }

    #[test]
    fn paste_command_reaches_bracketed_child() {
        let mut app = mock_app();
        run_interactive_child(&mut app, "printf '\\033[?2004h'; cat", "MARK4");
        assert!(
            app.manager.active_tab().unwrap().bracketed_paste(),
            "child should have bracketed paste on"
        );
        app.clipboard = Box::new(MockClipboard::with_content(
            crate::clipboard::ClipboardKind::Clipboard,
            "bracketed-text",
        ));
        app.run_command(&Command::PasteClipboard, 40, 10);
        wait_grid(&mut app, "bracketed-text");
        assert!(app.active_notice().unwrap().contains("Pasted"));
    }

    #[test]
    fn wheel_scrolls_active_tab_and_typing_snaps_back() {
        let mut app = mock_app();
        run_child(&mut app, "seq 1 200", "MARK5");
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            let _ = app.manager.poll_all();
            if app.manager.active_tab().unwrap().term.grid().history_size() > 100 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        app.handle_event(
            AppEvent::Mouse(mouse_ev(
                crate::mouse::MouseButton::WheelUp,
                0,
                0,
                MouseEventKind::Press,
            )),
            40,
            10,
        );
        assert_eq!(app.manager.active_tab().unwrap().display_offset(), 3);
        // Typing snaps back to the bottom.
        app.handle_event(
            AppEvent::Key(AppKeyEvent::new(AppModifiers::empty(), AppKey::Enter)),
            40,
            10,
        );
        assert!(app.manager.active_tab().unwrap().at_bottom());
    }

    #[test]
    fn mouse_reporting_forwards_to_child_and_shift_bypasses() {
        let mut app = mock_app();
        run_child(&mut app, "printf '\\033[?1000h\\033[?1006h'", "MARK6");

        // Non-shift press is encoded and forwarded (not treated as selection).
        app.handle_event(
            AppEvent::Mouse(mouse_ev(
                crate::mouse::MouseButton::Left,
                5,
                2,
                MouseEventKind::Press,
            )),
            40,
            10,
        );
        assert!(
            !app.session.selection.is_active(),
            "press should go to child, not selection"
        );

        // Shift+click bypasses reporting → local selection begins.
        let mut ev = mouse_ev(crate::mouse::MouseButton::Left, 1, 1, MouseEventKind::Press);
        ev.mods.shift = true;
        app.handle_event(AppEvent::Mouse(ev), 40, 10);
        assert!(app.session.selection.is_active());
    }

    #[test]
    fn osc52_store_from_child_sets_clipboard() {
        let mut app = mock_app();
        // "prox" base64-encodes to cHJveA==.
        run_child(&mut app, "printf '\\033]52;c;cHJveA==\\007'", "MARK7");
        let events = app.manager.drain_events();
        assert!(!events.is_empty());
        app.handle_tab_events(events);
        let stored = app
            .clipboard
            .load(crate::clipboard::ClipboardKind::Clipboard)
            .unwrap();
        assert_eq!(stored, "prox");
    }

    #[test]
    fn title_from_child_renames_tab() {
        let mut app = mock_app();
        run_child(&mut app, "printf '\\033]0;renamed-tab\\007'", "MARK8");
        let events = app.manager.drain_events();
        app.handle_tab_events(events);
        assert_eq!(app.manager.active_tab().unwrap().title, "renamed-tab");
    }

    #[test]
    fn tab_switch_clears_selection() {
        let mut app = mock_app();
        app.run_command(&Command::NewTab, 40, 10);
        assert_eq!(app.manager.tabs.len(), 2);
        app.session.selection.begin(
            crate::mouse::SelectionKind::Simple,
            crate::mouse::SelPoint { line: 0, col: 0 },
        );
        app.run_command(&Command::PrevTab, 40, 10);
        assert!(
            !app.session.selection.is_active(),
            "selection must reset on tab switch"
        );
    }

    #[test]
    fn copy_without_selection_notifies() {
        let mut app = mock_app();
        app.run_command(&Command::CopySelection, 40, 10);
        assert_eq!(app.active_notice(), Some("No selection"));
    }

    #[test]
    fn copy_chord_reaches_app_layer() {
        // Regression for the dispatch gap: Ctrl+Shift+C used to be executed
        // via manager.execute (which ignores clipboard commands) and vanish.
        // It must come back as InputAction::Command and run through the app's
        // clipboard path.
        let mut app = mock_app();
        let ev = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('C'),
            crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::SHIFT,
        );
        let action = app.handle_key(ev, 40, 10);
        assert_eq!(action, InputAction::Handled, "copy ran through run_command");
        assert_eq!(app.active_notice(), Some("No selection"));
    }

    #[test]
    fn paste_chord_reaches_app_layer_and_pastes() {
        let mut app = mock_app();
        run_interactive_child(&mut app, "cat", "MARKPASTE");
        app.clipboard = Box::new(MockClipboard::with_content(
            crate::clipboard::ClipboardKind::Clipboard,
            "via-chord",
        ));
        let ev = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('V'),
            crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::SHIFT,
        );
        let action = app.handle_key(ev, 40, 10);
        assert_eq!(action, InputAction::Handled);
        wait_grid(&mut app, "via-chord");
        assert!(app.active_notice().unwrap().contains("Pasted"));
    }

    #[test]
    fn disable_palette_config_blocks_opening() {
        let cli = default_cli();
        let mut cfg = Config::default();
        cfg.ui.disable_palette = true;
        let mut app = App::with_clipboard(cli, cfg, Box::new(MockClipboard::new())).unwrap();
        app.run_command(&Command::OpenPalette, 40, 10);
        assert!(!app.palette.is_open(), "ui.disable_palette must keep the palette closed");
        assert!(app.active_notice().unwrap().contains("disabled"));
    }

    #[test]
    fn config_macros_resolve_at_startup() {
        let cli = Cli::try_parse_from(["rs-mrxvt"]).unwrap();
        let mut cfg = Config::default();
        cfg.macros.insert("Ctrl+Shift+Y".into(), "ResetTerminal".into());
        let app = App::with_clipboard(cli, cfg, Box::new(MockClipboard::new())).unwrap();
        let ev = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('y'),
            crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::SHIFT,
        );
        // Must be a bound command, not passthrough bytes.
        let resolved = app.router.bindings.resolve(ev);
        assert_eq!(resolved, Some(Command::ResetTerminal));
    }

    #[test]
    fn resize_event_updates_tab_size() {
        // The main loop calls resize_all when the renderer's size changes;
        // manager-side, resize must reach both the PTY and the grid.
        let mut app = mock_app();
        let before = app.manager.active_tab().unwrap().size();
        app.manager.resize_all(before.0 + 10, before.1 + 2);
        let after = app.manager.active_tab().unwrap().size();
        assert_eq!(after, (before.0 + 10, before.1 + 2));
    }

    #[test]
    fn alt_screen_transition_clears_selection() {
        let mut app = mock_app();
        // The child prints the alt-screen switch from its own output (we
        // can't write it to stdin: the tty line discipline echoes ESC as a
        // literal "^[" pair in canonical mode, so the parser never sees it).
        let profile = crate::config::Profile {
            command: vec![
                "sh".into(),
                "-c".into(),
                "printf 'MARKALT'; sleep 0.3; printf '\\033[?1049h'; sleep 5".into(),
            ],
            ..crate::config::Profile::default()
        };
        while !app.manager.tabs.is_empty() {
            app.manager.close_tab(0);
        }
        app.manager.open_tab(&profile, Some("test".into()), 40, 10).unwrap();
        wait_grid(&mut app, "MARKALT");

        app.session.selection.begin(
            crate::mouse::SelectionKind::Simple,
            SelPoint { line: 0, col: 0 },
        );
        // First observation records the (off) state without clearing.
        app.invalidate_selection_on_alt_screen_change();
        assert!(app.session.selection.is_active());

        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            let _ = app.manager.poll_all();
            if app.manager.active_tab().unwrap().alternate_screen() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            app.manager.active_tab().unwrap().alternate_screen(),
            "child should have entered the alt screen"
        );
        app.invalidate_selection_on_alt_screen_change();
        assert!(
            !app.session.selection.is_active(),
            "alt-screen transition must clear the selection"
        );
    }
}
