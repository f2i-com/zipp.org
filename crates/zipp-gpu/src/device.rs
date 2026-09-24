//! The native half of `js/webgpu-shim.js`: WebGPU objects as wgpu objects in
//! handle tables, driven by `__zippHostCall("gpu.*", ...)`.
//!
//! Validation stays wgpu's (it implements WebGPU's rules): a bad descriptor
//! or command lands in the innermost matching error scope the script pushed,
//! exactly where a browser reports it, and anything outside every scope goes
//! to a recorder instead of wgpu's default handler, which panics (and this
//! binary aborts on panic). Only what wgpu would reject by panicking rather
//! than by an error (a mapped-at-creation size that is not a multiple of four,
//! unknown usage bits) is refused here first, as the browser also refuses it
//! synchronously.
use std::collections::HashMap;
use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use zipp_vm::embed::HostCtx;

/// The WebGPU limits the shim exposes, by their WebGPU names.
macro_rules! limit_table {
    ($($js:literal => $field:ident),* $(,)?) => {
        fn limits_json(limits: &wgpu::Limits) -> String {
            let mut out = String::from("{");
            $(
                if out.len() > 1 { out.push(','); }
                out.push_str(&format!("\"{}\":{}", $js, limits.$field));
            )*
            out.push('}');
            out
        }
        /// Apply one `requiredLimits` entry; `false` for a name this table does not know.
        fn set_limit(limits: &mut wgpu::Limits, name: &str, value: f64) -> bool {
            match name {
                $($js => { limits.$field = value as _; true })*
                _ => false,
            }
        }
        /// Whether `value` for `name` is within what `adapter` supports.
        fn limit_supported(adapter: &wgpu::Limits, name: &str, value: f64) -> bool {
            match name {
                // "Alignment" limits are better when smaller.
                "minUniformBufferOffsetAlignment" => value >= adapter.min_uniform_buffer_offset_alignment as f64,
                "minStorageBufferOffsetAlignment" => value >= adapter.min_storage_buffer_offset_alignment as f64,
                $(#[allow(unreachable_patterns)] $js => value <= adapter.$field as f64,)*
                _ => false,
            }
        }
    };
}
limit_table! {
    "maxBindGroups" => max_bind_groups,
    "maxBindingsPerBindGroup" => max_bindings_per_bind_group,
    "maxDynamicUniformBuffersPerPipelineLayout" => max_dynamic_uniform_buffers_per_pipeline_layout,
    "maxDynamicStorageBuffersPerPipelineLayout" => max_dynamic_storage_buffers_per_pipeline_layout,
    "maxStorageBuffersPerShaderStage" => max_storage_buffers_per_shader_stage,
    "maxUniformBuffersPerShaderStage" => max_uniform_buffers_per_shader_stage,
    "maxUniformBufferBindingSize" => max_uniform_buffer_binding_size,
    "maxStorageBufferBindingSize" => max_storage_buffer_binding_size,
    "minUniformBufferOffsetAlignment" => min_uniform_buffer_offset_alignment,
    "minStorageBufferOffsetAlignment" => min_storage_buffer_offset_alignment,
    "maxBufferSize" => max_buffer_size,
    "maxComputeWorkgroupStorageSize" => max_compute_workgroup_storage_size,
    "maxComputeInvocationsPerWorkgroup" => max_compute_invocations_per_workgroup,
    "maxComputeWorkgroupSizeX" => max_compute_workgroup_size_x,
    "maxComputeWorkgroupSizeY" => max_compute_workgroup_size_y,
    "maxComputeWorkgroupSizeZ" => max_compute_workgroup_size_z,
    "maxComputeWorkgroupsPerDimension" => max_compute_workgroups_per_dimension,
}

/// The adapter a host found, as the shim's `requestAdapter` reports it.
#[derive(Debug, Clone)]
pub struct AdapterSummary {
    /// The adapter's name (`GPUAdapterInfo.description`).
    pub name: String,
    /// `vulkan`, `dx12`, `metal`.
    pub backend: String,
    /// The PCI vendor id, in hex (`GPUAdapterInfo.vendor`).
    pub vendor: String,
    /// The driver and its version.
    pub driver: String,
    /// `discrete`, `integrated`, `virtual`, `cpu` or `other`.
    pub device_type: String,
    /// A software rasterizer (WARP, lavapipe, SwiftShader): gpu-lab refuses it.
    pub is_fallback: bool,
    /// Direct3D 12's shader compiler, `dxc` or `fxc` (see [`find_adapter`]).
    pub shader_compiler: Option<String>,
}

impl AdapterSummary {
    fn of(info: &wgpu::AdapterInfo) -> Self {
        let device_type = match info.device_type {
            wgpu::DeviceType::DiscreteGpu => "discrete",
            wgpu::DeviceType::IntegratedGpu => "integrated",
            wgpu::DeviceType::VirtualGpu => "virtual",
            wgpu::DeviceType::Cpu => "cpu",
            wgpu::DeviceType::Other => "other",
        };
        let driver = [info.driver.as_str(), info.driver_info.as_str()]
            .iter()
            .filter(|s| !s.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" ");
        AdapterSummary {
            name: info.name.clone(),
            backend: info.backend.to_str().to_owned(),
            vendor: format!("{:#06x}", info.vendor),
            driver,
            device_type: device_type.to_owned(),
            is_fallback: info.device_type == wgpu::DeviceType::Cpu,
            shader_compiler: None,
        }
    }
    /// One line for a log: name, backend, driver (and D3D12's shader compiler).
    pub fn describe(&self) -> String {
        let mut text = format!("{} ({}", self.name, self.backend);
        if !self.driver.is_empty() {
            text.push_str(", ");
            text.push_str(&self.driver);
        }
        if let Some(compiler) = &self.shader_compiler {
            text.push_str(", ");
            text.push_str(compiler);
        }
        text.push(')');
        text
    }
}

/// Which native backends to try, in order.
pub fn backend_order(requested: Option<&str>) -> Result<Vec<wgpu::Backends>, String> {
    match requested.map(|s| s.trim().to_ascii_lowercase()) {
        None => Ok(default_order()),
        Some(s) if s.is_empty() || s == "auto" => Ok(default_order()),
        Some(s) => match s.as_str() {
            "vulkan" | "vk" => Ok(vec![wgpu::Backends::VULKAN]),
            "dx12" | "d3d12" => Ok(vec![wgpu::Backends::DX12]),
            "metal" | "mtl" => Ok(vec![wgpu::Backends::METAL]),
            "gl" | "gles" | "opengl" => Err(
                "ZIPP_GPU_BACKEND=gl: this build has no OpenGL backend (vulkan, dx12 or metal)"
                    .into(),
            ),
            other => Err(format!(
                "ZIPP_GPU_BACKEND={other}: expected auto, vulkan, dx12 or metal"
            )),
        },
    }
}

fn default_order() -> Vec<wgpu::Backends> {
    // Vulkan first where it exists; D3D12 next on Windows; Metal on Apple.
    vec![
        wgpu::Backends::VULKAN,
        wgpu::Backends::DX12,
        wgpu::Backends::METAL,
    ]
}

/// DXC (`dxcompiler.dll`) where the program's loader would find it: next to
/// the executable, then on PATH, when `ZIPP_GPU_DXC=1` asks for it. It is
/// seconds faster per large kernel than the system's FXC, but each compiler
/// rounds some kernels (fused multiply-adds, divisions) its own way, in the
/// last bits; FXC is always there, so by default Direct3D 12's results do not
/// depend on what else is on PATH.
fn find_dxc() -> Option<String> {
    if std::env::var_os("ZIPP_GPU_DXC").is_none_or(|v| v != "1") {
        return None;
    }
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| p.to_owned()))
    {
        dirs.push(dir);
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs.into_iter()
        .map(|d| d.join("dxcompiler.dll"))
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}

