// The density harness's aggregation (tests/node/bench-density.cjs), checked
// against small exact references: the reported p50/p95/p99 are POOLED
// percentiles of every unit of every healthy worker, the per-worker
// summaries are named as such, hostile cycles are never useful units, and
// idle workers, failed units and unequal sample counts do not distort any
// of it (the 11 September 2026 close audit's ZA-11). No engine involved.
"use strict";
const assert = require("node:assert/strict");
const { aggregate, percentile, parseArgs, SCENARIOS } = require("./bench-density.cjs");

let pass = 0, fail = 0;
function test(name, fn) {
  try { fn(); pass++; console.log(`  ok   ${name}`); }
  catch (e) { fail++; console.log(`  FAIL ${name} — ${e.message}`); }
}
const report = (latencies, extra = {}) => ({ role: "peer", units: latencies.length, failures: 0, samplesDropped: 0, latencies, loadMs: 1, heapBytes: 0, instance: {}, ...extra });

test("the percentile convention is nearest-rank (lower) on the sorted samples", () => {
  assert.equal(percentile([], 50), 0);
  assert.equal(percentile([7], 99), 7);
  assert.equal(percentile([1, 2, 3, 4], 50), 3);
  assert.equal(percentile([1, 2, 3, 4], 95), 4);
});

test("the audit's case: 1,000 one-millisecond units beside one 100 ms unit are pooled, not averaged per worker", () => {
  const a = aggregate("sync", [report(Array(1000).fill(1)), report([100])], 1, 0.5);
  // The old harness reported p50 = 50.5 and p95 = p99 = 100 for this shape.
  assert.deepEqual([a.unit_ms_p50, a.unit_ms_p95, a.unit_ms_p99], [1, 1, 1]);
  assert.equal(a.worst_worker_p99, 100, "the slow worker's tail is still visible, under its own name");
  assert.equal(a.mean_worker_p50, 50.5, "the mean of medians is reported as what it is");
  assert.equal(a.units, 1001);
  assert.equal(a.samples, 1001);
  assert.equal(a.units_per_s, 1001);
  assert.equal(a.cpu_s_per_unit, +(0.5 / 1001).toFixed(6));
});

test("pooled percentiles equal those of the concatenated, sorted population", () => {
  const w1 = [5, 1, 9, 3], w2 = [2, 8, 8, 8, 8, 8, 8, 8], w3 = [100];
  const pooled = [...w1, ...w2, ...w3].sort((x, y) => x - y);
  const a = aggregate("burst", [report(w1), report(w2), report(w3)], 2, 1);
  assert.deepEqual([a.unit_ms_p50, a.unit_ms_p95, a.unit_ms_p99], [percentile(pooled, 50), percentile(pooled, 95), percentile(pooled, 99)]);
  assert.equal(a.worst_worker_p99, 100);
});

test("an idle worker contributes no samples and does not drag the median down or the mean of medians to zero", () => {
  const a = aggregate("idle", [report([4, 4, 4, 4]), report([])], 1, 0);
  assert.deepEqual([a.unit_ms_p50, a.samples, a.idle_workers, a.mean_worker_p50, a.worst_worker_p99], [4, 4, 1, 4, 4]);
});

test("a hostile tenant's cycles are counted apart from the peers' useful units", () => {
  const a = aggregate("hostile", [
    report([50, 50, 50], { role: "attacker" }),
    report([1, 1, 1, 1], { role: "peer" }),
    report([2, 2], { role: "peer" }),
  ], 1, 0.6);
  assert.equal(a.units, 6, "only healthy peer units are useful work");
  assert.equal(a.attack_cycles, 3);
  assert.equal(a.healthy_peers, 2);
  assert.deepEqual([a.unit_ms_p50, a.unit_ms_p99], [1, 2], "attacker latencies are not in the population");
  assert.equal(a.cpu_s_per_unit, 0.1, "process CPU over healthy units: the cost under interference");
  assert.match(a.cpu_s_per_unit_basis, /interference/);
});

test("no healthy peers: nothing is divided by zero and the figures say so", () => {
  const a = aggregate("hostile", [report([50], { role: "attacker" })], 1, 0.2);
  assert.deepEqual([a.units, a.cpu_s_per_unit, a.unit_ms_p50, a.worst_worker_p99, a.mean_worker_p50, a.attack_cycles, a.samples], [0, null, 0, null, null, 1, 0]);
});

test("failed units are counted, and dropped samples are reported rather than lost silently", () => {
  const a = aggregate("alloc", [report([3, 3], { failures: 2, samplesDropped: 5 })], 1, 0);
  assert.equal(a.failures, 2);
  assert.equal(a.samples_dropped, 5);
});

test("the command line is validated rather than silently producing an empty run", () => {
  assert.throws(() => parseArgs(["--instances", "0"]), /--instances/);
  assert.throws(() => parseArgs(["--instances", "1.5"]), /--instances/);
  assert.throws(() => parseArgs(["--instances", "999"]), /--instances/);
  assert.throws(() => parseArgs(["--seconds", "-1"]), /--seconds/);
  assert.throws(() => parseArgs(["--seconds", "nope"]), /--seconds/);
  assert.throws(() => parseArgs(["--scenario", "nope"]), /--scenario/);
  assert.throws(() => parseArgs(["--bogus"]), /unknown option/);
  assert.deepEqual(parseArgs(["--instances", "4", "--seconds", "2", "--scenario", "sync"]), { instances: 4, seconds: 2, scenario: "sync", jsonOut: null });
  assert.deepEqual(SCENARIOS, ["turnover", "idle", "sync", "burst", "alloc", "hostile"]);
});

console.log(`\n${pass} passed, ${fail} failed`);
if (fail > 0) process.exit(1);
