// Regressions from the 23 September 2026 host-boundary review.
"use strict";
const zipp = require("./pkg/zipp_wasm.js");
const { Engine } = zipp;

let pass = 0;
function same(label, got, want) {
  const actual = JSON.stringify(got);
  const expected = JSON.stringify(want);
  if (actual !== expected) throw new Error(`${label}: got ${actual}, want ${expected}`);
  pass++;
  console.log(`  ok   ${label}`);
}
function caught(fn) { try { fn(); return null; } catch (error) { return String(error); } }
function close(engine) { try { engine.dispose(); } catch {} try { engine.free(); } catch {} }

// takeFailedConsole pages like takeConsole: nothing past the first page is lost.
{
  const engine = new Engine();
  engine.setInstructionBudget(2_000_000_000);
  caught(() => engine.initScript('for (let i = 0; i < 800000; i++) console.log(""); throw new Error("x")'));
  let total = 0, pages = 0;
  for (;;) { const page = engine.takeFailedConsole(); if (!page.length) break; total += page.length; pages++; }
  same("a failed init's console comes back whole, over several pages", [total, pages > 1], [800000, true]);
  close(engine);
}

// A failed init's retained dynamic code is part of the instance totals, and
// every engine is counted once as disposed, initialized or not.
{
  const before = JSON.parse(JSON.stringify(zipp.zippInstanceUsage()));
  const failed = new Engine();
  caught(() => failed.initScript('for (let i = 0; i < 50; i++) new Function("return " + i); throw new Error("fail")'));
  close(failed);
  const bare = new Engine();
  close(bare);
  const after = zipp.zippInstanceUsage();
  same("a failed init's retained functions are counted",
    after.retainedFunctions - before.retainedFunctions >= 50, true);
  same("failed and never-initialized engines are both disposed once",
    after.enginesDisposed - before.enginesDisposed, 2);
}

// Keys cannot carry a lone surrogate, so two of them are refused rather than
// merged into one U+FFFD key with a value silently lost.
{
  const engine = new Engine();
  engine.initScript("function keys(o) { return Object.keys(o).length; }");
  const error = caught(() => engine.callFunction("keys", [{ "\uD800": 1, "\uDC00": 2 }]));
  same("a host key with a lone surrogate is refused", /lone surrogate/.test(error ?? ""), true);
  same("an ordinary key still crosses", engine.callFunction("keys", [{ a: 1, "�": 2 }]), 2);
  close(engine);
}

// A mistake in the host's own call is classified as usage, not as whatever
// the previous error was.
{
  const engine = new Engine();
  const slots = engine.initScript("var a = 1; function boom() { throw new Error('guest'); }");
  const after = fn => { caught(() => engine.callFunction("boom", [])); caught(fn); return engine.lastErrorKind(); };
  same("setGlobalsBatch length mismatch", after(() => engine.setGlobalsBatch([slots.a.index], [1, 2])), "usage");
  same("setGlobalsBatch duplicate index", after(() => engine.setGlobalsBatch([slots.a.index, slots.a.index], [1, 2])), "usage");
  same("resolveHostCallback bad id", after(() => engine.resolveHostCallback(-1, null)), "usage");
  same("cancelHostCallback bad id", after(() => engine.cancelHostCallback(NaN)), "usage");
  close(engine);
  const unknown = new Engine();
  caught(() => unknown.initSource("x", "ruby"));
  same("initSource with an unknown language", unknown.lastErrorKind(), "usage");
  close(unknown);
}

console.log(`audit-2026-09-23: ${pass} checks passed`);
