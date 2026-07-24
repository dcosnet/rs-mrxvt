#!/usr/bin/env bash
# sysprep.sh — install build dependencies for rs-mrxvt on any Linux distro.
#
# Detects the distro via /etc/os-release and runs the appropriate package
# manager command. Pulls in everything needed to build rs-mrxvt with all
# feature flags: rust toolchain, vulkan headers, wayland, xkbcommon, and
# the optional deps for Lua + image support.
#
# Usage:
#   ./scripts/sysprep.sh             # install everything
#   ./scripts/sysprep.sh --no-rust   # skip rustup (use system rust)
#   ./scripts/sysprep.sh --no-gpu    # skip GPU dev headers (TUI-only build)
#   ./scripts/sysprep.sh --dry-run   # print what would be installed, don't run
#
# Supported distros (auto-detected):
#   arch, manjaro, endeavouros      → pacman
#   debian, ubuntu, pop!_os, mint   → apt
#   fedora, rhel, rocky, alma       → dnf
#   opensuse, suse                  → zypper
#   void                            → xbps-install
#   alpine                          → apk
#   nixos                           → nix-shell (prints a shell.nix recipe)
#   sourcemage                      → cast
#   gentoo, funtoo                  → emerge
#   unknown                         → prints manual instructions

set -e

# ─── Flags ───────────────────────────────────────────────────────────────────
INSTALL_RUST=1
INSTALL_GPU=1
DRY_RUN=0

for arg in "$@"; do
    case "$arg" in
        --no-rust)  INSTALL_RUST=0 ;;
        --no-gpu)   INSTALL_GPU=0 ;;
        --dry-run)  DRY_RUN=1 ;;
        -h|--help)
            sed -n '2,20p' "$0"
            exit 0
            ;;
        *)
            echo "unknown flag: $arg" >&2
            exit 1
            ;;
    esac
done

# ─── Helpers ─────────────────────────────────────────────────────────────────

run() {
    if [ "$DRY_RUN" = "1" ]; then
        echo "  [dry-run] $*"
    else
        "$@"
    fi
}

need_root() {
    if [ "$(id -u)" != "0" ] && [ "$DRY_RUN" = "0" ]; then
        echo "This command needs root; re-running with sudo." >&2
        exec sudo "$0" "$@"
    fi
}

# ─── Distro detection ────────────────────────────────────────────────────────

detect_distro() {
    if [ ! -f /etc/os-release ]; then
        echo "unknown"
        return
    fi
    # shellcheck disable=SC1091
    . /etc/os-release
    case "$ID" in
        arch|manjaro|endeavouros|garuda|artix) echo "arch" ;;
        debian|ubuntu|linuxmint|pop|elementary|kali|raspbian) echo "debian" ;;
        fedora|rhel|rocky|almalinux|centos|amzn) echo "fedora" ;;
        opensuse*|suse|sles) echo "opensuse" ;;
        void) echo "void" ;;
        alpine) echo "alpine" ;;
        nixos) echo "nixos" ;;
        sourcemage|smgl) echo "sourcemage" ;;
        gentoo|funtoo) echo "gentoo" ;;
        *) echo "unknown" ;;
    esac
}

DISTRO=$(detect_distro)
echo "=== rs-mrxvt sysprep ==="
echo "  detected distro: $DISTRO"
echo "  install rust:    $([ "$INSTALL_RUST" = "1" ] && echo yes || echo no)"
echo "  install gpu deps: $([ "$INSTALL_GPU" = "1" ] && echo yes || echo no)"
echo "  dry-run:         $([ "$DRY_RUN" = "1" ] && echo yes || echo no)"
echo ""

# ─── Common package lists ────────────────────────────────────────────────────

# Required for any build:
#   - rust toolchain (rustup or system rust)
#   - C compiler + linker (gcc/cc)
#   - pkg-config
#   - git (for fetching source if needed)

# Required for --features gpu:
#   - vulkan headers/loader
#   - wayland client + protocols
#   - xkbcommon
#   - libx11 + libxcb (for X11 fallback in winit)

# Optional for --features images:
#   - none extra (image crate vendors its deps)

# Optional for --features lua:
#   - none extra (mlua vendors Lua 5.4)

# ─── Per-distro installers ───────────────────────────────────────────────────

install_arch() {
    echo "[arch] installing deps via pacman..."
    local pkgs=(base-devel pkgconf git)
    if [ "$INSTALL_RUST" = "1" ]; then
        pkgs+=(rust)
    fi
    if [ "$INSTALL_GPU" = "1" ]; then
        pkgs+=(vulkan-headers vulkan-loader wayland-protocols libxkbcommon xorg-server-xauth)
    fi
    run pacman -S --needed --noconfirm "${pkgs[@]}"
}

install_debian() {
    echo "[debian] installing deps via apt..."
    local pkgs=(build-essential pkg-config git ca-certificates)
    if [ "$INSTALL_RUST" = "1" ]; then
        # Debian's rust package is often outdated; prefer rustup.
        pkgs+=(curl)
    fi
    if [ "$INSTALL_GPU" = "1" ]; then
        pkgs+=(libvulkan-dev libwayland-dev libxkbcommon-dev libx11-dev libxcb1-dev)
    fi
    run apt-get update -y
    run apt-get install -y "${pkgs[@]}"
    if [ "$INSTALL_RUST" = "1" ] && [ "$DRY_RUN" = "0" ]; then
        # Install rustup if rust isn't already present.
        if ! command -v cargo &>/dev/null; then
            curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal
            # shellcheck disable=SC1091
            . "$HOME/.cargo/env"
        fi
    fi
}

