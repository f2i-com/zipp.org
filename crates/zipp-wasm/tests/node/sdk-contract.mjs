// The reference host adapter's main-thread contract (host-sdk/zipp-host.mjs)
// under a mocked Worker and deterministic timers: no engine, no browser, a
// few milliseconds. It pins the state handling the 11 September 2026 close
// audit's ZA-01/02/03 found wanting, with the Worker's replies scripted:
//
//   ZA-01  categories come from the engine's structured envelope, never
//          from message text; only an envelope marked terminal (or a
//          deadline/termination) kills the host.
//   ZA-02  a request whose payload structured clone refuses is rejected
//          locally with `usage`, retains no timer or pending entry, and the
//          host is still ready before and after its former deadline.
//   ZA-03  `dead` is monotonic (an initialization reply in flight cannot
//          resurrect a terminated host); ordinary operations before `ready`
//          are refused locally and nothing is posted; deadlines are
//          validated; every promise settles exactly once.
//
// The Worker half is exercised by tests/browser/ against the real package.
import assert from "node:assert/strict";
import { createZippHost, categorizeEngineError, ZIPP_HOST_SDK_VERSION, MAX_DEADLINE_MS } from "../../host-sdk/zipp-host.mjs";

let pass = 0, fail = 0;
async function test(name, fn) {
  const saved = { Worker: globalThis.Worker, setTimeout: globalThis.setTimeout, clearTimeout: globalThis.clearTimeout };
  const timers = new Map();
  let timerId = 0;
  globalThis.Worker = FakeWorker;
  globalThis.setTimeout = (callback, delay) => { const id = ++timerId; timers.set(id, { callback, delay }); return id; };
  globalThis.clearTimeout = (id) => { timers.delete(id); };
  const ctx = {
    timers,
    fireAll() { for (const [id, t] of [...timers]) { if (timers.delete(id)) t.callback(); } },
    make(extra = {}) {
      const host = createZippHost({ moduleUrl: "m", wasmUrl: "w", workerUrl: "k", deadlineMs: 50, ...extra });
      return { host, worker: FakeWorker.instances.at(-1) };
    },
    async ready(extra) {
      const pair = this.make(extra);
      const p = pair.host.init("function f() { return 1; }");
      pair.worker.reply(pair.worker.sent.at(-1), { value: { f: { index: 1 } } });
      await p;
      return pair;
    },
  };
  try { await fn(ctx); pass++; console.log(`  ok   ${name}`); }
  catch (e) { fail++; console.log(`  FAIL ${name} — ${e && e.stack || e}`); }
  finally { Object.assign(globalThis, saved); timers.clear(); }
}
class FakeWorker {
  static instances = [];
  constructor() { this.handlers = new Map(); this.sent = []; this.terminated = false; FakeWorker.instances.push(this); }
  addEventListener(type, fn) { const a = this.handlers.get(type) || []; a.push(fn); this.handlers.set(type, a); }
  postMessage(message) {
    if (this.terminated) throw new Error("fake worker already terminated");
    this.sent.push(structuredClone(message)); // throws DataCloneError for a function
  }
  // Script a reply to a sent message: `{ value }` or `{ error }`.
  reply(message, outcome) {
    const data = outcome.error
      ? { gen: message.gen, id: message.id, ok: false, error: outcome.error }
      : { gen: message.gen, id: message.id, ok: true, value: outcome.value };
    for (const fn of this.handlers.get("message") || []) fn({ data });
  }
  crash(message) { for (const fn of this.handlers.get("error") || []) fn({ message }); }
  terminate() { this.terminated = true; }
}
const settled = (p) => p.then((value) => ({ ok: true, value }), (error) => ({ ok: false, error }));

console.log(`sdk ${ZIPP_HOST_SDK_VERSION}`);

