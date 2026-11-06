//! Tier C and region property sites take an accessor through its shape-keyed
//! lane (`AccLane`) when the site's cache keys it by the receiver's shape:
//! a prototype accessor read and written over 64 receivers of one shape, a
//! redefinition and a merge of it (the merge replaces the getter without a
//! version bump, which the helper's re-read catches), a receiver that gains
//! a shadowing own property, one whose prototype is swapped, own accessors
//! with a closure per object, a getter that throws in a hot loop, an accessor
//! on `%Object.prototype%` read through plain literals (the default link),
//! and a setter that redefines its own accessor. The expected lines are
//! Node's (v24), with the lane, without it (`ZIPP_NO_ACC_LANE`) and in the
//! interpreter.

const SOURCE: &str = r##"
"use strict";
const out = [];
function V(x) { this._x = x; }
Object.defineProperty(V.prototype, "x", { get() { return this._x; }, set(v) { this._x = v | 0; }, configurable: true });
const vs = [];
for (let i = 0; i < 64; i++) vs.push(new V(i));
function run(n) {
  let h = 0;
  for (let i = 0; i < n; i++) { const v = vs[i & 63]; v.x = v.x + 1; h = (h + v.x) | 0; }
  return h;
}
out.push("base " + run(200000));
// Redefine the getter on the prototype (version bump).
Object.defineProperty(V.prototype, "x", { get() { return this._x * 2; }, set(v) { this._x = v | 0; }, configurable: true });
out.push("redef " + run(50000));
// Accessor merge: replace only the getter (the setter stays).
Object.defineProperty(V.prototype, "x", { get() { return this._x + 7; } });
out.push("merge " + run(50000));
// One receiver gets an own data property shadowing the accessor (its shape changes).
Object.defineProperty(vs[5], "x", { value: 1000, writable: true, configurable: true });
out.push("shadow " + run(50000));
// One receiver's prototype is swapped.
const P2 = { get x() { return -1; }, set x(v) { } };
Object.setPrototypeOf(vs[9], P2);
out.push("proto " + run(50000));
// Own accessors per object, each with its own closure.
function temp(c) {
  const t = { id: c }; let cel = c;
  Object.defineProperty(t, "c", { get() { return cel; }, set(v) { cel = v; }, enumerable: true, configurable: true });
  return t;
}
const ts = []; for (let i = 0; i < 24; i++) ts.push(temp(i));
let g = 0;
for (let i = 0; i < 100000; i++) { const t = ts[i % 24]; t.c = t.c + 1; g = (g + t.c) | 0; }
out.push("own " + g);
// A getter that throws partway through a hot loop.
const T = { get boom() { if (this.n++ > 10000) throw new Error("boom at " + this.n); return this.n; }, n: 0 };
const ws = []; for (let i = 0; i < 8; i++) ws.push(Object.create(T));
let caught = "";
try { let s = 0; for (let i = 0; i < 100000; i++) s += ws[i & 7].boom; } catch (e) { caught = e.message; }
out.push("throw " + caught);
// An accessor on Object.prototype read through plain literals (default link).
Object.defineProperty(Object.prototype, "twice", { get() { return (this.v | 0) * 2; }, set(v) { this.v = v >> 1; }, configurable: true });
const os = []; for (let i = 0; i < 16; i++) os.push({ v: i, w: 0 });
let q = 0;
for (let i = 0; i < 100000; i++) { const o = os[i & 15]; o.twice = o.twice + 2; q = (q + o.twice) | 0; }
delete Object.prototype.twice;
out.push("default " + q + " " + os[3].twice);
// A setter that redefines its own accessor mid-loop.
function S() { this.k = 0; }
let swaps = 0;
Object.defineProperty(S.prototype, "y", { get() { return this.k; }, set(v) { this.k = v; if (v === 5000) { swaps++; Object.defineProperty(S.prototype, "y", { get() { return this.k + 100; }, set(v) { this.k = v; }, configurable: true }); } }, configurable: true });
const ss = []; for (let i = 0; i < 4; i++) ss.push(new S());
let r = 0;
for (let i = 0; i < 40000; i++) { const s = ss[i & 3]; s.y = s.y + 1; r = (r + s.y) | 0; }
out.push("selfredef " + r + " " + swaps);
console.log(out.join("\n"));
"##;

const EXPECTED: &str = "base 318900000\nredef -908320\nmerge 156750048\nshadow 462992189\nproto 758972535\nown 209533272\nthrow boom at 10002\ndefault 626600000 undefined\nselfredef 909553404 1";

#[test]
fn accessor_shape_lane_worker() {
    if std::env::var_os("ZIPP_ACC_LANE_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn accessor_shape_lane_matches_node() {
    if std::env::var_os("ZIPP_ACC_LANE_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, var) in [("default", None), ("lane-off", Some("ZIPP_NO_ACC_LANE")), ("interpreter", Some("ZIPP_NOJIT"))] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["accessor_shape_lane_worker", "--exact", "--nocapture"])
            .env("ZIPP_ACC_LANE_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_NO_ACC_LANE");
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