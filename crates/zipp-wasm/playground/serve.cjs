#!/usr/bin/env node
// A dependency-free static server for the playground. It serves the
// REPOSITORY ROOT (so the page can reach ../dist/all/ for the engine and
// ../../../examples/ for the sample projects) on loopback only.
//
//   cd crates/zipp-wasm
//   ./build-variants.sh all        # once: the Python-enabled engine
//   node playground/serve.cjs      # then open the printed URL
"use strict";
const fs = require("node:fs");
const http = require("node:http");
const path = require("node:path");

const ROOT = path.resolve(__dirname, "..", "..", "..");
const PORT = Number(process.env.PORT) || 8765;
const TYPES = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".cjs": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".wasm": "application/wasm",
  ".py": "text/plain; charset=utf-8",
  ".md": "text/plain; charset=utf-8",
  ".txt": "text/plain; charset=utf-8",
  ".svg": "image/svg+xml",
  ".png": "image/png",
};

const server = http.createServer((request, response) => {
  let pathname;
  try {
    pathname = decodeURIComponent(new URL(request.url, "http://localhost").pathname);
  } catch {
    response.writeHead(400).end("bad request");
    return;
  }
  if (pathname.endsWith("/")) pathname += "index.html";
  const file = path.resolve(ROOT, "." + pathname);
  if (!file.startsWith(ROOT + path.sep) && file !== ROOT) {
    response.writeHead(403).end("forbidden");
    return;
  }
  fs.readFile(file, (error, body) => {
    if (error) {
      response.writeHead(error.code === "ENOENT" ? 404 : 500).end(error.code === "ENOENT" ? "not found" : "error");
      return;
    }
    response.writeHead(200, {
      "content-type": TYPES[path.extname(file).toLowerCase()] || "application/octet-stream",
      "cache-control": "no-store",
      "cross-origin-opener-policy": "same-origin",
      "cross-origin-embedder-policy": "require-corp",
    });
    response.end(body);
  });
});

server.listen(PORT, "127.0.0.1", () => {
  const engine = path.join(ROOT, "crates", "zipp-wasm", "dist", "all", "zipp_wasm_bg.wasm");
  if (!fs.existsSync(engine)) {
    console.log("note: no Python-enabled engine at crates/zipp-wasm/dist/all/ yet; run ./build-variants.sh all first");
  }
  console.log(`zipp playground: http://127.0.0.1:${PORT}/crates/zipp-wasm/playground/`);
});
