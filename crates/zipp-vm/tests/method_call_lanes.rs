//! Method calls compiled code makes natively, and the reads and calls around
//! them: a class method looked up by its site's cache and called through the
//! emitted cross-call lane (`CallMethod`), a method read ahead of its
//! arguments and called through the same lane (`CallWithThis`), a leaf call
//! whose typed lane misses into the boxed splice, and an inlined leaf body's
//! property reads over several shapes. Every way the target can change under
//! a lane: polymorphic receivers past the lane's arms, a subclass override
//! and `super`, a prototype method patched, deleted and restored mid-loop, an
//! instance shadowing the method, a re-prototyped instance, a getter at the
//! same site, a throwing method, recursion through `this`, an arrow field and
//! arity mismatches. Run interpreted, at the default thresholds (which must
//! take both method lanes, per `ZIPP_JITLOG`), with every function compiled
//! at once, and with the lanes and memos switched off; the expected line is
//! Node 24's.

const EXPECTED: &str = "42000|145|AB:ACAB:AC0|15600|7 7 1001 1001 1001 undefined 7 7 7|12800|AC|303000|12368878/52|28468|B:A|18000|4000|30000|2.000000|36000";

#[test]
fn method_call_lanes_child() {
    if std::env::var_os("ZIPP_METHOD_CALL_LANES_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, [EXPECTED]);
}

#[test]
fn method_call_lanes_run_as_node_runs_them() {
    if std::env::var_os("ZIPP_METHOD_CALL_LANES_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    let off: &[(&str, &str)] = &[
        ("ZIPP_NO_CROSS3_CLASS", "1"),
        ("ZIPP_NO_CROSS3_CWT", "1"),
        ("ZIPP_NO_METHOD_MEMO", "1"),
        ("ZIPP_NO_LEAF_READ_MEMO", "1"),
        ("ZIPP_NO_LANE_BOXED_FALLBACK", "1"),
    ];
    for (mode, env) in [
        ("interpreter", &[("ZIPP_NOJIT", "1")][..]),
        ("default-thresholds", &[][..]),
        ("compile-at-once", &[("ZIPP_JIT_THRESHOLD", "1")][..]),
        ("lanes-off", off),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "method_call_lanes_child", "--nocapture"])
            .env("ZIPP_METHOD_CALL_LANES_CHILD", "1")
            .env("ZIPP_JITLOG", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("re-run test binary");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "{mode}:\n--- stdout ---\n{}\n--- stderr (tail) ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            stderr.lines().rev().take(40).collect::<Vec<_>>().join("\n")
        );
        if cfg!(all(feature = "jit", target_arch = "x86_64")) && mode == "default-thresholds" {
            for lane in ["CROSS3-CLASS", "CROSS3-CWT"] {
                assert!(stderr.contains(lane), "the default tiers never planned a {lane} lane");
            }
        }
    }
}

const PROGRAM: &str = r####"// Method calls compiled code makes natively.
const out = [];
class A { constructor(v) { this.v = v; } get2(k) { return this.v * k; } who() { return "A"; } rec(n) { return n <= 0 ? this.v : this.rec(n - 1) + 1; } }
class B extends A { who() { return "B:" + super.who(); } }
class C { constructor(v) { this.v = v; } get2(k) { return -this.v * k; } who() { return "C"; } }
function mono(o, n) { let s = 0; for (let i = 0; i < n; i++) s += o.get2(i & 7); return s; }
out.push(mono(new A(3), 4000), mono(new A(5), 10));
// Polymorphic receivers at one site, then more classes than the lane has arms.
const objs = [new A(1), new B(2), new C(3)];
function poly(n) { let s = ""; let t = 0; for (let i = 0; i < n; i++) { const o = objs[i % 3]; t += o.get2(2); if (i < 6) s += o.who(); } return s + t; }
out.push(poly(3000));
const many = [];
for (let k = 0; k < 12; k++) many.push(new (class { constructor() { this.k = k; } get2(x) { return this.k + x; } })());
function wide(n) { let t = 0; for (let i = 0; i < n; i++) t += many[i % 12].get2(1); return t; }
out.push(wide(2400));
// The method patched on the prototype mid-loop, deleted, then restored.
function patched(o, n) { const r = []; for (let i = 0; i < n; i++) { if (i === 1000) A.prototype.get2 = function (k) { return 1000 + k; }; if (i === 2000) { delete A.prototype.get2; r.push(typeof o.get2); A.prototype.get2 = function (k) { return this.v * k; }; } if (i % 500 === 0 || i === 1001 || i === 2002) r.push(o.get2(1)); else o.get2(1); } return r.join(" "); }
out.push(patched(new A(7), 2600));
// An instance that shadows the method, and one re-prototyped mid-loop.
const sh = new A(9);
function shadow(n) { let s = 0; for (let i = 0; i < n; i++) { if (i === 800) sh.get2 = (k) => -k; s += sh.get2(2); } return s; }
out.push(shadow(1600));
const rp = new A(4);
function reproto(n) { let s = ""; for (let i = 0; i < n; i++) { if (i === 900) Object.setPrototypeOf(rp, C.prototype); const w = rp.who(); if (i === 899 || i === 900) s += w; } return s; }
out.push(reproto(1800));
// A getter of the same name at the same site; a method that throws.
class G { get get2() { return (k) => k * 100; } }
function mixed(n) { let s = 0; const g = new G(), a = new A(1); for (let i = 0; i < n; i++) s += (i & 1 ? g : a).get2(3); return s; }
out.push(mixed(2000));
class T { m(i) { if (i % 97 === 0) throw new RangeError("r" + i); return i; } }
function thrower(n) { const t = new T(); let s = 0, c = 0; for (let i = 0; i < n; i++) { try { s += t.m(i); } catch (e) { c++; } } return s + "/" + c; }
out.push(thrower(5000));
// Recursion through this.m(), super, and the spec-order (CallWithThis) form.
function recur(n) { const a = new A(2); let s = 0; for (let i = 0; i < n; i++) s += a.rec(i & 15); return s; }
out.push(recur(3000));
function sup(n) { const b = new B(1); let s = ""; for (let i = 0; i < n; i++) s = b.who(); return s; }
out.push(sup(2000));
let side = 0;
function arg() { side++; return side & 3; }
class P { constructor() { this.inner = new A(3); } run(n) { let s = 0; for (let i = 0; i < n; i++) s += this.inner.get2(arg()); return s; } }
out.push(new P().run(4000), side);
// An arrow class field called as a method, and arity mismatches.
class F { constructor() { this.base = 5; } f = (k) => this.base + k; g(a, b) { return (a ?? 0) + (b ?? 0); } }
function fields(n) { const o = new F(); let s = 0; for (let i = 0; i < n; i++) s += o.f(i & 3) + o.g(i & 1) + o.g(1, 2, 3); return s; }
out.push(fields(3000));
// A leaf call whose typed lane was planned for ints and then sees doubles.
function plain(a, b) { return a * b + 1; }
function leaf(n) { let s = 0; for (let i = 0; i < n; i++) s = plain(s, i < n / 2 ? 2 : 0.5) % 1e6; return s; }
out.push(leaf(4000).toFixed(6));
// An inlined leaf body's property reads over receivers of several shapes.
function rd(o) { return o.x + o.y; }
function shapes(n) { const os = [{ x: 1, y: 2 }, { y: 3, x: 4 }, { x: 5, y: 6, z: 0 }, { x: 7, get y() { return 8; } }]; let s = 0; for (let i = 0; i < n; i++) s += rd(os[i & 3]); return s; }
out.push(shapes(4000));
console.log(out.join("|"));
"####;
