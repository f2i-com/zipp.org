//! gpu-lab's own protocol cases (tests/browser-cases.mjs `checkBackend`, the
//! list the browser smoke runs on WebGPU and WebGL2) on the native WebGPU
//! backend, each against the cpu-js reference in the same state, with the
//! browser harness's tolerances: 2e-4 absolute plus 2e-4 relative, and bit
//! equality (a zero's sign included) for the `exact` integer and
//! data-movement cases.
//!
//! Skipped, not failed, where there is no hardware adapter (CI runners): the
//! CPU fallback is covered by the CLI's tests.
use zipp_gpu::{GpuHost, GpuOptions};

fn open() -> Option<GpuHost> {
    if zipp_gpu::disabled_by_env() {
        eprintln!("skipped: ZIPP_GPU disables the native GPU");
        return None;
    }
    let options = GpuOptions {
        with_cases: true,
        ..GpuOptions::from_env()
    };
    match GpuHost::open(&options) {
        Ok(Some(host)) => Some(host),
        Ok(None) => {
            eprintln!("skipped: no hardware GPU adapter");
            None
        }
        Err(error) => panic!("an adapter exists but the runtime did not start: {error}"),
    }
}

fn on_big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(f)
        .expect("spawn")
        .join()
        .expect("test thread")
}

/// Run an async gpu-lab expression to completion and return its JSON.
fn run_json(host: &mut GpuHost, expression: &str) -> String {
    host.eval(&format!(
        "var __zgpuOut = null; Promise.resolve().then(() => {expression}).then(\
         (v) => {{ __zgpuOut = JSON.stringify(v); }}, (e) => {{ __zgpuOut = 'ERROR ' + String(e && e.stack || e); }}); null"
    ))
    .expect("eval");
    match host.eval("__zgpuOut").expect("read") {
        zipp_vm::embed::JsValue::String(s) => s,
        other => panic!("the harness did not finish: {other:?}"),
    }
}

#[test]
fn browser_protocol_cases_on_native_webgpu() {
    on_big_stack(|| {
        let Some(mut host) = open() else { return };
        eprintln!("adapter: {}", host.adapter().describe());
        let json = run_json(
            &mut host,
            "__zgpuModules['tests/browser-cases.mjs'].checkBackend('webgpu')",
        );
        assert!(!json.starts_with("ERROR"), "{json}");
        std::fs::write(
            std::env::temp_dir().join("zipp-gpu-protocol-report.json"),
            &json,
        )
        .ok();
        let report: Vec<(String, String, f64)> = parse_checks(&json);
        let failed: Vec<_> = report.iter().filter(|(_, s, _)| s != "passed").collect();
        let worst = report.iter().map(|(_, _, e)| *e).fold(0.0f64, f64::max);
        eprintln!(
            "{} of {} cases passed; max abs error {worst:e}",
            report.len() - failed.len(),
            report.len()
        );
        assert!(failed.is_empty(), "failed: {failed:#?}");
        assert!(report.len() > 200, "only {} cases ran", report.len());
        assert!(json.contains("\"status\":\"passed\""));
    });
}

/// (name, status, maxAbsError) for every check in a `checkBackend` report.
fn parse_checks(json: &str) -> Vec<(String, String, f64)> {
    let mut out = Vec::new();
    let Some(start) = json.find("\"checks\":[") else {
        return out;
    };
    for item in json[start + 10..].split("{\"name\":").skip(1) {
        let name = item
            .split("\",\"")
            .next()
            .unwrap_or("")
            .trim_start_matches('"')
            .to_owned();
        let status = item
            .split("\"status\":\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .unwrap_or("")
            .to_owned();
        let error = item
            .split("\"maxAbsError\":")
            .nth(1)
            .and_then(|s| s.split([',', '}']).next())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0);
        out.push((name, status, error));
    }
    out
}

/// gpu-lab's own training benchmark (`benchmarkTraining`, what the browser
/// smoke reports) on the native backend: run with
/// `cargo test --release -p zipp-gpu --test protocol_cases -- --ignored --nocapture`.
#[test]
#[ignore]
fn gpu_lab_training_benchmark() {
    on_big_stack(|| {
        let Some(mut host) = open() else { return };
        for backend in ["webgpu", "cpu-js"] {
            let json = run_json(
                &mut host,
                &format!("__zgpuModules['tests/browser-cases.mjs'].benchmarkTraining('{backend}')"),
            );
            println!("{backend}: {json}");
        }
    });
}

/// Where a per-call training step's host time goes natively (diagnostic).
#[test]
#[ignore]
fn execute_time_breakdown() {
    on_big_stack(|| {
        let Some(mut host) = open() else { return };
        let json = run_json(
            &mut host,
            r#"(async () => {
  const M = __zgpuModules, {mlpTrainingStep} = M['tests/ml-cases.mjs'], {validateProgram, float32Data} = M['src/graph.mjs'];
  const {createRuntime} = M['src/runtime.mjs'];
  const {program} = mlpTrainingStep({sizes: [784, 256, 10], batch: 64});
  for (const n of program.nodes) if (n.data) n.data = Float32Array.from(n.data);
  const rt = await createRuntime({backend: 'webgpu'});
  const time = async (f, k = 5) => { await f(); const t = performance.now(); for (let i = 0; i < k; i++) await f(); return (performance.now() - t) / k; };
  let elements = 0; for (const n of program.nodes) if (n.data) elements += n.data.length;
  const out = {inputElements: elements, outputs: program.outputs.length,
    float32DataAll: await time(() => { for (const n of program.nodes) if (n.data) float32Data(n.data); }),
    validate: await time(() => validateProgram(program, {maxWork: 1e12})),
    executeLossOnly: await time(() => rt.execute({...program, outputs: [program.outputs[0]]}, {typedOutputs: true})),
    executeTyped: await time(() => rt.execute(program, {typedOutputs: true})),
  };
  rt.dispose();
  return out;
})()"#,
        );
        println!("{json}");
    });
}

/// The native `float32Data` a session's feeds go through returns what
/// gpu-lab's does and raises what it raises.
#[test]
fn accelerated_float32_data_matches_gpu_labs() {
    on_big_stack(|| {
        let Some(mut host) = open() else { return };
        let json = run_json(
            &mut host,
            r#"(() => {
  const graph = __zgpuModules['src/graph.mjs'];
  const fast = graph.float32Data;
  const data = new Float32Array(1000);
  for (let i = 0; i < data.length; i++) data[i] = (i % 7 - 3) * 0.37;
  data[5] = -0; data[6] = 3.4028234663852886e38;
  const copy = fast(data);
  const same = copy !== data && copy.length === data.length && copy.every((v, i) => Object.is(v, data[i]));
  const errors = [];
  for (const bad of [NaN, Infinity, -Infinity]) {
    const d = data.slice(); d[700] = bad;
    try { fast(d); errors.push('accepted'); } catch (e) { errors.push(e.code + ': ' + e.message); }
  }
  const small = fast(new Float32Array([1, -0, 2]));
  let list; try { fast([1, 'x']); list = 'accepted'; } catch (e) { list = e.code; }
  return {same, errors, small: Array.from(small).map(v => Object.is(v, -0) ? '-0' : v), list};
})()"#,
        );
        assert_eq!(
            json,
            r#"{"same":true,"errors":["NUMBER: Values must be finite float32 numbers","NUMBER: Values must be finite float32 numbers","NUMBER: Values must be finite float32 numbers"],"small":[1,"-0",2],"list":"NUMBER"}"#
        );
    });
}
