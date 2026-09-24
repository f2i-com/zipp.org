//! Native replay of a prepared session's step.
//!
//! A prepared step's commands are the same every step once the session has
//! run: the same kernels over the same buffers, with only the fed batch, the
//! step-dependent uniforms (Adam's bias correction, a `uniform` draw's step)
//! and the read-back values changing. gpu-lab records them in JavaScript each
//! step (validation, the node loop, one uniform slot and bind group per
//! dispatch), which on a small model is most of a step.
//!
//! So the driver (`js/driver.js`) captures one ordinary single-step run of a
//! session: the command script the shim hands [`WebGpu`] at `finish`, the
//! uniform bytes, which buffer each fed input and each read-back output
//! landed in, which uniform words depend on the step, and the buffers the
//! session holds (weights, optimizer state, gradients, constants). From that
//! [`WebGpu::replay_create`] builds a [`Replay`]: the same passes, pipelines
//! and dispatches over the session's held buffers, with every other buffer
//! (feeds, intermediates, the uniforms, the read-back staging) its own, so no
//! other graph or session can touch them. A later request for that session
//! (`crate::GpuHost::try_replay`) writes the feeds straight from the Python
//! program's upload region, patches the uniforms with words gpu-lab itself
//! computed for the step (`__zgpuReplayBegin`), submits one command buffer per
//! step and maps the read-backs once. The arithmetic is the recorded
//! kernels' over the same values, so every result is the same bits as
//! gpu-lab's own run; anything a replay does not cover (another feed set or
//! read-back list, a value that fails a check, an unusable session) takes
//! gpu-lab's path, which produces the result or error it always did.
use std::collections::{HashMap, HashSet};
use std::num::NonZeroU64;

use zipp_vm::embed::HostCtx;

use crate::device::{error_text, int, region, WebGpu};

/// One fed input of a replayed step.
pub(crate) struct Feed {
    pub(crate) id: u32,
    buffer: wgpu::Buffer,
    /// Elements.
    pub(crate) size: usize,
    /// Class targets: every value an integer in [0, classes).
    pub(crate) classes: Option<f64>,
    /// An index: every value an integer in [0, bound).
    pub(crate) bound: Option<f64>,
}

/// One read-back output of a replayed step.
pub(crate) struct Output {
    pub(crate) name: String,
    buffer: wgpu::Buffer,
    pub(crate) size: usize,
    pub(crate) shape: Vec<f64>,
    /// Byte offset within one step's read-back.
    offset: u64,
}

enum PassOp {
    Pipeline(wgpu::ComputePipeline),
    Group(u32, wgpu::BindGroup, Vec<u32>),
    Dispatch(u32, u32, u32),
}

enum Op {
    Pass(Vec<PassOp>),
    Copy {
        src: wgpu::Buffer,
        src_offset: u64,
        dst: wgpu::Buffer,
        dst_offset: u64,
        size: u64,
    },
}

/// A prepared session's step, replayable natively.
pub(crate) struct Replay {
    device_id: u32,
    device: wgpu::Device,
    queue: wgpu::Queue,
    ops: Vec<Op>,
    uniform: wgpu::Buffer,
    template: Vec<u8>,
    pub(crate) feeds: Vec<Feed>,
    pub(crate) outputs: Vec<Output>,
    /// Bytes one step reads back.
    step_bytes: u64,
    staging: Option<(wgpu::Buffer, u64)>,
    /// Bumped when the read-back buffer is replaced.
    staging_generation: u64,
    /// The next run's first step, encoded ahead (with the read-back
    /// buffer's generation it copies into).
    ready: Option<(wgpu::CommandBuffer, u64)>,
    /// Buffers this replay made (kept alive with it).
    _owned: Vec<wgpu::Buffer>,
}

