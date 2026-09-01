#!/usr/bin/env node

/**
 * Reproducible, adapter-inclusive WebAssembly comparison over Zipp's frozen
 * real13 and hostile suites.
 *
 * The compared modules remain live for the complete run. Every observation
 * creates and tears down a fresh guest JavaScript context, using production
 * Zipp's Engine embedding and QuickJS-NG's official WASI reactor entry point.
 * Workload source is never scaled or rewritten. A separate empty source is
 * paired with each workload to expose fixed adapter/context cost.
 */

import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import { execFileSync, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { WASI } from "node:wasi";

const SCRIPT = fileURLToPath(import.meta.url);
const ROOT = path.resolve(path.dirname(SCRIPT), "..", "..");
const REAL_ROOT = path.join(ROOT, "bench", "real");
const HOSTILE_ROOT = path.join(ROOT, "bench", "hostile");
const HOSTILE_MANIFEST = path.join(HOSTILE_ROOT, "manifest.json");
const ZIPP_PREAMBLE = path.join(ROOT, "crates", "zipp-wasm", "src", "preamble.js");
const ZIPP_WASM_LIB = path.join(ROOT, "crates", "zipp-wasm", "src", "lib.rs");
const DEFAULT_SEED = 0x5a172026;
const QUICKJS_NG_VERSION = "0.16.2";
const QUICKJS_NG_COMMIT = "1ab8676f4b6d6d669baeb5f21790fb9734636a20";
const QUICKJS_NG_REACTOR_SHA256 =
  "fc638ef0bad35edb860ca93fe5c0ea288a6ad137888b34afa8ca2c2513727cf0";
const ENGINE_NAMES = ["zipp", "quickjs-ng"];
const CONTROL_ENV_PREFIXES = [
  "ZIPP_", "RUST", "LLVM_", "MIMALLOC_", "NODE_", "DENO_", "BUN_",
  "ASAN_", "LSAN_", "MSAN_", "TSAN_", "UBSAN_",
];
const CONTROL_ENV_MARKER = "WASM_SUITES_CONTROLLED_ENV";
const CONTROL_ENV_REMOVED = "WASM_SUITES_REMOVED_ENV_NAMES";

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

const REAL_HEADLINE = new Set([
  "parse-large-js",
  "json-large",
  "markdown-render",
  "map-set-heavy",
  "typedarray-math",
  "regex-log-scan",
  "class-prototype-hot",
  "async-promise-chain",
  "polymorphic-objects",
  "sparse-array",
]);

const QUICKJS_REACTOR_NO_JOB_DRAIN = new Set([
  "real13:async-promise-chain",
  "hostile:async-burst",
  "hostile:async-lived",
]);

const ZIPP_FIXED_LIMITS = {
  initial_source_bytes: 2 * 1024 * 1024,
  lifetime_instructions: 50_000_000,
  approximate_heap_bytes: 128 * 1024 * 1024,
  lifetime_output_bytes: 96 * 1024,
  source: "crates/zipp-wasm/src/lib.rs compile-time production constants",
  host_can_raise_limits: false,
};

const ZIPP_LIMIT_MESSAGES = new Map([
  ["RangeError: initial script source exceeds", "initial-source"],
  ["RangeError: script exceeded its instruction budget", "instructions"],
  ["RangeError: script exceeded its memory budget", "heap"],
  ["RangeError: script exceeded its output budget", "output"],
  ["RangeError: dynamic code source exceeds", "dynamic-source"],
  ["RangeError: dynamic code exceeded", "dynamic-lifetime"],
  ["RangeError: regular expression exceeded its execution budget", "regexp-steps"],
  ["RangeError: regular expression exceeded its backtrack memory budget", "regexp-memory"],
]);

function usage() {
  console.log(`usage: node --no-warnings bench/comparison/run_wasm_suites.mjs [options]

Options:
  --suite NAME          real13, hostile, or all (default all)
  --cases LIST          comma-separated case ids or suite:id keys (diagnostic subset)
  --reps N              paired execution repetitions; even (default 6)
  --compile-reps N      fresh-process compile repetitions; even (default 6)
  --startup-reps N      fresh-process startup repetitions; even (default 6)
  --seed N              deterministic schedule seed (default ${DEFAULT_SEED})
  --node PATH           Node executable used as the stdout oracle
  --node-timeout-ms N   timeout for each Node oracle process (default 300000)
  --output PATH         raw JSON output
  --zipp-wasm PATH      final production Zipp .wasm
  --zipp-glue PATH      matching Zipp web glue
  --quickjs-wasm PATH   official QuickJS-NG WASI reactor
  --validation-only     validate sources/output without timed execution phases
  --list                list inventory/support status and exit
  --overwrite           replace an existing output artifact
  --allow-dirty         run from a dirty tree and record the override
  --allow-busy          override the active-build-process safety check
  --help                show this text

Hostile module cases remain in the inventory as explicitly unsupported. The
production Zipp module is built with wasm-no-fs-loader, so this runner measures
the 15 script cases and never silently treats modules as scripts.`);
}

export function parseArgs(argv) {
  const args = {
    suite: "all",
    cases: [],
    reps: 6,
    compileReps: 6,
    startupReps: 6,
    seed: DEFAULT_SEED,
    node: process.execPath,
    nodeTimeoutMs: 300_000,
    output: path.join(ROOT, "target", "comparison", "results", "wasm-suites.json"),
    zippWasm: path.join(ROOT, "landing", "public", "wasm", "zipp_wasm_bg.wasm"),
    zippGlue: path.join(ROOT, "landing", "public", "wasm", "zipp_wasm.js"),
    quickjsWasm: path.join(
      ROOT,
      "target",
      "comparison",
      "bin",
      "quickjs-ng-v0.16.2",
      "qjs-wasi-reactor.wasm",
    ),
    validationOnly: false,
    list: false,
    overwrite: false,
    allowDirty: false,
    allowBusy: false,
  };
  const values = new Map([
    ["--suite", ["suite", String]],
    ["--cases", ["cases", value => value.split(",").filter(Boolean)]],
    ["--reps", ["reps", Number]],
    ["--compile-reps", ["compileReps", Number]],
    ["--startup-reps", ["startupReps", Number]],
    ["--seed", ["seed", Number]],
    ["--node", ["node", String]],
    ["--node-timeout-ms", ["nodeTimeoutMs", Number]],
    ["--output", ["output", String]],
    ["--zipp-wasm", ["zippWasm", String]],
    ["--zipp-glue", ["zippGlue", String]],
    ["--quickjs-wasm", ["quickjsWasm", String]],
  ]);
  for (let index = 0; index < argv.length; index++) {
    const option = argv[index];
    if (option === "--help") {
      usage();
      return null;
    }
    if (option === "--validation-only") {
      args.validationOnly = true;
      continue;
    }
    if (option === "--list") {
      args.list = true;
      continue;
    }
    if (option === "--overwrite") {
      args.overwrite = true;
      continue;
    }
    if (option === "--allow-dirty") {
      args.allowDirty = true;
      continue;
    }
    if (option === "--allow-busy") {
      args.allowBusy = true;
      continue;
    }
    const spec = values.get(option);
    if (!spec || index + 1 >= argv.length) {
      throw new Error(`unknown or incomplete option: ${option}`);
    }
    const [key, convert] = spec;
    args[key] = convert(argv[++index]);
  }
  if (!["real13", "hostile", "all"].includes(args.suite)) {
    throw new Error("--suite must be real13, hostile, or all");
  }
  for (const key of ["reps", "compileReps", "startupReps"]) {
    if (!Number.isInteger(args[key]) || args[key] <= 0 || args[key] % 2 !== 0) {
      throw new Error(`--${key.replace(/[A-Z]/g, c => `-${c.toLowerCase()}`)} must be a positive even integer`);
    }
  }
  if (!Number.isSafeInteger(args.seed)) throw new Error("--seed must be a safe integer");
  if (!Number.isSafeInteger(args.nodeTimeoutMs) || args.nodeTimeoutMs <= 0) {
    throw new Error("--node-timeout-ms must be a positive integer");
  }
  for (const key of ["node", "output", "zippWasm", "zippGlue", "quickjsWasm"]) {
    args[key] = path.resolve(args[key]);
  }
  return args;
}

function sha256(value) {
  return crypto.createHash("sha256").update(value).digest("hex");
}

export function controlledEnvironment(source = process.env) {
  const environment = {};
  const removedNames = [];
  for (const [name, value] of Object.entries(source)) {
    const upper = name.toUpperCase();
    if (CONTROL_ENV_PREFIXES.some(prefix => upper.startsWith(prefix))) {
      removedNames.push(name);
    } else if (value !== undefined) {
      environment[name] = value;
    }
  }
  removedNames.sort((left, right) => left.localeCompare(right));
  return { environment, removed_names: removedNames };
}

export function verifyZippFixedLimits(source = fs.readFileSync(ZIPP_WASM_LIB, "utf8")) {
  const declarations = [
    ["initial_source_bytes", /const MAX_INITIAL_SOURCE_BYTES: usize = 2 \* 1024 \* 1024;/],
    ["lifetime_instructions", /const MAX_LIFETIME_STEPS: u64 = 50_000_000;/],
    ["approximate_heap_bytes", /const MAX_APPROX_HEAP_BYTES: usize = 128 \* 1024 \* 1024;/],
    ["lifetime_output_bytes", /const MAX_LIFETIME_OUTPUT_BYTES: usize = 96 \* 1024;/],
  ];
  const missing = declarations.filter(([, pattern]) => !pattern.test(source)).map(([name]) => name);
  if (missing.length) {
    throw new Error(`Zipp fixed-limit provenance drift: ${missing.join(", ")}`);
  }
  return true;
}

function artifact(pathname) {
  const bytes = fs.readFileSync(pathname);
  return { path: pathname, bytes: bytes.length, sha256: sha256(bytes) };
}

function median(values) {
  if (!values.length) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2
    ? sorted[middle]
    : (sorted[middle - 1] + sorted[middle]) / 2;
}

function geometricMean(values) {
  if (!values.length || values.some(value => !(value > 0))) return null;
  return Math.exp(values.reduce((sum, value) => sum + Math.log(value), 0) / values.length);
}

function seededShuffle(values, seed) {
  const output = [...values];
  let state = seed >>> 0;
  const random = () => {
    state ^= state << 13;
    state ^= state >>> 17;
    state ^= state << 5;
    return (state >>> 0) / 0x100000000;
  };
  for (let index = output.length - 1; index > 0; index--) {
    const other = Math.floor(random() * (index + 1));
    [output[index], output[other]] = [output[other], output[index]];
  }
  return output;
}

export function engineOrderForRep(rep, seed = DEFAULT_SEED) {
  const first = (seed & 1) === 0 ? [...ENGINE_NAMES] : [...ENGINE_NAMES].reverse();
  return rep % 2 === 0 ? first : [...first].reverse();
}

export function pairOrderFor(rep, engine) {
  const engineIndex = ENGINE_NAMES.indexOf(engine);
  if (engineIndex < 0) throw new Error(`unknown engine: ${engine}`);
  return (rep + engineIndex) % 2 === 0
    ? ["work", "control"]
    : ["control", "work"];
}

export function caseOrderForRep(baseOrder, rep) {
  if (!Array.isArray(baseOrder) || baseOrder.length === 0) {
    throw new Error("base case order must be a nonempty array");
  }
  if (!Number.isInteger(rep) || rep < 0) throw new Error("rep must be a nonnegative integer");
  const rotation = Math.floor(rep / 2) % baseOrder.length;
  const rotated = baseOrder.map(
    (_, index) => baseOrder[(index + rotation) % baseOrder.length],
  );
  return rep % 2 === 0 ? rotated : [...rotated].reverse();
}

export function canonicalStdout(value) {
  const raw = Buffer.isBuffer(value) ? value : Buffer.from(value);
  const canonical = Buffer.from(raw.toString("binary").replaceAll("\r\n", "\n"), "binary");
  if (canonical.includes(13)) {
    throw new Error("stdout contains a lone carriage return");
  }
  return canonical;
}

export function zippLinesToStdout(lines) {
  if (!Array.isArray(lines) || lines.some(line => typeof line !== "string")) {
    throw new Error("Zipp takeOutput() did not return an array of strings");
  }
  return Buffer.from(lines.length ? `${lines.join("\n")}\n` : "", "utf8");
}

function outputRecord(value) {
  const buffer = Buffer.isBuffer(value) ? value : Buffer.from(value);
  let canonical;
  let canonicalError = null;
  try {
    canonical = canonicalStdout(buffer);
  } catch (error) {
    canonical = Buffer.alloc(0);
    canonicalError = String(error?.message || error);
  }
  return {
    bytes: buffer.length,
    sha256: sha256(buffer),
    base64: buffer.toString("base64"),
    utf8: buffer.toString("utf8"),
    canonical_bytes: canonical.length,
    canonical_sha256: sha256(canonical),
    canonicalization_error: canonicalError,
  };
}

function errorText(error) {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.stack || error.message;
  try {
    return JSON.stringify(error);
  } catch {
    return String(error);
  }
}

export function classifyZippFailure(error) {
  const text = errorText(error);
  for (const [needle, limit] of ZIPP_LIMIT_MESSAGES) {
    if (text.includes(needle)) {
      return {
        kind: "engine-error",
        limit: null,
        message: text,
        fixed_limit_message_match: limit,
        classification_basis: (
          "advisory exact-message match only; the wasm-bindgen Engine API does not "
          + "export the VM's typed resource-limit status"
        ),
      };
    }
  }
  return {
    kind: "engine-error",
    limit: null,
    message: text,
    fixed_limit_message_match: null,
    classification_basis: "untyped wasm-bindgen exception",
  };
}

function engineSupportFor(key, goal) {
  if (goal === "module") {
    return {
      zipp: {
        status: "unsupported",
        reason_code: "zipp-wasm-no-fs-loader",
        reason: "production Zipp WASM exposes no filesystem/module loader through Engine.initScript",
      },
      "quickjs-ng": {
        status: "not-assessed",
        reason_code: "comparison-requires-zipp-module-loader",
        reason: "module case is outside this cross-engine harness because Zipp cannot load its graph",
      },
    };
  }
  return {
    zipp: { status: "supported", reason_code: null, reason: null },
    "quickjs-ng": QUICKJS_REACTOR_NO_JOB_DRAIN.has(key)
      ? {
          status: "unsupported",
          reason_code: "quickjs-reactor-no-job-drain",
          reason: (
            "official QuickJS-NG v0.16.2 qjs-wasi-reactor evaluates then returns without "
            + "js_std_loop/JS_ExecutePendingJob and exports no pending-job drain"
          ),
        }
      : { status: "supported", reason_code: null, reason: null },
  };
}

function resolveInside(root, relative, label) {
  if (typeof relative !== "string" || !relative || path.isAbsolute(relative)) {
    throw new Error(`${label} must be a nonempty relative path`);
  }
  const resolved = path.resolve(root, relative);
  const relation = path.relative(root, resolved);
  if (relation === "" || relation.startsWith("..") || path.isAbsolute(relation)) {
    throw new Error(`${label} escapes its suite root: ${relative}`);
  }
  const stat = fs.lstatSync(resolved);
  if (!stat.isFile() || stat.isSymbolicLink()) {
    throw new Error(`${label} is not a regular non-symlink file: ${relative}`);
  }
  return resolved;
}

function exactUtf8(bytes, label) {
  if (bytes.includes(0)) throw new Error(`${label} contains an embedded NUL byte`);
  const text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  if (!Buffer.from(text, "utf8").equals(bytes)) {
    throw new Error(`${label} does not round-trip as exact UTF-8`);
  }
  return text;
}

function realInventory() {
  const actual = fs.readdirSync(REAL_ROOT)
    .filter(name => name.endsWith(".js"))
    .map(name => name.slice(0, -3))
    .sort();
  const expected = [...REAL13].sort();
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    throw new Error(`bench/real membership drift: ${JSON.stringify({ expected, actual })}`);
  }
  return REAL13.map(id => {
    const sourcePath = resolveInside(REAL_ROOT, `${id}.js`, `real13:${id}`);
    const sourceBytes = fs.readFileSync(sourcePath);
    return {
      key: `real13:${id}`,
      suite: "real13",
      id,
      goal: "script",
      category: REAL_HEADLINE.has(id) ? "headline" : "diagnostic",
      entry: path.relative(ROOT, sourcePath).replaceAll("\\", "/"),
      sourcePath,
      sourceBytes,
      sourceText: exactUtf8(sourceBytes, `real13:${id}`),
      inputs: [{
        path: path.relative(ROOT, sourcePath).replaceAll("\\", "/"),
        bytes: sourceBytes.length,
        sha256: sha256(sourceBytes),
        resolvedPath: sourcePath,
      }],
      supported: true,
      support: { status: "supported", reason: null },
      engine_support: engineSupportFor(`real13:${id}`, "script"),
      timeout_s: 300,
    };
  });
}

