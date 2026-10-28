//! Class-method calls from loops that run as memory regions (the class lane:
//! `jit_method_callee` looks the method up, the arm naming its fid calls it):
//! a binary heap built per call and drained through `push`, `pop` and a getter,
//! a base method and a `super`-calling override at one site, a method replaced
//! on the prototype while hot, a throwing method, recursion through methods,
//! zero-argument calls on rotating receivers in one long loop with a callee
//! replaced halfway, and an own property shadowing the method. Run
//! interpreted, at the default thresholds, with every function compiled at
//! once, under GC stress and with the region lane off; the expected line is
//! Node 24's.

const EXPECTED: &str = "7449742,520000,0,104537,309,8175000,9990000";

#[test]
fn region_method_lanes_child() {
    if std::env::var_os("ZIPP_REGION_METHOD_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, [EXPECTED]);
}

#[test]
fn region_class_method_calls_match_node() {
    if std::env::var_os("ZIPP_REGION_METHOD_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("region-lane-off", Some(("ZIPP_NO_REGION_CROSS3_CLASS", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "region_method_lanes_child", "--nocapture"])
            .env("ZIPP_REGION_METHOD_CHILD", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS");
        if let Some((k, v)) = env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("re-run test binary");
        assert!(
            out.status.success(),
            "{mode}:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

const PROGRAM: &str = r####"// Class-method calls from loops that run as memory regions (OSR): a fresh
// receiver per outer call, methods with loops and early returns, a subclass
// override at one site, a method replaced on the prototype while hot, a
// throwing method, deep recursion, and a method reading private state.
"use strict";
const out = [];
class Heap {
  #h = [];
  push(v) { const h = this.#h; h.push(v); let i = h.length - 1; while (i > 0) { const p = (i - 1) >> 1; if (h[i] >= h[p]) break; const t = h[i]; h[i] = h[p]; h[p] = t; i = p; } }
  pop() { const h = this.#h; if (h.length === 0) return undefined; const top = h[0]; const last = h.pop(); if (h.length > 0) { h[0] = last; let i = 0; for (;;) { const l = 2 * i + 1, r = l + 1; let m = i; if (l < h.length && h[l] < h[m]) m = l; if (r < h.length && h[r] < h[m]) m = r; if (m === i) break; const t = h[i]; h[i] = h[m]; h[m] = t; i = m; } } return top; }
  get size() { return this.#h.length; }
}
function sortVia(n, seed) {
  const q = new Heap();
  let s = seed;
  for (let i = 0; i < n; i++) { s = (s * 1103515245 + 12345) % 2147483648; q.push(s % 1000); }
  let prev = -1, ok = true, sum = 0;
  while (q.size > 0) { const v = q.pop(); if (v < prev) ok = false; prev = v; sum += v; }
  return ok ? sum : -1;
}
let t = 0; for (let r = 0; r < 30; r++) t += sortVia(500, r);
out.push(t);
// A subclass override and the base at one site, in a long loop.
class A { f(x) { return x + 1; } }
class B extends A { f(x) { return super.f(x) * 2; } }
const objs = [new A(), new B(), new A(), new B()];
t = 0; for (let i = 0; i < 40000; i++) t += objs[i & 3].f(i & 15);
out.push(t);
// The method replaced on the prototype mid-loop.
class C { g(x) { return x; } }
const c = new C(); t = 0;
for (let i = 0; i < 30000; i++) { if (i === 15000) C.prototype.g = function (x) { return -x; }; t += c.g(i & 7); }
out.push(t);
// A throwing method caught in the loop, and recursion through methods.
class D { h(x) { if (x % 97 === 0) throw new Error("e" + x); return x & 3; } depth(n) { return n === 0 ? 0 : 1 + this.depth(n - 1); } }
const d = new D(); let caught = 0; t = 0;
for (let i = 1; i < 30000; i++) { try { t += d.h(i); } catch (e) { caught++; } }
for (let i = 0; i < 200; i++) t += d.depth(300);
out.push(t, caught);
// Zero-argument class-method calls (CallMethod) in one long loop, so an OSR
// region: rotating receivers of a class and a subclass override, a method
// that calls another, and the method replaced on the prototype mid-loop.
class P { constructor(v) { this.v = v; } get1() { if (this.v > 5) return this.v - 5; return this.v + this.w0(); } w0() { return 1; } }
class P2 extends P { get1() { if (this.v === 3) return -1; return super.get1(); } }
function runAll(arr, n) {
  let s = 0;
  for (let i = 0; i < n; i++) {
    if (i === (n >> 1)) P.prototype.w0 = function () { return 100; };
    const o = arr[i % arr.length];
    s += o.get1();
  }
  return s;
}
out.push(runAll([new P(1), new P(9), new P2(3), new P2(4)], 300000));
// An own property shadowing the method on one instance.
const e1 = new C(), e2 = new C(); e2.g = (x) => 1000 + x; t = 0;
for (let i = 0; i < 20000; i++) t += (i & 1 ? e1 : e2).g(i & 3);
out.push(t);
console.log(out.join(","));
"####;
