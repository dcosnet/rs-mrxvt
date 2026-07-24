#!/bin/sh
# rs-mrxvt — distro-agnostic install script.
#
# Builds the release binary and copies it (plus docs and examples) into the
# requested prefix. No package manager is touched. Works on any Linux
# distro (Debian, Arch, SourceMage, Fedora, NixOS with adaptation, …).
#
# Usage:
#   ./install.sh                 # interactive: asks for prefix
#   ./install.sh /usr            # installs to /usr/{bin,share/...}
#   PREFIX=/usr ./install.sh     # same, via env var
#   ./install.sh --sysprep       # run sysprep.sh first to install build deps
#   ./install.sh --features lua,images,gpu  # build with these features
#
# Environment:
#   PREFIX    — install prefix (default: /usr/local)
#   CARGO     — cargo binary (default: cargo)
#   NO_BUILD  — if set to 1, skip `cargo build --release` (assume it's done)
#   FEATURES  — cargo features to build with (default: lua,images,gpu)

set -e

PKG_NAME="rs-mrxvt"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

CARGO="${CARGO:-cargo}"
PREFIX="${PREFIX:-}"
DO_SYSPREP=0
FEATURES="${FEATURES:-lua,images,gpu}"

# Parse args: --sysprep, --features X, or a positional PREFIX.
while [ $# -gt 0 ]; do
    case "$1" in
        --sysprep) DO_SYSPREP=1; shift ;;
        --features) FEATURES="$2"; shift 2 ;;
        --features=*) FEATURES="${1#--features=}"; shift ;;
        -h|--help)
            sed -n '2,22p' "$0"
            exit 0
            ;;
        *)
            if [ -z "$PREFIX" ]; then
                PREFIX="$1"
            fi
            shift
            ;;
    esac
done

# If still no prefix, prompt.
if [ -z "$PREFIX" ]; then
    printf "Install prefix [default: /usr/local]: "
    read -r input
    PREFIX="${input:-/usr/local}"
fi

BINDIR="$PREFIX/bin"
DATADIR="$PREFIX/share"
MANDIR="$DATADIR/man/man1"
APPDIR="$DATADIR/applications"
EXAMPLEDIR="$DATADIR/$PKG_NAME/examples"

echo "=== Installing $PKG_NAME to $PREFIX ==="
echo "  binary:    $BINDIR/$PKG_NAME"
echo "  examples:  $EXAMPLEDIR/"
echo ""

# Build (unless caller skipped).
if [ "${NO_BUILD:-0}" != "1" ]; then
    # Optional: run sysprep to install build deps first.
    if [ "$DO_SYSPREP" = "1" ]; then
        echo "[0/4] Running sysprep.sh to install build deps..."
        sh ./scripts/sysprep.sh || {
            echo "sysprep.sh failed; continuing anyway (deps may already be present)" >&2
        }
    fi

    echo "[1/4] Building release binary (features: ${FEATURES:-<none>})..."
    if [ -n "$FEATURES" ]; then
        $CARGO build --release --features "$FEATURES"
    else
        $CARGO build --release
    fi
else
    echo "[1/4] Skipping build (NO_BUILD=1)."
fi

# Verify the binary exists.
if [ ! -f "target/release/$PKG_NAME" ]; then
    echo "Error: target/release/$PKG_NAME not found. Run without NO_BUILD=1." >&2
    exit 1
fi

# Sudo escalation if the prefix isn't user-writable.
NEED_SUDO=""
if [ -w "$PREFIX" ] || [ -w "$(dirname "$PREFIX")" ] 2>/dev/null; then
    NEED_SUDO=""
else
    NEED_SUDO="sudo"
    echo "  (using sudo for install — prefix $PREFIX is not user-writable)"
fi

echo "[2/4] Creating directories..."
$NEED_SUDO mkdir -p "$BINDIR" "$EXAMPLEDIR"
[ -d "$MANDIR" ] && $NEED_SUDO mkdir -p "$MANDIR" || true
[ -d "$APPDIR" ] && $NEED_SUDO mkdir -p "$APPDIR" || true

echo "[3/4] Copying files..."
$NEED_SUDO install -m 755 "target/release/$PKG_NAME" "$BINDIR/"
$NEED_SUDO install -m 644 "examples/config.toml" "$EXAMPLEDIR/"

# Install man page if it exists (built from docs via pandoc; not required).
if [ -f "docs/$PKG_NAME.1" ]; then
    $NEED_SUDO install -m 644 "docs/$PKG_NAME.1" "$MANDIR/"
    echo "  installed man page → $MANDIR/$PKG_NAME.1"
fi

# Install .desktop if it exists.
if [ -f "assets/$PKG_NAME.desktop" ]; then
    $NEED_SUDO install -m 644 "assets/$PKG_NAME.desktop" "$APPDIR/"
    echo "  installed .desktop → $APPDIR/$PKG_NAME.desktop"
fi

echo "[4/4] Done."
echo ""
echo "Try: $BINDIR/$PKG_NAME --help"
echo "Or:  $BINDIR/$PKG_NAME -n 3 -j   # 3 tabs, broadcast on"
echo ""
echo "To uninstall: sudo rm -f $BINDIR/$PKG_NAME $MANDIR/$PKG_NAME.1 $APPDIR/$PKG_NAME.desktop && sudo rm -rf $EXAMPLEDIR"
