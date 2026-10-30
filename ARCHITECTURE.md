# Architecture

This document explains how rs-mrxvt fits together, why the modules are where
they are, and how to extend it (especially with the planned wgpu GUI backend).

## 1. Module map

```
src/
├── lib.rs            — public API surface; re-exports alacritty_terminal
├── main.rs           — binary entry point; CLI parse → App::run
├── cli.rs            — clap-derived CLI struct (mrxvt flag compatibility)
├── command.rs        — the Command enum (single source of truth for actions)
├── config.rs         — TOML config + ConfigSource trait (future: Lua)
├── app.rs            — App struct: owns TerminalManager + InputRouter + PaletteState
│                       + SessionState + the clipboard backend
├── clipboard.rs      — copy/paste: external tools + OSC 52 + MockClipboard
├── input/
│   ├── mod.rs
│   ├── bindings.rs   — KeyChord + KeyBindingTable (default mrxvt chords)
│   └── router.rs     — translate crossterm KeyEvents to bytes OR Commands
├── session.rs        — SessionState: mouse/selection/clicks/hyperlinks/images
├── mouse.rs          — mouse modes, SGR/X11 encoders, line-based Selection
├── terminal/
│   ├── mod.rs
│   ├── pty.rs        — PtySession: spawn child (login-shell aware) + read/write/resize
│   ├── tab.rs        — TerminalTab: PtySession + Term + reader thread + VT-event channel
│   └── manager.rs    — TerminalManager: tabs vec, active idx, broadcast router
└── ui/
    ├── mod.rs        — Renderer trait (the swap point for backends)
    ├── event.rs      — AppEvent / AppKey / AppModifiers (backend-agnostic input)
    ├── backend.rs    — Backend enum, BackendRegistry, auto-detect chain
    ├── mock.rs       — MockRenderer for tests
    ├── palette.rs    — PaletteState: fuzzy-search overlay
    ├── tui.rs        — TuiRenderer: ratatui + crossterm (default, always works)
    ├── wgpu.rs       — WgpuRenderer: GPU acceleration (behind `gpu` feature)
    └── soft.rs       — SoftRenderer: CPU rasterizer (behind `gpu` feature)
```

## 2. The backend fallback chain

This is the core of the distro-agnostic design. At startup, `Backend::Auto`
probes factories in priority order:

```
┌─────────────────────────────────────────────────────────────┐
│  1. WgpuFactory.available()?                                │
│     └─ probe: create wgpu::Instance, request_adapter        │
│     └─ needs: Vulkan or GL drivers + dev headers at build   │
│         YES → use WgpuRenderer (full GPU acceleration)      │
│         NO  ↓                                               │
│  2. SoftFactory.available()?                                │
│     └─ probe: $DISPLAY or $WAYLAND_DISPLAY set?             │
│     └─ needs: a display server (X11 or Wayland)             │
│         YES → use SoftRenderer (CPU raster, "VESA mode")    │
│         NO  ↓                                               │
│  3. TuiFactory.available()?                                 │
│     └─ always true                                          │
│     └─ use TuiRenderer (ratatui + crossterm)                │
└─────────────────────────────────────────────────────────────┘
```

### The "VESA mode" rationale

Classic VESA VBE was a CPU-driven linear framebuffer with no GPU
acceleration. It worked on any VGA card because the CPU did all the pixel
pushing. `softbuffer` + `tiny-skia` is the modern equivalent:

- **`softbuffer`** provides a CPU-writable pixel buffer that the compositor
  displays as a window surface. No GPU driver required.
- **`tiny-skia`** is a Skia port that rasterizes paths, text, and shapes
  on the CPU.
- **`ab_glyph`** shapes and rasterizes glyphs.

This stack runs on any machine with a display server — including retro
hardware with no working GPU driver, or VMs with broken 3D acceleration.
It's slower than wgpu but it always works, which is exactly the property
the fallback tier needs.

