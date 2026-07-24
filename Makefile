# rs-mrxvt — distro-agnostic Makefile.
#
# Targets:
#   build         — cargo build --release (default)
#   check         — cargo check
#   test          — full test suite (unit + integration)
#   stress        — 50-instance Python broadcast harness
#   fmt / clippy  — code quality
#   clean         — cargo clean
#   install       — install binary + assets to $PREFIX (default /usr/local)
#   uninstall     — reverse of install
#   dist          — build source tarball: rs-mrxvt-<ver>.tar.xz
#   deb           — build .deb via cargo-deb (only if cargo-deb installed)
#   rpm           — build .rpm via cargo-rpm (only if cargo-rpm installed)
#   run           — cargo run --release
#   help          — this list

PREFIX     ?= /usr/local
BINDIR     ?= $(PREFIX)/bin
DATADIR    ?= $(PREFIX)/share
MANDIR     ?= $(DATADIR)/man/man1
APPDIR     ?= $(DATADIR)/applications

CARGO      ?= cargo
VERSION    := $(shell grep '^version' Cargo.toml | head -1 | cut -d '"' -f 2)
PKG_NAME   := rs-mrxvt

.PHONY: help build build-gpu check check-gpu test test-gpu stress fmt clippy clean install uninstall dist deb rpm run

help:
        @echo "rs-mrxvt $(VERSION) — make targets:"
        @echo "  build      cargo build --release (TUI only, no GPU deps)"
        @echo "  build-gpu  cargo build --release --features gpu (wgpu + softbuffer)"
        @echo "  check      cargo check"
        @echo "  check-gpu  cargo check --features gpu"
        @echo "  test       cargo test"
        @echo "  test-gpu   cargo test --features gpu"
        @echo "  stress     50-instance Python broadcast stress harness"
        @echo "  fmt        cargo fmt --all -- --check"
        @echo "  clippy     cargo clippy --all-targets -- -D warnings"
        @echo "  install    install to $(PREFIX) (set PREFIX=... to override)"
        @echo "  uninstall  remove from $(PREFIX)"
        @echo "  dist       build $(PKG_NAME)-$(VERSION).tar.xz source tarball"
        @echo "  deb        build .deb (requires cargo-deb)"
        @echo "  rpm        build .rpm (requires cargo-rpm)"
        @echo "  run        cargo run --release"
        @echo "  clean      cargo clean"

build:
        $(CARGO) build --release

# Build with GPU acceleration (wgpu + softbuffer).
# Requires system deps: libvulkan-dev, libwayland-dev, libxkbcommon-dev (or
# distro equivalents).  Falls back to TUI at runtime if no GPU is found.
build-gpu:
        $(CARGO) build --release --features gpu

check:
        $(CARGO) check

check-gpu:
        $(CARGO) check --features gpu

test:
        $(CARGO) test

test-gpu:
        $(CARGO) test --features gpu

stress: build
        python3 scripts/stress_test.py --tabs 50 --timeout 30

fmt:
        $(CARGO) fmt --all -- --check

clippy:
        $(CARGO) clippy --all-targets -- -D warnings

run:
        $(CARGO) run --release

clean:
        $(CARGO) clean

# ─── Installation ────────────────────────────────────────────────────────────
# Distro-agnostic: just copy files into $PREFIX. No package manager involved.

install: build
        install -d $(DESTDIR)$(BINDIR)
        install -d $(DESTDIR)$(MANDIR)
        install -d $(DESTDIR)$(APPDIR)
        install -d $(DESTDIR)$(DATADIR)/$(PKG_NAME)/examples
        install -m 755 target/release/$(PKG_NAME) $(DESTDIR)$(BINDIR)/
        install -m 644 examples/config.toml $(DESTDIR)$(DATADIR)/$(PKG_NAME)/examples/
        # Install man page if it exists (built from docs/ via pandoc; not required).
        if [ -f docs/$(PKG_NAME).1 ]; then \
                install -m 644 docs/$(PKG_NAME).1 $(DESTDIR)$(MANDIR)/; \
        fi
        # Install .desktop if it exists.
        if [ -f assets/$(PKG_NAME).desktop ]; then \
                install -m 644 assets/$(PKG_NAME).desktop $(DESTDIR)$(APPDIR)/; \
        fi
        @echo ""
        @echo "Installed to $(PREFIX). Try: $(BINDIR)/$(PKG_NAME)"

uninstall:
        rm -f $(DESTDIR)$(BINDIR)/$(PKG_NAME)
        rm -f $(DESTDIR)$(MANDIR)/$(PKG_NAME).1
        rm -f $(DESTDIR)$(APPDIR)/$(PKG_NAME).desktop
        rm -rf $(DESTDIR)$(DATADIR)/$(PKG_NAME)
        @echo "Removed from $(PREFIX)."

# ─── Source distribution ─────────────────────────────────────────────────────
# A versioned tarball suitable for downstream packagers (Debian, Arch, Fedora,
# SourceMage, …) to consume via their own packaging scripts.

dist: clean
        @echo "Building source tarball for v$(VERSION)..."
        @mkdir -p dist
        @tar -cJf dist/$(PKG_NAME)-$(VERSION).tar.xz \
                --exclude-vcs \
                --exclude=target \
                --exclude=dist \
                --exclude=.git \
                .
        @echo "Created: dist/$(PKG_NAME)-$(VERSION).tar.xz"
        @ls -lh dist/$(PKG_NAME)-$(VERSION).tar.xz

# ─── Optional distro-specific packaging ──────────────────────────────────────
# These are gated on the corresponding cargo subcommand being installed,
# so the default build path stays distro-agnostic.

deb:
        @command -v cargo-deb >/dev/null 2>&1 || { \
                echo "Error: cargo-deb not installed. Install with: cargo install cargo-deb"; \
                exit 1; \
        }
        $(CARGO) deb

rpm:
        @command -v cargo-rpm >/dev/null 2>&1 || { \
                echo "Error: cargo-rpm not installed. Install with: cargo install cargo-rpm"; \
                exit 1; \
        }
        $(CARGO) rpm

# ─── Convenience ─────────────────────────────────────────────────────────────

.PHONY: all
all: build test

.PHONY: ci
ci: fmt clippy test stress
        @echo "All CI checks passed."
