// The hardened ArrayBuffer ceiling, and the guard that actually bounds memory.
//
// Raising a sandbox limit is only defensible if you can say what still stops a
// hostile script. The ceiling refuses ONE absurd request; the heap budget is
// what bounds total use. Both are pinned here, so a future change to either has
// to face the other.
//
// This lives in the Node suite rather than crates/zipp-vm/tests because
// safe-sandbox is only enabled in this crate's separate workspace — a Rust test
// gated on the feature would silently compile to nothing and never run.
const fs = require("node:fs");
const path = require("node:path");
const { Engine } = require("./pkg/zipp_wasm.js");

let pass = 0;
let fail = 0;
function check(label, ok, detail) {
  if (ok) {
    pass++;
    console.log(`  ok   ${label}`);
  } else {
    fail++;
    console.log(`  FAIL ${label}${detail ? " — " + detail : ""}`);
  }
}

const CEILING = 32 * 1024 * 1024;

function engine() {
  const e = new Engine();
  const r = e.initScript(`
    function alloc(n) {
      try { return "ok:" + new Uint8Array(n).length }
      catch (err) { return "err:" + err }
    }
    function allocF32(n) {
      try { return "ok:" + new Float32Array(n).length }
      catch (err) { return "err:" + err }
    }
    // Retained, so nothing can be collected between rounds.
    let held = []
    function hoard(chunkBytes, rounds) {
      let i = 0
      try {
        while (i < rounds) { held.push(new Uint8Array(chunkBytes)); i = i + 1 }
        return "survived:" + held.length
      } catch (err) {
        return "stopped:" + held.length
      }
    }
  `);
  if (r && r.error) throw new Error("initScript failed: " + r.error);
  return e;
}

// 1. The ceiling is where it says it is.
{
  const e = engine();
  check("a buffer exactly at the 32 MiB ceiling allocates", e.callFunction("alloc", [CEILING]) === "ok:" + CEILING);
  const over = e.callFunction("alloc", [CEILING + 1]);
  check("one byte over the ceiling is a RangeError", String(over).includes("exceeds the maximum"), String(over));
  e.dispose();
}

// 2. The two applications the ceiling was raised for.
{
  const e = engine();
  const cart = 8 * 1024 * 1024;
  check("an 8 MB Game Boy cartridge fits in one Uint8Array", e.callFunction("alloc", [cart]) === "ok:" + cart);
  // 267 * 1536 / 48000 = 8.544 s of 48 kHz mono f32.
  check("one 8.5 s audio frame fits in one Float32Array", e.callFunction("allocF32", [410112]) === "ok:410112");
  e.dispose();
}

// 3. THE IMPORTANT ONE. The ceiling only refuses a single absurd request; the
//    heap budget is what bounds the total. If this ever reports "survived", the
//    ceiling has become the only thing between a script and the host.
//
//    Caught on the HOST side, not in the script. A ceiling RangeError is an
//    ordinary throw the script can catch, but the memory-budget guard is a
//    resource guard: it tears the Engine down as it fires, so the error arrives
//    out of callFunction and never reaches the script's own try/catch. That
//    difference is the point — a script cannot swallow the budget guard.
{
  const e = engine();
  let verdict;
  try {
    verdict = "returned:" + String(e.callFunction("hoard", [8 * 1024 * 1024, 64]));
  } catch (err) {
    verdict = "host-threw:" + String(err && err.message ? err.message : err);
  }
  check(
    "64 x 8 MB (512 MB) is stopped by the memory budget",
    /memory budget/i.test(verdict) || verdict.startsWith("returned:stopped:"),
    verdict
  );
  check(
    "the budget guard is not catchable inside the script",
    verdict.startsWith("host-threw:"),
    verdict
  );
  try { e.dispose(); } catch { /* already torn down by the guard */ }
}

// 4. An absurd request is still refused outright, not attempted.
{
  const e = engine();
  for (const n of [64 * 1024 * 1024, 512 * 1024 * 1024, 2 * 1024 * 1024 * 1024]) {
    const out = String(e.callFunction("alloc", [n]));
    check(`${(n / 1048576).toFixed(0)} MB is refused by the ceiling`, out.includes("exceeds the maximum"), out);
  }
  e.dispose();
}

// 5. The engine survives having refused: a ceiling RangeError is catchable and
//    leaves the Engine usable, rather than tearing it down like a budget guard.
{
  const e = engine();
  e.callFunction("alloc", [2 * 1024 * 1024 * 1024]);
  check("the Engine still works after a refused allocation", e.callFunction("alloc", [1024]) === "ok:1024");
  e.dispose();
}

console.log(`\n${pass} passed, ${fail} failed`);
if (fail > 0) process.exit(1);
