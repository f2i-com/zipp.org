#!/usr/bin/env node
// node test-browser.mjs <kit-dir> <modules-dir> [--browsers chromium,firefox,webkit]
//
// The embed kit in real browsers, driven by Playwright: serves the kit with
// the release modules beside it (as its README tells a user to), then
//   - examples/browser.html runs its program;
//   - examples/sandbox.html shows a sandbox refusing storage, files and the
//     network, then granted them (and refusing another site), then stopping
//     an endless loop by its time limit;
//   - isolated sandboxes run every engine, python-base with the torch package;
//   - the host SDK runs a guest in a Worker and ends it at its deadline;
//   - every engine loads through zipp.mjs and runs, served once as
//     application/wasm and once as application/octet-stream (what GitHub
//     release downloads send), and python-base gains torch from the package;
//   - a page under the Content-Security-Policy the README recommends loads.
// Needs the `playwright` package resolvable (NODE_PATH works) and its
// browsers installed.
import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const args = process.argv.slice(2);
const [kitDir, modulesDir] = args;
let browsers = ["chromium"];
for (let i = 2; i < args.length; i++) {
  if (args[i] === "--browsers") browsers = args[++i].split(",").filter(Boolean);
  else throw new Error(`unknown option ${args[i]}`);
}
if (!kitDir || !modulesDir) {
  console.error("usage: test-browser.mjs <kit-dir> <modules-dir> [--browsers ...]");
  process.exit(2);
}
const playwright = require("playwright");
const manifest = JSON.parse(fs.readFileSync(path.join(kitDir, "modules.json"), "utf8"));
const version = manifest.version;

const TYPES = { ".html": "text/html; charset=utf-8", ".mjs": "text/javascript", ".js": "text/javascript", ".json": "application/json" };
const CSP = "default-src 'self'; script-src 'self' blob: 'wasm-unsafe-eval'; worker-src 'self' blob:";
const server = http.createServer((req, res) => {
  const url = new URL(req.url, "http://localhost");
  if (url.pathname === "/csp.html") {
    res.writeHead(200, { "content-type": TYPES[".html"], "content-security-policy": CSP });
    res.end(`<!doctype html><script type="module" src="/csp.mjs"></script>`);
    return;
  }
  if (url.pathname === "/csp.mjs") {
    res.writeHead(200, { "content-type": TYPES[".mjs"] });
    res.end(`import { loadZipp } from "/zipp.mjs";
      loadZipp("/zipp-${version}-javascript.wasm")
        .then(async z => {
          const direct = z.run("console.log(40 + 2)").output;
          const sandbox = await z.createSandbox();
          const isolated = (await sandbox.run("console.log(6 * 7)")).output;
          await sandbox.close();
          window.cspResult = [...direct, ...isolated];
        })
        .catch(e => { window.cspResult = String(e.message); });`);
    return;
  }
  const name = decodeURIComponent(url.pathname).replace(/^\/+/, "");
  const fromKit = path.normalize(path.join(kitDir, name));
  const fromModules = path.normalize(path.join(modulesDir, name));
  const file = [fromKit, fromModules].find(f => (f.startsWith(path.normalize(kitDir)) || f.startsWith(path.normalize(modulesDir)))
    && fs.existsSync(f) && fs.statSync(f).isFile());
  if (!file) {
    res.writeHead(404);
    res.end("not found");
    return;
  }
  const wasmType = url.searchParams.get("type") === "octet" ? "application/octet-stream" : "application/wasm";
  const ext = path.extname(file);
  // A .wasm.br is sent as a server configured for it would
  // (`Content-Encoding: br`, the browser decodes), or with `?enc=none` as a
  // plain file a server knows nothing about.
  if (file.endsWith(".wasm.br")) {
    const decode = url.searchParams.get("enc") !== "none";
    res.writeHead(200, decode
      ? { "content-type": "application/wasm", "content-encoding": "br", "cache-control": "no-store" }
      : { "content-type": "application/octet-stream", "cache-control": "no-store" });
    fs.createReadStream(file).pipe(res);
    return;
  }
  res.writeHead(200, { "content-type": ext === ".wasm" ? wasmType : TYPES[ext] || "application/octet-stream", "cache-control": "no-store" });
  fs.createReadStream(file).pipe(res);
});
await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
const origin = `http://127.0.0.1:${server.address().port}`;