/// Find a hardware adapter. `Ok(None)`: no driver, or only a software one.
/// Direct3D 12 is tried with DXC first when [`find_dxc`] finds one (asked for
/// with `ZIPP_GPU_DXC=1`; with FXC if that instance offers no adapter), else
/// with FXC, unless `WGPU_DX12_COMPILER` says.
pub fn find_adapter(
    order: &[wgpu::Backends],
) -> Result<Option<(wgpu::Instance, wgpu::Adapter, Option<String>)>, String> {
    let mut attempts: Vec<(wgpu::Backends, Option<wgpu::Dx12Compiler>)> = Vec::new();
    for &backends in order {
        if backends == wgpu::Backends::DX12 && std::env::var_os("WGPU_DX12_COMPILER").is_none() {
            if let Some(dxc_path) = find_dxc() {
                attempts.push((backends, Some(wgpu::Dx12Compiler::DynamicDxc { dxc_path })));
            }
            attempts.push((backends, Some(wgpu::Dx12Compiler::Fxc)));
        } else {
            attempts.push((backends, None));
        }
    }
    for (backends, compiler) in attempts {
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        // No backend validation layers (they are slow and chatty); WGPU_*
        // environment variables still apply for debugging, except that the
        // backend is ZIPP_GPU_BACKEND's choice, not WGPU_BACKEND's.
        descriptor.flags = wgpu::InstanceFlags::empty();
        let mut descriptor = descriptor.with_env();
        descriptor.backends = backends;
        let name = match &compiler {
            Some(wgpu::Dx12Compiler::DynamicDxc { .. }) => Some("dxc".to_string()),
            Some(_) => Some("fxc".to_string()),
            None if backends == wgpu::Backends::DX12 => {
                Some("compiler per WGPU_DX12_COMPILER".to_string())
            }
            None => None,
        };
        if let Some(compiler) = compiler {
            descriptor.backend_options.dx12.shader_compiler = compiler;
        }
        let instance = wgpu::Instance::new(descriptor);
        let options = wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
            // Report the device's real limits: this is trusted code, and
            // gpu-lab sizes its tensors from them.
            apply_limit_buckets: false,
        };
        if let Ok(adapter) = pollster::block_on(instance.request_adapter(&options)) {
            if adapter.get_info().device_type != wgpu::DeviceType::Cpu {
                return Ok(Some((instance, adapter, name)));
            }
        }
    }
    Ok(None)
}

/// [`find_adapter`] on a thread of its own: loading the drivers is most of
/// the GPU's start-up (creating a Vulkan instance alone takes ~130 ms on an
/// NVIDIA driver), and the runtime's script compiles meanwhile.
pub struct Probe {
    thread: std::thread::JoinHandle<
        Result<Option<(wgpu::Instance, wgpu::Adapter, Option<String>)>, String>,
    >,
}

impl Probe {
    pub fn start(order: Vec<wgpu::Backends>) -> Result<Probe, String> {
        std::thread::Builder::new()
            .name("zipp-gpu-probe".into())
            .spawn(move || find_adapter(&order))
            .map(|thread| Probe { thread })
            .map_err(|e| format!("zipp-gpu: the adapter probe did not start: {e}"))
    }

    /// Wait for the probe. `Ok(None)`: no driver, or only a software one.
    pub fn finish(self) -> Result<Option<WebGpu>, String> {
        let found = self
            .thread
            .join()
            .map_err(|_| "zipp-gpu: the adapter probe failed".to_string())??;
        Ok(found.map(|(instance, adapter, compiler)| {
            let mut gpu = WebGpu::new(instance, adapter);
            gpu.summary.shader_compiler = compiler;
            gpu
        }))
    }
}

pub(crate) struct BufferEntry {
    pub(crate) buffer: wgpu::Buffer,
    pub(crate) size: u64,
}

/// Errors wgpu reports outside every scope the script pushed.
#[derive(Default)]
struct Recorded {
    uncaptured: Vec<String>,
    lost: Option<String>,
}

/// One requested device, its queue and its error-scope stack.
pub(crate) struct Dev {
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    scopes: Vec<wgpu::ErrorScopeGuard>,
    recorded: Arc<Mutex<Recorded>>,
}