// ── ZA-01 ──────────────────────────────────────────────────────────────────
await test("the classifier reads the engine's envelope, never the message", async () => {
  for (const text of ["Error: business limit reached", "Error: budget forecast missing", "Error: unlimited mode unavailable", "RangeError: instruction budget exceeded"]) {
    assert.equal(categorizeEngineError(text, "call"), "guest", text);
    assert.equal(categorizeEngineError({ kind: "guest", terminal: false }, "call"), "guest");
  }
  assert.equal(categorizeEngineError({ kind: "resource", terminal: true }, "call"), "resource");
  assert.equal(categorizeEngineError({ kind: "conversion", terminal: false }, "read"), "conversion");
  assert.equal(categorizeEngineError({ kind: "usage", terminal: false }, "call"), "usage");
  assert.equal(categorizeEngineError({ kind: "source", terminal: true }, "init"), "source");
  assert.equal(categorizeEngineError({ kind: "guest", terminal: false }, "init"), "source");
});
await test("a guest error whose text names a limit keeps the host usable", async (ctx) => {
  const { host, worker } = await ctx.ready();
  const p = settled(host.call("f"));
  worker.reply(worker.sent.at(-1), { error: { category: "guest", message: "Error: business limit reached", terminal: false } });
  const r = await p;
  assert.equal(r.ok, false);
  assert.equal(r.error.category, "guest");
  assert.equal(r.error.terminal, false);
  assert.equal(host.state, "ready");
  assert.equal(worker.terminated, false);
  const again = settled(host.call("f"));
  worker.reply(worker.sent.at(-1), { value: 7 });
  assert.deepEqual(await again, { ok: true, value: 7 });
  host.terminate();
});
await test("a recoverable conversion failure keeps the host usable", async (ctx) => {
  const { host, worker } = await ctx.ready();
  const p = settled(host.readGlobals([1]));
  worker.reply(worker.sent.at(-1), { error: { category: "conversion", message: "RangeError: host value exceeds the conversion string limit (16777216 bytes)", terminal: false } });
  const r = await p;
  assert.equal(r.error.category, "conversion");
  assert.equal(host.state, "ready");
  host.terminate();
});
await test("an envelope the engine marks terminal kills the host once and rejects everything pending once", async (ctx) => {
  const { host, worker } = await ctx.ready();
  const first = settled(host.call("f"));
  const second = settled(host.call("f"));
  assert.equal(host.pendingRequests, 2);
  worker.reply(worker.sent.at(-2), { error: { category: "resource", message: "RangeError: instruction budget exceeded", terminal: true } });
  const a = await first, b = await second;
  assert.equal(a.error.category, "resource");
  assert.equal(a.error.terminal, true);
  assert.equal(b.error.category, "resource", "the other pending request dies with the host");
  assert.equal(host.state, "dead");
  assert.equal(host.deathCause.category, "resource");
  assert.equal(worker.terminated, true);
  assert.equal(host.pendingRequests, 0);
  assert.equal(ctx.timers.size, 0, "no timer outlives the death");
  const late = await settled(host.call("f"));
  assert.equal(late.error.category, "usage");
});
await test("a Worker crash is a terminal host error", async (ctx) => {
  const { host, worker } = await ctx.ready();
  const p = settled(host.call("f"));
  worker.crash("boom");
  assert.equal((await p).error.category, "host");
  assert.equal(host.deathCause.category, "host");
});

// ── ZA-02 ──────────────────────────────────────────────────────────────────
await test("a payload structured clone refuses is rejected locally, cleanly", async (ctx) => {
  const { host, worker } = await ctx.ready();
  const before = worker.sent.length;
  for (const bad of [() => 1, Symbol("s"), { get x() { throw new Error("getter threw"); } }]) {
    const r = await settled(host.call("f", [bad]));
    assert.equal(r.ok, false);
    assert.equal(r.error.category, "usage");
    assert.match(r.error.message, /could not be sent/);
    assert.equal(host.state, "ready");
    assert.equal(host.pendingRequests, 0, "no pending entry survives a failed send");
    assert.equal(ctx.timers.size, 0, "no timer survives a failed send");
    assert.equal(worker.sent.length, before, "nothing was posted");
  }
  // A valid call works now...
  const ok1 = settled(host.call("f", [1]));
  worker.reply(worker.sent.at(-1), { value: "first" });
  assert.deepEqual(await ok1, { ok: true, value: "first" });
  // ...and after the rejected requests' former deadlines would have fired.
  ctx.fireAll();
  assert.equal(host.state, "ready", "no retained deadline killed the host");
  assert.equal(worker.terminated, false);
  const ok2 = settled(host.call("f", [2]));
  worker.reply(worker.sent.at(-1), { value: "second" });
  assert.deepEqual(await ok2, { ok: true, value: "second" });
  host.terminate();
});
await test("a throwing transport does not disturb an unrelated pending request", async (ctx) => {
  const { host, worker } = await ctx.ready();
  const good = settled(host.call("f", [1]));
  const goodMessage = worker.sent.at(-1);
  assert.equal(ctx.timers.size, 1);
  worker.postMessage = () => { throw new Error("transport down"); };
  const bad = await settled(host.call("f", [2]));
  assert.equal(bad.error.category, "usage");
  assert.equal(ctx.timers.size, 1, "the good request's timer is untouched");
  assert.equal(host.pendingRequests, 1);
  worker.reply(goodMessage, { value: "still delivered" });
  assert.deepEqual(await good, { ok: true, value: "still delivered" });
  assert.equal(ctx.timers.size, 0);
  host.terminate();
});
await test("unprintable clone errors still release their request and deadline", async (ctx) => {
  const { host, worker } = await ctx.ready();
  const errors = [
    { get message() { throw new Error("message getter failed"); } },
    { message: { toString() { throw new Error("message conversion failed"); } } },
    { get name() { throw new Error("name getter failed"); }, message: "clone refused" },
    { toString() { throw new Error("string conversion failed"); } },
    Proxy.revocable({}, {}).proxy,
  ];
  const revoked = Proxy.revocable({}, {});
  revoked.revoke();
  errors.push(revoked.proxy);
  for (const error of errors) {
    const before = worker.sent.length;
    const value = { get payload() { throw error; } };
    const result = await settled(host.call("f", [value]));
    assert.equal(result.ok, false);
    assert.equal(result.error.category, "usage");
    assert.equal(host.pendingRequests, 0);
    assert.equal(ctx.timers.size, 0);
    assert.equal(worker.sent.length, before);
    ctx.fireAll();
    assert.equal(host.state, "ready");
  }
  const good = settled(host.call("f"));
  worker.reply(worker.sent.at(-1), { value: "still usable" });
  assert.deepEqual(await good, { ok: true, value: "still usable" });
  host.terminate();
});

