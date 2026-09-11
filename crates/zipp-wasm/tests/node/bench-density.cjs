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
// package — a separate WebAssembly instance, as a browser Worker would be.
// A cell (one scenario at one instance count) runs in PHASES, so that what
// is measured is what the label says (the 11 September 2026 close audit's
// ZA-12):
//
//   load      every worker requires the package and reports `load_ms`
//             (cold start, reported separately, never inside the window);
//   measure   all workers start the same wall-clock window together and run
//             the scenario until it ends, timing every unit on a monotonic
//             clock and validating every unit's output;
//   snapshot  every worker holds its live instance while it reports; the
//             main thread samples process RSS and the engines' own figures
//             with ALL instances of the cell alive and none of the next;
//   teardown  workers dispose on request, report `dispose_ms`, and exit; the
//             next cell starts only after every exit.
//
// Figures per cell:
//   units             completed units of USEFUL work (healthy peers only)
//   units_per_s       units / window seconds
//   unit_ms_p50/p95/p99   POOLED percentiles over every unit of every healthy
//                     worker (one population; a busy worker weighs more)
//   worst_worker_p99  the highest per-worker p99 (a tenant's tail, named so)
//   mean_worker_p50   the mean of per-worker medians (named so; not a pooled
//                     percentile — the two used to be conflated, ZA-11)
//   cpu_s_per_unit    process CPU seconds over the window / useful units
//                     (all threads; in `hostile`, "under interference")
//   attack_cycles     the hostile tenant's budget-exhaustion cycles (hostile
//                     scenario only; never counted as useful work, ZA-11)
//   rss_mb            process RSS at the snapshot, every instance alive
//   heap_mb           mean of the engines' own resident-heap estimates
//   load_ms / dispose_ms   cold start and teardown, mean per worker
//   samples / samples_dropped   latency samples pooled, and any past the cap
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
const SCENARIOS = ["turnover", "idle", "sync", "burst", "alloc", "hostile"];
const MAX_INSTANCES = 64;
const MAX_SAMPLES_PER_WORKER = 400000;
const APP = `
  "use strict";
  var state = { items: [], counter: 0, name: "density" };
  function tick(n) { for (var i = 0; i < n; i++) state.items.push({ id: state.counter++, v: i * 1.5 }); if (state.items.length > 512) state.items.length = 0; return state.counter; }
  function tiny() { return state.counter++; }
  function alloc() { var out = []; for (var i = 0; i < 10000; i++) out.push({ i: i, s: "k" + i, a: [i, i + 1] }); var text = JSON.stringify(out); var back = JSON.parse(text); return back.length + text.length; }
  function spin() { for (;;) {} }
`;

// The harness's percentile convention: the element at floor(p/100 * n) of the
// sorted samples (nearest-rank, lower). Stated once so the pooled and the
// per-worker figures agree by construction.
function percentile(sorted, p) {
  if (sorted.length === 0) return 0;
  const i = Math.min(sorted.length - 1, Math.floor((p / 100) * sorted.length));
  return sorted[i];
}

// Aggregate one cell from the workers' reports (ZA-11). Pure: unit-tested by
// tests/node/density-stats.cjs against a small exact pooled reference.
function aggregate(scenario, reports, windowSeconds, cpuSeconds) {
  const hostile = scenario === "hostile";
  const peers = hostile ? reports.filter((r) => r.role === "peer") : reports;
  const attackers = hostile ? reports.filter((r) => r.role === "attacker") : [];
  const pooled = [];
  let samplesDropped = 0;
  for (const r of peers) { for (const x of r.latencies) pooled.push(x); samplesDropped += r.samplesDropped; }
  pooled.sort((a, b) => a - b);
  const workerP = (r, p) => percentile(r.latencies.slice().sort((a, b) => a - b), p);
  const units = peers.reduce((a, r) => a + r.units, 0);
  const failures = reports.reduce((a, r) => a + r.failures, 0);
  const attackCycles = attackers.reduce((a, r) => a + r.units, 0);
  const activePeers = peers.filter((r) => r.latencies.length > 0);
  return {
    units,
    failures,
    units_per_s: windowSeconds > 0 ? +(units / windowSeconds).toFixed(1) : null,
    cpu_s_per_unit: units > 0 ? +(cpuSeconds / units).toFixed(6) : null,
    cpu_s_per_unit_basis: hostile ? "process CPU (attacker included) / healthy peer units: cost under hostile interference" : "process CPU / units",
    unit_ms_p50: +percentile(pooled, 50).toFixed(3),
    unit_ms_p95: +percentile(pooled, 95).toFixed(3),
    unit_ms_p99: +percentile(pooled, 99).toFixed(3),
    worst_worker_p99: activePeers.length ? +Math.max(...activePeers.map((r) => workerP(r, 99))).toFixed(3) : null,
    mean_worker_p50: activePeers.length ? +(activePeers.reduce((a, r) => a + workerP(r, 50), 0) / activePeers.length).toFixed(3) : null,
    attack_cycles: hostile ? attackCycles : undefined,
    healthy_peers: hostile ? peers.length : undefined,
    samples: pooled.length,
    samples_dropped: samplesDropped,
    idle_workers: peers.length - activePeers.length,
  };
}