/// One adapter and, once requested, its device: the objects behind the shim.
pub struct WebGpu {
    _instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    pub summary: AdapterSummary,
    pub(crate) devices: HashMap<u32, Dev>,
    /// The device the current call addresses (every call after
    /// `requestDevice` names one first).
    pub(crate) current: u32,
    next: u32,
    pub(crate) buffers: HashMap<u32, BufferEntry>,
    modules: HashMap<u32, wgpu::ShaderModule>,
    pub(crate) group_layouts: HashMap<u32, wgpu::BindGroupLayout>,
    pipeline_layouts: HashMap<u32, wgpu::PipelineLayout>,
    pub(crate) pipelines: HashMap<u32, wgpu::ComputePipeline>,
    groups: HashMap<u32, wgpu::BindGroup>,
    /// How each live bind group was made: its layout and its
    /// [binding, buffer, offset, size or -1] entries (a replay rebinds them).
    pub(crate) group_specs: HashMap<u32, (u32, Vec<(u32, u32, u64, i64)>)>,
    /// `gpu.capture`: command scripts finished while capturing (a replay's source).
    capturing: bool,
    pub(crate) captured: Vec<String>,
    /// Prepared sessions replayed natively, by the handler's session token.
    pub(crate) replays: HashMap<String, crate::replay::Replay>,
    commands: HashMap<u32, wgpu::CommandBuffer>,
    epoch: Instant,
    /// `ZIPP_GPU_PROFILE`: time spent in each host call kind (and blocked
    /// on the device inside them), printed to stderr when the host drops.
    profile: Option<Profile>,
}

/// A timestamped command buffer: readback and resolve buffers, the query
/// set, and each dispatch's (start query, end query, pipeline label).
type Timed = (
    wgpu::Buffer,
    wgpu::Buffer,
    wgpu::QuerySet,
    Vec<(u32, u32, String)>,
);

/// Where the shim's host calls spend their time (`ZIPP_GPU_PROFILE=1`, or
/// `=kernels` to add per-kernel GPU time from timestamp queries).
#[derive(Default)]
struct Profile {
    calls: HashMap<String, (u64, f64)>,
    /// Blocked in `device.poll` waiting for submitted work (map, queue drain).
    wait_ms: f64,
    waits: u64,
    dispatches: u64,
    passes: u64,
    /// With timestamp queries: GPU time per pipeline label (count, ms).
    kernels: HashMap<String, (u64, f64)>,
    labels: HashMap<u32, String>,
    /// Command buffers whose dispatches were timestamped: the readback
    /// buffer, the query set and each dispatch's (start, end, label).
    timed: HashMap<u32, Timed>,
    timestamps: bool,
    /// Native replays: runs, and ms per phase (see [`REPLAY_PHASES`]).
    replays: u64,
    replay_ms: [f64; REPLAY_PHASES.len()],
}

/// The phases of a replayed run the profile times.
pub(crate) const REPLAY_PHASES: [&str; 9] = [
    "check feeds",
    "begin (JS)",
    "write feeds+uniforms",
    "encode",
    "submit",
    "wait for the GPU",
    "read back",
    "end (JS)",
    "build reply",
];

impl WebGpu {
    /// Adds `ms` to replay phase `phase` (`ZIPP_GPU_PROFILE`); `run` counts a run.
    pub(crate) fn replay_phase(&mut self, phase: usize, ms: f64, run: bool) {
        if let Some(p) = self.profile.as_mut() {
            p.replay_ms[phase] += ms;
            if run {
                p.replays += 1;
            }
        }
    }
}

impl Drop for WebGpu {
    fn drop(&mut self) {
        if let Some(p) = &self.profile {
            let mut rows: Vec<_> = p.calls.iter().collect();
            rows.sort_by(|a, b| b.1 .1.partial_cmp(&a.1 .1).unwrap());
            eprintln!(
                "zipp-gpu profile: {} dispatches in {} passes; blocked on the device {:.1} ms in {} waits",
                p.dispatches, p.passes, p.wait_ms, p.waits
            );
            for (kind, (count, ms)) in rows {
                eprintln!("  {kind:<22} {count:>8} calls {ms:>10.1} ms");
            }
            if p.replays > 0 {
                eprintln!("zipp-gpu replays: {} runs, per run:", p.replays);
                for (name, ms) in REPLAY_PHASES.iter().zip(p.replay_ms) {
                    eprintln!("  {name:<22} {:>10.1} us", ms * 1000.0 / p.replays as f64);
                }
            }
            if !p.kernels.is_empty() {
                let mut rows: Vec<_> = p.kernels.iter().collect();
                rows.sort_by(|a, b| b.1 .1.partial_cmp(&a.1 .1).unwrap());
                let total: f64 = rows
                    .iter()
                    .filter(|r| !r.0.starts_with('('))
                    .map(|r| r.1 .1)
                    .sum();
                eprintln!("zipp-gpu kernels (timestamp queries): {total:.1} ms of GPU time");
                for (kind, (count, ms)) in rows {
                    eprintln!(
                        "  {kind:<22} {count:>8} dispatches {ms:>10.2} ms {:>8.1} us each",
                        ms * 1000.0 / *count as f64
                    );
                }
            }
        }
    }
}

pub(crate) fn num(args: &[String], i: usize) -> Result<f64, String> {
    args.get(i)
        .and_then(|s| s.parse::<f64>().ok())
        .ok_or_else(|| format!("TypeError: WebGPU host call argument {i} must be a number"))
}
pub(crate) fn int(args: &[String], i: usize) -> Result<u64, String> {
    let v = num(args, i)?;
    if v < 0.0 || v.fract() != 0.0 || v > 9_007_199_254_740_991.0 {
        return Err(format!(
            "TypeError: WebGPU host call argument {i} must be a non-negative integer"
        ));
    }
    Ok(v as u64)
}
pub(crate) fn id(args: &[String], i: usize) -> Result<u32, String> {
    let v = int(args, i)?;
    u32::try_from(v).map_err(|_| "TypeError: not a WebGPU object id".to_string())
}
fn missing(what: &str) -> String {
    format!("TypeError: unknown or destroyed WebGPU {what}")
}
pub(crate) fn error_text(error: &wgpu::Error) -> String {
    let mut text = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(inner) = source {
        let line = inner.to_string();
        if !text.contains(&line) {
            text.push_str(": ");
            text.push_str(&line);
        }
        source = inner.source();
    }
    text
}

impl WebGpu {
    pub fn new(instance: wgpu::Instance, adapter: wgpu::Adapter) -> Self {
        let summary = AdapterSummary::of(&adapter.get_info());
        WebGpu {
            _instance: instance,
            adapter,
            summary,
            devices: HashMap::new(),
            current: 0,
            next: 1,
            buffers: HashMap::new(),
            modules: HashMap::new(),
            group_layouts: HashMap::new(),
            pipeline_layouts: HashMap::new(),
            pipelines: HashMap::new(),
            groups: HashMap::new(),
            group_specs: HashMap::new(),
            capturing: false,
            captured: Vec::new(),
            replays: HashMap::new(),
            commands: HashMap::new(),
            epoch: Instant::now(),
            profile: std::env::var_os("ZIPP_GPU_PROFILE").map(|_| Profile::default()),
        }
    }