install_fedora() {
    echo "[fedora] installing deps via dnf..."
    local pkgs=(gcc pkgconf-pkg-config git)
    if [ "$INSTALL_RUST" = "1" ]; then
        pkgs+=(rust cargo)
    fi
    if [ "$INSTALL_GPU" = "1" ]; then
        pkgs+=(vulkan-headers vulkan-loader-devel wayland-devel wayland-protocols-devel libxkbcommon-devel libX11-devel libxcb-devel)
    fi
    run dnf install -y "${pkgs[@]}"
}

install_opensuse() {
    echo "[opensuse] installing deps via zypper..."
    local pkgs=(patterns-devel-base-devel_basis pkg-config git)
    if [ "$INSTALL_RUST" = "1" ]; then
        pkgs+=(rust cargo)
    fi
    if [ "$INSTALL_GPU" = "1" ]; then
        pkgs+=(vulkan wayland-protocols libxkbcommon-devel libX11-devel libxcb-devel)
    fi
    run zypper install -y "${pkgs[@]}"
}

install_void() {
    echo "[void] installing deps via xbps-install..."
    local pkgs=(base-devel pkg-config git)
    if [ "$INSTALL_RUST" = "1" ]; then
        pkgs+=(rust)
    fi
    if [ "$INSTALL_GPU" = "1" ]; then
        pkgs+=(vulkan-loader Vulkan-Headers wayland-protocols libxkbcommon-devel libX11-devel libxcb-devel)
    fi
    run xbps-install -Sy "${pkgs[@]}"
}

install_alpine() {
    echo "[alpine] installing deps via apk..."
    local pkgs=(build-base pkgconf git)
    if [ "$INSTALL_RUST" = "1" ]; then
        pkgs+=(rust cargo)
    fi
    if [ "$INSTALL_GPU" = "1" ]; then
        pkgs+=(vulkan-loader-dev vulkan-headers wayland-protocols libxkbcommon-dev libx11-dev libxcb-dev)
    fi
    run apk add "${pkgs[@]}"
}

install_nixos() {
    echo "[nixos] NixOS uses declarative packaging."
    echo "  Create a shell.nix with:"
    cat <<'EOF'
    { pkgs ? import <nixpkgs> {} }:
    pkgs.mkShell {
      nativeBuildInputs = with pkgs; [
        pkg-config
        rustc cargo rustPlatform.rustcSrc
      ] ++ lib.optionals true [
        vulkan-headers vulkan-loader
        wayland wayland-protocols
        libxkbcommon
        xorg.libX11 xorg.libxcb
      ];
    }
EOF
    echo "  Then run: nix-shell shell.nix"
}

install_sourcemage() {
    echo "[sourcemage] installing deps via cast..."
    local spells=(rust pkgconfig git)
    if [ "$INSTALL_GPU" = "1" ]; then
        spells+=(vulkan-loader wayland-protocols xkbcommon)
    fi
    run cast "${spells[@]}"
}

install_gentoo() {
    echo "[gentoo] installing deps via emerge..."
    local pkgs=(dev-vcs/git dev-util/pkgconf)
    if [ "$INSTALL_RUST" = "1" ]; then
        pkgs+=(dev-lang/rust)
    fi
    if [ "$INSTALL_GPU" = "1" ]; then
        pkgs+=(media-libs/vulkan-loader dev-libs/wayland-protocols x11-libs/libxkbcommon)
    fi
    run emerge --ask=n "${pkgs[@]}"
}

install_unknown() {
    echo "=== Unknown distro — manual install instructions ==="
    echo ""
    echo "Required for any build:"
    echo "  - rust toolchain (>= 1.75):  https://rustup.rs"
    echo "  - C compiler + linker (gcc/clang)"
    echo "  - pkg-config"
    echo "  - git"
    echo ""
    echo "Required for --features gpu:"
    echo "  - Vulkan headers + loader"
    echo "  - wayland client + protocols"
    echo "  - xkbcommon"
    echo "  - libX11 + libxcb (for X11 fallback)"
    echo ""
    echo "Optional for --features lua:"
    echo "  - none (mlua vendors Lua 5.4)"
    echo ""
    echo "Optional for --features images:"
    echo "  - none (image crate vendors its deps)"
    echo ""
    echo "After installing deps, run: ./scripts/build.sh"
}

# ─── Main ────────────────────────────────────────────────────────────────────

case "$DISTRO" in
    arch)       need_root; install_arch ;;
    debian)     need_root; install_debian ;;
    fedora)     need_root; install_fedora ;;
    opensuse)   need_root; install_opensuse ;;
    void)       need_root; install_void ;;
    alpine)     need_root; install_alpine ;;
    nixos)      install_nixos ;;
    sourcemage) need_root; install_sourcemage ;;
    gentoo)     need_root; install_gentoo ;;
    *)          install_unknown; exit 0 ;;
esac

echo ""
echo "=== sysprep complete ==="
if [ "$DRY_RUN" = "0" ]; then
    echo "Next step: ./scripts/build.sh"
fi
