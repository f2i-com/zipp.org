// Regressions from the 15 September 2026 dispatch/engine audit, against the
// artifact — the hardened profile, whose nested run-loop re-entry cap is 32.
//
//   eval ceilings    an eval refused for its per-call size or its lifetime
//                    call allowance is "usage" (and terminal); lastErrorKind()
//                    used to keep whatever the previous call left, so a
//                    guest throw followed by an oversized eval read "guest".
//   re-entry cap     async and Promise-executor recursion past the cap
//                    settles with a RangeError; the refused frame used to stay
//                    pushed, the caller re-ran its own body forever, and the
//                    engine died ~3 s later with a misleading memory error.
//   new Function     a body that closes the assembled wrapper early is a
//                    SyntaxError, and none of it runs.
//   int * int        a zero product with a negative operand is -0.
//   @@ keys          Symbol-keyed properties (engine-internal "@@…" keys)
//                    never cross the host value boundary: reads used to
//                    export them as plain "@@sym:1" entries, and host data
//                    with an "@@iterator" key installed a Symbol.iterator.
"use strict";
const { Engine } = require("./pkg/zipp_wasm.js");

let pass = 0;
function same(label, got, want) {
  const actual = JSON.stringify(got);
  const expected = JSON.stringify(want);
  if (actual !== expected) throw new Error(`${label}: got ${actual}, want ${expected}`);
  pass++;
  console.log(`  ok   ${label}`);
}
function caught(fn) {
  try { fn(); return ""; } catch (x) { return String(x && x.message ? x.message : x); }
}
function close(engine) { try { engine.dispose(); } catch {} try { engine.free(); } catch {} }
function init(source) {
  const engine = new Engine();
  engine.setInstructionBudget(500_000_000);
  engine.initScript(source);
  return engine;
}

// ── eval ceilings are usage, classified where they are raised ──
{
  const e = init("function boom() { throw new Error('x'); }");
  caught(() => e.callFunction("boom", []));
  same("a guest throw is guest", [e.lastErrorKind(), e.disposed], ["guest", false]);
  const big = caught(() => e.evalInContextRich("1+" + " ".repeat(70_000)));
  same("an oversized eval after a guest throw is usage and terminal",
    [/per-call limit/.test(big), e.lastErrorKind(), e.disposed], [true, "usage", true]);
  close(e);
}
{
  const e = init("function boom() { throw new Error('x'); }");
  for (let i = 0; i < 256; i++) e.evalInContextRich("1");
  caught(() => e.callFunction("boom", []));
  const over = caught(() => e.evalInContextRich("1"));
  same("the eval call allowance is usage and terminal",
    [/lifetime limit/.test(over), e.lastErrorKind(), e.disposed], [true, "usage", true]);
  close(e);
}

// ── recursion past the re-entry cap settles instead of spinning ──
{
  const e = init(`
    var out = "pending", runs = 0, seen = {};
    async function g(n) { runs++; seen[n] = (seen[n] || 0) + 1; if (n > 0) await g(n - 1); }
    function start(n) {
      out = "pending"; runs = 0; seen = {};
      g(n).then(function () { out = "resolved"; },
                function (err) { out = err instanceof RangeError ? "RangeError" : "other:" + err; });
    }
    function result() {
      var dup = 0; for (var k in seen) if (seen[k] > 1) dup++;
      return out + ":" + (runs <= 1000) + ":" + dup;
    }
    var exRuns = 0, exReason = "none";
    function ex(n) {
      exRuns++; if (exRuns > 5000) return;
      new Promise(function (res) { if (n > 0) ex(n - 1); res(); }).catch(function (err) {
        if (exReason === "none") exReason = err instanceof RangeError ? "RangeError" : "other:" + err;
      });
    }
    function startEx(n) { exRuns = 0; exReason = "none"; ex(n); return exRuns <= n + 1; }
    function exResult() { return exReason; }
    function alive() { return 42; }
  `);
  e.callFunction("start", [5]);
  same("shallow async recursion resolves", e.callFunction("result", []), "resolved:true:0");
  e.callFunction("start", [200]);
  same("async recursion past the cap rejects with a RangeError, each body once",
    e.callFunction("result", []), "RangeError:true:0");
  same("executor recursion past the cap runs each body once", e.callFunction("startEx", [200]), true);
  same("…and rejects the refused executor's promise with a RangeError", e.callFunction("exResult", []), "RangeError");
  same("the engine is live afterwards", [e.callFunction("alive", []), e.disposed], [42, false]);
  close(e);
}

// ── CreateDynamicFunction parses the body on its own; int*int keeps -0 ──
{
  const e = init(`
    function inject() {
      var r;
      try { new Function("a", "}); globalThis.pwned = 1; (function(){"); r = "constructed"; }
      catch (err) { r = err instanceof SyntaxError ? "SyntaxError" : "other"; }
      return r + ":" + typeof globalThis.pwned;
    }
    function negZero(a, b) { return [1 / (a * b), Object.is(a * b, -0)].join(","); }
  `);
  same("a body that closes the wrapper is a SyntaxError and nothing runs",
    e.callFunction("inject", []), "SyntaxError:undefined");
  same("0 * -5 is -0", e.callFunction("negZero", [0, -5]), "-Infinity,true");
  same("5 * 0 is +0", e.callFunction("negZero", [5, 0]), "Infinity,false");
  close(e);
}

// ── Symbol-keyed properties never cross the host value boundary ──
{
  const e = new Engine();
  e.setInstructionBudget(500_000_000);
  const syms = e.initScript(`
    var st = { a: 1 };
    var cap = Symbol('capability');
    st[cap] = 'secret-token';
    st[Symbol.toStringTag] = 'Tagged';
    var fresh = null;
    function probe() {
      var s; try { s = String(st); } catch (err) { s = err.name; }
      return [st.a, typeof st[Symbol.iterator], s, st[cap]].join(",");
    }
    function probeFresh() {
      var s; try { s = String(fresh); } catch (err) { s = err.name; }
      return [fresh.b, typeof fresh[Symbol.iterator], s].join(",");
    }
  `);
  same("a read carries only the data (as the guest's JSON.stringify does)",
    e.getGlobalByIndex(syms.st.index), { a: 1 });
  e.setGlobalByIndex(syms.st.index, JSON.parse('{"a":2,"@@iterator":5,"@@toPrimitive":1,"@@sym:1":"forged"}'));
  same("a write-back ignores @@ keys and keeps the Symbol-keyed properties",
    e.callFunction("probe", []), "2,undefined,[object Tagged],secret-token");
  e.setGlobalByIndex(syms.fresh.index, JSON.parse('{"b":3,"@@iterator":5,"@@toPrimitive":1}'));
  same("host data never creates a Symbol-keyed property",
    e.callFunction("probeFresh", []), "3,undefined,[object Object]");
  close(e);
}

console.log(`\n${pass} passed, 0 failed`);
