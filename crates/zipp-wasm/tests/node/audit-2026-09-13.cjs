// Regressions from the v0.0.17 external host-boundary review.
"use strict";
const { Engine } = require("./pkg/zipp_wasm.js");

let pass = 0;
function same(label, got, want) {
  const actual = JSON.stringify(got);
  const expected = JSON.stringify(want);
  if (actual !== expected) throw new Error(`${label}: got ${actual}, want ${expected}`);
  pass++;
  console.log(`  ok   ${label}`);
}
function caught(fn) { try { fn(); } catch {} }
function close(engine) { try { engine.dispose(); } catch {} try { engine.free(); } catch {} }
function init(source) {
  const engine = new Engine();
  engine.setInstructionBudget(500_000_000);
  engine.initScript(source);
  return engine;
}

{
  const a = init("function fail() { throw new Error('a'); } function healthy() { return 1; }");
  const b = init("function fail() { throw new Error('b'); }");
  caught(() => a.callFunction("fail", []));
  caught(() => b.callFunction("noSuchFunction", []));
  same("error classification belongs to its Engine", [a.lastErrorKind(), b.lastErrorKind()], ["guest", "usage"]);
  a.callFunction("healthy", []);
  same("a successful call leaves that Engine's classification stale", a.lastErrorKind(), "guest");
  close(a); close(b);
}

{
  const engine = init(`
    var tooManyArgs = new Array(2100000).fill("");
    function pushOversize(n) {
      for (var i = 0; i < n; i++) {
        var id = ++__zHostId;
        __zHostQueue.push({ id: id, kind: "big", args: tooManyArgs });
        __zHostCbs[id] = function () {};
        __zHostPending++;
      }
    }
    function queued() { return __zHostQueue.length; }
  `);
  engine.callFunction("pushOversize", [100]);
  const status = engine.drainPendingHostCallsStatus();
  same(
    "a rejection-only bounded pass reports the remaining queue",
    [status.calls.length, status.hasMore, status.stopReason, engine.callFunction("queued", []) > 0],
    [0, true, "work-limit", true],
  );
  close(engine);
}

console.log(`\n${pass} passed, 0 failed`);