    /// Errors reported outside every error scope so far (taken).
    pub fn take_uncaptured(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        for dev in self.devices.values() {
            out.append(&mut dev.recorded.lock().unwrap().uncaptured);
        }
        out
    }

    fn fresh(&mut self) -> u32 {
        let id = self.next;
        self.next = self.next.wrapping_add(1).max(1);
        id
    }

    pub(crate) fn dev(&self) -> Result<&Dev, String> {
        self.devices
            .get(&self.current)
            .ok_or_else(|| "InvalidStateError: the WebGPU device has been destroyed".into())
    }
    pub(crate) fn device(&self) -> Result<&wgpu::Device, String> {
        self.dev().map(|d| &d.device)
    }
    fn queue(&self) -> Result<&wgpu::Queue, String> {
        self.dev().map(|d| &d.queue)
    }
    pub(crate) fn lost(&self) -> Option<String> {
        self.dev()
            .ok()
            .and_then(|d| d.recorded.lock().unwrap().lost.clone())
    }
    /// Block until the queue's submitted work (and every map callback) is done.
    fn wait(&mut self) -> Result<(), String> {
        let start = self.profile.as_ref().map(|_| Instant::now());
        let device = self.device()?;
        let done = device
            .poll(wgpu::PollType::wait_indefinitely())
            .map(|_| ())
            .map_err(|e| e.to_string());
        if let (Some(p), Some(start)) = (self.profile.as_mut(), start) {
            p.wait_ms += start.elapsed().as_secs_f64() * 1000.0;
            p.waits += 1;
        }
        done
    }

    /// Serve one `__zippHostCall("gpu.<kind>", ...)`.
    pub fn call(
        &mut self,
        ctx: &mut dyn HostCtx,
        kind: &str,
        args: &[String],
    ) -> Result<String, String> {
        if self.profile.is_none() {
            return self.serve(ctx, kind, args);
        }
        let start = Instant::now();
        let result = self.serve(ctx, kind, args);
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        if let Some(p) = self.profile.as_mut() {
            let name = match (kind, args.get(4)) {
                ("gpu.pipeline", Some(label)) => format!("gpu.pipeline {label}"),
                _ => kind.to_owned(),
            };
            let slot = p.calls.entry(name).or_default();
            slot.0 += 1;
            slot.1 += ms;
        }
        result
    }

