//! 15 September 2026 audit, track E-gc-natives.
//!
//! Native built-ins that read guest getters or Proxy traps one property at a
//! time used to collect the results in Rust `Vec`s/`ObjMap`s that no GC root
//! reached: object spread, `Object.assign`, object rest, `Object.values` /
//! `entries`, CreateListFromArrayLike (`apply`, `Reflect.apply` /
//! `construct`, a Proxy `ownKeys` list), `Object.create(proto, props)`,
//! ToPropertyDescriptor, the Proxy `defineProperty` / `ownKeys` invariant
//! checks, InstallErrorCause, `super()` over base-class field initializers,
//! the construct paths that read `newTarget.prototype` after allocating, and
//! `flat` / `flatMap`. An allocating getter reached a safe point, a minor or
//! major collection freed the partial results, and the built-in returned
//! reused slots — unrelated objects, `null`s, or a TypeError from a swept
//! receiver. Every probe below failed on the base build in every mode; each
//! one runs in a clean child process under natural GC (default, interpreter,
//! forced JIT, majors only) and under `ZIPP_GC_STRESS`.
//!
//! The same file pins the rest of the track against node's output: frozen /
//! sealed array redefinition, arguments own keys, `flat` holes, StrWhiteSpace,
//! AggregateError order, the apply argument ceiling, the non-object
//! `newTarget.prototype` fallback, integer-first own-key order for functions,
//! RegExps and String wrappers, the per-key re-check in array
//! `Object.values`/`entries`, destructured parameters of a function that can
//! direct-eval (their slots are boxed and the pattern used to read the box),
//! and the iterator protocol behind for-of, spread and destructuring (last
//! section).

use zipp_vm::embed::{compile_script, JsValue, ScriptState};

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

/// One line per built-in, `name=<failed rounds>` (plus the last error). The
/// `ROUNDS` / `CHURN` prelude sizes the run: natural GC needs real allocation
/// pressure, GC stress collects at every safe point anyway.
const GC_PROBES: &str = r#"
let junk;
function churn() { for (let j = 0; j < CHURN; j++) junk = [{ j }, "c" + j]; }
const out = [];
function check(name, fn) {
  let bad = 0, err = "";
  for (let r = 0; r < ROUNDS; r++) {
    try { if (!fn()) bad++; } catch (e) { bad++; err = String(e && e.message || e); }
  }
  out.push(name + "=" + bad + (err ? "(" + err + ")" : ""));
}
function getterSrc(n) {
  const src = {};
  for (let i = 0; i < n; i++) Object.defineProperty(src, "p" + i, { enumerable: true, configurable: true, get() { churn(); return { i }; } });
  return src;
}
function okSeq(vals, n) { if (vals.length !== n) return false; for (let i = 0; i < n; i++) if (!vals[i] || vals[i].i !== i) return false; return true; }
const trapProxy = () => new Proxy({ a: 0, b: 1, c: 2, d: 3 }, { get(t, k) { churn(); return typeof k === "string" ? { i: t[k] } : t[k]; } });

