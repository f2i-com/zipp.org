//! A method naming its own class (`return new V(…)`, `return W`) compiles to
//! `LoadClassValue`. Tier C reads the value a class evaluated once had at
//! compile time while `Vm::mi_class_epoch` has not moved, and calls
//! `jit_load_class_value_ic` otherwise. Covered: a class evaluated once, a
//! class expression evaluated per call (each evaluation's methods see their
//! own class), hot code compiled before another class is evaluated (the
//! epoch moves, the helper answers), and a static method reached from the
//! class's own static block. The expected lines are Node's (v24), with the
//! lane, without it (`ZIPP_NO_TIERC_CLASS_CONST`) and in the interpreter.

const SOURCE: &str = r##"
"use strict";
class V { constructor(x) { this.x = x; } add(o) { return new V(this.x + o.x); } static make(x) { return new V(x); } }
let s = 0;
for (let i = 0; i < 200000; i++) { s = (s + V.make(i).add(new V(1)).x) | 0; }
console.log("fixed", s);
function mk(k) { return class C { constructor(x) { this.x = x + k; } dup() { return new C(this.x); } who() { return C; } }; }
let t = 0, same = 0;
for (let i = 0; i < 3000; i++) {
  const C = mk(i & 7);
  let c = new C(1);
  for (let j = 0; j < 50; j++) c = c.dup();
  if (c.who() === C) same++;
  t = (t + c.x) | 0;
}
console.log("repeated", t, same);
class W { m() { return W; } n() { return new W(); } }
const w = new W();
let ok = 0;
for (let i = 0; i < 200000; i++) if (w.m() === W && w.n() instanceof W) ok++;
const X = class { };
for (let i = 0; i < 200000; i++) if (w.m() === W && w.n() instanceof W) ok++;
console.log("epoch", ok, typeof X);
class S { static seen = 0; static f() { return S; } static { for (let i = 0; i < 1000; i++) if (S.f() === S) S.seen++; } }
let hot = 0;
for (let i = 0; i < 100000; i++) if (S.f() === S) hot++;
console.log("static", S.seen, hot);
"##;

const EXPECTED: &str = "fixed -1474736480\nrepeated 538500 3000\nepoch 400000 function\nstatic 1000 100000";

#[test]
fn tierc_class_const_worker() {
    if std::env::var_os("ZIPP_TIERC_CLASS_CONST_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn tierc_class_const_matches_node() {
    if std::env::var_os("ZIPP_TIERC_CLASS_CONST_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, var) in [("default", None), ("lane-off", Some("ZIPP_NO_TIERC_CLASS_CONST")), ("interpreter", Some("ZIPP_NOJIT"))] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["tierc_class_const_worker", "--exact", "--nocapture"])
            .env("ZIPP_TIERC_CLASS_CONST_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_NO_TIERC_CLASS_CONST");
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