    fn serve(
        &mut self,
        ctx: &mut dyn HostCtx,
        kind: &str,
        args: &[String],
    ) -> Result<String, String> {
        match kind {
            "gpu.now" => return Ok(format!("{}", self.epoch.elapsed().as_secs_f64() * 1000.0)),
            "gpu.requestAdapter" => return Ok(self.adapter_json()),
            "gpu.requestDevice" => return self.request_device(args),
            "gpu.allFinite" => {
                // Whether the first `n` float32 values in `__zgpuUp` are all
                // finite (js/accelerate.js: gpu-lab's float32Data and checkFiniteOutput).
                let n = int(args, 0)? as usize;
                let bytes = region(
                    ctx,
                    "__zgpuUp",
                    n.checked_mul(4).ok_or("RangeError: too long")?,
                )?;
                let finite = bytes[..n * 4]
                    .chunks_exact(4)
                    .all(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]).is_finite());
                return Ok(if finite { "1" } else { "0" }.into());
            }
            "gpu.allIndices" => {
                // Whether the first `n` float32 values in `__zgpuUp` are all
                // integers in [0, bound) (js/accelerate.js: gpu-lab's
                // checkIndices and checkClassTargets).
                let n = int(args, 0)? as usize;
                let bound = num(args, 1)?;
                let bytes = region(
                    ctx,
                    "__zgpuUp",
                    n.checked_mul(4).ok_or("RangeError: too long")?,
                )?;
                let ok = bytes[..n * 4].chunks_exact(4).all(|b| {
                    let v = f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64;
                    v.fract() == 0.0 && v >= 0.0 && v < bound
                });
                return Ok(if ok { "1" } else { "0" }.into());
            }
            _ => {}
        }
        self.current = id(args, 0)?;
        let args = &args[1..];
        match kind {
            "gpu.buffer" => self.create_buffer(args),
            "gpu.bufferDestroy" => {
                if let Some(entry) = self.buffers.remove(&id(args, 0)?) {
                    entry.buffer.destroy();
                }
                Ok(String::new())
            }
            "gpu.bufferUnmap" => self.unmap(ctx, args),
            "gpu.bufferMap" => self.map_read(ctx, args),
            "gpu.writeBuffer" => self.write_buffer(ctx, args),
            "gpu.shader" => {
                let code = args.first().ok_or("TypeError: shader code is required")?;
                let module = self
                    .device()?
                    .create_shader_module(wgpu::ShaderModuleDescriptor {
                        label: None,
                        source: wgpu::ShaderSource::Wgsl(code.as_str().into()),
                    });
                let id = self.fresh();
                self.modules.insert(id, module);
                Ok(id.to_string())
            }
            "gpu.bindGroupLayout" => self.bind_group_layout(args),
            "gpu.pipelineLayout" => {
                let mut layouts = Vec::new();
                for part in args.first().map(String::as_str).unwrap_or("").split(',') {
                    if part.is_empty() {
                        continue;
                    }
                    let key: u32 = part.parse().map_err(|_| missing("bind group layout"))?;
                    layouts.push(
                        self.group_layouts
                            .get(&key)
                            .ok_or_else(|| missing("bind group layout"))?,
                    );
                }
                let layout =
                    self.device()?
                        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                            label: None,
                            bind_group_layouts: &layouts
                                .iter()
                                .map(|l| Some(*l))
                                .collect::<Vec<_>>(),
                            immediate_size: 0,
                        });
                let id = self.fresh();
                self.pipeline_layouts.insert(id, layout);
                Ok(id.to_string())
            }
            "gpu.pipeline" => self.pipeline(args),
            "gpu.bindGroup" => self.bind_group(args),
            "gpu.finish" => self.finish(args),
            "gpu.submit" => {
                let mut list = Vec::new();
                for part in args.first().map(String::as_str).unwrap_or("").split(',') {
                    if part.is_empty() {
                        continue;
                    }
                    let key: u32 = part.parse().map_err(|_| missing("command buffer"))?;
                    // A command buffer is submitted at most once (WebGPU makes a
                    // second submission a validation error; here it is unknown).
                    list.push(
                        self.commands
                            .remove(&key)
                            .ok_or_else(|| missing("command buffer"))?,
                    );
                }
                self.queue()?.submit(list);
                if self.profile.as_ref().is_some_and(|p| !p.timed.is_empty()) {
                    self.collect_timestamps(args.first().map(String::as_str).unwrap_or(""))?;
                }
                Ok(String::new())
            }
            "gpu.done" => {
                self.wait()?;
                Ok(self.lost().map(|m| format!("LOST:{m}")).unwrap_or_default())
            }
            "gpu.pushErrorScope" => {
                let filter = match args.first().map(String::as_str) {
                    Some("validation") => wgpu::ErrorFilter::Validation,
                    Some("out-of-memory") => wgpu::ErrorFilter::OutOfMemory,
                    Some("internal") => wgpu::ErrorFilter::Internal,
                    _ => return Err("TypeError: unknown error filter".into()),
                };
                let dev = self
                    .devices
                    .get_mut(&self.current)
                    .ok_or_else(|| missing("device"))?;
                let guard = dev.device.push_error_scope(filter);
                dev.scopes.push(guard);
                Ok(String::new())
            }
            "gpu.popErrorScope" => {
                let dev = self
                    .devices
                    .get_mut(&self.current)
                    .ok_or_else(|| missing("device"))?;
                let Some(guard) = dev.scopes.pop() else {
                    return Ok("ERR:popErrorScope with no scope pushed".into());
                };
                Ok(match pollster::block_on(guard.pop()) {
                    None => String::new(),
                    Some(error) => {
                        let kind = match &error {
                            wgpu::Error::OutOfMemory { .. } => "out-of-memory",
                            wgpu::Error::Internal { .. } => "internal",
                            _ => "validation",
                        };
                        format!("{kind}:{}", error_text(&error))
                    }
                })
            }
            "gpu.destroyDevice" => {
                // Its objects stay in the tables until the script drops them
                // (they are unusable either way, as in WebGPU); the device's
                // memory is released now.
                if let Some(mut dev) = self.devices.remove(&self.current) {
                    dev.scopes.clear();
                    dev.device.destroy();
                }
                Ok(String::new())
            }
            "gpu.capture" => {
                self.capturing = args.first().map(String::as_str) == Some("1");
                if self.capturing {
                    self.captured.clear();
                }
                Ok(String::new())
            }
            "gpu.replayCreate" => self.replay_create(ctx, args),
            "gpu.replayDrop" => {
                if let Some(token) = args.first() {
                    self.replays.remove(token);
                }
                Ok(String::new())
            }
            "gpu.drop" => {
                // Objects the script no longer references (FinalizationRegistry).
                let key = id(args, 1)?;
                match args.first().map(String::as_str) {
                    Some("group") => {
                        self.groups.remove(&key);
                        self.group_specs.remove(&key);
                    }
                    Some("pipeline") => {
                        self.pipelines.remove(&key);
                    }
                    Some("module") => {
                        self.modules.remove(&key);
                    }
                    Some("commands") => {
                        self.commands.remove(&key);
                    }
                    _ => {}
                }
                Ok(String::new())
            }
            _ => Err(format!("TypeError: unknown WebGPU host call {kind}")),
        }
    }

    fn adapter_json(&self) -> String {
        let s = &self.summary;
        let info = format!(
            "{{\"vendor\":{},\"architecture\":{},\"device\":\"\",\"description\":{},\"backend\":{},\"deviceType\":{},\"isFallbackAdapter\":{}}}",
            json_string(&s.vendor),
            json_string(&s.driver),
            json_string(&s.name),
            json_string(&s.backend),
            json_string(&s.device_type),
            s.is_fallback
        );
        format!(
            "{{\"info\":{info},\"limits\":{},\"features\":[]}}",
            limits_json(&self.adapter.limits())
        )
    }

    fn request_device(&mut self, args: &[String]) -> Result<String, String> {
        let adapter_limits = self.adapter.limits();
        let mut limits = wgpu::Limits::default();
        if let Some(json) = args.first() {
            for (name, value) in parse_flat_numbers(json)? {
                if !limit_supported(&adapter_limits, &name, value) {
                    return Err(format!(
                        "OperationError: requiredLimits.{name} = {value} exceeds the adapter"
                    ));
                }
                if !set_limit(&mut limits, &name, value) {
                    return Err(format!("OperationError: unknown limit {name}"));
                }
            }
        }
        let descriptor = wgpu::DeviceDescriptor {
            label: Some("zipp-gpu"),
            required_features: if self.profile.is_some()
                && self.adapter.features().contains(
                    wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES,
                ) {
                if let Some(p) = self.profile.as_mut() {
                    p.timestamps = std::env::var("ZIPP_GPU_PROFILE").as_deref() == Ok("kernels");
                }
                if self.profile.as_ref().is_some_and(|p| p.timestamps) {
                    wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES
                } else {
                    wgpu::Features::empty()
                }
            } else {
                wgpu::Features::empty()
            },
            required_limits: limits.clone(),
            ..Default::default()
        };
        let (device, queue) = pollster::block_on(self.adapter.request_device(&descriptor))
            .map_err(|e| format!("OperationError: {e}"))?;
        let recorded = Arc::new(Mutex::new(Recorded::default()));
        let sink = recorded.clone();
        device.on_uncaptured_error(Arc::new(move |error: wgpu::Error| {
            let mut r = sink.lock().unwrap();
            if r.uncaptured.len() < 64 {
                r.uncaptured.push(error_text(&error));
            }
        }));
        let sink = recorded.clone();
        device.set_device_lost_callback(move |reason, message| {
            if reason != wgpu::DeviceLostReason::Destroyed {
                sink.lock().unwrap().lost = Some(if message.is_empty() {
                    format!("{reason:?}")
                } else {
                    message
                });
            }
        });
        let json = limits_json(&device.limits());
        let key = self.fresh();
        self.devices.insert(
            key,
            Dev {
                device,
                queue,
                scopes: Vec::new(),
                recorded,
            },
        );
        Ok(format!("{{\"id\":{key},\"limits\":{json}}}"))
    }

    fn create_buffer(&mut self, args: &[String]) -> Result<String, String> {
        let size = int(args, 0)?;
        let usage_bits = int(args, 1)?;
        let mapped = int(args, 2)? != 0;
        let usage = u32::try_from(usage_bits)
            .ok()
            .and_then(wgpu::BufferUsages::from_bits)
            .ok_or("TypeError: unknown GPUBufferUsage bits")?;
        if mapped && size % 4 != 0 {
            return Err(
                "RangeError: a buffer mapped at creation needs a size that is a multiple of 4"
                    .into(),
            );
        }
        let buffer = self.device()?.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size,
            usage,
            mapped_at_creation: mapped,
        });
        let id = self.fresh();
        self.buffers.insert(id, BufferEntry { buffer, size });
        Ok(id.to_string())
    }

    fn entry(&self, key: u32) -> Result<&BufferEntry, String> {
        self.buffers.get(&key).ok_or_else(|| missing("buffer"))
    }

    fn unmap(&mut self, ctx: &mut dyn HostCtx, args: &[String]) -> Result<String, String> {
        let key = id(args, 0)?;
        let upload = num(args, 1)?;
        let entry = self.entry(key)?;
        if upload >= 0.0 {
            // The shadow the script wrote while the buffer was mapped at creation.
            let length = upload as usize;
            let bytes = region(ctx, "__zgpuUp", length)?;
            let mut view = entry
                .buffer
                .get_mapped_range_mut(..)
                .map_err(|e| format!("OperationError: {e}"))?;
            let n = length.min(view.len());
            view.slice(..n).copy_from_slice(&bytes[..n]);
            drop(view);
        }
        entry.buffer.unmap();
        Ok(String::new())
    }

    fn map_read(&mut self, ctx: &mut dyn HostCtx, args: &[String]) -> Result<String, String> {
        let key = id(args, 0)?;
        let offset = int(args, 1)?;
        let size = int(args, 2)?;
        if let Some(message) = self.lost() {
            return Ok(format!("LOST:{message}"));
        }
        let entry = self.entry(key)?;
        if offset % 8 != 0 || size % 4 != 0 || offset + size > entry.size {
            return Ok("ERR:mapAsync range is misaligned or outside the buffer".into());
        }
        if size == 0 {
            return Ok("ok".into());
        }
        let outcome: Arc<Mutex<Option<Result<(), wgpu::BufferAsyncError>>>> =
            Arc::new(Mutex::new(None));
        let sink = outcome.clone();
        entry
            .buffer
            .map_async(wgpu::MapMode::Read, offset..offset + size, move |result| {
                *sink.lock().unwrap() = Some(result);
            });
        self.wait()?;
        if let Some(message) = self.lost() {
            return Ok(format!("LOST:{message}"));
        }
        let result = outcome.lock().unwrap().take();
        match result {
            Some(Ok(())) => {}
            Some(Err(error)) => return Ok(format!("ERR:mapAsync failed: {error}")),
            None => return Ok("ERR:mapAsync did not complete".into()),
        }
        let entry = self.entry(key)?;
        let view = entry
            .buffer
            .get_mapped_range(offset..offset + size)
            .map_err(|e| format!("OperationError: {e}"))?;
        let target = region(ctx, "__zgpuDown", size as usize)?;
        target[..size as usize].copy_from_slice(&view);
        Ok("ok".into())
    }

    fn write_buffer(&mut self, ctx: &mut dyn HostCtx, args: &[String]) -> Result<String, String> {
        let key = id(args, 0)?;
        let offset = int(args, 1)?;
        let length = int(args, 2)? as usize;
        let bytes = region(ctx, "__zgpuUp", length)?;
        let entry = self.entry(key)?;
        if offset % 4 != 0 || length % 4 != 0 {
            return Err(
                "OperationError: writeBuffer offset and size must be multiples of 4".into(),
            );
        }
        if offset + length as u64 > entry.size {
            return Err("OperationError: writeBuffer range is outside the buffer".into());
        }
        self.queue()?
            .write_buffer(&entry.buffer, offset, &bytes[..length]);
        Ok(String::new())
    }

    fn bind_group_layout(&mut self, args: &[String]) -> Result<String, String> {
        let spec = args
            .first()
            .ok_or("TypeError: layout entries are required")?;
        let mut entries = Vec::new();
        for item in parse_rows(spec)? {
            // [binding, visibility, type, hasDynamicOffset, minBindingSize]
            if item.len() != 5 {
                return Err("TypeError: malformed bind group layout entry".into());
            }
            let binding = item[0].parse::<u32>().map_err(|_| "TypeError: binding")?;
            let visibility = item[1]
                .parse::<u32>()
                .map_err(|_| "TypeError: visibility")?;
            let ty = match item[2].trim_matches('"') {
                "uniform" => wgpu::BufferBindingType::Uniform,
                "storage" => wgpu::BufferBindingType::Storage { read_only: false },
                "read-only-storage" => wgpu::BufferBindingType::Storage { read_only: true },
                other => return Err(format!("TypeError: unknown buffer binding type {other}")),
            };
            let min = item[4]
                .parse::<u64>()
                .map_err(|_| "TypeError: minBindingSize")?;
            entries.push(wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::from_bits_truncate(visibility),
                ty: wgpu::BindingType::Buffer {
                    ty,
                    has_dynamic_offset: item[3] == "1",
                    min_binding_size: NonZeroU64::new(min),
                },
                count: None,
            });
        }
        let layout = self
            .device()?
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: None,
                entries: &entries,
            });
        let id = self.fresh();
        self.group_layouts.insert(id, layout);
        Ok(id.to_string())
    }

    fn pipeline(&mut self, args: &[String]) -> Result<String, String> {
        let layout = self
            .pipeline_layouts
            .get(&id(args, 0)?)
            .ok_or_else(|| missing("pipeline layout"))?;
        let module = self
            .modules
            .get(&id(args, 1)?)
            .ok_or_else(|| missing("shader module"))?;
        let entry = args.get(2).map(String::as_str).unwrap_or("main");
        let device = self.device()?;
        // Its own scope: an asynchronous pipeline reports failure through its
        // promise, not through the scopes around it.
        // A backend's shader compiler failing is an *internal* error in wgpu.
        let memory = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
        let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: None,
            layout: Some(layout),
            module,
            entry_point: Some(entry),
            compilation_options: Default::default(),
            cache: None,
        });
        let errors = [
            pollster::block_on(validation.pop()),
            pollster::block_on(internal.pop()),
            pollster::block_on(memory.pop()),
        ];
        if let Some(error) = errors.into_iter().flatten().next() {
            return Ok(format!("ERR:{}", error_text(&error)));
        }
        let id = self.fresh();
        if let (Some(p), Some(label)) = (self.profile.as_mut(), args.get(3)) {
            p.labels.insert(id, label.clone());
        }
        self.pipelines.insert(id, pipeline);
        Ok(id.to_string())
    }

    fn bind_group(&mut self, args: &[String]) -> Result<String, String> {
        let layout = self
            .group_layouts
            .get(&id(args, 0)?)
            .ok_or_else(|| missing("bind group layout"))?;
        let spec = args
            .get(1)
            .ok_or("TypeError: bind group entries are required")?;
        let rows = parse_rows(spec)?;
        let layout_id = id(args, 0)?;
        let mut record = Vec::with_capacity(rows.len());
        let mut entries = Vec::with_capacity(rows.len());
        for item in &rows {
            // [binding, buffer, offset, size or -1]
            if item.len() != 4 {
                return Err("TypeError: malformed bind group entry".into());
            }
            let binding = item[0].parse::<u32>().map_err(|_| "TypeError: binding")?;
            let key = item[1].parse::<u32>().map_err(|_| missing("buffer"))?;
            let buffer = &self
                .buffers
                .get(&key)
                .ok_or_else(|| missing("buffer"))?
                .buffer;
            let offset = item[2].parse::<u64>().map_err(|_| "TypeError: offset")?;
            let size = item[3].parse::<i64>().map_err(|_| "TypeError: size")?;
            record.push((binding, key, offset, size));
            entries.push(wgpu::BindGroupEntry {
                binding,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer,
                    offset,
                    size: if size < 0 {
                        None
                    } else {
                        NonZeroU64::new(size as u64)
                    },
                }),
            });
        }
        let group = self
            .device()?
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout,
                entries: &entries,
            });
        let id = self.fresh();
        self.groups.insert(id, group);
        self.group_specs.insert(id, (layout_id, record));
        Ok(id.to_string())
    }

    /// Replay one encoder's recording: `P` begins a compute pass, `E` ends
    /// it, `S<pipeline>`, `B<index>,<group>[,<offset>...]`, `D<x>,<y>,<z>`
    /// inside it; `C<src>,<srcOffset>,<dst>,<dstOffset>,<size>` outside.
    fn finish(&mut self, args: &[String]) -> Result<String, String> {
        let script = args.first().map(String::as_str).unwrap_or("");
        if self.capturing {
            self.captured.push(script.to_owned());
        }
        let mut timing = 0u32;
        if let Some(p) = self.profile.as_mut() {
            for op in script.split(';') {
                match op.as_bytes().first() {
                    Some(b'D') => {
                        p.dispatches += 1;
                        timing += 2;
                    }
                    Some(b'P') => p.passes += 1,
                    _ => {}
                }
            }
            if !p.timestamps {
                timing = 0;
            }
        }
        let timing = timing.min(4096);
        let device = self.device()?;
        let queries = (timing > 0).then(|| {
            device.create_query_set(&wgpu::QuerySetDescriptor {
                label: None,
                ty: wgpu::QueryType::Timestamp,
                count: timing,
            })
        });
        let mut spans: Vec<(u32, u32, String)> = Vec::new();
        let mut current_label = String::new();
        let labels = self.profile.as_ref().map(|p| &p.labels);
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        let mut ops = script.split(';').filter(|s| !s.is_empty()).peekable();
        let numbers = |text: &str| -> Result<Vec<u64>, String> {
            text.split(',')
                .map(|p| {
                    p.parse::<u64>()
                        .map_err(|_| "TypeError: malformed command".to_string())
                })
                .collect()
        };
        while let Some(op) = ops.next() {
            let (tag, rest) = op.split_at(1);
            match tag {
                "P" => {
                    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: None,
                        timestamp_writes: None,
                    });
                    for op in ops.by_ref() {
                        let (tag, rest) = op.split_at(1);
                        match tag {
                            "E" => break,
                            "S" => {
                                let key: u32 = rest.parse().map_err(|_| missing("pipeline"))?;
                                if let Some(labels) = labels {
                                    current_label = labels
                                        .get(&key)
                                        .cloned()
                                        .unwrap_or_else(|| format!("pipeline {key}"));
                                }
                                pass.set_pipeline(
                                    self.pipelines
                                        .get(&key)
                                        .ok_or_else(|| missing("pipeline"))?,
                                );
                            }
                            "B" => {
                                let n = numbers(rest)?;
                                if n.len() < 2 {
                                    return Err("TypeError: malformed setBindGroup".into());
                                }
                                let group = self
                                    .groups
                                    .get(&(n[1] as u32))
                                    .ok_or_else(|| missing("bind group"))?;
                                let offsets: Vec<u32> = n[2..].iter().map(|&v| v as u32).collect();
                                pass.set_bind_group(n[0] as u32, group, &offsets);
                            }
                            "D" => {
                                let n = numbers(rest)?;
                                if n.len() != 3 {
                                    return Err("TypeError: malformed dispatch".into());
                                }
                                let q = spans.len() as u32 * 2;
                                let timed = queries.as_ref().filter(|_| q + 2 <= timing);
                                if let Some(set) = timed {
                                    pass.write_timestamp(set, q);
                                }
                                pass.dispatch_workgroups(n[0] as u32, n[1] as u32, n[2] as u32);
                                if let Some(set) = timed {
                                    pass.write_timestamp(set, q + 1);
                                    spans.push((q, q + 1, current_label.clone()));
                                }
                            }
                            _ => {
                                return Err(format!(
                                    "TypeError: unknown compute pass command {tag}"
                                ))
                            }
                        }
                    }
                }
                "C" => {
                    let n = numbers(rest)?;
                    if n.len() != 5 {
                        return Err("TypeError: malformed copyBufferToBuffer".into());
                    }
                    let src = &self.entry(n[0] as u32)?.buffer;
                    let dst = &self.entry(n[2] as u32)?.buffer;
                    encoder.copy_buffer_to_buffer(src, n[1], dst, n[3], Some(n[4]));
                }
                _ => return Err(format!("TypeError: unknown command {tag}")),
            }
        }
        let mut timed = None;
        if let (Some(set), false) = (queries, spans.is_empty()) {
            let bytes = spans.len() as u64 * 16;
            let resolve = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: bytes,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            let read = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: bytes,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            encoder.resolve_query_set(&set, 0..spans.len() as u32 * 2, &resolve, 0);
            encoder.copy_buffer_to_buffer(&resolve, 0, &read, 0, Some(bytes));
            timed = Some((read, resolve, set, spans));
        }
        let commands = encoder.finish();
        let id = self.fresh();
        if let (Some(t), Some(p)) = (timed, self.profile.as_mut()) {
            p.timed.insert(id, t);
        }
        self.commands.insert(id, commands);
        Ok(id.to_string())
    }
}