impl Replay {
    /// One step's commands: the recorded passes and copies, then its
    /// read-backs into the staging buffer at step `s`'s offset.
    fn encode(&self, device: &wgpu::Device, s: u64) -> wgpu::CommandBuffer {
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        for op in &self.ops {
            match op {
                Op::Pass(list) => {
                    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: None,
                        timestamp_writes: None,
                    });
                    for p in list {
                        match p {
                            PassOp::Pipeline(pipeline) => pass.set_pipeline(pipeline),
                            PassOp::Group(index, group, offsets) => {
                                pass.set_bind_group(*index, group, &offsets[..])
                            }
                            PassOp::Dispatch(x, y, z) => pass.dispatch_workgroups(*x, *y, *z),
                        }
                    }
                }
                Op::Copy {
                    src,
                    src_offset,
                    dst,
                    dst_offset,
                    size,
                } => encoder.copy_buffer_to_buffer(src, *src_offset, dst, *dst_offset, Some(*size)),
            }
        }
        if let Some((staging, _)) = &self.staging {
            let base = s * self.step_bytes;
            for o in &self.outputs {
                encoder.copy_buffer_to_buffer(
                    &o.buffer,
                    0,
                    staging,
                    base + o.offset,
                    Some(o.size as u64 * 4),
                );
            }
        }
        encoder.finish()
    }
}

/// Why a replay failed after device work began: `(code, message)`.
pub(crate) type Failure = (String, String);

fn field<'a>(v: &'a serde_json::Value, name: &str) -> Result<&'a serde_json::Value, String> {
    v.get(name)
        .ok_or_else(|| format!("replay spec has no {name}"))
}
fn num_u32(v: &serde_json::Value) -> Result<u32, String> {
    v.as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| "replay spec: not an id".to_string())
}

impl WebGpu {
    /// `gpu.replayCreate(device, token, spec, templateBytes)`: build a replay
    /// from the one script captured since `gpu.capture` and the driver's spec
    /// (`js/driver.js` `__zgpuReplayRecord`). `"1"` when it was built.
    pub(crate) fn replay_create(
        &mut self,
        ctx: &mut dyn HostCtx,
        args: &[String],
    ) -> Result<String, String> {
        let token = args.first().ok_or("TypeError: replay token")?.clone();
        let spec: serde_json::Value =
            serde_json::from_str(args.get(1).ok_or("TypeError: replay spec")?)
                .map_err(|e| format!("TypeError: replay spec: {e}"))?;
        let length = int(args, 2)? as usize;
        let template = region(ctx, "__zgpuUp", length)?[..length].to_vec();
        let scripts = std::mem::take(&mut self.captured);
        match self.build_replay(&spec, template, &scripts) {
            Ok(replay) => {
                self.replays.insert(token, replay);
                Ok("1".into())
            }
            Err(why) => {
                if std::env::var_os("ZIPP_GPU_LOG").is_some() {
                    eprintln!("zipp-gpu: no native replay for this session: {why}");
                }
                Ok("0".into())
            }
        }
    }

