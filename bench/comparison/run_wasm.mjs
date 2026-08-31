#!/usr/bin/env node

/**
 * Reproducible WASM diagnostic for Zipp, QuickJS-NG, and Boa.
 *
 * This is deliberately separate from the project's canonical benchmark. The
 * public APIs are not equivalent: Zipp exposes a persistent Engine, while the
 * official QuickJS-NG reactor and Boa package create a fresh JS context for an
 * evaluation. Cross-engine execution is therefore estimated with paired,
 * byte-matched work/control programs inside persistent WASM instances.
 */

import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import { execFileSync, spawnSync } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";
import { WASI } from "node:wasi";

const SCRIPT = fileURLToPath(import.meta.url);
const ROOT = path.resolve(path.dirname(SCRIPT), "..", "..");
const DEFAULT_SEED = 0x5a172026;
const QUICKJS_NG_VERSION = "0.16.2";
const QUICKJS_NG_COMMIT = "1ab8676f4b6d6d669baeb5f21790fb9734636a20";
const QUICKJS_NG_REACTOR_SHA256 =
  "fc638ef0bad35edb860ca93fe5c0ea288a6ad137888b34afa8ca2c2513727cf0";
const BOA_VERSION = "0.22.0";
const BOA_COMMIT = "337a3668a0dc86dd401ea20906e782249a64a228";
const BOA_WASM_SHA256 =
  "03a3e4c1c0e71514cb28d2158ea52566dbbfbefe16fee795480a751e9b6b5f31";
const ZIPP_FINGERPRINT_METHOD = "getGlobalsFingerprint";
const ZIPP_FINGERPRINT_EXPORT = "engine_getGlobalsFingerprint";

const ENGINE_NAMES = ["zipp", "quickjs-ng", "boa"];
const PERMUTATIONS = [
  ["zipp", "quickjs-ng", "boa"],
  ["zipp", "boa", "quickjs-ng"],
  ["quickjs-ng", "zipp", "boa"],
  ["quickjs-ng", "boa", "zipp"],
  ["boa", "zipp", "quickjs-ng"],
  ["boa", "quickjs-ng", "zipp"],
];

// These preserve the operations in bench/long while using WASM-friendly
// scales. Each generated pair has the same byte length and AST; only the
// single-byte mode literal differs (0 = control, 1 = work).
const WORKLOADS = [
  {
    name: "fib-recursive",
    fixture: "bench/long/fib.js",
    workExpected: "832040",
    controlExpected: "1",
    body: `
function __wasm_cmp_fib(n) {
  return n < 2 ? n : __wasm_cmp_fib(n - 1) + __wasm_cmp_fib(n - 2);
}
function __wasm_cmp_run(mode) {
  return String(__wasm_cmp_fib(1 + mode * 29));
}`,
  },
  {
    name: "loop-arithmetic",
    fixture: "bench/long/loop.js",
    workExpected: "2000001000000",
    controlExpected: "0",
    body: `
function __wasm_cmp_run(mode) {
  let n = mode * 2000000;
  let total = 0;
  let i = 1;
  while (i <= n) {
    total = total + i;
    i = i + 1;
  }
  return String(total);
}`,
  },
  {
    name: "array-hof",
    fixture: "bench/long/array.js",
    workExpected: "3333366666",
    controlExpected: "0",
    body: `
function __wasm_cmp_run(mode) {
  let n = mode * 100000;
  let a = [];
  for (let i = 0; i < n; i++) a.push(i);
  let r = a.map(x => x * 2).filter(x => x % 3 === 0).reduce((p, c) => p + c, 0);
  return String(r);
}`,
  },
  {
    name: "object-properties",
    fixture: "bench/long/object.js",
    workExpected: "250000500000",
    controlExpected: "0",
    body: `
function __wasm_cmp_run(mode) {
  let n = mode * 500000;
  let o = { a: 0, b: 0, c: 0 };
  let s = 0;
  for (let i = 0; i < n; i++) {
    o.a = i;
    o.b = o.a + 1;
    o.c = o.b * 2;
    s += o.c;
  }
  return String(s);
}`,
  },
  {
    name: "sort-comparator",
    fixture: "bench/long/sort.js",
    workExpected: "0,25000,49999",
    controlExpected: "0",
    body: `
function __wasm_cmp_run(mode) {
  let n = mode * 50000;
  let a = [];
  for (let i = 0; i < n; i++) a.push((i * 7919) % n);
  a.sort((x, y) => x - y);
  if (n === 0) return "0";
  return String(a[0]) + "," + String(a[n / 2]) + "," + String(a[n - 1]);
}`,
  },
];