impl WebGpu {
    /// `ZIPP_GPU_PROFILE=kernels`: wait for the command buffers just
    /// submitted and add their dispatches' timestamps to the profile.
    fn collect_timestamps(&mut self, ids: &str) -> Result<(), String> {
        let period = self.queue()?.get_timestamp_period() as f64;
        let device = self.device()?.clone();
        let Some(p) = self.profile.as_mut() else {
            return Ok(());
        };
        for part in ids.split(',').filter(|s| !s.is_empty()) {
            let Ok(key) = part.parse::<u32>() else {
                continue;
            };
            let Some((read, _resolve, _set, spans)) = p.timed.remove(&key) else {
                continue;
            };
            read.slice(..).map_async(wgpu::MapMode::Read, |_| {});
            device
                .poll(wgpu::PollType::wait_indefinitely())
                .map_err(|e| e.to_string())?;
            let view = read
                .slice(..)
                .get_mapped_range()
                .map_err(|e| format!("OperationError: {e}"))?;
            let stamps: Vec<u64> = view
                .chunks_exact(8)
                .map(|b: &[u8]| u64::from_le_bytes(b.try_into().unwrap()))
                .collect();
            drop(view);
            read.unmap();
            if let (Some(first), Some(last)) = (spans.first(), spans.last()) {
                let span = stamps[last.1 as usize].saturating_sub(stamps[first.0 as usize]) as f64
                    * period;
                let slot = p
                    .kernels
                    .entry("(first to last dispatch)".into())
                    .or_default();
                slot.0 += 1;
                slot.1 += span / 1e6;
            }
            for (a, b, label) in spans {
                let ns = stamps[b as usize].saturating_sub(stamps[a as usize]) as f64 * period;
                let slot = p.kernels.entry(label).or_default();
                slot.0 += 1;
                slot.1 += ns / 1e6;
            }
        }
        Ok(())
    }
}

