//! Class getters read from compiled code through the getter lane (looked up by
//! `jit_getter_callee` and called natively): fresh receivers, a private field,
//! inheritance and an override at one site, an own property shadowing the
//! getter, the getter redefined on the prototype and the prototype chain
//! changed while hot, a throwing getter, try/finally and `arguments` bodies,
//! static getters, recursion through a getter, a frozen instance, and many
//! fresh receivers through collections. Run interpreted, at the default
//! thresholds, with every function compiled at once, under GC stress, with the
//! lane off, with the lane's inline window entry declining on new register
//! ground as it used to, and with the helper-driven window entry; the
//! expected line is Node 24's.

const EXPECTED: &str = "97500,93000,20000,200,16,20050,1402000,16000,65150,199990000";

#[test]
fn class_getter_lanes_child() {
    if std::env::var_os("ZIPP_CLASS_GETTER_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, [EXPECTED]);
}

#[test]
fn class_getter_reads_match_node() {
    if std::env::var_os("ZIPP_CLASS_GETTER_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("lane-off", Some(("ZIPP_NO_CROSS3_GETTER", "1"))),
        ("no-grow", Some(("ZIPP_NO_CROSS3_GROW", "1"))),
        ("helper-entry", Some(("ZIPP_NO_CROSS3_INLINE", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "class_getter_lanes_child", "--nocapture"])
            .env("ZIPP_CLASS_GETTER_CHILD", "1")
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

const PROGRAM: &str = r####"// Class getters read from compiled code: fresh receivers, private fields,
// inheritance and overrides, an own property shadowing the getter, the
// getter replaced or the prototype chain changed while hot, throws, a
// try/finally body, static getters, recursion, and two classes at one site.
const out = [];
class Q {
  #h = [1, 2, 3];
  constructor(n) { this.n = n; }
  get size() { return this.#h.length + this.n; }
  get self() { return this; }
  get twice() { return this.size * 2; }
  push(v) { this.#h.push(v); }
}
function sum(q, k) { let s = 0; for (let i = 0; i < k; i++) s += q.size; return s; }
let t = 0;
for (let r = 0; r < 3000; r++) t += sum(new Q(r & 7), 5);
out.push(t);
const q = new Q(1);
t = 0; for (let r = 0; r < 3000; r++) { if (r % 1000 === 0) q.push(r); t += sum(q, 3) + (q.self === q ? 1 : 0) + q.twice; }
out.push(t);
// A subclass inheriting and one overriding the getter, at one site.
class R extends Q { }
class S extends Q { get size() { return -super.size; } }
const mixed = [new Q(1), new R(2), new S(3), new R(4)];
t = 0; for (let r = 0; r < 4000; r++) t += sum(mixed[r & 3], 2);
out.push(t);
// An own data property shadows the getter on one instance.
const sh = new Q(5);
for (let r = 0; r < 2000; r++) sum(sh, 2);
Object.defineProperty(sh, "size", { value: 100, writable: true });
out.push(sum(sh, 2), sum(new Q(5), 2));
// The getter replaced on the prototype while hot, then the chain changed.
const hot = new Q(2);
t = 0; for (let r = 0; r < 2000; r++) t += sum(hot, 2);
Object.defineProperty(Q.prototype, "size", { get() { return 7; }, configurable: true });
t += sum(hot, 2) + sum(new R(9), 2);
Object.setPrototypeOf(hot, { get size() { return 11; } });
t += sum(hot, 2);
out.push(t);
// A getter that throws on some receivers, caught by the reader.
class T { constructor(x) { this.x = x; } get v() { if (this.x === 3) throw new RangeError("v" + this.x); return this.x; } }
function readAll(arr) { let s = 0, c = 0; for (const o of arr) { try { s += o.v; } catch (e) { c++; } } return s * 100 + c; }
const ts = [new T(1), new T(2), new T(3), new T(4)];
t = 0; for (let r = 0; r < 2000; r++) t += readAll(ts);
out.push(t);
// A getter body with try/finally, and one using arguments.
class U { constructor(x) { this.x = x; } get f() { let y = 0; try { y = this.x; } finally { y++; } return y; } get a() { return arguments.length + this.x; } }
function rf(u, k) { let s = 0; for (let i = 0; i < k; i++) s += u.f + u.a; return s; }
t = 0; for (let r = 0; r < 2000; r++) t += rf(new U(r & 3), 2);
out.push(t);
// Static getters, recursion through a getter, a frozen instance.
class V { static #count = 0; static get count() { return ++V.#count; } constructor(d) { this.d = d; this.next = d > 0 ? new V(d - 1) : null; } get depth() { return this.next === null ? 0 : 1 + this.next.depth; } }
t = 0; for (let r = 0; r < 300; r++) t += V.count + new V(20).depth;
const fz = Object.freeze(new Q(3));
for (let r = 0; r < 2000; r++) t += sum(fz, 1);
out.push(t);
// Many fresh receivers through GC.
class W { constructor(i) { this.i = i; this.blob = { i }; } get b() { return this.blob.i; } }
t = 0; for (let r = 0; r < 20000; r++) { const w = new W(r); t += w.b; }
out.push(t);
console.log(out.join(","));
"####;
