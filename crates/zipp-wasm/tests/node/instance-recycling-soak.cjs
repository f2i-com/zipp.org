// Instance recycling soak (ecosystem review ZP-01, 14 September 2026).
//
// The README's "Resource limits" says it plainly: dynamically compiled
// definitions can outlive `Engine.dispose()` inside one WASM instance, and
// per-engine caps do not bound repeated new engines in one instance. The
// only reclamation is recycling the instance. This check proves the two
// halves of that contract against the production artifact, as a synthetic
// multi-tenant soak:
//
//   1. Within ONE instance, batches of create → run dynamic code → dispose
//      leave retained definitions behind, and the total keeps growing across
//      completed batches (what a host that never recycles accumulates).
//   2. Recycling the instance (a fresh instantiation, as a new Worker or a
//      fresh Node module) starts the account at zero and its linear memory
//      at the initial size: growth does not carry across tenant batches.
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

/** A brand-new WASM instance: drop the module from the require cache and load it again. */
function freshInstance() {
  for (const key of Object.keys(require.cache)) {
    if (key.startsWith(path.join(__dirname, "pkg"))) delete require.cache[key];
  }
  const mod = require(PKG);
  // wasm-bindgen's nodejs target exposes the instance memory on the module.
  const memory = mod.__wasm && mod.__wasm.memory ? mod.__wasm.memory : null;
  return { mod, memory };
}

function retainedBytes(usage) {
  return (usage.retainedFunctionBytes || 0) + (usage.retainedClassBytes || 0);
}

/** One tenant batch: engines that each compile dynamic code, then are disposed. */
function runBatch(mod, engines) {
  let heapPeak = 0;
  for (let i = 0; i < engines; i++) {
    const e = new mod.Engine();
    try {
      e.setInstructionBudget(5_000_000);
      e.initScript("var registry = []; function keep(f) { registry.push(f); return registry.length; }");
      // Two dynamic compiles per engine that stay reachable until dispose,
      // like a UI host that evaluates expressions through eval wrappers.
      e.evalInContext(`keep(new Function("x", "return x + ${i};"))`);
      e.evalInContext(`keep(class T${i} { m() { return ${i}; } })`);
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

// ── 1. one instance, never recycled: retention grows across completed batches ──
{
  const { mod, memory } = freshInstance();
  const start = mod.zippInstanceUsage();
  const memStart = memory ? memory.buffer.byteLength : -1;
  let previous = retainedBytes(start);
  let monotonic = true;
  let lastHeap = 0;
  for (let b = 0; b < BATCHES; b++) {
    const { heapPeak } = runBatch(mod, ENGINES);
    const usage = mod.zippInstanceUsage();
    const retained = retainedBytes(usage);
    lastHeap = heapPeak;
    console.log(`  [same instance] batch ${b + 1}: enginesDisposed=${usage.enginesDisposed} retainedFunctions=${usage.retainedFunctions} retainedClasses=${usage.retainedClasses} retainedBytes=${retained} liveHeapPeak=${heapPeak} linearMemory=${memory ? memory.buffer.byteLength : "n/a"}`);
    if (retained <= previous) monotonic = false;
    previous = retained;
  }
  const end = mod.zippInstanceUsage();
  check("retained definitions survive dispose() and keep growing across completed batches in one instance", monotonic && retainedBytes(end) > retainedBytes(start), `start=${retainedBytes(start)} end=${retainedBytes(end)}`);
  check("every engine of every batch was created and disposed (no leak of live engines)", end.enginesCreated - start.enginesCreated === BATCHES * ENGINES && end.enginesDisposed - start.enginesDisposed === BATCHES * ENGINES, JSON.stringify(end));
  check("live VM heap is reported per engine, separately from the instance account", lastHeap > 0 && lastHeap < retainedBytes(end) + 64 * 1024 * 1024);
  if (memory) check("linear memory is reported per instance and did not shrink below its initial size", memory.buffer.byteLength >= memStart);
}

// ── 2. recycling: a fresh instance per batch starts from zero every time ──
{
  let carried = false;
  let initialMemory = null;
  let memoryStable = true;
  for (let b = 0; b < BATCHES; b++) {
    const { mod, memory } = freshInstance();
    const fresh = mod.zippInstanceUsage();
    if (retainedBytes(fresh) !== 0 || fresh.enginesDisposed !== 0) carried = true;
    if (memory) {
      if (initialMemory === null) initialMemory = memory.buffer.byteLength;
      else if (memory.buffer.byteLength !== initialMemory) memoryStable = false;
    }
    runBatch(mod, ENGINES);
    const after = mod.zippInstanceUsage();
    console.log(`  [recycled]      batch ${b + 1}: freshRetained=${retainedBytes(fresh)} afterRetained=${retainedBytes(after)} linearMemoryAtStart=${memory ? initialMemory : "n/a"}`);
  }
  check("a recycled instance starts with no retained definitions and no disposed engines", !carried);
  check("a recycled instance starts with the same linear memory size every time", memoryStable);
}

const rssAfter = process.memoryUsage().rss;
console.log(`  process RSS: ${Math.round(rssBefore / 1048576)} MiB -> ${Math.round(rssAfter / 1048576)} MiB (reported only; allocator reservation is not a leak)`);
console.log(`instance recycling soak: ${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
