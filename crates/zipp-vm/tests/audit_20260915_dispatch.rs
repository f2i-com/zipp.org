//! Regression coverage for the 15 September 2026 dispatch/engine audit track.
//!
//! * Int×Int multiplication with a zero product and a negative operand is -0
//!   (`0 * -5`, `x *= -1` with x = 0). The interpreter's `Mul` arm, the
//!   off-frame method inliner and the x86 INT tiers (Tier A, xmm and GPR
//!   region homes, guard-elided and immediate-multiplier forms) all boxed it
//!   as Int 0, so `1/x`, `Object.is`, `Math.atan2` and typed-array bytes saw
//!   +0 — and a hot function changed its answer as it tiered up.
//! * `new Function` and its generator/async kin must parse the body on its
//!   own: a body that closes the assembled wrapper early (`}); code; (function(){`)
//!   ran `code` during construction and returned a function of the body's
//!   choosing.
//! * The nested-`run_loop` re-entry cap returned its RangeError with the
//!   refused frame still pushed; callers that swallow the error (the async
//!   driver, a Promise executor) then re-ran their own body from a stale ip
//!   forever. And the JIT's native array-callback lane never counted against
//!   the cap at all, so recursion through `forEach`/`map`/`filter`/`reduce`/
//!   `sort` overflowed the native stack and aborted the process.
//! * Dynamic `import()` resolved its specifier against the ENTRY directory
//!   instead of the referencing module's, and linked + evaluated the target
//!   synchronously inside the call instead of in a later job.
//! * Every module binding, eval-introduced global and ShadowRealm name drew
//!   from one fixed 1024-slot pool, so an ordinary module graph with more
//!   than ~1024 top-level declarations failed to load with an "eval" error.
//! * `Object.prototype.toString`'s "Error" tag came from a structural own
//!   `name`, so `class MyErr extends Error {}` instances tagged "Object" while
//!   a plain `{name:"TypeError"}` tagged "Error"; it now reads the
//!   [[ErrorData]] brand the Error constructors set.
//! * The smaller interpreter/engine/embedding findings: toPrecision ties,
//!   string spread by code point, the Math spread reduction, object rest's
//!   per-key re-read, ToPropertyKey for object method keys, patched
//!   iterator protocols on arrays and the other built-in iterables, named
//!   module/realm slots (TDZ errors, no `"?"` fallback), module errors never
//!   steering `run_module_file`, the heap-limit walk on native entry, and
//!   `steps_used` after a budget renewal.
//!
//! The tier-sensitive programs run in child processes under the default,
//! interpreter-only and compile-everything modes, each on a big-stack thread
//! behind a timeout (the depth regressions HANG rather than fail).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

/// The CLI runs the VM on a 256 MiB thread, which the native profile's 4096
/// nested re-entries fit with optimized frames. Unoptimized test builds carry
/// several times larger interpreter frames, so give them proportionally more
/// (reserved address space; only the touched part is committed).
#[cfg(debug_assertions)]
const STACK_BYTES: usize = 2 * 1024 * 1024 * 1024;
#[cfg(not(debug_assertions))]
const STACK_BYTES: usize = 256 * 1024 * 1024;

/// Run `src` on a big-stack thread behind a timeout, so a hang fails the test
/// instead of wedging the suite.
fn run_guarded(src: &'static str) -> Vec<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .stack_size(STACK_BYTES)
        .spawn(move || {
            let _ = tx.send(run_ok(src));
        })
        .expect("spawn guarded VM thread");
    rx.recv_timeout(Duration::from_secs(240))
        .expect("engine hung or crashed: a depth-limit regression")
}

/// Every place an Int×Int product is narrowed. Expected output is node v24's.
const NEGATIVE_ZERO_MUL: &str = r#"
  var lines = [];
  function inv(x) { return 1 / x; }
  function mul(a, b) { return a * b; }
  // Interpreter / cold call sites and literals.
  var z = 0, m1 = -1, q = -3, y = -5;
  y *= 0;
  var t = 0; t *= -3;
  lines.push([inv(z * m1), inv(m1 * z), inv(mul(-5, 0)), inv(mul(0, -5)), inv(mul(-5, -0)),
    inv(mul(5, 0)), inv(-7 * 0), inv(0 * -5), inv(q * 0), inv(y), inv(t),
    Math.atan2(z * m1, -1), Object.is(Math.sign(z * m1), -0), Object.is(-2 * 0, -0),
    new Uint8Array(Float64Array.of(mul(-3, 0)).buffer)[7], inv(-2.5 * 0), inv(3 * 4 - 12)].join(","));
  // A hot function: interpreted first, then compiled.
  var hot = 0;
  for (var i = 0; i < 30000; i++) if (Object.is(mul(-(i % 7) - 1, 0), -0)) hot++;
  var hot2 = 0;
  function negate(k) { return k * -1; }
  for (var i = 0; i < 30000; i++) if (Object.is(negate(0), -0)) hot2++;
  lines.push("hot=" + hot + "," + hot2);
  // Method-inlined `this.x * this.y`.
  class P { constructor(x, y) { this.x = x; this.y = y; } m() { return this.x * this.y; } }
  var ps = [new P(0, -5), new P(2, 3), new P(-0, 4), new P(7, 0), new P(-7, 0)], neg = 0, pos = 0;
  for (var k = 0; k < 20000; k++) for (var j = 0; j < ps.length; j++) {
    var r = ps[j].m(); if (r === 0) { if (Object.is(r, -0)) neg++; else pos++; }
  }
  lines.push("method=" + neg + ":" + pos);
  // Loop regions: xmm INT homes, GPR homes (a bitwise op engages them),
  // immediate multipliers of both signs, range-proven (guard-elided) products.
  function intLoop(a, b) { var r = 1; for (var i = 0; i < 20000; i++) { r = a * b; } return 1 / r; }
  function gprLoop(a, b) { var h = 0, r = 1; for (var i = 0; i < 20000; i++) { h = (h ^ i) | 0; r = a * b; } return 1 / r; }
  function gprImmNeg(a) { var h = 0, r = 1; for (var i = 0; i < 20000; i++) { h = (h ^ i) | 0; r = a * -3; } return 1 / r; }
  function gprImmPos(a) { var h = 0, r = 1; for (var i = 0; i < 20000; i++) { h = (h ^ i) | 0; r = a * 3; } return 1 / r; }
  function elideXmm() { var r = 1, s = 0; for (var i = 0; i < 1000; i++) { r = (i - 999) * (i - 1000); s = s + r; } return 1 / r; }
  function elideGpr() { var r = 1, h = 0; for (var i = 0; i < 1000; i++) { h = (h ^ i) | 0; r = (i - 999) * (i - 1000); } return 1 / r; }
  function elideImmNeg() { var r = 1, h = 0; for (var i = 0; i < 1000; i++) { h = (h ^ i) | 0; r = (i - 999) * -4; } return 1 / r; }
  function elideNoNeg() { var r = 1, h = 0; for (var i = 0; i < 1000; i++) { h = (h ^ i) | 0; r = (999 - i) * (i + 1); } return 1 / r; }
  function mixed(n) { var h = 0, neg = 0, pos = 0; for (var i = 0; i < n; i++) { h = (h ^ i) | 0; var r = (i % 3) * (i % 2 ? -2 : 2); if (r === 0) { if (1 / r < 0) neg++; else pos++; } } return neg + ":" + pos; }
  var regions = [];
  for (var rep = 0; rep < 20; rep++) {
    regions = [intLoop(0, -1), intLoop(-4, 0), intLoop(3, 0), gprLoop(0, -1), gprLoop(5, 0),
      gprImmNeg(0), gprImmPos(0), elideXmm(), elideGpr(), elideImmNeg(), elideNoNeg(), mixed(3000)];
  }
  lines.push("regions=" + regions.join(","));
  console.log(lines.join(";"));
