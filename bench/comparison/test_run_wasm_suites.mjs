#!/usr/bin/env node

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  auditValidationRuntimeRecovery,
  caseOrderForRep,
  canonicalStdout,
  captureStatus,
  classifyZippFailure,
  controlledEnvironment,
  createZippEvaluator,
  engineOrderForRep,
  loadSuiteInventory,
  pairOrderFor,
  parseArgs,
  summarizeExecution,
  validationRecord,
  verifyDeclaredInputsUnchanged,
  verifyZippFixedLimits,
  zippLinesToStdout,
} from "./run_wasm_suites.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const RUNNER = path.join(HERE, "run_wasm_suites.mjs");
const ROOT = path.resolve(HERE, "..", "..");

function sha256(value) {
  return crypto.createHash("sha256").update(value).digest("hex");
}

const REAL13 = [
  "async-promise-chain",
  "class-prototype-hot",
  "json-large",
  "map-set-heavy",
  "markdown-render",
  "parse-large-js",
  "polymorphic-objects",
  "regex-log-scan",
  "sparse-array",
  "typedarray-math",
  "polymorphic-objects-v2",
  "property-ic-shapes",
  "sparse-array-v2",
];

const HOSTILE17 = [
  "calls-baseline",
  "calls-closures",
  "shapes-stable",
  "shapes-megamorphic",
  "types-stable",
  "types-churn",
  "branch-control",
  "throw-catch",
  "allocation-ephemeral",
  "allocation-survival",
  "async-burst",
  "async-lived",
  "reactish-reconcile",
  "warm-router",
  "bytecode-vm",
  "module-hot-graph",
  "npm-nanoid",
];

test("stdout canonicalization changes CRLF only and preserves raw framing", () => {
  assert.deepEqual(canonicalStdout(Buffer.from("one\r\ntwo\n\n")), Buffer.from("one\ntwo\n\n"));
  assert.deepEqual(canonicalStdout(Buffer.from([0xff, 0x0a])), Buffer.from([0xff, 0x0a]));
  assert.deepEqual(canonicalStdout(Buffer.alloc(0)), Buffer.alloc(0));
  assert.throws(() => canonicalStdout(Buffer.from("one\rtwo\n")), /lone carriage return/);
});

test("Zipp takeOutput lines reconstruct stdout without trimming", () => {
  assert.deepEqual(zippLinesToStdout([]), Buffer.alloc(0));
  assert.deepEqual(
    zippLinesToStdout(["first", "", "snowman ☃", "embedded\nline"]),
    Buffer.from("first\n\nsnowman ☃\nembedded\nline\n"),
  );
  assert.throws(() => zippLinesToStdout("line"), /array of strings/);
  assert.throws(() => zippLinesToStdout(["ok", 7]), /array of strings/);
});

test("capture status accepts CRLF equivalence but fails closed on mismatches", () => {
  const matching = captureStatus(
    {
      ok: true,
      status: 0,
      stdout: Buffer.from("async-result\r\n"),
      stderr: Buffer.alloc(0),
      failure: null,
    },
    Buffer.from("async-result\n"),
  );
  assert.equal(matching.valid, true);
  assert.equal(matching.output_exact, true);
  assert.equal(matching.stdout.bytes, 14);
  assert.equal(matching.stdout.canonical_bytes, 13);

  const mismatch = captureStatus(
    { ok: true, status: 0, stdout: Buffer.from("wrong\n"), stderr: Buffer.alloc(0), failure: null },
    Buffer.from("right\n"),
  );
  assert.equal(mismatch.valid, false);
  assert.equal(mismatch.output_exact, false);

  const unhealthy = captureStatus(
    {
      ok: false,
      status: null,
      stdout: Buffer.from("right\n"),
      stderr: Buffer.from("failed\n"),
      failure: { kind: "engine-error", message: "failed" },
    },
    Buffer.from("right\n"),
  );
  assert.equal(unhealthy.valid, false);
  assert.equal(unhealthy.output_exact, true);
  assert.equal(unhealthy.failure.kind, "engine-error");

  const badOracle = captureStatus(
    { ok: true, status: 0, stdout: Buffer.from("x\n"), stderr: Buffer.alloc(0), failure: null },
    Buffer.from("x\r"),
  );
  assert.equal(badOracle.valid, false);
  assert.equal(badOracle.failure.kind, "oracle-error");

  const noisyQuickJs = captureStatus(
    {
      ok: true,
      status: 0,
      stdout: Buffer.from("right\n"),
      stderr: Buffer.from("unexpected diagnostic\n"),
      failure: null,
    },
    Buffer.from("right\n"),
  );
  assert.equal(noisyQuickJs.output_exact, true);
  assert.equal(noisyQuickJs.valid, false, "nonempty engine stderr must invalidate evidence");
});

