// Instance recycling soak (ecosystem review ZP-01, 14 September 2026).
//
// What one WASM instance keeps across the engines it hosts, against the
// production artifact, as a synthetic multi-tenant soak. Since #53 an
// engine's dynamic code (`eval`, `new Function`, classes in either) is owned
// by its state and freed by `Engine.dispose()`; `zippInstanceUsage()` keeps
// running totals of what disposed engines compiled, which are counts, not
// memory the instance holds. (This soak once asserted the opposite: that the
// definitions survived dispose and only a fresh instance reclaimed them.)
//
//   1. Within ONE instance, batches of create → run dynamic code → dispose
//      grow the disposed-engine totals with every batch, while the
//      instance's linear memory levels off after the first: the code each
//      engine compiled is freed, and the next engines reuse its memory.
//   2. A fresh instance (a new Worker or a fresh Node module) starts its
//      totals at zero and its linear memory at the initial size. Linear
//      memory never shrinks, so a fresh instance is how a host returns a
//      high-water mark; it is not needed to reclaim disposed engines' code.
//
// It reports live VM heap (per engine), allocated linear memory (per
// instance) and process RSS SEPARATELY; RSS is printed for the operator and
// never asserted, because the allocator's reservation is not a leak.
//
//   node tests/node/instance-recycling-soak.cjs            # 4 batches × 50 engines
//   ZIPP_SOAK_BATCHES=10 ZIPP_SOAK_ENGINES=200 node ...    # heavier
"use strict";
const path = require("node:path");

const BATCHES = Number(process.env.ZIPP_SOAK_BATCHES || 4);
const ENGINES = Number(process.env.ZIPP_SOAK_ENGINES || 50);
const PKG = path.join(__dirname, "pkg", "zipp_wasm.js");

let pass = 0, fail = 0;
function check(label, ok, detail) {
  if (ok) { pass++; console.log(`  ok   ${label}`); }
  else { fail++; console.log(`  FAIL ${label}${detail ? " — " + detail : ""}`); }
}

// The glue instantiates the module itself and no longer exposes it
// (`__wasm`): keep the memory of the instance each load creates.
let lastMemory = null;
const Instance = WebAssembly.Instance;
WebAssembly.Instance = function (module, imports) {
  const instance = new Instance(module, imports);
  if (instance.exports.memory) lastMemory = instance.exports.memory;
  return instance;
};

/** A brand-new WASM instance: drop the module from the require cache and load it again. */
function freshInstance() {
  for (const key of Object.keys(require.cache)) {
    if (key.startsWith(path.join(__dirname, "pkg"))) delete require.cache[key];
  }
  lastMemory = null;
  const mod = require(PKG);
  return { mod, memory: lastMemory };
}

function disposedBytes(usage) {
  return usage.disposedDynamicFunctionBytes + usage.disposedDynamicClassBytes;
}

// A function body of about 20 KB of source, so a batch compiles megabytes of
// dynamic code: enough for linear memory to show it if it outlived dispose.
const BIG_BODY = Array.from({ length: 400 }, (_, k) => `if (x === ${k}) return "${"v".repeat(24)}${k}";`).join("\n") + "\nreturn null;";

/** One tenant batch: engines that each compile dynamic code, then are disposed. */
function runBatch(mod, engines) {
  let heapPeak = 0;
  for (let i = 0; i < engines; i++) {
    const e = new mod.Engine();
    try {
      e.setInstructionBudget(5_000_000);
      e.initScript("var registry = []; function keep(f) { registry.push(f); return registry.length; }");
      // Dynamic compiles per engine that stay reachable until dispose,
      // like a UI host that evaluates expressions through eval wrappers.
      e.evalInContext(`keep(new Function("x", "return x + ${i};"))`);
      e.evalInContext(`keep(class T${i} { m() { return ${i}; } })`);
      e.evalInContext(`keep(new Function("x", ${JSON.stringify(BIG_BODY + "// " + i)}))`);
      const usage = e.resourceUsage();
      if (usage.heapBytes > heapPeak) heapPeak = usage.heapBytes;
    } finally {
      try { e.dispose(); } finally { try { e.free(); } catch {} }
    }
  }
  return { heapPeak };
}

