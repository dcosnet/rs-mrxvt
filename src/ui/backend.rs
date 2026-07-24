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


//! Backend selection: which `Renderer` should we use?
//!
//! Selection happens at startup, in this order:
//!
//! 1. **CLI flag** (`--backend {auto,tui,wgpu,soft}`) — explicit override.
//! 2. **`MRXVT_BACKEND` env var** — same effect, useful for tests/wrappers.
//! 3. **Auto-detect** — structured GPU probe → softbuffer → TUI.
//!
//! ## GPU detection
//!
//! When the `gpu` feature is enabled, auto-detect runs [`gpu_detect::GpuDetect`]
//! which probes each wgpu backend (Vulkan → Metal → DX12 → GL), logs what it
//! finds, and records *why* each step succeeded or failed. The full probe
//! result is stored in [`LAST_GPU_PROBE`] so `--gpu-info` can print it later.
//!
//! ## The "VESA mode" fallback
//!
//! Classic VESA VBE was a CPU-driven linear framebuffer with no acceleration.
//! The modern equivalent is [`softbuffer`](https://crates.io/crates/softbuffer)
//! (a CPU pixel buffer that the compositor displays) paired with
//! [`tiny-skia`](https://crates.io/crates/tiny-skia) for rasterization.
//!
//! This is the third tier in our chain: when no GPU is available but a
//! display server exists, the softbuffer renderer gives you a real window
//! with software-rendered glyphs. It's slower than wgpu but works on any
//! hardware — including the kind of retro box where you'd still be running
//! VESA drivers.

use std::sync::OnceLock;

use anyhow::Result;
use clap::ValueEnum;

use crate::app::App;

use super::Renderer;

/// Which backend to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum Backend {
    /// Auto-detect: try wgpu → soft → tui.
    #[default]
    Auto,
    /// Always use the TUI renderer (ratatui + crossterm).
    Tui,
    /// Use the wgpu renderer (Vulkan, Metal, DX12, or GL).
    ///
    /// Only available when built with `--features gpu`.
    Wgpu,
    /// Use the software rasterizer (softbuffer + tiny-skia).
    ///
    /// Only available when built with `--features gpu`.
    Soft,
}

impl Backend {
    /// Resolve `Auto` to a concrete backend by probing.
    ///
    /// This runs the full structured GPU detection (if gpu feature enabled)
    /// and logs each step of the fallback chain.
    ///
    /// `gpu_config` is passed so the detection can respect the user's
    /// `[gpu]` config section (preferred backend order, software rasterizer
    /// acceptance, etc.).
    pub fn resolve(self, gpu_config: &crate::config::GpuConfig) -> Backend {
        match self {
            Backend::Auto => auto_detect(gpu_config),
            other => {
                log::info!("backend explicitly set to: {other:?}");
                other
            }
        }
    }

    /// Returns `true` if this backend requires the `gpu` cargo feature.
    pub fn needs_gpu_feature(self) -> bool {
        matches!(self, Backend::Wgpu | Backend::Soft)
    }
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auto => write!(f, "auto"),
            Self::Tui => write!(f, "tui"),
            Self::Wgpu => write!(f, "wgpu"),
            Self::Soft => write!(f, "soft"),
        }
    }
}

// ─── Global GPU probe result ─────────────────────────────────────────────────
//
// Stored once at startup so that `--gpu-info` and the wgpu renderer can
// access the detection results without re-probing.

/// The last GPU probe result. Set during `auto_detect()` or when `--gpu-info`
/// triggers an explicit probe. `None` if probing hasn't happened yet.
pub static LAST_GPU_PROBE: OnceLock<GpuProbeOutcome> = OnceLock::new();

/// A simplified probe outcome for the global cache.
/// Uses `Box` to keep the type small.
#[derive(Debug, Clone)]
pub struct GpuProbeOutcome {
    pub available: bool,
    pub summary: String,
    #[cfg(feature = "gpu")]
    pub backend: Option<crate::ui::gpu_detect::GpuBackendType>,
    #[cfg(feature = "gpu")]
    pub adapter_name: Option<String>,
    #[cfg(feature = "gpu")]
    pub device_type: Option<crate::ui::gpu_detect::GpuDeviceType>,
}

