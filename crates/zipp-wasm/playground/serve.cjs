#!/usr/bin/env node
// A dependency-free playground server for Python and JavaScript on WASM. It serves the
// REPOSITORY ROOT (so the page can reach ../dist/all/ for the engine and
// ../../../examples/ for the sample projects) on loopback only. Any web page
// can send requests to a loopback port, so it also answers only its own Host
// (a DNS-rebinding page names another), never serves a dot-file or
// dot-folder (`.git/`, `landing/.dev.vars`), and rejects paths with NUL bytes.
//
//   cd crates/zipp-wasm
//   ./build-variants.sh all        # once: the Python-enabled engine
//   node playground/serve.cjs      # then open the printed URL
"use strict";
const fs = require("node:fs");
const http = require("node:http");
const path = require("node:path");

const ROOT = path.resolve(__dirname, "..", "..", "..");
let PORT = Number(process.env.PORT) || 8765;
const TYPES = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".cjs": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".wasm": "application/wasm",
  ".py": "text/plain; charset=utf-8",
  ".safetensors": "application/octet-stream",
  ".md": "text/plain; charset=utf-8",
  ".txt": "text/plain; charset=utf-8",
  ".svg": "image/svg+xml",
  ".png": "image/png",
};

// The Host header a browser sends to this server, and only that.
function ownHost(host) {
  return host === `127.0.0.1:${PORT}` || host === `localhost:${PORT}`;
}

const server = http.createServer((request, response) => {
  if (!ownHost(request.headers.host)) {
    response.writeHead(421).end("misdirected request");
    return;
  }
  let pathname, url;
  try {
    url = new URL(request.url, 'http://localhost');
    pathname = decodeURIComponent(url.pathname);
  } catch {
    response.writeHead(400).end("bad request");
    return;
  }
  if (pathname.includes("\0")) {
    response.writeHead(400).end("bad request");
    return;
  }
  if (pathname.endsWith("/")) pathname += "index.html";
  if (pathname.split(/[\\/]/).some((segment) => segment.startsWith("."))) {
    response.writeHead(403).end("forbidden");
    return;
  }
  const file = path.resolve(ROOT, "." + pathname);
  if (!file.startsWith(ROOT + path.sep) && file !== ROOT) {
    response.writeHead(403).end("forbidden");
    return;
  }
  readFile(file, response);
});

function readFile(file, response) {
  const reply = (error, body) => {
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
  };
  try {
    fs.readFile(file, reply);
  } catch (error) {
    // Invalid paths throw synchronously; answer rather than crash the server.
    reply(error);
  }
}

for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => { server.close(); process.exit(0); });

server.on('error', error => {
  if (error.code === 'EADDRINUSE' && !process.env.PORT && PORT < 8775) {
    console.log(`Port ${PORT} is occupied; trying ${PORT + 1}.`);
    PORT++;
    server.listen(PORT, '127.0.0.1');
  } else {
    console.error(`Cannot start playground: ${error.message}. Set PORT to an available local port.`);
    process.exitCode = 1;
  }
});
server.on('listening', () => {
  const engine = path.join(ROOT, "crates", "zipp-wasm", "dist", "all", "zipp_wasm_bg.wasm");
  if (!fs.existsSync(engine)) {
    console.log("note: no Python-enabled engine at crates/zipp-wasm/dist/all/ yet; run ./build-variants.sh all first");
  }
  console.log(`zipp playground: http://127.0.0.1:${PORT}/crates/zipp-wasm/playground/`);
  // The experimental local-model lab is served from the same root when present.
  const lab = path.join(ROOT, "crates", "zipp-wasm", "model-plugins", "demo", "index.html");
  if (fs.existsSync(lab)) {
    console.log(`model lab:      http://127.0.0.1:${PORT}/crates/zipp-wasm/model-plugins/demo/`);
    console.log(`backend parity: http://127.0.0.1:${PORT}/crates/zipp-wasm/model-plugins/demo/parity.html`);
  }
});
server.listen(PORT, '127.0.0.1');