/// The bytes of the typed-array global `name`, at least `length` of them.
pub(crate) fn region<'a>(
    ctx: &mut dyn HostCtx,
    name: &str,
    length: usize,
) -> Result<&'a mut [u8], String> {
    let (address, count, kind) = ctx.typed_array_region(name)?;
    if kind != 1 {
        return Err(format!("TypeError: {name} must be a Uint8Array"));
    }
    if count < length {
        return Err(format!(
            "RangeError: {name} holds {count} bytes, {length} needed"
        ));
    }
    if count == 0 {
        return Ok(&mut []);
    }
    // SAFETY: the engine pinned this ArrayBuffer (it is never resized,
    // transferred or detached, and not freed while the VM lives) and nothing
    // in the VM runs while this host call is being served, so this is the
    // only live reference to those `count` bytes for the call's duration.
    // The slice is used only inside the call that resolved it.
    Ok(unsafe { std::slice::from_raw_parts_mut(address as *mut u8, count) })
}

fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `{"name": number, ...}` with plain keys (what the shim sends as requiredLimits).
fn parse_flat_numbers(json: &str) -> Result<Vec<(String, f64)>, String> {
    let body = json.trim();
    let body = body
        .strip_prefix('{')
        .and_then(|b| b.strip_suffix('}'))
        .ok_or("TypeError: requiredLimits must be an object")?;
    let mut out = Vec::new();
    for pair in body.split(',').filter(|p| !p.trim().is_empty()) {
        let (key, value) = pair
            .split_once(':')
            .ok_or("TypeError: malformed requiredLimits")?;
        let key = key.trim().trim_matches('"').to_owned();
        let value: f64 = value
            .trim()
            .parse()
            .map_err(|_| "TypeError: a limit must be a number")?;
        out.push((key, value));
    }
    Ok(out)
}

