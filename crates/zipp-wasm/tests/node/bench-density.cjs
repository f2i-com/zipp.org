#!/usr/bin/env node
// Workload-density measurement for the WebAssembly engine (the 11 September
// 2026 audit's ZIPP-20): how much useful, contained work a fixed host runs at
// several concurrency levels, reported as raw figures with no headline.
//
//   node tests/node/bench-density.cjs                       # 1,2,4,8 instances x all scenarios, 3 s each
//   node tests/node/bench-density.cjs --instances 4 --seconds 5 --scenario sync
//   node tests/node/bench-density.cjs --json out.json
//
// Each instance is a worker_thread with its OWN require of the Node-target
// package — a separate WebAssembly instance, as a browser Worker would be —
// running one scenario for a fixed wall-clock window and validating every
// unit's output. The main thread aggregates:
//
//   units          completed units of work, per instance and in total
//   unit_ms p50/p95/p99   per-unit latency in the worker (ms)
//   cpu_s_per_unit process CPU seconds per completed unit (all threads)
//   rss_mb         process RSS after the window (the OS view; not per instance)
//   heap_mb        the engine's own resident-heap estimate at the end (mean)
//   retained       what each instance retains at the end (zippInstanceUsage)
//
// Scenarios (engine-only shapes; a downstream application workload belongs
// in its own harness):
//   turnover  create → initScript(app) → one call → dispose, per unit
//   idle      a live engine served one tiny call every 16 ms; a unit is a frame
//   sync      a call that mutates state, then fingerprints + reads of what moved
//   burst     100 short calls into a live engine, per unit
//   alloc     an allocation-heavy task (10,000 objects + a JSON round trip)
//   hostile   one tenant repeatedly spends a small budget in a runaway loop and
//             is recreated (terminal), while the OTHER instances run `sync`:
//             fairness under a slow tenant is their latency, reported as usual
//
// A single RSS number cannot separate live retention from allocator
// reservation: read it beside `retained` and `heap_mb`. Nothing here is a
// performance claim; it is the saturation curve the audit asked to measure.
"use strict";
const { Worker, isMainThread, parentPort, workerData } = require("node:worker_threads");
const os = require("node:os");
const path = require("node:path");
const fs = require("node:fs");

const PKG = path.join(__dirname, "pkg", "zipp_wasm.js");
const APP = `
  "use strict";
  var state = { items: [], counter: 0, name: "density" };
  function tick(n) { for (var i = 0; i < n; i++) state.items.push({ id: state.counter++, v: i * 1.5 }); if (state.items.length > 512) state.items.length = 0; return state.counter; }
  function tiny() { return state.counter++; }
  function alloc() { var out = []; for (var i = 0; i < 10000; i++) out.push({ i: i, s: "k" + i, a: [i, i + 1] }); var text = JSON.stringify(out); var back = JSON.parse(text); return back.length + text.length; }
  function spin() { for (;;) {} }
`;

function percentile(sorted, p) {
  if (sorted.length === 0) return 0;
  const i = Math.min(sorted.length - 1, Math.floor((p / 100) * sorted.length));
  return sorted[i];
}

// ── worker ─────────────────────────────────────────────────────────────────
if (!isMainThread) {
  const { scenario, seconds, index } = workerData;
  const zipp = require(PKG);
  const { Engine, zippInstanceUsage } = zipp;
  const latencies = [];
  let units = 0;
  let failures = 0;
  let engine = null;
  let symbols = null;
  const fresh = (budget) => {
    const e = new Engine();
    if (budget) e.setInstructionBudget(budget);
    const s = e.initScript(APP);
    return [e, s];
  };
  const expect = (ok, what) => { if (!ok) { failures++; if (failures < 3) console.error(`instance ${index}: ${scenario} validation failed: ${what}`); } };
  const deadline = Date.now() + seconds * 1000;
  const time = (fn) => { const t0 = performance.now(); fn(); latencies.push(performance.now() - t0); units++; };

  if (scenario === "turnover") {
    while (Date.now() < deadline) {
      time(() => {
        const [e] = fresh();
        expect(e.callFunction("tick", [8]) === 8, "tick");
        e.dispose(); e.free();
      });
    }
  } else if (scenario === "idle") {
    [engine, symbols] = fresh();
    let next = performance.now();
    while (Date.now() < deadline) {
      const now = performance.now();
      if (now >= next) {
        next += 16;
        time(() => { engine.renewInstructionBudget(); expect(typeof engine.callFunction("tiny", []) === "number", "tiny"); });
      } else {
        Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, Math.max(0, Math.min(16, next - now)));
      }
    }
  } else if (scenario === "sync" || scenario === "hostile-peer") {
    [engine, symbols] = fresh();
    const idx = [symbols.state.index, symbols.tick.index];
    let last = engine.getGlobalsFingerprint(idx);
    while (Date.now() < deadline) {
      time(() => {
        engine.renewInstructionBudget();
        engine.callFunction("tick", [4]);
        const now = engine.getGlobalsFingerprint(idx);
        const changed = [];
        for (let i = 0; i < idx.length; i++) if (now[i] !== last[i]) changed.push(idx[i]);
        const values = engine.getGlobalsBatch(changed);
        expect(changed.length === 1 && values[0] && Array.isArray(values[0].items), "changed state read");
        last = now;
      });
    }
  } else if (scenario === "burst") {
    [engine, symbols] = fresh();
    while (Date.now() < deadline) {
      time(() => { engine.renewInstructionBudget(); let last = 0; for (let i = 0; i < 100; i++) last = engine.callFunction("tiny", []); expect(typeof last === "number", "tiny"); });
    }
  } else if (scenario === "alloc") {
    [engine, symbols] = fresh();
    while (Date.now() < deadline) {
      time(() => { engine.renewInstructionBudget(); expect(engine.callFunction("alloc", []) > 10000, "alloc"); });
    }
  } else if (scenario === "hostile") {
    // A runaway tenant under a small budget: terminal each time, recreated.
    while (Date.now() < deadline) {
      time(() => {
        const [e] = fresh(2_000_000);
        let threw = false;
        try { e.callFunction("spin", []); } catch { threw = true; }
        expect(threw, "the budget stops the runaway call");
        try { e.dispose(); } catch {}
        try { e.free(); } catch {}
      });
    }
  }
  latencies.sort((a, b) => a - b);
  const usage = engine ? engine.resourceUsage() : null;
  parentPort.postMessage({
    index, scenario, units, failures,
    p50: percentile(latencies, 50), p95: percentile(latencies, 95), p99: percentile(latencies, 99),
    heapBytes: usage ? usage.heapBytes : 0,
    instance: zippInstanceUsage(),
  });
  if (engine) { try { engine.dispose(); engine.free(); } catch {} }
  process.exit(0);
}