async function inPage(page, version) {
  return page.evaluate(async version => {
    const { loadZipp } = await import("/zipp.mjs");
    const { createZippHost } = await import("/host-sdk/zipp-host.mjs");
    const results = [];
    // Isolated sandboxes (their own Workers) for each engine.
    for (const variant of ["javascript", "javascript-lite", "javascript-python"]) {
      const zipp = await loadZipp(`/zipp-${version}-${variant}.wasm`);
      const sandbox = await zipp.createSandbox({ permissions: { storage: true } });
      const out = (await sandbox.run("localStorage.setItem('k', 'v'); console.log(localStorage.getItem('k'), typeof fetch)")).output.join("|");
      await sandbox.close();
      results.push({ type: "sandbox", variant, version: zipp.meta.version, js: out, py: null });
    }
    const base = await loadZipp({ engine: `/zipp-${version}-javascript-python-base.wasm`, packages: [`/zipp-${version}-torch-package.wasm`] });
    const pySandbox = await base.createSandbox({ language: "python", limits: { timeoutMs: 120000 }, permissions: { files: { "helpers.py": "def triple(x):\n    return 3 * x\n" } } });
    const pyOut = (await pySandbox.run("import helpers\nimport torch\nprint(helpers.triple(2), (torch.tensor([1.0, 2.0]) * 3).tolist())")).output.join("|");
    await pySandbox.close();
    results.push({ type: "sandbox", variant: "javascript-python-base+torch-package", version: base.meta.version, js: null, py: pyOut });
    // The Brotli copy, decoded by the server, or sent raw (the loader then
    // decompresses it itself where the browser can, or falls back to the
    // plain .wasm beside it).
    for (const enc of ["br", "none"]) {
      const zipp = await loadZipp(`/zipp-${version}-javascript.wasm.br?enc=${enc}`);
      results.push({ type: "brotli", variant: `javascript.wasm.br (${enc === "br" ? "Content-Encoding: br" : "no encoding"})`, version: zipp.meta.version, js: zipp.run("console.log(6 * 7)").output.join("|"), py: null });
    }
    // The host SDK: a Worker per guest, ended at its deadline.
    // Enough steps that the deadline, not the instruction budget, ends the loop.
    const host = createZippHost({ wasmUrl: `/zipp-${version}-javascript.wasm`, deadlineMs: 2000, instructionBudget: 2_000_000_000 });
    await host.init("function render(s) { return s.n * 2; } function spin() { for (;;) {} }");
    const rendered = await host.call("render", [{ n: 21 }]);
    let spun = "finished";
    try { await host.call("spin", [], { deadlineMs: 300 }); } catch (e) { spun = `${e.category} ${e.terminal} ${host.dead}`; }
    results.push({ type: "host-sdk", variant: "javascript", version, js: `${JSON.stringify(rendered)} ${spun}`, py: null });
    for (const type of ["wasm", "octet"]) {
      for (const variant of ["javascript", "javascript-lite", "javascript-python", "javascript-python-base"]) {
        const zipp = await loadZipp(`/zipp-${version}-${variant}.wasm?type=${type}`);
        const js = zipp.run("console.log([1, 2, 3].map(n => n * n).join(' '), typeof Intl)").output.join("|");
        let py = null;
        if (zipp.languages.includes("python")) {
          if (variant === "javascript-python-base") await zipp.addTorch(`/zipp-${version}-torch-package.wasm?type=${type}`);
          py = zipp.run("import torch\nprint((torch.tensor([1.0, 2.0]) * 3).tolist())", { language: "python" }).output.join("|");
        }
        results.push({ type, variant, version: zipp.meta.version, js, py });
      }
    }
    return results;
  }, version);
}

let failed = 0;
const check = (name, cond, detail) => {
  if (cond) console.log(`  ok   ${name}`);
  else { failed++; console.log(`  FAIL ${name}: ${JSON.stringify(detail)}`); }
};
for (const name of browsers) {
  const browser = await playwright[name].launch();
  console.log(`=== ${name} ${browser.version()}`);
  const page = await browser.newPage();
  const errors = [];
  page.on("pageerror", e => errors.push(String(e)));

  await page.goto(`${origin}/examples/browser.html`);
  await page.waitForFunction(() => window.zippExample?.ready, null, { timeout: 120000 });
  await page.click("#run");
  await page.waitForFunction(() => document.getElementById("out").textContent !== "", null, { timeout: 120000 });
  const out = await page.textContent("#out");
  check("examples/browser.html runs its program", out.includes("squares: 1, 4, 9, 16") && out.includes("1,234,567.891"), out);

  await page.goto(`${origin}/examples/sandbox.html`);
  await page.waitForFunction(() => window.zippExample?.ready, null, { timeout: 120000 });
  const text = await page.evaluate(() => window.zippExample.text());
  const [, denied = "", granted = "", loop = ""] = text.split("---");
  check("examples/sandbox.html: nothing is reachable by default",
    denied.includes("storage: refused") && denied.includes("file: refused") && denied.includes("network: no fetch here"), denied);
  check("examples/sandbox.html: granted storage, a file and one folder of the network, not another site",
    granted.includes("storage: saved") && granted.includes("file: hello from a file") && granted.includes("network: HTTP 200")
      && granted.includes("other site: refused") && granted.includes('{"visits":"1"}'), granted);
  check("examples/sandbox.html: the time limit stops an endless loop", loop.includes("timeout:"), loop);

  const results = await inPage(page, version);
  for (const r of results) {
    if (r.type === "sandbox") {
      const good = r.py === null ? r.js === "v undefined" : r.py === "6 [3.0, 6.0]";
      check(`isolated sandbox: ${r.variant}`, r.version === version && good, r);
      continue;
    }
    if (r.type === "brotli") {
      check(`loads ${r.variant}`, r.version === version && r.js === "42", r);
      continue;
    }
    if (r.type === "host-sdk") {
      check("host SDK: a Worker guest, ended by its deadline", r.js === "42 deadline true true", r);
      continue;
    }
    const intl = r.variant === "javascript-lite" ? "undefined" : "object";
    const pyOk = r.variant.includes("python") ? r.py === "[3.0, 6.0]" : r.py === null;
    check(`${r.variant} served as ${r.type}`, r.version === version && r.js === `1 4 9 ${intl}` && pyOk, r);
  }

  await page.goto(`${origin}/csp.html`);
  await page.waitForFunction(() => window.cspResult !== undefined, null, { timeout: 60000 });
  const csp = await page.evaluate(() => window.cspResult);
  check(`loads, and sandboxes, under "${CSP}"`, JSON.stringify(csp) === JSON.stringify(["42", "42"]), csp);

  for (const e of errors) check("no page error", false, e);
  await browser.close();
}
server.close();
console.log(failed ? `${failed} browser checks failed` : "all browser checks passed");
process.exit(failed ? 1 : 0);