function usage() {
  console.log(`usage: node --no-warnings bench/comparison/run_wasm.mjs [options]

Options:
  --reps N             paired execution repetitions (multiple of 6; default 6)
  --compile-reps N     cold module compile repetitions (default 6)
  --startup-reps N     compiled-module startup repetitions (default 6)
  --seed N             deterministic schedule seed (default ${DEFAULT_SEED})
  --output PATH        raw JSON output (default target/comparison/results/wasm-final.json)
  --zipp-wasm PATH     final Zipp .wasm
  --zipp-glue PATH     matching Zipp web glue
  --quickjs-wasm PATH  official QuickJS-NG WASI reactor
  --boa-wasm PATH      official @boa-dev/boa_wasm module
  --boa-glue PATH      matching boa_wasm_bg.js
  --allow-busy         override the build-process safety check
  --help               show this text

Reported phases:
  compile              bytes already read; fresh Node/V8 process per sample
  startup              compiled module -> initialized WASM instance
  persistent total     fresh JS context/evaluation in an already-live module
  adjusted execution   paired work - byte-matched zero-work control

The adjusted number is a diagnostic estimate, not a claim that the three host
interfaces or feature sets are equivalent.`);
}

function parseArgs(argv) {
  const defaults = {
    reps: 6,
    compileReps: 6,
    startupReps: 6,
    seed: DEFAULT_SEED,
    output: path.join(ROOT, "target", "comparison", "results", "wasm-final.json"),
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
    boaWasm: path.join(
      ROOT,
      "target",
      "comparison",
      "bin",
      "boa-wasm",
      "unpacked",
      "package",
      "boa_wasm_bg.wasm",
    ),
    boaGlue: path.join(
      ROOT,
      "target",
      "comparison",
      "bin",
      "boa-wasm",
      "unpacked",
      "package",
      "boa_wasm_bg.js",
    ),
    allowBusy: false,
  };
  const valueOptions = new Map([
    ["--reps", ["reps", Number]],
    ["--compile-reps", ["compileReps", Number]],
    ["--startup-reps", ["startupReps", Number]],
    ["--seed", ["seed", Number]],
    ["--output", ["output", String]],
    ["--zipp-wasm", ["zippWasm", String]],
    ["--zipp-glue", ["zippGlue", String]],
    ["--quickjs-wasm", ["quickjsWasm", String]],
    ["--boa-wasm", ["boaWasm", String]],
    ["--boa-glue", ["boaGlue", String]],
  ]);
  for (let index = 0; index < argv.length; index++) {
    const option = argv[index];
    if (option === "--help") {
      usage();
      process.exit(0);
    }
    if (option === "--allow-busy") {
      defaults.allowBusy = true;
      continue;
    }
    const spec = valueOptions.get(option);
    if (!spec || index + 1 >= argv.length) {
      throw new Error(`unknown or incomplete option: ${option}`);
    }
    const [key, convert] = spec;
    defaults[key] = convert(argv[++index]);
  }
  for (const key of ["reps", "compileReps", "startupReps"]) {
    if (!Number.isInteger(defaults[key]) || defaults[key] <= 0) {
      throw new Error(`${key} must be a positive integer`);
    }
  }
  if (defaults.reps % PERMUTATIONS.length !== 0) {
    throw new Error("--reps must be a multiple of 6 for complete order balance");
  }
  if (!Number.isSafeInteger(defaults.seed)) {
    throw new Error("--seed must be a safe integer");
  }
  for (const key of ["output", "zippWasm", "zippGlue", "quickjsWasm", "boaWasm", "boaGlue"]) {
    defaults[key] = path.resolve(defaults[key]);
  }
  return defaults;
}

function sha256(bytes) {
  return crypto.createHash("sha256").update(bytes).digest("hex");
}

function artifact(pathname) {
  const bytes = fs.readFileSync(pathname);
  return { path: pathname, bytes: bytes.length, sha256: sha256(bytes) };
}

function nowNs() {
  return process.hrtime.bigint();
}

function elapsedMs(started) {
  return Number(nowNs() - started) / 1e6;
}

function median(values) {
  if (values.length === 0) throw new Error("median of empty sample");
  const sorted = [...values].sort((a, b) => a - b);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2;
}

function geometricMean(values) {
  if (values.length === 0 || values.some(value => !(value > 0))) return null;
  return Math.exp(values.reduce((sum, value) => sum + Math.log(value), 0) / values.length);
}

function seededShuffle(values, seed) {
  const result = [...values];
  let state = seed >>> 0;
  const random = () => {
    state ^= state << 13;
    state ^= state >>> 17;
    state ^= state << 5;
    return (state >>> 0) / 0x100000000;
  };
  for (let index = result.length - 1; index > 0; index--) {
    const other = Math.floor(random() * (index + 1));
    [result[index], result[other]] = [result[other], result[index]];
  }
  return result;
}