    fn build_replay(
        &self,
        spec: &serde_json::Value,
        template: Vec<u8>,
        scripts: &[String],
    ) -> Result<Replay, String> {
        let [script] = scripts else {
            return Err(format!(
                "{} command buffers captured, one expected",
                scripts.len()
            ));
        };
        let dev = self.dev()?;
        let (device, queue) = (dev.device.clone(), dev.queue.clone());
        let held: HashSet<u32> = field(spec, "held")?
            .as_array()
            .ok_or("held")?
            .iter()
            .map(num_u32)
            .collect::<Result<_, _>>()?;
        let uniform_id = num_u32(field(spec, "uniform")?)?;
        let staging_id = field(spec, "staging")?.as_i64().unwrap_or(-1);
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("zipp-gpu replay uniforms"),
            size: (template.len() as u64).max(256).div_ceil(256) * 256,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut owned = vec![uniform.clone()];
        // Every buffer the script names, as the replay sees it: the session's
        // held ones themselves, the uniforms and every transient one its own.
        let mut map: HashMap<u32, wgpu::Buffer> = HashMap::new();
        let mut resolve =
            |id: u32, owned: &mut Vec<wgpu::Buffer>| -> Result<wgpu::Buffer, String> {
                if let Some(b) = map.get(&id) {
                    return Ok(b.clone());
                }
                let entry = self
                    .buffers
                    .get(&id)
                    .ok_or_else(|| format!("buffer {id} is gone"))?;
                let buffer = if held.contains(&id) {
                    entry.buffer.clone()
                } else if id == uniform_id {
                    uniform.clone()
                } else {
                    let b = device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("zipp-gpu replay"),
                        size: entry.size,
                        usage: wgpu::BufferUsages::STORAGE
                            | wgpu::BufferUsages::COPY_SRC
                            | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    });
                    owned.push(b.clone());
                    b
                };
                map.insert(id, buffer.clone());
                Ok(buffer)
            };
        let mut groups: HashMap<u32, wgpu::BindGroup> = HashMap::new();
        let numbers = |text: &str| -> Result<Vec<u64>, String> {
            text.split(',')
                .map(|p| {
                    p.parse::<u64>()
                        .map_err(|_| "malformed command".to_string())
                })
                .collect()
        };
        let mut ops = Vec::new();
        let mut words = script.split(';').filter(|s| !s.is_empty());
        while let Some(op) = words.next() {
            let (tag, rest) = op.split_at(1);
            match tag {
                "P" => {
                    let mut pass = Vec::new();
                    loop {
                        let op = words.next().ok_or("unterminated pass")?;
                        let (tag, rest) = op.split_at(1);
                        match tag {
                            "E" => break,
                            "S" => {
                                let key: u32 = rest.parse().map_err(|_| "pipeline")?;
                                let pipeline = self
                                    .pipelines
                                    .get(&key)
                                    .ok_or_else(|| format!("pipeline {key} is gone"))?;
                                pass.push(PassOp::Pipeline(pipeline.clone()));
                            }
                            "B" => {
                                let n = numbers(rest)?;
                                if n.len() < 2 {
                                    return Err("malformed setBindGroup".into());
                                }
                                let key = n[1] as u32;
                                if !groups.contains_key(&key) {
                                    let (layout, rows) = self
                                        .group_specs
                                        .get(&key)
                                        .ok_or_else(|| format!("bind group {key} is gone"))?;
                                    let layout = self
                                        .group_layouts
                                        .get(layout)
                                        .ok_or("bind group layout is gone")?;
                                    let buffers = rows
                                        .iter()
                                        .map(|r| resolve(r.1, &mut owned))
                                        .collect::<Result<Vec<_>, _>>()?;
                                    let entries: Vec<_> = rows
                                        .iter()
                                        .zip(&buffers)
                                        .map(|(r, buffer)| wgpu::BindGroupEntry {
                                            binding: r.0,
                                            resource: wgpu::BindingResource::Buffer(
                                                wgpu::BufferBinding {
                                                    buffer,
                                                    offset: r.2,
                                                    size: if r.3 < 0 {
                                                        None
                                                    } else {
                                                        NonZeroU64::new(r.3 as u64)
                                                    },
                                                },
                                            ),
                                        })
                                        .collect();
                                    let group =
                                        device.create_bind_group(&wgpu::BindGroupDescriptor {
                                            label: None,
                                            layout,
                                            entries: &entries,
                                        });
                                    groups.insert(key, group);
                                }
                                let offsets = n[2..].iter().map(|&v| v as u32).collect();
                                pass.push(PassOp::Group(
                                    n[0] as u32,
                                    groups[&key].clone(),
                                    offsets,
                                ));
                            }
                            "D" => {
                                let n = numbers(rest)?;
                                if n.len() != 3 {
                                    return Err("malformed dispatch".into());
                                }
                                pass.push(PassOp::Dispatch(n[0] as u32, n[1] as u32, n[2] as u32));
                            }
                            other => return Err(format!("unknown pass command {other}")),
                        }
                    }
                    ops.push(Op::Pass(pass));
                }
                "C" => {
                    let n = numbers(rest)?;
                    if n.len() != 5 {
                        return Err("malformed copy".into());
                    }
                    // The run's own read-back: the replay reads back itself.
                    if n[2] as i64 == staging_id {
                        continue;
                    }
                    ops.push(Op::Copy {
                        src: resolve(n[0] as u32, &mut owned)?,
                        src_offset: n[1],
                        dst: resolve(n[2] as u32, &mut owned)?,
                        dst_offset: n[3],
                        size: n[4],
                    });
                }
                other => return Err(format!("unknown command {other}")),
            }
        }
        let mut outputs = Vec::new();
        let mut step_bytes = 0u64;
        for o in field(spec, "outputs")?.as_array().ok_or("outputs")? {
            let size = field(o, "size")?.as_u64().ok_or("output size")? as usize;
            let buffer = resolve(num_u32(field(o, "buffer")?)?, &mut owned)?;
            outputs.push(Output {
                name: field(o, "name")?.as_str().ok_or("output name")?.to_owned(),
                buffer,
                size,
                shape: field(o, "shape")?
                    .as_array()
                    .ok_or("output shape")?
                    .iter()
                    .map(|v| v.as_f64().unwrap_or(0.0))
                    .collect(),
                offset: step_bytes,
            });
            step_bytes += size as u64 * 4;
        }
        let mut feeds = Vec::new();
        for f in field(spec, "feeds")?.as_array().ok_or("feeds")? {
            let id = num_u32(field(f, "buffer")?)?;
            if held.contains(&id) || !map.contains_key(&id) {
                return Err("a fed input's buffer is not the step's own".into());
            }
            feeds.push(Feed {
                id: num_u32(field(f, "id")?)?,
                buffer: map[&id].clone(),
                size: field(f, "size")?.as_u64().ok_or("feed size")? as usize,
                classes: f.get("classes").and_then(|v| v.as_f64()),
                bound: f.get("bound").and_then(|v| v.as_f64()),
            });
        }
        queue.write_buffer(&uniform, 0, &template);
        Ok(Replay {
            device_id: self.current,
            device,
            queue,
            ops,
            uniform,
            template,
            feeds,
            outputs,
            step_bytes,
            staging: None,
            staging_generation: 0,
            ready: None,
            _owned: owned,
        })
    }

    /// Run `feeds.len()` steps of the session `token`'s replay: `feeds[s][i]`
    /// is step `s`'s bytes for `Replay::feeds[i]`, `patches[s]` its uniform
    /// words as (word index, bits). Returns each step's read-back values, in
    /// `Replay::outputs` order, or the failure once device work has begun.
    pub(crate) fn replay_run(
        &mut self,
        token: &str,
        feeds: &[Vec<&[u8]>],
        patches: &[Vec<(usize, u32)>],
    ) -> Result<Vec<Vec<f32>>, Failure> {
        let gpu_error = |m: String| ("GPU".to_string(), m);
        let replay = self
            .replays
            .get_mut(token)
            .ok_or_else(|| gpu_error("the session's replay is gone".into()))?;
        let (device, queue) = (replay.device.clone(), replay.queue.clone());
        let steps = feeds.len() as u64;
        let total = replay.step_bytes * steps;
        if total > 0 && replay.staging.as_ref().is_none_or(|s| s.1 < total) {
            let size = total.max(4096);
            replay.staging_generation += 1;
            replay.staging = Some((
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("zipp-gpu replay read-back"),
                    size,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                size,
            ));
        }
        let memory = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let mut uniforms = replay.template.clone();
        let mut phase = [0.0f64; 3];
        for (s, step) in feeds.iter().enumerate() {
            let t0 = std::time::Instant::now();
            for (feed, bytes) in replay.feeds.iter().zip(step) {
                queue.write_buffer(&feed.buffer, 0, bytes);
            }
            uniforms.copy_from_slice(&replay.template);
            for &(word, bits) in &patches[s] {
                if let Some(slot) = uniforms.get_mut(word * 4..word * 4 + 4) {
                    slot.copy_from_slice(&bits.to_le_bytes());
                }
            }
            queue.write_buffer(&replay.uniform, 0, &uniforms);
            let t1 = std::time::Instant::now();
            phase[0] += (t1 - t0).as_secs_f64() * 1000.0;
            // The first step's commands were encoded while the last run's
            // were executing, unless the read-back buffer has changed since.
            let commands = match replay.ready.take() {
                Some((commands, generation))
                    if s == 0 && generation == replay.staging_generation =>
                {
                    commands
                }
                _ => replay.encode(&device, s as u64),
            };
            let t2 = std::time::Instant::now();
            phase[1] += (t2 - t1).as_secs_f64() * 1000.0;
            queue.submit([commands]);
            phase[2] += t2.elapsed().as_secs_f64() * 1000.0;
        }
        // The next run's first step, encoded while this one executes (a
        // command buffer is single-use; its commands are the same every step).
        let t3 = std::time::Instant::now();
        replay.ready = Some((replay.encode(&device, 0), replay.staging_generation));
        phase[1] += t3.elapsed().as_secs_f64() * 1000.0;
        let waited = std::time::Instant::now();
        let mut wait_ms = 0.0;
        let mut values = Vec::with_capacity(feeds.len());
        let mapped = if let (Some((staging, _)), true) = (&replay.staging, total > 0) {
            let slice = staging.slice(0..total);
            let outcome = std::sync::Arc::new(std::sync::Mutex::new(None));
            let sink = outcome.clone();
            slice.map_async(wgpu::MapMode::Read, move |r| {
                *sink.lock().unwrap() = Some(r)
            });
            device
                .poll(wgpu::PollType::wait_indefinitely())
                .map_err(|e| gpu_error(e.to_string()))?;
            wait_ms = waited.elapsed().as_secs_f64() * 1000.0;
            let result = outcome.lock().unwrap().take();
            match result {
                Some(Ok(())) => {
                    let view = slice
                        .get_mapped_range()
                        .map_err(|e| gpu_error(format!("OperationError: {e}")))?;
                    for s in 0..feeds.len() {
                        let at = s * replay.step_bytes as usize;
                        values.push(
                            view[at..at + replay.step_bytes as usize]
                                .chunks_exact(4)
                                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                                .collect::<Vec<f32>>(),
                        );
                    }
                    drop(view);
                    staging.unmap();
                    Ok(())
                }
                Some(Err(e)) => Err(gpu_error(format!("mapAsync failed: {e}"))),
                None => Err(gpu_error("mapAsync did not complete".into())),
            }
        } else {
            device
                .poll(wgpu::PollType::wait_indefinitely())
                .map_err(|e| gpu_error(e.to_string()))?;
            values.resize(feeds.len(), Vec::new());
            Ok(())
        };
        let errors = [
            pollster::block_on(validation.pop()),
            pollster::block_on(memory.pop()),
        ];
        let device_id = replay.device_id;
        let read_ms = waited.elapsed().as_secs_f64() * 1000.0 - wait_ms;
        for (i, ms) in [
            (2, phase[0]),
            (3, phase[1]),
            (4, phase[2]),
            (5, wait_ms),
            (6, read_ms),
        ] {
            self.replay_phase(i, ms, false);
        }
        if let Some(error) = errors.into_iter().flatten().next() {
            return Err(gpu_error(error_text(&error)));
        }
        mapped?;
        let current = std::mem::replace(&mut self.current, device_id);
        let lost = self.lost();
        self.current = current;
        if let Some(message) = lost {
            return Err((
                "DEVICE_LOST".into(),
                format!("WebGPU device is unavailable: {message}"),
            ));
        }
        // Readback's rule: every output value finite (gpu-lab's checkFiniteOutput).
        if values.iter().flatten().any(|v| !v.is_finite()) {
            return Err((
                "NUMBER".into(),
                "Output contains non-finite values; graph readback requires finite float32".into(),
            ));
        }
        Ok(values)
    }
}