function hostileInventory() {
  const manifestBytes = fs.readFileSync(HOSTILE_MANIFEST);
  const manifest = JSON.parse(exactUtf8(manifestBytes, "hostile manifest"));
  if (manifest.schema_version !== 1 || !Array.isArray(manifest.cases) || !manifest.cases.length) {
    throw new Error("unsupported or empty hostile manifest");
  }
  const seen = new Set();
  const cases = manifest.cases.map((item, index) => {
    if (!item || typeof item !== "object" || typeof item.id !== "string") {
      throw new Error(`hostile manifest case ${index} is malformed`);
    }
    if (!/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(item.id)) {
      throw new Error(`hostile manifest case ${index} has an unsafe id: ${item.id}`);
    }
    if (seen.has(item.id)) throw new Error(`duplicate hostile case id: ${item.id}`);
    seen.add(item.id);
    const goal = item.goal || (String(item.entry).endsWith(".mjs") ? "module" : "script");
    if (!["script", "module"].includes(goal)) throw new Error(`invalid goal for ${item.id}`);
    if ((goal === "script" && !String(item.entry).endsWith(".js"))
      || (goal === "module" && !String(item.entry).endsWith(".mjs"))) {
      throw new Error(`${item.id} entry suffix does not match its ${goal} goal`);
    }
    const inputNames = item.inputs || [item.entry];
    if (!Array.isArray(inputNames) || !inputNames.includes(item.entry)) {
      throw new Error(`${item.id} inputs must include its entry`);
    }
    const inputSeen = new Set();
    const inputs = inputNames.map((relative, inputIndex) => {
      if (inputSeen.has(relative)) throw new Error(`${item.id} has duplicate input ${relative}`);
      inputSeen.add(relative);
      const inputPath = resolveInside(HOSTILE_ROOT, relative, `${item.id}.inputs[${inputIndex}]`);
      const bytes = fs.readFileSync(inputPath);
      return {
        path: path.relative(ROOT, inputPath).replaceAll("\\", "/"),
        bytes: bytes.length,
        sha256: sha256(bytes),
        resolvedPath: inputPath,
      };
    });
    const sourcePath = resolveInside(HOSTILE_ROOT, item.entry, `${item.id}.entry`);
    const sourceBytes = fs.readFileSync(sourcePath);
    const supported = goal === "script";
    const key = `hostile:${item.id}`;
    return {
      key,
      suite: "hostile",
      id: item.id,
      goal,
      category: item.category,
      family: item.family ?? null,
      variant: item.variant ?? null,
      features: item.features ?? [],
      description: item.description ?? null,
      entry: path.relative(ROOT, sourcePath).replaceAll("\\", "/"),
      sourcePath,
      sourceBytes,
      sourceText: supported ? exactUtf8(sourceBytes, `hostile:${item.id}`) : null,
      inputs,
      supported,
      support: supported
        ? { status: "supported", reason: null }
        : {
            status: "unsupported",
            reason_code: "zipp-wasm-no-fs-loader",
            reason: "production Zipp WASM is built with wasm-no-fs-loader; module graphs cannot be loaded through Engine.initScript",
          },
      engine_support: engineSupportFor(key, goal),
      timeout_s: Number(item.timeout_s || 300),
    };
  });
  return {
    cases,
    manifest: {
      path: path.relative(ROOT, HOSTILE_MANIFEST).replaceAll("\\", "/"),
      bytes: manifestBytes.length,
      sha256: sha256(manifestBytes),
      schema_version: manifest.schema_version,
    },
  };
}

