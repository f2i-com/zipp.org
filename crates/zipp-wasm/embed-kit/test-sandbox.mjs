#!/usr/bin/env node
// node test-sandbox.mjs <kit-or-source-dir> <modules-dir>
//
// The sandbox contract of zipp.mjs, in-process and isolated (a worker
// thread), against the release modules in <modules-dir>:
//   - nothing is permitted by default: no storage, files, network, host
//     functions or ambient Node/browser objects;
//   - each permission grants exactly what it says (private storage with a
//     quota, read-only or writable files, network to allowed URLs only, with
//     method, redirect and size rules, served by a local HTTP server here);
//   - host functions answer host.call;
//   - the instruction limit stops a program and ends its sandbox; the time
//     limit ends an isolated sandbox running a loop that never yields, and
//     the next sandbox is unaffected;
//   - Python runs with its files as modules, and an engine loaded with the
//     torch package gives each isolated sandbox torch;
//   - unknown options are refused rather than ignored.
import assert from "node:assert/strict";
import fs from "node:fs";
import http from "node:http";
import path from "node:path";
import { pathToFileURL } from "node:url";

const [kitDir, modulesDir] = process.argv.slice(2);
if (!kitDir || !modulesDir) {
  console.error("usage: test-sandbox.mjs <kit-or-source-dir> <modules-dir>");
  process.exit(2);
}
const { loadZipp, ZippSandboxError } = await import(pathToFileURL(path.join(kitDir, "zipp.mjs")));
const find = suffix => {
  const file = fs.readdirSync(modulesDir).find(f => f.endsWith(suffix));
  assert.ok(file, `no *${suffix} in ${modulesDir}`);
  return path.join(modulesDir, file);
};

const server = http.createServer((req, res) => {
  if (req.url === "/data.json") {
    res.writeHead(200, { "content-type": "application/json", "x-zipp": "yes" });
    res.end(JSON.stringify({ hello: "world", cookie: req.headers.cookie ?? null, method: req.method }));
  } else if (req.url === "/echo" && req.method === "POST") {
    let body = "";
    req.on("data", c => { body += c; });
    req.on("end", () => { res.writeHead(201); res.end(`got ${body} ${req.headers["x-token"]}`); });
  } else if (req.url === "/redirect") {
    res.writeHead(302, { location: "http://localhost:1/elsewhere" });
    res.end();
  } else if (req.url.startsWith("/hop/")) {
    // Redirects for `redirects: true`: to a denied origin, a relative chain,
    // a loop, the method-changing and method-keeping statuses, and a hop to
    // another allowed origin.
    const to = {
      "/hop/denied": [302, `${deniedOrigin}/secret`],
      "/hop/a": [302, "/hop/b"],
      "/hop/b": [301, "../data.json"],
      "/hop/loop": [302, "/hop/loop"],
      "/hop/303": [303, "/echo-any"],
      "/hop/307": [307, "/echo-any"],
      "/hop/cross": [302, `${otherOrigin}/echo-any`],
    }[req.url];
    req.resume();
    res.writeHead(to ? to[0] : 404, to ? { location: to[1] } : {});
    res.end();
  } else if (req.url === "/echo-any") {
    echoAny(req, res);
  } else if (req.url === "/slow") {
    // Answers after 5 s, unless the client gives up first (counted).
    const timer = setTimeout(() => { res.writeHead(200); res.end("slow"); }, 5000);
    res.on("close", () => { clearTimeout(timer); if (!res.writableEnded) slowAbandoned++; });
  } else if (req.url === "/big") {
    res.writeHead(200);
    res.end("x".repeat(2000));
  } else {
    res.writeHead(404);
    res.end("missing");
  }
});
// What a request arrived as: method, body, and the headers a redirect may drop.
function echoAny(req, res) {
  let body = "";
  req.on("data", c => { body += c; });
  req.on("end", () => {
    res.writeHead(200);
    res.end(`${req.method} ${body || "-"} ${req.headers.authorization ?? "-"} ${req.headers["content-type"] ?? "-"}`);
  });
}
// An origin the sandboxes are never allowed: it counts every request it gets.
let deniedHits = 0;
// Requests to /slow whose client gave up before the answer.
let slowAbandoned = 0;
const until = async (test, ms = 2000) => {
  for (const end = Date.now() + ms; !test() && Date.now() < end;) await new Promise(r => setTimeout(r, 10));
  return test();
};
const deniedServer = http.createServer((req, res) => { deniedHits++; res.writeHead(200); res.end("reached"); });
// A second allowed origin, for a cross-origin redirect.
const otherServer = http.createServer(echoAny);
const listen = s => new Promise(resolve => s.listen(0, "127.0.0.1", resolve));
await Promise.all([listen(server), listen(deniedServer), listen(otherServer)]);
const origin = `http://127.0.0.1:${server.address().port}`;
const deniedOrigin = `http://127.0.0.1:${deniedServer.address().port}`;
const otherOrigin = `http://127.0.0.1:${otherServer.address().port}`;