test("limit-looking guest text remains an untyped engine error", () => {
  const instruction = classifyZippFailure(
    "RangeError: script exceeded its instruction budget",
  );
  assert.equal(instruction.kind, "engine-error");
  assert.equal(instruction.limit, null);
  assert.equal(instruction.fixed_limit_message_match, "instructions");
  assert.match(instruction.classification_basis, /advisory exact-message match only/);
  assert.match(instruction.classification_basis, /does not export.*typed resource-limit status/);

  const arbitrary = classifyZippFailure(
    new Error("guest says instruction budget, but not the exact recorder message"),
  );
  assert.equal(arbitrary.kind, "engine-error");
  assert.equal(arbitrary.limit, null);
  assert.equal(arbitrary.fixed_limit_message_match, null);
});

test("QuickJS adapter exclusion requires the exact healthy no-job-drain shape", () => {
  const item = {
    key: "hostile:async-burst",
    engine_support: {
      "quickjs-ng": {
        status: "unsupported",
        reason_code: "quickjs-reactor-no-job-drain",
        reason: "no pending-job drain",
      },
    },
  };
  const base = {
    milliseconds: 1,
    status: 0,
    stdout: Buffer.alloc(0),
    stderr: Buffer.alloc(0),
    stderr_observable: true,
    teardown_clean: true,
    teardown_failures: [],
  };

  const expectedLimitation = validationRecord(
    "quickjs-ng",
    item,
    { ...base, ok: true, failure: null },
    Buffer.from("async result\n"),
  );
  assert.equal(expectedLimitation.valid, false);
  assert.equal(expectedLimitation.failure.kind, "adapter-unsupported");
  assert.equal(expectedLimitation.failure.reason_code, "quickjs-reactor-no-job-drain");
  assert.equal(expectedLimitation.raw_engine_ok, true);
  assert.equal(expectedLimitation.raw_engine_failure, null);

  const trap = { kind: "engine-error", message: "wasm trap" };
  const trapped = validationRecord(
    "quickjs-ng",
    item,
    { ...base, ok: false, status: null, failure: trap },
    Buffer.from("async result\n"),
  );
  assert.equal(trapped.valid, false);
  assert.equal(trapped.failure, trap);
  assert.equal(trapped.raw_engine_ok, false);
  assert.equal(trapped.raw_engine_failure, trap);
  assert.notEqual(trapped.failure.kind, "adapter-unsupported");

  const teardown = { kind: "teardown-error", message: "qjs_destroy failed" };
  const dirtyTeardown = validationRecord(
    "quickjs-ng",
    item,
    {
      ...base,
      ok: false,
      failure: teardown,
      teardown_clean: false,
      teardown_failures: [{ action: "qjs_destroy", message: "failed" }],
    },
    Buffer.from("async result\n"),
  );
  assert.equal(dirtyTeardown.valid, false);
  assert.equal(dirtyTeardown.teardown_clean, false);
  assert.equal(dirtyTeardown.failure, teardown);
  assert.equal(dirtyTeardown.raw_engine_failure, teardown);
  assert.notEqual(dirtyTeardown.failure.kind, "adapter-unsupported");
});

test("captureStatus exposes a poisoned QuickJS runtime to validation recovery", () => {
  const poisoned = captureStatus({
    ok: false,
    milliseconds: 0,
    stdout: Buffer.alloc(0),
    stderr: Buffer.alloc(0),
    stderr_observable: true,
    status: null,
    teardown_clean: false,
    runtime_poisoned: true,
    teardown_failures: [{ action: "qjs_destroy", message: "destroy failed" }],
    failure: {
      kind: "teardown-error",
      message: "QuickJS-NG context teardown failed",
    },
  }, Buffer.alloc(0));

  assert.equal(poisoned.valid, false);
  assert.equal(poisoned.runtime_poisoned, true);
  assert.equal(poisoned.teardown_clean, false);
  assert.deepEqual(
    poisoned.teardown_failures.map(item => item.action),
    ["qjs_destroy"],
  );
});