export function loadSuiteInventory(options = {}) {
  const suite = options.suite || "all";
  const selectors = options.cases || [];
  if (!["real13", "hostile", "all"].includes(suite)) throw new Error(`unknown suite: ${suite}`);
  const allCases = [];
  let manifest = null;
  if (suite === "real13" || suite === "all") allCases.push(...realInventory());
  if (suite === "hostile" || suite === "all") {
    const hostile = hostileInventory();
    allCases.push(...hostile.cases);
    manifest = hostile.manifest;
  }
  const knownSelectors = new Set(allCases.flatMap(item => [item.key, item.id]));
  const unknown = selectors.filter(selector => !knownSelectors.has(selector));
  if (unknown.length) throw new Error(`unknown --cases selector(s): ${unknown.join(", ")}`);
  const selectedCases = selectors.length
    ? allCases.filter(item => selectors.includes(item.key) || selectors.includes(item.id))
    : allCases;
  if (!selectedCases.length) throw new Error("case selection is empty");
  return {
    suite,
    manifest,
    allCases,
    selectedCases,
    runnableCases: selectedCases.filter(item => item.supported),
    unsupportedCases: allCases.filter(item => !item.supported),
    selectedUnsupportedCases: selectedCases.filter(item => !item.supported),
    completeSelection: selectors.length === 0,
  };
}

function serializableCase(item) {
  const {
    sourcePath: _sourcePath,
    sourceBytes: _sourceBytes,
    sourceText: _sourceText,
    ...metadata
  } = item;
  return {
    ...metadata,
    inputs: item.inputs.map(({ resolvedPath: _resolvedPath, ...input }) => input),
    source_bytes: item.sourceBytes.length,
    source_sha256: sha256(item.sourceBytes),
    source_transformations: [],
    completion_suppression: {
      applied: false,
      reason: "Engine.initScript, qjs -e, and Node script execution do not print script completion values",
    },
  };
}

export function verifyDeclaredInputsUnchanged(cases) {
  const unique = new Map();
  for (const item of cases) {
    for (const input of item.inputs) {
      const existing = unique.get(input.resolvedPath);
      if (existing && existing.sha256 !== input.sha256) {
        throw new Error(`conflicting recorded hashes for ${input.path}`);
      }
      unique.set(input.resolvedPath, input);
    }
  }
  const drift = [];
  for (const input of unique.values()) {
    const current = sha256(fs.readFileSync(input.resolvedPath));
    if (current !== input.sha256) drift.push(`declared input changed: ${input.path}`);
  }
  return { checked_files: unique.size, drift };
}

function nowNs() {
  return process.hrtime.bigint();
}

function elapsedMs(started) {
  return Number(process.hrtime.bigint() - started) / 1e6;
}

let importNonce = 0;
async function importSourceModule(source, label) {
  const tagged = `${source}\n// ${label} ${importNonce++}\n`;
  return import(`data:text/javascript;base64,${Buffer.from(tagged).toString("base64")}`);
}

async function makeZippRuntime(wasmPath, gluePath) {
  const module = await WebAssembly.compile(fs.readFileSync(wasmPath));
  const glue = await importSourceModule(fs.readFileSync(gluePath, "utf8"), "zipp-suite-glue");
  for (const method of ["initScript", "takeOutput", "dispose"]) {
    if (typeof glue.Engine?.prototype?.[method] !== "function") {
      throw new Error(`Zipp glue lacks Engine.${method}`);
    }
  }
  glue.initSync({ module });
  let poisonedBy = null;
  return {
    module,
    evaluate(source) {
      if (poisonedBy) {
        return {
          ok: false,
          milliseconds: 0,
          stdout: Buffer.alloc(0),
          stderr: Buffer.alloc(0),
          stderr_observable: false,
          status: null,
          teardown_clean: false,
          teardown_failures: poisonedBy,
          failure: {
            kind: "runtime-poisoned",
            message: "Zipp runtime was not reused after an earlier teardown failure",
            teardown_failures: poisonedBy,
          },
        };
      }
      let engine;
      let stdout = Buffer.alloc(0);
      let failure = null;
      const teardownFailures = [];
      const started = nowNs();
      try {
        engine = new glue.Engine();
        engine.initScript(source);
        stdout = zippLinesToStdout(engine.takeOutput());
      } catch (error) {
        failure = classifyZippFailure(error);
      } finally {
        if (engine) {
          try { engine.dispose(); } catch (error) {
            teardownFailures.push({ action: "Engine.dispose", message: errorText(error) });
          }
          try { engine.free(); } catch (error) {
            teardownFailures.push({ action: "Engine.free", message: errorText(error) });
          }
        }
      }
      if (teardownFailures.length) {
        poisonedBy = teardownFailures;
        failure = failure
          ? { ...failure, teardown_failures: teardownFailures }
          : {
              kind: "teardown-error",
              message: "Zipp context teardown failed",
              teardown_failures: teardownFailures,
            };
      }
      return {
        ok: failure === null,
        milliseconds: elapsedMs(started),
        stdout,
        stderr: Buffer.alloc(0),
        stderr_observable: false,
        status: failure === null ? 0 : null,
        teardown_clean: teardownFailures.length === 0,
        teardown_failures: teardownFailures,
        failure,
      };
    },
  };
}

function allocateArgv(exports, strings) {
  const encoder = new TextEncoder();
  const allocations = [];
  const stringPointers = [];
  let freed = false;
  const freeAll = () => {
    if (freed) return;
    freed = true;
    const failures = [];
    for (const pointer of [...allocations].reverse()) {
      try { exports.free(pointer); } catch (error) { failures.push(error); }
    }
    if (failures.length) throw new AggregateError(failures, "QuickJS-NG argv cleanup failed");
  };
  try {
    for (const text of strings) {
      const encoded = encoder.encode(`${text}\0`);
      const pointer = exports.malloc(encoded.length) >>> 0;
      if (!pointer) throw new Error("QuickJS-NG malloc failed");
      allocations.push(pointer);
      new Uint8Array(exports.memory.buffer).set(encoded, pointer);
      stringPointers.push(pointer);
    }
    const argv = exports.calloc(strings.length + 1, 4) >>> 0;
    if (!argv) throw new Error("QuickJS-NG calloc failed");
    allocations.push(argv);
    const view = new DataView(exports.memory.buffer);
    stringPointers.forEach((pointer, index) => view.setUint32(argv + index * 4, pointer, true));
    return { argv, free: freeAll };
  } catch (error) {
    try {
      freeAll();
    } catch (cleanupError) {
      throw new AggregateError([error, cleanupError], "QuickJS-NG argv allocation and cleanup failed");
    }
    throw error;
  }
}

async function makeQuickJsRuntime(wasmPath) {
  const module = await WebAssembly.compile(fs.readFileSync(wasmPath));
  const wasi = new WASI({ version: "preview1", args: [], env: {}, preopens: {}, returnOnExit: true });
  let instance;
  let stdoutChunks = [];
  let stderrChunks = [];
  const imports = { ...wasi.wasiImport };
  const originalFdWrite = wasi.wasiImport.fd_write.bind(wasi.wasiImport);
  imports.fd_write = (fd, iovs, iovsLength, writtenPointer) => {
    if (fd !== 1 && fd !== 2) return originalFdWrite(fd, iovs, iovsLength, writtenPointer);
    if (!instance?.exports?.memory) return 8;
    const view = new DataView(instance.exports.memory.buffer);
    const chunks = fd === 1 ? stdoutChunks : stderrChunks;
    let written = 0;
    for (let index = 0; index < iovsLength; index++) {
      const pointer = view.getUint32(iovs + index * 8, true);
      const length = view.getUint32(iovs + index * 8 + 4, true);
      chunks.push(Buffer.from(new Uint8Array(instance.exports.memory.buffer, pointer, length)));
      written += length;
    }
    view.setUint32(writtenPointer, written, true);
    return 0;
  };
  instance = new WebAssembly.Instance(module, { wasi_snapshot_preview1: imports });
  wasi.initialize(instance);
  const exports = instance.exports;
  for (const name of ["malloc", "calloc", "free", "qjs_init_argv", "qjs_destroy"]) {
    if (typeof exports[name] !== "function") throw new Error(`QuickJS-NG reactor lacks ${name}`);
  }
  let poisonedBy = null;
  return {
    module,
    evaluate(source) {
      if (poisonedBy) {
        return {
          ok: false,
          milliseconds: 0,
          stdout: Buffer.alloc(0),
          stderr: Buffer.alloc(0),
          stderr_observable: true,
          status: null,
          teardown_clean: false,
          teardown_failures: poisonedBy,
          failure: {
            kind: "runtime-poisoned",
            message: "QuickJS-NG runtime was not reused after an earlier teardown failure",
            teardown_failures: poisonedBy,
          },
        };
      }
      stdoutChunks = [];
      stderrChunks = [];
      let args;
      let status = null;
      let failure = null;
      let destroyNeeded = false;
      const teardownFailures = [];
      const started = nowNs();
      try {
        args = allocateArgv(exports, ["qjs", "-e", source]);
        destroyNeeded = true;
        status = exports.qjs_init_argv(3, args.argv);
        if (status !== 0) {
          failure = {
            kind: "engine-error",
            limit: null,
            message: `QuickJS-NG qjs_init_argv returned ${status}`,
          };
        }
      } catch (error) {
        failure = { kind: "engine-error", limit: null, message: errorText(error) };
      } finally {
        if (destroyNeeded) {
          try {
            exports.qjs_destroy();
          } catch (error) {
            teardownFailures.push({ action: "qjs_destroy", message: errorText(error) });
          }
        }
        if (args) {
          try { args.free(); } catch (error) {
            teardownFailures.push({ action: "argv.free", message: errorText(error) });
          }
        }
      }
      if (teardownFailures.length) {
        poisonedBy = teardownFailures;
        failure = failure
          ? { ...failure, teardown_failures: teardownFailures }
          : {
              kind: "teardown-error",
              limit: null,
              message: "QuickJS-NG context teardown failed",
              teardown_failures: teardownFailures,
            };
      }
      const stdout = Buffer.concat(stdoutChunks);
      const stderr = Buffer.concat(stderrChunks);
      const milliseconds = elapsedMs(started);
      return {
        ok: failure === null,
        milliseconds,
        stdout,
        stderr,
        stderr_observable: true,
        status,
        teardown_clean: teardownFailures.length === 0,
        teardown_failures: teardownFailures,
        failure,
      };
    },
  };
}