## 3. Event abstraction

Every backend translates its native events into [`AppEvent`] at the renderer
boundary. This lets `App::handle_event` stay backend-agnostic:

```
crossterm::KeyEvent ─┐
                     ├─→ AppEvent ─→ App::handle_event ─→ InputRouter / Palette
winit::KeyEvent ─────┘
```

`AppEvent` variants:
- `Key(AppKeyEvent)` — normalized key + modifiers + released flag
- `Resize(u16, u16)` — window/drawing area resized
- `FocusGained` / `FocusLost` — window focus changes
- `Paste(String)` — clipboard paste
- `Quit` — window close, Ctrl+C in TUI, etc.

The `AppKeyEvent` uses our own `AppKey` enum (not crossterm's or winit's)
so binding tables work across backends.

## 4. Data flow (one frame)

```
┌────────────────────────────────────────────────────────────┐
│                       Event Loop (app.rs)                  │
│                                                            │
│  ┌──────────────────┐    poll_all()    ┌────────────────┐  │
│  │ TerminalManager  │ ◄─────────────── │  poll_pty()    │  │
│  │   - tabs[i]      │                  │  on each tab   │  │
│  │   - active       │                  │  (non-blocking)│  │
│  │   - broadcast    │                  └────────────────┘  │
│  └────────┬─────────┘                                       │
│           │                                                │
│           │ route_input(bytes) per broadcast mode          │
│           ▼                                                │
│  ┌──────────────────┐    write_all()   ┌────────────────┐  │
│  │ InputRouter      │ ──────────────► │  PtySession    │  │
│  │  - bindings      │                  │  .master       │  │
│  │  - key_to_bytes  │                  └────────────────┘  │
│  └────────┬─────────┘                                       │
│           │                                                │
│           │ Command (from binding)                          │
│           ▼                                                │
│  ┌──────────────────┐    execute()      ┌────────────────┐  │
│  │ App::run_command │ ──────────────► │ TerminalManager │  │
│  │  (or palette)    │                  │  ::execute()    │  │
│  └──────────────────┘                  └────────────────┘  │
│                                                            │
│  renderer.render(self) ◄────────────────────────────────┘ │
└────────────────────────────────────────────────────────────┘
```

The loop:
1. `manager.poll_all()` — non-blocking drain of every tab's reader-thread channel.
2. `renderer.poll_event(timeout)` — wait up to 50ms for an `AppEvent`.
3. While events arrive: `app.handle_event(ev)` → either routes raw bytes
   via `manager.route_input` (honoring broadcast mode), or dispatches a
   `Command` via `manager.execute`.
4. `renderer.render(app)` — draws tab bar, active terminal grid, status bar,
   and (if open) the palette overlay.

## 5. Threading model

Each tab owns a **dedicated reader thread** (`tab.rs::TerminalTab::new`) that
does blocking `read()` on the PTY master FD and forwards bytes through an
`mpsc::channel`. The main loop drains the channel with `try_recv()`, so a
tab with no output costs ~0 CPU.

This is the fix for the original mrxvt's "heavy process in tab 1 lags tab 2"
problem: each PTY read is on its own thread, and the main loop never blocks
on a single tab's I/O.

## 6. The Renderer trait — the swap point

```rust
pub trait Renderer {
    fn init(&mut self) -> Result<()>;
    fn fini(&mut self) -> Result<()>;
    fn poll_event(&mut self, timeout_ms: u64) -> Result<Option<AppEvent>>;
    fn render(&mut self, app: &mut App) -> Result<()>;
    fn size(&self) -> (u16, u16);
}
```

`App::run` takes `Box<dyn Renderer>`, so the backend is chosen at runtime.
Three implementations exist today:

| Backend       | File          | Feature | Status      |
|---------------|---------------|---------|-------------|
| TuiRenderer   | `ui/tui.rs`   | default | ✅ working  |
| WgpuRenderer  | `ui/wgpu.rs`  | `gpu`   | ✅ working  |
| SoftRenderer  | `ui/soft.rs`  | `gpu`   | ✅ working  |
| MockRenderer  | `ui/mock.rs`  | default | ✅ for tests |

All three real backends render the full grid (text, colors, cursor,
selection highlight, tab bar, status bar) and accept mouse input
(selection, SGR reporting to the child, wheel scrollback). Remaining
backend gaps, in rough priority order: the command palette overlay is
only drawn by the TUI backend, inline images and hyperlink underlines
aren't composited by any backend yet, and bold/italic rasterization
falls back to the regular glyph (`GlyphCache::rasterize` ignores the
style flags).

## 7. Backend selection — `BackendRegistry`

The `BackendRegistry` holds a priority-ordered list of `BackendFactory`
trait objects. Each factory has:

```rust
pub trait BackendFactory: Send + 'static {
    fn available(&self) -> bool;
    fn create(&self) -> Result<Box<dyn Renderer>>;
}
```

`default_registry()` registers them in order: wgpu → soft → tui (when `gpu`
feature is on), or just tui (when it's off). The registry pattern makes it
easy to add new backends (e.g. a future `WaylandRenderer` that bypasses
winit) without touching the selection logic.

## 8. Broadcasting — the mrxvt killer feature

`BroadcastTarget` is the central type:

```rust
pub enum BroadcastTarget {
    Active,
    All,
    Group(String),
}
```

`TerminalManager::route_input(bytes)` matches on this enum and writes to
either the active tab, every tab, or only tabs whose `tag == Some(group)`.
The "fall back to active when no tab matches the group" rule preserves the
classic mrxvt UX: you never type into the void.

The CLI flag `-j` (alias `--broadcast`) flips `BroadcastTarget::Active` to
`BroadcastTarget::All` at startup. `-g <tag>` (alias `--tag`) assigns the
given tag to all startup tabs, so `rs-mrxvt -n 5 -j -g web` gives you 5
tabs all tagged "web" with broadcast on.

## 9. Command palette — modernizing hidden shortcuts

`Command` is the single source of truth for actions. Both the keybinding
table and the palette consume it:

- **Keybindings**: `KeyBindingTable::defaults()` maps chords to `Command`
  variants. `InputRouter::handle` resolves a `KeyEvent` to a `Command`
  (or routes raw bytes if no binding matches).
- **Palette**: `Command::defaults()` returns a list of `(Command, name,
  category)` triples for display. The palette uses `fuzzy-matcher`'s
  SkimMatcherV2 to filter.
- **Execution**: both paths funnel through `TerminalManager::execute(&cmd)`
  (for manager-affecting commands) or `App::run_command(&cmd)` (for
  palette/quit).

When the router returns `InputAction::OpenPalette`, the app layer opens the
palette — the router itself doesn't touch UI state, keeping it pure.

## 10. Config — TOML today, Lua tomorrow

`ConfigSource` is the trait that lets us swap config backends:

```rust
pub trait ConfigSource {
    fn load(&self) -> Result<Config>;
}
```

`FileConfigSource` is the TOML impl. A future `LuaConfigSource` (using
`mlua`) can implement the same trait and be selected at runtime via a
`--config-format lua` flag or by file extension. The rest of the codebase
won't need to change.

## 11. Clipboard, selection, and VT events

### Clipboard

`ClipboardBackend` is the swap point (`src/clipboard.rs`):

```rust
pub trait ClipboardBackend: Send {
    fn store(&mut self, kind: ClipboardKind, text: &str) -> Result<()>;
    fn load(&mut self, kind: ClipboardKind) -> Result<String>;
}
```

Production uses `SystemClipboard`: writes go to the first available
external tool (`xclip`/`xsel`/`wl-copy`/`pbcopy`/`termux-clipboard-set`,
auto-detected and cached) **and** are mirrored to the host terminal as an
OSC 52 escape sequence — which is what makes copy work over SSH. Reads
come from the external tool. Tests use `MockClipboard` (with an `as_any`
downcast hook so tests can inspect stores).

Paste flows through `sanitize_paste` (strip C0/C1 injection, normalize
CRLF, drop one trailing newline) and is wrapped in bracketed-paste
markers when the child enabled DECSET 2004.

### Selection

`Selection` (`src/mouse.rs`) stores grid coordinates (`SelPoint`), so a
selection survives scrolling. It is **line-based** like every mainstream
terminal: `line_span(line, cols)` gives the half-open column range for a
row; interior rows are fully selected. `kind` distinguishes
Simple/Word/Line gestures (single/double/triple click via
`SessionState.clicks`), and `TerminalTab::selection_text` extracts the
text (trailing whitespace trimmed per line, `\n`-joined).

### VT events

`TerminalTab`'s `EventForwarder` (the `Term`'s `EventListener`) pushes
alacritty events into a channel during `poll_pty`. Each tick,
`TerminalManager::drain_events` collects them and `App::handle_tab_event`
acts:

| Event | Behavior |
|-------|----------|
| `Title(s)` / `ResetTitle` | rename the tab / restore default title |
| `ClipboardStore(ty, s)` | OSC 52 write → clipboard backend (size-capped) |
| `ClipboardLoad(ty, fmt)` | OSC 52 read → gated on `clipboard.osc52_read` |
| `PtyWrite(s)` | write to that tab's PTY |
| `Bell` | status-bar notice |

### Login shells and the child environment

The child environment is inherited in full (portable-pty re-applies the
parent env). Default-shell tabs run as **login shells**
(`CommandBuilder::new_default_prog`, argv0 `-bash`, `profile.login_shell
= false` to opt out), because `/etc/profile` is where distros export the
GUI session environment — without it, some GUI apps launched from tabs
can't find their display. When neither `DISPLAY` nor `WAYLAND_DISPLAY`
reaches the child, `PtySession::spawn` logs a one-time warning.

## 12. Testing strategy

- **Unit tests** (`cargo test --lib`): 280+ tests covering input routing,
  config parsing, tab manager state transitions, palette filtering,
  keybinding resolution, key-to-bytes translation, event abstraction,
  backend selection logic, app event handling, clipboard (base64/OSC 52,
  paste sanitation, mock round-trips), selection (line spans, word/line
  kinds, text extraction over real PTYs), scrollback, mouse-mode
  bridging (including SGR motion encoding), OSC 52/title event
  forwarding, and login-shell spawning.
- **Integration tests** (`tests/integration.rs`): 10 end-to-end tests
  spawning real subprocesses via PTY and verifying the alacritty_terminal
  emulator renders the output.
- **Broadcasting integration tests** (`tests/broadcasting.rs`): 7 tests
  specifically for the mrxvt killer feature.
- **Stress harness** (`scripts/stress_test.py`): Python script that
  spawns N PTYs (default 50), broadcasts a marker, and verifies all N
  received it within the timeout.

Total: 300+ Rust tests + 1 Python harness, all passing. The same suite
passes with `--features gpu`, `--features lua`, `--features images`, and
combinations thereof.

## 13. Build configurations

| Config                | Cargo command                      | Backends available      |
|-----------------------|------------------------------------|-------------------------|
| Default (TUI only)    | `cargo build`                      | TUI                     |
| With GPU backends     | `cargo build --features gpu`       | TUI + Wgpu + Soft       |

The `gpu` feature pulls in `wgpu`, `winit`, `softbuffer`, `tiny-skia`,
`ab_glyph`, and `pollster`. These need system dev headers
(`libvulkan-dev`, `libwayland-dev`, `libxkbcommon-dev` on Debian) at build
time, but the default build has zero system graphics dependencies.