function generatedSource(workload, mode) {
  const source = `${workload.body.trim()}\n
let __wasm_cmp_mode = ${mode};
let __wasm_cmp_actual = __wasm_cmp_run(__wasm_cmp_mode);
let __wasm_cmp_expected = __wasm_cmp_mode ? ${JSON.stringify(workload.workExpected)} : ${JSON.stringify(workload.controlExpected)};
if (__wasm_cmp_actual !== __wasm_cmp_expected) {
  throw new Error("wasm comparison result mismatch: " + __wasm_cmp_actual + " !== " + __wasm_cmp_expected);
}
__wasm_cmp_actual;\n`;
  return source;
}

function sourcesFor(workload) {
  const control = generatedSource(workload, 0);
  const work = generatedSource(workload, 1);
  if (Buffer.byteLength(control) !== Buffer.byteLength(work)) {
    throw new Error(`${workload.name}: work/control source sizes differ`);
  }
  return {
    control,
    work,
    controlExpected: workload.controlExpected,
    workExpected: workload.workExpected,
  };
}

let moduleNonce = 0;
async function importSourceModule(source, label) {
  const tagged = `${source}\n// ${label} ${moduleNonce++}\n`;
  const url = `data:text/javascript;base64,${Buffer.from(tagged).toString("base64")}`;
  return import(url);
}

function importsFromNamespace(module, namespace) {
  const imports = Object.create(null);
  for (const item of WebAssembly.Module.imports(module)) {
    if (item.kind !== "function") {
      throw new Error(`unsupported ${item.module}.${item.name} ${item.kind} import`);
    }
    const value = namespace[item.name];
    if (typeof value !== "function") {
      throw new Error(`glue does not export ${item.module}.${item.name}`);
    }
    (imports[item.module] ??= Object.create(null))[item.name] = value;
  }
  return imports;
}

function requireZippFingerprintBindings(module, glue) {
  const rawExport = WebAssembly.Module.exports(module).find(
    entry => entry.kind === "function" && entry.name === ZIPP_FINGERPRINT_EXPORT,
  );
  if (!rawExport) {
    throw new Error(
      `Zipp preflight failed: raw WASM export ${ZIPP_FINGERPRINT_EXPORT} is missing; `
      + "the module predates the v0.0.4 fingerprint API",
    );
  }
  if (typeof glue.Engine?.prototype?.[ZIPP_FINGERPRINT_METHOD] !== "function") {
    throw new Error(
      `Zipp preflight failed: Engine.${ZIPP_FINGERPRINT_METHOD} is missing; `
      + "the generated glue is stale or does not match the module",
    );
  }
  return {
    engine_method: ZIPP_FINGERPRINT_METHOD,
    raw_wasm_export: ZIPP_FINGERPRINT_EXPORT,
  };
}

function requireFingerprintArray(label, value, length) {
  if (!Array.isArray(value) || value.length !== length) {
    throw new Error(`${label}: expected an Array of length ${length}, got ${JSON.stringify(value)}`);
  }
  if (!value.every(cell => Number.isSafeInteger(cell) && cell >= 0)) {
    throw new Error(`${label}: expected exact non-negative JS integers, got ${JSON.stringify(value)}`);
  }
}