"#;

const NEGATIVE_ZERO_EXPECTED: &str = "-Infinity,-Infinity,-Infinity,-Infinity,Infinity,Infinity,\
-Infinity,-Infinity,-Infinity,-Infinity,-Infinity,-3.141592653589793,true,true,128,-Infinity,Infinity;\
hot=30000,30000;\
method=60000:20000;\
regions=-Infinity,-Infinity,Infinity,-Infinity,Infinity,-Infinity,Infinity,-Infinity,-Infinity,-Infinity,Infinity,500:500";

/// Recursion that crosses the nested re-entry cap along every path that used
/// to lose track of it. Each recursion carries its own valve so a regression
/// reports a count instead of spinning. The lines are cap-agnostic: they hold
/// for the native profile's 4096 and the hardened profile's 32.
const DEPTH_LIMITS: &str = r#"
  var lines = [];
  function dups(m) { var n = 0; m.forEach(function (c) { if (c > 1) n++; }); return n; }
  function bump(m, d) { m.set(d, (m.get(d) || 0) + 1); }
  // 1. Fire-and-forget async recursion: no body may run twice.
  var fireTotal = 0, fireSeen = new Map();
  async function walk(d) { fireTotal++; bump(fireSeen, d); if (fireTotal > 50000) return; if (d < 6000) walk(d + 1); }
  walk(0).catch(function () {});
  lines.push("fire:" + (fireTotal <= 6001) + ":" + dups(fireSeen));
  // 2. Awaited async recursion settles (completion, or a RangeError at the cap).
  var awTotal = 0, awSeen = new Map();
  async function aw(d) { awTotal++; bump(awSeen, d); if (awTotal > 50000) return; if (d < 6000) await aw(d + 1); }
  var awPromise = aw(0).then(function () { return "settled"; }, function (e) { return e instanceof RangeError ? "settled" : "other:" + e; });
  // 3. Recursion through a Promise executor.
  var exTotal = 0, exSeen = new Map(), exReason = "none";
  function ex(d) {
    exTotal++; bump(exSeen, d); if (exTotal > 50000) return;
    new Promise(function (res) { if (d < 6000) ex(d + 1); res(); }).catch(function (e) {
      if (exReason === "none") exReason = e instanceof RangeError ? "RangeError" : "other:" + e;
    });
  }
  ex(0);
  lines.push("executor:" + (exTotal <= 6001) + ":" + dups(exSeen));
  // 4. A caught RangeError leaves every caller frame usable.
  var syncDepth = 0;
  function sync(d) { syncDepth = d; if (d > 50000) return; try { [0].forEach(function () { sync(d + 1); }); } catch (e) { if (!(e instanceof RangeError)) throw e; } }
  sync(0);
  lines.push("sync-after:" + (syncDepth > 0));
  // 5. Runaway recursion through array-builtin callbacks (the JIT's native
  //    callback lane): a catchable RangeError, never a native stack overflow.
  ["forEach", "map", "filter", "reduce", "some"].forEach(function (name) {
    var depth = 0;
    function g() { depth++; if (depth > 200000) return 0; [1, 2][name](g, 0); return 0; }
    var r;
    try { g(); r = "noerr"; } catch (e) { r = e instanceof RangeError ? "RangeError" : "other"; }
    lines.push(name + ":" + r);
  });
  (function () {
    var depth = 0;
    function cmp(a, b) { depth++; if (depth > 200000) return 0; [2, 1].sort(cmp); return 0; }
    var r;
    try { cmp(); r = "noerr"; } catch (e) { r = e instanceof RangeError ? "RangeError" : "other"; }
    lines.push("sort:" + r);
  })();
  (function () {
    var seen = 0;
    function visit(n) { seen++; if (seen > 200000) return; n.children.forEach(visit); }
    var a = { children: [] }, b = { children: [a] }; a.children.push(b);
    var r;
    try { visit(a); r = "noerr"; } catch (e) { r = e instanceof RangeError ? "RangeError" : "other"; }
    lines.push("cycle:" + r);
  })();
  (function () {
    var d = 0;
    function term() { if (++d < 40000) [1].forEach(term); }
    var r;
    try { term(); r = "noerr"; } catch (e) { r = e instanceof RangeError ? "RangeError" : "other"; }
    lines.push("deep-terminating:" + r);
  })();
  awPromise.then(function (how) {
    lines.push("await:" + how + ":" + (awTotal <= 6001) + ":" + dups(awSeen));
    lines.push("executor-reason:" + exReason);
    console.log(lines.join(";"));
  });