test("Zipp dispose failure poisons the instance even when Engine.free succeeds", () => {
  let constructed = 0;
  let freed = 0;
  class RecoverableEngine {
    constructor() {
      this.sequence = constructed++;
    }

    initScript() {
      if (this.sequence === 0) throw new Error("guest evaluation trapped");
    }

    takeOutput() {
      return ["second evaluation ran"];
    }

    dispose() {
      if (this.sequence === 0) throw new Error("dispose retained a trapped borrow");
    }

    free() {
      freed++;
    }
  }

  const evaluate = createZippEvaluator(RecoverableEngine);
  const first = evaluate("trapping source");
  assert.equal(first.ok, false);
  assert.equal(captureStatus(first, Buffer.alloc(0)).valid, false);
  assert.equal(first.failure.kind, "engine-error");
  assert.equal(first.teardown_clean, false);
  assert.equal(first.wrapper_destroyed, true);
  assert.equal(first.runtime_poisoned, true);
  assert.deepEqual(first.teardown_failures.map(item => item.action), ["Engine.dispose"]);

  const second = evaluate("healthy source");
  assert.equal(second.ok, false);
  assert.equal(second.failure.kind, "runtime-poisoned");
  assert.equal(second.runtime_poisoned, true);
  assert.deepEqual(second.stdout, Buffer.alloc(0));
  assert.equal(constructed, 1, "poisoned runtime must not construct another Engine");
  assert.equal(freed, 1, "the failed observation's wrapper must still be freed");
});

test("Zipp Engine.free failure poisons the persistent runtime", () => {
  let constructed = 0;
  class UnfreedEngine {
    constructor() {
      constructed++;
    }

    initScript() {}

    takeOutput() {
      return ["first evaluation completed"];
    }

    dispose() {}

    free() {
      throw new Error("wrapper free failed");
    }
  }

  const evaluate = createZippEvaluator(UnfreedEngine);
  const first = evaluate("healthy source");
  assert.equal(first.ok, false);
  assert.equal(
    captureStatus(first, Buffer.from("first evaluation completed\n")).valid,
    false,
  );
  assert.equal(first.failure.kind, "teardown-error");
  assert.equal(first.teardown_clean, false);
  assert.equal(first.wrapper_destroyed, false);
  assert.equal(first.runtime_poisoned, true);
  assert.deepEqual(first.teardown_failures.map(item => item.action), ["Engine.free"]);

  const second = evaluate("must not execute");
  assert.equal(second.ok, false);
  assert.equal(second.failure.kind, "runtime-poisoned");
  assert.equal(second.runtime_poisoned, true);
  assert.equal(second.wrapper_destroyed, false);
  assert.deepEqual(second.stdout, Buffer.alloc(0));
  assert.equal(constructed, 1, "poisoned runtime must not construct another Engine");
});

test("validation recovery audit rejects a missing poison reset", () => {
  const failure = { kind: "teardown-error", message: "dispose failed" };
  const teardownFailures = [{ action: "Engine.dispose", message: "dispose failed" }];
  const poisoned = {
    engine: "zipp",
    case: "real13:example",
    runtime_generation: 0,
    runtime_poisoned: true,
    failure,
    teardown_failures: teardownFailures,
  };
  const controls = {
    zipp: { engine: "zipp", runtime_generation: 0, runtime_poisoned: false },
    "quickjs-ng": { engine: "quickjs-ng", runtime_generation: 0, runtime_poisoned: false },
  };

  const missing = auditValidationRuntimeRecovery(controls, [poisoned], []);
  assert.equal(missing.valid, false);
  assert.equal(missing.poison_events.length, 1);

  const matching = auditValidationRuntimeRecovery(controls, [poisoned], [{
    sequence: 0,
    engine: "zipp",
    after_case: "real13:example",
    old_generation: 0,
    new_generation: 1,
    reason: "runtime-poisoned-after-evaluation",
    trigger_failure: failure,
    trigger_teardown_failures: teardownFailures,
    replacement_instantiated: true,
    control_validation: {
      engine: "zipp",
      runtime_generation: 1,
      valid: true,
      runtime_poisoned: false,
    },
    success: true,
    failure: null,
  }]);
  assert.equal(matching.valid, true);
});

