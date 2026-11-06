//! A compiled accessor lane serves a trivial accessor in place
//! (`AccLane::field`: `return this.f`, `this.f = v`, `this.f = v | 0`
//! on a prototype; `ZIPP_NO_ACC_INLINE` keeps the call): hot reads and
//! writes, a fraction to the `| 0` setter and a string and an object to
//! the plain one (the call), the getter replaced in place through
//! `__defineGetter__` (an accessor merge, which now bumps the holder) and
//! through `defineProperty`, a frozen receiver, a non-writable own field,
//! a receiver of another shape, a class getter and setter, and the accessor
//! turned into a data property. The expected lines are Node's (v24), with
//! the lane, without it and in the interpreter.

const SOURCE: &str = r##"
"use strict";
const out = [];
function P(x, y) { this._x = x; this._y = y; }
Object.defineProperty(P.prototype, "x", { get() { return this._x; }, set(v) { this._x = v | 0; }, configurable: true });
Object.defineProperty(P.prototype, "y", { get() { return this._y; }, set(v) { this._y = v; }, configurable: true });
const ps = [];
for (let i = 0; i < 32; i++) ps.push(new P(i, -i));
function hot(n) {
  let s = 0;
  for (let i = 0; i < n; i++) {
    const p = ps[i & 31];
    p.x = p.y + 3;            // trivial `| 0` setter, Int value
    p.y = p.x * 2;            // trivial plain setter, Int value
    s = (s + p.x + p.y) | 0;  // trivial getters
  }
  return s;
}
out.push("hot " + hot(20000) + " " + ps[5].x + " " + ps[5].y);
// A fraction to the `| 0` setter, a string and an object to the plain one.
ps[1].x = 7.9; ps[2].y = "str"; ps[3].y = { k: 1 };
out.push("vals " + ps[1].x + " " + ps[2].y + " " + typeof ps[3].y + " " + ps[3].y.k + " " + hot(2000));
// Replace the getter in place through __defineGetter__ (an accessor merge).
P.prototype.__defineGetter__("x", function () { return this._x + 1000; });
out.push("merged " + hot(2000) + " " + ps[4].x);
// Replace through defineProperty.
Object.defineProperty(P.prototype, "y", { get() { return -this._y; }, set(v) { this._y = v + 1; }, configurable: true });
out.push("redefined " + hot(2000) + " " + ps[6].y);
// A frozen receiver, a non-writable own field, a different shape.
const fz = new P(1, 2); Object.freeze(fz);
let threw = false;
try { fz.x = 5; } catch (e) { threw = e instanceof TypeError; }
out.push("frozen " + fz._x + " " + threw);
const ro = new P(3, 4); Object.defineProperty(ro, "_x", { writable: false });
threw = false;
try { ro.x = 9; } catch (e) { threw = e instanceof TypeError; }
out.push("readonly " + ro._x + " " + threw);
const odd = Object.create(P.prototype); odd._y = 10; odd._x = 20;
let t = 0;
for (let i = 0; i < 3000; i++) { odd.x = i; t = (t + odd.x + odd.y) | 0; }
out.push("odd " + t + " " + odd._x + " " + odd._y);
// A class getter and setter over a field, then the accessor turned into data.
class C { constructor() { this.v = 1; } get val() { return this.v; } set val(n) { this.v = n | 0; } }
const cs = [];
for (let i = 0; i < 16; i++) cs.push(new C());
let cv = 0;
for (let i = 0; i < 20000; i++) { const c = cs[i & 15]; c.val = c.val + 1; cv = (cv + c.val) | 0; }
out.push("class " + cv + " " + cs[3].v);
Object.defineProperty(C.prototype, "val", { value: 42, writable: true });
let dv = 0;
for (let i = 0; i < 100; i++) dv += cs[i & 15].val;
out.push("data " + dv + " " + cs[3].v);
console.log(out.join("\n"));
"##;

const EXPECTED: &str = "hot -179088 -3 -6\nvals 7 str object 1 -18018\nmerged -6210000 -1003\nredefined 1430957232 -669\nfrozen 1 true\nreadonly 3 true\nodd 7468500 2999 10\nclass 12530000 1251\ndata 4200 1251";

#[test]
fn accessor_inline_lane_worker() {
    if std::env::var_os("ZIPP_ACC_INLINE_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn accessor_inline_lane_matches_node() {
    if std::env::var_os("ZIPP_ACC_INLINE_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, var) in [
        ("default", None),
        ("inline-off", Some("ZIPP_NO_ACC_INLINE")),
        ("interpreter", Some("ZIPP_NOJIT")),
    ] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["accessor_inline_lane_worker", "--exact", "--nocapture"])
            .env("ZIPP_ACC_INLINE_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_NO_ACC_INLINE");
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