//! `obj.f(args)` where `f` is an own data property holding a function, called
//! from compiled code (the memory region's CROSS3M lane, now in Tier C too):
//! arrows and plain functions (`this` is the receiver), the property replaced
//! by another closure of the same and of other code, deleted so the
//! prototype's method answers, turned into an accessor and a native, made
//! non-callable, a throwing callee, a try/finally callee, recursion through
//! the field, receivers of three shapes at one site, and argument-count
//! mismatches. Run interpreted, at the default thresholds, with every
//! function compiled at once, under GC stress and with the Tier C lane off;
//! the expected line is Node 24's.

const EXPECTED: &str = "105000,18000,12000,21000,2205,TypeError,400,18000,20000,14500,6000";

#[test]
fn own_method_lanes_child() {
    if std::env::var_os("ZIPP_OWN_METHOD_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, [EXPECTED]);
}

#[test]
fn own_property_method_calls_match_node() {
    if std::env::var_os("ZIPP_OWN_METHOD_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("tierc-lane-off", Some(("ZIPP_NO_TIERC_CROSS3M", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "own_method_lanes_child", "--nocapture"])
            .env("ZIPP_OWN_METHOD_CHILD", "1")
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

const PROGRAM: &str = r####"// `obj.f(args)` where `f` is an own data property holding a function, called
// from a compiled method: arrows and plain functions (`this` is the
// receiver), the property replaced, deleted, turned into an accessor, a
// native, a non-callable, a throwing callee, and receivers of other shapes.
"use strict";
const out = [];
class Q {
  constructor(cmp) { this.cmp = cmp; this.n = 0; }
  run(n) { let s = 0; for (let k = 0; k < n; k++) s += this.cmp(k, 1); return s; }
  run2(a, b) { return this.cmp(a, b); }
}
const arrow = new Q((a, b) => a - b);
let t = 0; for (let r = 0; r < 3000; r++) t += arrow.run(10);
out.push(t);
// A plain function in the field sees the receiver as `this`.
const plain = new Q(function (a, b) { this.n++; return a * b; });
t = 0; for (let r = 0; r < 3000; r++) t += plain.run(4);
out.push(t, plain.n);
// The field replaced by another closure of the same code, then other code.
const mk = (d) => (a, b) => a - b + d;
const rep = new Q(mk(0));
t = 0;
for (let r = 0; r < 3000; r++) { if (r === 1000) rep.cmp = mk(5); if (r === 2000) rep.cmp = (a, b) => a + b; t += rep.run(3); }
out.push(t);
// Deleted: the prototype's method answers; then an accessor; then a native.
class P extends Q {}
P.prototype.cmp = function (a, b) { return 100 + a + b; };
const del = new P((a, b) => a);
t = 0; for (let r = 0; r < 2000; r++) t += del.run(2);
delete del.cmp; t += del.run(2);
Object.defineProperty(del, "cmp", { get() { return (a, b) => -1; }, configurable: true });
t += del.run(2);
Object.defineProperty(del, "cmp", { value: Math.max, writable: true, configurable: true });
t += del.run(3);
out.push(t);
// Not callable, and a throwing callee caught by the caller.
const bad = new Q((a) => a);
for (let r = 0; r < 2000; r++) bad.run(2);
bad.cmp = 7;
try { bad.run(1); out.push("no throw"); } catch (e) { out.push(e.constructor.name); }
const thrower = new Q((a, b) => { if (a === 3) throw new RangeError("x" + a); return a; });
let caught = 0;
for (let r = 0; r < 2000; r++) { try { thrower.run(r % 5); } catch (e) { caught++; } }
out.push(caught);
// A callee with its own try/finally, and deep recursion through the field.
const fin = new Q((a, b) => { let x = 0; try { x = a + b; } finally { x++; } return x; });
t = 0; for (let r = 0; r < 2000; r++) t += fin.run(3);
out.push(t);
const rec = new Q(null);
rec.cmp = (a, b) => (a <= 0 ? b : rec.run2(a - 1, b + 1));
t = 0; for (let r = 0; r < 500; r++) t += rec.run2(40, 0);
out.push(t);
// Receivers of different shapes through one site.
const shapes = [new Q((a, b) => a + b), Object.assign(new Q((a, b) => a * 2), { extra: 1 }), new Q((a, b) => b)];
t = 0; for (let r = 0; r < 3000; r++) t += shapes[r % 3].run2(r & 7, 2);
out.push(t);
// Argument count mismatch and an arguments-using callee.
const argsy = new Q(function () { return arguments.length; });
t = 0; for (let r = 0; r < 2000; r++) t += argsy.run2(1, 2);
const fewer = new Q((a) => a);
for (let r = 0; r < 2000; r++) t += fewer.run2(1, 2);
out.push(t);
console.log(out.join(","));
"####;