async function preflightZippFingerprintContract(wasmPath, gluePath) {
  const module = await WebAssembly.compile(fs.readFileSync(wasmPath));
  const glue = await importSourceModule(
    fs.readFileSync(gluePath, "utf8"),
    "zipp-fingerprint-preflight",
  );
  const bindings = requireZippFingerprintBindings(module, glue);
  glue.initSync({ module });

  const source = `
let __zipp_fp_state = { items: [1, 2], nested: { ok: true } };
let __zipp_fp_scalar = 7;
function __zipp_fp_mutate() { __zipp_fp_state.items.push(3); }
function __zipp_fp_restore() {
  __zipp_fp_state = { items: [1, 2], nested: { ok: true } };
}
`;
  let engine;
  try {
    engine = new glue.Engine();
    const symbols = engine.initScript(source);
    const state = symbols.__zipp_fp_state;
    const scalar = symbols.__zipp_fp_scalar;
    if (!state || !scalar || !Number.isInteger(state.index) || !Number.isInteger(scalar.index)) {
      throw new Error("Zipp fingerprint preflight did not expose its probe globals");
    }
    const indices = [state.index, scalar.index];
    const initial = engine.getGlobalsFingerprint(indices);
    requireFingerprintArray("initial Zipp fingerprints", initial, indices.length);
    const unchanged = engine.getGlobalsFingerprint(indices);
    if (JSON.stringify(unchanged) !== JSON.stringify(initial)) {
      throw new Error("Zipp fingerprint changed without a value mutation");
    }
    const values = engine.getGlobalsBatch(indices);
    if (JSON.stringify(values) !== JSON.stringify([
      { items: [1, 2], nested: { ok: true } },
      7,
    ])) {
      throw new Error(`Zipp fingerprint preflight read unexpected globals: ${JSON.stringify(values)}`);
    }

    engine.callFunction("__zipp_fp_mutate", []);
    const mutated = engine.getGlobalsFingerprint(indices);
    requireFingerprintArray("mutated Zipp fingerprints", mutated, indices.length);
    if (mutated[0] === initial[0] || mutated[1] !== initial[1]) {
      throw new Error(
        `Zipp fingerprint did not isolate an in-place mutation: ${JSON.stringify({ initial, mutated })}`,
      );
    }

    engine.callFunction("__zipp_fp_restore", []);
    const restored = engine.getGlobalsFingerprint(indices);
    if (JSON.stringify(restored) !== JSON.stringify(initial)) {
      throw new Error(
        `Zipp structurally equal replacement did not restore its fingerprint: ${JSON.stringify({ initial, restored })}`,
      );
    }
    const duplicate = engine.getGlobalsFingerprint([state.index, state.index]);
    if (JSON.stringify(duplicate) !== JSON.stringify([initial[0], initial[0]])) {
      throw new Error(`Zipp fingerprint index ordering changed: ${JSON.stringify(duplicate)}`);
    }

    return {
      ...bindings,
      source_sha256: sha256(Buffer.from(source)),
      glue_prototype_binding: true,
      raw_wasm_binding: true,
      stable_without_mutation: true,
      in_place_mutation_changes_digest: true,
      equal_replacement_restores_digest: true,
      index_order_and_duplicates_preserved: true,
      observed: { initial, mutated, restored },
    };
  } finally {
    if (engine) {
      try { engine.dispose(); } catch {}
      try { engine.free(); } catch {}
    }
  }
}

async function makeZippRuntime(wasmPath, gluePath) {
  const module = await WebAssembly.compile(fs.readFileSync(wasmPath));
  const glue = await importSourceModule(fs.readFileSync(gluePath, "utf8"), "zipp-glue");
  requireZippFingerprintBindings(module, glue);
  glue.initSync({ module });
  return {
    module,
    evaluate(source, expected) {
      let engine;
      let result;
      const started = nowNs();
      try {
        engine = new glue.Engine();
        const symbols = engine.initScript(source);
        const symbol = symbols.__wasm_cmp_actual;
        if (!symbol || !Number.isInteger(symbol.index)) {
          throw new Error("Zipp did not expose __wasm_cmp_actual");
        }
        result = engine.getGlobalByIndex(symbol.index);
        engine.dispose();
        engine.free();
        engine = undefined;
      } finally {
        if (engine) {
          try { engine.dispose(); } catch {}
          try { engine.free(); } catch {}
        }
      }
      const milliseconds = elapsedMs(started);
      if (result !== expected) {
        throw new Error(`Zipp returned ${JSON.stringify(result)}, expected ${JSON.stringify(expected)}`);
      }
      return { milliseconds, result };
    },
  };
}

async function makeBoaRuntime(wasmPath, gluePath) {
  const module = await WebAssembly.compile(fs.readFileSync(wasmPath));
  const glue = await importSourceModule(fs.readFileSync(gluePath, "utf8"), "boa-glue");
  const imports = importsFromNamespace(module, glue);
  const instance = new WebAssembly.Instance(module, imports);
  glue.__wbg_set_wasm(instance.exports);
  instance.exports.__wbindgen_start();
  return {
    module,
    evaluate(source, expected) {
      const started = nowNs();
      // Boa's JsValue::display() includes quotes around a string completion.
      // The guest assertion has already checked the value; checking this exact
      // display also validates the package's return bridge.
      const displayed = glue.evaluate(source);
      const milliseconds = elapsedMs(started);
      if (displayed !== JSON.stringify(expected)) {
        throw new Error(`Boa returned ${JSON.stringify(displayed)}, expected ${JSON.stringify(JSON.stringify(expected))}`);
      }
      return { milliseconds, result: expected };
    },
  };
}