// ── ZA-03 ──────────────────────────────────────────────────────────────────
await test("initialization cannot resurrect a terminated host", async (ctx) => {
  const { host, worker } = ctx.make();
  const p = settled(host.init(""));
  worker.reply(worker.sent.at(-1), { value: {} }); // resolves the request, not the async continuation
  host.terminate();
  const r = await p;
  assert.equal(host.state, "dead");
  assert.equal(host.dead, true);
  assert.equal(host.deathCause.category, "terminated");
  assert.equal(r.ok, false, "init does not report success on a dead host");
  assert.equal(r.error.category, "terminated");
  assert.equal(worker.terminated, true);
});
await test("a late reply for a dead host settles nothing", async (ctx) => {
  const { host, worker } = await ctx.ready();
  const p = settled(host.call("f"));
  const msg = worker.sent.at(-1);
  host.terminate();
  const r = await p;
  assert.equal(r.error.category, "terminated");
  worker.reply(msg, { value: "late" }); // must be inert
  assert.equal(host.state, "dead");
});
await test("ordinary operations before ready are refused locally; nothing is posted", async (ctx) => {
  const { host, worker } = ctx.make();
  const r = await settled(host.call("f"));
  assert.equal(r.error.category, "usage");
  assert.match(r.error.message, /state created/);
  assert.equal(worker.sent.length, 0);
  const p = settled(host.init("function f() {}"));
  const during = await settled(host.readGlobals([0]));
  assert.equal(during.error.category, "usage");
  assert.match(during.error.message, /state initializing/);
  assert.equal(worker.sent.length, 1, "only the init message was posted");
  worker.reply(worker.sent.at(-1), { value: { f: { index: 0 } } });
  assert.equal((await p).ok, true);
  assert.equal(host.state, "ready");
  const ok = settled(host.call("f"));
  assert.equal(worker.sent.length, 2);
  worker.reply(worker.sent.at(-1), { value: 1 });
  assert.equal((await ok).value, 1);
  host.terminate();
});
await test("init twice is a usage error; a failed init is terminal as source", async (ctx) => {
  const { host, worker } = ctx.make();
  const p = settled(host.init("function ("));
  const second = await settled(host.init(""));
  assert.equal(second.error.category, "usage");
  worker.reply(worker.sent.at(-1), { error: { category: "source", message: "SyntaxError: ...", terminal: true } });
  const r = await p;
  assert.equal(r.error.category, "source");
  assert.equal(host.deathCause.category, "source");
});
await test("deadlines are validated, not silently converted", async (ctx) => {
  for (const bad of [0, -1, NaN, Infinity, "5", MAX_DEADLINE_MS + 1, null]) {
    assert.throws(() => createZippHost({ moduleUrl: "m", wasmUrl: "w", workerUrl: "k", deadlineMs: bad }), (e) => e.category === "usage", String(bad));
  }
  const { host, worker } = await ctx.ready();
  for (const bad of [0, -1, NaN, Infinity, "5", MAX_DEADLINE_MS + 1]) {
    const r = await settled(host.call("f", [], { deadlineMs: bad }));
    assert.equal(r.error.category, "usage", String(bad));
    assert.equal(ctx.timers.size, 0, "no timer was installed for a bad deadline");
    assert.equal(host.pendingRequests, 0);
  }
  assert.equal(worker.sent.length, 1, "nothing beyond init was posted");
  host.terminate();
});
await test("the deadline kills the host exactly once and every promise settles exactly once", async (ctx) => {
  const { host, worker } = await ctx.ready();
  let settledCount = 0;
  const a = host.call("f").then(() => settledCount++, () => settledCount++);
  const b = host.call("f").then(() => settledCount++, () => settledCount++);
  assert.equal(ctx.timers.size, 2);
  ctx.fireAll();
  await Promise.all([a, b]);
  assert.equal(settledCount, 2);
  assert.equal(host.deathCause.category, "deadline");
  assert.equal(worker.terminated, true);
  worker.reply(worker.sent.at(-1), { value: "late" }); // inert
  assert.equal(settledCount, 2);
  assert.equal(ctx.timers.size, 0);
});
await test("the configuration is frozen at creation", async (ctx) => {
  const capabilities = ["ls.getItem"];
  const { host, worker } = ctx.make({ capabilities });
  capabilities.push("db.query");
  const p = host.init("");
  assert.deepEqual(worker.sent.at(-1).capabilities, ["ls.getItem"]);
  worker.reply(worker.sent.at(-1), { value: {} });
  await p;
  host.terminate();
});

console.log(`\n${pass} passed, ${fail} failed`);
if (fail > 0) process.exit(1);