/// Probe the system for the best available backend.
///
/// ## Fallback chain (with logging)
///
/// 1. **wgpu** — Run [`gpu_detect::GpuDetect::probe_with_options()`].
///    If an adapter is found, log the GPU name, backend, and device type.
///    If not, log the specific reasons (no Vulkan runtime, device creation
///    failed, software rasterizer rejected, etc.).
///
/// 2. **softbuffer** — Check for a display server ($DISPLAY or
///    $WAYLAND_DISPLAY). Log which variable was found. If neither is set,
///    log that no display server is reachable (headless/SSH).
///
/// 3. **TUI** — Always available. Log that we're falling back to the
///    terminal-based renderer.
///
/// The probe result is cached in [`LAST_GPU_PROBE`] for later access.
pub fn auto_detect(gpu_config: &crate::config::GpuConfig) -> Backend {
    log::info!("=== Backend auto-detect starting ===");
    log::info!("fallback chain: wgpu (GPU) → softbuffer (CPU raster) → tui (terminal)");

    // `gpu_config` is only consumed when the `gpu` feature is enabled (it
    // drives `GpuDetectOptions::from_config`). Touch it here so the default
    // (gpu-less) build doesn't warn about an unused parameter — the param
    // is part of the public API and must stay regardless of features.
    let _ = gpu_config;

    #[cfg(feature = "gpu")]
    {
        // ── Tier 1: wgpu (GPU-accelerated) ──────────────────────────────────
        log::info!("--- probing tier 1: wgpu (GPU) ---");

        let gpu_opts = crate::ui::gpu_detect::GpuDetectOptions::from_config(
            gpu_config,
        );
        let probe = crate::ui::gpu_detect::GpuDetect::probe_with_options(gpu_opts);

        let outcome = match &probe {
            crate::ui::gpu_detect::GpuProbeResult::Available { adapter_info, .. } => {
                GpuProbeOutcome {
                    available: true,
                    summary: probe.summary(),
                    backend: Some(adapter_info.backend),
                    adapter_name: Some(adapter_info.name.clone()),
                    device_type: Some(adapter_info.device_type),
                }
            }
            crate::ui::gpu_detect::GpuProbeResult::Unavailable { .. } => {
                GpuProbeOutcome {
                    available: false,
                    summary: probe.summary(),
                    backend: None,
                    adapter_name: None,
                    device_type: None,
                }
            }
        };

        // Cache the result for --gpu-info and renderer init.
        if LAST_GPU_PROBE.set(outcome).is_err() {
            log::debug!("GPU probe result already cached; skipping overwrite");
        }

        if let crate::ui::gpu_detect::GpuProbeResult::Available { adapter_info, .. } = &probe {
            log::info!(
                "✓ tier 1 selected: wgpu via {} ({}, {})",
                adapter_info.backend,
                adapter_info.device_type,
                adapter_info.name,
            );
            log::info!("=== Backend auto-detect complete: wgpu ===");
            return Backend::Wgpu;
        }

        log::warn!("✗ tier 1 (wgpu) unavailable — reasons logged above");
        log::info!("");

        // ── Tier 2: softbuffer (CPU rasterizer) ─────────────────────────────
        log::info!("--- probing tier 2: softbuffer (CPU rasterizer) ---");

        if probe_softbuffer_available() {
            log::info!(
                "✓ tier 2 selected: softbuffer (display server detected: {})",
                display_server_name()
            );
            log::info!("=== Backend auto-detect complete: softbuffer ===");
            return Backend::Soft;
        }

        log::warn!(
            "✗ tier 2 (softbuffer) unavailable — no display server detected ($DISPLAY and $WAYLAND_DISPLAY unset)"
        );
        log::info!("");
    }

    #[cfg(not(feature = "gpu"))]
    {
        log::info!("gpu feature not enabled at compile time — skipping wgpu and softbuffer tiers");
        log::info!("to enable GPU backends, rebuild with:  cargo build --features gpu");
        log::info!("or use the Makefile:  make build-gpu");
        log::info!("(requires system deps: libvulkan-dev, libwayland-dev, libxkbcommon-dev)");
        if LAST_GPU_PROBE.set(GpuProbeOutcome {
            available: false,
            summary: "gpu feature not compiled (--features gpu required)".into(),
        }).is_err() {
            log::debug!("GPU probe result already cached; skipping overwrite");
        }
    }

    // ── Tier 3: TUI (always available) ─────────────────────────────────────
    log::info!("--- falling back to tier 3: tui (ratatui + crossterm) ---");
    log::info!("✓ tier 3 selected: tui (always available, even over SSH)");
    log::info!("=== Backend auto-detect complete: tui ===");
    Backend::Tui
}