function allocateArgv(exports, strings) {
  const encoder = new TextEncoder();
  const allocations = [];
  const stringPointers = [];
  for (const text of strings) {
    const encoded = encoder.encode(`${text}\0`);
    const pointer = exports.malloc(encoded.length) >>> 0;
    if (!pointer) throw new Error("QuickJS-NG malloc failed");
    new Uint8Array(exports.memory.buffer).set(encoded, pointer);
    allocations.push(pointer);
    stringPointers.push(pointer);
  }
  const argv = exports.calloc(strings.length + 1, 4) >>> 0;
  if (!argv) throw new Error("QuickJS-NG calloc failed");
  allocations.push(argv);
  const view = new DataView(exports.memory.buffer);
  stringPointers.forEach((pointer, index) => view.setUint32(argv + index * 4, pointer, true));
  return {
    argv,
    free() {
      for (const pointer of allocations.reverse()) exports.free(pointer);
    },
  };
}

async function makeQuickJsRuntime(wasmPath) {
  const module = await WebAssembly.compile(fs.readFileSync(wasmPath));
  const wasi = new WASI({ version: "preview1", args: [], env: {}, preopens: {}, returnOnExit: true });
  const instance = new WebAssembly.Instance(module, {
    wasi_snapshot_preview1: wasi.wasiImport,
  });
  wasi.initialize(instance);
  const exports = instance.exports;
  for (const name of ["malloc", "calloc", "free", "qjs_init_argv", "qjs_destroy"]) {
    if (typeof exports[name] !== "function") throw new Error(`QuickJS-NG reactor lacks ${name}`);
  }
  return {
    module,
    evaluate(source, expected) {
      const started = nowNs();
      const args = allocateArgv(exports, ["qjs", "-e", source]);
      let status;
      try {
        status = exports.qjs_init_argv(3, args.argv);
        exports.qjs_destroy();
      } finally {
        args.free();
      }
      const milliseconds = elapsedMs(started);
      if (status !== 0) {
        throw new Error(`QuickJS-NG qjs_init_argv returned ${status}; embedded exact-result check failed`);
      }
      return { milliseconds, result: expected };
    },
  };
}

async function childCompile(wasmPath) {
  const bytes = fs.readFileSync(wasmPath);
  const started = nowNs();
  const module = await WebAssembly.compile(bytes);
  const milliseconds = elapsedMs(started);
  if (!(module instanceof WebAssembly.Module)) throw new Error("compile did not return a module");
  console.log(JSON.stringify({ milliseconds }));
}

async function childStartup(engine, wasmPath, gluePath) {
  const module = await WebAssembly.compile(fs.readFileSync(wasmPath));
  if (engine === "zipp") {
    const glue = await importSourceModule(fs.readFileSync(gluePath, "utf8"), "zipp-startup");
    const started = nowNs();
    glue.initSync({ module });
    console.log(JSON.stringify({ milliseconds: elapsedMs(started) }));
    return;
  }
  if (engine === "boa") {
    const glue = await importSourceModule(fs.readFileSync(gluePath, "utf8"), "boa-startup");
    const imports = importsFromNamespace(module, glue);
    const started = nowNs();
    const instance = new WebAssembly.Instance(module, imports);
    glue.__wbg_set_wasm(instance.exports);
    instance.exports.__wbindgen_start();
    console.log(JSON.stringify({ milliseconds: elapsedMs(started) }));
    return;
  }
  if (engine === "quickjs-ng") {
    const wasi = new WASI({ version: "preview1", args: [], env: {}, preopens: {}, returnOnExit: true });
    const started = nowNs();
    const instance = new WebAssembly.Instance(module, {
      wasi_snapshot_preview1: wasi.wasiImport,
    });
    wasi.initialize(instance);
    console.log(JSON.stringify({ milliseconds: elapsedMs(started) }));
    return;
  }
  throw new Error(`unknown startup engine ${engine}`);
}

function runChild(mode, args, timeoutMs = 120000) {
  const result = spawnSync(
    process.execPath,
    ["--no-warnings", SCRIPT, mode, ...args],
    { encoding: "utf8", timeout: timeoutMs, windowsHide: true },
  );
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`child ${mode} failed (${result.status}): ${result.stderr || result.stdout}`);
  }
  const lines = result.stdout.trim().split(/\r?\n/);
  return JSON.parse(lines.at(-1));
}

