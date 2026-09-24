//! Native replay of prepared steps (`src/replay.rs`) against gpu-lab's own
//! path, through the bridge's request entry point (`GpuHost::request`): the
//! same session programs (gpu-lab's MLP with Adam, its dropout MLP whose
//! `uniform` masks depend on the step, and its embedding step whose feeds are
//! checked indices), the same batches and the same run pattern -- single
//! steps, eight-step and three-step runs, an explicit `step`, a refused
//! feed, a different read-back list -- on a host that replays and on one
//! that does not (`no_replay`). Every reply (each step's read-back bits, a
//! refusal's code and message, the session's step) and the downloaded
//! weights, gradients and optimizer state must be the same bits, and the
//! replaying host must actually have replayed.
use zipp_gpu::{GpuHost, GpuOptions};
use zipp_vm::embed::HostValue;

fn on_big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(f)
        .expect("spawn")
        .join()
        .expect("test thread")
}

fn open(no_replay: bool) -> Option<GpuHost> {
    if zipp_gpu::disabled_by_env() {
        return None;
    }
    let options = GpuOptions {
        with_cases: true,
        no_replay,
        ..GpuOptions::from_env()
    };
    GpuHost::open(&options).expect("runtime")
}

fn field<'a>(v: &'a HostValue, name: &str) -> Option<&'a HostValue> {
    match v {
        HostValue::Object(pairs) => pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v),
        _ => None,
    }
}

/// A reply as comparable text: success and each step's outputs as bits, or
/// the error; statistics (timings) left out.
fn canonical(reply: &HostValue, replayed: &mut usize) -> String {
    let mut out = String::new();
    match field(reply, "ok") {
        Some(HostValue::Bool(true)) => {
            let value = field(reply, "value").unwrap();
            if let Some(HostValue::Object(stats)) = field(value, "stats") {
                if stats
                    .iter()
                    .any(|(k, v)| k == "replayed" && *v == HostValue::Bool(true))
                {
                    *replayed += 1;
                }
            }
            out.push_str(&format!("step {:?};", field(value, "step")));
            if let Some(HostValue::Array(steps)) = field(value, "steps") {
                for s in steps {
                    out.push_str(&format!("[{:?}", field(s, "step")));
                    if let Some(HostValue::Object(outputs)) = field(s, "outputs") {
                        let mut names: Vec<_> = outputs.iter().collect();
                        names.sort_by(|a, b| a.0.cmp(&b.0));
                        for (name, o) in names {
                            out.push_str(&format!(" {name} {:?}", field(o, "shape")));
                            if let Some(HostValue::Float32Array(data)) = field(o, "data") {
                                for v in data {
                                    out.push_str(&format!(" {:08x}", v.to_bits()));
                                }
                            }
                        }
                    }
                    out.push(']');
                }
            }
            if let Some(HostValue::Object(outputs)) = field(value, "outputs") {
                // A download's outputs.
                let mut names: Vec<_> = outputs.iter().collect();
                names.sort_by(|a, b| a.0.cmp(&b.0));
                for (name, o) in names {
                    out.push_str(&format!(" {name}"));
                    if let Some(HostValue::Float32Array(data)) = field(o, "data") {
                        for v in data {
                            out.push_str(&format!(" {:08x}", v.to_bits()));
                        }
                    }
                }
            }
        }
        _ => out.push_str(&format!("error {:?}", field(reply, "error"))),
    }
    out
}

/// `{"session": token, "steps": [...], "readback": [...], "step"?}` with each
/// feed's values as float32 bytes.
fn run_body(
    token: &str,
    batches: &[serde_json::Value],
    readback: &serde_json::Value,
    step: Option<u64>,
    poison: bool,
) -> (serde_json::Value, Vec<u8>) {
    let mut bytes = Vec::new();
    let mut steps = Vec::new();
    for (b, batch) in batches.iter().enumerate() {
        let mut inputs = serde_json::Map::new();
        for (id, values) in batch["inputs"].as_object().unwrap() {
            let at = bytes.len();
            let values = values.as_array().unwrap();
            for (i, v) in values.iter().enumerate() {
                let mut x = v.as_f64().unwrap() as f32;
                if poison && b == 0 && i == 3 {
                    x = f32::NAN;
                }
                bytes.extend_from_slice(&x.to_le_bytes());
            }
            inputs.insert(id.clone(), serde_json::json!({"$f32": [at, values.len()]}));
        }
        steps.push(serde_json::json!({"inputs": inputs}));
    }
    let mut body = serde_json::json!({"session": token, "steps": steps, "readback": readback});
    if let Some(step) = step {
        body["step"] = serde_json::json!(step);
    }
    (body, bytes)
}

