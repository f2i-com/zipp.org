//! A leaf class method called as `a.dot(b)` is inlined behind its
//! class-method lane's guards (`LeafInlinePlan::class_guard`), with `this`
//! bound to the receiver; a strict method read into a register first
//! (`a.dot(f())`) binds it the same way (`LeafInlinePlan::this_reg`). Other
//! receivers, a replaced or shadowed method, coercing and accessor fields,
//! deleted fields, Proxy arguments, extracted methods, defaults, `arguments`
//! and prototype changes mid-loop give Node 24's results in every execution
//! mode and with the inlining off (`ZIPP_NO_CLASS_LEAF=1`,
//! `ZIPP_NO_LEAF_THIS=1`, `ZIPP_NO_LEAF_PROP_SHAPE=1`); at the default
//! thresholds the planner must name a class-method plan (`ZIPP_JITLOG`).

use std::process::Command;

const CHILD_ENV: &str = "ZIPP_CLASS_METHOD_LEAF_INLINE_CHILD";

#[test]
fn class_method_leaf_inline_child() {
    let out = zipp_vm::run(PROGRAM).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}; output: {:?}", out.error, out.output);
    let lines: Vec<String> = out.output.iter().flat_map(|l| l.lines()).map(str::to_string).collect();
    assert_eq!(
        lines,
        [
            "mono 128000",
            "poly 47250",
            "proto-replace 1018000",
            "own-shadow 25000",
            "coerce 132000 1000",
            "getter 123000 1000",
            "arg-getter 110000 1000",
            "deleted 96000/1000",
            "arg-shapes 6000 750",
            "self 4000",
            "extracted 4000",
            "defaults 26000",
            "arguments 4000",
            "inherited 23000",
            "spec-order 10000 4000",
            "spec-replace 26997",
            "strict-literal 4000",
            "frozen 24000",
            "reproto 17000",
        ]
    );
}

#[test]
fn class_method_leaf_inline_in_all_modes() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    const SWITCHES: [&str; 3] = ["ZIPP_NO_CLASS_LEAF", "ZIPP_NO_LEAF_THIS", "ZIPP_NO_LEAF_PROP_SHAPE"];
    for (mode, env) in [
        ("default-thresholds", &[("ZIPP_JITLOG", "1")][..]),
        ("interpreter", &[("ZIPP_NOJIT", "1")][..]),
        ("forced-jit", &[("ZIPP_JIT_THRESHOLD", "1")][..]),
        ("gc-stress", &[("ZIPP_GC_STRESS", "1")][..]),
        ("interpreter-gc-stress", &[("ZIPP_NOJIT", "1"), ("ZIPP_GC_STRESS", "1")][..]),
        ("no-class-leaf", &[("ZIPP_NO_CLASS_LEAF", "1")][..]),
        ("no-leaf-this", &[("ZIPP_NO_LEAF_THIS", "1")][..]),
        ("no-leaf-prop-shape", &[("ZIPP_NO_LEAF_PROP_SHAPE", "1")][..]),
    ] {
        let mut cmd = Command::new(&exe);
        cmd.args(["--exact", "class_method_leaf_inline_child", "--nocapture"])
            .env(CHILD_ENV, "1")
            .env("ZIPP_CACHE_DIR", "")
            .env_remove("ZIPP_JITLOG")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS");
        for s in SWITCHES {
            cmd.env_remove(s);
        }
        cmd.envs(env.iter().copied());
        let out = cmd.output().expect("spawn execution-mode child");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "{mode} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            stderr.lines().rev().take(40).collect::<Vec<_>>().join("\n")
        );
        if cfg!(all(feature = "jit", target_arch = "x86_64")) && mode == "default-thresholds" {
            assert!(
                stderr.lines().any(|l| l.starts_with("[leaf]") && l.contains("CLASS-LEAF")),
                "the default tiers never planned a class-method leaf"
            );
        }
    }
}

const PROGRAM: &str = r####"
// Class methods inlined at their call sites: what the inline must preserve.
const out = [];
const N = 4000, K = 3000;
class V {
  constructor(x, y, z) { this.x = x; this.y = y; this.z = z; }
  dot(o) { return this.x * o.x + this.y * o.y + this.z * o.z; }
  self() { return this; }
  sum() { return this.x + this.y + this.z; }
  def(o = { x: 10 }) { return this.x + o.x; }
  cnt() { return arguments.length; }
}
class W { constructor(x) { this.x = x; this.y = 0; this.z = 0; } dot(o) { return -1; } }
class S extends V {}

// 1. Hot and monomorphic.
function mono(a, b) { let s = 0; for (let i = 0; i < N; i++) s += a.dot(b); return s; }
out.push("mono " + mono(new V(1, 2, 3), new V(4, 5, 6)));

// 2. Other receivers after the loop is hot: another class, a plain object, a subclass.
function poly(rs, b) { let s = 0; for (let i = 0; i < N; i++) s += rs[i < K ? 0 : i & 3].dot(b); return s; }
out.push("poly " + poly([new V(1, 1, 1), new W(5), { dot(o) { return 100; } }, new S(2, 2, 2)], new V(1, 2, 3)));

// 3. The method replaced on the prototype mid-loop.
const saveSum = V.prototype.sum;
function protoReplace(a) { let s = 0; for (let i = 0; i < N; i++) { if (i === K) V.prototype.sum = function () { return 1000; }; s += a.sum(); } return s; }
out.push("proto-replace " + protoReplace(new V(1, 2, 3)));
V.prototype.sum = saveSum;