/// `[[a,b,...],[...]]` of numbers and plain strings (what the shim sends for entries).
fn parse_rows(json: &str) -> Result<Vec<Vec<String>>, String> {
    let body = json.trim();
    let body = body
        .strip_prefix('[')
        .and_then(|b| b.strip_suffix(']'))
        .ok_or("TypeError: malformed entry list")?;
    let mut rows = Vec::new();
    for row in body.split(']') {
        let row = row.trim().trim_start_matches(',').trim();
        if row.is_empty() {
            continue;
        }
        let row = row.strip_prefix('[').ok_or("TypeError: malformed entry")?;
        rows.push(row.split(',').map(|s| s.trim().to_owned()).collect());
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_shim_json() {
        assert_eq!(
            parse_rows("[[0,4,\"uniform\",1,0],[1,4,\"read-only-storage\",0,0]]").unwrap(),
            vec![
                vec!["0", "4", "\"uniform\"", "1", "0"],
                vec!["1", "4", "\"read-only-storage\"", "0", "0"]
            ]
        );
        assert_eq!(parse_rows("[]").unwrap(), Vec::<Vec<String>>::new());
        assert_eq!(
            parse_flat_numbers(
                "{\"maxBufferSize\":1073741824,\"maxStorageBufferBindingSize\":1073741824}"
            )
            .unwrap(),
            vec![
                ("maxBufferSize".into(), 1073741824.0),
                ("maxStorageBufferBindingSize".into(), 1073741824.0)
            ]
        );
        assert_eq!(parse_flat_numbers("{}").unwrap(), vec![]);
    }
    #[test]
    fn backend_names() {
        assert_eq!(
            backend_order(Some("vulkan")).unwrap(),
            vec![wgpu::Backends::VULKAN]
        );
        assert_eq!(
            backend_order(Some("DX12")).unwrap(),
            vec![wgpu::Backends::DX12]
        );
        assert!(backend_order(Some("gl")).is_err());
        assert!(backend_order(Some("cuda")).is_err());
        assert_eq!(backend_order(None).unwrap().len(), 3);
    }
}
