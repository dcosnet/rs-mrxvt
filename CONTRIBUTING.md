# Contributing to rs-mrxvt

Thanks for your interest! This doc covers the practical bits: getting a
build going, the code style we use, and how to land a PR.

## Build from source

You need:

- Rust 1.75 or newer (1.97+ recommended; we test against latest stable).
- A POSIX shell (`/bin/sh`).
- Python 3.10+ (only if you want to run the stress harness).

That's it. The MVP backend is a TUI (ratatui + crossterm), so no Vulkan,
Wayland, or X11 dev headers are required.

```bash
git clone <your-fork-url> rs-mrxvt
cd rs-mrxvt
cargo build              # debug build
cargo build --release    # optimized build (~5MB stripped binary)
./target/debug/rs-mrxvt  # run
```

## Running the tests

```bash
cargo test                            # full suite (~10s)
cargo test --lib                      # unit tests only
cargo test --test integration         # PTY round-trip tests
cargo test --test broadcasting        # broadcast routing tests
python3 scripts/stress_test.py        # 50-instance stress harness
```

All 71 Rust tests + the stress harness must pass before a PR can merge.

## Code style

- **Edition 2021**, `rustfmt` defaults, `clippy` clean.
- Module-level docs at the top of every file. Explain *why*, not *what*.
- Public items get doc comments. Private items get them when non-obvious.
- Tests live inline (`#[cfg(test)] mod tests`) for unit tests, in
  `tests/` for integration tests.
- No `unwrap()` in production code paths — use `anyhow::Result` and
  propagate. `unwrap()` is fine in tests.
- Imports grouped: std → external crates → crate-local.

Run before pushing:

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
```

## Architecture orientation

Read [`ARCHITECTURE.md`](ARCHITECTURE.md) first. The 30-second version:

- `App` owns `TerminalManager` + `InputRouter` + `PaletteState`.
- `TerminalManager` owns the `Vec<TerminalTab>` and routes input bytes
  based on `BroadcastTarget` (Active / All / Group).
- Each `TerminalTab` owns a `PtySession` + `alacritty_terminal::Term` +
  a dedicated reader thread.
- `Renderer` is a trait; `TuiRenderer` is the default impl. A future
  `WgpuRenderer` will slot in via the same trait.
- `Command` is the single source of truth for user actions. Palette and
  keybindings both consume it.

## Where help is wanted

These are the planned-but-unimplemented features. Each is sized for a
focused PR:

| Feature | Difficulty | Where to start |
|---|---|---|
| wgpu/Wayland GUI backend | Hard | New `WgpuRenderer` impl of `Renderer` trait |
| Lua config | Medium | `mlua` impl of `ConfigSource` trait |
|                                  |            |                                             |
| Per-tab fading in TUI | Easy | `TabState` lerp in `TuiRenderer::draw` |
| Sixel image protocol | Hard | Hook `alacritty_terminal`'s Handler trait |
| Hyperlink (OSC 8) clickable URLs | Medium | ratatui `Paragraph` + mouse hit-testing |
| Command palette for macros | Easy | Add macro entries to `Command::defaults()` |

## Pull request checklist

- [ ] `cargo fmt --all -- --check` clean
- [ ] `cargo clippy --all-targets -- -D warnings` clean
- [ ] `cargo test` green (71+ tests)
- [ ] If you added a feature, added a test for it
- [ ] If you changed public API, updated `ARCHITECTURE.md`
- [ ] If you added a CLI flag, added a test in `tests/integration.rs`
- [ ] Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/)

## Reporting bugs

Open an issue with:

1. rs-mrxvt version (`rs-mrxvt --version`)
2. Distro + desktop environment (e.g. "Arch + Sway 1.10")
3. Repro steps
4. What you expected vs what happened
5. `RUST_LOG=debug` output if it's a behavior bug

## License

By contributing, you agree your contributions are licensed under the MIT
license covering the project.
