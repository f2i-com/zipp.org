// The playground development server's request boundary:
//   node --test crates/zipp-wasm/playground/serve.test.cjs
// Any web page can reach a loopback port, so the server answers only its own
// Host (a DNS-rebinding page sends another), never serves dot-files such as
// `landing/.dev.vars` or `.git/`, and survives a NUL byte in the path, which
// used to throw inside `fs.readFile` and end the process.
"use strict";
const { test, before, after } = require("node:test");
const assert = require("node:assert/strict");
const http = require("node:http");
const path = require("node:path");
const { spawn } = require("node:child_process");

const PORT = 20000 + Math.floor(Math.random() * 20000);
let server;

function get(rawPath, host = `127.0.0.1:${PORT}`) {
  return new Promise((resolve, reject) => {
    const request = http.request({ host: "127.0.0.1", port: PORT, path: rawPath, headers: { host } }, (response) => {
      let body = "";
      response.on("data", (chunk) => { body += chunk; });
      response.on("end", () => resolve({ status: response.statusCode, body }));
    });
    request.on("error", reject);
    request.end();
  });
}

before(async () => {
  server = spawn(process.execPath, [path.join(__dirname, "serve.cjs")], {
    env: { ...process.env, PORT: String(PORT) }, stdio: ["ignore", "pipe", "pipe"],
  });
  await new Promise((resolve, reject) => {
    server.stdout.on("data", (data) => { if (String(data).includes("http://")) resolve(); });
    server.on("exit", (code) => reject(new Error(`serve.cjs exited with ${code}`)));
  });
});

after(() => { server.kill(); });

test("serves repository files to its own host names", async () => {
  for (const host of [`127.0.0.1:${PORT}`, `localhost:${PORT}`]) {
    const response = await get("/crates/zipp-wasm/playground/index.html", host);
    assert.equal(response.status, 200, host);
    assert.match(response.body, /<title>/);
  }
});

test("refuses a request for another host (DNS rebinding)", async () => {
  for (const host of [`rebind.attacker.example:${PORT}`, "127.0.0.1", `127.0.0.1:${PORT + 1}`, `localhost.attacker.example:${PORT}`]) {
    assert.equal((await get("/README.md", host)).status, 421, JSON.stringify(host));
  }
});

test("never serves dot-files or dot-folders", async () => {
  for (const target of ["/landing/.dev.vars", "/.git/HEAD", "/.github/workflows/ci.yml", "/crates/%2e%2e%2f.git/config", "/.env"]) {
    assert.equal((await get(target)).status, 403, target);
  }
});

test("a NUL byte or traversal is answered and the server keeps running", async () => {
  assert.equal((await get("/README.md%00")).status, 400);
  assert.equal((await get("/..%2f..%2fsecret.txt")).status, 403);
  assert.equal((await get("/README.md")).status, 200);
  assert.equal(server.exitCode, null);
});