/// Returns a human-readable name for the detected display server protocol.
#[cfg(feature = "gpu")]
fn display_server_name() -> &'static str {
    if std::env::var("WAYLAND_DISPLAY").is_ok() {
        "Wayland"
    } else {
        "X11"
    }
}

/// A factory that constructs the chosen `Renderer`.
///
/// The trait lets us mock the probe in tests.
pub trait BackendFactory: Send + 'static {
    /// Returns `true` if this backend can run on the current system.
    fn available(&self) -> bool;

    /// Construct the renderer. Only called when `available()` returned `true`.
    fn create(&self) -> Result<Box<dyn Renderer>>;
}

/// Walks a list of factories and returns the first one that reports available.
pub struct BackendRegistry {
    factories: Vec<(&'static str, Box<dyn BackendFactory>)>,
}

impl BackendRegistry {
    pub fn new() -> Self {
        Self { factories: Vec::new() }
    }

    /// Register a factory. Order matters: earlier = higher priority.
    pub fn register<F: BackendFactory + 'static>(&mut self, name: &'static str, factory: F) {
        self.factories.push((name, Box::new(factory)));
    }

    /// Returns the name of the first available factory, or `None`.
    pub fn probe(&self) -> Option<&'static str> {
        self.factories.iter().find(|(_, f)| f.available()).map(|(name, _)| *name)
    }

    /// Construct the first available renderer. Returns `Err` if none is
    /// available (which should be impossible because the TUI factory is
    /// always available).
    pub fn create(&self) -> Result<Box<dyn Renderer>> {
        self.factories.iter()
            .find(|(_, f)| f.available())
            .ok_or_else(|| anyhow::anyhow!("no backend available (not even TUI - this should be impossible)"))
            .and_then(|(_, f)| f.create())
    }

    /// Number of registered factories.
    pub fn len(&self) -> usize {
        self.factories.len()
    }

    /// Is the registry empty?
    pub fn is_empty(&self) -> bool {
        self.factories.is_empty()
    }

    /// Names of all registered factories, in priority order.
    pub fn names(&self) -> Vec<&'static str> {
        self.factories.iter().map(|(n, _)| *n).collect()
    }
}

impl Default for BackendRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Runtime probes ──────────────────────────────────────────────────────────
//
// These are gated on the `gpu` feature. When the feature is off, the registry
// only contains the TUI factory and `auto_detect()` returns `Tui` unconditionally.

/// Fast-path probe: is *any* wgpu adapter available?
///
/// This is used by the factory's `available()` method. It does NOT run
/// the full structured probe — it just checks if an adapter exists at all.
/// The full probe (with logging, capability checks, etc.) runs only once
/// during `auto_detect()`.
#[cfg(feature = "gpu")]
fn probe_wgpu_available() -> bool {
    crate::ui::gpu_detect::GpuDetect::is_available()
}

/// Check if a display server is reachable (for softbuffer).
///
/// Heuristic: `$DISPLAY` is set (X11) or `$WAYLAND_DISPLAY` is set (Wayland).
/// This doesn't actually create a window — it just checks if the environment
/// suggests a compositor is running.
#[cfg(feature = "gpu")]
fn probe_softbuffer_available() -> bool {
    if std::env::var("DISPLAY").is_ok() {
        log::info!("  $DISPLAY is set — X11 display server detected");
        return true;
    }
    if std::env::var("WAYLAND_DISPLAY").is_ok() {
        log::info!("  $WAYLAND_DISPLAY is set — Wayland display server detected");
        return true;
    }
    log::info!("  $DISPLAY and $WAYLAND_DISPLAY both unset — no display server");
    false
}

#[cfg(feature = "gpu")]
pub(crate) fn probe_wgpu_available_pub() -> bool {
    probe_wgpu_available()
}

#[cfg(feature = "gpu")]
pub(crate) fn probe_softbuffer_available_pub() -> bool {
    probe_softbuffer_available()
}

// ─── Default registry ────────────────────────────────────────────────────────

