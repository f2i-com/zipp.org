//! Methods whose body makes an instance (`return new V(...)`) inlined at their call
//! sites with the instance made in place (`Vm::plan_alloc_leaf`, `emit_new_lane_into`):
//! results and identity, a bail after the instance was made, a class evaluated again,
//! a constructor that is not store-only, polymorphic receivers, instances that escape
//! and survive collections. Run interpreted, at the default thresholds and with every
//! function compiled at once; the expected lines are Node 24's.

const SOURCE: &str = r##"// Methods whose body makes an instance (`return new V(...)`) inlined at
// their call sites with the instance made in place: results and identity,
// a bail after the instance was made (a non-number operand), a class
// evaluated again (fresh class values per call of a factory), a constructor
// whose body is not store-only, polymorphic receivers, an instance that
// escapes into an array, and the same calls with the interpreter's.
"use strict";
const out = [];
class V {
  x; y; z;
  constructor(x, y, z) { this.x = x; this.y = y; this.z = z; }
  add(o) { return new V(this.x + o.x, this.y + o.y, this.z + o.z); }
  sub(o) { return new V(this.x - o.x, this.y - o.y, this.z - o.z); }
  scale(k) { return new V(this.x * k, this.y * k, this.z * k); }
  dot(o) { return this.x * o.x + this.y * o.y + this.z * o.z; }
  pairSum(o) { const a = new V(this.x, this.y, 0); return a.x + o.x * 2 + a.y; }
}
function run(n) {
  let acc = new V(0, 0, 0), s = 0;
  const step = new V(0.5, -0.25, 2);
  for (let i = 0; i < n; i++) {
    const p = new V(i, i * 0.5, -i);
    const q = p.sub(step).scale(1.5).add(acc.scale(0.001));
    s += q.dot(step) + p.pairSum(step);
    acc = q;
  }
  return [s, acc.x, acc.y, acc.z, acc instanceof V, Object.keys(acc).join("")];
}
for (let r = 0; r < 30; r++) run(200);
out.push(run(2000).join(","));
// A bail after the instance: `o.x` not a number partway through.
function mixed(n) {
  const base = new V(1, 2, 3);
  let t = "";
  for (let i = 0; i < n; i++) {
    const o = (i % 97 === 96) ? { x: "s", y: 1, z: 2 } : new V(i, 1, 2);
    const r = base.add(o);
    if (i % 97 === 96 || i === n - 1) t += r.x + "|";
  }
  return t;
}
for (let r = 0; r < 20; r++) mixed(300);
out.push(mixed(1000));
// Class evaluated again: each factory call makes a new class value.
function factory(tag) {
  return class W {
    a; b;
    constructor(a, b) { this.a = a; this.b = b; }
    twin() { return new W(this.b, this.a + tag); }
  };
}
let tw = "";
for (let k = 0; k < 5; k++) {
  const W = factory(k);
  let w = new W(1, 2);
  for (let i = 0; i < 400; i++) w = w.twin();
  tw += w.a + ":" + w.b + ":" + (w instanceof W) + ";";
}
out.push(tw);
// A constructor that is not store-only, and a replaced method.
class C {
  v;
  constructor(v) { this.v = v * 2 + 1; }
  next() { return new C(this.v); }
}
let c = new C(1), cs = 0;
for (let i = 0; i < 300; i++) { c = c.next(); cs = (cs + c.v) | 0; if (c.v > 1e6) c = new C(1); }
out.push(cs);
// Polymorphic receivers of one method shape.
class A { constructor(x) { this.x = x; } mk() { return new A(this.x + 1); } }
class B { constructor(x) { this.x = x; this.k = 1; } mk() { return new B(this.x + 2); } }
let pa = new A(0), pb = new B(0);
for (let i = 0; i < 2000; i++) { const t = (i & 1) ? pa : pb; const u = t.mk(); if (i & 1) pa = u; else pb = u; }
out.push(pa.x + "," + pb.x + "," + (pa instanceof A) + (pb instanceof B) + pb.k);
// Instances that escape into an array and survive collections.
const keep = [];
let src = new V(1, 1, 1);
for (let i = 0; i < 50000; i++) { src = src.scale(1.0000001); if ((i & 1023) === 0) keep.push(src.add(new V(i, 0, 0))); }
out.push(keep.length + ":" + keep[keep.length - 1].x.toFixed(6) + ":" + keep[3].y.toFixed(6));
console.log(out.join("\n"));
"##;

const EXPECTED: &str = "-1889885.5015888654,3000.7492477462447,1501.125374623873,-3004.5030014999984,true,xyz\n1s|1s|1s|1s|1s|1s|1s|1s|1s|1s|1000|\n1:2:true;201:202:true;401:402:true;601:602:true;801:802:true;\n33586764\n1000,2000,truetrue1\n49:49153.004927:1.000307";

#[test]
fn alloc_leaf_inline_worker() {
    if std::env::var_os("ZIPP_ALLOC_LEAF_INLINE_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn alloc_leaf_inline_match_node() {
    if std::env::var_os("ZIPP_ALLOC_LEAF_INLINE_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, var) in [
        ("default", None),
        ("interpreter", Some("ZIPP_NOJIT")),
        ("compile-at-once", Some("ZIPP_JIT_THRESHOLD")),
    ] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["alloc_leaf_inline_worker", "--exact", "--nocapture"])
            .env("ZIPP_ALLOC_LEAF_INLINE_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD");
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
