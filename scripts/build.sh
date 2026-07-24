#!/usr/bin/env bash
# build.sh — feature-flag-aware build wrapper for rs-mrxvt.
#
# Wraps `cargo build` with sensible defaults and feature-flag presets.
# Reads the FEATURES env var or accepts --features / --release / --debug
# flags. Mirrors the dependencies installed by sysprep.sh.
#
# Usage:
#   ./scripts/build.sh                       # release, all features
#   ./scripts/build.sh --debug               # debug build, all features
#   ./scripts/build.sh --features lua,images # specific features only
#   ./scripts/build.sh --no-features         # bare TUI build
#   ./scripts/build.sh --release --features gpu,lua,images
#   ./scripts/build.sh --check               # cargo check only
#   ./scripts/build.sh --test                # cargo test
#   FEATURES=lua,images ./scripts/build.sh
#
# Feature flags:
#   lua    — dynamic Lua config support (mlua, vendored)
#   images — iTerm2 + Sixel image protocol (image crate)
#   gpu    — wgpu + softbuffer rendering backends
#
# Default (no flags): all three features enabled.

set -e

# ─── Defaults ────────────────────────────────────────────────────────────────
PROFILE="release"
FEATURES="${FEATURES:-lua,images,gpu}"
DO_CHECK=0
DO_TEST=0

# ─── Parse args ──────────────────────────────────────────────────────────────
while [ $# -gt 0 ]; do
    case "$1" in
        --debug)        PROFILE="debug"; shift ;;
        --release)      PROFILE="release"; shift ;;
        --features)     FEATURES="$2"; shift 2 ;;
        --no-features)  FEATURES=""; shift ;;
        --check)        DO_CHECK=1; shift ;;
        --test)         DO_TEST=1; shift ;;
        -h|--help)
            sed -n '2,25p' "$0"
            exit 0
            ;;
        *)
            echo "unknown flag: $1" >&2
            exit 1
            ;;
    esac
done

# ─── Ensure rust is installed ────────────────────────────────────────────────
if ! command -v cargo &>/dev/null; then
    if [ -x "$HOME/.cargo/bin/cargo" ]; then
        # shellcheck disable=SC1091
        . "$HOME/.cargo/env"
    else
        echo "Error: cargo not found. Run ./scripts/sysprep.sh first." >&2
        exit 1
    fi
fi

# ─── Build the cargo command ────────────────────────────────────────────────
cd "$(dirname "$0")/.."

CARGO_ARGS=()

if [ "$DO_TEST" = "1" ]; then
    CARGO_ARGS+=("test")
    if [ "$PROFILE" = "release" ]; then
        CARGO_ARGS+=("--release")
    fi
elif [ "$DO_CHECK" = "1" ]; then
    CARGO_ARGS+=("check")
else
    CARGO_ARGS+=("build")
    if [ "$PROFILE" = "release" ]; then
        CARGO_ARGS+=("--release")
    fi
fi

if [ -n "$FEATURES" ]; then
    CARGO_ARGS+=("--features" "$FEATURES")
fi

# ─── Echo + run ──────────────────────────────────────────────────────────────
echo "=== rs-mrxvt build ==="
echo "  profile: $PROFILE"
echo "  features: ${FEATURES:-<none>}"
echo "  command:  cargo ${CARGO_ARGS[*]}"
echo ""

cargo "${CARGO_ARGS[@]}"

echo ""
if [ "$DO_TEST" = "0" ] && [ "$DO_CHECK" = "0" ] && [ "$PROFILE" = "release" ]; then
    BINARY="target/release/rs-mrxvt"
    if [ -x "$BINARY" ]; then
        SIZE=$(du -h "$BINARY" | cut -f1)
        echo "Built: $BINARY ($SIZE)"
        echo "Try:   $BINARY --help"
    fi
fi