/// Build the default backend registry for this build.
///
/// When the `gpu` feature is enabled, registers wgpu → soft → tui in that order.
/// When it's disabled, registers only tui.
pub fn default_registry() -> BackendRegistry {
    let mut reg = BackendRegistry::new();

    #[cfg(feature = "gpu")]
    {
        reg.register("wgpu", crate::ui::wgpu::WgpuFactory);
        reg.register("soft", crate::ui::soft::SoftFactory);
    }

    reg.register("tui", crate::ui::tui::TuiFactory);
    reg
}

/// Construct a renderer by backend name.
///
/// Looks up the factory by name and calls `create()`. If the factory isn't
/// registered (e.g. user passed `--backend wgpu` but the `gpu` feature is off),
/// returns an error.
pub fn create_by_name(name: &str) -> Result<Box<dyn Renderer>> {
    let reg = default_registry();
    // Bind the lookup to a local so the temporary iterator returned by
    // `factories_iter()` is dropped at the end of this statement — before
    // `reg` is dropped at the end of the function. Without this, the tail-
    // expression temporary would outlive `reg` and trip E0597.
    let (_, factory) = reg.factories_iter()
        .find(|(n, _)| *n == name)
        .ok_or_else(|| anyhow::anyhow!("unknown backend '{name}' (registered: {})", reg.names().join(", ")))?;
    if factory.available() {
        factory.create()
    } else {
        anyhow::bail!("backend '{name}' is not available on this system")
    }
}

// Expose the factories vec for `create_by_name`. We hide this behind a method
// to keep the public API clean.
impl BackendRegistry {
    fn factories_iter(&self) -> impl Iterator<Item = &(&'static str, Box<dyn BackendFactory>)> {
        self.factories.iter()
    }
}

/// Run the app with the given backend, falling back to TUI on error.
///
/// If the backend is `Auto`, this runs the full detection chain first.
/// When wgpu is selected, the wgpu renderer reads [`LAST_GPU_PROBE`] to
/// log which adapter it actually uses.
pub fn run_with_backend(app: &mut App, backend: Backend) -> Result<i32> {
    let resolved = backend.resolve(&app.config.gpu);
    log::info!("final backend selection: {resolved}");

    let renderer: Box<dyn Renderer> = match resolved {
        Backend::Tui => Box::new(crate::ui::TuiRenderer::new()?),
        #[cfg(feature = "gpu")]
        Backend::Wgpu => {
            // Log the cached probe info for the wgpu renderer.
            if let Some(probe) = LAST_GPU_PROBE.get() {
                if let (Some(ref name), Some(ref dt)) = (&probe.adapter_name, &probe.device_type) {
                    log::info!("wgpu renderer initializing with adapter: {} ({})", name, dt);
                }
            }
            Box::new(crate::ui::wgpu::WgpuRenderer::new()?)
        }
        #[cfg(feature = "gpu")]
        Backend::Soft => Box::new(crate::ui::soft::SoftRenderer::new()?),
        #[cfg(not(feature = "gpu"))]
        Backend::Wgpu | Backend::Soft => {
            anyhow::bail!("backend '{resolved}' requires building with --features gpu");
        }
        Backend::Auto => {
            anyhow::bail!("Backend::Auto was not resolved before run_with_backend")
        }
    };

    app.run(renderer)
}

/// Print GPU detection info and exit. Used by `--gpu-info`.
///
/// Runs the full structured probe (even if auto_detect hasn't been called yet),
/// prints the summary, and exits.
pub fn print_gpu_info_and_exit() {
    #[cfg(feature = "gpu")]
    {
        println!("rs-mrxvt GPU detection report");
        println!("================================\n");

        // Run the full probe.
        let opts = crate::ui::gpu_detect::GpuDetectOptions::default();
        let probe = crate::ui::gpu_detect::GpuDetect::probe_with_options(opts);
        println!("{}", probe.summary());

        // Also check softbuffer availability.
        println!("\nSoftbuffer (CPU rasterizer):");
        if probe_softbuffer_available() {
            println!("  Available: yes ({})", display_server_name());
        } else {
            println!("  Available: no (no display server detected)");
        }

        // Always-available fallback.
        println!("\nTUI (ratatui + crossterm):");
        println!("  Available: always");

        println!("\nFallback chain: wgpu → softbuffer → tui");
    }

    #[cfg(not(feature = "gpu"))]
    {
        println!("rs-mrxvt GPU detection report");
        println!("================================\n");
        println!("GPU feature not compiled into this build.");
        println!("Recompile with --features gpu to enable wgpu and softbuffer backends.");
        println!("\nAvailable backends: tui only");
    }
}

