//! Native GPU execution of ZIPP's `torch.compile` / `zipp_gpu` graphs.
//!
//! In the browser a Python program's graphs leave the engine as host requests
//! and run on gpu-lab's JavaScript runtime over WebGPU. This crate gives the
//! native `zipp` executable the same thing without a browser: a second,
//! trusted ZIPP JavaScript state runs gpu-lab's runtime *unchanged* (its
//! sources are compiled into the binary, see [`bundle`]), and the WebGPU API
//! that runtime calls is a small shim (`js/webgpu-shim.js`) over wgpu
//! ([`device`]), which drives Vulkan, Direct3D 12 or Metal. One helper,
//! gpu-lab's `float32Data` as the session module imports it, is served by a
//! native finiteness scan for Float32Array feeds (`js/accelerate.js`: same
//! results, same errors, a fraction of this engine's per-element loop cost).
//!
//! [`GpuHost::open`] finds a hardware adapter and brings the runtime up on
//! it; `Ok(None)` means there is none (no driver, a software rasterizer only),
//! and the caller keeps its CPU path. [`GpuHost::handle`] serves one request
//! (the five `gpu.*` kinds a browser host serves) and returns its reply.
//! [`SyncBridge`] puts that behind a Python program's `__zippHostCall`, so
//! `zipp_gpu` runs each graph on the GPU *synchronously*, exactly where the
//! CPU evaluator would have run it (see [`bridge`]).
use std::cell::RefCell;
use std::rc::Rc;

use zipp_vm::embed::{HostValue, HostValueBudget, ScriptState};

pub mod bridge;
pub mod bundle;
pub mod device;

pub use bridge::SyncBridge;
pub use device::AdapterSummary;

const SHIM: &str = include_str!("../js/webgpu-shim.js");
const DRIVER: &str = include_str!("../js/driver.js");
const ACCELERATE: &str = include_str!("../js/accelerate.js");

/// Host values a reply may carry: every tensor of a large session's sync.
const REPLY_NODES: usize = 4_000_000;
const REPLY_BYTES: usize = 1 << 30;

/// How the host is chosen.
#[derive(Debug, Clone, Default)]
pub struct GpuOptions {
    /// `vulkan`, `dx12`, `metal` or `auto` (the default order: Vulkan, then
    /// D3D12, then Metal). From `ZIPP_GPU_BACKEND` in [`GpuOptions::from_env`].
    pub backend: Option<String>,
    /// Also bundle gpu-lab's protocol cases and benchmark (tests only).
    pub with_cases: bool,
    /// Keep gpu-lab's browser limits (64 MiB of graph storage, 4M elements a
    /// tensor, a 1G work budget) instead of the device-bounded native ones.
    pub browser_limits: bool,
}

impl GpuOptions {
    /// `ZIPP_GPU_BACKEND`.
    pub fn from_env() -> Self {
        GpuOptions {
            backend: std::env::var("ZIPP_GPU_BACKEND").ok(),
            ..Default::default()
        }
    }
}

/// Whether the environment turns the native GPU off (`ZIPP_GPU=0`, `off`,
/// `false`, `no`, `cpu`).
pub fn disabled_by_env() -> bool {
    std::env::var("ZIPP_GPU")
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "off" | "false" | "no" | "cpu"
            )
        })
        .unwrap_or(false)
}

/// The native GPU runtime: gpu-lab in its own JavaScript state, on one device.
pub struct GpuHost {
    state: ScriptState,
    gpu: Rc<RefCell<device::WebGpu>>,
    handle_slot: u32,
    take_slot: u32,
    summary: AdapterSummary,
    status: HostValue,
}

fn slot_of(state: &ScriptState, name: &str) -> Result<u32, String> {
    state
        .symbols()
        .into_iter()
        .find(|s| s.name == name)
        .map(|s| s.index)
        .ok_or_else(|| format!("zipp-gpu: the driver has no {name}"))
}

fn field<'a>(value: &'a HostValue, name: &str) -> Option<&'a HostValue> {
    match value {
        HostValue::Object(pairs) => pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v),
        _ => None,
    }
}

impl GpuHost {
    /// Find a hardware adapter and start gpu-lab's WebGPU runtime on it.
    /// `Ok(None)`: no usable adapter. `Err`: an adapter exists but the
    /// runtime could not start on it (the caller falls back as well, and may
    /// report why).
    pub fn open(options: &GpuOptions) -> Result<Option<GpuHost>, String> {
        let order = device::backend_order(options.backend.as_deref())?;
        let Some((instance, adapter)) = device::find_adapter(&order)? else {
            return Ok(None);
        };
        Self::start(device::WebGpu::new(instance, adapter), options).map(Some)
    }

