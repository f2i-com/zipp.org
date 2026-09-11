#!/usr/bin/env node
// Browser Worker smoke test for the exact web-target package, driven by
// Playwright (the 11 September 2026 audit's ZIPP-17). Serves this crate over
// a loopback HTTP server, opens tests/browser/index.html in each requested
// browser, and reads the results the page records after running every
// scenario through the reference host adapter in real Workers:
// initialization, source and resource failures, host calls with completion
// and cancellation, Worker-side sync bridges under default-deny, deadline
// termination and recreation, multi-tenant isolation, events and console.
//
//   node tests/browser/worker-smoke.mjs                 # chromium (bundled)
//   node tests/browser/worker-smoke.mjs --browsers chromium,firefox,webkit
//   node tests/browser/worker-smoke.mjs --channel chrome # an installed Chrome
//
// Needs `tests/browser/pkg/` built with `wasm-bindgen --target web` from the
// artifact under test, and the `playwright` package resolvable (install it
// alongside, or point NODE_PATH at a directory that has it) with the
// browsers installed (`npx playwright install --with-deps <browser>`).
import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const here = path.dirname(fileURLToPath(import.meta.url));
const crateRoot = path.resolve(here, "..", "..");
const args = process.argv.slice(2);
let browsers = ["chromium"];
let channel = null;
for (let i = 0; i < args.length; i++) {
  if (args[i] === "--browsers") browsers = args[++i].split(",").map((s) => s.trim()).filter(Boolean);
  else if (args[i] === "--channel") channel = args[++i];
  else throw new Error(`unknown option ${args[i]}`);
}
if (!fs.existsSync(path.join(here, "pkg", "zipp_wasm_bg.wasm"))) {
  console.error("no web package at tests/browser/pkg; build it with wasm-bindgen --target web");
  process.exit(2);
}
const playwright = require("playwright");

const TYPES = { ".html": "text/html; charset=utf-8", ".js": "text/javascript", ".mjs": "text/javascript", ".wasm": "application/wasm", ".json": "application/json", ".css": "text/css" };
const server = http.createServer((req, res) => {
  const url = new URL(req.url, "http://localhost");
  const file = path.normalize(path.join(crateRoot, url.pathname));
  if (!file.startsWith(crateRoot) || !fs.existsSync(file) || fs.statSync(file).isDirectory()) {
    res.writeHead(404); res.end("not found"); return;
  }
  res.writeHead(200, {
    "content-type": TYPES[path.extname(file)] || "application/octet-stream",
    "cache-control": "no-store",
    // Workers and wasm need no special isolation here; keep the page plain.
  });
  fs.createReadStream(file).pipe(res);
});
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const origin = `http://127.0.0.1:${server.address().port}`;

let failed = 0;
for (const name of browsers) {
  const type = playwright[name];
  if (!type) { console.log(`=== ${name}: unknown browser`); failed++; continue; }
  console.log(`=== ${name}${channel ? ` (${channel})` : ""}`);
  let browser;
  try {
    browser = await type.launch(channel && name === "chromium" ? { channel } : {});
  } catch (error) {
    console.log(`  SKIP ${name}: cannot launch (${String(error.message).split("\n")[0]})`);
    failed++;
    continue;
  }
  const version = browser.version();
  const page = await browser.newPage();
  const consoleErrors = [];
  page.on("pageerror", (e) => consoleErrors.push(String(e)));
  await page.goto(`${origin}/tests/browser/index.html`);
  await page.waitForFunction(() => window.__zippDone === true, null, { timeout: 120000 });
  const results = await page.evaluate(() => window.__zippResults);
  let ok = 0, bad = 0;
  for (const r of results) {
    if (r.ok) { ok++; console.log(`  ok   ${r.name}`); }
    else { bad++; console.log(`  FAIL ${r.name} — ${JSON.stringify(r.detail)}`); }
  }
  for (const e of consoleErrors) { bad++; console.log(`  FAIL page error: ${e}`); }
  console.log(`  ${name} ${version}: ${ok} passed, ${bad} failed`);
  if (bad) failed++;
  await browser.close();
}
server.close();
process.exit(failed ? 1 : 0);
