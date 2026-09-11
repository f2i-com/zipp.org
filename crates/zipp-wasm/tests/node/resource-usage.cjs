// Retained-resource accounting and the rich-value eval, against the
// production artifact.
//
//   ZIPP-06 (stage 1)  `resourceUsage()` reports what an engine retains and
//                      has spent; `zippInstanceUsage()` reports what the WASM
//                      instance has accumulated across disposed engines — the
//                      figure `dispose()` cannot give back and a host recycles
//                      the instance on. A create/run/dispose churn records
//                      them alongside linear-memory pages and process RSS,
//                      which are reported separately and never conflated.
//   ZIPP-12            `evalInContextRich` marshals structured values under
//                      the slot-read contract, next to the JSON projection.
const { Engine, zippInstanceUsage } = require("./pkg/zipp_wasm.js");

let pass = 0, fail = 0;
function check(label, ok, detail) {
  if (ok) { pass++; console.log(`  ok   ${label}`); }
  else { fail++; console.log(`  FAIL ${label}${detail ? " — " + detail : ""}`); }
}
function same(label, got, want) {
  const g = JSON.stringify(got), w = JSON.stringify(want);
  check(label, g === w, `got ${g}, want ${w}`);
}
function thrown(fn) {
  try { fn(); return ""; } catch (x) { return String(x && x.message ? x.message : x); }
}
function close(e) { try { e.dispose(); } catch {} try { e.free(); } catch {} }

// ── ZIPP-12: the rich eval next to the JSON projection ─────────────────────
{
  const e = new Engine();
  e.initScript("var cyc = {}; cyc.self = cyc; var m = new Map(); var log = []; function f() { return 1; }");
  const rich = e.evalInContextRich("({ a: [1, -0, NaN, Infinity], f: f, m: m, s: 'x', n: null, u: undefined })");
  check("-0, NaN and Infinity cross as themselves", Object.is(rich.a[1], -0) && Number.isNaN(rich.a[2]) && rich.a[3] === Infinity, JSON.stringify(rich.a));
  check("a function and a Map read as null (opaque), undefined as undefined", rich.f === null && rich.m === null && rich.u === undefined && rich.n === null);
  same("strings and nested arrays cross", [rich.s, rich.a[0]], ["x", 1]);
  same("a cycle reads as null rather than throwing", e.evalInContextRich("cyc"), { self: null });
  same("the JSON projection still answers the old way", e.evalInContext("[NaN, -0]"), [null, 0]);
  same("declarations persist and microtasks are drained", [
    e.evalInContextRich("(later = 7, Promise.resolve().then(function () { log.push('ran'); }), later)"),
    e.evalInContextRich("[later, log.length]"),
  ], [7, [7, 1]]);
  check("a guest throw surfaces as an error", /boom/.test(thrown(() => e.evalInContextRich("(function () { throw new Error('boom'); })()"))));
  const usage = e.resourceUsage();
  check("resourceUsage counts the eval calls and retained definitions", usage.evalCalls === 6 && usage.retainedFunctions >= 3 && usage.dynamicCodeCalls >= 6 && usage.evalRetainedSourceBytes > 0, JSON.stringify(usage));
  check("resourceUsage reports a positive heap and the chosen budget", usage.heapBytes > 0 && usage.instructionBudget === 50000000 && usage.stepsUsed > 0, JSON.stringify(usage));
  e.evalInContext("(console.log('a'), console.error('b'))");
  same("buffered console lines are counted until taken", e.resourceUsage().consoleLinesBuffered, 2);
  e.takeOutput();
  same("…and not after", e.resourceUsage().consoleLinesBuffered, 0);
  check("lifetime console bytes are never credited back", e.resourceUsage().consoleBytesLifetime > 0);
  e.dispose();
  check("a disposed engine refuses resourceUsage", /disposed/.test(thrown(() => e.resourceUsage())));
  e.free();
}

// ── ZIPP-06: churn one instance and record what it retains ─────────────────
{
  const start = zippInstanceUsage();
  check("instance usage starts with the engines this file already made", start.enginesCreated >= 1 && start.enginesDisposed >= 1, JSON.stringify(start));
  const rss0 = process.memoryUsage().rss;
  const rounds = 200;
  let retainedPerEngine = 0;
  for (let i = 0; i < rounds; i++) {
    const e = new Engine();
    e.initScript("var n = 0; function tick() { n++; return n; }");
    e.callFunction("tick", []);
    // Two dynamic compilations per engine: one host eval, one guest eval.
    e.evalInContext("(function () { return eval('n + 1'); })()");
    const u = e.resourceUsage();
    if (i === 0) retainedPerEngine = u.retainedFunctions;
    if (u.retainedFunctions !== retainedPerEngine) { fail++; console.log(`  FAIL engine ${i} retained ${u.retainedFunctions} functions, expected ${retainedPerEngine}`); break; }
    close(e);
  }
  const end = zippInstanceUsage();
  same("every churned engine was counted as created and disposed", [end.enginesCreated - start.enginesCreated, end.enginesDisposed - start.enginesDisposed], [rounds, rounds]);
  check("the instance total grows by exactly what each disposed engine retained", end.retainedFunctions - start.retainedFunctions === rounds * retainedPerEngine && end.dynamicCodeCalls - start.dynamicCodeCalls === rounds * 2, JSON.stringify({ start, end, retainedPerEngine }));
  check("a fresh engine starts from zero: retention is the instance's, not the engine's", retainedPerEngine > 0);
  check("each disposed engine's compiled program is counted as retained by the instance", end.programFunctions - start.programFunctions >= rounds && end.programBytecodeBytes - start.programBytecodeBytes > 0 && end.programSourceBytes - start.programSourceBytes > 0, JSON.stringify({ start, end }));
  const rss1 = process.memoryUsage().rss;
  // Reported, not asserted: a linear-memory high-water mark that does not
  // shrink is not by itself proof of a live-allocation leak, and RSS is the
  // process's, not the instance's.
  console.log(`  info churn of ${rounds} engines: instance retains ${end.retainedFunctions - start.retainedFunctions} dynamic functions (${end.dynamicCodeSourceBytes - start.dynamicCodeSourceBytes} source bytes) and ${end.programFunctions - start.programFunctions} program functions (${end.programBytecodeBytes - start.programBytecodeBytes} bytecode bytes, ${end.programSourceBytes - start.programSourceBytes} source bytes); process RSS ${rss0} -> ${rss1} (+${rss1 - rss0} bytes)`);
}

console.log(`\n${pass} passed, ${fail} failed`);
if (fail > 0) process.exit(1);