let checks = 0;
const ok = name => { checks++; console.log(`  ok   ${name}`); };

const js = await loadZipp(find("-javascript.wasm"));
for (const isolate of [false, true]) {
  const mode = isolate ? "isolated" : "in-process";
  const make = (options = {}) => js.createSandbox({ isolate, ...options });

  // Nothing by default.
  {
    const sb = await make();
    const { output } = await sb.run(`
      const probe = f => { try { return String(f()); } catch (e) { return e.message.split(":")[0]; } };
      console.log([typeof process, typeof require, typeof fetch, typeof document, typeof WebAssembly].join(","));
      console.log(probe(() => localStorage.getItem("k")), probe(() => host.callSync("fs.read", "a.txt")));
      host.call("net.fetch", ["${origin}/data.json", "{}"], r => console.log("net", r.ok, r.error.code));
      host.call("anything", [], r => console.log("host", r.ok, r.error.code));
    `);
    assert.deepEqual(output, ["undefined,undefined,undefined,undefined,undefined", "SecurityError SecurityError", "net false denied", "host false denied"]);
    await sb.close();
    ok(`${mode}: nothing is permitted by default`);
  }

  // Storage: private, seeded, quota-bound, visible to the host.
  {
    const sb = await make({ permissions: { storage: { initial: { theme: "dark" }, quota: 64 } } });
    const { output } = await sb.run(`
      console.log(localStorage.getItem("theme"), localStorage.getItem("none"));
      localStorage.setItem("count", "1");
      let over = "no";
      try { localStorage.setItem("big", "x".repeat(100)); } catch (e) { over = e.name || "error"; }
      console.log(over);
    `);
    assert.equal(output[0], "dark null");
    assert.notEqual(output[1], "no");
    assert.deepEqual((await sb.snapshot()).storage, { theme: "dark", count: "1" });
    const other = await make({ permissions: { storage: true } });
    assert.deepEqual((await other.run(`console.log(localStorage.getItem("theme"))`)).output, ["null"]);
    await sb.close();
    await other.close();
    ok(`${mode}: storage is private to its sandbox, seeded and bounded`);
  }

  // Files: read-only by default, writable when granted, never outside the map.
  {
    const ro = await make({ permissions: { files: { "data/a.txt": "alpha", "b.txt": "beta" } } });
    const { output } = await ro.run(`
      console.log(host.callSync("fs.read", "./data/a.txt"), host.callSync("fs.read", "c.txt"));
      console.log(JSON.stringify(host.callSync("fs.list")), JSON.stringify(host.callSync("fs.list", "data")), host.callSync("fs.exists", "b.txt"));
      let w = "allowed";
      try { host.callSync("fs.write", "c.txt", "x"); } catch (e) { w = e.message.split(":")[0]; }
      let up = "read";
      try { host.callSync("fs.read", "../secret"); } catch (e) { up = "refused"; }
      console.log(w, up);
    `);
    assert.deepEqual(output, ["alpha null", '["b.txt","data/a.txt"] ["data/a.txt"] true', "SecurityError refused"]);
    const rw = await make({ permissions: { files: { files: { "a.txt": "1" }, writable: true } } });
    await rw.run(`host.callSync("fs.write", "out/report.txt", "done"); host.callSync("fs.remove", "a.txt");`);
    assert.deepEqual((await rw.snapshot()).files, { "out/report.txt": "done" });
    await ro.close();
    await rw.close();
    ok(`${mode}: files are read-only unless writable, and confined to their map`);
  }

  // Network: allowed URLs only, no cookies, methods, redirects and size bounded.
  {
    const sb = await make({ permissions: { network: { allow: [`${origin}/`], methods: ["GET", "POST"], maxResponseBytes: 1000 } } });
    const { output } = await sb.run(`
      async function main() {
        const r = await fetch("${origin}/data.json", { headers: { cookie: "session=1" } });
        const body = await r.json();
        console.log(r.status, r.ok, r.headers.get("x-zipp"), body.hello, body.method, body.cookie);
        const p = await fetch("${origin}/echo", { method: "POST", body: "payload", headers: { "x-token": "t" } });
        console.log(p.status, await p.text());
        for (const [url, init] of [["https://example.com/", {}], ["${origin}/data.json", { method: "DELETE" }], ["${origin}/redirect", {}], ["${origin}/big", {}]]) {
          try { await fetch(url, init); console.log("allowed", url); } catch (e) { console.log(e.name); }
        }
      }
      main();
    `);
    assert.equal(output[0], "200 true yes world GET null");
    assert.equal(output[1], "201 got payload t");
    assert.deepEqual(output.slice(2), ["TypeError", "TypeError", "TypeError", "TypeError"]);
    await sb.close();
    ok(`${mode}: fetch reaches allowed URLs only, within its method, redirect and size rules`);
  }

  // Redirects, when permitted: each hop's destination is authorized before
  // it is requested (a denied origin is never contacted), within a hop limit,
  // with the Fetch standard's method rules and credentials kept to their
  // origin.
  {
    const sb = await make({ permissions: { network: { allow: [`${origin}/`, `${otherOrigin}/`], redirects: true } } });
    const before = deniedHits;
    const { output } = await sb.run(`
      async function show(path, init) {
        try { const r = await fetch("${origin}" + path, init); console.log(r.status, r.url.replace(/^http:\\/\\/[^/]+/, ""), await r.text()); }
        catch (e) { console.log(e.name, e.message.replace(/127\\.0\\.0\\.1:\\d+/, "HOST")); }
      }
      async function main() {
        await show("/hop/denied");
        await show("/hop/a");
        await show("/hop/loop");
        await show("/hop/303", { method: "POST", body: "p", headers: { authorization: "a", "content-type": "text/plain" } });
        await show("/hop/307", { method: "POST", body: "p", headers: { authorization: "a", "content-type": "text/plain" } });
        await show("/hop/cross", { headers: { authorization: "a" } });
      }
      main();
    `);
    assert.equal(deniedHits, before, "the denied origin was contacted");
    assert.deepEqual(output, [
      "TypeError fetch: the request was redirected to http://HOST, which is not permitted",
      '200 /data.json {"hello":"world","cookie":null,"method":"GET"}',
      "TypeError fetch: more than 5 redirects",
      "200 /echo-any GET - a -",
      "200 /echo-any POST p a text/plain",
      "200 /echo-any GET - - -",
    ]);
    await sb.close();
    // A browser's fetch answers a manual redirect with an opaque response
    // that hides its destination: the redirect is refused, not followed.
    const opaque = await make({ permissions: { network: { allow: [`${origin}/`], redirects: true, fetch: async () => ({ type: "opaqueredirect", status: 0, url: "", headers: new Headers(), body: null }) } } });
    const hidden = await opaque.run(`fetch("${origin}/hop/a").then(() => console.log("followed"), e => console.log(e.name, e.message));`);
    assert.deepEqual(hidden.output, ["TypeError fetch: the request was redirected, and this environment's fetch does not say where, so the redirect cannot be checked"]);
    await opaque.close();
    ok(`${mode}: a permitted redirect is checked before it is requested, within a hop limit`);
  }

  // Host functions.
  {
    const sb = await make({ host: { "time.add": async (a, b) => Number(a) + Number(b), "fail.now": () => { throw new Error("no"); } } });
    const { output } = await sb.run(`
      host.call("time.add", [2, 3], r => console.log("add", r.ok, r.value));
      host.call("fail.now", [], r => console.log("fail", r.ok, r.error.message));
    `);
    assert.deepEqual(output.sort(), ["add true 5", "fail false no"]);
    await sb.close();
    ok(`${mode}: host functions answer host.call`);
  }

  // Host requests belong to their sandbox: at most 64 run at once, the rest
  // waiting in order; a time limit gives a request up, aborting its signal
  // (`this.signal`, a fetch's own); one that ignores the signal keeps its
  // place until it returns, and its late answer is discarded.
  {
    let running = 0, maxRunning = 0, slowAborted = 0, stubbornStarted = 0;
    let release;
    const stubbornDone = new Promise(resolve => { release = resolve; });
    const sb = await make({
      limits: { timeoutMs: 700 },
      permissions: { network: { allow: [`${origin}/`] } },
      host: {
        "wide.op": async function (n) {
          running++;
          maxRunning = Math.max(maxRunning, running);
          await new Promise(resolve => setTimeout(resolve, 5));
          running--;
          return Number(n) * 2;
        },
        "slow.op": function () {
          return new Promise((resolve, reject) => {
            this.signal.addEventListener("abort", () => { slowAborted++; reject(this.signal.reason); });
          });
        },
        "stubborn.op": function () { stubbornStarted++; return stubbornDone.then(() => "late"); },
      },
    });
    await sb.run(`
      var got = [];
      function wide() { got = []; for (var i = 0; i < 200; i++) host.call("wide.op", [String(i)], function (r) { got.push(r.value); }); }
      function total() { var s = 0; for (var i = 0; i < got.length; i++) s += got[i]; return got.length + " " + s; }
      function slow() { host.call("slow.op", [], function (r) { got.push("slow " + r.ok); }); }
      function slowFetch() { fetch("${origin}/slow").then(function () { got.push("fetched"); }, function () { got.push("failed"); }); }
      function stubborn() { got = []; host.call("stubborn.op", [], function (r) { got.push(r.value); }); }
      function seen() { return got.join(","); }
    `);
    await sb.call("wide");
    assert.equal((await sb.call("total")).value, "200 39800");
    assert.ok(maxRunning > 1 && maxRunning <= 64, `at most 64 at once, saw ${maxRunning}`);
    await assert.rejects(sb.call("slow"), e => e.category === "timeout" && !e.terminal);
    assert.ok(await until(() => slowAborted === 1), "the abandoned host function's signal aborted");
    const abandonedBefore = slowAbandoned;
    await assert.rejects(sb.call("slowFetch"), e => e.category === "timeout");
    assert.ok(await until(() => slowAbandoned === abandonedBefore + 1), "the abandoned fetch was cancelled");
    await assert.rejects(sb.call("stubborn"), e => e.category === "timeout");
    assert.equal(stubbornStarted, 1);
    assert.equal((await sb.usage()).hostRequestsRunning, 1, "a request that ignores its signal keeps its place");
    release();
    let left = 1;
    for (const end = Date.now() + 2000; left && Date.now() < end;) {
      left = (await sb.usage()).hostRequestsRunning;
      if (left) await new Promise(resolve => setTimeout(resolve, 10));
    }
    assert.equal(left, 0, "it gives its place back when it returns");
    assert.equal((await sb.call("seen")).value, "", "its late answer is discarded");
    await sb.close();
    ok(`${mode}: host requests are bounded, abandoned at the time limit, and kept until they end`);
  }

  // close() ends a run waiting on host requests at once, aborts the running
  // ones and starts none of those still waiting.
  {
    let started = 0, aborted = 0;
    const sb = await make({
      limits: { timeoutMs: 20000 },
      host: {
        "hang.op": function () {
          started++;
          return new Promise((resolve, reject) => {
            this.signal.addEventListener("abort", () => { aborted++; reject(this.signal.reason); });
          });
        },
      },
    });
    const t0 = Date.now();
    const running = sb.run(`for (var i = 0; i < 70; i++) host.call("hang.op", [], function () {});`);
    assert.ok(await until(() => started === 64), `64 started, saw ${started}`);
    await sb.close();
    await assert.rejects(running, e => e.category === "closed" && e.terminal);
    assert.ok(Date.now() - t0 < 5000, "close() settled the run before its time limit");
    assert.ok(await until(() => aborted === 64), `every running request aborted, saw ${aborted}`);
    assert.equal(started, 64, "none of the waiting requests started");
    // Two calls waiting at once both settle.
    started = 0;
    const two = await make({ limits: { timeoutMs: 20000 }, host: { "hang.op": function () { started++; return new Promise(() => {}); } } });
    await two.run(`function hang() { host.call("hang.op", [], function () {}); }`);
    const t1 = Date.now();
    const calls = [two.call("hang"), two.call("hang")];
    assert.ok(await until(() => started === 2), `both calls' requests started, saw ${started}`);
    await two.close();
    for (const c of calls) await assert.rejects(c, e => e.category === "closed");
    assert.ok(Date.now() - t1 < 5000, "close() settled both calls before their time limit");
    ok(`${mode}: close() settles a waiting run at once and aborts its host requests`);
  }

  // Calls and globals persist; the instruction limit ends the sandbox.
  {
    const sb = await make({ limits: { instructions: 2_000_000 } });
    await sb.run("let total = 0; function add(n) { total += n; return total; } function spin() { for (;;) {} }");
    assert.equal((await sb.call("add", [2])).value, 2);
    assert.equal((await sb.call("add", [3])).value, 5);
    await assert.rejects(sb.call("spin"), e => e instanceof ZippSandboxError && e.category === "resource" && e.terminal);
    await assert.rejects(sb.call("add", [1]), e => e.terminal);
    await sb.close();
    ok(`${mode}: functions keep state; the instruction limit ends the sandbox`);
  }

  // Unknown options are refused, not ignored.
  await assert.rejects(make({ permissions: { netwrok: true } }), /unknown permissions option "netwrok"/);
  await assert.rejects(make({ permissions: { network: ["ftp://x"] } }), /not an http\(s\) URL/);
  ok(`${mode}: unknown or malformed options are refused`);
}