function activeBuildProcesses() {
  const buildNames = new Set([
    "cargo", "rustc", "wasm-opt", "clang", "clang++", "gcc", "g++",
    "cmake", "ninja", "make", "msbuild", "link", "ld", "lld",
  ]);
  try {
    if (process.platform === "win32") {
      const output = execFileSync("tasklist", ["/fo", "csv", "/nh"], { encoding: "utf8" });
      const names = output.split(/\r?\n/).map(line => {
        const match = line.match(/^"([^"]+)"/);
        return match ? match[1].replace(/\.exe$/i, "").toLowerCase() : "";
      });
      return [...new Set(names.filter(name => buildNames.has(name)))];
    }
    const output = execFileSync("ps", ["-A", "-o", "comm="], { encoding: "utf8" });
    return [...new Set(output.split(/\r?\n/).map(name => path.basename(name.trim()).toLowerCase()).filter(name => buildNames.has(name)))];
  } catch (error) {
    throw new Error(`could not audit active build processes: ${error.message}`);
  }
}

function gitMetadata() {
  const invoke = args => {
    const result = spawnSync("git", args, { cwd: ROOT, encoding: "utf8", windowsHide: true });
    return result.status === 0 ? result.stdout.trim() : null;
  };
  return {
    head: invoke(["rev-parse", "HEAD"]),
    status_porcelain: invoke(["status", "--short"]),
  };
}

function summarizePhase(observations) {
  const summary = {};
  for (const engine of ENGINE_NAMES) {
    const values = observations.filter(item => item.engine === engine).map(item => item.milliseconds);
    summary[engine] = { median_ms: median(values), samples_ms: values };
  }
  return summary;
}

function summarizeExecution(observations) {
  const byEngine = {};
  for (const engine of ENGINE_NAMES) {
    byEngine[engine] = {};
    for (const workload of WORKLOADS) {
      const rows = observations.filter(item => item.engine === engine && item.case === workload.name);
      const work = rows.map(item => item.work_ms);
      const control = rows.map(item => item.control_ms);
      const adjusted = rows.map(item => item.adjusted_ms);
      byEngine[engine][workload.name] = {
        persistent_work_median_ms: median(work),
        persistent_control_median_ms: median(control),
        adjusted_execution_median_ms: median(adjusted),
        positive_adjusted_samples: adjusted.filter(value => value > 0).length,
        samples: rows.length,
      };
    }
  }
  const ratios = {};
  for (const competitor of ["quickjs-ng", "boa"]) {
    const persistentByCase = {};
    const adjustedByCase = {};
    for (const workload of WORKLOADS) {
      const zipp = byEngine.zipp[workload.name];
      const other = byEngine[competitor][workload.name];
      persistentByCase[workload.name] =
        zipp.persistent_work_median_ms / other.persistent_work_median_ms;
      adjustedByCase[workload.name] =
        zipp.adjusted_execution_median_ms > 0 && other.adjusted_execution_median_ms > 0
          ? zipp.adjusted_execution_median_ms / other.adjusted_execution_median_ms
          : null;
    }
    ratios[competitor] = {
      zipp_over_competitor_persistent_geomean: geometricMean(Object.values(persistentByCase)),
      zipp_over_competitor_adjusted_geomean: geometricMean(Object.values(adjustedByCase).filter(value => value !== null)),
      persistent_by_case: persistentByCase,
      adjusted_by_case: adjustedByCase,
    };
  }
  return { by_engine: byEngine, ratios };
}

function printSummary(summary, output) {
  const fixed = (value, digits) => value === null ? "n/a" : value.toFixed(digits);
  console.log("\nCold module phases (fresh process; median ms)");
  console.log("engine       compile    startup    module-ready sum");
  for (const engine of ENGINE_NAMES) {
    const compile = summary.compile[engine].median_ms;
    const startup = summary.startup[engine].median_ms;
    console.log(`${engine.padEnd(12)} ${compile.toFixed(2).padStart(9)} ${startup.toFixed(2).padStart(10)} ${(compile + startup).toFixed(2).padStart(19)}`);
  }
  console.log("\nPersistent module / fresh JS context (work | adjusted work-control, median ms)");
  for (const workload of WORKLOADS) {
    console.log(`  ${workload.name}`);
    for (const engine of ENGINE_NAMES) {
      const row = summary.execution.by_engine[engine][workload.name];
      console.log(`    ${engine.padEnd(11)} ${fixed(row.persistent_work_median_ms, 3).padStart(10)} | ${fixed(row.adjusted_execution_median_ms, 3).padStart(10)}`);
    }
  }
  console.log("\nZipp / competitor geomean (<1 means Zipp faster)");
  for (const competitor of ["quickjs-ng", "boa"]) {
    const row = summary.execution.ratios[competitor];
    console.log(`  ${competitor.padEnd(11)} persistent=${fixed(row.zipp_over_competitor_persistent_geomean, 3)} adjusted=${fixed(row.zipp_over_competitor_adjusted_geomean, 3)}`);
  }
  console.log(`\nRaw evidence: ${output}`);
}

