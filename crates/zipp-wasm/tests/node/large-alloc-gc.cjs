// Collections must be scheduled on bytes, not only on allocation count.
//
// The rest of the schedule counts allocations: a minor every
// NURSERY_YOUNG_BUDGET (16,384) of them, a major every 64 minors. An embedder's
// memory ceiling is in bytes, and the two units come apart as objects grow. At
// 2 KiB an object the count-based schedule keeps pace; at 4 KiB, 16,384
// allocations is 67 MB and a 128 MiB budget is spent before a minor is due.
//
// The symptom was an emulator: a 23 KB framebuffer encoded per frame reached
// the ceiling in roughly 1,400 allocations and the engine was torn down about
// ninety seconds into a game.
const { Engine } = require("./pkg/zipp_wasm.js");

let pass = 0;
let fail = 0;
function check(label, ok, detail) {
  if (ok) { pass++; console.log(`  ok   ${label}`); }
  else { fail++; console.log(`  FAIL ${label}${detail ? " — " + detail : ""}`); }
}

// Churn `total` bytes in `size`-byte pieces, keeping nothing alive. With the
// collector reaching them, total is irrelevant and this always completes.
function churn(size, totalBytes) {
  const e = new Engine();
  const r = e.initScript(`
    let t = null
    let sink = 0
    function churn(k, size) {
      let i = 0
      while (i < k) { t = new Uint8Array(size); sink = t.length; i = i + 1 }
      return sink
    }
  `);
  if (r && r.error) throw new Error("initScript: " + r.error);
  const iters = Math.floor(totalBytes / size);
  const batch = Math.max(1, Math.min(20000, Math.floor(iters / 40)));
  let done = 0;
  let failure = "";
  try {
    while (done < iters) {
      // Renew, so the instruction budget can never be what stops this. The
      // question here is only whether memory comes back.
      e.renewInstructionBudget();
      e.callFunction("churn", [batch, size]);
      done += batch;
    }
  } catch (err) {
    failure = String(err && err.message ? err.message : err);
  }
  try { e.dispose(); } catch {}
  return { done, bytes: done * size, failure };
}

const TOTAL = 384 * 1024 * 1024; // three times the 128 MiB budget

// Every size the collector must reach. 4 KiB is where it used to stop.
for (const size of [512, 2048, 4096, 8192, 23040, 32768]) {
  const r = churn(size, TOTAL);
  check(
    `${String(size).padStart(5)} byte allocations churn ${(TOTAL / 1048576) | 0} MB`,
    r.failure === "",
    `stopped after ${(r.bytes / 1048576).toFixed(0)} MB: ${r.failure}`
  );
}

// The emulator's actual shape: a framebuffer encoded to base64 every frame,
// both allocations above the threshold, for well over the ninety seconds that
// used to kill it.
{
  const e = new Engine();
  const r = e.initScript(`
    let fb = new Uint8Array(23040)
    let sink = 0
    function frame() { sink = fb.toBase64().length; return sink }
  `);
  if (r && r.error) throw new Error("initScript: " + r.error);
  let frames = 0;
  let failure = "";
  try {
    for (let i = 0; i < 60 * 5; i++) {       // 300 batches of 60 frames
      e.renewInstructionBudget();
      e.callFunction("frame", []);
      for (let f = 1; f < 60; f++) e.callFunction("frame", []);
      frames += 60;
    }
  } catch (err) {
    failure = String(err && err.message ? err.message : err);
  }
  check(
    `${frames / 60} seconds of framebuffer encoding at 60fps`,
    failure === "",
    `stopped at ${(frames / 60).toFixed(0)}s: ${failure}`
  );
  try { e.dispose(); } catch {}
}

// The ceiling must still exist: retained large buffers are a real leak and have
// to be stopped, or this "fix" would just be removing the guard.
{
  const e = new Engine();
  const r = e.initScript(`
    let held = []
    function hoard(n) {
      let i = 0
      try { while (i < n) { held.push(new Uint8Array(1024 * 1024)); i = i + 1 } return "survived" }
      catch (err) { return "stopped:" + held.length }
    }
  `);
  if (r && r.error) throw new Error("initScript: " + r.error);
  let verdict;
  try {
    e.renewInstructionBudget();
    verdict = "returned:" + e.callFunction("hoard", [512]);
  } catch (err) {
    verdict = "host-threw:" + String(err && err.message ? err.message : err);
  }
  check("512 RETAINED 1 MB buffers are still stopped", /memory budget/i.test(verdict), verdict);
  try { e.dispose(); } catch {}
}

console.log(`\n${pass} passed, ${fail} failed`);
if (fail > 0) process.exit(1);