/// A graph with intermediates of every size the cases use.
fn interloper() -> serde_json::Value {
    let a: Vec<f32> = (0..32 * 784)
        .map(|i| ((i * 7) % 13) as f32 / 13.0 - 0.5)
        .collect();
    let b: Vec<f32> = (0..784 * 64)
        .map(|i| ((i * 5) % 11) as f32 / 11.0 - 0.5)
        .collect();
    serde_json::json!({"version": 2, "nodes": [
        {"id": 0, "op": "input", "shape": [32, 784], "data": a},
        {"id": 1, "op": "input", "shape": [784, 64], "data": b},
        {"id": 2, "op": "matmul", "a": 0, "b": 1},
        {"id": 3, "op": "relu", "a": 2},
        {"id": 4, "op": "sum", "a": 3}], "outputs": [{"name": "r", "id": 4}]})
}

/// One case's whole run on `host`: every reply as canonical text, and how
/// many runs were replayed.
fn drive(host: &mut GpuHost, case: &serde_json::Value) -> (Vec<String>, usize) {
    let mut replayed = 0;
    let create = serde_json::json!({"program": case["program"], "resident": case["resident"]});
    let created = host.request("gpu.session.create", &create, &[]).unwrap();
    let value = field(&created, "value").expect("session created");
    let Some(HostValue::String(token)) = field(value, "session") else {
        panic!("no session token: {created:?}")
    };
    let token = token.clone();
    let batches = case["batches"].as_array().unwrap();
    let readback = &case["readback"];
    let mut log = Vec::new();
    let mut at = 0;
    // (steps in the run, explicit step, a NaN in the first feed, other read-back)
    let pattern: &[(usize, Option<u64>, bool, bool)] = &[
        (1, None, false, false),
        (1, None, false, false),
        (1, None, false, false),
        (1, None, false, false),
        (8, None, false, false),
        (3, None, false, false),
        (1, None, true, false),
        (2, Some(40), false, false),
        (1, None, false, true),
        (1, None, false, false),
        (8, None, false, false),
    ];
    for &(count, step, poison, other) in pattern {
        let chosen: Vec<_> = (0..count)
            .map(|i| batches[(at + i) % batches.len()].clone())
            .collect();
        at += count;
        let rb = if other {
            serde_json::json!([])
        } else {
            readback.clone()
        };
        let (body, bytes) = run_body(&token, &chosen, &rb, step, poison);
        let reply = host.request("gpu.session.run", &body, &bytes).unwrap();
        log.push(canonical(&reply, &mut replayed));
        // Unrelated work between steps takes buffers from the same pool.
        let other = host.request("gpu.execute", &interloper(), &[]).unwrap();
        log.push(canonical(&other, &mut replayed));
    }
    let names = case["resident"].clone();
    let download = host
        .request(
            "gpu.session.download",
            &serde_json::json!({"session": token, "names": names}),
            &[],
        )
        .unwrap();
    log.push(canonical(&download, &mut replayed));
    host.request(
        "gpu.session.dispose",
        &serde_json::json!({"session": token}),
        &[],
    )
    .unwrap();
    (log, replayed)
}