test("inventory is exactly frozen real13 plus all hostile manifest entries", () => {
  const inventory = loadSuiteInventory({ suite: "all" });
  assert.equal(inventory.allCases.length, 30);
  assert.equal(inventory.selectedCases.length, 30);
  assert.equal(inventory.runnableCases.length, 28);
  assert.equal(inventory.unsupportedCases.length, 2);
  assert.equal(inventory.completeSelection, true);

  assert.deepEqual(
    inventory.allCases.filter(item => item.suite === "real13").map(item => item.id),
    REAL13,
  );
  assert.deepEqual(
    inventory.allCases.filter(item => item.suite === "hostile").map(item => item.id),
    HOSTILE17,
  );
  assert.deepEqual(
    verifyDeclaredInputsUnchanged(
      inventory.allCases.filter(item => item.suite === "hostile"),
    ),
    { checked_files: 24, drift: [] },
  );
  assert.equal(
    inventory.allCases.filter(item => item.suite === "real13" && item.category === "headline").length,
    10,
  );
  assert.equal(
    inventory.allCases.filter(item => item.suite === "real13" && item.category === "diagnostic").length,
    3,
  );

  for (const item of inventory.allCases) {
    assert.deepEqual(item.sourceBytes, fs.readFileSync(item.sourcePath), `${item.key} source drift`);
    assert.ok(item.inputs.length >= 1, `${item.key} has no declared input`);
    assert.ok(item.inputs.some(input => input.path === item.entry), `${item.key} entry missing from inputs`);
    for (const input of item.inputs) {
      const bytes = fs.readFileSync(path.join(ROOT, input.path));
      assert.equal(input.bytes, bytes.length, `${item.key}/${input.path} byte count drift`);
      assert.equal(input.sha256, sha256(bytes), `${item.key}/${input.path} hash drift`);
    }
    if (item.supported) {
      assert.equal(item.goal, "script");
      assert.equal(Buffer.from(item.sourceText, "utf8").equals(item.sourceBytes), true);
    }
  }

  assert.deepEqual(
    inventory.unsupportedCases.map(item => item.key),
    ["hostile:module-hot-graph", "hostile:npm-nanoid"],
  );
  for (const item of inventory.unsupportedCases) {
    assert.equal(item.goal, "module");
    assert.equal(item.supported, false);
    assert.equal(item.sourceText, null);
    assert.equal(item.support.status, "unsupported");
    assert.equal(item.support.reason_code, "zipp-wasm-no-fs-loader");
    assert.match(item.support.reason, /production Zipp WASM/);
  }

  const quickJsAsyncUnsupported = inventory.allCases
    .filter(item => item.engine_support["quickjs-ng"].reason_code === "quickjs-reactor-no-job-drain")
    .map(item => item.key);
  assert.deepEqual(quickJsAsyncUnsupported, [
    "real13:async-promise-chain",
    "hostile:async-burst",
    "hostile:async-lived",
  ]);
  for (const key of quickJsAsyncUnsupported) {
    const item = inventory.allCases.find(candidate => candidate.key === key);
    assert.equal(item.goal, "script");
    assert.equal(item.supported, true, `${key} must remain runnable for Node and Zipp`);
    assert.equal(item.engine_support.zipp.status, "supported");
    assert.equal(item.engine_support["quickjs-ng"].status, "unsupported");
    assert.match(item.engine_support["quickjs-ng"].reason, /exports no pending-job drain/);
  }
});

test("module-only selection stays inventoried and explicitly non-runnable", () => {
  const inventory = loadSuiteInventory({
    suite: "hostile",
    cases: ["module-hot-graph", "hostile:npm-nanoid"],
  });
  assert.equal(inventory.allCases.length, 17);
  assert.deepEqual(
    inventory.selectedCases.map(item => item.key),
    ["hostile:module-hot-graph", "hostile:npm-nanoid"],
  );
  assert.equal(inventory.runnableCases.length, 0);
  assert.equal(inventory.unsupportedCases.length, 2);
  assert.equal(inventory.completeSelection, false);
});