"#;

const DEPTH_EXPECTED: &str = "fire:true:0;executor:true:0;sync-after:true;\
forEach:RangeError;map:RangeError;filter:RangeError;reduce:RangeError;some:RangeError;\
sort:RangeError;cycle:RangeError;deep-terminating:RangeError;\
await:settled:true:0;executor-reason:RangeError";

/// The medium/low interpreter findings, run in every tier (spread and
/// destructuring have JIT fast paths): toPrecision's tie rule, string spread by
/// code point, the variadic Math spread reduction, object rest's per-key
/// descriptor re-read, ToPropertyKey for an object method key, a patched
/// %ArrayIteratorPrototype%.next, and overridden @@iterator on the other
/// built-in iterables. Expected output is node v24's.
const ITERATION_AND_COERCION: &str = r#"
  var lines = [];
  // Number.prototype.toPrecision picks the larger candidate on an exact tie.
  lines.push([(2.5).toPrecision(1), (94.5).toPrecision(2), (0.125).toPrecision(2), (1.25).toPrecision(2),
    (25).toPrecision(1), (332500).toPrecision(3), (-40332438149.515625).toPrecision(16),
    (-6270.625).toPrecision(6), (9.99).toPrecision(2), (0.0000001).toPrecision(2), (123.456).toPrecision(3),
    (99.95).toPrecision(3), (3.5).toPrecision(1), (0.000001).toPrecision(2)].join(" "));
  // Spreading a string steps by code point and keeps lone surrogates.
  function codes(a) { return a.map(function (c) { return c.charCodeAt(0); }).join("."); }
  function spreadArgs() { return Array.prototype.slice.call(arguments); }
  var lone = "a\uD800b", rope = "x\uDC00" + "y\uD83D";
  lines.push([codes([...lone]), codes(spreadArgs(...lone)), codes([...rope]), [..."a\uD83D\uDE00b"].length].join(" "));
  // Math spread: -0 < +0, a ±Infinity hypot argument beats NaN, no overflow.
  lines.push([Math.hypot(...[NaN, Infinity]), Object.is(Math.max(...[-0, 0]), 0), Object.is(Math.min(...[0, -0]), -0),
    Math.hypot(...[1e200, 1e200]), Math.hypot(1e-200, 1e-200), Math.hypot(3, 4), Math.hypot(...[1, 2, 3])].join(" "));
  // Object rest re-reads each key's descriptor when it copies it.
  var src = { get a() { delete this.b; Object.defineProperty(this, "c", { enumerable: false }); return 1; }, b: 2, c: 3, d: 4 };
  var { z, ...rest } = src;
  var k = "z", src2 = { get a() { delete this.b; return 1; }, b: 2, c: 3 };
  var { [k]: z2, ...rest2 } = src2;
  lines.push(Object.keys(rest).join(",") + " " + Object.keys(rest2).join(","));
  // A computed method key that is an object goes through ToPropertyKey.
  var calls = 0, w = new String("push"); w.toString = function () { calls++; return "pop"; };
  var arr1 = [5, 6], popped = arr1[w](7);
  lines.push(calls + " " + popped + " " + arr1.join(","));
  // A patched %ArrayIteratorPrototype%.next is honoured by every array walk.
  var AIP = Object.getPrototypeOf([][Symbol.iterator]()), origNext = AIP.next;
  AIP.next = function () { var r = origNext.call(this); if (!r.done) r.value *= 10; return r; };
  var arr = [1, 2, 3];
  var [p1, p2] = arr; var [h, ...tail] = arr;
  function param([x, y]) { return x + "|" + y; }
  lines.push([[...arr].join(","), Math.max(...arr), spreadArgs(...arr).join(","), p1, p2, h, tail.join(","), param(arr)].join(" "));
  AIP.next = origNext;
  // Overridden @@iterator on Set / Map / String / TypedArray is honoured.
  class S extends Set { *[Symbol.iterator]() { yield "sub"; } }
  class M extends Map { *[Symbol.iterator]() { yield "msub"; } }
  class U extends Uint8Array { *[Symbol.iterator]() { yield "usub"; } }
  var viaForOf = []; for (var v of new S([1])) viaForOf.push(v);
  var [firstOfMap] = new M([[1, 1]]);
  var own = new Set([1]); own[Symbol.iterator] = function* () { yield "own"; };
  var strIter = String.prototype[Symbol.iterator];
  String.prototype[Symbol.iterator] = function* () { yield "str"; };
  var strSpread = [..."ab"].join(",");
  String.prototype[Symbol.iterator] = strIter;
  var broken = new Set([1]); broken[Symbol.iterator] = undefined, brokenResult = "iterated";
  try { [...broken]; } catch (e) { brokenResult = e.constructor.name; }
  lines.push([viaForOf.join(","), [...new S([1])].join(","), firstOfMap, spreadArgs(...new M()).join(","),
    [...new U([7])].join(","), [...own].join(","), strSpread, brokenResult,
    [...new Set([1, 2])].join(","), JSON.stringify([...new Map([[1, 2]])]), [..."ab"].join(",")].join(" "));
  // GetIterator reads @@iterator exactly once for every kind and consumer
  // (spread, for-of, destructuring) — whether the getter hands back the
  // pristine method or a replacement; a non-callable one is a TypeError.
  var counts = [];
  [[Array.prototype, function () { return [1, 2]; }],
   [String.prototype, function () { return "ab"; }],
   [Set.prototype, function () { return new Set([1, 2]); }],
   [Map.prototype, function () { return new Map([[1, 2]]); }],
   [Object.getPrototypeOf(Uint8Array.prototype), function () { return new Uint8Array(2); }]].forEach(function (pair) {
    var proto = pair[0], make = pair[1];
    var desc = Object.getOwnPropertyDescriptor(proto, Symbol.iterator);
    [desc.value, function () { return desc.value.call(this); }].forEach(function (method) {
      var gets = 0;
      Object.defineProperty(proto, Symbol.iterator, { configurable: true, get: function () { gets++; return method; } });
      var n = [...make()].length; for (var x of make()) {} var [first] = make();
      Object.defineProperty(proto, Symbol.iterator, desc);
      counts.push(gets + ":" + n);
    });
  });
  var arrIter = Array.prototype[Symbol.iterator], deleted = [];
  Array.prototype[Symbol.iterator] = undefined;
  try { [...[1]]; deleted.push("spread"); } catch (e) { deleted.push(e.constructor.name); }
  try { for (var y of [1]) {} deleted.push("for-of"); } catch (e) { deleted.push(e.constructor.name); }
  Array.prototype[Symbol.iterator] = arrIter;
  lines.push(counts.join(",") + " " + deleted.join(","));
  console.log(lines.join(";"));