#[test]
fn replayed_steps_are_gpu_labs_steps_bit_for_bit() {
    on_big_stack(|| {
        let Some(mut fast) = open(false) else {
            eprintln!("skipped: no hardware GPU adapter");
            return;
        };
        let mut slow = open(true).expect("second host");
        let cases = match fast
            .eval(
                r#"(() => {
  const M = __zgpuModules['tests/ml-cases.mjs'];
  const plain = (k, v) => (v instanceof Float32Array || v instanceof Uint8Array) ? Array.from(v) : v;
  const mlp = M.mlpSessionProgram({sizes: [784, 64, 10], batch: 32, lr: 0.002}), rnd = M.seeded(11);
  const batches = Array.from({length: 12}, () => ({inputs: {0: Array.from({length: 32 * 784}, () => rnd(0, 1)),
    1: Array.from({length: 32}, () => Math.floor(rnd(0, 10)))}}));
  const d = M.dropoutSessionProgram(), e = M.embeddingSessionProgram();
  return JSON.stringify([
    {name: 'mlp', program: mlp.program, resident: mlp.resident, batches, readback: ['loss']},
    {name: 'dropout', program: d.program, resident: d.resident, batches: d.batches(12), readback: ['loss', 'noise']},
    {name: 'embedding', program: e.program, resident: e.resident, batches: e.batches(12), readback: ['loss', 'rows']},
  ], plain);
})()"#,
            )
            .expect("cases")
        {
            zipp_vm::embed::JsValue::String(s) => s,
            other => panic!("cases: {other:?}"),
        };
        let cases: serde_json::Value = serde_json::from_str(&cases).unwrap();
        for case in cases.as_array().unwrap() {
            let (a, replayed) = drive(&mut fast, case);
            let (b, none) = drive(&mut slow, case);
            let name = case["name"].as_str().unwrap();
            assert_eq!(none, 0, "{name}: the host without replay replayed");
            assert!(replayed >= 5, "{name}: only {replayed} runs were replayed");
            assert_eq!(a.len(), b.len());
            for (i, (x, y)) in a.iter().zip(&b).enumerate() {
                assert_eq!(x, y, "{name}: request {i} differs");
            }
            eprintln!(
                "{name}: {} requests the same bits, {replayed} replayed",
                a.len()
            );
        }
    });
}

/// Host-side time of a prepared step through the bridge's entry point,
/// replayed and through gpu-lab (no Python):
/// `cargo test --release -p zipp-gpu --test replay -- --ignored --nocapture`.
#[test]
#[ignore]
fn replay_timing() {
    on_big_stack(|| {
        for no_replay in [false, true] {
            let Some(mut host) = open(no_replay) else {
                return;
            };
            for (sizes, batch) in [("[784, 256, 10]", 64), ("[784, 1024, 1024, 10]", 256)] {
                let case = match host
                    .eval(&format!(
                        r#"(() => {{
  const M = __zgpuModules['tests/ml-cases.mjs'];
  const plain = (k, v) => (v instanceof Float32Array) ? Array.from(v) : v;
  const s = M.mlpSessionProgram({{sizes: {sizes}, batch: {batch}, lr: 0.002}});
  return JSON.stringify({{program: s.program, resident: s.resident, readback: ['loss']}}, plain);
}})()"#
                    ))
                    .unwrap()
                {
                    zipp_vm::embed::JsValue::String(s) => s,
                    other => panic!("{other:?}"),
                };
                let case: serde_json::Value = serde_json::from_str(&case).unwrap();
                let create =
                    serde_json::json!({"program": case["program"], "resident": case["resident"]});
                let created = host.request("gpu.session.create", &create, &[]).unwrap();
                let Some(HostValue::String(token)) =
                    field(field(&created, "value").unwrap(), "session")
                else {
                    panic!()
                };
                let batches: Vec<serde_json::Value> = (0..8)
                    .map(|k| serde_json::json!({"inputs": {
                        "0": (0..batch * 784).map(|i| ((i * 31 + k * 7) % 97) as f64 / 97.0).collect::<Vec<_>>(),
                        "1": (0..batch).map(|i| ((i * 3 + k) % 10) as f64).collect::<Vec<_>>()}}))
                    .collect();
                let bodies: Vec<_> = (0..8)
                    .map(|i| run_body(token, &batches[i..i + 1], &case["readback"], None, false))
                    .collect();
                let eight = run_body(token, &batches, &case["readback"], None, false);
                let median = |mut t: Vec<f64>| {
                    t.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    t[t.len() / 2]
                };
                let mut one = Vec::new();
                for i in 0..43 {
                    let (body, bytes) = &bodies[i % 8];
                    let t = std::time::Instant::now();
                    host.request("gpu.session.run", body, bytes).unwrap();
                    if i >= 3 {
                        one.push(t.elapsed().as_secs_f64() * 1000.0);
                    }
                }
                let mut many = Vec::new();
                for i in 0..8 {
                    let t = std::time::Instant::now();
                    host.request("gpu.session.run", &eight.0, &eight.1).unwrap();
                    if i >= 1 {
                        many.push(t.elapsed().as_secs_f64() * 1000.0 / 8.0);
                    }
                }
                println!(
                    "{} {sizes} batch {batch}: step {:.3} ms, 8 per run {:.3} ms per step",
                    if no_replay { "gpu-lab" } else { "replay " },
                    median(one),
                    median(many)
                );
                host.request(
                    "gpu.session.dispose",
                    &serde_json::json!({"session": token}),
                    &[],
                )
                .unwrap();
            }
        }
    });
}