test("engine and work/control orders are balanced for every even run", () => {
  for (const seed of [0, 1, 0x5a172026]) {
    const positionCounts = Object.fromEntries(
      ["zipp", "quickjs-ng"].map(engine => [engine, [0, 0]]),
    );
    const pairCounts = Object.fromEntries(
      ["zipp", "quickjs-ng"].map(engine => [engine, { work: 0, control: 0 }]),
    );
    for (let rep = 0; rep < 6; rep++) {
      const order = engineOrderForRep(rep, seed);
      assert.deepEqual([...order].sort(), ["quickjs-ng", "zipp"]);
      order.forEach((engine, position) => positionCounts[engine][position]++);
      for (const engine of order) pairCounts[engine][pairOrderFor(rep, engine)[0]]++;
    }
    assert.deepEqual(positionCounts.zipp, [3, 3]);
    assert.deepEqual(positionCounts["quickjs-ng"], [3, 3]);
    assert.deepEqual(pairCounts.zipp, { work: 3, control: 3 });
    assert.deepEqual(pairCounts["quickjs-ng"], { work: 3, control: 3 });
  }
  assert.throws(() => pairOrderFor(0, "unknown"), /unknown engine/);
});

test("case order counterbalances every case across each repetition pair", () => {
  for (const length of [7, 28]) {
    const base = Array.from({ length }, (_, index) => `case-${index}`);
    for (let rep = 0; rep < 6; rep += 2) {
      const forward = caseOrderForRep(base, rep);
      const reverse = caseOrderForRep(base, rep + 1);
      assert.deepEqual([...forward].sort(), [...base].sort());
      assert.deepEqual([...reverse].sort(), [...base].sort());
      assert.deepEqual(reverse, [...forward].reverse());
      for (const item of base) {
        assert.equal(
          forward.indexOf(item) + reverse.indexOf(item),
          length - 1,
          `${item} must occupy complementary early/late positions in reps ${rep}/${rep + 1}`,
        );
      }
    }
  }
  assert.throws(() => caseOrderForRep([], 0), /nonempty array/);
  assert.throws(() => caseOrderForRep(["case"], -1), /nonnegative integer/);
});

function observation(caseKey, engine, rep, work, control, valid = true) {
  return {
    case: caseKey,
    engine,
    rep,
    valid,
    work_ms: work,
    control_ms: control,
    adjusted_ms: work - control,
  };
}

test("summary reports complete per-case ratios and suite geomeans", () => {
  const cases = [
    { key: "real13:a", suite: "real13", id: "a", supported: true },
    { key: "real13:b", suite: "real13", id: "b", supported: true },
    { key: "hostile:module", suite: "hostile", id: "module", supported: false },
  ];
  const rows = [];
  for (let rep = 0; rep < 2; rep++) {
    rows.push(observation("real13:a", "zipp", rep, 2, 1));
    rows.push(observation("real13:a", "quickjs-ng", rep, 4, 2));
    rows.push(observation("real13:b", "zipp", rep, 8, 5));
    rows.push(observation("real13:b", "quickjs-ng", rep, 4, 2));
  }
  const summary = summarizeExecution(cases, rows, 2);
  assert.equal(summary.by_case["real13:a"].zipp_over_quickjs_persistent, 0.5);
  assert.equal(summary.by_case["real13:a"].zipp_over_quickjs_adjusted, 0.5);
  assert.equal(summary.by_case["real13:b"].zipp_over_quickjs_persistent, 2);
  assert.equal(summary.by_case["real13:b"].zipp_over_quickjs_adjusted, 1.5);
  assert.equal(summary.by_suite.real13.comparable_cases, 2);
  assert.equal(summary.by_suite.real13.complete_persistent_geomean, 1);
  assert.ok(Math.abs(summary.by_suite.real13.complete_adjusted_geomean - Math.sqrt(0.75)) < 1e-12);
  assert.equal(summary.by_suite.hostile.supported_script_cases, 0);
  assert.equal(summary.by_suite.hostile.complete_persistent_geomean, null);
});

test("real13 summary preserves the retained-ten and diagnostic-three slices", () => {
  const cases = loadSuiteInventory({ suite: "real13" }).runnableCases;
  const rows = [];
  for (const item of cases) {
    for (let rep = 0; rep < 2; rep++) {
      rows.push(observation(item.key, "zipp", rep, 2, 1));
      rows.push(observation(item.key, "quickjs-ng", rep, 4, 2));
    }
  }
  const result = summarizeExecution(cases, rows, 2);
  const suite = result.by_suite.real13;
  assert.equal(suite.supported_script_cases, 13);
  assert.equal(suite.cross_engine_supported_cases, 12);
  assert.equal(suite.engine_support_excluded_cases, 1);
  assert.equal(suite.complete_persistent_geomean, 0.5);
  assert.equal(suite.retained10.supported_script_cases, 10);
  assert.equal(suite.retained10.expected_script_cases, 10);
  assert.equal(suite.retained10.cross_engine_supported_cases, 9);
  assert.equal(suite.retained10.engine_support_excluded_cases, 1);
  assert.equal(suite.retained10.complete_persistent_geomean, 0.5);
  assert.equal(suite.diagnostic3.supported_script_cases, 3);
  assert.equal(suite.diagnostic3.expected_script_cases, 3);
  assert.equal(suite.diagnostic3.complete_persistent_geomean, 0.5);
  assert.equal(result.combined_scripts.supported_script_cases, 13);
  assert.equal(result.combined_scripts.cross_engine_supported_cases, 12);
  assert.equal(result.combined_scripts.complete_persistent_geomean, 0.5);
});

