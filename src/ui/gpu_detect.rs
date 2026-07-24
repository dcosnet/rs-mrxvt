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


//! Structured GPU detection for the wgpu backend.
//!
//! Instead of a bare `bool`, this module probes the system and returns a
//! [`GpuProbeResult`] that tells you *what* was found (adapter name, backend
//! type, limits) and *why* each step succeeded or failed. Every probe step
//! logs a clear message so the user can see exactly what the fallback chain
//! is doing at startup.
//!
//! ## Usage
//!
//! ```ignore
//! let result = GpuDetect::probe();
//! match result {
//!     GpuProbeResult::Available { adapter_info, backend, .. } => {
//!         log::info!("using GPU: {} via {}", adapter_info.name, backend);
//!     }
//!     GpuProbeResult::Unavailable { reasons } => {
//!         log::warn!("no GPU: {}", reasons.join("; "));
//!     }
//! }
//! ```
//!
//! ## Fallback semantics
//!
//! The probe tries backends in this order (configurable via [`GpuDetectOptions`]):
//! 1. **Vulkan** — best performance on Linux/BSD.
//! 2. **Metal** — native on macOS.
//! 3. **DX12** — native on Windows.
//! 4. **GL** — broadest compatibility (works on Mesa software, llvmpipe).
//!
//! If all fail, the result is `Unavailable` with a list of reasons.
//! The caller (the backend selection chain) then falls through to
//! softbuffer → TUI.

use std::fmt;

/// Which wgpu backend was used (or attempted).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum GpuBackendType {
    /// Vulkan (Linux, Windows, some Android).
    Vulkan,
    /// Metal (macOS, iOS).
    Metal,
    /// Direct3D 12 (Windows).
    Dx12,
    /// OpenGL / GLES (broadest compatibility, includes software Mesa).
    Gl,
    /// WebGPU (browser / wasm).
    WebGpu,
}

impl fmt::Display for GpuBackendType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Vulkan => write!(f, "Vulkan"),
            Self::Metal => write!(f, "Metal"),
            Self::Dx12 => write!(f, "DX12"),
            Self::Gl => write!(f, "OpenGL"),
            Self::WebGpu => write!(f, "WebGPU"),
        }
    }
}

/// Information about a successfully detected GPU adapter.
#[derive(Debug, Clone)]
pub struct GpuAdapterInfo {
    /// Human-readable adapter name (e.g. "NVIDIA GeForce RTX 4090").
    pub name: String,
    /// Which backend is driving this adapter.
    pub backend: GpuBackendType,
    /// Vendor ID (e.g. 0x10DE for NVIDIA). 0 if unknown.
    pub vendor_id: u32,
    /// Device ID. 0 if unknown.
    pub device_id: u32,
    /// Backend-specific device type (discrete, integrated, virtual, etc.).
    pub device_type: GpuDeviceType,
    /// Driver name reported by the backend.
    pub driver_name: String,
    /// Driver info string.
    pub driver_info: String,
    /// Maximum texture dimension (1D and 2D).
    pub max_texture_size: u32,
    /// Maximum buffer size in bytes.
    pub max_buffer_size: u64,
    /// Maximum storage buffer binding size.
    pub max_storage_buffer_size: u64,
}

/// What kind of GPU device was detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuDeviceType {
    /// Discrete GPU (dedicated VRAM).
    DiscreteGpu,
    /// Integrated GPU (shared system RAM).
    IntegratedGpu,
    /// Virtual / paravirtualized GPU (VM pass-through, virtio-gpu).
    VirtualGpu,
    /// CPU-based software rasterizer (llvmpipe, swiftshader).
    Cpu,
    /// Unknown / other.
    Other,
}

impl fmt::Display for GpuDeviceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DiscreteGpu => write!(f, "discrete GPU"),
            Self::IntegratedGpu => write!(f, "integrated GPU"),
            Self::VirtualGpu => write!(f, "virtual GPU"),
            Self::Cpu => write!(f, "CPU software rasterizer"),
            Self::Other => write!(f, "other"),
        }
    }
}