// ─── Mock factory for tests ──────────────────────────────────────────────────

#[cfg(test)]
pub mod test_support {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// A mock factory whose `available()` is controlled by a flag.
    pub struct MockFactory {
        pub name: &'static str,
        pub available: AtomicBool,
    }

    impl MockFactory {
        pub fn new(name: &'static str, available: bool) -> Self {
            Self {
                name,
                available: AtomicBool::new(available),
            }
        }

        pub fn set(&self, v: bool) {
            self.available.store(v, Ordering::SeqCst);
        }
    }

    impl BackendFactory for MockFactory {
        fn available(&self) -> bool {
            self.available.load(Ordering::SeqCst)
        }
        fn create(&self) -> Result<Box<dyn Renderer>> {
            Ok(Box::new(crate::ui::mock::MockRenderer::new(self.name)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::test_support::*;

    #[test]
    fn auto_resolves_to_concrete() {
        let cfg = crate::config::GpuConfig::default();
        let b = Backend::Auto.resolve(&cfg);
        // Without gpu feature: always Tui. With gpu feature: depends on system.
        // Just assert it resolved to a known variant.
        assert!(matches!(b, Backend::Tui | Backend::Wgpu | Backend::Soft));
    }

    #[test]
    fn explicit_backend_resolves_to_itself() {
        let cfg = crate::config::GpuConfig::default();
        assert_eq!(Backend::Tui.resolve(&cfg), Backend::Tui);
        assert_eq!(Backend::Wgpu.resolve(&cfg), Backend::Wgpu);
        assert_eq!(Backend::Soft.resolve(&cfg), Backend::Soft);
    }

    #[test]
    fn needs_gpu_feature_classification() {
        assert!(!Backend::Tui.needs_gpu_feature());
        assert!(!Backend::Auto.needs_gpu_feature());
        assert!(Backend::Wgpu.needs_gpu_feature());
        assert!(Backend::Soft.needs_gpu_feature());
    }

    #[test]
    fn registry_returns_first_available() {
        let mut reg = BackendRegistry::new();
        reg.register("wgpu", MockFactory::new("wgpu", false));
        reg.register("soft", MockFactory::new("soft", true));
        reg.register("tui", MockFactory::new("tui", true));

        assert_eq!(reg.probe(), Some("soft"));
    }

    #[test]
    fn registry_falls_through_when_none_available() {
        let mut reg = BackendRegistry::new();
        reg.register("wgpu", MockFactory::new("wgpu", false));
        reg.register("soft", MockFactory::new("soft", false));
        reg.register("tui", MockFactory::new("tui", true));

        assert_eq!(reg.probe(), Some("tui"));
    }

    #[test]
    fn registry_returns_none_if_all_unavailable() {
        let mut reg = BackendRegistry::new();
        reg.register("wgpu", MockFactory::new("wgpu", false));
        reg.register("soft", MockFactory::new("soft", false));
        // No tui fallback in this test.
        assert_eq!(reg.probe(), None);
    }

    #[test]
    fn default_registry_always_has_tui() {
        let reg = default_registry();
        let names = reg.names();
        assert!(names.contains(&"tui"));
        // TUI is always last (lowest priority).
        assert_eq!(*names.last().unwrap(), "tui");
    }

    #[test]
    fn default_registry_includes_gpu_when_feature_enabled() {
        let reg = default_registry();
        let names = reg.names();
        #[cfg(feature = "gpu")]
        {
            assert!(names.contains(&"wgpu"));
            assert!(names.contains(&"soft"));
            // Order: wgpu first, then soft, then tui.
            assert_eq!(names[0], "wgpu");
            assert_eq!(names[1], "soft");
        }
        #[cfg(not(feature = "gpu"))]
        {
            assert!(!names.contains(&"wgpu"));
            assert!(!names.contains(&"soft"));
        }
    }

    #[test]
    fn backend_display_round_trip() {
        assert_eq!(Backend::Auto.to_string(), "auto");
        assert_eq!(Backend::Tui.to_string(), "tui");
        assert_eq!(Backend::Wgpu.to_string(), "wgpu");
        assert_eq!(Backend::Soft.to_string(), "soft");
    }
}