    fn start(gpu: device::WebGpu, options: &GpuOptions) -> Result<GpuHost, String> {
        let summary = gpu.summary.clone();
        let mut modules: Vec<&[bundle::Module]> = vec![bundle::RUNTIME];
        if options.with_cases {
            modules.push(bundle::CASES);
        }
        let source = format!(
            "{SHIM}\n{}\n{DRIVER}\n",
            bundle::bundle_with(&modules, &[("src/graph.mjs", ACCELERATE)])?
        );
        let mut state = zipp_vm::embed::compile_script(&source)
            .map_err(|e| format!("zipp-gpu: the runtime does not compile: {e}"))?;
        let gpu = Rc::new(RefCell::new(gpu));
        let served = gpu.clone();
        state.set_host_call_ctx(Box::new(move |ctx, kind, args| {
            served.borrow_mut().call(ctx, kind, args)
        }));
        // Diagnostics the runtime prints go to stderr, never to the program's stdout.
        state.set_console_sink(Box::new(|_, line| eprintln!("zipp-gpu: {line}")));
        state
            .run_init()
            .map_err(|e| format!("zipp-gpu: the runtime did not load: {e}"))?;
        let init = slot_of(&state, "__zgpuInit")?;
        let status_slot = slot_of(&state, "__zgpuStatus")?;
        state
            .call_slot(
                init,
                &[
                    HostValue::String("webgpu".into()),
                    HostValue::String(if options.browser_limits { "" } else { "native" }.into()),
                    HostValue::String(summary.describe()),
                ],
            )
            .map_err(|e| format!("zipp-gpu: {e}"))?;
        let status = state
            .call_slot(status_slot, &[])
            .map_err(|e| format!("zipp-gpu: {e}"))?;
        if !matches!(field(&status, "ok"), Some(HostValue::Bool(true))) {
            let why = match field(&status, "error") {
                Some(HostValue::String(s)) => s.clone(),
                _ => "unknown error".into(),
            };
            return Err(format!(
                "zipp-gpu: WebGPU runtime unavailable on {}: {why}",
                summary.describe()
            ));
        }
        Ok(GpuHost {
            handle_slot: slot_of(&state, "__zgpuHandle")?,
            take_slot: slot_of(&state, "__zgpuTake")?,
            state,
            gpu,
            summary,
            status,
        })
    }

    /// The adapter the runtime runs on.
    pub fn adapter(&self) -> &AdapterSummary {
        &self.summary
    }

    /// `runtime.info()` as the driver reported it at start.
    pub fn status(&self) -> &HostValue {
        &self.status
    }

    /// Serve one host request (`kind` is one of the five `gpu.*` kinds) and
    /// return the reply to deliver: `{ok: true, value}` or
    /// `{ok: false, error: {code, message[, poisoned]}}`.
    pub fn handle(&mut self, id: f64, kind: &str, payload: HostValue) -> HostValue {
        let started = self.state.call_slot(
            self.handle_slot,
            &[
                HostValue::Number(id),
                HostValue::String(kind.to_owned()),
                payload,
            ],
        );
        if let Err(message) = started {
            return failure("GPU", &message);
        }
        let mut budget = HostValueBudget::new(REPLY_NODES, REPLY_BYTES);
        let replies = match self
            .state
            .call_slot_bounded(self.take_slot, &[], &mut budget)
        {
            Ok(HostValue::Array(items)) => items,
            Ok(_) => Vec::new(),
            Err(error) => return failure("GPU", &error.into_message()),
        };
        let uncaptured = self.gpu.borrow_mut().take_uncaptured();
        if !uncaptured.is_empty() && std::env::var_os("ZIPP_GPU_LOG").is_some() {
            for line in &uncaptured {
                eprintln!("zipp-gpu: uncaptured WebGPU error: {line}");
            }
        }
        for reply in replies {
            if let HostValue::Array(mut pair) = reply {
                if pair.len() == 2 && pair[0] == HostValue::Number(id) {
                    return pair.pop().unwrap_or(HostValue::Null);
                }
            }
        }
        failure("GPU", "the GPU runtime did not complete the request")
    }

    /// Evaluate `source` in the runtime's state (tests and benchmarks drive
    /// gpu-lab's own harness this way) and drain its jobs.
    pub fn eval(&mut self, source: &str) -> Result<zipp_vm::embed::JsValue, String> {
        let value = self.state.eval_in_context(source)?;
        self.state.run_microtasks();
        Ok(value)
    }
}

impl Drop for GpuHost {
    fn drop(&mut self) {
        if let Ok(slot) = slot_of(&self.state, "__zgpuDispose") {
            let _ = self.state.call_slot(slot, &[]);
        }
    }
}

fn failure(code: &str, message: &str) -> HostValue {
    HostValue::Object(vec![
        ("ok".into(), HostValue::Bool(false)),
        (
            "error".into(),
            HostValue::Object(vec![
                ("code".into(), HostValue::String(code.into())),
                (
                    "message".into(),
                    HostValue::String(message.chars().take(512).collect()),
                ),
            ]),
        ),
    ])
}