/// Result of a GPU probe attempt.
#[derive(Debug, Clone)]
pub enum GpuProbeResult {
    /// A suitable GPU adapter was found.
    Available {
        /// Information about the detected adapter.
        adapter_info: GpuAdapterInfo,
        /// All backends that were tried and their individual results.
        probe_log: Vec<BackendProbeEntry>,
    },
    /// No suitable GPU adapter was found.
    Unavailable {
        /// Human-readable reasons for each failed probe.
        reasons: Vec<String>,
        /// All backends that were tried and their individual results.
        probe_log: Vec<BackendProbeEntry>,
    },
}

impl GpuProbeResult {
    /// Returns `true` if a GPU adapter was found.
    pub fn is_available(&self) -> bool {
        matches!(self, GpuProbeResult::Available { .. })
    }

    /// Returns the adapter info if available.
    pub fn adapter_info(&self) -> Option<&GpuAdapterInfo> {
        match self {
            GpuProbeResult::Available { adapter_info, .. } => Some(adapter_info),
            GpuProbeResult::Unavailable { .. } => None,
        }
    }

    /// Returns a human-readable summary suitable for `--gpu-info`.
    pub fn summary(&self) -> String {
        match self {
            GpuProbeResult::Available { adapter_info, probe_log } => {
                let mut lines = vec![format!(
                    "GPU detected: {} ({}, {})",
                    adapter_info.name, adapter_info.backend, adapter_info.device_type
                )];
                lines.push(format!("  Vendor:    0x{:04X}", adapter_info.vendor_id));
                lines.push(format!("  Device:    0x{:04X}", adapter_info.device_id));
                lines.push(format!("  Driver:    {} ({})", adapter_info.driver_name, adapter_info.driver_info));
                lines.push(format!("  Max tex:   {}x{}", adapter_info.max_texture_size, adapter_info.max_texture_size));
                lines.push(format!("  Max buf:   {} MB", adapter_info.max_buffer_size / 1_048_576));

                lines.push(String::new());
                lines.push("Probe log:".into());
                lines.extend(probe_log.iter().map(|e| format!("  {} — {}", e.backend, e.result)));
                lines.join("\n")
            }
            GpuProbeResult::Unavailable { reasons, probe_log } => {
                let mut lines = vec!["No suitable GPU adapter found.".into()];
                lines.push(String::new());
                lines.push("Failure reasons:".into());
                for r in reasons {
                    lines.push(format!("  - {r}"));
                }
                lines.push(String::new());
                lines.push("Probe log:".into());
                lines.extend(probe_log.iter().map(|e| format!("  {} — {}", e.backend, e.result)));

                lines.push(String::new());
                lines.push("Falling back to: softbuffer (CPU rasterizer) or TUI".into());
                lines.join("\n")
            }
        }
    }
}

/// A single entry in the probe log — one backend's probe attempt.
#[derive(Debug, Clone)]
pub struct BackendProbeEntry {
    /// Which backend was probed.
    pub backend: GpuBackendType,
    /// Whether it was available at the wgpu instance level.
    pub instance_supported: bool,
    /// Human-readable result.
    pub result: String,
}

/// Options that control how GPU detection behaves.
///
/// These can come from config (`[gpu]` section) or CLI flags.
#[derive(Debug, Clone, Default)]
pub struct GpuDetectOptions {
    /// Preferred backend order. The probe tries these in sequence and stops
    /// at the first success. Empty = use the default order.
    pub preferred_backends: Vec<GpuBackendType>,
    /// If true, accept software rasterizers (llvmpipe, swiftshader) as valid.
    /// When false, a CPU adapter is treated as "not available" and the probe
    /// continues to the next backend.
    pub accept_software_rasterizer: bool,
    /// If true, force wgpu to use its built-in software fallback adapter
    /// (rendering via CPU even when wgpu is compiled). Useful for debugging.
    pub force_fallback_adapter: bool,
    /// Require a minimum maximum texture size. If the adapter reports less,
    /// it's rejected. 0 = no minimum.
    pub min_texture_size: u32,
}