export function captureStatus(result, expectedStdout) {
  const stdout = outputRecord(result.stdout || Buffer.alloc(0));
  const stderr = outputRecord(result.stderr || Buffer.alloc(0));
  const stderrObservable = result.stderr_observable !== false;
  const teardownClean = result.teardown_clean !== false;
  let expectedCanonical;
  try {
    expectedCanonical = canonicalStdout(expectedStdout);
  } catch (error) {
    return {
      valid: false,
      output_exact: false,
      stderr_observable: stderrObservable,
      stderr_empty: stderrObservable ? stderr.bytes === 0 : null,
      teardown_clean: teardownClean,
      teardown_failures: result.teardown_failures || [],
      stdout,
      stderr,
      failure: { kind: "oracle-error", message: errorText(error) },
    };
  }
  const actualCanonical = stdout.canonicalization_error
    ? null
    : canonicalStdout(result.stdout || Buffer.alloc(0));
  const outputExact = actualCanonical !== null && actualCanonical.equals(expectedCanonical);
  const stderrEmpty = stderrObservable ? stderr.bytes === 0 : null;
  const failure = result.failure || (result.ok && stderrEmpty === false
    ? { kind: "unexpected-stderr", message: "engine emitted nonempty stderr" }
    : null);
  return {
    valid: Boolean(result.ok && outputExact && stderrEmpty !== false && teardownClean),
    output_exact: outputExact,
    stderr_observable: stderrObservable,
    stderr_empty: stderrEmpty,
    teardown_clean: teardownClean,
    teardown_failures: result.teardown_failures || [],
    stdout,
    stderr,
    status: result.status,
    failure,
  };
}

function nodeOracle(node, sourcePath, timeoutMs, environment) {
  const started = nowNs();
  const result = spawnSync(node, [sourcePath], {
    encoding: null,
    env: environment,
    timeout: timeoutMs,
    windowsHide: true,
    stdio: ["ignore", "pipe", "pipe"],
  });
  const timedOut = result.error?.code === "ETIMEDOUT";
  const stdout = result.stdout || Buffer.alloc(0);
  const stderr = result.stderr || Buffer.alloc(0);
  const processHealthy = !result.error && result.status === 0;
  const stderrEmpty = stderr.length === 0;
  const healthy = processHealthy && stderrEmpty;
  return {
    ok: healthy,
    milliseconds: elapsedMs(started),
    stdout,
    stderr,
    status: result.status,
    signal: result.signal,
    timed_out: timedOut,
    failure: healthy ? null : processHealthy
      ? { kind: "unexpected-stderr", message: "Node oracle emitted nonempty stderr" }
      : {
        kind: timedOut ? "node-timeout" : "node-error",
        message: result.error ? errorText(result.error) : `Node exited ${result.status}`,
      },
  };
}

function nodeIdentity(node, environment) {
  const expression = "JSON.stringify({execPath:process.execPath,versions:process.versions,platform:process.platform,arch:process.arch})";
  const result = spawnSync(node, ["-p", expression], {
    encoding: "utf8",
    env: environment,
    timeout: 30_000,
    windowsHide: true,
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (result.error || result.status !== 0 || result.stderr) {
    throw new Error(
      `Node oracle identity probe failed: ${result.error ? errorText(result.error) : result.stderr || result.status}`,
    );
  }
  return JSON.parse(result.stdout.trim());
}

async function childCompile(wasmPath) {
  const bytes = fs.readFileSync(wasmPath);
  const started = nowNs();
  const module = await WebAssembly.compile(bytes);
  if (!(module instanceof WebAssembly.Module)) throw new Error("compile did not return a module");
  console.log(JSON.stringify({ milliseconds: elapsedMs(started) }));
}

async function childStartup(engine, wasmPath, gluePath) {
  const module = await WebAssembly.compile(fs.readFileSync(wasmPath));
  if (engine === "zipp") {
    const glue = await importSourceModule(fs.readFileSync(gluePath, "utf8"), "zipp-suite-startup");
    const started = nowNs();
    glue.initSync({ module });
    console.log(JSON.stringify({ milliseconds: elapsedMs(started) }));
    return;
  }
  if (engine === "quickjs-ng") {
    const wasi = new WASI({ version: "preview1", args: [], env: {}, preopens: {}, returnOnExit: true });
    const started = nowNs();
    const instance = new WebAssembly.Instance(module, { wasi_snapshot_preview1: wasi.wasiImport });
    wasi.initialize(instance);
    console.log(JSON.stringify({ milliseconds: elapsedMs(started) }));
    return;
  }
  throw new Error(`unknown startup engine: ${engine}`);
}

function runChild(mode, args, timeoutMs = 120_000, environment = controlledEnvironment().environment) {
  const result = spawnSync(process.execPath, ["--no-warnings", SCRIPT, mode, ...args], {
    encoding: "utf8",
    env: environment,
    timeout: timeoutMs,
    windowsHide: true,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`child ${mode} failed (${result.status}): ${result.stderr || result.stdout}`);
  }
  return JSON.parse(result.stdout.trim().split(/\r?\n/).at(-1));
}

function activeBuildProcesses() {
  const names = new Set([
    "cargo", "rustc", "wasm-opt", "clang", "clang++", "gcc", "g++",
    "cmake", "ninja", "make", "msbuild", "cl", "link", "ld", "lld", "lld-link",
  ]);
  if (process.platform === "win32") {
    const output = execFileSync("tasklist", ["/fo", "csv", "/nh"], { encoding: "utf8" });
    return [...new Set(output.split(/\r?\n/).map(line => {
      const match = line.match(/^"([^"]+)"/);
      return match ? match[1].replace(/\.exe$/i, "").toLowerCase() : "";
    }).filter(name => names.has(name)))];
  }
  const output = execFileSync("ps", ["-A", "-o", "comm="], { encoding: "utf8" });
  return [...new Set(output.split(/\r?\n/)
    .map(name => path.basename(name.trim()).toLowerCase())
    .filter(name => names.has(name)))];
}

function gitMetadata() {
  const invoke = args => {
    const result = spawnSync("git", args, { cwd: ROOT, encoding: "utf8", windowsHide: true });
    return result.status === 0 ? result.stdout.trim() : null;
  };
  return { head: invoke(["rev-parse", "HEAD"]), status_porcelain: invoke(["status", "--short"]) };
}

function stageSources(cases) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "zipp-wasm-suites-"));
  const files = new Map();
  for (const item of cases) {
    const filename = `${item.suite}-${item.id}.js`;
    const destination = path.join(directory, filename);
    fs.writeFileSync(destination, item.sourceBytes, { flag: "wx", mode: 0o444 });
    if (sha256(fs.readFileSync(destination)) !== sha256(item.sourceBytes)) {
      throw new Error(`staged source hash mismatch: ${item.key}`);
    }
    files.set(item.key, destination);
  }
  return {
    directory,
    files,
    verify() {
      return cases.flatMap(item => {
        const current = sha256(fs.readFileSync(files.get(item.key)));
        return current === sha256(item.sourceBytes)
          ? []
          : [`staged source changed: ${item.key}`];
      });
    },
    cleanup() {
      const resolved = path.resolve(directory);
      const temp = path.resolve(os.tmpdir());
      if (path.dirname(resolved) !== temp || !path.basename(resolved).startsWith("zipp-wasm-suites-")) {
        throw new Error(`refusing to clean unexpected staging directory: ${resolved}`);
      }
      for (const pathname of files.values()) fs.unlinkSync(pathname);
      fs.rmdirSync(resolved);
    },
  };
}

function writeJsonAtomic(destination, value, overwrite) {
  const directory = path.dirname(destination);
  fs.mkdirSync(directory, { recursive: true });
  const temporary = path.join(
    directory,
    `.${path.basename(destination)}.${process.pid}.${crypto.randomBytes(8).toString("hex")}.tmp`,
  );
  let descriptor = null;
  try {
    descriptor = fs.openSync(temporary, "wx", 0o600);
    const payload = Buffer.from(`${JSON.stringify(value, null, 2)}\n`, "utf8");
    let offset = 0;
    while (offset < payload.length) {
      offset += fs.writeSync(descriptor, payload, offset, payload.length - offset);
    }
    fs.fsyncSync(descriptor);
    fs.closeSync(descriptor);
    descriptor = null;
    if (overwrite) {
      fs.renameSync(temporary, destination);
    } else {
      // A same-directory hard link publishes the fully synced inode without
      // an exists-check race or a partially visible destination.
      fs.linkSync(temporary, destination);
      fs.unlinkSync(temporary);
    }
  } catch (error) {
    if (descriptor !== null) {
      try { fs.closeSync(descriptor); } catch {}
    }
    if (fs.existsSync(temporary)) {
      try { fs.unlinkSync(temporary); } catch {}
    }
    throw error;
  }
}

