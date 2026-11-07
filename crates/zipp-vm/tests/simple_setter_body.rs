//! A setter compiled code runs off-frame (`Vm::accessor_fast_set`) must be
//! a store and nothing else: `simple_setter_field` matched only a setter's
//! first instructions, so `set y(v) { this.k = v; if (v === 5000) calls++; }`
//! stored and skipped the rest. The expected lines are Node's (v24), in the
//! JIT and the interpreter.

const SOURCE: &str = r##"
"use strict";
function S() { this.k = 0; }
let calls = 0;
Object.defineProperty(S.prototype, "y", { get() { return this.k; }, set(v) { this.k = v; if (v === 5000) { calls++; } }, configurable: true });
const ss = []; for (let i = 0; i < 4; i++) ss.push(new S());
for (let i = 0; i < 40000; i++) { const s = ss[i & 3]; s.y = s.y + 1; }
let n = 0;
function T() { this.k = 0; }
Object.defineProperty(T.prototype, "z", { set(v) { this.k = v | 0; n += v; }, configurable: true });
const ts = []; for (let i = 0; i < 4; i++) ts.push(new T());
for (let i = 0; i < 40000; i++) ts[i & 3].z = i;
function U() { this.k = 0; }
Object.defineProperty(U.prototype, "w", { set(v) { this.k = v; }, configurable: true });
const us = []; for (let i = 0; i < 4; i++) us.push(new U());
for (let i = 0; i < 40000; i++) us[i & 3].w = i;
console.log("calls", calls, ss[0].k, n, ts[3].k, us[1].k);
"##;

const EXPECTED: &str = "calls 4 10000 799980000 39999 39997";

#[test]
fn simple_setter_body_worker() {
    if std::env::var_os("ZIPP_SIMPLE_SETTER_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn simple_setter_body_matches_node() {
    if std::env::var_os("ZIPP_SIMPLE_SETTER_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, nojit) in [("default", false), ("interpreter", true)] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["simple_setter_body_worker", "--exact", "--nocapture"])
            .env("ZIPP_SIMPLE_SETTER_WORKER", "1")
            .env_remove("ZIPP_NOJIT");
        if nojit {
            command.env("ZIPP_NOJIT", "1");
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