console.log(`instance recycling soak: ${BATCHES} batches × ${ENGINES} engines`);
const rssBefore = process.memoryUsage().rss;

// ── 1. one instance, never recycled: the totals grow, memory levels off ──
{
  const { mod, memory } = freshInstance();
  check("the instance's linear memory is readable", memory !== null);
  const start = mod.zippInstanceUsage();
  const memStart = memory.buffer.byteLength;
  let previous = disposedBytes(start);
  let monotonic = true;
  let lastHeap = 0;
  let afterFirst = null;
  let totalsAfterFirst = 0;
  for (let b = 0; b < BATCHES; b++) {
    const { heapPeak } = runBatch(mod, ENGINES);
    const usage = mod.zippInstanceUsage();
    const total = disposedBytes(usage);
    lastHeap = heapPeak;
    if (b === 0) { afterFirst = memory.buffer.byteLength; totalsAfterFirst = total; }
    console.log(`  [same instance] batch ${b + 1}: enginesDisposed=${usage.enginesDisposed} disposedDynamicFunctions=${usage.disposedDynamicFunctions} disposedDynamicClasses=${usage.disposedDynamicClasses} disposedDynamicBytes=${total} liveHeapPeak=${heapPeak} linearMemory=${memory.buffer.byteLength}`);
    if (total <= previous) monotonic = false;
    previous = total;
  }
  const end = mod.zippInstanceUsage();
  const grownTotals = disposedBytes(end) - totalsAfterFirst;
  const grownMemory = memory.buffer.byteLength - afterFirst;
  check("the disposed engines' dynamic-code totals grow with every completed batch", monotonic && disposedBytes(end) > disposedBytes(start), `start=${disposedBytes(start)} end=${disposedBytes(end)}`);
  check("the deprecated retained* names report the same totals", end.retainedFunctions === end.disposedDynamicFunctions && end.retainedClasses === end.disposedDynamicClasses && end.retainedFunctionBytes === end.disposedDynamicFunctionBytes && end.retainedClassBytes === end.disposedDynamicClassBytes, JSON.stringify(end));
  check("every engine of every batch was created and disposed (no leak of live engines)", end.enginesCreated - start.enginesCreated === BATCHES * ENGINES && end.enginesDisposed - start.enginesDisposed === BATCHES * ENGINES && end.enginesLive === end.enginesCreated - end.enginesDisposed, JSON.stringify(end));
  check("live VM heap is reported per engine, separately from the instance account", lastHeap > 0 && lastHeap < 64 * 1024 * 1024);
  check("linear memory did not shrink below its initial size", memory.buffer.byteLength >= memStart);
  check("disposed engines' code is freed: after the first batch, linear memory grew by under 1 MiB while the totals grew by over 4 MiB",
    BATCHES < 2 || (grownMemory < 1 << 20 && grownTotals > 4 << 20), `memory +${grownMemory} totals +${grownTotals}`);
}

// ── 2. recycling: a fresh instance per batch starts from zero every time ──
{
  let carried = false;
  let initialMemory = null;
  let memoryStable = true;
  for (let b = 0; b < BATCHES; b++) {
    const { mod, memory } = freshInstance();
    const fresh = mod.zippInstanceUsage();
    if (disposedBytes(fresh) !== 0 || fresh.enginesDisposed !== 0) carried = true;
    if (memory) {
      if (initialMemory === null) initialMemory = memory.buffer.byteLength;
      else if (memory.buffer.byteLength !== initialMemory) memoryStable = false;
    } else {
      memoryStable = false;
    }
    runBatch(mod, ENGINES);
    const after = mod.zippInstanceUsage();
    console.log(`  [recycled]      batch ${b + 1}: freshTotals=${disposedBytes(fresh)} afterTotals=${disposedBytes(after)} linearMemoryAtStart=${memory ? initialMemory : "n/a"}`);
  }
  check("a fresh instance starts with zero totals and no disposed engines", !carried);
  check("a fresh instance starts with the same linear memory size every time", memoryStable);
}

const rssAfter = process.memoryUsage().rss;
console.log(`  process RSS: ${Math.round(rssBefore / 1048576)} MiB -> ${Math.round(rssAfter / 1048576)} MiB (reported only; allocator reservation is not a leak)`);
console.log(`instance recycling soak: ${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