function phaseSummary(observations) {
  const output = {};
  for (const engine of ENGINE_NAMES) {
    const samples = observations.filter(item => item.engine === engine).map(item => item.milliseconds);
    output[engine] = { median_ms: median(samples), samples_ms: samples };
  }
  return output;
}

function aggregateCaseRows(rows, expectedCases = rows.length) {
  const crossEngineRows = rows.filter(row => row.cross_engine_supported);
  const comparable = crossEngineRows.filter(row => row.comparable);
  const persistent = comparable
    .map(row => row.zipp_over_quickjs_persistent)
    .filter(value => value !== null);
  const adjusted = comparable
    .map(row => row.zipp_over_quickjs_adjusted)
    .filter(value => value !== null);
  const selectionComplete = rows.length === expectedCases;
  return {
    supported_script_cases: rows.length,
    expected_script_cases: expectedCases,
    cross_engine_supported_cases: crossEngineRows.length,
    engine_support_excluded_cases: rows.length - crossEngineRows.length,
    comparable_cases: comparable.length,
    persistent_ratio_cases: persistent.length,
    adjusted_ratio_cases: adjusted.length,
    available_persistent_geomean: geometricMean(persistent),
    complete_persistent_geomean:
      selectionComplete && crossEngineRows.length > 0 && persistent.length === crossEngineRows.length
        ? geometricMean(persistent) : null,
    available_adjusted_geomean: geometricMean(adjusted),
    complete_adjusted_geomean:
      selectionComplete && crossEngineRows.length > 0 && adjusted.length === crossEngineRows.length
        ? geometricMean(adjusted) : null,
    persistent_point_wins: rows.filter(row =>
      row.zipp_over_quickjs_persistent !== null
      && row.zipp_over_quickjs_persistent < 1).length,
    adjusted_point_wins: rows.filter(row =>
      row.zipp_over_quickjs_adjusted !== null
      && row.zipp_over_quickjs_adjusted < 1).length,
  };
}

function categoryBalancedSummary(categories, fullInventoryCategoryNames = Object.keys(categories)) {
  const entries = Object.entries(categories);
  const rows = entries
    .filter(([, row]) => row.cross_engine_supported_cases > 0)
    .map(([, row]) => row);
  const availablePersistent = rows
    .map(row => row.available_persistent_geomean)
    .filter(value => value !== null);
  const availableAdjusted = rows
    .map(row => row.available_adjusted_geomean)
    .filter(value => value !== null);
  const completePersistent = rows
    .map(row => row.complete_persistent_geomean)
    .filter(value => value !== null);
  const completeAdjusted = rows
    .map(row => row.complete_adjusted_geomean)
    .filter(value => value !== null);
  return {
    category_count: rows.length,
    inventory_category_count: fullInventoryCategoryNames.length,
    full_inventory_category_count: fullInventoryCategoryNames.length,
    supported_script_category_count: entries.length,
    cross_engine_category_count: rows.length,
    non_script_inventory_categories: fullInventoryCategoryNames
      .filter(name => !Object.hasOwn(categories, name)),
    engine_support_excluded_categories: entries
      .filter(([, row]) => row.cross_engine_supported_cases === 0)
      .map(([name]) => name),
    persistent_available_categories: availablePersistent.length,
    adjusted_available_categories: availableAdjusted.length,
    persistent_complete_categories: completePersistent.length,
    adjusted_complete_categories: completeAdjusted.length,
    available_persistent_geomean: geometricMean(availablePersistent),
    complete_persistent_geomean:
      completePersistent.length === rows.length ? geometricMean(completePersistent) : null,
    available_adjusted_geomean: geometricMean(availableAdjusted),
    complete_adjusted_geomean:
      completeAdjusted.length === rows.length ? geometricMean(completeAdjusted) : null,
    persistent_category_wins: availablePersistent.filter(value => value < 1).length,
    adjusted_category_wins: availableAdjusted.filter(value => value < 1).length,
  };
}

export function summarizeExecution(cases, observations, reps) {
  const byCase = {};
  for (const item of cases.filter(item => item.supported)) {
    const engines = {};
    for (const engine of ENGINE_NAMES) {
      const rows = observations.filter(row => row.case === item.key && row.engine === engine && row.valid);
      const work = rows.map(row => row.work_ms);
      const control = rows.map(row => row.control_ms);
      const adjusted = rows.map(row => row.adjusted_ms);
      engines[engine] = {
        samples: rows.length,
        expected_samples: reps,
        complete: rows.length === reps,
        persistent_work_median_ms: median(work),
        persistent_control_median_ms: median(control),
        adjusted_execution_median_ms: median(adjusted),
        positive_adjusted_samples: adjusted.filter(value => value > 0).length,
      };
    }
    const engineSupport = item.engine_support ?? Object.fromEntries(
      ENGINE_NAMES.map(engine => [engine, { status: "supported", reason_code: null, reason: null }]),
    );
    const crossEngineSupported = ENGINE_NAMES.every(engine =>
      (engineSupport[engine]?.status ?? "supported") === "supported");
    const comparable = crossEngineSupported
      && ENGINE_NAMES.every(engine => engines[engine].complete);
    const persistentRatio = comparable
      && engines.zipp.persistent_work_median_ms > 0
      && engines["quickjs-ng"].persistent_work_median_ms > 0
      ? engines.zipp.persistent_work_median_ms / engines["quickjs-ng"].persistent_work_median_ms
      : null;
    const adjustedRatio = comparable
      && engines.zipp.adjusted_execution_median_ms > 0
      && engines["quickjs-ng"].adjusted_execution_median_ms > 0
      ? engines.zipp.adjusted_execution_median_ms / engines["quickjs-ng"].adjusted_execution_median_ms
      : null;
    byCase[item.key] = {
      suite: item.suite,
      id: item.id,
      category: item.category ?? null,
      engine_support: engineSupport,
      cross_engine_supported: crossEngineSupported,
      comparable,
      engines,
      zipp_over_quickjs_persistent: persistentRatio,
      zipp_over_quickjs_adjusted: adjustedRatio,
    };
  }
  const bySuite = {};
  for (const suite of [...new Set(cases.map(item => item.suite))]) {
    const suiteCases = cases.filter(item => item.suite === suite && item.supported);
    const rows = suiteCases.map(item => byCase[item.key]);
    bySuite[suite] = aggregateCaseRows(rows);
    if (suite === "real13") {
      bySuite[suite].retained10 = aggregateCaseRows(
        rows.filter(row => row.category === "headline"),
        REAL_HEADLINE.size,
      );
      bySuite[suite].diagnostic3 = aggregateCaseRows(
        rows.filter(row => row.category === "diagnostic"),
        REAL13.length - REAL_HEADLINE.size,
      );
    }
    if (suite === "hostile") {
      const categoryNames = [...new Set(suiteCases.map(item => item.category))].sort();
      const fullInventoryCategoryNames = [...new Set(
        cases.filter(item => item.suite === suite).map(item => item.category),
      )].sort();
      const categories = Object.fromEntries(categoryNames.map(category => [
        category,
        aggregateCaseRows(rows.filter(row => row.category === category)),
      ]));
      bySuite[suite].categories = categories;
      bySuite[suite].category_balanced = categoryBalancedSummary(
        categories,
        fullInventoryCategoryNames,
      );
    }
  }
  const combinedRows = cases
    .filter(item => item.supported)
    .map(item => byCase[item.key]);
  return {
    by_case: byCase,
    by_suite: bySuite,
    combined_scripts: aggregateCaseRows(combinedRows),
  };
}

export function validationRecord(engine, item, result, expected) {
  const status = captureStatus(result, expected);
  const adapterSupport = item.engine_support?.[engine]
    || { status: "supported", reason_code: null, reason: null };
  if (adapterSupport.status === "unsupported") {
    const expectedHealthyLimitation = Boolean(
      result.ok
      && result.status === 0
      && status.teardown_clean
      && status.stderr_empty !== false
      && status.stdout.bytes === 0
      && status.failure === null
    );
    if (!expectedHealthyLimitation) {
      return {
        engine,
        case: item.key,
        milliseconds: result.milliseconds,
        ...status,
        valid: false,
        adapter_support: adapterSupport,
        raw_engine_ok: Boolean(result.ok),
        raw_engine_failure: result.failure,
      };
    }
    return {
      engine,
      case: item.key,
      milliseconds: result.milliseconds,
      ...status,
      valid: false,
      adapter_support: adapterSupport,
      raw_engine_ok: true,
      raw_engine_failure: null,
      failure: {
        kind: "adapter-unsupported",
        reason_code: adapterSupport.reason_code,
        message: adapterSupport.reason,
        observed_engine_failure: status.failure,
      },
    };
  }
  return {
    engine,
    case: item.key,
    milliseconds: result.milliseconds,
    ...status,
    adapter_support: adapterSupport,
    raw_engine_ok: Boolean(result.ok),
    raw_engine_failure: result.failure,
  };
}

function knownEngineExclusion(item, engine, record) {
  if (!record || record.valid || record.teardown_clean === false) return null;
  const support = item.engine_support?.[engine];
  if (support?.status === "unsupported"
    && record.failure?.kind === "adapter-unsupported"
    && record.failure?.reason_code === support.reason_code
    && record.raw_engine_ok === true
    && record.raw_engine_failure === null
    && record.status === 0
    && record.stdout?.bytes === 0
    && record.stderr_empty !== false) {
    return support.reason_code;
  }
  if (engine === "zipp" && record.failure?.fixed_limit_message_match) {
    return `zipp-fixed-limit-message:${record.failure.fixed_limit_message_match}`;
  }
  return null;
}

