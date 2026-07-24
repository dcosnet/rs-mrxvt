# rs-mrxvt Quick Start

A 60-second tour from zero to running terminal. For full docs see
[`README.md`](../README.md).

## 1. Install build deps

```bash
./scripts/sysprep.sh
```

This auto-detects your distro and installs Rust + the system libraries
needed for the GPU backends. Works on Arch, Debian/Ubuntu, Fedora,
openSUSE, Void, Alpine, NixOS, SourceMage, and Gentoo.

Skip GPU deps if you only want the TUI backend:

```bash
./scripts/sysprep.sh --no-gpu
```

## 2. Build

```bash
./scripts/build.sh
```

Defaults to release mode with all features (`lua,images,gpu`). For a
barebones TUI-only build:

```bash
./scripts/build.sh --no-features
```

For a debug build while hacking:

```bash
./scripts/build.sh --debug
```

## 3. Run

```bash
./target/release/rs-mrxvt
```

You should see a bash shell in a tabbed terminal. Try these:

| Keys              | What happens                              |
|-------------------|-------------------------------------------|
| `Alt+N`           | New bash tab                              |
| `Alt+Z`           | New zsh tab                               |
| `Alt+1` … `Alt+0` | Jump to tab 1..10                         |
| `Alt+Left/Right`  | Shuffle to previous/next tab              |
| `Alt+Shift+X`     | Close the focused tab                     |
| `Ctrl+Shift+I`    | Toggle input broadcasting to all tabs     |
| `Ctrl+Shift+P`    | Open the command palette (fuzzy search)   |
| `Ctrl+Shift+T`    | New tab (classic mrxvt binding)           |
| `Ctrl+Shift+W`    | Close tab (classic mrxvt binding)         |

## 4. Broadcasting (the killer feature)

Open three tabs and broadcast keystrokes to all of them:

```bash
./target/release/rs-mrxvt -n 3 -j
```

Now anything you type goes to every tab simultaneously. The status bar
turns red and shows `● BROADCAST:All`. Toggle it off with `Ctrl+Shift+I`.

For tagged-group broadcasting (only some tabs receive input):

```bash
./target/release/rs-mrxvt -n 5 -g web
```

All 5 tabs are tagged "web". Use `ToggleBroadcastGroup("web")` from the
command palette to broadcast only to them.

## 5. Pick a theme

Edit `~/.config/rs-mrxvt/config.toml`:

```toml
[ui]
theme = "tokyo-night"   # or: mrxvt, gruvbox, dracula, solarized-dark, solarized-light
```

Or use Lua for dynamic theming (`~/.config/rs-mrxvt/config.lua`, requires
`--features lua`):

```lua
local hour = tonumber(os.date("%H"))
local theme = "mrxvt"
if hour >= 20 or hour < 6 then
  theme = "tokyo-night"
end

return {
  ui = { theme = theme },
}
```

## 6. Install system-wide

```bash
sudo ./install.sh              # /usr/local/bin/rs-mrxvt
sudo ./install.sh /usr         # /usr/bin/rs-mrxvt
```

Or one-shot from a fresh checkout:

```bash
sudo ./install.sh --sysprep    # install deps + build + install
```

## 7. Verify it works

```bash
rs-mrxvt --version
rs-mrxvt --help
python3 scripts/stress_test.py --tabs 50   # 50-instance broadcast test
```

## Next steps

- Read [`README.md`](../README.md) for the full feature list and config schema.
- Read [`ARCHITECTURE.md`](../ARCHITECTURE.md) for the module layout.
- Read [`CONTRIBUTING.md`](../CONTRIBUTING.md) if you want to hack on it.
- File issues / PRs at the project repo.