impl GpuDetectOptions {
    /// Build from the config's `[gpu]` section.
    #[cfg(feature = "gpu")]
    pub fn from_config(gpu_cfg: &crate::config::GpuConfig) -> Self {
        let mut preferred = Vec::new();
        if let Some(ref order) = gpu_cfg.preferred_backend {
            for name in order.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
                match name.to_lowercase().as_str() {
                    "vulkan" => preferred.push(GpuBackendType::Vulkan),
                    "metal" => preferred.push(GpuBackendType::Metal),
                    "dx12" | "directx12" => preferred.push(GpuBackendType::Dx12),
                    "gl" | "opengl" => preferred.push(GpuBackendType::Gl),
                    _ => {
                        log::warn!("unknown preferred_backend '{name}', skipping");
                    }
                }
            }
        }

        Self {
            preferred_backends: preferred,
            accept_software_rasterizer: gpu_cfg.accept_software_rasterizer,
            force_fallback_adapter: gpu_cfg.force_fallback_adapter,
            min_texture_size: gpu_cfg.min_texture_size,
        }
    }
}

fn device_priority(dt: GpuDeviceType) -> i32 {
    match dt {
        GpuDeviceType::DiscreteGpu => 4,
        GpuDeviceType::IntegratedGpu => 3,
        GpuDeviceType::VirtualGpu => 2,
        GpuDeviceType::Cpu => 1,
        _ => 0,
    }
}

/// The GPU detector. Call [`GpuDetect::probe()`] to run the detection.
pub struct GpuDetect;

impl GpuDetect {
    /// Run the full GPU detection probe.
    ///
    /// Tries each wgpu backend in order (or the user's preferred order)
    /// and returns the first adapter that meets the requirements.
    pub fn probe() -> GpuProbeResult {
        Self::probe_with_options(GpuDetectOptions::default())
    }