function printInventory(inventory) {
  for (const item of inventory.allCases) {
    const selected = inventory.selectedCases.includes(item) ? "selected" : "not-selected";
    const engineSupport = ENGINE_NAMES.map(engine => {
      const support = item.engine_support?.[engine];
      return `${engine}=${support?.status ?? "not-assessed"}${support?.reason_code ? `:${support.reason_code}` : ""}`;
    }).join("\t");
    console.log(
      `${item.key}\t${item.goal}\t${item.support.status}\t${engineSupport}\t${selected}\t${item.entry}`,
    );
  }
}

function printSummary(result) {
  console.log("\nCold module phases (fresh process; median ms)");
  for (const engine of ENGINE_NAMES) {
    const compile = result.summary.compile[engine].median_ms;
    const startup = result.summary.startup[engine].median_ms;
    const ready = compile !== null && startup !== null ? compile + startup : null;
    console.log(
      `  ${engine.padEnd(11)} compile=${compile?.toFixed(3) ?? "n/a"} `
      + `startup=${startup?.toFixed(3) ?? "n/a"} ready=${ready?.toFixed(3) ?? "n/a"}`,
    );
  }
  console.log("\nPersistent module / fresh context suite ratios (Zipp / QuickJS-NG)");
  for (const [suite, row] of Object.entries(result.summary.execution.by_suite)) {
    console.log(
      `  ${suite.padEnd(8)} persistent=${row.complete_persistent_geomean?.toFixed(4) ?? "incomplete"} `
      + `adjusted=${row.complete_adjusted_geomean?.toFixed(4) ?? "incomplete"} `
      + `comparable=${row.comparable_cases}/${row.cross_engine_supported_cases} `
      + `(inventory scripts=${row.supported_script_cases})`,
    );
  }
  if (result.unsupported_cases.length) {
    console.log("\nExplicitly unsupported cases");
    for (const item of result.unsupported_cases) {
      console.log(`  ${item.key}: ${item.support.reason}`);
    }
  }
  if (result.engine_support_exclusions.length) {
    console.log("\nEngine-specific cross-comparison exclusions");
    for (const item of result.engine_support_exclusions) {
      for (const engine of ENGINE_NAMES) {
        const support = item.engine_support?.[engine];
        if (support?.status !== "supported") {
          console.log(`  ${item.key}/${engine}: ${support.reason_code}: ${support.reason}`);
        }
      }
    }
  }
  console.log(`\nRaw evidence: ${result.output_path}`);
}