async function main(args) {
  const busy = activeBuildProcesses();
  if (busy.length && !args.allowBusy) {
    throw new Error(`refusing to time while build processes are active: ${busy.join(", ")}`);
  }

  for (const pathname of [args.zippWasm, args.zippGlue, args.quickjsWasm, args.boaWasm, args.boaGlue]) {
    fs.accessSync(pathname, fs.constants.R_OK);
  }
  const artifacts = {
    zipp_wasm: artifact(args.zippWasm),
    zipp_glue: artifact(args.zippGlue),
    quickjs_ng_reactor_wasm: artifact(args.quickjsWasm),
    boa_wasm: artifact(args.boaWasm),
    boa_glue: artifact(args.boaGlue),
  };
  if (artifacts.quickjs_ng_reactor_wasm.sha256 !== QUICKJS_NG_REACTOR_SHA256) {
    throw new Error("QuickJS-NG reactor SHA-256 is not the official v0.16.2 artifact");
  }
  if (artifacts.boa_wasm.sha256 !== BOA_WASM_SHA256) {
    throw new Error("Boa SHA-256 is not the official @boa-dev/boa_wasm 0.22.0 artifact");
  }
  const boaPackage = JSON.parse(fs.readFileSync(path.join(path.dirname(args.boaWasm), "package.json"), "utf8"));
  if (boaPackage.name !== "@boa-dev/boa_wasm" || boaPackage.version !== BOA_VERSION) {
    throw new Error(`unexpected Boa package identity: ${boaPackage.name}@${boaPackage.version}`);
  }

  // Do this before the first timed child process. A mutually stale Zipp glue
  // and module can still execute the benchmark while silently omitting a new
  // host API; performance evidence from that older product is not evidence for
  // the current release. The functional probe also catches a binding with the
  // right name but the wrong module or digest semantics.
  console.log("Preflighting Zipp fingerprint host contract...");
  const zippFingerprintContract = await preflightZippFingerprintContract(
    args.zippWasm,
    args.zippGlue,
  );

  const shuffledPermutations = seededShuffle(PERMUTATIONS, args.seed);
  const compile = [];
  const startup = [];
  const enginePaths = {
    zipp: [args.zippWasm, args.zippGlue],
    "quickjs-ng": [args.quickjsWasm, "-"],
    boa: [args.boaWasm, args.boaGlue],
  };
  console.log("Measuring cold module compilation...");
  for (let rep = 0; rep < args.compileReps; rep++) {
    const order = shuffledPermutations[rep % shuffledPermutations.length];
    for (const engine of order) {
      const sample = runChild("__child_compile", [enginePaths[engine][0]]);
      compile.push({ rep, engine, order: [...order], milliseconds: sample.milliseconds });
    }
  }
  console.log("Measuring compiled-module startup...");
  for (let rep = 0; rep < args.startupReps; rep++) {
    const order = shuffledPermutations[(rep + 3) % shuffledPermutations.length];
    for (const engine of order) {
      const sample = runChild("__child_startup", [engine, ...enginePaths[engine]]);
      startup.push({ rep, engine, order: [...order], milliseconds: sample.milliseconds });
    }
  }

  console.log("Instantiating persistent modules and validating exact results...");
  const runtimes = {
    zipp: await makeZippRuntime(args.zippWasm, args.zippGlue),
    "quickjs-ng": await makeQuickJsRuntime(args.quickjsWasm),
    boa: await makeBoaRuntime(args.boaWasm, args.boaGlue),
  };
  const sourceMetadata = {};
  for (const workload of WORKLOADS) {
    const pair = sourcesFor(workload);
    sourceMetadata[workload.name] = {
      fixture: path.join(ROOT, workload.fixture),
      fixture_sha256: sha256(fs.readFileSync(path.join(ROOT, workload.fixture))),
      source_bytes: Buffer.byteLength(pair.work),
      work_sha256: sha256(Buffer.from(pair.work)),
      control_sha256: sha256(Buffer.from(pair.control)),
      differing_utf8_bytes: [...Buffer.from(pair.work)].filter((byte, index) => byte !== Buffer.from(pair.control)[index]).length,
      work_expected: pair.workExpected,
      control_expected: pair.controlExpected,
    };
    for (const engine of ENGINE_NAMES) {
      runtimes[engine].evaluate(pair.control, pair.controlExpected);
      runtimes[engine].evaluate(pair.work, pair.workExpected);
    }
  }

  console.log("Measuring persistent-module paired evaluations...");
  const execution = [];
  const caseOrderBase = seededShuffle(WORKLOADS.map(item => item.name), args.seed ^ 0x9e3779b9);
  const workloadByName = Object.fromEntries(WORKLOADS.map(item => [item.name, item]));
  for (let rep = 0; rep < args.reps; rep++) {
    const caseOrder = caseOrderBase.map((_, index) => caseOrderBase[(index + rep) % caseOrderBase.length]);
    for (let casePosition = 0; casePosition < caseOrder.length; casePosition++) {
      const caseName = caseOrder[casePosition];
      const workload = workloadByName[caseName];
      const pair = sourcesFor(workload);
      // Every workload sees every one of the six engine permutations exactly
      // once per six reps, independent of that workload's rotated position.
      const engineOrder = shuffledPermutations[rep % shuffledPermutations.length];
      for (let enginePosition = 0; enginePosition < engineOrder.length; enginePosition++) {
        const engine = engineOrder[enginePosition];
        // Each engine runs work first three times and control first three times
        // per workload in every complete six-rep block.
        const workFirst = (rep + ENGINE_NAMES.indexOf(engine)) % 2 === 0;
        const pairOrder = workFirst ? ["work", "control"] : ["control", "work"];
        const measured = {};
        for (const kind of pairOrder) {
          const expected = kind === "work" ? pair.workExpected : pair.controlExpected;
          measured[kind] = runtimes[engine].evaluate(pair[kind], expected);
        }
        execution.push({
          rep,
          case: caseName,
          case_position: casePosition,
          engine,
          engine_position: enginePosition,
          engine_order: [...engineOrder],
          pair_order: pairOrder,
          work_ms: measured.work.milliseconds,
          control_ms: measured.control.milliseconds,
          adjusted_ms: measured.work.milliseconds - measured.control.milliseconds,
          work_result: measured.work.result,
          control_result: measured.control.result,
        });
      }
    }
  }

  const summary = {
    compile: summarizePhase(compile),
    startup: summarizePhase(startup),
    execution: summarizeExecution(execution),
  };
  const result = {
    schema: 1,
    generated_at: new Date().toISOString(),
    methodology: {
      scope: "diagnostic; host APIs and bundled features are not equivalent",
      compile: "fresh Node process; file read excluded; WebAssembly.compile included",
      startup: "fresh Node process; module compilation and glue parsing excluded; instantiation and wasm start included",
      persistent: "one live WASM instance per engine; each evaluation creates and tears down a fresh JS context",
      adjusted: "paired work elapsed minus a same-byte/same-AST zero-work control; pair order and engine order counterbalanced",
      quickjs_result_validation: "the identical guest source throws unless its exact result matches; qjs_init_argv status must be zero",
      caveats: [
        "Zipp is a bounded safe-sandbox interpreter with a persistent Engine host API.",
        "QuickJS-NG uses the official WASI reactor; its public exports do not expose JS_Eval or JS_Call, so qjs_init_argv creates the context.",
        "Boa evaluate() constructs Context::default() for every call and bundles Intl, Temporal, Annex B, and experimental features.",
        "Persistent totals include each binding's source/result marshalling and context teardown; adjusted deltas cancel most fixed costs but remain diagnostic.",
      ],
    },
    configuration: {
      reps: args.reps,
      compile_reps: args.compileReps,
      startup_reps: args.startupReps,
      seed: args.seed,
      engine_permutations: shuffledPermutations,
    },
    environment: {
      node: process.version,
      v8: process.versions.v8,
      platform: process.platform,
      release: os.release(),
      arch: process.arch,
      cpu: os.cpus()[0]?.model ?? null,
      logical_cpus: os.cpus().length,
      git: gitMetadata(),
    },
    provenance: {
      zipp: {
        required_host_contract: zippFingerprintContract,
      },
      quickjs_ng: { version: QUICKJS_NG_VERSION, commit: QUICKJS_NG_COMMIT },
      boa: { package: `@boa-dev/boa_wasm@${BOA_VERSION}`, commit: BOA_COMMIT },
    },
    artifacts,
    sources: sourceMetadata,
    observations: { compile, startup, execution },
    summary,
  };
  fs.mkdirSync(path.dirname(args.output), { recursive: true });
  fs.writeFileSync(args.output, `${JSON.stringify(result, null, 2)}\n`, { flag: "wx" });
  printSummary(summary, args.output);
}

if (process.argv[2] === "__child_compile") {
  await childCompile(path.resolve(process.argv[3]));
} else if (process.argv[2] === "__child_startup") {
  await childStartup(process.argv[3], path.resolve(process.argv[4]), path.resolve(process.argv[5]));
} else {
  try {
    await main(parseArgs(process.argv.slice(2)));
  } catch (error) {
    console.error(error?.stack || String(error));
    process.exitCode = 1;
  }
}