test("hostile async category remains visible when reactor support excludes its rows", () => {
  const cases = loadSuiteInventory({ suite: "hostile" }).runnableCases;
  const rows = [];
  for (const item of cases) {
    const quickJsSupported = item.engine_support["quickjs-ng"].status === "supported";
    for (let rep = 0; rep < 2; rep++) {
      rows.push(observation(item.key, "zipp", rep, 2, 1));
      if (quickJsSupported) rows.push(observation(item.key, "quickjs-ng", rep, 4, 2));
    }
  }
  const suite = summarizeExecution(cases, rows, 2).by_suite.hostile;
  assert.equal(suite.supported_script_cases, 15);
  assert.equal(suite.cross_engine_supported_cases, 13);
  assert.equal(suite.engine_support_excluded_cases, 2);
  assert.equal(suite.categories.async.supported_script_cases, 2);
  assert.equal(suite.categories.async.cross_engine_supported_cases, 0);
  assert.equal(suite.categories.async.complete_persistent_geomean, null);
  assert.equal(suite.category_balanced.inventory_category_count, 9);
  assert.equal(suite.category_balanced.cross_engine_category_count, 8);
  assert.deepEqual(suite.category_balanced.engine_support_excluded_categories, ["async"]);
  assert.equal(suite.category_balanced.complete_persistent_geomean, 0.5);
});

test("hostile category-balanced summary gives each category equal weight", () => {
  const cases = [
    { key: "hostile:a-fast", suite: "hostile", id: "a-fast", category: "a", supported: true },
    { key: "hostile:a-even", suite: "hostile", id: "a-even", category: "a", supported: true },
    { key: "hostile:b-slow", suite: "hostile", id: "b-slow", category: "b", supported: true },
  ];
  const ratios = new Map([
    ["hostile:a-fast", [1, 4]],
    ["hostile:a-even", [2, 2]],
    ["hostile:b-slow", [4, 2]],
  ]);
  const rows = [];
  for (const item of cases) {
    const [zippWork, quickJsWork] = ratios.get(item.key);
    for (let rep = 0; rep < 2; rep++) {
      rows.push(observation(item.key, "zipp", rep, zippWork, zippWork / 2));
      rows.push(observation(item.key, "quickjs-ng", rep, quickJsWork, quickJsWork / 2));
    }
  }
  const suite = summarizeExecution(cases, rows, 2).by_suite.hostile;
  assert.equal(suite.categories.a.complete_persistent_geomean, 0.5);
  assert.equal(suite.categories.b.complete_persistent_geomean, 2);
  assert.ok(Math.abs(suite.complete_persistent_geomean - Math.cbrt(0.5)) < 1e-12);
  assert.equal(suite.category_balanced.category_count, 2);
  assert.equal(suite.category_balanced.persistent_complete_categories, 2);
  assert.equal(suite.category_balanced.complete_persistent_geomean, 1);
  assert.equal(suite.category_balanced.complete_adjusted_geomean, 1);
});

test("one incomplete row preserves available evidence but nulls complete geomeans", () => {
  const cases = [
    { key: "real13:complete", suite: "real13", id: "complete", supported: true },
    { key: "real13:incomplete", suite: "real13", id: "incomplete", supported: true },
  ];
  const rows = [
    observation("real13:complete", "zipp", 0, 2, 1),
    observation("real13:complete", "zipp", 1, 2, 1),
    observation("real13:complete", "quickjs-ng", 0, 4, 2),
    observation("real13:complete", "quickjs-ng", 1, 4, 2),
    observation("real13:incomplete", "zipp", 0, 1, 0.5),
    observation("real13:incomplete", "zipp", 1, 1, 0.5),
    observation("real13:incomplete", "quickjs-ng", 0, 1, 0.5),
    observation("real13:incomplete", "quickjs-ng", 1, 1, 0.5, false),
  ];
  const summary = summarizeExecution(cases, rows, 2).by_suite.real13;
  assert.equal(summary.supported_script_cases, 2);
  assert.equal(summary.comparable_cases, 1);
  assert.equal(summary.available_persistent_geomean, 0.5);
  assert.equal(summary.complete_persistent_geomean, null);
  assert.equal(summary.available_adjusted_geomean, 0.5);
  assert.equal(summary.complete_adjusted_geomean, null);
});