// ── worker ─────────────────────────────────────────────────────────────────
if (!isMainThread) {
  const { scenario, index } = workerData;
  const t0 = performance.now();
  const zipp = require(PKG);
  const { Engine, zippInstanceUsage } = zipp;
  const loadMs = performance.now() - t0;
  const latencies = [];
  let samplesDropped = 0;
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
  const time = (fn) => {
    const start = performance.now();
    fn();
    const ms = performance.now() - start;
    if (latencies.length < MAX_SAMPLES_PER_WORKER) latencies.push(ms); else samplesDropped++;
    units++;
  };
  const pause = (ms) => Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, Math.max(0, ms));

  const run = (endAt) => {
    const running = () => Date.now() < endAt;
    if (scenario === "turnover") {
      while (running()) {
        time(() => {
          const [e] = fresh();
          expect(e.callFunction("tick", [8]) === 8, "tick");
          e.dispose(); e.free();
        });
      }
    } else if (scenario === "idle") {
      [engine, symbols] = fresh();
      let next = performance.now();
      let expected = 0;
      while (running()) {
        const now = performance.now();
        if (now >= next) {
          next += 16;
          time(() => { engine.renewInstructionBudget(); const got = engine.callFunction("tiny", []); expect(got === expected, `tiny ${got} != ${expected}`); expected++; });
        } else {
          pause(Math.min(16, next - now));
        }
      }
    } else if (scenario === "sync" || scenario === "hostile-peer") {
      [engine, symbols] = fresh();
      const idx = [symbols.state.index, symbols.tick.index];
      let last = engine.getGlobalsFingerprint(idx);
      let expectedCounter = 0;
      while (running()) {
        time(() => {
          engine.renewInstructionBudget();
          expectedCounter += 4;
          const counter = engine.callFunction("tick", [4]);
          const now = engine.getGlobalsFingerprint(idx);
          const changed = [];
          for (let i = 0; i < idx.length; i++) if (now[i] !== last[i]) changed.push(idx[i]);
          const values = engine.getGlobalsBatch(changed);
          expect(counter === expectedCounter && changed.length === 1 && changed[0] === symbols.state.index && values[0] && values[0].counter === expectedCounter && Array.isArray(values[0].items), "changed state read");
          last = now;
        });
      }
    } else if (scenario === "burst") {
      [engine, symbols] = fresh();
      let expected = 0;
      while (running()) {
        time(() => { engine.renewInstructionBudget(); let last = -1; for (let i = 0; i < 100; i++) last = engine.callFunction("tiny", []); expected += 100; expect(last === expected - 1, `burst ${last} != ${expected - 1}`); });
      }
    } else if (scenario === "alloc") {
      [engine, symbols] = fresh();
      while (running()) {
        time(() => { engine.renewInstructionBudget(); const got = engine.callFunction("alloc", []); expect(got === 395565, `alloc ${got}`); });
      }
    } else if (scenario === "hostile") {
      // A runaway tenant under a small budget: terminal each time, recreated.
      while (running()) {
        time(() => {
          const [e] = fresh(2_000_000);
          let threw = false;
          try { e.callFunction("spin", []); } catch { threw = true; }
          expect(threw && e.disposed === true, "the budget stops the runaway call and disposes the engine");
          try { e.dispose(); } catch {}
          try { e.free(); } catch {}
        });
      }
    } else {
      throw new Error(`unknown scenario ${scenario}`);
    }
  };

  parentPort.on("message", (msg) => {
    if (msg.type === "start") {
      // Everyone starts the same wall-clock window; nothing before it counts.
      while (Date.now() < msg.startAt) pause(1);
      run(msg.endAt);
      const usage = engine ? engine.resourceUsage() : null;
      const samples = Float64Array.from(latencies);
      parentPort.postMessage({
        type: "result", index, scenario, role: scenario === "hostile" ? "attacker" : "peer",
        units, failures, samplesDropped, loadMs, latencies: samples,
        heapBytes: usage ? usage.heapBytes : 0,
        instance: zippInstanceUsage(),
      }, [samples.buffer]);
      // Hold the live instance until told to tear down.
    } else if (msg.type === "teardown") {
      const start = performance.now();
      if (engine) { try { engine.dispose(); engine.free(); } catch {} }
      engine = null;
      parentPort.postMessage({ type: "done", index, disposeMs: performance.now() - start });
      process.exit(0);
    } else if (msg.type === "load?") {
      parentPort.postMessage({ type: "ready", index, loadMs });
    }
  });
  parentPort.postMessage({ type: "ready", index, loadMs });
}