check("assign", () => { const o = Object.assign({}, getterSrc(6)); return okSeq(Object.keys(o).map(k => o[k]), 6); });
check("assignPrim", () => { const o = Object.assign(1, getterSrc(6)); return typeof o === "object" && okSeq([o.p0, o.p1, o.p2, o.p3, o.p4, o.p5], 6); });
check("spread", () => { const o = { ...getterSrc(6) }; return okSeq(Object.keys(o).map(k => o[k]), 6); });
check("spreadProxy", () => { const o = { ...trapProxy() }; return okSeq([o.a, o.b, o.c, o.d], 4); });
check("rest", () => { const { p0, ...rest } = getterSrc(6); return p0.i === 0 && okSeq([{ i: 0 }, rest.p1, rest.p2, rest.p3, rest.p4, rest.p5], 6); });
check("restDyn", () => { const k = "p0"; const { [k]: z, ...rest } = getterSrc(6); return z.i === 0 && okSeq([{ i: 0 }, rest.p1, rest.p2, rest.p3, rest.p4, rest.p5], 6); });
check("restProxy", () => { const { a, ...rest } = trapProxy(); return a.i === 0 && okSeq([{ i: 0 }, rest.b, rest.c, rest.d], 4); });
check("restArr", () => { const arr = [0, 1, 2, 3]; for (let i = 0; i < 4; i++) Object.defineProperty(arr, i, { enumerable: true, get() { churn(); return { i }; } }); const { ...rest } = arr; return okSeq([rest[0], rest[1], rest[2], rest[3]], 4); });
check("restMap", () => { const m = new Map(); for (let i = 0; i < 4; i++) Object.defineProperty(m, "p" + i, { enumerable: true, get() { churn(); return { i }; } }); const { p0, ...rest } = m; return p0.i === 0 && okSeq([p0, rest.p1, rest.p2, rest.p3], 4); });
check("values", () => okSeq(Object.values(getterSrc(6)), 6));
check("entries", () => { const e = Object.entries(getterSrc(6)); return e.length === 6 && e.every((p, i) => p[0] === "p" + i && p[1].i === i); });
check("valuesProxy", () => okSeq(Object.values(trapProxy()), 4));
check("entriesProxy", () => { const e = Object.entries(trapProxy()); return e.length === 4 && e.every((p, i) => p[1].i === i); });
check("valuesArr", () => { const arr = []; for (let i = 0; i < 4; i++) Object.defineProperty(arr, i, { enumerable: true, get() { churn(); return { i }; } }); return okSeq(Object.values(arr), 4); });
check("entriesArr", () => { const arr = [0, 1, 2, 3]; arr.x = 9; Object.defineProperty(arr, "y", { enumerable: true, get() { churn(); return { i: 5 }; } }); for (let i = 0; i < 4; i++) Object.defineProperty(arr, i, { get() { churn(); return { i }; } }); const e = Object.entries(arr); return e.length === 6 && e.slice(0, 4).every((p, i) => p[0] === String(i) && p[1].i === i) && e[5][1].i === 5; });
check("valuesFn", () => { const f = function () {}; for (let i = 0; i < 4; i++) Object.defineProperty(f, "p" + i, { enumerable: true, get() { churn(); return { i }; } }); return okSeq(Object.values(f), 4); });
check("valuesMap", () => { const m = new Map(); for (let i = 0; i < 4; i++) Object.defineProperty(m, "p" + i, { enumerable: true, get() { churn(); return { i }; } }); return okSeq(Object.values(m), 4); });
check("valuesStrWrap", () => { const s = new String("ab"); for (let i = 0; i < 4; i++) Object.defineProperty(s, "p" + i, { enumerable: true, get() { churn(); return { i }; } }); const v = Object.values(s); return v[0] === "a" && okSeq(v.slice(2), 4); });
function arrayLike(n) { const al = { length: n }; for (let j = 0; j < n; j++) Object.defineProperty(al, j, { get() { churn(); return { i: j }; } }); return al; }
function F(a, b, c, d, e) { return [a, b, c, d, e]; }
check("apply", () => okSeq(F.apply(null, arrayLike(5)), 5));
check("applyNative", () => { const o = Object.assign.apply(null, [{}, getterSrc(3)]); return okSeq([o.p0, o.p1, o.p2], 3); });
check("reflectApply", () => okSeq(Reflect.apply(F, null, arrayLike(5)), 5));
check("reflectConstruct", () => { function C(a, b, c) { this.v = [a, b, c]; } return okSeq(Reflect.construct(C, arrayLike(3)).v, 3); });
check("ownKeysArrayLike", () => { const al = { length: 3 }; for (let j = 0; j < 3; j++) Object.defineProperty(al, j, { get() { churn(); return "k" + j + "_" + r; } }); var r = Math.random(); const ks = Reflect.ownKeys(new Proxy({}, { ownKeys() { return al; } })); return ks.length === 3 && ks.every((k, i) => typeof k === "string" && k.startsWith("k" + i)); });
check("objectCreate", () => { const descs = {}; for (let i = 0; i < 5; i++) Object.defineProperty(descs, "p" + i, { enumerable: true, get() { churn(); return { value: { i }, enumerable: true }; } }); const o = Object.create(null, descs); return okSeq([o.p0, o.p1, o.p2, o.p3, o.p4], 5); });
check("defineProperties", () => { const descs = {}; for (let i = 0; i < 5; i++) Object.defineProperty(descs, "p" + i, { enumerable: true, get() { churn(); return { value: { i }, enumerable: true }; } }); const o = Object.defineProperties({}, descs); return okSeq([o.p0, o.p1, o.p2, o.p3, o.p4], 5); });
check("ownKeysInvariant", () => { const inner = new Proxy({}, { isExtensible(t) { churn(); return Reflect.isExtensible(t); } }); const p = new Proxy(inner, { ownKeys() { const r = []; for (let i = 0; i < 20; i++) r.push("key_" + i + "_" + "x".repeat(3)); return r; } }); const ks = Reflect.ownKeys(p); return ks.length === 20 && ks.every((k, i) => k === "key_" + i + "_xxx"); });
check("toPropDesc", () => { const o = {}; Object.defineProperty(o, "x", { get value() { return { tag: "T" }; }, get writable() { churn(); return true; }, get enumerable() { churn(); return true; }, configurable: true }); churn(); return o.x && o.x.tag === "T"; });
check("toPropDescGet", () => { const o = {}; Object.defineProperty(o, "x", { get get() { return function () { return { tag: "G" }; }; }, get set() { churn(); return undefined; }, get enumerable() { churn(); return true; }, configurable: true }); churn(); return o.x && o.x.tag === "G"; });
check("defineTrapTarget", () => { const inner = new Proxy({}, { isExtensible(t) { churn(); return Reflect.isExtensible(t); } }); const p = new Proxy(inner, { defineProperty(t, k, d) { return Reflect.defineProperty(t, k, d); } }); Object.defineProperty(p, "q" + Math.random(), { value: 1, configurable: true }); return true; });
check("errorCause", () => { const e = new Error("m", { get cause() { churn(); return { c: 1 }; } }); churn(); return e.cause && e.cause.c === 1 && e.message === "m"; });
check("errorCauseReflect", () => { const e = Reflect.construct(TypeError, [{ toString() { churn(); return "msg"; } }, { get cause() { churn(); return { c: 2 }; } }]); churn(); return e.cause && e.cause.c === 2 && e.message === "msg" && e instanceof TypeError; });
check("errorCauseSubclass", () => { class E extends RangeError {} const e = new E("m", { get cause() { churn(); return { c: 3 }; } }); churn(); return e.cause && e.cause.c === 3 && e instanceof E; });
check("superFields", () => { const l = []; class A { f = function () { l.push("wrong"); }; g = (churn(), {}); constructor() { this.ok = 1; } } class B extends A { constructor() { super(); } } const b = new B(); return b.ok === 1 && l.length === 0; });
check("superFieldsImplicit", () => { class A { g = (churn(), {}); constructor() { this.ok = 1; } } class B extends A {} return new B().ok === 1; });
check("superBound", () => { class A { g = (churn(), {}); constructor(v) { this.v = v; } } class D extends A { constructor(a, b) { super(a); this.b = b; } } const X = D.bind(null, { v: 1 }); const x = new X({ v: 2 }); return x.v.v === 1 && x.b.v === 2; });
check("constructProxyNT", () => { class K { constructor() { this.t = typeof this; this.q = 1; } } const pr = { marker: 1 }; const NT = new Proxy(function () {}, { get(t, k) { if (k === "prototype") { churn(); return pr; } return t[k]; } }); const o = Reflect.construct(K, [], NT); return o.t === "object" && o.q === 1 && Object.getPrototypeOf(o) === pr; });
check("constructFunctionNT", () => { const fp = Object.create(Function.prototype); const NT = new Proxy(function () {}, { get(t, k) { if (k === "prototype") { churn(); return fp; } return t[k]; } }); const f = Reflect.construct(Function, ["return 7"], NT); return typeof f === "function" && f() === 7 && Object.getPrototypeOf(f) === fp; });
check("constructErrorNT", () => { const NT = new Proxy(function () {}, { get(t, k) { if (k === "prototype") { churn(); return Object.create(TypeError.prototype, { marker: { value: 1 } }); } return t[k]; } }); const e = Reflect.construct(TypeError, [{ toString() { churn(); return "m"; } }], NT); return e.marker === 1 && e.message === "m"; });
check("constructDisposableNT", () => { if (typeof DisposableStack !== "function") return true; const pr = Object.create(DisposableStack.prototype); const NT = new Proxy(function () {}, { get(t, k) { if (k === "prototype") { churn(); return pr; } return t[k]; } }); const s = Reflect.construct(DisposableStack, [], NT); s.defer(() => {}); s.dispose(); return s.disposed === true; });
check("builtinSubclassImplicit", () => { class MyP extends Promise {} const p = new MyP(function () { churn(); }); return Object.getPrototypeOf(p) === MyP.prototype && p instanceof MyP && p instanceof Promise; });
check("flatMap", () => okSeq([0, 1, 2, 3, 4, 5].flatMap(i => { churn(); return { i }; }), 6));
check("flatGetters", () => { const arr = [0, 1, 2, 3]; for (let i = 0; i < 4; i++) Object.defineProperty(arr, i, { get() { churn(); return [{ i }]; } }); return okSeq(arr.flat(), 4); });
console.log(out.join("\n"));
"#;

const GC_CHECKS: usize = 42;

#[test]
fn audit_20260915_gc_natives_child() {
    let Some(size) = std::env::var_os("ZIPP_AUDIT_GC_NATIVES_CHILD") else {
        return;
    };
    let (rounds, churn) = if size == "stress" { (2, 40) } else { (8, 3000) };
    let src = format!("const ROUNDS = {rounds}, CHURN = {churn};\n{GC_PROBES}");
    let out = run_ok(&src);
    assert_eq!(out.len(), 1, "{out:?}");
    let lines: Vec<&str> = out[0].lines().collect();
    assert_eq!(lines.len(), GC_CHECKS, "{lines:?}");
    let failed: Vec<&str> = lines.iter().copied().filter(|l| !l.ends_with("=0")).collect();
    assert!(failed.is_empty(), "getter/trap results lost to GC: {failed:?}");
}