test("resource-limit observations remain raw but are excluded from aggregates", () => {
  const cases = [
    { key: "real13:ok", suite: "real13", id: "ok", supported: true },
    { key: "real13:limited", suite: "real13", id: "limited", supported: true },
  ];
  const failure = classifyZippFailure("RangeError: script exceeded its instruction budget");
  const rows = [
    observation("real13:ok", "zipp", 0, 2, 1),
    observation("real13:ok", "zipp", 1, 2, 1),
    observation("real13:ok", "quickjs-ng", 0, 4, 2),
    observation("real13:ok", "quickjs-ng", 1, 4, 2),
    { ...observation("real13:limited", "zipp", 0, 0, 0, false), failure },
    { ...observation("real13:limited", "zipp", 1, 0, 0, false), failure },
    observation("real13:limited", "quickjs-ng", 0, 4, 2),
    observation("real13:limited", "quickjs-ng", 1, 4, 2),
  ];
  const result = summarizeExecution(cases, rows, 2);
  assert.equal(rows.filter(row => row.failure === failure).length, 2);
  assert.equal(result.by_case["real13:limited"].comparable, false);
  assert.equal(result.by_case["real13:limited"].zipp_over_quickjs_persistent, null);
  assert.equal(result.by_suite.real13.comparable_cases, 1);
  assert.equal(result.by_suite.real13.available_persistent_geomean, 0.5);
  assert.equal(result.by_suite.real13.complete_persistent_geomean, null);
});

test("declared adapter-unsupported rows fail closed even if raw samples exist", () => {
  const cases = [{
    key: "hostile:async",
    suite: "hostile",
    id: "async",
    category: "async",
    supported: true,
    engine_support: {
      zipp: { status: "supported", reason_code: null, reason: null },
      "quickjs-ng": {
        status: "unsupported",
        reason_code: "quickjs-reactor-no-job-drain",
        reason: "no pending-job drain",
      },
    },
  }];
  const rows = [];
  for (let rep = 0; rep < 2; rep++) {
    rows.push(observation("hostile:async", "zipp", rep, 2, 1));
    rows.push(observation("hostile:async", "quickjs-ng", rep, 4, 2));
  }
  const result = summarizeExecution(cases, rows, 2);
  const row = result.by_case["hostile:async"];
  assert.equal(row.cross_engine_supported, false);
  assert.equal(row.comparable, false);
  assert.equal(row.zipp_over_quickjs_persistent, null);
  assert.equal(row.zipp_over_quickjs_adjusted, null);
  assert.equal(result.by_suite.hostile.cross_engine_supported_cases, 0);
  assert.equal(result.by_suite.hostile.comparable_cases, 0);
  assert.equal(result.by_suite.hostile.available_persistent_geomean, null);
});

test("nonpositive comparable medians are excluded without becoming point wins", () => {
  const cases = [
    { key: "real13:floor", suite: "real13", id: "floor", supported: true },
  ];
  const rows = [
    observation("real13:floor", "zipp", 0, 0, 1),
    observation("real13:floor", "zipp", 1, 0, 1),
    observation("real13:floor", "quickjs-ng", 0, 2, 1),
    observation("real13:floor", "quickjs-ng", 1, 2, 1),
  ];
  const summary = summarizeExecution(cases, rows, 2).by_suite.real13;
  assert.equal(summary.comparable_cases, 1);
  assert.equal(summary.persistent_ratio_cases, 0);
  assert.equal(summary.available_persistent_geomean, null);
  assert.equal(summary.complete_persistent_geomean, null);
  assert.equal(summary.persistent_point_wins, 0);
});