// ── main ───────────────────────────────────────────────────────────────────
async function runLevel(scenario, instances, seconds) {
  const workers = [];
  const ready = [];
  const results = new Map();
  const done = [];
  const exits = [];
  let failed = null;
  for (let i = 0; i < instances; i++) {
    const s = scenario === "hostile" ? (i === 0 ? "hostile" : "hostile-peer") : scenario;
    const w = new Worker(__filename, { workerData: { scenario: s, index: i } });
    w.on("error", (e) => { failed = failed || e; });
    exits.push(new Promise((resolve) => w.once("exit", (code) => resolve({ index: i, code }))));
    ready.push(new Promise((resolve) => {
      w.on("message", (msg) => {
        if (msg.type === "ready") resolve(msg);
        else if (msg.type === "result") results.set(msg.index, msg);
        else if (msg.type === "done") done.push(msg);
      });
    }));
    workers.push(w);
  }
  // load: every worker has the package before the window opens.
  const loaded = await Promise.all(ready);
  if (failed) throw failed;
  // measure: one shared wall-clock window.
  const startAt = Date.now() + 100;
  const endAt = startAt + seconds * 1000;
  while (Date.now() < startAt - 20) await new Promise((r) => setTimeout(r, 5));
  const cpu0 = process.cpuUsage();
  for (const w of workers) w.postMessage({ type: "start", startAt, endAt });
  // snapshot: wait for every result while every instance is still alive.
  while (results.size < instances) {
    if (failed) throw failed;
    await new Promise((r) => setTimeout(r, 10));
  }
  const cpu = process.cpuUsage(cpu0);
  const cpuSeconds = (cpu.user + cpu.system) / 1e6;
  const rssMb = +(process.memoryUsage().rss / 1048576).toFixed(1);
  const reports = [...results.values()].sort((a, b) => a.index - b.index);
  // teardown: dispose, then wait for every exit before the next cell.
  for (const w of workers) w.postMessage({ type: "teardown" });
  const exitCodes = await Promise.all(exits);
  const bad = exitCodes.filter((x) => x.code !== 0);
  if (bad.length) throw new Error(`workers exited abnormally: ${JSON.stringify(bad)}`);
  const agg = aggregate(scenario, reports, seconds, cpuSeconds);
  return {
    scenario, instances, window_s: seconds,
    ...agg,
    rss_mb: rssMb,
    heap_mb: +(reports.reduce((a, r) => a + r.heapBytes, 0) / Math.max(1, reports.length) / 1048576).toFixed(2),
    retained_program_functions: reports.reduce((a, r) => a + r.instance.programFunctions, 0),
    retained_dynamic_functions: reports.reduce((a, r) => a + r.instance.retainedFunctions, 0),
    load_ms: +(loaded.reduce((a, r) => a + r.loadMs, 0) / loaded.length).toFixed(1),
    dispose_ms: +(done.reduce((a, r) => a + r.disposeMs, 0) / Math.max(1, done.length)).toFixed(2),
    _sanity: agg.samples > 0 || (scenario === "hostile" && instances === 1) ? "ok" : "no-latency-samples",
  };
}