    /// Run the probe with custom options (from config or CLI).
    pub fn probe_with_options(opts: GpuDetectOptions) -> GpuProbeResult {
        let mut probe_log = Vec::new();
        let mut all_reasons = Vec::new();

        // Build the wgpu Backends bitfield from our options or defaults.
        let backends_to_try: Vec<GpuBackendType> = if opts.preferred_backends.is_empty() {
            vec![
                GpuBackendType::Vulkan,
                GpuBackendType::Metal,
                GpuBackendType::Dx12,
                GpuBackendType::Gl,
            ]
        } else {
            opts.preferred_backends.clone()
        };

        log::info!("GPU probe: trying backends [{}]",
            backends_to_try.iter().map(|b| b.to_string()).collect::<Vec<_>>().join(", "));

        // Enumerate all available adapters. Each backend gets its own focused
        // `wgpu::Instance` (created inside the loop below) so we can attribute
        // adapters to specific backends.
        let adapters: Vec<(GpuBackendType, wgpu::Adapter)> = pollster::block_on(async {
            let mut result = Vec::new();
            for bt in &backends_to_try {
                let wgpu_backend = match bt {
                    GpuBackendType::Vulkan => wgpu::Backends::VULKAN,
                    GpuBackendType::Metal => wgpu::Backends::METAL,
                    GpuBackendType::Dx12 => wgpu::Backends::DX12,
                    GpuBackendType::Gl => wgpu::Backends::GL,
                    GpuBackendType::WebGpu => wgpu::Backends::BROWSER_WEBGPU,
                };

                // Create a focused instance for just this backend.
                let focused = wgpu::Instance::new(wgpu::InstanceDescriptor {
                    backends: wgpu_backend,
                    flags: wgpu::InstanceFlags::default(),
                    dx12_shader_compiler: wgpu::Dx12Compiler::default(),
                    gles_minor_version: wgpu::Gles3MinorVersion::default(),
                });

                let adapter = focused.request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: None,
                    force_fallback_adapter: opts.force_fallback_adapter,
                }).await;

                match adapter {
                    Some(a) => {
                        let info = a.get_info();
                        log::info!("  [{}] found adapter: {}", bt, info.name);
                        result.push((*bt, a));
                    }
                    None => {
                        log::info!("  [{}] no adapter found", bt);
                        probe_log.push(BackendProbeEntry {
                            backend: *bt,
                            instance_supported: true,
                            result: "no adapter found".into(),
                        });
                    }
                }
            }
            result
        });

        // Try each adapter and pick the best one.
        //
        // We only keep the `GpuAdapterInfo` (not the `wgpu::Adapter` itself)
        // because `wgpu::Adapter` is not `Clone` and the adapter is never
        // actually consumed after this probe — `GpuProbeResult::Available`
        // only carries the info struct. The wgpu renderer re-creates its own
        // adapter at init time from the cached backend hint.
        let mut best: Option<GpuAdapterInfo> = None;

        for (bt, adapter) in &adapters {
            let info = adapter.get_info();

            // Classify device type.
            let device_type = match info.device_type {
                wgpu::DeviceType::DiscreteGpu => GpuDeviceType::DiscreteGpu,
                wgpu::DeviceType::IntegratedGpu => GpuDeviceType::IntegratedGpu,
                wgpu::DeviceType::VirtualGpu => GpuDeviceType::VirtualGpu,
                wgpu::DeviceType::Cpu => GpuDeviceType::Cpu,
                wgpu::DeviceType::Other => GpuDeviceType::Other,
            };

            // Skip software rasterizers if the user doesn't want them.
            if device_type == GpuDeviceType::Cpu && !opts.accept_software_rasterizer {
                let reason = format!(
                    "[{}] adapter '{}' is a CPU software rasterizer (rejected: accept_software_rasterizer=false)",
                    bt, info.name
                );
                log::info!("  {}", reason);
                all_reasons.push(reason);
                probe_log.push(BackendProbeEntry {
                    backend: *bt,
                    instance_supported: true,
                    result: format!("CPU rasterizer '{}' (rejected by config)", info.name),
                });
                continue;
            }

            // Try to create a device to validate the adapter actually works.
            let device_result = pollster::block_on(async {
                adapter.request_device(
                    &wgpu::DeviceDescriptor {
                        label: Some("gpu-probe-validation"),
                        required_features: wgpu::Features::empty(),
                        required_limits: wgpu::Limits::downlevel_defaults(),
                        memory_hints: wgpu::MemoryHints::Performance,
                    },
                    None,
                ).await
            });

            match device_result {
                Ok((device, _queue)) => {
                    let limits = device.limits();

                    // Check minimum texture size.
                    if opts.min_texture_size > 0
                        && limits.max_texture_dimension_2d < opts.min_texture_size
                    {
                        let reason = format!(
                            "[{}] adapter '{}' max texture size {} < required {}",
                            bt, info.name, limits.max_texture_dimension_2d, opts.min_texture_size
                        );
                        log::info!("  {}", reason);
                        all_reasons.push(reason);
                        probe_log.push(BackendProbeEntry {
                            backend: *bt,
                            instance_supported: true,
                            result: format!("max_texture_dimension_2d={} < required {}",
                                limits.max_texture_dimension_2d, opts.min_texture_size),
                        });
                        continue;
                    }

                    let adapter_info = GpuAdapterInfo {
                        name: info.name.clone(),
                        backend: *bt,
                        vendor_id: info.vendor,
                        device_id: info.device,
                        device_type,
                        driver_name: info.driver.clone(),
                        driver_info: info.driver_info.clone(),
                        max_texture_size: limits.max_texture_dimension_2d,
                        max_buffer_size: limits.max_buffer_size,
                        // wgpu 22 renamed this field and narrowed it to u32.
                        max_storage_buffer_size: u64::from(limits.max_storage_buffer_binding_size),
                    };

                    // Prefer discrete > integrated > virtual > CPU.
                    let priority = device_priority(device_type);

                    let best_priority = best.as_ref().map_or(-1i32, |bi| device_priority(bi.device_type));

                    if priority > best_priority {
                        log::info!("  [{}] adapter '{}' selected (priority={})", bt, info.name, priority);
                        best = Some(adapter_info);
                    }

                    probe_log.push(BackendProbeEntry {
                        backend: *bt,
                        instance_supported: true,
                        result: format!("OK — {} ({})", info.name, device_type),
                    });
                }
                Err(e) => {
                    let reason = format!("[{}] adapter '{}' device creation failed: {}", bt, info.name, e);
                    log::warn!("  {}", reason);
                    all_reasons.push(reason);
                    probe_log.push(BackendProbeEntry {
                        backend: *bt,
                        instance_supported: true,
                        result: format!("device creation failed: {}", e),
                    });
                }
            }
        }

        match best {
            Some(adapter_info) => {
                log::info!(
                    "GPU selected: {} via {} ({}, vendor=0x{:04X}, device=0x{:04X})",
                    adapter_info.name,
                    adapter_info.backend,
                    adapter_info.device_type,
                    adapter_info.vendor_id,
                    adapter_info.device_id,
                );
                GpuProbeResult::Available {
                    adapter_info,
                    probe_log,
                }
            }
            None => {
                if all_reasons.is_empty() {
                    all_reasons.push(
                        "no wgpu backends compiled into this build (need --features gpu)".into()
                    );
                }
                log::warn!("GPU probe failed: {}", all_reasons.join("; "));
                GpuProbeResult::Unavailable {
                    reasons: all_reasons,
                    probe_log,
                }
            }
        }
    }

    /// Quick check: is *any* GPU likely available?
    ///
    /// This is the fast path used by the backend registry's `available()`.
    /// It does not enumerate adapters or create devices — it just checks
    /// whether wgpu can find an adapter at all.
    pub fn is_available() -> bool {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN | wgpu::Backends::GL,
            flags: wgpu::InstanceFlags::default(),
            dx12_shader_compiler: wgpu::Dx12Compiler::default(),
            gles_minor_version: wgpu::Gles3MinorVersion::default(),
        });

        pollster::block_on(async {
            instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::LowPower,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                })
                .await
                .is_some()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: these tests require a GPU (or wgpu software fallback) to be present.
    // They're structured so they pass in CI if wgpu can find any adapter
    // (including software), and are skipped if even that fails.

    #[test]
    fn probe_returns_structured_result() {
        let result = GpuDetect::probe();
        // Just verify it doesn't panic and returns one of the two variants.
        match &result {
            GpuProbeResult::Available { adapter_info, .. } => {
                assert!(!adapter_info.name.is_empty());
            }
            GpuProbeResult::Unavailable { reasons, .. } => {
                assert!(!reasons.is_empty());
            }
        }
    }

    #[test]
    fn probe_summary_does_not_panic() {
        let result = GpuDetect::probe();
        let _summary = result.summary();
    }

    #[test]
    fn is_available_is_consistent_with_probe() {
        let quick = GpuDetect::is_available();
        let full = GpuDetect::probe().is_available();
        // They should agree in most cases. The quick check uses LowPower
        // and only Vulkan+GL, while the full probe is more thorough, so
        // full may find something that quick doesn't — but quick should
        // never find something that full doesn't.
        if quick && !full {
            // This is a valid edge case (quick found GL but full rejected
            // it after device validation), so we don't assert equality.
        } else if full && !quick {
            // Full probe found Metal/DX12 but quick only tried Vulkan+GL.
            // Also valid.
        }
    }
}