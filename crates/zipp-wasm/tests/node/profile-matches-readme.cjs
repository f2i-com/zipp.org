// The README's resource table is held to the artifact's own profile, so the
// documented contract cannot drift from the enforced one again (at v0.0.14
// four rows described an older build; the 6 September 2026 audit's Z04).
//
// Each entry names a README row by its leading cell and the profile figures
// that must appear, formatted with thousands separators, in that row.
const fs = require("node:fs");
const path = require("node:path");
const { zippProfile } = require("./pkg/zipp_wasm.js");

const profile = JSON.parse(zippProfile());
const limits = profile.limits;
const readme = fs.readFileSync(path.join(__dirname, "..", "..", "README.md"), "utf8");
const table = new Map();
for (const line of readme.split("\n")) {
  const m = /^\|\s*([^|]+?)\s*\|\s*(.+?)\s*\|\s*$/.exec(line);
  if (m) table.set(m[1], m[2]);
}
const grouped = (n) => n.toLocaleString("en-US");

let pass = 0, fail = 0;
function row(cell, ...figures) {
  const text = table.get(cell);
  if (text === undefined) { fail++; console.log(`  FAIL no README row starts with ${JSON.stringify(cell)}`); return; }
  const missing = figures.filter((n) => !text.includes(grouped(n)));
  if (missing.length === 0) { pass++; console.log(`  ok   ${cell}: ${figures.map(grouped).join(", ")}`); }
  else { fail++; console.log(`  FAIL ${cell}: README says ${JSON.stringify(text)}, artifact enforces ${missing.map(grouped).join(", ")}`); }
}

row("Initial guest source", limits.initialSourceBytes);
row("One `evalInContext` expression", limits.evalExpressionBytes);
row("Retained `evalInContext` wrapper source", limits.evalRetainedSourceBytes, limits.evalCalls);
row("All runtime compilation (`eval`, `Function`, `ShadowRealm`, and host eval)",
  limits.dynamicCodeSourceBytes, limits.dynamicCodeRetainedSourceBytes, limits.dynamicCodeCalls,
  limits.dynamicCodeFunctions, limits.dynamicCodeClasses);
row("VM execution", limits.lifetimeSteps, limits.maxInstructionBudgetSteps);
row("Payload-aware VM heap high-water", limits.approxHeapBytes);
row("WebAssembly linear memory", limits.linkedMemoryMaxBytes);
row("Lifetime console output", limits.lifetimeOutputBytes);
row("Synchronous host bridge", limits.syncBridgeKindBytes, limits.syncBridgeArgs, limits.syncBridgeBytes);
row("Host value conversion", limits.hostValueNodes, limits.hostValueStringBytes, limits.fingerprintNodes, limits.fingerprintStringBytes);
row("Asynchronous `host.call`", limits.hostCallQueue, limits.hostCallPending, limits.hostCallRequestUnits, limits.hostCallDrainRequests, limits.hostCallDrainStringBytes);
row("`accel.make` binding spec", limits.accelSpecBytes, limits.accelSpecEntries, limits.accelSpecNameBytes);

// The version itself is held to the release tag by release.yml, not here.
console.log(`\n${pass} passed, ${fail} failed (artifact ${profile.version})`);
if (fail) process.exit(1);
