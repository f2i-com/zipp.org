//! gpu-lab's runtime timed on its own (no Python) on the native backend:
//! `tests/engine_bench.js`, the function the browser benchmark
//! (crates/zipp-cli/tests/native_gpu/bench_browser.py) runs in Chrome.
//!
//! ```text
//! cargo test --release -p zipp-gpu --test engine_bench -- --ignored --nocapture
//! ENGINE_BENCH_OPTS='{"only":["mm_2048"],"reps":30}' cargo test ...
//! ```
//! `ENGINE_BENCH_BACKEND` picks gpu-lab's backend (default `webgpu`).
//! `ENGINE_BENCH_EXTRA` names a JavaScript file evaluated after the bench
//! (it may define `engineExtra(M)`, whose result is printed instead).
use zipp_gpu::{GpuHost, GpuOptions};

fn on_big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(f)
        .expect("spawn")
        .join()
        .expect("bench thread")
}

#[test]
#[ignore]
fn engine_bench() {
    on_big_stack(|| {
        if zipp_gpu::disabled_by_env() {
            return;
        }
        let options = GpuOptions {
            with_cases: true,
            ..GpuOptions::from_env()
        };
        let Some(mut host) = GpuHost::open(&options).expect("runtime") else {
            eprintln!("skipped: no hardware GPU adapter");
            return;
        };
        eprintln!("adapter: {}", host.adapter().describe());
        let bench = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/engine_bench.js"
        ))
        .unwrap();
        host.eval(&format!(
            "{bench}\nglobalThis.engineBench = engineBench; null"
        ))
        .expect("bench source");
        let mut call = format!(
            "engineBench(M, '{}', {})",
            std::env::var("ENGINE_BENCH_BACKEND").unwrap_or_else(|_| "webgpu".into()),
            std::env::var("ENGINE_BENCH_OPTS").unwrap_or_else(|_| "{}".into())
        );
        if let Ok(path) = std::env::var("ENGINE_BENCH_EXTRA") {
            let extra = std::fs::read_to_string(path).unwrap();
            host.eval(&format!(
                "{extra}\nglobalThis.engineExtra = engineExtra; null"
            ))
            .expect("extra source");
            call = "engineExtra(M)".into();
        }
        host.eval(&format!(
            "var __zgpuOut = null; var M = Object.assign({{}}, __zgpuModules['src/runtime.mjs'], __zgpuModules['tests/ml-cases.mjs'], \
             {{backends: {{webgpu: __zgpuModules['src/backends/webgpu.mjs']}}}});\
             Promise.resolve().then(() => {call}).then(\
             (v) => {{ __zgpuOut = JSON.stringify(v); }}, (e) => {{ __zgpuOut = 'ERROR ' + String(e && e.stack || e); }}); null"
        ))
        .expect("eval");
        match host.eval("__zgpuOut").expect("read") {
            zipp_vm::embed::JsValue::String(s) => println!("ENGINE {s}"),
            other => panic!("the bench did not finish: {other:?}"),
        }
    });
}