// 4. An own property shadowing the method, added mid-loop.
function ownShadow(a) { let s = 0; for (let i = 0; i < N; i++) { if (i === K) a.sum = () => 7; s += a.sum(); } return s; }
out.push("own-shadow " + ownShadow(new V(1, 2, 3)));

// 5. A field becomes an object with valueOf: each call coerces exactly once.
let valueOfs = 0;
function coerce(a, b) { let s = 0; for (let i = 0; i < N; i++) { if (i === K) a.x = { valueOf() { valueOfs++; return 2; } }; s += a.dot(b); } return s; }
out.push("coerce " + coerce(new V(1, 2, 3), new V(4, 5, 6)) + " " + valueOfs);

// 6. A field becomes a getter on the receiver: each call reads it once.
let gets = 0;
function getter(a, b) { let s = 0; for (let i = 0; i < N; i++) { if (i === K) Object.defineProperty(a, "y", { get() { gets++; return 1; } }); s += a.dot(b); } return s; }
out.push("getter " + getter(new V(1, 2, 3), new V(4, 5, 6)) + " " + gets);

// 7. A getter on the ARGUMENT's field, and a deleted field.
let argGets = 0;
function argGetter(a, b) { let s = 0; for (let i = 0; i < N; i++) { if (i === K) Object.defineProperty(b, "z", { get() { argGets++; return 0; } }); s += a.dot(b); } return s; }
out.push("arg-getter " + argGetter(new V(1, 2, 3), new V(4, 5, 6)) + " " + argGets);
function deleted(a, b) { let s = 0, nan = 0; for (let i = 0; i < N; i++) { if (i === K) delete a.y; const r = a.dot(b); if (r !== r) nan++; else s += r; } return s + "/" + nan; }
out.push("deleted " + deleted(new V(1, 2, 3), new V(4, 5, 6)));

// 8. Arguments of several shapes, a Proxy among them.
let traps = 0;
const px = new Proxy({ x: 1, y: 1, z: 1 }, { get(t, k) { traps++; return t[k]; } });
function argShapes(a) {
  const bs = [new V(1, 0, 0), { x: 0, y: 1, z: 0 }, { z: 1, y: 0, x: 0 }, px];
  let s = 0; for (let i = 0; i < N; i++) s += a.dot(bs[i < K ? 0 : i & 3]); return s;
}
out.push("arg-shapes " + argShapes(new V(1, 2, 3)) + " " + traps);

// 9. `this` is the receiver itself.
function selfId(a) { let same = 0; for (let i = 0; i < N; i++) if (a.self() === a) same++; return same; }
out.push("self " + selfId(new V(0, 0, 0)));

// 10. The method called with no receiver throws, every time.
function extracted() { const f = V.prototype.sum; let threw = 0; for (let i = 0; i < N; i++) { try { f(); } catch (e) { if (e instanceof TypeError) threw++; } } return threw; }
out.push("extracted " + extracted());

// 11. A default parameter, and `arguments`.
function defaults(a) { let s = 0; for (let i = 0; i < N; i++) s += i & 1 ? a.def() : a.def({ x: 1 }); return s; }
out.push("defaults " + defaults(new V(1, 0, 0)));
function args(a) { let s = 0; for (let i = 0; i < N; i++) s += i & 1 ? a.cnt() : a.cnt(1, 2); return s; }
out.push("arguments " + args(new V(0, 0, 0)));

// 12. An inherited method overridden on the subclass mid-loop.
function inherited(s1, b) { let s = 0; for (let i = 0; i < N; i++) { if (i === K) S.prototype.dot = function () { return 5; }; s += s1.dot(b); } return s; }
out.push("inherited " + inherited(new S(1, 1, 1), new V(1, 2, 3)));
delete S.prototype.dot;

// 13. The spec-order call form (the callee read before the arguments run).
let argRuns = 0;
const mk = (i) => (argRuns++, i & 1 ? new V(1, 0, 0) : new V(0, 1, 0));
function specOrder(a) { let s = 0; for (let i = 0; i < N; i++) s += a.dot(mk(i)); return s; }
out.push("spec-order " + specOrder(new V(2, 3, 4)) + " " + argRuns);
function specReplace(a) { let s = 0; for (let i = 0; i < N; i++) s += a.dot((i === K && (a.dot = () => 9), new V(1, 1, 1))); return s; }
out.push("spec-replace " + specReplace(new V(1, 2, 3)));

// 14. A strict object-literal method reached through a callee register.
function strictScope() {
  "use strict";
  const o = { k: 2, m(v) { return this.k * v; } };
  let s = 0; for (let i = 0; i < N; i++) s += o.m(mk(i).x); return s;
}
out.push("strict-literal " + strictScope());

// 15. A frozen receiver, and a receiver whose prototype changes mid-loop.
function frozen(a, b) { let s = 0; for (let i = 0; i < N; i++) { if (i === K) Object.freeze(a); s += a.dot(b); } return s; }
out.push("frozen " + frozen(new V(1, 2, 3), new V(1, 1, 1)));
function reproto(a, b) { let s = 0; for (let i = 0; i < N; i++) { if (i === K) Object.setPrototypeOf(a, W.prototype); s += a.dot(b); } return s; }
out.push("reproto " + reproto(new V(1, 2, 3), new V(1, 1, 1)));

console.log(out.join("\n"));
"####;