async function runMain(args) {
  const currentControlEnvironment = controlledEnvironment(process.env);
  const bootstrapRemovedNames = (() => {
    try {
      const parsed = JSON.parse(process.env[CONTROL_ENV_REMOVED] || "[]");
      return Array.isArray(parsed) && parsed.every(name => typeof name === "string") ? parsed : [];
    } catch {
      return [];
    }
  })();
  const removedEnvironmentNames = [...new Set([
    ...bootstrapRemovedNames,
    ...currentControlEnvironment.removed_names,
  ])].sort((left, right) => left.localeCompare(right));
  const environmentControl = {
    policy: "case-insensitive prefix removal; names recorded, values never recorded",
    removed_prefixes: CONTROL_ENV_PREFIXES,
    removed_names: removedEnvironmentNames,
    runner_bootstrap: process.env[CONTROL_ENV_MARKER] || "not-directly-invoked",
    runner_startup_sanitized: currentControlEnvironment.removed_names.length === 0
      && ["already-clean", "reexecuted"].includes(process.env[CONTROL_ENV_MARKER]),
    node_oracle_and_cold_children_sanitized: true,
  };
  const childEnvironment = { ...currentControlEnvironment.environment };
  delete childEnvironment[CONTROL_ENV_MARKER];
  delete childEnvironment[CONTROL_ENV_REMOVED];
  const inventory = loadSuiteInventory({ suite: args.suite, cases: args.cases });
  if (args.list) {
    printInventory(inventory);
    return 0;
  }
  if (fs.existsSync(args.output) && !args.overwrite) {
    throw new Error(`refusing to overwrite ${args.output}; pass --overwrite`);
  }
  const busy = activeBuildProcesses();
  if (busy.length && !args.allowBusy) {
    throw new Error(`refusing to run while build processes are active: ${busy.join(", ")}`);
  }
  for (const pathname of [
    args.node, args.zippWasm, args.zippGlue, args.quickjsWasm, ZIPP_PREAMBLE, ZIPP_WASM_LIB,
  ]) {
    fs.accessSync(pathname, fs.constants.R_OK);
  }
  verifyZippFixedLimits();
  const artifacts = {
    node_oracle_executable: artifact(args.node),
    zipp_wasm: artifact(args.zippWasm),
    zipp_glue: artifact(args.zippGlue),
    zipp_implicit_preamble: artifact(ZIPP_PREAMBLE),
    zipp_wasm_lib_source: artifact(ZIPP_WASM_LIB),
    quickjs_ng_reactor_wasm: artifact(args.quickjsWasm),
  };
  if (artifacts.quickjs_ng_reactor_wasm.sha256 !== QUICKJS_NG_REACTOR_SHA256) {
    throw new Error("QuickJS-NG reactor SHA-256 is not the official v0.16.2 artifact");
  }
  const nodeOracleIdentityBefore = nodeIdentity(args.node, childEnvironment);

  const harnessBefore = sha256(fs.readFileSync(SCRIPT));
  const gitBefore = gitMetadata();
  if (gitBefore.status_porcelain && !args.allowDirty) {
    throw new Error("refusing to measure a dirty worktree; pass --allow-dirty for a diagnostic run");
  }
  const enginePaths = {
    zipp: [args.zippWasm, args.zippGlue],
    "quickjs-ng": [args.quickjsWasm, "-"],
  };
  const compile = [];
  const startup = [];
  if (!args.validationOnly) {
    console.log("Measuring cold module compilation...");
    for (let rep = 0; rep < args.compileReps; rep++) {
      const order = engineOrderForRep(rep, args.seed);
      for (const engine of order) {
        const sample = runChild(
          "__child_compile",
          [enginePaths[engine][0]],
          120_000,
          childEnvironment,
        );
        compile.push({ rep, engine, engine_order: order, milliseconds: sample.milliseconds });
      }
    }
    console.log("Measuring compiled-module startup...");
    for (let rep = 0; rep < args.startupReps; rep++) {
      const order = engineOrderForRep(rep, args.seed ^ 1);
      for (const engine of order) {
        const sample = runChild(
          "__child_startup",
          [engine, ...enginePaths[engine]],
          120_000,
          childEnvironment,
        );
        startup.push({ rep, engine, engine_order: order, milliseconds: sample.milliseconds });
      }
    }
  }

  console.log("Instantiating validation modules...");
  let runtimes = {
    zipp: await makeZippRuntime(args.zippWasm, args.zippGlue),
    "quickjs-ng": await makeQuickJsRuntime(args.quickjsWasm),
  };
  const stage = stageSources(inventory.runnableCases);
  const validation = [];
  const validationByCase = new Map();
  const validationFailures = [];
  const expectedByCase = new Map();
  const controlValidation = {};
  const timedRuntimeControlValidation = {};
  const execution = [];
  const schedules = [];
  const executionFailures = [];
  const empty = "";

  try {
    console.log("Validating empty controls...");
    for (const engine of ENGINE_NAMES) {
      const result = runtimes[engine].evaluate(empty);
      const status = captureStatus(result, Buffer.alloc(0));
      controlValidation[engine] = { engine, milliseconds: result.milliseconds, ...status };
      if (!status.valid) validationFailures.push(`${engine}: empty control validation failed`);
    }

    console.log("Validating exact suite output against Node...");
    for (const item of inventory.runnableCases) {
      const node = nodeOracle(
        args.node,
        stage.files.get(item.key),
        args.nodeTimeoutMs,
        childEnvironment,
      );
      const nodeStatus = {
        engine: "node",
        case: item.key,
        milliseconds: node.milliseconds,
        valid: node.ok,
        output_exact: node.ok,
        stderr_empty: node.stderr.length === 0,
        stdout: outputRecord(node.stdout),
        stderr: outputRecord(node.stderr),
        status: node.status,
        signal: node.signal,
        timed_out: node.timed_out,
        failure: node.failure,
      };
      if (nodeStatus.stdout.canonicalization_error) {
        nodeStatus.valid = false;
        nodeStatus.output_exact = false;
        nodeStatus.failure = {
          kind: "node-stdout-canonicalization",
          message: nodeStatus.stdout.canonicalization_error,
        };
      }
      validation.push(nodeStatus);
      if (!nodeStatus.valid) {
        validationFailures.push(`${item.key}: Node oracle failed`);
        validationByCase.set(item.key, { node: nodeStatus });
        continue;
      }
      if (node.stdout.length === 0) {
        nodeStatus.valid = false;
        nodeStatus.failure = { kind: "node-empty-output", message: "frozen workload produced empty stdout" };
        validationFailures.push(`${item.key}: Node oracle produced empty stdout`);
        validationByCase.set(item.key, { node: nodeStatus });
        continue;
      }
      expectedByCase.set(item.key, node.stdout);
      const statuses = { node: nodeStatus };
      for (const engine of ENGINE_NAMES) {
        const result = runtimes[engine].evaluate(item.sourceText);
        const record = validationRecord(engine, item, result, node.stdout);
        validation.push(record);
        statuses[engine] = record;
        if (!record.valid) {
          const detail = record.failure?.reason_code
            || (record.failure?.fixed_limit_message_match
            ? `untyped-limit-message:${record.failure.fixed_limit_message_match}`
            : record.failure?.kind || (record.output_exact ? "engine-error" : "stdout-mismatch"));
          validationFailures.push(`${item.key}/${engine}: ${detail}`);
        }
      }
      validationByCase.set(item.key, statuses);
    }

    if (!args.validationOnly) {
      console.log("Instantiating fresh persistent modules for timed execution...");
      runtimes = {
        zipp: await makeZippRuntime(args.zippWasm, args.zippGlue),
        "quickjs-ng": await makeQuickJsRuntime(args.quickjsWasm),
      };
      console.log("Validating timed-runtime empty controls...");
      for (const engine of ENGINE_NAMES) {
        const control = runtimes[engine].evaluate(empty);
        const status = captureStatus(control, Buffer.alloc(0));
        timedRuntimeControlValidation[engine] = {
          engine,
          milliseconds: control.milliseconds,
          ...status,
        };
        if (!status.valid) {
          executionFailures.push(`${engine}: timed-runtime empty control validation failed`);
        }
      }

      console.log("Measuring paired full-source and empty-control evaluations...");
      const baseCaseOrder = seededShuffle(
        inventory.runnableCases.map(item => item.key),
        args.seed ^ 0x9e3779b9,
      );
      const byKey = new Map(inventory.runnableCases.map(item => [item.key, item]));
      for (let rep = 0; rep < args.reps; rep++) {
        const caseOrder = caseOrderForRep(baseCaseOrder, rep);
        const engineOrder = engineOrderForRep(rep, args.seed);
        schedules.push({ rep, case_order: caseOrder, engine_order: engineOrder });
        for (let casePosition = 0; casePosition < caseOrder.length; casePosition++) {
          const caseKey = caseOrder[casePosition];
          const item = byKey.get(caseKey);
          const expected = expectedByCase.get(caseKey);
          const validated = validationByCase.get(caseKey);
          for (let enginePosition = 0; enginePosition < engineOrder.length; enginePosition++) {
            const engine = engineOrder[enginePosition];
            const pairOrder = pairOrderFor(rep, engine);
            if (!expected
              || !validated?.[engine]?.valid
              || !controlValidation[engine]?.valid
              || !timedRuntimeControlValidation[engine]?.valid) {
              execution.push({
                rep,
                case: caseKey,
                case_position: casePosition,
                engine,
                engine_position: enginePosition,
                engine_order: engineOrder,
                pair_order: pairOrder,
                skipped: true,
                valid: false,
                skip_reason: "validation-failed",
                validation_failure: validated?.[engine]?.failure ?? null,
                timed_runtime_control_failure:
                  timedRuntimeControlValidation[engine]?.failure ?? null,
              });
              continue;
            }
            const measured = {};
            for (const kind of pairOrder) {
              const source = kind === "work" ? item.sourceText : empty;
              const wanted = kind === "work" ? expected : Buffer.alloc(0);
              const result = runtimes[engine].evaluate(source);
              measured[kind] = {
                milliseconds: result.milliseconds,
                ...captureStatus(result, wanted),
              };
            }
            const valid = measured.work.valid && measured.control.valid;
            if (!valid) executionFailures.push(`${caseKey}/${engine}/rep-${rep}: invalid observation`);
            execution.push({
              rep,
              case: caseKey,
              case_position: casePosition,
              engine,
              engine_position: enginePosition,
              engine_order: engineOrder,
              pair_order: pairOrder,
              skipped: false,
              valid,
              work_ms: measured.work.milliseconds,
              control_ms: measured.control.milliseconds,
              adjusted_ms: measured.work.milliseconds - measured.control.milliseconds,
              work: measured.work,
              control: measured.control,
            });
          }
        }
        console.log(`  rep ${rep + 1}/${args.reps} complete`);
      }
    }

    const declaredInputVerification = verifyDeclaredInputsUnchanged(inventory.allCases);
    const originalDrift = declaredInputVerification.drift;
    const stagedDrift = stage.verify();
    const manifestDrift = inventory.manifest
      && sha256(fs.readFileSync(HOSTILE_MANIFEST)) !== inventory.manifest.sha256
      ? ["hostile manifest changed during run"]
      : [];
    const sourceDrift = [...originalDrift, ...stagedDrift, ...manifestDrift];
    const harnessAfter = sha256(fs.readFileSync(SCRIPT));
    const gitAfter = gitMetadata();
    const artifactsAfter = {
      node_oracle_executable: artifact(args.node),
      zipp_wasm: artifact(args.zippWasm),
      zipp_glue: artifact(args.zippGlue),
      zipp_implicit_preamble: artifact(ZIPP_PREAMBLE),
      zipp_wasm_lib_source: artifact(ZIPP_WASM_LIB),
      quickjs_ng_reactor_wasm: artifact(args.quickjsWasm),
    };
    const nodeOracleIdentityAfter = nodeIdentity(args.node, childEnvironment);
    const artifactDrift = Object.keys(artifacts).flatMap(name =>
      artifacts[name].sha256 === artifactsAfter[name].sha256
        ? []
        : [`artifact changed: ${name}`]);
    // Summaries always retain the frozen suite denominators. Diagnostic
    // selectors therefore leave unselected rows visibly incomplete rather
    // than making a subset look like a complete suite result.
    const executionSummary = summarizeExecution(inventory.allCases, execution, args.reps);
    const supportedCases = inventory.runnableCases.length;
    const selectedByKey = new Map(inventory.runnableCases.map(item => [item.key, item]));
    const nodeOraclesValid = inventory.runnableCases.every(item =>
      validationByCase.get(item.key)?.node?.valid);
    const validationControlsValid = ENGINE_NAMES.every(engine => controlValidation[engine]?.valid);
    const timedRuntimeControlsValid = args.validationOnly
      ? null
      : ENGINE_NAMES.every(engine => timedRuntimeControlValidation[engine]?.valid);
    const controlsValid = validationControlsValid && (timedRuntimeControlsValid ?? true);
    const validationCaptureComplete = inventory.runnableCases.every(item => {
      const statuses = validationByCase.get(item.key);
      return statuses?.node && ENGINE_NAMES.every(engine => statuses?.[engine]);
    });
    const allValidationCorrect = nodeOraclesValid
      && inventory.runnableCases.every(item => {
        const statuses = validationByCase.get(item.key);
        return ENGINE_NAMES.every(engine => statuses?.[engine]?.valid);
      })
      && controlsValid;
    const executionAttemptComplete = args.validationOnly
      ? null
      : inventory.runnableCases.every(item => ENGINE_NAMES.every(engine =>
        execution.filter(row => row.case === item.key && row.engine === engine).length === args.reps));
    const coldCaptureComplete = args.validationOnly
      ? null
      : ENGINE_NAMES.every(engine =>
        compile.filter(row => row.engine === engine).length === args.compileReps
        && startup.filter(row => row.engine === engine).length === args.startupReps);
    const captureAttemptComplete = validationCaptureComplete
      && (args.validationOnly || (executionAttemptComplete && coldCaptureComplete));
    const executionComplete = args.validationOnly
      ? null
      : inventory.runnableCases.every(item => ENGINE_NAMES.every(engine =>
          execution.filter(row => row.case === item.key && row.engine === engine && row.valid).length === args.reps));
    const aggregateEligibleCases = inventory.runnableCases.filter(item =>
      ENGINE_NAMES.every(engine => item.engine_support?.[engine]?.status === "supported"));
    const aggregateComplete = args.validationOnly
      ? null
      : aggregateEligibleCases.every(item => ENGINE_NAMES.every(engine =>
        execution.filter(row => row.case === item.key && row.engine === engine && row.valid).length === args.reps));
    const knownExclusions = [];
    const unexpectedValidationFailures = [];
    for (const item of inventory.runnableCases) {
      const statuses = validationByCase.get(item.key);
      for (const engine of ENGINE_NAMES) {
        const record = statuses?.[engine];
        if (record?.valid) continue;
        const exclusion = knownEngineExclusion(item, engine, record);
        const description = `${item.key}/${engine}`;
        if (exclusion) knownExclusions.push(`${description}: ${exclusion}`);
        else unexpectedValidationFailures.push(description);
      }
    }
    const unexpectedExecutionFailures = [];
    if (!args.validationOnly) {
      for (const row of execution.filter(item => !item.valid)) {
        const item = selectedByKey.get(row.case);
        if (row.skipped) {
          const record = validationByCase.get(row.case)?.[row.engine];
          const exclusion = item && knownEngineExclusion(item, row.engine, record);
          if (!exclusion) unexpectedExecutionFailures.push(`${row.case}/${row.engine}/rep-${row.rep}: skipped`);
          continue;
        }
        const invalidParts = ["work", "control"].filter(kind => !row[kind]?.valid);
        const exclusions = invalidParts.map(kind =>
          item && knownEngineExclusion(item, row.engine, row[kind]));
        if (!invalidParts.length || exclusions.some(exclusion => !exclusion)) {
          unexpectedExecutionFailures.push(`${row.case}/${row.engine}/rep-${row.rep}: invalid`);
        }
      }
    }

    const result = {
      schema_version: 1,
      diagnostic_only: true,
      publishable: false,
      generated_at_utc: new Date().toISOString(),
      output_path: args.output,
      methodology: {
        source: "identical exact frozen guest file bytes decoded with strict UTF-8; embedded NUL rejected; no downscaling or rewriting",
        completion_suppression: "not applied",
        oracle: "Node stdout with empty stderr required; observable engine stdout/stderr is validated; comparison canonicalizes CRLF to LF only and rejects lone CR; raw bytes retained",
        zipp_output_channels: (
          "Engine.takeOutput() merges VM stdout and errput into one ordered line array; it is compared to "
          + "Node stdout, but Zipp stderr emptiness cannot be asserted independently (the frozen tests emit no stderr)"
        ),
        persistent: "one live WASM module instance per engine; each work/control evaluation creates and tears down a fresh guest context",
        timed_runtime_freshness: (
          "validation uses separate module instances; timed execution creates fresh persistent instances, "
          + "preconditions each only with one validated empty control, then retains it for the full schedule"
        ),
        control: "separate empty source paired with every exact workload; work/control order alternates by engine and repetition",
        order: (
          "engine and work/control order alternate every repetition; seeded cases use paired forward/reverse "
          + "rotations so each even/odd pair balances every case across complementary early/late positions"
        ),
        compile: "fresh Node process per sample; file reading excluded; WebAssembly.compile included",
        startup: "fresh Node process per sample; module compilation/glue parsing excluded; instantiation and Wasm start included",
        quickjs_output_timing: "fd_write byte copying and final Buffer.concat result marshalling are inside each measured evaluation",
        zipp_implicit_preamble: (
          "Engine.initScript internally prepends the production crates/zipp-wasm/src/preamble.js; "
          + "the guest workload bytes are unchanged and the preamble hash is captured as an implicit adapter input"
        ),
        zipp_production_limits: (
          "Engine.initScript uses the compile-time production limits recorded in provenance.zipp_fixed_limits; "
          + "the wasm API exposes no host override"
        ),
        zipp_limit_failures: (
          "raw embedding errors are retained; exact known limit-message matches are advisory only "
          + "because this wasm-bindgen API does not export the VM's typed limit status"
        ),
        quickjs_reactor_job_drain: (
          "the pinned official reactor evaluates and returns without js_std_loop/JS_ExecutePendingJob and "
          + "exports no drain function; the three known async cases remain inventoried and raw validation is "
          + "retained, but they are explicitly excluded from cross-engine aggregate denominators"
        ),
        aggregate_denominators: (
          "real13 cross-engine completeness is 12/13 (retained10 9/10; diagnostic3 3/3); "
          + "hostile scripts are 13/15; complete_* fields mean complete over explicitly cross-engine-supported rows"
        ),
        wasm_evaluation_timeout: (
          "none: manifest timeout_s is provenance only and cannot interrupt synchronous in-process WASM; "
          + "a hung guest can hang this diagnostic runner"
        ),
        uncertainty: "raw samples and medians/geomeans only; no confidence intervals or universal significance claims",
        scope: "adapter-inclusive diagnostic; host interfaces and bundled features are not equivalent",
      },
      configuration: {
        suite: args.suite,
        selectors: args.cases,
        complete_selection: inventory.completeSelection,
        reps: args.reps,
        compile_reps: args.compileReps,
        startup_reps: args.startupReps,
        seed: args.seed,
        validation_only: args.validationOnly,
        allow_dirty: args.allowDirty,
        allow_busy: args.allowBusy,
        node_oracle_path: args.node,
        node_timeout_ms: args.nodeTimeoutMs,
      },
      environment: {
        node: process.version,
        v8: process.versions.v8,
        node_oracle_before: nodeOracleIdentityBefore,
        node_oracle_after: nodeOracleIdentityAfter,
        platform: process.platform,
        release: os.release(),
        arch: process.arch,
        cpu: os.cpus()[0]?.model ?? null,
        logical_cpus: os.cpus().length,
        process_exec_argv: process.execArgv,
        controlled_environment: environmentControl,
        busy_processes_at_start: busy,
        git_before: gitBefore,
        git_after: gitAfter,
      },
      provenance: {
        quickjs_ng: {
          version: QUICKJS_NG_VERSION,
          commit: QUICKJS_NG_COMMIT,
          reactor_async_limitation: {
            reason_code: "quickjs-reactor-no-job-drain",
            affected_cases: [...QUICKJS_REACTOR_NO_JOB_DRAIN],
          },
        },
        zipp_fixed_limits: ZIPP_FIXED_LIMITS,
        harness_sha256_before: harnessBefore,
        harness_sha256_after: harnessAfter,
        hostile_manifest: inventory.manifest,
        declared_input_files_rehashed: declaredInputVerification.checked_files,
      },
      artifacts_before: artifacts,
      artifacts_after: artifactsAfter,
      artifacts,
      inventory: inventory.allCases.map(serializableCase),
      selected_cases: inventory.selectedCases.map(item => item.key),
      unsupported_cases: inventory.unsupportedCases.map(serializableCase),
      engine_support_exclusions: inventory.allCases
        .filter(item => item.supported && !ENGINE_NAMES.every(engine =>
          item.engine_support?.[engine]?.status === "supported"))
        .map(serializableCase),
      control_validation: controlValidation,
      timed_runtime_control_validation: timedRuntimeControlValidation,
      validation,
      observations: { compile, startup, execution },
      schedules,
      summary: {
        compile: phaseSummary(compile),
        startup: phaseSummary(startup),
        execution: executionSummary,
      },
      status: {
        supported_script_cases: supportedCases,
        selected_supported_script_cases: supportedCases,
        frozen_inventory_supported_script_cases: inventory.allCases.filter(item => item.supported).length,
        cross_engine_supported_script_cases: inventory.runnableCases.filter(item =>
          ENGINE_NAMES.every(engine => item.engine_support?.[engine]?.status === "supported")).length,
        selected_cross_engine_supported_script_cases: inventory.runnableCases.filter(item =>
          ENGINE_NAMES.every(engine => item.engine_support?.[engine]?.status === "supported")).length,
        frozen_inventory_cross_engine_supported_script_cases: inventory.allCases.filter(item =>
          item.supported && ENGINE_NAMES.every(engine =>
            item.engine_support?.[engine]?.status === "supported")).length,
        engine_specific_unsupported_script_cases: inventory.runnableCases.filter(item =>
          !ENGINE_NAMES.every(engine => item.engine_support?.[engine]?.status === "supported")).length,
        unsupported_manifest_cases: inventory.unsupportedCases.length,
        all_validation_output_exact: allValidationCorrect,
        node_oracles_valid: nodeOraclesValid,
        controls_valid: controlsValid,
        validation_controls_valid: validationControlsValid,
        timed_runtime_controls_valid: timedRuntimeControlsValid,
        validation_capture_complete: validationCaptureComplete,
        cold_capture_complete: coldCaptureComplete,
        execution_attempt_complete: executionAttemptComplete,
        capture_attempt_complete: captureAttemptComplete,
        execution_complete: executionComplete,
        aggregate_complete: aggregateComplete,
        all_correct: allValidationCorrect && (executionComplete ?? true),
        source_drift: sourceDrift,
        artifact_drift: artifactDrift,
        validation_failures: validationFailures,
        execution_failures: executionFailures,
        known_exclusions: knownExclusions,
        unexpected_validation_failures: unexpectedValidationFailures,
        unexpected_execution_failures: unexpectedExecutionFailures,
        harness_unchanged: harnessBefore === harnessAfter,
        git_head_unchanged: gitBefore.head === gitAfter.head,
        git_status_unchanged: gitBefore.status_porcelain === gitAfter.status_porcelain,
        node_oracle_identity_unchanged:
          JSON.stringify(nodeOracleIdentityBefore) === JSON.stringify(nodeOracleIdentityAfter),
      },
    };
    result.status.capture_integrity = (
      result.status.source_drift.length === 0
      && result.status.artifact_drift.length === 0
      && result.status.harness_unchanged
      && result.status.git_head_unchanged
      && result.status.git_status_unchanged
      && result.status.node_oracle_identity_unchanged
    );
    result.status.capture_usable = Boolean(
      !args.validationOnly
      && inventory.completeSelection
      && !args.allowDirty
      && !args.allowBusy
      && busy.length === 0
      && gitBefore.status_porcelain === ""
      && gitAfter.status_porcelain === ""
      && environmentControl.runner_startup_sanitized
      && result.status.node_oracles_valid
      && result.status.controls_valid
      && result.status.capture_attempt_complete
      && result.status.capture_integrity
      && result.status.unexpected_validation_failures.length === 0
      && result.status.unexpected_execution_failures.length === 0
    );
    result.status.evidence_usable = result.status.capture_usable;
    result.status.comparison_publishable = Boolean(
      result.status.capture_usable && result.status.aggregate_complete
    );
    result.publishable = result.status.comparison_publishable;
    writeJsonAtomic(args.output, result, args.overwrite);
    printSummary(result);
    return result.status.capture_integrity
      && (result.status.all_correct || result.status.capture_usable) ? 0 : 1;
  } finally {
    stage.cleanup();
  }
}

