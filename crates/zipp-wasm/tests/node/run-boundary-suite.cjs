#!/usr/bin/env node
// The one list of production host-boundary checks, run in order against
// `tests/node/pkg/` — the Node-target wasm-bindgen package built from the
// exact artifact under test.
//
// CI, the release workflow and the security workflow all invoke THIS file
// rather than carrying their own copies of the list: the release lane used
// to omit two checks ordinary CI ran (the 11 September 2026 audit's ZIPP-09),
// which is exactly the drift a single source prevents. Every check runs even
// after one fails, so a red run names all of its failures, and the exit
// status is non-zero if any check failed.
//
//   node tests/node/run-boundary-suite.cjs            # the required set
//   node tests/node/run-boundary-suite.cjs --list     # print it and exit
//
// Inherited `ZIPP_*` switches are cleared for every child so a developer
// shell's ablation flags cannot change what the suite proves.
"use strict";
const { spawnSync } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

const here = __dirname;
const pkgWasm = path.join(here, "pkg", "zipp_wasm_bg.wasm");

// Order matters only for readability: the artifact identity check first, the
// audit-day regression suites last.
const CHECKS = [
  ["check-wasm-memory.cjs", [pkgWasm]],
  ["host-contract.cjs"],
  ["buffer-ceiling.cjs"],
  ["budget-and-seed.cjs"],
  ["large-alloc-gc.cjs"],
  ["output-budget.cjs"],
  ["host-writeback-identity.cjs"],
  ["worker-deadline.cjs"],
  ["audit-defaults.cjs"],
  ["audit-2026-09-11.cjs"],
  ["audit-2026-09-11-close.cjs"],
  ["audit-2026-09-13.cjs"],
  // The reference host adapter's main-thread contract under a mocked Worker
  // (no engine needed; ZA-01/02/03). Cheap, so it rides in the minimum gate.
  ["sdk-contract.mjs"],
  ["sdk-worker-contract.mjs"],
  // The density harness's aggregation against exact pooled references
  // (ZA-11); no engine involved, so it is cheap enough for the gate.
  ["density-stats.cjs"],
  ["resource-usage.cjs"],
  ["profile-matches-readme.cjs"],
  ["syntax-corpus.cjs"],
];

if (process.argv.includes("--list")) {
  for (const [script] of CHECKS) console.log(script);
  process.exit(0);
}

if (!fs.existsSync(pkgWasm)) {
  console.error(`no built package at ${path.dirname(pkgWasm)}; see tests/node/README.md`);
  process.exit(2);
}

const env = {};
for (const [key, value] of Object.entries(process.env)) {
  if (!key.startsWith("ZIPP_")) env[key] = value;
}

let failed = 0;
for (const [script, args = []] of CHECKS) {
  const file = path.join(here, script);
  console.log(`\n=== ${script}`);
  const r = spawnSync(process.execPath, [file, ...args], { cwd: here, env, stdio: "inherit" });
  if (r.status !== 0) {
    failed++;
    console.log(`=== ${script}: FAILED (exit ${r.status ?? r.signal})`);
  }
}
console.log(`\n${CHECKS.length - failed} of ${CHECKS.length} boundary checks passed`);
process.exit(failed ? 1 : 0);
