#!/usr/bin/env node
// node test-kit.mjs <kit-dir> <modules-dir> [variant ...]
//
// Checks an assembled embed kit against the release modules it was built
// for, through the kit's own copy of zipp.mjs, as an embedder would load
// them: the checksums the kit records, each engine's record and profile,
// JavaScript and Python runs, the Lite contract, the engine API the release
// smoke has always exercised (globals and their fingerprints), torch added
// to the base Python engine against the complete engine's output, and the
// kit's Node example. With variants named, only those engines are loaded.
// Pointed at the source directory (no modules.json yet), it checks the
// modules it finds by their own records: one variant's CI job checks its
// module this way before the kit exists. RELEASE_VERSION and
// EXPECTED_COMMIT, when set, must be what every module records.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

const [kitDir, modulesDir, ...only] = process.argv.slice(2);
if (!kitDir || !modulesDir) {
  console.error("usage: test-kit.mjs <kit-dir> <modules-dir> [variant ...]");
  process.exit(2);
}
const sha256 = bytes => createHash("sha256").update(bytes).digest("hex");
const { loadZipp, readZippMeta, compileZipp } = await import(pathToFileURL(path.join(kitDir, "zipp.mjs")));
let checks = 0;
const ok = (name) => { checks++; console.log(`  ok   ${name}`); };
const assembled = fs.existsSync(path.join(kitDir, "modules.json"));
let manifest;
if (assembled) {
  manifest = JSON.parse(fs.readFileSync(path.join(kitDir, "modules.json"), "utf8"));
  // The kit's own checksums, and the modules it names.
  for (const line of fs.readFileSync(path.join(kitDir, "SHA256SUMS"), "utf8").trim().split("\n")) {
    const [hash, name] = line.split(/\s+/);
    assert.equal(sha256(fs.readFileSync(path.join(kitDir, name))), hash, name);
  }
  ok("kit SHA256SUMS");
} else {
  const modules = [];
  for (const file of fs.readdirSync(modulesDir).filter(f => /^zipp-.*\.wasm$/.test(f)).sort()) {
    const bytes = fs.readFileSync(path.join(modulesDir, file));
    const meta = readZippMeta(new WebAssembly.Module(bytes));
    if (meta) modules.push({ file, variant: meta.variant, languages: meta.languages, bytes: bytes.length, sha256: sha256(bytes), version: meta.version, commit: meta.commit });
  }
  assert.ok(modules.length, `no ZIPP modules in ${modulesDir}`);
  manifest = { version: modules[0].version, commit: modules[0].commit, modules };
}
if (process.env.RELEASE_VERSION) assert.equal(manifest.version, process.env.RELEASE_VERSION, "release version");
if (process.env.EXPECTED_COMMIT) assert.equal(manifest.commit, process.env.EXPECTED_COMMIT, "release commit");
const byVariant = new Map();
for (const m of manifest.modules) {
  const bytes = fs.readFileSync(path.join(modulesDir, m.file));
  assert.equal(bytes.length, m.bytes, m.file);
  assert.equal(sha256(bytes), m.sha256, m.file);
  byVariant.set(m.variant, { ...m, path: path.join(modulesDir, m.file) });
}
ok(`modules.json names ${manifest.modules.length} modules of ${manifest.version} at ${manifest.commit}`);

// The Brotli copies a release publishes beside the modules: each must
// decompress to its module, and the loader must take one directly.
{
  const zlib = await import("node:zlib");
  const packed = manifest.modules.filter(m => fs.existsSync(path.join(modulesDir, `${m.file}.br`)));
  for (const m of packed) {
    const br = fs.readFileSync(path.join(modulesDir, `${m.file}.br`));
    assert.ok(zlib.brotliDecompressSync(br).equals(fs.readFileSync(path.join(modulesDir, m.file))), `${m.file}.br`);
    if (m.brotli) assert.equal(sha256(br), m.brotli.sha256, `${m.file}.br sha256`);
  }
  const js = packed.find(m => m.variant === "javascript" || m.variant === "lite");
  if (js) {
    const viaBr = await loadZipp(path.join(modulesDir, `${js.file}.br`));
    assert.equal(viaBr.meta.variant, js.variant);
    assert.deepEqual(viaBr.run("console.log(6 * 7)").output, ["42"]);
  }
  if (packed.length) ok(`${packed.length} .wasm.br files decompress to their modules, and ${js ? js.file + ".br loads" : "none is an engine"}`);
}

const engines = ["javascript", "lite", "javascript-typescript", "javascript-python", "javascript-python-base"]
  .filter(v => byVariant.has(v) && (only.length === 0 || only.includes(v)));