// Only isolation can stop a program that never yields.
{
  const started = Date.now();
  const sb = await js.createSandbox({ limits: { timeoutMs: 500, instructions: 2_000_000_000 } });
  await sb.run("function spin() { for (;;) {} }");
  await assert.rejects(sb.call("spin"), e => e.category === "timeout" && e.terminal);
  assert.ok(Date.now() - started < 10000);
  assert.equal(sb.closed, true);
  const next = await js.createSandbox();
  assert.deepEqual((await next.run("console.log('still here')")).output, ["still here"]);
  await next.close();
  ok("isolated: the time limit terminates a loop that never yields; the next sandbox runs");
}

// Python, with modules, and the torch package added under isolation.
{
  const base = await loadZipp({ engine: find("-javascript-python-base.wasm"), packages: [find("-torch-package.wasm")] });
  for (const isolate of [false, true]) {
    const sb = await base.createSandbox({ language: "python", isolate, permissions: { files: { "helpers.py": "def double(x):\n    return 2 * x\n" } } });
    const { output } = await sb.run("import helpers\nimport torch\nprint(helpers.double(21), (torch.tensor([1.0, 2.0]) * 3).tolist())");
    assert.deepEqual(output, ["42 [3.0, 6.0]"]);
    await sb.close();
  }
  await assert.rejects(base.createSandbox({ language: "python", permissions: { network: true } }), /Python program reaches no storage, network/);
  ok("python: files are modules, and the engine's torch package reaches isolated sandboxes");
}

server.close();
deniedServer.close();
otherServer.close();
console.log(`${checks} sandbox checks passed`);