"#;

const ITERATION_EXPECTED: &str = "3 95 0.13 1.3 3e+1 3.33e+5 -40332438149.51563 -6270.63 10 1.0e-7 123 100 4 0.0000010;\
97.55296.98 97.55296.98 120.56320.121.55357 3;\
Infinity true true 1.414213562373095e+200 1.414213562373095e-200 5 3.741657386773941;\
a,d a,c;\
1 6 5;\
10,20,30 30 10,20,30 10 20 10 20,30 10|20;\
sub sub msub msub usub own str TypeError 1,2 [[1,2]] a,b;\
3:2,3:2,3:2,3:2,3:2,3:2,3:1,3:1,3:2,3:2 TypeError,TypeError";

#[test]
fn audit_20260915_dispatch_tier_child() {
    if std::env::var_os("ZIPP_AUDIT_DISPATCH_CHILD").is_none() {
        return;
    }
    assert_eq!(run_guarded(NEGATIVE_ZERO_MUL), [NEGATIVE_ZERO_EXPECTED]);
    assert_eq!(run_guarded(DEPTH_LIMITS), [DEPTH_EXPECTED]);
    assert_eq!(run_guarded(ITERATION_AND_COERCION), [ITERATION_EXPECTED]);
}

#[test]
fn audit_20260915_dispatch_tiers_match() {
    if std::env::var_os("ZIPP_AUDIT_DISPATCH_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("default", None),
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("forced-jit", Some(("ZIPP_JIT_THRESHOLD", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "audit_20260915_dispatch_tier_child", "--nocapture"])
            // An inherited setting must not decide what a mode tests.
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env("ZIPP_AUDIT_DISPATCH_CHILD", "1");
        if let Some((key, value)) = env {
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

/// CreateDynamicFunction parses the body as a standalone FunctionBody: a body
/// that closes the wrapper is a SyntaxError for all four constructors, and
/// nothing in it runs.
#[test]
fn audit_20260915_dynamic_function_body_standalone() {
    let out = run_ok(
        r#"
        var GF = Object.getPrototypeOf(function*(){}).constructor;
        var AF = Object.getPrototypeOf(async function(){}).constructor;
        var AGF = Object.getPrototypeOf(async function*(){}).constructor;
        var lines = [];
        function t(label, flag, make) {
          var r;
          try { var f = make(); r = "ok:" + typeof f + ":" + f.length; }
          catch (e) { r = e instanceof SyntaxError ? "SyntaxError" : "other:" + e; }
          lines.push(label + "=" + r + ":" + typeof globalThis[flag]);
        }
        t("Function", "p1", function () { return new Function("a", "}); globalThis.p1 = 1; (function(){"); });
        t("GeneratorFunction", "p2", function () { return new GF("a", "}); globalThis.p2 = 1; (function*(){"); });
        t("AsyncFunction", "p3", function () { return new AF("a", "}); globalThis.p3 = 1; (async function(){"); });
        t("AsyncGeneratorFunction", "p4", function () { return new AGF("a", "}); globalThis.p4 = 1; (async function*(){"); });
        t("body-only", "p5", function () { return Function("}); globalThis.p5 = 1; (function(){"); });
        t("call-shape", "p6", function () { return new Function("})(globalThis.p6 = 1, function(){"); });
        t("sequence-shape", "p7", function () { return new Function("}, globalThis.p7 = 1, function(){"); });
        function isValidBody(src) { try { new Function(src); return true; } catch (e) { return false; } }
        lines.push("validator=" + isValidBody("}); globalThis.p8 = 'ran'; (function(){") + ":" + typeof globalThis.p8);
        // The standalone parse uses THIS constructor's [Yield]/[Await]
        // parameters (and a FunctionBody's top-level `return` / `new.target`),
        // so it neither rejects a legal body nor accepts an illegal one.
        function ctx(label, make) {
          try { make(); lines.push(label + "=ok"); }
          catch (e) { lines.push(label + "=" + (e instanceof SyntaxError ? "SyntaxError" : "other:" + e)); }
        }
        ctx("fn-await-ident", function () { return new Function("var await = 1; return await"); });
        ctx("fn-yield-ident", function () { return new Function("var yield = 1; return yield"); });
        ctx("fn-await-expr", function () { return new Function("return await 1"); });
        ctx("fn-yield-expr", function () { return new Function("yield 1"); });
        ctx("gen-yield-expr", function () { return new GF("yield 1"); });
        ctx("gen-yield-ident", function () { return new GF("var yield = 1"); });
        ctx("gen-await-ident", function () { return new GF("var await = 1"); });
        ctx("async-await-expr", function () { return new AF("return await 1"); });
        ctx("async-await-ident", function () { return new AF("var await = 1"); });
        ctx("async-yield-ident", function () { return new AF("var yield = 1"); });
        ctx("asyncgen-both", function () { return new AGF("yield await 1"); });
        ctx("asyncgen-await-ident", function () { return new AGF("var await = 1"); });
        ctx("new-target", function () { return new Function("return new.target"); });
        ctx("super", function () { return new Function("return super.x"); });
        // Controls: ordinary bodies keep working.
        t("comment-tail", "x", function () { return new Function("a", "return 1 //"); });
        t("empty", "x", function () { return new Function(); });
        t("stray-brace", "x", function () { return new Function("}"); });
        t("strict", "x", function () { return new Function("a,b", "'use strict'; return a + b"); });
        lines.push("results=" + new Function("a", "b", "return a * b")(6, 7) + ":" +
          [...new GF("yield 1; yield 2")()].join("|") + ":" +
          new Function("function g(){ return { v: 3 }; } return g().v")());
        console.log(lines.join(";"));
        "#,
    );
    assert_eq!(
        out,
        ["Function=SyntaxError:undefined;GeneratorFunction=SyntaxError:undefined;\
AsyncFunction=SyntaxError:undefined;AsyncGeneratorFunction=SyntaxError:undefined;\
body-only=SyntaxError:undefined;call-shape=SyntaxError:undefined;sequence-shape=SyntaxError:undefined;\
validator=false:undefined;\
fn-await-ident=ok;fn-yield-ident=ok;fn-await-expr=SyntaxError;fn-yield-expr=SyntaxError;\
gen-yield-expr=ok;gen-yield-ident=SyntaxError;gen-await-ident=ok;\
async-await-expr=ok;async-await-ident=SyntaxError;async-yield-ident=ok;\
asyncgen-both=ok;asyncgen-await-ident=SyntaxError;new-target=ok;super=SyntaxError;\
comment-tail=ok:function:1:undefined;empty=ok:function:0:undefined;\
stray-brace=SyntaxError:undefined;strict=ok:function:2:undefined;results=42:1|2:3"]
    );
}

/// Object.prototype.toString tags "Error" from the [[ErrorData]] brand the
/// Error constructors set, not from a structural own `name`: subclass
/// instances and renamed errors ARE errors, plain `{name:"TypeError"}` and
/// `Object.create(Error.prototype)` are not.
#[test]
fn audit_20260915_error_tag_uses_the_error_brand() {
    let out = run_ok(
        r#"
        var tag = Object.prototype.toString;
        function t(v) { return tag.call(v); }
        class MyErr extends Error {}
        class AppError extends Error { constructor(m) { super(m); this.name = "AppError"; } }
        var renamed = new Error("x"); renamed.name = "Custom";
        var nameless = new RangeError("x"); delete nameless.name;
        var caught; try { null.x; } catch (e) { caught = e; }
        var deep; try { (function r() { return r(); })(); } catch (e) { deep = e; }
        console.log([
          t(new MyErr("boom")), t(new AppError("boom")), t(renamed), t(nameless),
          t(new TypeError("q")), t(new AggregateError([], "q")), t(caught), t(deep),
          t({ name: "TypeError", message: "x" }), t(JSON.parse('{"name":"RangeError"}')),
          t(Object.create(Error.prototype)), t(Error.prototype), t(TypeError.prototype),
          Error.isError(new MyErr()), Error.isError({ name: "TypeError" })
        ].join(" "));
        "#,
    );
    assert_eq!(
        out,
        ["[object Error] [object Error] [object Error] [object Error] \
[object Error] [object Error] [object Error] [object Error] \
[object Object] [object Object] [object Object] [object Object] [object Object] true false"]
    );
}

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

/// A throwaway module tree under the temp directory.
struct Fixture(PathBuf);

impl Fixture {
    fn new(name: &str, files: &[(&str, String)]) -> Self {
        let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "zipp-audit-dispatch-{name}-{}-{id}",
            std::process::id()
        ));
        for (file, src) in files {
            let path = dir.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).expect("create fixture directory");
            std::fs::write(path, src).expect("write fixture");
        }
        Self(dir)
    }

    /// Run `entry` as an ES-module entry through the loader.
    fn module(&self, entry: &str) -> Vec<String> {
        let out = zipp_vm::run_module_file(&self.0.join(entry), None).expect("module compiles");
        assert!(out.error.is_none(), "unexpected error: {:?}", out.error);
        out.output
    }

    /// Run `src` as a classic script whose dynamic imports resolve against
    /// the fixture directory.
    fn script(&self, src: &str) -> Vec<String> {
        let out = zipp_vm::run_with_base(src, Some(self.0.clone())).expect("script compiles");
        assert!(out.error.is_none(), "unexpected error: {:?}", out.error);
        out.output
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `import()` resolves against the module that contains it — from top-level
/// await, a function, direct eval, `new Function` and a timer callback alike —
/// never against the entry's directory. A same-named file in the root must not
/// be picked, and a file that exists only next to the referrer must load.
#[test]
fn audit_20260915_dynamic_import_resolves_against_referrer() {
    let fx = Fixture::new(
        "referrer",
        &[
            ("x.mjs", "export const where = 'root';".into()),
            ("sub/x.mjs", "export const where = 'sub';".into()),
            ("sub/onlysub.mjs", "export const v = 42;".into()),
            (
                "sub/m.mjs",
                r#"
                import { where as s } from './x.mjs';
                const d = (await import('./x.mjs')).where;
                function f() { return import('./x.mjs'); }
                const e = (await f()).where;
                const ev = (await eval("import('./x.mjs')")).where;
                const nf = (await new Function("return import('./x.mjs')")()).where;
                const only = (await import('./onlysub.mjs')).v;
                console.log([s, d, e, ev, nf, only].join(','));
                setTimeout(() => import('./x.mjs').then(m => console.log('timer:' + m.where)), 0);
                "#
                .into(),
            ),
            ("entry.mjs", "import './sub/m.mjs';".into()),
            ("script-dep.mjs", "export const where = 'script-dir';".into()),
        ],
    );
    assert_eq!(fx.module("entry.mjs"), ["sub,sub,sub,sub,sub,42", "timer:sub"]);
    // A classic script keeps resolving against its own (the base) directory.
    assert_eq!(
        fx.script("import('./script-dep.mjs').then(m => console.log(m.where));"),
        ["script-dir"]
    );
}

/// ContinueDynamicImport links and evaluates in a later job: the target's body
/// runs after the rest of the calling code AND after jobs queued before the
/// `import()` call — from a script and from a module entry.
#[test]
fn audit_20260915_dynamic_import_evaluates_in_a_later_job() {
    let order = r#"
        Promise.resolve().then(() => console.log('tick queued before import()'));
        const p = import('./side.mjs');
        console.log('after import() call');
        p.then(m => console.log('settled ' + m.v));
    "#;
    let fx = Fixture::new(
        "order",
        &[
            ("side.mjs", "console.log('side body'); export const v = 1;".into()),
            ("entry.mjs", order.into()),
        ],
    );
    let expected = [
        "after import() call",
        "tick queued before import()",
        "side body",
        "settled 1",
    ];
    assert_eq!(fx.script(order), expected);
    assert_eq!(fx.module("entry.mjs"), expected);
}

/// Module bindings, eval-introduced globals and `new Function` globals no
/// longer share a 1024-slot pool with a misleading "eval" error: large module
/// graphs load, many eval'd names work, and retrying a module whose link fails
/// neither changes its error nor starves an unrelated later eval. (The
/// hardened profile keeps its small committed pool; see `EVAL_POOL`.)
#[cfg(not(feature = "safe-sandbox"))]
#[test]
fn audit_20260915_module_and_eval_global_pool() {
    let exports: String = (0..1100).map(|i| format!("export const v{i} = {i};\n")).collect();
    let locals: String = (0..3000).map(|i| format!("const c{i} = {i};\n")).collect();
    let broken = format!(
        "import {{ nope }} from './good.mjs';\n{}",
        (0..10).map(|i| format!("export const b{i} = {i};\n")).collect::<String>()
    );
    let mut files: Vec<(&str, String)> = vec![
        ("big.mjs", exports),
        (
            "entry-big.mjs",
            "import { v0, v1099 } from './big.mjs'; console.log(v0 + ',' + v1099);".into(),
        ),
        ("entry-locals.mjs", format!("{locals}console.log(c0 + ',' + c2999);")),
        ("good.mjs", "export const a = 1;".into()),
        ("broken.mjs", broken),
        (
            "entry-retry.mjs",
            r#"
            let first, last;
            for (let i = 0; i < 150; i++) {
              try { await import('./broken.mjs'); }
              catch (e) { if (i === 0) first = e.name; last = e.name; }
            }
            (0, eval)('var totallyFresh = 7');
            console.log(first + ',' + last + ',' + totallyFresh);
            "#
            .into(),
        ),
    ];
    let names: Vec<String> = (0..12).map(|m| format!("mod{m}.mjs")).collect();
    let mut agg = String::new();
    for (m, name) in names.iter().enumerate() {
        let body: String = (0..100)
            .map(|i| format!("export function f{i}() {{ return {}; }}\n", m * 1000 + i))
            .collect();
        files.push((name.as_str(), body));
        agg.push_str(&format!("import * as m{m} from './{name}';\n"));
    }
    agg.push_str("console.log(m0.f0() + m11.f99());");
    files.push(("entry-agg.mjs", agg));
    let fx = Fixture::new("pool", &files);
    assert_eq!(fx.module("entry-big.mjs"), ["0,1099"]);
    assert_eq!(fx.module("entry-locals.mjs"), ["0,2999"]);
    assert_eq!(fx.module("entry-agg.mjs"), ["11099"]);
    assert_eq!(fx.module("entry-retry.mjs"), ["SyntaxError,SyntaxError,7"]);
    assert_eq!(
        run_ok(
            r#"
            var ok = 0;
            for (var i = 0; i < 1500; i++) { (0, eval)('var g' + i + ' = ' + i); ok++; }
            for (var j = 0; j < 1500; j++) { new Function('h' + j + ' = ' + j)(); ok++; }
            console.log(ok + ',' + g1499 + ',' + h1499);
            "#
        ),
        ["3000,1499,1499"]
    );
}

/// A module declaration read, `typeof`'d or written in its TDZ names itself and
/// never falls back to the global object (the slot had no registered name, so
/// the fallback probed a property literally called "?" — a TDZ bypass through
/// `Object.prototype["?"]`); a ShadowRealm's undeclared name does not leak the
/// incubating realm's `globalThis["?"]` either.
#[test]
fn audit_20260915_module_and_realm_slots_are_named() {
    let fx = Fixture::new(
        "tdz",
        &[
            (
                "dep.mjs",
                r#"
                Object.prototype['?'] = 'LEAK';
                export function read() { return y; }
                const out = [];
                try { out.push(read()); } catch (e) { out.push(e.name + ':' + e.message); }
                try { out.push(typeof y); } catch (e) { out.push(e.name + ':' + e.message); }
                try { y = 5; out.push('wrote'); } catch (e) { out.push(e.name + ':' + e.message); }
                delete Object.prototype['?'];
                console.log(out.join('|'));
                export let y = 2;
                console.log('after ' + y);
                "#
                .into(),
            ),
            ("entry.mjs", "import { y } from './dep.mjs'; console.log('entry ' + y);".into()),
        ],
    );
    let tdz = "ReferenceError:Cannot access 'y' before initialization";
    assert_eq!(
        fx.module("entry.mjs"),
        [format!("{tdz}|{tdz}|{tdz}"), "after 2".into(), "entry 2".into()]
    );
    assert_eq!(
        run_ok(
            r#"
            globalThis['?'] = 'OUTER';
            var realm = new ShadowRealm();
            console.log(realm.evaluate(
              "(function () { try { return String(notDeclared); } catch (e) { return e.name; } })()"));
            console.log(realm.evaluate("var q = 5; q + 1") + ' ' + realm.evaluate("typeof zzz"));
            "#
        ),
        ["ReferenceError", "6 undefined"]
    );
}

/// `run_module_file` no longer picks a re-execution path by matching the
/// guest's error text: a module that throws "top-level await is not
/// supported" reports that error after its dependencies and its own body ran
/// exactly once — it used to be re-run on a fresh VM without its imports.
#[test]
fn audit_20260915_module_error_text_is_not_a_control_signal() {
    let fx = Fixture::new(
        "tla-text",
        &[
            ("dep.mjs", "console.log('dep evaluated'); export const x = 42;".into()),
            (
                "main.mjs",
                r#"
                import { x } from './dep.mjs';
                console.log('main body, x = ' + x);
                throw new Error('top-level await is not supported');
                "#
                .into(),
            ),
            (
                "tla.mjs",
                "import { x } from './dep.mjs'; const v = await Promise.resolve(x); console.log('tla ' + v);"
                    .into(),
            ),
        ],
    );
    let out = zipp_vm::run_module_file(&fx.0.join("main.mjs"), None).expect("module compiles");
    assert_eq!(out.output, ["dep evaluated", "main body, x = 42"]);
    let err = out.error.expect("the guest's throw is the program error");
    assert!(err.contains("Error: top-level await is not supported"), "{err}");
    // Genuine top-level await keeps working through the loader.
    assert_eq!(fx.module("tla.mjs"), ["dep evaluated", "tla 42"]);
}

/// With a heap limit set and the JIT on, every interpreter-to-compiled entry
/// lends native steps (`meter_lend`), and the lend used to run the O(heap
/// slots) audit walk each time — a tax proportional to everything the script
/// holds on every call. The check now escalates and confirms like the
/// interpreter's poll, so many entries over a large live heap cost about what
/// they cost without a limit.
#[cfg(feature = "instrument")]
#[test]
fn audit_20260915_heap_limit_native_entry_skips_the_heap_walk() {
    use std::time::Instant;
    use zipp_vm::embed::{self, JsValue};
    let src = "var keep = []; for (var i = 0; i < 200000; i++) keep.push({ i: i }); \
               function work(n) { var s = 0; for (var i = 0; i < n; i++) s = (s + i) | 0; return s; }";
    let time_calls = |limit: bool| -> Duration {
        let mut st = embed::compile_script(src).expect("compiles");
        st.run_init().expect("runs");
        // Both runs carry a recorder (native code then lends metered steps on
        // every entry); only the heap ceiling differs.
        st.set_limits(u64::MAX / 4, None);
        if limit {
            // Finite, far above what the script holds: only the check's cost
            // is measured.
            st.set_heap_limit(usize::MAX / 2);
        }
        // Warm up: `work` gets compiled, and every later call enters native
        // code (the function, or its loop region) from the interpreter.
        for _ in 0..2_000 {
            st.call_global("work", &[JsValue::Number(64.0)]).expect("calls");
        }
        let start = Instant::now();
        for _ in 0..4_000 {
            assert_eq!(
                st.call_global("work", &[JsValue::Number(64.0)]).expect("calls"),
                JsValue::Number(2016.0)
            );
        }
        let elapsed = start.elapsed();
        eprintln!("heap limit {limit}: {elapsed:?}");
        elapsed
    };
    let unlimited = time_calls(false);
    let limited = time_calls(true);
    // Before the fix the limited run walked 200,000 slots per call (two
    // orders of magnitude slower); a generous bound keeps this robust.
    assert!(
        limited < unlimited * 8 + Duration::from_millis(250),
        "heap-limited entries took {limited:?} against {unlimited:?} unlimited"
    );
}

/// `Array.fromAsync`, `%AsyncIteratorPrototype%[@@asyncDispose]` and the
/// sync-dispose shim are JS polyfills; they reached `Function.prototype.call`,
/// `Object.defineProperty`, `Math` and the global `Object` / `Array` /
/// `TypeError` bindings, so user code that patched or shadowed any of them
/// changed or broke the builtins. They now run on captured intrinsics, and
/// fromAsync reads an iterator's `next` once. Expected output is node v24's.
#[test]
fn audit_20260915_polyfills_use_captured_intrinsics() {
    let out = run_ok(
        r#"
        var out = [];
        var fromAsync = Array.fromAsync, R = Reflect.apply, origCall = Function.prototype.call;
        var calls = 0;
        (async function () {
          Function.prototype.call = function (t, ...rest) { calls++; return R(this, t, rest); };
          var mapped = await fromAsync([1, 2, 3], function (x) { return x * 2; });
          var ait = (async function* () { yield 1; })();
          await ait[Symbol.asyncDispose]();
          var stack = new AsyncDisposableStack();
          stack.use({ [Symbol.dispose]() {} });
          await stack.disposeAsync();
          Function.prototype.call = origCall;
          out.push(JSON.stringify(mapped) + " calls=" + calls);
          var dp = Object.defineProperty;
          Object.defineProperty = function () { throw new Error("patched defineProperty"); };
          try { out.push(JSON.stringify(await fromAsync([1]))); } catch (e) { out.push(e.message); }
          Object.defineProperty = dp;
          var O = globalThis.Object, TE = globalThis.TypeError, M = globalThis.Math, A = globalThis.Array;
          globalThis.Object = function () { return {}; };
          globalThis.Math = { max: function () { throw new Error("patched Math"); } };
          globalThis.Array = function () { throw new Error("patched Array"); };
          globalThis.TypeError = function () { return { fake: true }; };
          try { out.push(JSON.stringify(await fromAsync([1])) + " " +
              JSON.stringify(await fromAsync.call(undefined, { length: 2.5, 0: "a", 1: "b" }))); }
          catch (e) { out.push("shadowed: " + e.message); }
          var nullItems;
          try { await fromAsync(null); } catch (e) { nullItems = e instanceof TE ? "TypeError" : JSON.stringify(e); }
          globalThis.Object = O; globalThis.Math = M; globalThis.Array = A; globalThis.TypeError = TE;
          out.push("null items -> " + nullItems);
          Object.prototype.get = function () {};
          try { out.push(JSON.stringify(await fromAsync([5]))); } catch (e) { out.push("inherited get: " + e.name); }
          delete Object.prototype.get;
          var reads = 0;
          var iterable = { [Symbol.asyncIterator]: function () {
            var i = 0;
            return { get next() { reads++; return function () { return Promise.resolve({ done: i >= 2, value: i++ }); }; } };
          } };
          out.push(JSON.stringify(await fromAsync(iterable)) + " next reads=" + reads);
          console.log(out.join(" | "));
        })();
        "#,
    );
    assert_eq!(
        out,
        [r#"[2,4,6] calls=0 | [1] | [1] ["a","b"] | null items -> TypeError | [5] | [0,1] next reads=1"#]
    );
}

/// Symbol-keyed properties live under engine-internal `"@@…"` keys, and the
/// host value boundary used to treat those as ordinary string keys in both
/// directions: a guest's Symbol-keyed capability left as `"@@sym:1"` (which
/// the guest's own `JSON.stringify` omits), and host data carrying an
/// `"@@iterator"` / `"@@toPrimitive"` key installed Symbol-keyed properties on
/// the guest object. Reads, digests and writes now skip them, and a write-back
/// keeps the Symbol-keyed properties the host never saw.
#[test]
fn audit_20260915_host_boundary_ignores_symbol_keys() {
    use zipp_vm::embed::{compile_script, HostValue, JsValue};
    let mut st = compile_script(
        r#"
        var cap = Symbol('capability');
        var st = { a: 1 };
        st[cap] = 'secret-token';
        st[Symbol.toStringTag] = 'Tagged';
        var twin = { a: 1 };
        function probe() {
          var s;
          try { s = String(st); } catch (e) { s = e.name; }
          return [st.a, typeof st[Symbol.iterator], s, st[cap],
            JSON.stringify(Object.getOwnPropertySymbols(st).length)].join(',');
        }
        function probeFresh() {
          var s;
          try { s = String(fresh); } catch (e) { s = e.name; }
          return [fresh.b, typeof fresh[Symbol.iterator], s].join(',');
        }
        var fresh = null;
        "#,
    )
    .expect("compiles");
    st.run_init().expect("runs");
    let slot = |st: &zipp_vm::embed::ScriptState, name: &str| {
        st.symbols()
            .into_iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no global named {name}"))
            .index
    };
    let (st_slot, twin_slot, fresh_slot) = (slot(&st, "st"), slot(&st, "twin"), slot(&st, "fresh"));
    // Out: only the data, as the guest's JSON.stringify sees it; the digest
    // agrees with a twin holding the same data and no Symbol keys.
    let one = HostValue::Object(vec![("a".into(), HostValue::Number(1.0))]);
    assert_eq!(st.get_slot(st_slot), one);
    assert_eq!(st.fingerprint_slot(st_slot), st.fingerprint_slot(twin_slot));
    // In (merge): host `@@` keys are ignored, and the Symbol-keyed properties
    // the host never saw survive the write-back.
    let hostile = HostValue::Object(vec![
        ("a".into(), HostValue::Number(2.0)),
        ("@@iterator".into(), HostValue::Number(5.0)),
        ("@@toPrimitive".into(), HostValue::Number(1.0)),
        ("@@sym:1".into(), HostValue::String("forged".into())),
    ]);
    assert!(st.set_slot(st_slot, &hostile));
    assert_eq!(
        st.call_global("probe", &[]).expect("probe runs"),
        JsValue::String("2,undefined,[object Tagged],secret-token,2".into())
    );
    // In (fresh object): the same keys create nothing.
    let fresh = HostValue::Object(vec![
        ("b".into(), HostValue::Number(3.0)),
        ("@@iterator".into(), HostValue::Number(5.0)),
        ("@@toPrimitive".into(), HostValue::Number(1.0)),
    ]);
    assert!(st.set_slot(fresh_slot, &fresh));
    assert_eq!(
        st.call_global("probeFresh", &[]).expect("probe runs"),
        JsValue::String("3,undefined,[object Object]".into())
    );
}

/// `steps_used() + steps_remaining() == max_steps` holds after a renewal too:
/// the native profile's figure now counts under the current ceiling, as the
/// release wasm meter's always did, instead of carrying the lifetime total.
#[cfg(feature = "instrument")]
#[test]
fn audit_20260915_steps_used_counts_under_the_current_budget() {
    use zipp_vm::embed::{self, JsValue};
    let mut st = embed::compile_script(
        "function burn(n) { var s = 0; for (var i = 0; i < n; i++) s = (s + i) | 0; return s; }",
    )
    .expect("compiles");
    st.run_init().expect("runs");
    const BUDGET: u64 = 10_000_000;
    st.set_limits(BUDGET, None);
    for _ in 0..3 {
        st.call_global("burn", &[JsValue::Number(20_000.0)]).expect("burns");
        assert_eq!(st.steps_used() + st.steps_remaining(), BUDGET);
    }
    assert!(st.steps_used() > 0);
    assert!(st.renew_step_budget(BUDGET), "the budget is renewable");
    assert_eq!(st.steps_used(), 0, "a renewal starts the count again");
    assert_eq!(st.steps_remaining(), BUDGET);
    st.call_global("burn", &[JsValue::Number(20_000.0)]).expect("burns");
    assert!(st.steps_used() > 0);
    assert_eq!(st.steps_used() + st.steps_remaining(), BUDGET);
}