#[test]
fn audit_20260915_gc_natives_modes() {
    if std::env::var_os("ZIPP_AUDIT_GC_NATIVES_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, size, env) in [
        ("default", "natural", &[][..]),
        ("interpreter", "natural", &[("ZIPP_NOJIT", "1")][..]),
        ("forced-jit", "natural", &[("ZIPP_JIT_THRESHOLD", "1")][..]),
        ("majors-only", "natural", &[("ZIPP_NO_NURSERY", "1")][..]),
        ("gc-stress", "stress", &[("ZIPP_GC_STRESS", "1")][..]),
        // The full mark beside every minor: a young object the minor's
        // young-only trace missed — a value rooted through an old holder with
        // no write barrier, or a root pushed after the promotion — panics here
        // instead of silently surviving on a stale slot.
        ("nursery-verify", "natural", &[("ZIPP_NURSERY_VERIFY", "1")][..]),
        (
            "nursery-verify-stress",
            "stress",
            &[("ZIPP_NURSERY_VERIFY", "1"), ("ZIPP_GC_STRESS", "1")][..],
        ),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "audit_20260915_gc_natives_child", "--nocapture"])
            // An inherited setting must not decide what a mode tests.
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS")
            .env_remove("ZIPP_NO_NURSERY")
            .env_remove("ZIPP_NURSERY_VERIFY")
            .env("ZIPP_AUDIT_GC_NATIVES_CHILD", size);
        for (key, value) in env {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn mode child");
        assert!(
            out.status.success(),
            "{mode} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Every temporary-root scope unwinds on success AND on a guest throw in the
/// middle of the walk (a getter, a trap, a mapper), so nothing stays rooted
/// between host calls.
const ROOT_BALANCE: &str = r#"
    function churn() { const g = []; for (let i = 0; i < 64; i++) g.push({ i }); return g; }
    function bomb(n) {
        const o = {};
        for (let i = 0; i < 3; i++) Object.defineProperty(o, "p" + i, { enumerable: true, get() {
            churn(); if (i === n) throw new Error("getter " + i); return { i };
        } });
        return o;
    }
    function arrayLike(n) {
        const al = { length: 3 };
        for (let j = 0; j < 3; j++) Object.defineProperty(al, j, { get() {
            churn(); if (j === n) throw new Error("index " + j); return { j };
        } });
        return al;
    }
    function F(a, b, c) { return [a, b, c]; }
    const cases = {
        values: n => Object.values(bomb(n)).length,
        entries: n => Object.entries(bomb(n)).length,
        assign: n => Object.keys(Object.assign({}, bomb(n))).length,
        spread: n => Object.keys({ ...bomb(n) }).length,
        rest: n => { const { p0, ...r } = bomb(n); return Object.keys(r).length + 1; },
        apply: n => F.apply(null, arrayLike(n)).length,
        reflectConstruct: n => { function C(a, b, c) { this.n = [a, b, c].length; } return Reflect.construct(C, arrayLike(n)).n; },
        create: n => Object.keys(Object.create(null, Object.fromEntries(
            [0, 1, 2].map(i => [i, { enumerable: true, get value() { churn(); if (i === n) throw new Error("desc " + i); return i; } }])
        ))).length,
        flatMap: n => [0, 1, 2].flatMap(i => { churn(); if (i === n) throw new Error("mapper " + i); return [i]; }).length,
        construct: n => { const NT = new Proxy(function () {}, { get(t, k) {
            if (k === "prototype") { churn(); if (n >= 0) throw new Error("prototype"); return {}; } return t[k]; } });
            class K { constructor() { this.v = 1; } } return Reflect.construct(K, [1, 2, 3], NT).v + 2; },
        ownKeys: n => Reflect.ownKeys(new Proxy({}, { ownKeys() { churn(); return n >= 0 ? ["a", 1] : ["a", "b", "c"]; } })).length,
        arraySubclass: n => { class A extends Array {} const NT = new Proxy(function () {}, { get(t, k) {
            if (k === "prototype") { churn(); if (n >= 0) throw new Error("prototype"); return A.prototype; } return t[k]; } });
            return Reflect.construct(A, [1, 2, 3], NT).length; },
        promiseSubclass: n => { class P extends Promise {} new P(n >= 0 ? 5 : r => { churn(); r(1); }); return 3; },
        arrayNonObjectProto: n => { function Q() {} Q.prototype = 3; if (n >= 0) Q.prototype = { get x() { return 0; } };
            const a = Reflect.construct(Array, [1, 2, 3], Q); if (n >= 0) throw new Error("after"); return a.length; },
        flatArrayLike: n => Array.prototype.flat.call({ length: 3, 0: [1], 1: 2, get 2() {
            churn(); if (n >= 0) throw new Error("getter"); return [3]; } }).length,
        superFields: n => { class A { g = (churn(), {}); constructor(v) { if (n >= 0) throw new Error("base ctor"); this.v = v; } }
            class B extends A { constructor() { super(3); } } return new B().v; },
        boundDerived: n => { class A { g = (churn(), {}); constructor(v) { this.v = v; } }
            class D extends A { constructor(a) { super(a); if (n >= 0) throw new Error("derived"); } }
            return new (D.bind(null, 3))().v; },
        errorCause: n => new Error("m", { get cause() { churn(); if (n >= 0) throw new Error("cause"); return 3; } }).cause,
        aggErrors: n => new AggregateError({ [Symbol.iterator]() { let i = 0; return { next() {
            churn(); if (n >= 0) throw new Error("iter"); return i++ < 3 ? { value: i, done: false } : { done: true }; } }; } }, "m").errors.length,
        // (The iterable is a local: an object literal written INLINE in an
        // array-literal spread loses its enclosing function's upvalues on the
        // base build too — a pre-existing compile bug, not this track's.)
        spreadIter: n => { const src = { [Symbol.iterator]() { let i = 0; return { next() {
            churn(); if (n >= 0) throw new Error("next"); return i++ < 3 ? { value: i, done: false } : { done: true }; } }; } }; return [...src].length; },
        readDescriptor: n => Object.keys(Object.defineProperty({}, "k", {
            get value() { churn(); return 3; },
            get enumerable() { churn(); if (n >= 0) throw new Error("enumerable"); return true; },
        })).length + 2,
    };
    function go(name, n) { try { return String(cases[name](n)); } catch (e) { return "threw:" + e.message; } }
"#;

fn root_fixture() -> ScriptState {
    std::env::set_var("ZIPP_GC_STRESS", "1");
    let mut state = compile_script(ROOT_BALANCE).expect("compile root-balance fixture");
    state.run_init().expect("initialize root-balance fixture");
    state
}

#[test]
fn temporary_root_scopes_unwind_on_success_and_guest_throws() {
    let mut state = root_fixture();
    for name in [
        "values",
        "entries",
        "assign",
        "spread",
        "rest",
        "apply",
        "reflectConstruct",
        "create",
        "flatMap",
        "construct",
        "ownKeys",
        "arraySubclass",
        "promiseSubclass",
        "arrayNonObjectProto",
        "flatArrayLike",
        "superFields",
        "boundDerived",
        "errorCause",
        "aggErrors",
        "spreadIter",
        "readDescriptor",
    ] {
        for n in [-1.0, 1.0] {
            let got = state.call_global(
                "go",
                &[JsValue::String(name.into()), JsValue::Number(n)],
            );
            let JsValue::String(s) = got.expect("go() returns a string") else {
                panic!("{name}: non-string result");
            };
            if n < 0.0 {
                assert_eq!(s, "3", "{name} on the success path");
            } else {
                assert!(s.starts_with("threw:"), "{name}({n}) should throw, got {s}");
            }
            assert_eq!(state.host_result_roots_for_test(), 0, "{name}({n}) leaked roots");
        }
    }
}

/// Node v24's output for each line.
const SPEC_PROBES: &str = r#"
const out = [];
function t(name, fn) { let r; try { r = fn(); } catch (e) { r = "THROW " + e.constructor.name; } out.push(name + " " + JSON.stringify(r)); }
t("freeze-define", () => { const a = Object.freeze([1, 2]); return [Reflect.defineProperty(a, 0, { value: 9 }), a[0], Object.isFrozen(a)]; });
t("seal-accessor", () => { const s = Object.seal([1, 2]); return [Reflect.defineProperty(s, 0, { get() { return 7; } }), s[0]]; });
t("seal-value", () => { const s = Object.seal([1, 2]); return [Reflect.defineProperty(s, 0, { value: 8 }), s[0]]; });
t("seal-enumerable", () => { const s = Object.seal([1, 2]); return [Reflect.defineProperty(s, 0, { enumerable: false }), Object.keys(s)]; });
t("frozen-define-throws", () => { const a = Object.freeze([1, 2]); Object.defineProperty(a, 1, { value: 5 }); return a[1]; });
t("freeze-fn", () => { function foo() {} Object.freeze(foo); return [Reflect.defineProperty(foo, "name", { value: "bar" }), foo.name, Reflect.defineProperty(foo, "length", { value: 5 }), foo.length]; });
t("freeze-arguments", () => (function (x) { Object.freeze(arguments); return [Reflect.defineProperty(arguments, 0, { value: 9 }), arguments[0], x]; })(1));
t("nonext-define", () => { const a = Object.preventExtensions([1, 2]); return [Reflect.defineProperty(a, 0, { value: 9 }), a[0]]; });
t("args-names", () => (function () { return Object.getOwnPropertyNames(arguments); })(1, 2));
t("args-proxy", () => (function () { return Reflect.ownKeys(new Proxy(arguments, { ownKeys(t) { return Reflect.ownKeys(t); } })).map(String); })(1, 2));
t("args-delete-length", () => (function () { delete arguments.length; return Object.getOwnPropertyNames(arguments); })(1, 2));
t("args-strict", () => (function () { "use strict"; return Object.getOwnPropertyNames(arguments); })(1, 2));
t("flat-nested-hole", () => { const r = [[1, , 3]].flat(); return [r.length, 1 in r]; });
t("flat-top-hole", () => [1, , 3].flat().length);
t("flat-keys", () => { const r = [1, , [2, , 3]].flat(); return [r.length, Object.keys(r)]; });
t("flat-accessor", () => { const inner = [1, 2]; Object.defineProperty(inner, 1, { get() { return "getter"; } }); return [inner].flat(); });
t("flat-proxy", () => [new Proxy([7, 8], {})].flat());
t("flat-shrunk", () => { const a = [1, 2]; a.length = 4; return [a].flat(); });
t("flat-inherited", () => { Array.prototype[1] = "proto"; try { return [0, , 2].flat(); } finally { delete Array.prototype[1]; } });
t("flat-infinity", () => [1, [2, , 3]].flat(Infinity).length);
t("flat-species", () => { class MyA extends Array {} const r = MyA.from([[1], [2]]).flat(); return [r instanceof MyA, r.length]; });
// The dense-source proof the walk hoists must not survive guest code that
// installs an accessor, a hole, a length or a prototype index under it.
t("flat-mapper-mutates", () => [1, 2, 3].flatMap((v, i, a) => { if (i === 0) { Object.defineProperty(a, 1, { get() { return 99; }, configurable: true }); delete a[2]; } return [v]; }));
t("flat-proxy-length-mutates", () => { const src = [0, 0]; const inner = new Proxy([7, 8], { get(t, k) { if (k === "length") { src[1] = 5; Object.defineProperty(src, 0, { get() { return 1; }, configurable: true }); } return t[k]; } }); src[0] = inner; return src.flat(); });
t("flat-sparse", () => { const a = []; a[0] = [1]; a[5] = [2]; return [a.flat(), a.flat().length]; });
t("flat-nested-sparse", () => { const n = []; n[3] = 9; return [[n].flat(), [n].flat().length]; });
t("flat-proto-index", () => { Array.prototype[1] = "P"; try { const a = [[0]]; a.length = 3; return a.flat(); } finally { delete Array.prototype[1]; } });
t("values-array-accessor-adds", () => { const a = [1, 2, 3]; Object.defineProperty(a, 0, { get() { a[5] = 6; a.foo = 7; return 9; }, enumerable: true, configurable: true }); return [Object.values(a), Object.keys(a)]; });
t("values-array-bigindex", () => { const a = [1]; a[4294967295] = "big"; a[4294967294] = "idx"; return [Object.keys(a), Object.values(a)]; });
t("nel-number", () => [Number("\u008512"), +"\u00851", "\u00851" == 1, Math.abs("\u00851")]);
t("nel-bigint", () => { const r = [1n == "\u00851", 1n > "\uFEFF", 1n < "\uFEFF2"]; try { r.push(String(BigInt("\u00851"))); } catch (e) { r.push(e.name); } try { r.push(String(BigInt("\uFEFF1"))); } catch (e) { r.push(e.name); } return r; });
t("agg-order", () => { const log = []; try { Reflect.construct(AggregateError, [{ [Symbol.iterator]() { log.push("iter"); return [][Symbol.iterator](); } }, { toString() { log.push("msg"); return "m"; } }, { get cause() { log.push("cause"); } }]); } catch (e) {} return log; });
t("agg-props", () => Object.getOwnPropertyNames(new AggregateError(new Set([1, 2]), "msg", { cause: "c" })).filter(k => k === "cause" || k === "errors"));
t("agg-subclass", () => { class A extends AggregateError {} const e = new A([1, 2], "m", { cause: "c" }); return [Object.getOwnPropertyNames(e).filter(k => k !== "stack" && k !== "name"), e.errors, e.cause, e.message]; });
t("error-subclass-cause", () => { class E extends TypeError {} const e = new E("m", { cause: 5 }); return [e.cause, Object.getOwnPropertyNames(e).filter(k => k !== "stack" && k !== "name")]; });
t("apply-huge", () => { function f() { return arguments.length; } return f.apply(null, { length: 2 ** 32 - 1 }); });
t("reflect-apply-huge", () => { function f() { return arguments.length; } return Reflect.apply(f, null, { length: 2 ** 32 - 1 }); });
t("apply-large", () => { function f() { return arguments.length; } return f.apply(null, { length: 70000 }); });
t("newtarget-fallback", () => { function Q() {} Q.prototype = 3; function F() {} class P {} class Der extends P {} class MyP extends Promise {} class MyA extends Array {}
  return [Object.getPrototypeOf(Reflect.construct(F, [], Q)) === Object.prototype, Object.getPrototypeOf(Reflect.construct(P, [], Q)) === Object.prototype,
    Object.getPrototypeOf(Reflect.construct(Der, [], Q)) === Object.prototype, Object.getPrototypeOf(Reflect.construct(MyP, [function () {}], Q)) === Promise.prototype,
    Object.getPrototypeOf(Reflect.construct(MyA, [], Q)) === Array.prototype, Object.getPrototypeOf(Reflect.construct(Array, [], Q)) === Array.prototype,
    Object.getPrototypeOf(Reflect.construct(F, [], Q.bind())) === Object.prototype]; });
t("fn-key-order", () => { function f() {} f.b = 1; f[1] = 2; return [Object.keys(f), Reflect.ownKeys(f)]; });
t("string-key-order", () => { const s = new String("ab"); s.b = 1; s[5] = 1; s.a = 1; s[3] = 1; return [Object.keys(s), Object.getOwnPropertyNames(s)]; });
t("regexp-key-order", () => { const r = /x/; r.b = 1; r[3] = 1; r.a = 1; r[1] = 1; return [Object.getOwnPropertyNames(r), Object.keys(r)]; });
t("arrow-for-in-order", () => { const f = () => {}; f.z = 1; f[0] = 1; const k = []; for (const x in f) k.push(x); return [Object.keys(f), k]; });
t("values-after-delete", () => { const a = [1, 2, 3]; Object.defineProperty(a, 0, { get() { delete a[1]; return 9; }, enumerable: true, configurable: true }); return Object.values(a); });
t("entries-after-hide", () => { const a = [1, 2, 3]; Object.defineProperty(a, 0, { get() { Object.defineProperty(a, 2, { enumerable: false }); return 9; }, enumerable: true, configurable: true }); return Object.entries(a); });
t("entries-after-shrink", () => { const a = [1, 2, 3]; Object.defineProperty(a, 0, { get() { a.length = 1; return 9; }, enumerable: true, configurable: true }); return Object.entries(a); });
t("eval-rest-pattern", () => [(function (...[a = eval("1")]) { return a; })(), (function (...[a = eval("2")]) { return a; })(7)]);
t("eval-array-param", () => [(function ([a = (eval("var q = 1"), 3)]) { return a; })([]), (function ([a = (eval("var q = 1"), 3)]) { return a; })([9])]);
t("eval-param-default", () => [(function ([a] = [eval("4")]) { return a; })(), (function ({ x } = { x: eval("5") }, [y] = [6]) { return [x, y]; })()]);
t("eval-body-pattern", () => (function (p, { x }, [y]) { eval(""); return [p, x, y]; })(1, { x: 2 }, [3]));
console.log(out.join("\n"));
"#;

const SPEC_EXPECTED: &str = r#"freeze-define [false,1,true]
seal-accessor [false,1]
seal-value [true,8]
seal-enumerable [false,["0","1"]]
frozen-define-throws "THROW TypeError"
freeze-fn [false,"foo",false,0]
freeze-arguments [false,1,1]
nonext-define [true,9]
args-names ["0","1","length","callee"]
args-proxy ["0","1","length","callee","Symbol(Symbol.iterator)"]
args-delete-length ["0","1","callee"]
args-strict ["0","1","length","callee"]
flat-nested-hole [2,true]
flat-top-hole 2
flat-keys [3,["0","1","2"]]
flat-accessor [1,"getter"]
flat-proxy [7,8]
flat-shrunk [1,2]
flat-inherited [0,"proto",2]
flat-infinity 3
flat-species [true,2]
flat-mapper-mutates [1,99]
flat-proxy-length-mutates [7,8,5]
flat-sparse [[1,2],2]
flat-nested-sparse [[9],1]
flat-proto-index [0,"P"]
values-array-accessor-adds [[9,2,3],["0","1","2","5","foo"]]
values-array-bigindex [["0","4294967294","4294967295"],[1,"idx","big"]]
nel-number [null,null,false,null]
nel-bigint [false,true,true,"SyntaxError","1"]
agg-order ["msg","cause","iter"]
agg-props ["cause","errors"]
agg-subclass [["message","cause","errors"],[1,2],"c","m"]
error-subclass-cause [5,["message","cause"]]
apply-huge "THROW RangeError"
reflect-apply-huge "THROW RangeError"
apply-large 70000
newtarget-fallback [true,true,true,true,true,true,true]
fn-key-order [["1","b"],["1","length","name","arguments","caller","prototype","b"]]
string-key-order [["0","1","3","5","b","a"],["0","1","3","5","length","b","a"]]
regexp-key-order [["1","3","lastIndex","b","a"],["1","3","b","a"]]
arrow-for-in-order [["0","z"],["0","z"]]
values-after-delete [9,3]
entries-after-hide [["0",9],["1",2]]
entries-after-shrink [["0",9]]
eval-rest-pattern [1,7]
eval-array-param [3,9]
eval-param-default [4,[5,6]]
eval-body-pattern [1,2,3]"#;

#[test]
fn spec_fixes_match_node_child() {
    if std::env::var_os("ZIPP_AUDIT_SPEC_FIXES_CHILD").is_none() {
        return;
    }
    let out = run_ok(SPEC_PROBES);
    assert_eq!(out.len(), 1, "{out:?}");
    let got: Vec<&str> = out[0].lines().collect();
    let want: Vec<&str> = SPEC_EXPECTED.lines().collect();
    assert_eq!(got.len(), want.len(), "{got:#?}");
    for (g, w) in got.iter().zip(want.iter()) {
        assert_eq!(g, w);
    }
}

/// Own-key order, the array `Object.values`/`entries` re-check and the boxed
/// destructured parameters all have tier-specific paths, so the whole spec
/// sheet runs in every tier, not just the default one.
#[test]
fn spec_fixes_match_node() {
    if std::env::var_os("ZIPP_AUDIT_SPEC_FIXES_CHILD").is_some() {
        return;
    }
    run_in_modes(
        "spec_fixes_match_node_child",
        "ZIPP_AUDIT_SPEC_FIXES_CHILD",
        &[
            ("default", &[]),
            ("interpreter", &[("ZIPP_NOJIT", "1")]),
            ("forced-jit", &[("ZIPP_JIT_THRESHOLD", "1")]),
            ("gc-stress", &[("ZIPP_GC_STRESS", "1")]),
            ("nursery-verify", &[("ZIPP_NURSERY_VERIFY", "1")]),
        ],
    );
}

/// Re-run one `#[test]` of this binary once per tier, in a clean child
/// process so the latched-at-construction GC and JIT switches take effect.
fn run_in_modes(child_test: &str, gate: &str, modes: &[(&str, &[(&str, &str)])]) {
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in modes {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", child_test, "--nocapture"])
            // An inherited setting must not decide what a mode tests.
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS")
            .env_remove("ZIPP_NO_NURSERY")
            .env_remove("ZIPP_NURSERY_VERIFY")
            .env(gate, "1");
        for (key, value) in *env {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn mode child");
        assert!(
            out.status.success(),
            "{mode} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// The iterator protocol behind for-of, spread and destructuring: a string
/// is walked by code point (a surrogate pair stays whole, a lone surrogate is
/// not U+FFFD); an overridden `@@iterator` (own, subclass, patched prototype)
/// or a patched `%XIteratorPrototype%.next` is honoured for Array / Set / Map
/// / String / TypedArray; a `return` reachable from a built-in iterator
/// prototype is called by `break` / `return` / `throw` and by destructuring's
/// close (and one on the iterable's own prototype is not); a non-iterable
/// destructuring source is a TypeError; a TypedArray spread follows its
/// current length and throws once detached or out of bounds; a binding or
/// parameter pattern with defaults or nested patterns steps its iterator
/// between them. Node v24's output; every mode, because the JIT has its own
/// GetIterator admission.
const ITER_PROBES: &str = r#"
const out = [];
function t(name, fn) { let r; try { r = fn(); } catch (e) { r = "THROW " + (e && e.constructor && e.constructor.name); } out.push(name + " " + JSON.stringify(r)); }
const units = a => a.map(c => { if (typeof c !== "string") return String(c); let h = ""; for (let i = 0; i < c.length; i++) h += (i ? "+" : "") + c.charCodeAt(i).toString(16); return h; }).join(",");
const hex = s => units([...s]);
// R278
t("decl-astral", () => { const [x, y, z] = "a\u{1F600}b"; return units([x, y, z]); });
t("param-astral", () => (function ([u, v]) { return units([u, v]); })("a\u{1F600}b"));
t("arrow-astral", () => (([a, b]) => units([a, b]))("a\u{1F600}b"));
t("let-single", () => { let [l1, l2] = "\u{1F600}"; return [units([l1]), l2 === undefined]; });
t("rest-astral", () => { const [h, ...r] = "\u{1F600}b"; return [units([h]), units(r)]; });
t("forof-destructure", () => { const res = []; for (const [c, d] of ["\u{1F600}x"]) res.push(units([c, d])); return res; });
t("wrapper-destructure", () => { const [a, b] = new String("\u{1F600}x"); return [a.length, b]; });
t("empty-obj", () => { const [] = {}; return "no throw"; });
t("param-empty-obj", () => { (function ([]) {})({}); return "no throw"; });
t("arraylike", () => { const [a] = { 0: 1, length: 1 }; return a; });
t("fn-no-iter", () => { const [a] = function () {}; return a; });
t("proxy-arr", () => { const [a, b] = new Proxy([1, 2], {}); return [a, b]; });
t("str-proto-iter", () => { const o = String.prototype[Symbol.iterator]; String.prototype[Symbol.iterator] = function* () { yield "Z"; }; try { const [a] = "abc"; return [a, [..."ab"], Array.from("ab")]; } finally { String.prototype[Symbol.iterator] = o; } });
t("map-proto-iter", () => { const o = Map.prototype[Symbol.iterator]; Map.prototype[Symbol.iterator] = function* () { yield "M"; }; try { const [a] = new Map([[1, 2]]); const r = []; for (const x of new Map([[1, 2]])) r.push(x); return [a, [...new Map([[1, 2]])], r]; } finally { Map.prototype[Symbol.iterator] = o; } });
t("aip-next-patched", () => { const AIP = Object.getPrototypeOf([][Symbol.iterator]()); const o = AIP.next; let c = 0; AIP.next = function () { return c++ < 1 ? { value: "P" + c, done: false } : { value: undefined, done: true }; }; try { const [a] = [1]; c = 0; const s = [...[1]]; c = 0; const r = []; for (const x of [1, 2]) r.push(x); c = 0; function f(...args) { return args; } const fa = f(...[1, 2]); c = 0; return [a, s, r, fa]; } finally { AIP.next = o; } });
// R126/R165
t("set-subclass", () => { class S extends Set { *[Symbol.iterator]() { yield "sub"; } } const r = []; for (const v of new S([1, 2])) r.push(v); const [a] = new S([1]); return [r, [...new S([1, 2])], a, Array.from(new S([1, 2]))]; });
t("sorted-set", () => { class SortedSet extends Set { *[Symbol.iterator]() { yield* [...super.values()].sort((a, b) => a - b); } } const r = []; for (const v of new SortedSet([3, 1, 2])) r.push(v); return r; });
t("map-subclass", () => { class M extends Map { *[Symbol.iterator]() { yield "subm"; } } const r = []; for (const v of new M([[1, 2]])) r.push(v); return r; });
t("own-set-iter", () => { const s = new Set([1, 2]); s[Symbol.iterator] = function* () { yield "own"; }; const r = []; for (const v of s) r.push(v); const [d] = s; return [[...s], Array.from(s), r, d]; });
t("proto-set-iter", () => { const o = Set.prototype[Symbol.iterator]; Set.prototype[Symbol.iterator] = function* () { yield "S"; }; try { const r = []; for (const v of new Set([1, 2])) r.push(v); const [d, e] = new Set([1, 2]); return [[...new Set([1, 2])], r, d, e === undefined]; } finally { Set.prototype[Symbol.iterator] = o; } });
t("proto-str-forof", () => { const o = String.prototype[Symbol.iterator]; String.prototype[Symbol.iterator] = function* () { yield "strp"; }; try { const r = []; for (const v of "ab") r.push(v); return [r, [..."ab"]]; } finally { String.prototype[Symbol.iterator] = o; } });
t("proto-ta-iter", () => { const P = Object.getPrototypeOf(Uint8Array.prototype); const o = P[Symbol.iterator]; P[Symbol.iterator] = function* () { yield "T"; }; try { const r = []; for (const v of new Uint8Array(2)) r.push(v); return [[...new Uint8Array(2)], r]; } finally { P[Symbol.iterator] = o; } });
t("setiter-next", () => { const SIP = Object.getPrototypeOf(new Set()[Symbol.iterator]()); const o = SIP.next; SIP.next = () => ({ done: true }); try { const r = []; for (const v of new Set([1])) r.push(v); return [[...new Set([1, 2])], r]; } finally { SIP.next = o; } });
t("aip-next-done", () => { const AIP = Object.getPrototypeOf([][Symbol.iterator]()); const o = AIP.next; AIP.next = () => ({ done: true }); try { function f(...a) { return a.length; } const [d, e] = [1, 2]; return [[...[1, 2]], f(...[1, 2]), [d === undefined, e === undefined], Array.from([1, 2])]; } finally { AIP.next = o; } });
// R164
t("spread-lone", () => hex("a\uD800b\uDC00c\u{1F600}"));
t("call-spread-lone", () => (function (...a) { return units(a); })(..."a\uD800b\uDC00c\u{1F600}"));
t("new-spread-lone", () => { function F(...a) { this.a = a; } return units(new F(..."a\uD800b").a); });
t("rest-lone", () => { const [x, ...r] = "a\uD800b\uDC00"; return units([x, ...r]); });
t("mapGroupBy-lone", () => [...Map.groupBy("𐀀\uDC00", c => c).keys()].map(k => units([k])));
t("agg-lone", () => units(new AggregateError("a\uD800b").errors));
t("hot-lone", () => { let r = 0; for (let i = 0; i < 20000; i++) r = [..."\uD800"][0].charCodeAt(0); return r; });
// R293
t("ta-detached", () => { const u = new Uint8Array(4); u.buffer.transfer(); return [...u]; });
t("ta-detached-call", () => { const u = new Uint8Array(4); u.buffer.transfer(); return (function () { return arguments.length; })(...u); });
t("ta-oob", () => { const b = new ArrayBuffer(8, { maxByteLength: 8 }); const v = new Uint8Array(b, 4, 4); b.resize(2); return [...v]; });
t("ta-grow", () => { const b = new ArrayBuffer(2, { maxByteLength: 4 }); const v = new Uint8Array(b); b.resize(4); return [v.length, [...v].length, Array.from(v).length, (function () { return arguments.length; })(...v)]; });
t("ta-shrink", () => { const b = new ArrayBuffer(4, { maxByteLength: 4 }); const v = new Uint8Array(b); v[0] = 1; b.resize(1); return [...v]; });
t("ta-own-iter", () => { const u = new Uint8Array(2); u[Symbol.iterator] = function* () { yield 42; }; return [[...u], Array.from(u)]; });
t("ta-hot-detached", () => { let r; for (let i = 0; i < 20000; i++) { const u = new Uint8Array(4); try { r = [...u].length; } catch (e) { r = e.name; } } const u = new Uint8Array(4); u.buffer.transfer(); try { r = [...u].length; } catch (e) { r = e.name; } return r; });
t("ab-detached-max", () => { const b = new ArrayBuffer(2, { maxByteLength: 8 }); b.transfer(); const f = new ArrayBuffer(4); f.transfer(); return [b.maxByteLength, b.resizable, b.byteLength, b.detached, f.maxByteLength, new ArrayBuffer(4).maxByteLength, Object.getOwnPropertyDescriptor(ArrayBuffer.prototype, "maxByteLength").get.call(b)]; });
// R086
t("aip-return", () => { const AIP = Object.getPrototypeOf([][Symbol.iterator]()); let closed = 0; AIP.return = function () { closed++; return {}; }; try { for (const x of [1, 2, 3]) { break; } (function () { for (const x of [1, 2, 3]) return; })(); try { for (const x of [1, 2, 3]) throw 1; } catch (e) {} var [a] = [1, 2, 3]; return [closed, a]; } finally { delete AIP.return; } });
t("iterproto-return", () => { const IP = Object.getPrototypeOf(Object.getPrototypeOf([][Symbol.iterator]())); let closed = 0; IP.return = function () { closed++; return {}; }; try { for (const x of [1, 2, 3]) { break; } for (const x of new Set([1, 2])) { break; } for (const x of "ab") { break; } for (const x of new Map([[1, 2], [3, 4]])) { break; } return closed; } finally { delete IP.return; } });
t("aip-return-hot", () => { const AIP = Object.getPrototypeOf([][Symbol.iterator]()); let closed = 0; function g() { for (const x of [1, 2, 3]) return x; } for (let i = 0; i < 5000; i++) g(); AIP.return = function () { closed++; return {}; }; try { for (let i = 0; i < 5000; i++) g(); return closed; } finally { delete AIP.return; } });
t("no-return-normal", () => { let s = 0; for (const x of [1, 2, 3]) s += x; for (const x of new Set([4])) s += x; for (const c of "ab") s += c.length; const [a, b] = [5, 6]; return s + a + b; });
// The pristine proofs are memoized per prototype version: a warmed site must
// still see an in-place patch, an accessor, a delete, and each restore.
t("memo-invalidation", () => {
  const AIP = Object.getPrototypeOf([][Symbol.iterator]());
  const arr = [1, 2], s = new Set([3]), str = "ab";
  const run = () => { let o = ""; for (const x of arr) o += x; for (const x of s) o += x; for (const c of str) o += c; const [a] = arr; const [b] = str; return o + a + b + [...s, ...str].join(""); };
  const r = [];
  for (let i = 0; i < 3000; i++) run();
  r.push(run());
  const next = AIP.next; AIP.next = function () { return { done: true }; }; r.push(run()); AIP.next = next; r.push(run());
  const d = Object.getOwnPropertyDescriptor(Array.prototype, Symbol.iterator);
  Object.defineProperty(Array.prototype, Symbol.iterator, { get() { return function* () { yield "G"; }; }, configurable: true });
  r.push(run()); Object.defineProperty(Array.prototype, Symbol.iterator, d); r.push(run());
  const sd = Set.prototype[Symbol.iterator]; Set.prototype[Symbol.iterator] = function* () { yield "S"; }; r.push(run()); Set.prototype[Symbol.iterator] = sd; r.push(run());
  const strd = String.prototype[Symbol.iterator]; delete String.prototype[Symbol.iterator]; try { run(); r.push("no throw"); } catch (e) { r.push(e.constructor.name); } String.prototype[Symbol.iterator] = strd; r.push(run());
  Object.prototype.return = function () { r.push("closed"); return {}; }; for (const x of arr) break; delete Object.prototype.return; r.push(run());
  return r;
});
// A memo hit must also fall when the built-in iterator prototype's own
// [[Prototype]] is REPLACED — an inherited `return` is then reachable from a
// warmed site whose recorded ancestor chain no longer applies.
t("memo-proto-swap", () => {
  const probe = (warm, proto, run) => {
    for (let i = 0; i < 3000; i++) warm();
    let closed = 0;
    const oldP = Object.getPrototypeOf(proto);
    Object.setPrototypeOf(proto, { __proto__: oldP, return() { closed++; return {}; } });
    run();
    Object.setPrototypeOf(proto, oldP);
    run();
    return closed;
  };
  return [
    probe(() => { for (const x of [1, 2]) break; }, Object.getPrototypeOf([][Symbol.iterator]()),
      () => { for (const x of [1, 2, 3]) break; const [a] = [1, 2]; }),
    probe(() => { for (const x of new Set([1, 2])) break; }, Object.getPrototypeOf(new Set([1])[Symbol.iterator]()),
      () => { for (const x of new Set([1, 2, 3])) break; const [a] = new Set([1, 2]); }),
    probe(() => { for (const x of "ab") break; }, Object.getPrototypeOf(""[Symbol.iterator]()),
      () => { for (const x of "abc") break; const [a] = "ab"; }),
    probe(() => { for (const x of new Map([[1, 2]])) break; }, Object.getPrototypeOf(new Map([[1, 2]])[Symbol.iterator]()),
      () => { for (const x of new Map([[1, 2], [3, 4]])) break; const [a] = new Map([[1, 2], [3, 4]]); }),
  ];
});
// A `return` on the ITERABLE's prototype is not the iterator's: an array or
// string iterator never inherits from Array.prototype / String.prototype.
t("iterable-proto-return", () => { const log = []; const ks = [Array.prototype, String.prototype, Set.prototype]; ks.forEach((p, i) => { p.return = function () { log.push(i); return {}; }; }); try { for (const x of [1, 2]) break; for (const x of "ab") break; for (const x of new Set([1, 2])) break; var [a] = [1, 2]; var [c = 0] = [1, 2]; return [log, a, c]; } finally { ks.forEach(p => { delete p.return; }); } });
// R166: a binding / parameter array pattern steps its iterator once per
// element, running each default or nested pattern right after its own step.
function logIter(log, vals) { return { [Symbol.iterator]() { let i = 0; return { next() { log.push("next" + i); return i < vals.length ? { value: vals[i++], done: false } : { value: undefined, done: true }; }, return() { log.push("return"); return {}; } }; } }; }
t("gen-defaults", () => { const log = []; function* g() { log.push("g0"); yield undefined; log.push("g1"); yield 2; } const [a = (log.push("default"), 1), b] = g(); return [log, a, b]; });
t("iter-defaults", () => { const log = []; const [a = (log.push("default a"), 1), b = (log.push("default b"), 2)] = logIter(log, [undefined, 5, 6]); return [log, a, b]; });
t("throw-default", () => { const log = []; try { const [a = (() => { throw new Error("x"); })(), b] = logIter(log, [undefined, 1]); } catch (e) { log.push("threw"); } return log; });
t("param-defaults", () => { const log = []; (function ([a = log.push("param default"), b]) {})(logIter(log, [undefined, 1, 2])); return log; });
t("arrow-param-defaults", () => { const log = []; (([a = log.push("arrow default")]) => {})(logIter(log, [undefined])); return log; });
t("nested-defaults", () => { const log = []; function* g() { log.push("g0"); yield [undefined]; log.push("g1"); yield 2; } const [[x = log.push("inner")], y] = g(); return [log, y]; });
t("nested-throw", () => { const log = []; try { const [{ x }] = logIter(log, [null, 1]); } catch (e) { log.push(e.constructor.name); } return log; });
t("holes-step", () => { const log = []; const [, b = 0, , d = 9] = logIter(log, [1, 2, 3]); return [log, b, d]; });
t("rest-after-default", () => { const log = []; function* g() { log.push("g0"); yield undefined; log.push("g1"); yield 2; log.push("g2"); yield 3; } const [a = (log.push("def"), 1), ...r] = g(); return [log, a, r]; });
t("yield-default", () => { function* g() { const [a = yield "y1", b = yield "y2"] = [undefined]; return [a, b]; } const it = g(); const r = [it.next().value, it.next("A").value]; const f = it.next("B"); return r.concat([f.value, f.done]); });
t("return-at-yield", () => { const log = []; function* g() { const [a = yield 1, b] = logIter(log, [undefined, 2]); log.push("after"); } const it = g(); it.next(); it.return(5); return log; });
t("close-throws", () => { const it = { [Symbol.iterator]() { return { next() { return { value: 1, done: false }; }, return() { throw new Error("R"); } }; } }; try { const [a = 2] = it; return a; } catch (e) { return e.message; } });
t("close-non-object", () => { const it = { [Symbol.iterator]() { return { next() { return { value: 1, done: false }; }, return() { return 1; } }; } }; try { const [a = 2] = it; return a; } catch (e) { return e.constructor.name; } });
t("builtin-defaults", () => { const [x = 1, y = 2] = (function (a) { a = 9; return arguments; })(0); const [s = 1, t2] = "\u{1F600}x"; const [[k, v] = [0, 0], e = "none"] = new Map([[1, 2]]); const [p = 0, q = 9] = new Set([1]); const [u = 0, w = 7] = new Uint8Array([5]); return [x, y, s.length, t2, k, v, e, p, q, u, w]; });
t("forof-defaults", () => { const r = []; for (const [a = 1, { x } = { x: 0 }] of [[undefined], [2, { x: 3 }]]) r.push(a + x); for (const [k, { x }] of [["a", { x: 1 }]]) r.push(k + x); return r; });
t("catch-defaults", () => { try { throw [undefined, 2]; } catch ([a = 1, b]) { return [a, b]; } });
t("hot-defaults", () => { let s = 0; const src = [3]; function f([a = 1, b = 2]) { return a + b; } for (let i = 0; i < 20000; i++) { const [a = 1, [b] = [4]] = src; s += a + b + f(src); } return s; });
console.log(out.join("\n"));
"#;

const ITER_EXPECTED: &str = r#"decl-astral "61,d83d+de00,62"
param-astral "61,d83d+de00"
arrow-astral "61,d83d+de00"
let-single ["d83d+de00",true]
rest-astral ["d83d+de00","62"]
forof-destructure ["d83d+de00,78"]
wrapper-destructure [2,"x"]
empty-obj "THROW TypeError"
param-empty-obj "THROW TypeError"
arraylike "THROW TypeError"
fn-no-iter "THROW TypeError"
proxy-arr [1,2]
str-proto-iter ["Z",["Z"],["Z"]]
map-proto-iter ["M",["M"],["M"]]
aip-next-patched ["P1",["P1"],["P1"],["P1"]]
set-subclass [["sub"],["sub"],"sub",["sub"]]
sorted-set [1,2,3]
map-subclass ["subm"]
own-set-iter [["own"],["own"],["own"],"own"]
proto-set-iter [["S"],["S"],"S",true]
proto-str-forof [["strp"],["strp"]]
proto-ta-iter [["T"],["T"]]
setiter-next [[],[]]
aip-next-done [[],0,[true,true],[]]
spread-lone "61,d800,62,dc00,63,d83d+de00"
call-spread-lone "61,d800,62,dc00,63,d83d+de00"
new-spread-lone "61,d800,62"
rest-lone "61,d800,62,dc00"
mapGroupBy-lone ["d800+dc00","dc00"]
agg-lone "61,d800,62"
hot-lone 55296
ta-detached "THROW TypeError"
ta-detached-call "THROW TypeError"
ta-oob "THROW TypeError"
ta-grow [4,4,4,4]
ta-shrink [1]
ta-own-iter [[42],[42]]
ta-hot-detached "TypeError"
ab-detached-max [0,true,0,true,0,4,0]
aip-return [4,1]
iterproto-return 4
aip-return-hot 5000
no-return-normal 23
memo-invalidation ["123ab1a3ab","3abundefineda3ab","123ab1a3ab","G3abGa3ab","123ab1a3ab","12Sab1aSab","123ab1a3ab","TypeError","123ab1a3ab","closed","123ab1a3ab"]
memo-proto-swap [2,2,2,2]
iterable-proto-return [[],1,1]
gen-defaults [["g0","default","g1"],1,2]
iter-defaults [["next0","default a","next1","return"],1,5]
throw-default ["next0","return","threw"]
param-defaults ["next0","param default","next1","return"]
arrow-param-defaults ["next0","arrow default","return"]
nested-defaults [["g0","inner","g1"],2]
nested-throw ["next0","return","TypeError"]
holes-step [["next0","next1","next2","next3"],2,9]
rest-after-default [["g0","def","g1","g2"],1,[2,3]]
yield-default ["y1","y2",["A","B"],true]
return-at-yield ["next0","return"]
close-throws "R"
close-non-object "TypeError"
builtin-defaults [9,2,2,"x",1,2,"none",1,9,5,7]
forof-defaults [1,5,"a1"]
catch-defaults [1,2]
hot-defaults 240000"#;

#[test]
fn audit_20260915_iter_protocol_child() {
    if std::env::var_os("ZIPP_AUDIT_ITER_PROTOCOL_CHILD").is_none() {
        return;
    }
    let out = run_ok(ITER_PROBES);
    assert_eq!(out.len(), 1, "{out:?}");
    let got: Vec<&str> = out[0].lines().collect();
    let want: Vec<&str> = ITER_EXPECTED.lines().collect();
    assert_eq!(got.len(), want.len(), "{got:#?}");
    for (g, w) in got.iter().zip(want.iter()) {
        assert_eq!(g, w);
    }
}

#[test]
fn audit_20260915_iter_protocol_modes() {
    if std::env::var_os("ZIPP_AUDIT_ITER_PROTOCOL_CHILD").is_some() {
        return;
    }
    run_in_modes(
        "audit_20260915_iter_protocol_child",
        "ZIPP_AUDIT_ITER_PROTOCOL_CHILD",
        &[
            ("default", &[]),
            ("interpreter", &[("ZIPP_NOJIT", "1")]),
            ("forced-jit", &[("ZIPP_JIT_THRESHOLD", "1")]),
            ("majors-only", &[("ZIPP_NO_NURSERY", "1")]),
            ("gc-stress", &[("ZIPP_GC_STRESS", "1")]),
            ("nursery-verify", &[("ZIPP_NURSERY_VERIFY", "1")]),
        ],
    );
}