// ── main ───────────────────────────────────────────────────────────────────
async function runLevel(scenario, instances, seconds) {
  const cpu0 = process.cpuUsage();
  const t0 = performance.now();
  const workers = [];
  for (let i = 0; i < instances; i++) {
    const s = scenario === "hostile" ? (i === 0 ? "hostile" : "hostile-peer") : scenario;
    workers.push(new Promise((resolve, reject) => {
      const w = new Worker(__filename, { workerData: { scenario: s, seconds, index: i } });
      w.once("message", resolve);
      w.once("error", reject);
      w.once("exit", (code) => { if (code !== 0) reject(new Error(`worker ${i} exited ${code}`)); });
    }));
  }
  const results = await Promise.all(workers);
  const wall = (performance.now() - t0) / 1000;
  const cpu = process.cpuUsage(cpu0);
  const cpuSeconds = (cpu.user + cpu.system) / 1e6;
  const units = results.reduce((a, r) => a + r.units, 0);
  const failures = results.reduce((a, r) => a + r.failures, 0);
  const peers = scenario === "hostile" ? results.filter((r) => r.scenario === "hostile-peer") : results;
  const all = peers.flatMap((r) => [r.p50, r.p95, r.p99]);
  return {
    scenario, instances, seconds, wall_s: +wall.toFixed(2), units, failures,
    units_per_s: +(units / wall).toFixed(1),
    cpu_s_per_unit: units ? +(cpuSeconds / units).toFixed(6) : null,
    unit_ms_p50: +(peers.reduce((a, r) => a + r.p50, 0) / Math.max(1, peers.length)).toFixed(3),
    unit_ms_p95: +Math.max(...peers.map((r) => r.p95), 0).toFixed(3),
    unit_ms_p99: +Math.max(...peers.map((r) => r.p99), 0).toFixed(3),
    rss_mb: +(process.memoryUsage().rss / 1048576).toFixed(1),
    heap_mb: +(results.reduce((a, r) => a + r.heapBytes, 0) / Math.max(1, results.length) / 1048576).toFixed(2),
    retained_program_functions: results.reduce((a, r) => a + r.instance.programFunctions, 0),
    retained_dynamic_functions: results.reduce((a, r) => a + r.instance.retainedFunctions, 0),
    _sanity: all.length ? "ok" : "no-latency-samples",
  };
}

async function main() {
  const args = process.argv.slice(2);
  let instances = null, seconds = 3, scenario = "all", jsonOut = null;
  for (let i = 0; i < args.length; i++) {
    if (args[i] === "--instances") instances = Number(args[++i]);
    else if (args[i] === "--seconds") seconds = Number(args[++i]);
    else if (args[i] === "--scenario") scenario = args[++i];
    else if (args[i] === "--json") jsonOut = args[++i];
    else throw new Error(`unknown option ${args[i]}`);
  }
  if (!fs.existsSync(PKG)) { console.error("no built package at tests/node/pkg"); process.exit(2); }
  const levels = instances ? [instances] : [1, 2, 4, 8];
  const scenarios = scenario === "all" ? ["turnover", "idle", "sync", "burst", "alloc", "hostile"] : [scenario];
  const zipp = require(PKG);
  const profile = JSON.parse(zipp.zippProfile());
  const header = {
    engine: `${profile.engine} ${profile.version}`, source: profile.source.sha, node: process.version,
    cpu: os.cpus()[0]?.model, cores: os.cpus().length, platform: `${os.platform()} ${os.release()}`,
    wasm_sha256: require("node:crypto").createHash("sha256").update(fs.readFileSync(path.join(__dirname, "pkg", "zipp_wasm_bg.wasm"))).digest("hex"),
  };
  console.log(JSON.stringify(header));
  const rows = [];
  for (const s of scenarios) {
    for (const n of levels) {
      const row = await runLevel(s, n, seconds);
      rows.push(row);
      console.log(`${s.padEnd(9)} x${String(n).padEnd(2)} units ${String(row.units).padStart(7)}  ${String(row.units_per_s).padStart(8)}/s  cpu_s/unit ${String(row.cpu_s_per_unit).padStart(10)}  p50 ${row.unit_ms_p50}ms p95 ${row.unit_ms_p95}ms p99 ${row.unit_ms_p99}ms  rss ${row.rss_mb}MB heap ${row.heap_mb}MB  failures ${row.failures}`);
    }
  }
  if (jsonOut) fs.writeFileSync(jsonOut, JSON.stringify({ header, rows }, null, 2));
  const failed = rows.some((r) => r.failures > 0 || r._sanity !== "ok");
  process.exit(failed ? 1 : 0);
}
main().catch((e) => { console.error(e); process.exit(1); });