test("arguments default to safety gates and reject unbalanced schedules", () => {
  const defaults = parseArgs([]);
  assert.equal(defaults.suite, "all");
  assert.equal(defaults.reps, 6);
  assert.equal(defaults.allowBusy, false);
  assert.equal(defaults.allowDirty, false);
  assert.equal(defaults.overwrite, false);

  assert.throws(() => parseArgs(["--reps", "3"]), /positive even integer/);
  assert.throws(() => parseArgs(["--compile-reps", "0"]), /positive even integer/);
  assert.throws(() => parseArgs(["--startup-reps", "NaN"]), /positive even integer/);
  assert.throws(() => parseArgs(["--seed", "9007199254740992"]), /safe integer/);
  assert.throws(() => parseArgs(["--node-timeout-ms", "0"]), /positive integer/);
  assert.throws(() => parseArgs(["--suite", "micro5"]), /real13, hostile, or all/);
});

test("child environment strips benchmark-control variables and records their names", () => {
  const controlled = controlledEnvironment({
    PATH: "safe-path",
    USER_SETTING: "retained",
    ZIPP_NOJIT: "1",
    RustFlags: "-C target-cpu=native",
    NODE_OPTIONS: "--jitless",
    LLVM_PROFILE_FILE: "profile.profraw",
    ASAN_OPTIONS: "detect_leaks=0",
    UNDEFINED_VALUE: undefined,
  });
  assert.deepEqual(controlled.environment, {
    PATH: "safe-path",
    USER_SETTING: "retained",
  });
  assert.deepEqual(
    controlled.removed_names,
    ["ASAN_OPTIONS", "LLVM_PROFILE_FILE", "NODE_OPTIONS", "RustFlags", "ZIPP_NOJIT"],
  );
});

test("CLI rejects a non-official QuickJS reactor before instantiation", () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "zipp-wasm-suite-hash-test-"));
  try {
    const zippWasm = path.join(directory, "zipp.wasm");
    const zippGlue = path.join(directory, "zipp.js");
    const quickJsWasm = path.join(directory, "quickjs.wasm");
    const output = path.join(directory, "result.json");
    fs.writeFileSync(zippWasm, "not reached");
    fs.writeFileSync(zippGlue, "not reached");
    fs.writeFileSync(quickJsWasm, "not the official reactor");
    const result = spawnSync(
      process.execPath,
      [
        "--no-warnings",
        RUNNER,
        "--validation-only",
        "--allow-busy",
        "--allow-dirty",
        "--cases",
        "hostile:calls-baseline",
        "--zipp-wasm",
        zippWasm,
        "--zipp-glue",
        zippGlue,
        "--quickjs-wasm",
        quickJsWasm,
        "--output",
        output,
      ],
      { encoding: "utf8", windowsHide: true },
    );
    assert.equal(result.status, 1);
    assert.match(result.stderr, /official v0\.16\.2 artifact/);
    assert.equal(fs.existsSync(output), false);
  } finally {
    fs.rmSync(directory, { recursive: true, force: true });
  }
});

test("recorded Zipp limits stay pinned to production wasm constants", () => {
  const production = fs.readFileSync(
    path.join(ROOT, "crates", "zipp-wasm", "src", "lib.rs"),
    "utf8",
  );
  const harness = fs.readFileSync(RUNNER, "utf8");

  assert.equal(verifyZippFixedLimits(production), true);
  assert.throws(
    () => verifyZippFixedLimits(production.replace("50_000_000", "50_000_001")),
    /fixed-limit provenance drift: lifetime_instructions/,
  );

  const expected = [
    {
      rust: /const MAX_INITIAL_SOURCE_BYTES: usize = 16 \* 1024 \* 1024;/,
      harness: /initial_source_bytes: 16 \* 1024 \* 1024,/,
    },
    {
      rust: /const MAX_LIFETIME_STEPS: u64 = 50_000_000;/,
      harness: /lifetime_instructions: 50_000_000,/,
    },
    {
      rust: /const MAX_APPROX_HEAP_BYTES: usize = 512 \* 1024 \* 1024;/,
      harness: /approximate_heap_bytes: 512 \* 1024 \* 1024,/,
    },
    {
      rust: /const MAX_LIFETIME_OUTPUT_BYTES: usize = 8 \* 1024 \* 1024;/,
      harness: /lifetime_output_bytes: 8 \* 1024 \* 1024,/,
    },
  ];
  for (const item of expected) {
    assert.match(production, item.rust);
    assert.match(harness, item.harness);
  }
  assert.match(
    harness,
    /source: "crates\/zipp-wasm\/src\/lib\.rs compile-time production constants"/,
  );
  assert.match(harness, /host_can_raise_limits: false/);
});
