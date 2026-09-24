//! Native GPU execution of ZIPP's `torch.compile` / `zipp_gpu` graphs.
//!
//! In the browser a Python program's graphs leave the engine as host requests
//! and run on gpu-lab's JavaScript runtime over WebGPU. This crate gives the
//! native `zipp` executable the same thing without a browser: a second,
//! trusted ZIPP JavaScript state runs gpu-lab's runtime *unchanged* (its
//! sources are compiled into the binary, see [`bundle`]), and the WebGPU API
//! that runtime calls is a small shim (`js/webgpu-shim.js`) over wgpu
//! ([`device`]), which drives Vulkan, Direct3D 12 or Metal. Two of gpu-lab's
//! helpers, `float32Data` (taking ownership of an input) and
//! `checkFiniteOutput` (readback's finiteness rule), are served by a native
//! finiteness scan for Float32Arrays (`js/accelerate.js`, rebound in
//! graph.mjs's own scope: same results, same errors, a fraction of this
//! engine's per-element loop cost).
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
mod replay;

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
    /// Run every prepared step through gpu-lab (no native replay). From
    /// `ZIPP_GPU_REPLAY=0` in [`GpuOptions::from_env`].
    pub no_replay: bool,
}

impl GpuOptions {
    /// `ZIPP_GPU_BACKEND`.
    pub fn from_env() -> Self {
        GpuOptions {
            backend: std::env::var("ZIPP_GPU_BACKEND").ok(),
            no_replay: std::env::var("ZIPP_GPU_REPLAY").is_ok_and(|v| v.trim() == "0"),
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
    begin_slot: u32,
    end_slot: u32,
    /// Prepared steps replay natively (`ZIPP_GPU_REPLAY=0` turns it off).
    replay: bool,
    summary: AdapterSummary,
    status: HostValue,
    /// Ids of requests the host makes itself (negative: never a bridge's).
    next_internal: f64,
    next_request: f64,
    /// `ZIPP_GPU_PROFILE`: ms running requests in the runtime, and taking replies.
    profile: Option<(f64, f64)>,
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
        // The drivers are most of the start-up: the adapter is found on
        // another thread while this one compiles the runtime.
        let probe = device::Probe::start(order)?;
        let state = Self::compile(options);
        let Some(gpu) = probe.finish()? else {
            return Ok(None);
        };
        Self::start(gpu, state?, options).map(Some)
    }

    /// gpu-lab's runtime, the shim and the driver as one compiled script.
    fn compile(options: &GpuOptions) -> Result<ScriptState, String> {
        let mut modules: Vec<&[bundle::Module]> = vec![bundle::RUNTIME];
        if options.with_cases {
            modules.push(bundle::CASES);
        }
        let source = format!(
            "{SHIM}\n{}\n{DRIVER}\n",
            bundle::bundle_with(&modules, &[("src/graph.mjs", ACCELERATE)])?
        );
        zipp_vm::embed::compile_script(&source)
            .map_err(|e| format!("zipp-gpu: the runtime does not compile: {e}"))
    }

    fn start(
        gpu: device::WebGpu,
        mut state: ScriptState,
        options: &GpuOptions,
    ) -> Result<GpuHost, String> {
        let summary = gpu.summary.clone();
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
                    HostValue::Bool(!options.no_replay),
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
            begin_slot: slot_of(&state, "__zgpuReplayBegin")?,
            end_slot: slot_of(&state, "__zgpuReplayEnd")?,
            replay: !options.no_replay,
            state,
            gpu,
            summary,
            status,
            next_internal: 0.0,
            next_request: 0.0,
            profile: std::env::var_os("ZIPP_GPU_PROFILE").map(|_| (0.0, 0.0)),
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
        self.handle_owned(id, kind, payload, false)
    }

    /// [`GpuHost::handle`] for a payload built for this request alone whose
    /// float32 arrays the caller has checked are all finite (`finite`):
    /// gpu-lab then takes those arrays as they are instead of scanning and
    /// copying each one (`js/accelerate.js`). Nothing else may hold them.
    pub fn handle_owned(
        &mut self,
        id: f64,
        kind: &str,
        payload: HostValue,
        finite: bool,
    ) -> HostValue {
        let begun = std::time::Instant::now();
        let started = self.state.call_slot(
            self.handle_slot,
            &[
                HostValue::Number(id),
                HostValue::String(kind.to_owned()),
                payload,
                HostValue::Bool(finite),
            ],
        );
        if let Err(message) = started {
            return failure("GPU", &message);
        }
        let taken = std::time::Instant::now();
        let mut budget = HostValueBudget::new(REPLY_NODES, REPLY_BYTES);
        let replies = match self
            .state
            .call_slot_bounded(self.take_slot, &[], &mut budget)
        {
            Ok(HostValue::Array(items)) => items,
            Ok(_) => Vec::new(),
            Err(error) => return failure("GPU", &error.into_message()),
        };
        if let Some(p) = self.profile.as_mut() {
            p.0 += (taken - begun).as_secs_f64() * 1000.0;
            p.1 += taken.elapsed().as_secs_f64() * 1000.0;
        }
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

    /// Serve one request as the CLI's bridge sends it: `body` is its JSON,
    /// with each float32 tensor as `{"$f32": [offset, length]}` into `bytes`.
    /// A prepared step the session's replay covers runs natively
    /// ([`GpuHost::try_replay`]); anything else goes to gpu-lab.
    pub fn request(
        &mut self,
        kind: &str,
        body: &serde_json::Value,
        bytes: &[u8],
    ) -> Result<HostValue, String> {
        if kind == "gpu.session.run" {
            if let Some(reply) = self.try_replay(body, bytes) {
                return Ok(reply);
            }
        }
        // Every float32 value of the request finite: gpu-lab may then take
        // the arrays as they are (they are this request's alone).
        let finite = bytes.len() % 4 == 0
            && bytes
                .chunks_exact(4)
                .all(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) & 0x7f80_0000 != 0x7f80_0000);
        let payload = crate::bridge::to_host(body, bytes)?;
        self.next_request += 1.0;
        let id = self.next_request;
        Ok(self.handle_owned(id, kind, payload, finite))
    }

    /// Serve a `gpu.session.run` request natively when the session has a
    /// replay (`replay.rs`) that covers it: `body` is the request's JSON, its
    /// `{"$f32": [offset, length]}` tensors in `bytes`. `None` hands the
    /// request to gpu-lab (no replay, another feed set or read-back list, a
    /// value a check refuses, a session that cannot run): gpu-lab then gives
    /// the result or the error it always gives. A run of several steps on a
    /// session with no replay yet runs its first step through gpu-lab (which
    /// may capture one) and the rest natively when it did: the same steps.
    pub fn try_replay(&mut self, body: &serde_json::Value, bytes: &[u8]) -> Option<HostValue> {
        if !self.replay {
            return None;
        }
        let token = body.get("session")?.as_str()?.to_owned();
        let steps = body.get("steps")?.as_array()?;
        if steps.len() < 2 || self.gpu.borrow().replays.contains_key(&token) {
            return self.replay_run(body, bytes);
        }
        let object = body.as_object()?;
        let part = |steps: Vec<serde_json::Value>, keep_step: bool| {
            let mut o = object.clone();
            o.insert("steps".into(), serde_json::Value::Array(steps));
            if !keep_step {
                o.remove("step");
            }
            serde_json::Value::Object(o)
        };
        // The first step through gpu-lab, which captures the session's step.
        let head = part(vec![steps[0].clone()], true);
        let tail = part(steps[1..].to_vec(), false);
        let payload = crate::bridge::to_host(&head, bytes).ok()?;
        let id = self.fresh_id();
        let first = self.handle_owned(id, "gpu.session.run", payload, false);
        if !reply_ok(&first) {
            return Some(first);
        }
        let rest = match self.replay_run(&tail, bytes) {
            Some(reply) => reply,
            None => {
                let payload = crate::bridge::to_host(&tail, bytes).ok()?;
                let id = self.fresh_id();
                self.handle_owned(id, "gpu.session.run", payload, false)
            }
        };
        Some(merge_runs(first, rest))
    }

    fn fresh_id(&mut self) -> f64 {
        self.next_internal -= 1.0;
        self.next_internal
    }

    fn replay_run(&mut self, body: &serde_json::Value, bytes: &[u8]) -> Option<HostValue> {
        let obj = body.as_object()?;
        if obj
            .keys()
            .any(|k| !matches!(k.as_str(), "session" | "steps" | "readback" | "step"))
        {
            return None;
        }
        let token = obj.get("session")?.as_str()?;
        let steps = obj.get("steps")?.as_array()?;
        let readback: Vec<&str> = obj
            .get("readback")?
            .as_array()?
            .iter()
            .map(|v| v.as_str())
            .collect::<Option<_>>()?;
        let step = match obj.get("step") {
            None | Some(serde_json::Value::Null) => HostValue::Null,
            Some(v) => HostValue::Number(v.as_f64()?),
        };
        if steps.is_empty() {
            return None;
        }
        let started = std::time::Instant::now();
        let mut feeds: Vec<Vec<&[u8]>> = Vec::with_capacity(steps.len());
        {
            let gpu = self.gpu.borrow();
            let replay = gpu.replays.get(token)?;
            if !reads_back(replay, &readback) {
                return None;
            }
            for entry in steps {
                let entry = entry.as_object()?;
                if entry.len() != 1 {
                    return None;
                }
                let inputs = entry.get("inputs")?.as_object()?;
                if inputs.len() != replay.feeds.len() {
                    return None;
                }
                let mut step_feeds = Vec::with_capacity(replay.feeds.len());
                for feed in &replay.feeds {
                    let at = inputs.get(&feed.id.to_string())?.as_object()?;
                    if at.len() != 1 {
                        return None;
                    }
                    let at = at.get("$f32")?.as_array()?;
                    let (offset, length) = (
                        at.first()?.as_u64()? as usize,
                        at.get(1)?.as_u64()? as usize,
                    );
                    if length != feed.size {
                        return None;
                    }
                    let data = bytes.get(offset..offset.checked_add(length.checked_mul(4)?)?)?;
                    if !feed_accepted(feed, data) {
                        return None;
                    }
                    step_feeds.push(data);
                }
                feeds.push(step_feeds);
            }
        }
        let ran = match self.replay_steps(token, &feeds, step, started)? {
            Replayed::Ran(ran) => ran,
            Replayed::Failed(code, message) => return Some(replay_failure(code, message)),
        };
        let replying = std::time::Instant::now();
        let gpu = self.gpu.borrow();
        let replay = gpu.replays.get(token)?;
        let mut out_steps = Vec::with_capacity(ran.values.len());
        for (s, step_values) in ran.values.into_iter().enumerate() {
            let mut outputs = Vec::with_capacity(replay.outputs.len());
            let mut at = 0usize;
            for o in &replay.outputs {
                outputs.push((
                    o.name.clone(),
                    HostValue::Object(vec![
                        (
                            "shape".into(),
                            HostValue::Array(
                                o.shape.iter().map(|&d| HostValue::Number(d)).collect(),
                            ),
                        ),
                        ("dtype".into(), HostValue::String("float32".into())),
                        (
                            "data".into(),
                            HostValue::Float32Array(step_values[at..at + o.size].to_vec()),
                        ),
                    ]),
                ));
                at += o.size;
            }
            out_steps.push(HostValue::Object(vec![
                ("step".into(), HostValue::Number(ran.first + s as f64)),
                ("outputs".into(), HostValue::Object(outputs)),
            ]));
        }
        // As gpu-lab's reply: the last step's outputs again, at the top.
        let last_outputs = out_steps
            .last()
            .and_then(|s| field(s, "outputs"))
            .cloned()
            .unwrap_or(HostValue::Null);
        drop(gpu);
        self.gpu
            .borrow_mut()
            .replay_phase(8, replying.elapsed().as_secs_f64() * 1000.0, false);
        Some(HostValue::Object(vec![
            ("ok".into(), HostValue::Bool(true)),
            (
                "value".into(),
                HostValue::Object(vec![
                    ("version".into(), HostValue::Number(1.0)),
                    ("backend".into(), HostValue::String("webgpu".into())),
                    ("outputs".into(), last_outputs),
                    ("steps".into(), HostValue::Array(out_steps)),
                    ("stats".into(), HostValue::Object(ran.stats)),
                    ("step".into(), HostValue::Number(ran.session_step)),
                ]),
            ),
        ]))
    }

    /// A prepared step request in binary form (the bridge's `zipp.gpu.step`,
    /// which the Python runtime sends instead of `gpu.session.run`'s JSON
    /// for a run of steps whose inputs are all fed float32 tensors): `ids` the
    /// fed input ids in the order each step's arrays follow one another in
    /// `bytes`, `readback` the outputs read back. Served only by the
    /// session's replay, with the same checks, results and failures as
    /// [`GpuHost::try_replay`] gives the same request as JSON; `None` when a
    /// replay would not serve it (the caller then sends the JSON request,
    /// which gpu-lab serves as it always does).
    pub fn replay_binary(
        &mut self,
        token: &str,
        ids: &[u32],
        readback: &[&str],
        bytes: &[u8],
    ) -> Option<BinaryRun> {
        if !self.replay {
            return None;
        }
        let started = std::time::Instant::now();
        let mut feeds: Vec<Vec<&[u8]>> = Vec::new();
        let outputs;
        {
            let gpu = self.gpu.borrow();
            let replay = gpu.replays.get(token)?;
            if !reads_back(replay, readback) || ids.len() != replay.feeds.len() {
                return None;
            }
            // Where each of the replay's feeds sits within one step's bytes.
            let mut offsets: Vec<Option<usize>> = vec![None; replay.feeds.len()];
            let mut step_bytes = 0usize;
            for &id in ids {
                let i = replay.feeds.iter().position(|f| f.id == id)?;
                if offsets[i].is_some() {
                    return None;
                }
                offsets[i] = Some(step_bytes);
                step_bytes += replay.feeds[i].size * 4;
            }
            let offsets: Vec<usize> = offsets.into_iter().collect::<Option<_>>()?;
            if step_bytes == 0 || bytes.is_empty() || bytes.len() % step_bytes != 0 {
                return None;
            }
            for step in bytes.chunks_exact(step_bytes) {
                let mut step_feeds = Vec::with_capacity(replay.feeds.len());
                for (feed, &at) in replay.feeds.iter().zip(&offsets) {
                    let data = &step[at..at + feed.size * 4];
                    if !feed_accepted(feed, data) {
                        return None;
                    }
                    step_feeds.push(data);
                }
                feeds.push(step_feeds);
            }
            outputs = replay
                .outputs
                .iter()
                .map(|o| (o.name.clone(), o.size, o.shape.clone()))
                .collect::<Vec<_>>();
        }
        Some(match self.replay_steps(token, &feeds, HostValue::Null, started)? {
            Replayed::Ran(ran) => BinaryRun::Ran {
                first: ran.first,
                step: ran.session_step,
                outputs,
                values: ran.values,
                stats: HostValue::Object(ran.stats),
            },
            Replayed::Failed(code, message) => BinaryRun::Failed { code, message },
        })
    }

    /// Run `feeds.len()` steps of `token`'s replay (`feeds[s]` in the
    /// replay's feed order, already checked): gpu-lab's checks and uniform
    /// words (`__zgpuReplayBegin`), the device work, then its bookkeeping
    /// (`__zgpuReplayEnd`). `None`: gpu-lab should run it instead.
    fn replay_steps(
        &mut self,
        token: &str,
        feeds: &[Vec<&[u8]>],
        step: HostValue,
        started: std::time::Instant,
    ) -> Option<Replayed> {
        let count = feeds.len() as f64;
        let checked = std::time::Instant::now();
        let begun = self
            .state
            .call_slot(
                self.begin_slot,
                &[
                    HostValue::String(token.to_owned()),
                    HostValue::Number(count),
                    step,
                ],
            )
            .ok()?;
        let first = match field(&begun, "first") {
            Some(HostValue::Number(n)) => *n,
            _ => return None,
        };
        let mut patches = Vec::with_capacity(feeds.len());
        if let Some(HostValue::Array(list)) = field(&begun, "patches") {
            for words in list {
                let HostValue::Array(words) = words else {
                    return None;
                };
                patches.push(
                    words
                        .chunks_exact(2)
                        .map(|p| match p {
                            [HostValue::Number(w), HostValue::Number(b)] => {
                                (*w as usize, *b as u32)
                            }
                            _ => (usize::MAX, 0),
                        })
                        .collect::<Vec<_>>(),
                );
            }
        }
        let submitted = std::time::Instant::now();
        {
            let mut gpu = self.gpu.borrow_mut();
            gpu.replay_phase(0, (checked - started).as_secs_f64() * 1000.0, true);
            gpu.replay_phase(1, (submitted - checked).as_secs_f64() * 1000.0, false);
        }
        let result = if patches.len() == feeds.len() {
            self.gpu.borrow_mut().replay_run(token, feeds, &patches)
        } else {
            Err((
                "GPU".into(),
                "the replay's uniforms were not computed".into(),
            ))
        };
        let ending = std::time::Instant::now();
        let ended = self.state.call_slot(
            self.end_slot,
            &[
                HostValue::String(token.to_owned()),
                HostValue::Number(count),
                HostValue::Bool(result.is_ok()),
                HostValue::Number(first),
            ],
        );
        let values = match result {
            Ok(values) => values,
            Err((code, message)) => return Some(Replayed::Failed(code, message)),
        };
        self.gpu
            .borrow_mut()
            .replay_phase(7, ending.elapsed().as_secs_f64() * 1000.0, false);
        let session_step = match ended.as_ref().ok().and_then(|v| field(v, "step")) {
            Some(HostValue::Number(n)) => *n,
            _ => first + count,
        };
        let peak = ended
            .as_ref()
            .ok()
            .and_then(|v| field(v, "peak"))
            .cloned()
            .unwrap_or(HostValue::Null);
        let (upload, read) = {
            let gpu = self.gpu.borrow();
            let replay = gpu.replays.get(token)?;
            (
                replay.feeds.iter().map(|f| f.size).sum::<usize>() * feeds.len(),
                replay.outputs.iter().map(|o| o.size).sum::<usize>() * feeds.len(),
            )
        };
        let ms = |d: std::time::Duration| HostValue::Number(d.as_secs_f64() * 1000.0);
        let mut stats = vec![("steps".to_string(), HostValue::Number(count))];
        for key in [
            "nodes",
            "estimatedWork",
            "logicalAllocationBytes",
            "residentBytes",
        ] {
            stats.push((
                key.into(),
                field(&begun, key).cloned().unwrap_or(HostValue::Null),
            ));
        }
        stats.extend([
            ("uploadElements".into(), HostValue::Number(upload as f64)),
            ("readbackElements".into(), HostValue::Number(read as f64)),
            ("submitWallMs".into(), ms(submitted - started)),
            ("readbackWallMs".into(), ms(submitted.elapsed())),
            ("totalWallMs".into(), ms(started.elapsed())),
            ("webgpuBufferPeakBytes".into(), peak),
            ("adapter".into(), HostValue::String(self.summary.describe())),
            ("replayed".into(), HostValue::Bool(true)),
        ]);
        Some(Replayed::Ran(ReplayRan {
            first,
            session_step,
            values,
            stats,
        }))
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
        if let Some((run, take)) = self.profile {
            eprintln!("zipp-gpu host: {run:.1} ms serving requests in the runtime, {take:.1} ms taking replies");
        }
        if let Ok(slot) = slot_of(&self.state, "__zgpuDispose") {
            let _ = self.state.call_slot(slot, &[]);
        }
    }
}

/// A replay's outcome, before it is shaped into a reply.
enum Replayed {
    Ran(ReplayRan),
    /// Device work had begun: the session is poisoned, as gpu-lab leaves it.
    Failed(String, String),
}

struct ReplayRan {
    first: f64,
    session_step: f64,
    /// Each step's read-back values, in the replay's output order.
    values: Vec<Vec<f32>>,
    stats: Vec<(String, HostValue)>,
}

/// What [`GpuHost::replay_binary`] ran.
pub enum BinaryRun {
    Ran {
        /// The run's first step number, and the session's step after it.
        first: f64,
        step: f64,
        /// (name, elements, shape) of each read-back output, in the order
        /// each step's values follow one another in `values`.
        outputs: Vec<(String, usize, Vec<f64>)>,
        values: Vec<Vec<f32>>,
        stats: HostValue,
    },
    Failed {
        code: String,
        message: String,
    },
}

/// Whether `readback` names exactly the replay's read-back outputs.
fn reads_back(replay: &replay::Replay, readback: &[&str]) -> bool {
    let mut want: Vec<&str> = readback.to_vec();
    want.sort_unstable();
    want.dedup();
    let mut have: Vec<&str> = replay.outputs.iter().map(|o| o.name.as_str()).collect();
    have.sort_unstable();
    want == have && want.len() == readback.len()
}

/// gpu-lab's checks of a fed value (float32Data, checkClassTargets,
/// checkIndices): anything they would refuse goes to gpu-lab.
fn feed_accepted(feed: &replay::Feed, data: &[u8]) -> bool {
    match feed.classes.or(feed.bound) {
        // Finiteness only: the exponent bits of every value, folded over
        // blocks without an early exit, so the scan vectorizes.
        None => data.chunks(256).all(|block| {
            block.chunks_exact(4).fold(0u32, |bad, b| {
                let bits = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                bad | ((bits & 0x7f80_0000 == 0x7f80_0000) as u32)
            }) == 0
        }),
        Some(n) => data.chunks_exact(4).all(|b| {
            let v = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            let v = v as f64;
            v.is_finite() && v.fract() == 0.0 && v >= 0.0 && v < n
        }),
    }
}

fn replay_failure(code: String, message: String) -> HostValue {
    HostValue::Object(vec![
        ("ok".into(), HostValue::Bool(false)),
        (
            "error".into(),
            HostValue::Object(vec![
                ("code".into(), HostValue::String(code)),
                (
                    "message".into(),
                    HostValue::String(message.chars().take(512).collect()),
                ),
                ("poisoned".into(), HostValue::Bool(true)),
            ]),
        ),
    ])
}

fn reply_ok(reply: &HostValue) -> bool {
    matches!(field(reply, "ok"), Some(HostValue::Bool(true)))
}

/// Two consecutive runs' replies as one run's: the steps of both, the later
/// run's other fields (its step count and element counts summed with the
/// earlier's), or the later one's failure.
fn merge_runs(head: HostValue, tail: HostValue) -> HostValue {
    if !reply_ok(&tail) {
        return tail;
    }
    let steps_of = |r: &HostValue| match field(r, "value").and_then(|v| field(v, "steps")) {
        Some(HostValue::Array(a)) => a.clone(),
        _ => Vec::new(),
    };
    let stat = |r: &HostValue, k: &str| match field(r, "value")
        .and_then(|v| field(v, "stats"))
        .and_then(|s| field(s, k))
    {
        Some(HostValue::Number(n)) => *n,
        _ => 0.0,
    };
    let mut steps = steps_of(&head);
    steps.extend(steps_of(&tail));
    let HostValue::Object(mut pairs) = tail.clone() else {
        return tail;
    };
    for (key, value) in pairs.iter_mut() {
        let (true, HostValue::Object(fields)) = (key == "value", value) else {
            continue;
        };
        for (k, v) in fields.iter_mut() {
            match (k.as_str(), v) {
                ("steps", v) => *v = HostValue::Array(steps.clone()),
                ("outputs", v) => {
                    if let Some(HostValue::Object(last)) = steps.last() {
                        if let Some((_, o)) = last.iter().find(|(n, _)| n == "outputs") {
                            *v = o.clone();
                        }
                    }
                }
                ("stats", HostValue::Object(stats)) => {
                    for (sk, sv) in stats.iter_mut() {
                        if matches!(
                            sk.as_str(),
                            "steps" | "uploadElements" | "readbackElements" | "estimatedWork"
                        ) {
                            *sv = HostValue::Number(stat(&head, sk) + stat(&tail, sk));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    HostValue::Object(pairs)
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
