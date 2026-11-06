//! A `map`/`filter`/`reduce` callback that reads captured variables runs in
//! its fused kernel (`Vm::kernel_upvals` stores the captured values past the
//! callback's registers; `ZIPP_NO_KERNEL_UPVALS` sends it per element again):
//! values that change between calls but not during one, a capture still in
//! its temporal dead zone (the per-element path throws), and double/string
//! captures where the kernel bails to the per-element tail. The expected
//! lines are Node's (v24), with kernels, without them and in the interpreter.

const SOURCE: &str = r##"
"use strict";
const N = []; for (let i = 0; i < 1000; i++) N.push((i * 7) % 1000 - 500);
let s = 0;
for (let r = 0; r < 2000; r++) {
  const k = r & 31, m = (r % 7) + 1;
  const t = N.map((x) => x * m + k).filter((x) => (x & 3) !== (k & 3)).reduce((acc, x) => (acc + x * m) | 0, k);
  s = (s + t) | 0;
}
console.log(s);
// captured values change between calls but not during one
let bias = 0;
const add = (x) => x + bias;
let u = 0;
for (let r = 0; r < 50; r++) { bias = r; u = (u + N.map(add)[r]) | 0; }
console.log(u);
// a TDZ capture: the per-element path throws as the interpreter does
try { const f = (x) => x + later; [1, 2].map(f); let later = 1; } catch (e) { console.log(e.constructor.name); }
// a double and a string capture fall back per element where the kernel bails
const half = 0.5, tag = "x";
console.log(N.slice(0, 5).map((x) => x * half).join(), N.slice(0, 3).map((x) => x + tag).join());
"##;

const EXPECTED: &str = "70495622\n-15200\nReferenceError\n-250,-246.5,-243,-239.5,-236 -500x,-493x,-486x";

#[test]
fn kernel_upvals_worker() {
    if std::env::var_os("ZIPP_KERNEL_UPVALS_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn kernel_upvals_match_node() {
    if std::env::var_os("ZIPP_KERNEL_UPVALS_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, var) in [("default", None), ("upvals-off", Some("ZIPP_NO_KERNEL_UPVALS")), ("interpreter", Some("ZIPP_NOJIT"))] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["kernel_upvals_worker", "--exact", "--nocapture"])
            .env("ZIPP_KERNEL_UPVALS_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_NO_KERNEL_UPVALS");
        if let Some(v) = var {
            command.env(v, "1");
        }
        let out = command.output().expect("spawn the worker");
        assert!(
            out.status.success(),
            "{mode} diverged from node:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}