function usageError(message) {
  const e = new Error(message);
  e.usage = true;
  return e;
}

function parseArgs(argv) {
  let instances = null, seconds = 3, scenario = "all", jsonOut = null;
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === "--instances") instances = Number(argv[++i]);
    else if (argv[i] === "--seconds") seconds = Number(argv[++i]);
    else if (argv[i] === "--scenario") scenario = argv[++i];
    else if (argv[i] === "--json") jsonOut = argv[++i];
    else throw usageError(`unknown option ${argv[i]}`);
  }
  if (instances !== null && !(Number.isInteger(instances) && instances >= 1 && instances <= MAX_INSTANCES)) {
    throw usageError(`--instances must be an integer in [1, ${MAX_INSTANCES}], not ${instances}`);
  }
  if (!(Number.isFinite(seconds) && seconds > 0 && seconds <= 3600)) throw usageError(`--seconds must be a positive number of seconds, not ${seconds}`);
  if (scenario !== "all" && !SCENARIOS.includes(scenario)) throw usageError(`--scenario must be one of ${SCENARIOS.join(", ")} or all, not ${scenario}`);
  if (jsonOut !== null && !jsonOut) throw usageError("--json needs a path");
  return { instances, seconds, scenario, jsonOut };
}

async function main() {
  const { instances, seconds, scenario, jsonOut } = parseArgs(process.argv.slice(2));
  if (!fs.existsSync(PKG)) { console.error("no built package at tests/node/pkg"); process.exit(2); }
  const levels = instances ? [instances] : [1, 2, 4, 8];
  const scenarios = scenario === "all" ? SCENARIOS : [scenario];
  const zipp = require(PKG);
  const profile = JSON.parse(zipp.zippProfile());
  const header = {
    engine: `${profile.engine} ${profile.version}`, source: profile.source.sha, node: process.version,
    cpu: os.cpus()[0]?.model, cores: os.cpus().length, platform: `${os.platform()} ${os.release()}`,
    wasm_sha256: require("node:crypto").createHash("sha256").update(fs.readFileSync(path.join(__dirname, "pkg", "zipp_wasm_bg.wasm"))).digest("hex"),
    percentiles: "pooled over every unit of every healthy worker; worst_worker_p99 and mean_worker_p50 are per-worker summaries",
  };
  console.log(JSON.stringify(header));
  const rows = [];
  for (const s of scenarios) {
    for (const n of levels) {
      const row = await runLevel(s, n, seconds);
      rows.push(row);
      const attack = s === "hostile" ? `  attack_cycles ${row.attack_cycles}` : "";
      console.log(`${s.padEnd(9)} x${String(n).padEnd(2)} units ${String(row.units).padStart(7)}  ${String(row.units_per_s).padStart(8)}/s  cpu_s/unit ${String(row.cpu_s_per_unit).padStart(10)}  p50 ${row.unit_ms_p50}ms p95 ${row.unit_ms_p95}ms p99 ${row.unit_ms_p99}ms worst_p99 ${row.worst_worker_p99}ms  rss ${row.rss_mb}MB heap ${row.heap_mb}MB  load ${row.load_ms}ms dispose ${row.dispose_ms}ms  failures ${row.failures}${attack}`);
    }
  }
  if (jsonOut) fs.writeFileSync(jsonOut, JSON.stringify({ header, rows }, null, 2));
  const failed = rows.some((r) => r.failures > 0 || r._sanity !== "ok");
  process.exit(failed ? 1 : 0);
}

if (isMainThread) {
  if (require.main === module) {
    main().catch((e) => { console.error(e && e.usage ? e.message : e); process.exit(1); });
  } else {
    module.exports = { aggregate, percentile, parseArgs, SCENARIOS };
  }
}
