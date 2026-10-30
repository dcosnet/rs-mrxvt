# rs-mrxvt — The Modernized Power-User Terminal

`rs-mrxvt` is a modernized, distro-agnostic terminal emulator inspired by the
classic [mrxvt](https://wiki.archlinux.org/title/Mrxvt). It is written in Rust
and pairs 2008-era "tabbed power" with 2020s reliability.

The MVP ships with **four rendering backends** and an auto-detect chain that
falls back gracefully:

1. **wgpu (Vulkan)** — full GPU acceleration on modern hardware
2. **wgpu (GL)** — older GPUs that lack Vulkan drivers
3. **softbuffer + tiny-skia** — CPU rasterizer, the modern VESA mode
4. **TUI (ratatui + crossterm)** — always available, even over SSH

The default build uses the TUI backend (zero system graphics deps). Build
with `--features gpu` to enable the wgpu and softbuffer backends.

Three opt-in feature flags extend the terminal in orthogonal directions:
- `--features lua` — dynamic, programmable configuration via `config.lua`
- `--features images` — Sixel + iTerm2 inline image protocol support
- `--features gpu` — wgpu + softbuffer rendering backends

---

## ✨ Features (implemented)

### Core (always available)
- **Multi-tab PTY** — each tab runs an independent shell via `portable-pty`,
  with VT emulation by `alacritty_terminal`. Tabs are managed by a single
  `TerminalManager`; one tab's heavy output never blocks the others.
- **Copy & paste** — click-drag select (line-based, like xterm/Ghostty),
  double-click word, triple-click line. `Ctrl+Shift+C`/`Ctrl+Shift+V`,
  classic `Ctrl+Ins`/`Shift+Ins`, middle-click pastes the primary
  selection, right-click pastes the clipboard, and copy-on-select follows
  the classic mrxvt/xterm behavior. Copies are also mirrored to the host
  terminal via **OSC 52**, so copy works over SSH.
- **Scrollback navigation** — mouse wheel, `Shift+PageUp/PageDown`,
  `Shift+Home/End`, and palette commands. The wheel does the right thing
  on the alternate screen (sends arrows to `less`/`vim`); typing snaps the
  viewport back to the live bottom.
- **Mouse reporting** — DECSET 9/1000/1002/1003/1006 tracked from the
  child program; SGR-1006 and legacy X11 encodings both supported.
  `Shift+click` always bypasses reporting so you can select text in
  `vim`/`htop`.
- **OSC 52 in and out** — programs (tmux, nvim) can set the clipboard;
  reads are off by default for security (`clipboard.osc52_read = true`
  to enable).
- **Dynamic tab titles** — OSC 0/2 title sequences from the child rename
  the tab (e.g. `vim` shows the filename, `ssh` shows the host).
- **Login shells by default** — default-shell tabs run as login shells
  (classic mrxvt `loginShell: True`), so `/etc/profile` and its distro
  GUI-session exports (D-Bus, flatpak/snap paths) reach every app you
  launch — including GUI apps.
- **Input Broadcasting** — the classic mrxvt killer feature. Toggle
  broadcast-to-all with `Ctrl+Shift+I`, or broadcast only to tagged groups
  via `--tag` on the CLI or the `ToggleBroadcastGroup` command in the
  palette.
- **Command Palette** — `Ctrl+Shift+P` opens a fuzzy-search overlay over
  every command. Inspired by Warp / VS Code.
- **Backend auto-detect** — `--backend auto` (default) probes wgpu → soft →
  TUI. Override with `--backend {tui,wgpu,soft}` if needed.
- **Classic mrxvt CLI flags** — `-e CMD`, `-t TITLE`, `-n N`, `-j`
  (broadcast), `-g TAG` (group tag), `-c PATH` (config), `-d DIR` (cwd),
  `-b {auto,tui,wgpu,soft}` (backend).
- **TOML config** — `~/.config/rs-mrxvt/config.toml` with profiles, macros,
  keybindings, and transparency settings.
- **Stress-tested** — 280+ unit + integration tests, plus a 50-instance
  Python stress harness that broadcasts a marker to 50 shells in ~30ms.

### With `--features lua`
- **Dynamic Lua config** — `config.lua` with full logic: conditionals, env
  vars, time-of-day themes, programmable macros. Auto-detected by file
  extension; force with `--config-format lua`.
- **Backward-compatible** — TOML config still works; pick per file.

### With `--features gpu`
- **wgpu renderer** — Vulkan/GL accelerated. Instanced quad pipeline with
  a glyph atlas texture and WGSL shaders. Renders the full terminal grid
  (text + colors + cursor + selection highlight + tab bar + status bar),
  with mouse input (selection, SGR reporting, wheel) like the TUI backend.
- **softbuffer renderer** — CPU rasterizer via tiny-skia + ab_glyph. The
  modern VESA mode: works on any display server, no GPU driver required.
  Same grid/cursor/selection/mouse feature set as wgpu.
- **Themed rendering** — both GUI renderers resolve `ui.theme` /
  `[custom_themes]` per frame, matching the TUI colors.
- **Shared glyph cache** — `ab_glyph` rasterizes glyphs on demand; both
  backends use the same cache.
- **Pseudo-transparency + per-tab fading** (planned) — `[transparency]`
  config and the fade model already parse; the renderer-side shader/
  compositing wiring is the next step.

### With `--features images`
- **iTerm2 inline image decoder** — `ESC ] 1337 ; File = ...` payloads
  decode to RGBA (PNG/JPEG/GIF/WebP/BMP), normalized through
  `DynamicImage::to_rgba8` so every native layout decodes correctly.
- **Sixel parser** — pure-Rust state machine with color registers and
  repeat runs, tested against stride/regression cases.
- **Status** — parsers and the thread-safe image store are in place and
  tested; the PTY hooks that feed them and the renderer compositing that
  draws them are the remaining work.

## 🗺️ Roadmap

| Feature                | Status      | Notes                                          |
|------------------------|-------------|------------------------------------------------|
| Multi-tab PTY          | ✅ shipped  | 280+ tests covering routing, broadcasting, EOF |
| Input broadcasting     | ✅ shipped  | Active / All / Group(tag)                      |
| Command palette        | ✅ shipped  | fuzzy-matcher, Ctrl+Shift+P                    |
| Backend auto-detect    | ✅ shipped  | wgpu → soft → tui chain                        |
| TUI renderer           | ✅ shipped  | ratatui + crossterm, distro-agnostic           |
| wgpu renderer          | ✅ shipped  | instanced pipeline, glyph atlas, WGSL shaders  |
| softbuffer renderer    | ✅ shipped  | tiny-skia + ab_glyph, the VESA mode            |
| GUI backend input      | ✅ shipped  | Mouse (selection/SGR reporting/wheel), cursor + selection highlight, themes in wgpu+soft |
| Pseudo-transparency    | 🚧 planned  | Config parsed + modeled; GPU shader wiring next |
| Per-tab fading         | 🚧 planned  | fade.rs modeled; renderer wiring next          |
| Lua config             | ✅ shipped  | mlua, dynamic themes, programmable macros      |
| Config hot-reload      | ✅ shipped  | Polling watcher wired into the main loop       |
| iTerm2 image protocol  | 🚧 parser   | Decoder ready (tested); PTY hook + compositing next |
| Sixel image protocol   | 🚧 parser   | Pure-Rust parser (tested); PTY hook + drawing next |
| Mouse + SGR-1006       | ✅ shipped  | X10/X11/SGR encoders, motion bit, shift-bypass |
| OSC 8 hyperlinks       | 🚧 parser   | Parser + store; underline rendering next       |
| Alt+N / Alt+Arrows     | ✅ shipped  | New tab, shuffle forward/back                  |
| Alt+Shift+X close      | ✅ shipped  | Closes focused tab                             |
| Alt+Z zsh tab          | ✅ shipped  | Secondary shell on a hotkey                    |
| True-color themes      | ✅ shipped  | Tokyo Night, Gruvbox, Dracula, Solarized — applied in all three renderers |
| Custom themes          | ✅ shipped  | `[custom_themes.<name>]` tables in config.toml |
| Config macros          | ✅ shipped  | `[macros]` chord → command bindings            |
| Multi-distro sysprep   | ✅ shipped  | pacman/apt/dnf/zypper/xbps/apk/cast/emerge     |
| Clipboard              | ✅ shipped  | Select/copy/paste, OSC 52 mirror, tools+mock   |
| Scrollback navigation  | ✅ shipped  | Wheel, Shift+PgUp/PgDn/Home/End, palette cmds  |
| Selection modes        | ✅ shipped  | Drag/word/line, copy-on-select, highlight      |
| Tab title sync         | ✅ shipped  | OSC 0/2 from child renames the tab             |
| Login shells           | ✅ shipped  | GUI-session env reaches apps (mrxvt default)   |

## 🚀 Quick start

### Build from source (TUI only — default)

```bash
# Dependencies: Rust 1.75+ (rustup recommended), and a POSIX shell.
# No system graphics libs required for the TUI backend.

cargo build --release
./target/release/rs-mrxvt
```

### Build with everything

```bash
# Install system dev headers first:
#   Debian/Ubuntu: sudo apt install libvulkan-dev libwayland-dev libxkbcommon-dev
#   Arch:          sudo pacman -S vulkan-headers wayland-protocols libxkbcommon
#   Fedora:        sudo dnf install vulkan-headers wayland-devel libxkbcommon-devel
#   SourceMage:    cast vulkan-loader wayland-protocols libxkbcommon

cargo build --release --features gpu,lua,images
./target/release/rs-mrxvt                    # auto-detects best backend
./target/release/rs-mrxvt --backend wgpu     # force wgpu
./target/release/rs-mrxvt --backend soft     # force CPU rasterizer (VESA mode)
./target/release/rs-mrxvt --backend tui      # force TUI
./target/release/rs-mrxvt --config-format lua  # force Lua config
```

### Distro-agnostic install

```bash
sudo make install              # installs to /usr/local by default
sudo PREFIX=/usr make install  # installs to /usr
```

### Try it without installing

```bash
# 3 tabs, broadcasting on, all tagged "cluster"
./target/release/rs-mrxvt -n 3 -j -g cluster

# With a Lua config that picks theme by time of day
./target/release/rs-mrxvt --features lua -c ~/.config/rs-mrxvt/config.lua
```

## 🎹 Keybindings (default)

| Shortcut              | Action                                        |
|-----------------------|-----------------------------------------------|
| `Ctrl+Shift+T` / `Alt+N` | New tab (login shell)                      |
| `Alt+Z`               | New tab running zsh (secondary shell)         |
| `Alt+Shift+X`         | Close the currently focused tab               |
| `Ctrl+Shift+W`        | Close tab (classic mrxvt binding)             |
| `Ctrl+Shift+C` / `Ctrl+Insert` | Copy selection to clipboard          |
| `Ctrl+Shift+V` / `Shift+Insert` | Paste from clipboard                |
| `Ctrl+Shift+I`        | Toggle broadcast (all tabs)                   |
| `Ctrl+Shift+P`        | Open command palette                          |
| `Ctrl+Tab` / `Alt+Right` | Next tab (wraps)                           |
| `Ctrl+Shift+Tab` / `Alt+Left` | Previous tab (wraps)                  |
| `Alt+1` … `Alt+0`     | Go to tab N (1..10)                           |
| `Shift+PageUp` / `Shift+PageDown` | Scroll scrollback a page       |
| `Shift+Home` / `Shift+End` | Jump to top / bottom of scrollback       |

Mouse: drag selects, double-click selects a word, triple-click selects a
line, release copies (copy-on-select), middle-click pastes the primary
selection, right-click pastes the clipboard, and the wheel scrolls the
scrollback (or the alternate-screen program). `Shift+click` bypasses
application mouse reporting so you can always select text.

All bindings are rebindable in `config.toml` (`[macros]` maps a chord
string to a command spec, e.g. `"Ctrl+Shift+R" = "ResetTerminal"` or
`"Ctrl+Shift+G" = "ToggleBroadcastGroup:web"`) or `config.lua`.

> **Host-terminal note:** the `Ctrl+Shift+*` chords rely on the kitty
> keyboard protocol, which rs-mrxvt enables at startup. On supporting
> hosts (kitty, foot, WezTerm, ghostty, recent alacritty) they work as
> documented; legacy terminals map Ctrl+Shift+T and Ctrl+T to the same
> byte, so on those hosts prefer the `Alt+` bindings.

## ⚙️ Configuration

Default location: `~/.config/rs-mrxvt/config.toml` (or `.lua` with the lua
feature). Override with `--config` or `$MRXVT_CONFIG`. See
[`examples/config.toml`](examples/config.toml) and
[`examples/config.lua`](examples/config.lua) for fully-commented references.

### Transparency example (TOML)

```toml
[transparency]
enabled = true
tint = "#004080"      # blue tint
opacity = 0.85         # 1.0 = opaque, 0.0 = fully transparent
# background_image = "/path/to/wallpaper.png"
```

> **Note:** the `[transparency]` schema parses today but no renderer
> consumes it yet (see the roadmap) — settings have no visual effect
> until the shader wiring lands.

### Clipboard

```toml
[clipboard]
copy_on_select = true   # selecting text copies it (classic mrxvt/xterm)
osc52 = true            # mirror copies to the host terminal — works over SSH
osc52_read = false      # let programs read the clipboard via OSC 52 (security)
# tool = "xclip"        # force a tool; otherwise auto-detected
```

rs-mrxvt talks to the system clipboard through the standard tools
(`xclip`/`xsel` on X11, `wl-clipboard` on Wayland, `pbcopy`/`pbpaste` on
macOS, `termux-clipboard-*` on Android) — no clipboard library is linked.
Every copy is *additionally* emitted as an OSC 52 escape sequence, so the
**host terminal** performs the clipboard write too; over SSH with no
local clipboard tool, that mirror is what makes copy work.

Paste sanitation matches mainstream terminals: control characters that
could inject escape sequences are stripped, CRLF is normalized to LF, and
a single trailing newline is dropped so a paste doesn't auto-submit your
command. When the child enabled bracketed paste (DECSET 2004), pastes are
wrapped in `ESC[200~ … ESC[201~`.

### Time-based theme (Lua)

```lua
local hour = tonumber(os.date("%H"))
local theme = "mrxvt"
if hour >= 20 or hour < 6 then
  theme = "tokyo-night"
end

return {
  ui = { theme = theme },
  terminal = { cols = 120, rows = 40 },
}
```

## 🎨 Themes

Built-in true-color themes (set `ui.theme` in your config):

| Theme             | Style                          |
|-------------------|--------------------------------|
| `mrxvt`           | Classic green-on-black (default) |
| `tokyo-night`     | Dark blue, popularized by VS Code |
| `gruvbox`         | Warm retro palette              |
| `dracula`         | Dark purple                     |
| `solarized-dark`  | Solarized Dark                  |
| `solarized-light` | Solarized Light                 |

Custom themes are `[custom_themes.<name>]` tables in config.toml with the
full palette (16 ANSI colors + bg/fg/cursor, `#RRGGBB` values):

```toml
[ui]
theme = "midnight"

[custom_themes.midnight]
bg = "#0a0e14"
fg = "#c5cdd7"
cursor = "#c5cdd7"
# ... black, red, green, yellow, blue, magenta, cyan, white and the
# bright_* variants — see examples/config.toml for the complete list.
```

Themes apply in every renderer (TUI, wgpu, softbuffer), and with config
hot-reload a theme edit takes effect on the next frame.

## 🛠 Troubleshooting

**GUI apps refuse to start from a tab** (`cannot open display`, D-Bus
errors): default-shell tabs run **login shells**, which source
`/etc/profile` — that's where distros export the GUI session environment
(`DBUS_SESSION_BUS_ADDRESS`, flatpak/snap paths). If tabs still can't
launch GUI apps, rs-mrxvt itself was probably started from a
non-graphical context (plain TTY, `sudo`, a stripped service): the child
environment is inherited, so there is no `DISPLAY`/`WAYLAND_DISPLAY` to
inherit. Start rs-mrxvt from your graphical session, or set the variables
yourself. rs-mrxvt logs a warning at spawn when this is the case. If your
`~/.bash_profile` is noisy, per-profile `login_shell = false` restores
plain non-login shells.

**Copy works but paste doesn't (over SSH):** paste reads the *local*
system clipboard, which needs a local tool (`xclip`, `xsel`,
`wl-paste`). OSC 52 can only *write* the client's clipboard, not read it
synchronously — that's a protocol limit, not a bug.

## 🧪 Testing

```bash
cargo test                            # default suite (~10s, 150+ tests)
cargo test --features lua             # + Lua config tests
cargo test --features images          # + image + Sixel tests
cargo test --features gpu             # + GPU backend tests
cargo test --features lua,images      # everything, 200+ tests
python3 scripts/stress_test.py        # 50-instance broadcast harness
```

## 📦 Distribution-agnostic packaging

Three helper scripts make the build pipeline distro-agnostic:

```bash
./scripts/sysprep.sh              # install build deps (auto-detects distro)
./scripts/build.sh                # cargo build with feature flags
./install.sh                      # build + install to $PREFIX
./install.sh --sysprep            # sysprep + build + install in one go
```

### sysprep.sh

Detects your distro via `/etc/os-release` and installs the right packages:

| Distro family                       | Package manager |
|-------------------------------------|-----------------|
| Arch, Manjaro, EndeavourOS, Garuda  | `pacman`        |
| Debian, Ubuntu, Pop!_OS, Mint, Kali | `apt`           |
| Fedora, RHEL, Rocky, Alma, CentOS   | `dnf`           |
| openSUSE, SLES                      | `zypper`        |
| Void                                | `xbps-install`  |
| Alpine                              | `apk`           |
| NixOS                               | prints `shell.nix` recipe |
| SourceMage                          | `cast`          |
| Gentoo, Funtoo                      | `emerge`        |

Flags: `--no-rust` (skip rustup), `--no-gpu` (skip GPU headers), `--dry-run`.

### build.sh

Wraps `cargo build` with feature-flag presets:

```bash
./scripts/build.sh                          # release, all features
./scripts/build.sh --debug                  # debug build
./scripts/build.sh --features lua,images    # specific features
./scripts/build.sh --no-features            # bare TUI
./scripts/build.sh --test                   # cargo test
./scripts/build.sh --check                  # cargo check only
```

### install.sh

Builds + copies binary, examples, and (optional) man page / .desktop file
into `$PREFIX` (default `/usr/local`).

```bash
sudo ./install.sh                  # /usr/local
sudo ./install.sh /usr             # /usr
sudo ./install.sh --sysprep        # full pipeline: deps + build + install
sudo ./install.sh --no-features    # TUI-only build (no GPU headers needed)
```

The Makefile still works for the common cases (`make install`, `make dist`,
`make deb`, `make rpm`).

## 📜 License

GPL v2 (or later). See [`LICENSE`](LICENSE).

## 🤝 Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md). The wgpu and softbuffer backends
are functional but always benefit from optimization work — glyph atlas
packing, sub-pixel positioning, and shader effects are good first PRs.