function bootstrapControlledRunner() {
  if (["already-clean", "reexecuted"].includes(process.env[CONTROL_ENV_MARKER])) return null;
  const controlled = controlledEnvironment(process.env);
  if (controlled.removed_names.length === 0) {
    process.env[CONTROL_ENV_MARKER] = "already-clean";
    process.env[CONTROL_ENV_REMOVED] = "[]";
    return null;
  }
  const environment = {
    ...controlled.environment,
    [CONTROL_ENV_MARKER]: "reexecuted",
    [CONTROL_ENV_REMOVED]: JSON.stringify(controlled.removed_names),
  };
  const child = spawnSync(
    process.execPath,
    ["--no-warnings", SCRIPT, ...process.argv.slice(2)],
    { env: environment, stdio: "inherit", windowsHide: true },
  );
  if (child.error) throw child.error;
  return Number.isInteger(child.status) ? child.status : 1;
}

const invokedDirectly = process.argv[1] && path.resolve(process.argv[1]) === SCRIPT;
if (invokedDirectly) {
  try {
    const bootstrapStatus = bootstrapControlledRunner();
    if (bootstrapStatus !== null) {
      process.exitCode = bootstrapStatus;
    } else if (process.argv[2] === "__child_compile") {
      await childCompile(path.resolve(process.argv[3]));
    } else if (process.argv[2] === "__child_startup") {
      await childStartup(process.argv[3], path.resolve(process.argv[4]), path.resolve(process.argv[5]));
    } else {
      const args = parseArgs(process.argv.slice(2));
      if (args) process.exitCode = await runMain(args);
    }
  } catch (error) {
    console.error(error?.stack || String(error));
    process.exitCode = 1;
  }
}