assert.ok(engines.length, "no engine module to check");
const outputs = {};
for (const variant of engines) {
  const m = byVariant.get(variant);
  const zipp = await loadZipp(m.path);
  assert.equal(zipp.meta.variant, variant);
  assert.equal(zipp.meta.version, manifest.version);
  assert.equal(zipp.meta.commit, manifest.commit);
  assert.equal(zipp.profile.version, manifest.version);
  assert.equal(zipp.profile.source?.sha ?? null, manifest.commit);
  assert.deepEqual([...zipp.languages], m.languages);
  assert.equal(zipp.profile.variant === "lite", variant === "lite");

  const js = zipp.run(`
    const xs = [3, 1, 2].sort((a, b) => a - b);
    console.log(xs.join(","), typeof Intl, typeof Temporal, JSON.stringify({ a: [1, { b: null }] }));
    class P { #x = 1; get x() { return this.#x; } }
    console.log(new P().x, [..."héllo"].length, 2n ** 64n);
  `).output;
  const intl = variant === "lite" ? "undefined undefined" : "object object";
  assert.deepEqual(js, [`1,2,3 ${intl} {"a":[1,{"b":null}]}`, "1 5 18446744073709551616n"]);

  assert.throws(() => zipp.run("console.log('first'); null.x"), e => e.output[0] === "first" && /TypeError/.test(e.message));
  assert.throws(() => zipp.run("for (;;) {}", { instructionBudget: 100000 }), /instruction budget/);

  // The engine API, as the release smoke has always checked it.
  const engine = new zipp.Engine();
  try {
    const symbols = engine.initScript(`
      let probe = { items: [1] };
      function grow() { probe.items.push(2); return probe.items.length; }
      console.log("ready");
    `);
    const indices = [symbols.probe.index];
    const before = engine.getGlobalsFingerprint(indices);
    assert.deepEqual(engine.getGlobalsFingerprint(indices), before);
    assert.equal(engine.callFunction("grow", []), 2);
    assert.notEqual(engine.getGlobalsFingerprint(indices)[0], before[0]);
    assert.deepEqual(engine.getGlobalsBatch(indices), [{ items: [1, 2] }]);
    assert.deepEqual(engine.takeOutput(), ["ready"]);
  } finally {
    engine.dispose();
    engine.free();
  }

  // TypeScript: run, in a sandbox with its functions, or refused.
  const tsProgram = "enum E { A = 2, B } const total: number = E.B * 7; console.log(`ts ${total} ${E[3]}`);";
  if (zipp.languages.includes("typescript")) {
    assert.deepEqual(zipp.run(tsProgram, { language: "typescript" }).output, ["ts 21 B"]);
    const sandbox = await zipp.createSandbox({ language: "typescript", isolate: false });
    try {
      const ran = await sandbox.run("function twice(n: number): number { return n * 2; }\nconsole.log('up');");
      assert.deepEqual(ran.output, ["up"]);
      assert.equal((await sandbox.call("twice", [21])).value, 42);
    } finally {
      await sandbox.close();
    }
  } else {
    assert.throws(() => zipp.run(tsProgram, { language: "typescript" }));
  }

  if (zipp.languages.includes("python")) {
    assert.deepEqual(zipp.run("print(sum(x * x for x in range(4)))", { language: "python" }).output, ["14"]);
    const torchProgram = [
      "try:",
      "    import torch",
      "except ModuleNotFoundError as e:",
      "    print('no', e.name)",
      "else:",
      "    x = torch.tensor([[1.0, 2.0], [3.0, 4.0]], requires_grad=True)",
      "    y = (x @ x).relu().sum()",
      "    y.backward()",
      "    print(y.item(), x.grad.tolist())",
    ].join("\n");
    outputs[variant] = zipp.run(torchProgram, { language: "python" }).output;
    if (variant === "javascript-python-base" && !byVariant.has("torch-package")) {
      assert.deepEqual(outputs[variant], ["no torch"]);
    } else if (variant === "javascript-python-base") {
      assert.deepEqual(outputs[variant], ["no torch"]);
      const torchPath = byVariant.get("torch-package").path;
      const torchMeta = readZippMeta(await compileZipp(torchPath));
      assert.equal(torchMeta.variant, "torch-package");
      assert.equal(torchMeta.version, manifest.version);
      await zipp.addTorch(torchPath);
      outputs.withTorch = zipp.run(torchProgram, { language: "python" }).output;
    }
  } else {
    assert.throws(() => zipp.run("print(1)", { language: "python" }));
  }
  ok(`${variant}: ${m.file} (${zipp.languages.join(", ")})`);
}
if (outputs["javascript-python"] && outputs.withTorch) {
  assert.notDeepEqual(outputs.withTorch, ["no torch"]);
  assert.deepEqual(outputs.withTorch, outputs["javascript-python"]);
  ok(`python-base + torch package prints what the complete engine prints: ${outputs.withTorch}`);
}

// A torch package is not an engine; a plain module is refused.
if (byVariant.has("torch-package") && engines.length > 1) {
  await assert.rejects(loadZipp(byVariant.get("torch-package").path), /torch package, not an engine/);
  ok("the torch package is refused as an engine");
}

// The kit's Node example, against each engine it fits.
// (Only in an assembled kit: the source copy's default module path is a placeholder.)
for (const variant of assembled ? engines.filter(v => v === "javascript" || v === "javascript-python") : []) {
  const out = execFileSync(process.execPath, [path.join(kitDir, "examples/node.mjs"), byVariant.get(variant).path], { encoding: "utf8" });
  assert.match(out, /add\(3\) = 5/);
  assert.match(out, /\[ 'Ada & Grace' \]/);
  assert.match(out, /\[ '1 4 9' \]/);
  if (variant === "javascript-python") assert.match(out, /\[ '285' \]/);
  ok(`examples/node.mjs with ${variant}`);
}
console.log(`${checks} kit checks passed`);
