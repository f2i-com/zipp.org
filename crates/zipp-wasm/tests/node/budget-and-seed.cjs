// Two host-controlled properties, both of which failed in the field.
//
// 1. The instruction budget is a lifetime total. That bounds a runaway script
//    and also puts a fuse on every long-running embedder — an emulator spent it
//    in seconds and its engine was disposed mid-frame. Renewal has to restore
//    the budget WITHOUT restoring anything else.
//
// 2. The fingerprint mixer is a chain of bijections, so an unkeyed digest can
//    be inverted and SOLVED for a collision. The pair below was constructed
//    that way against the unkeyed build; keyed, it must separate.
const { Engine } = require("./pkg/zipp_wasm.js");

let pass = 0;
let fail = 0;
function check(label, ok, detail) {
  if (ok) { pass++; console.log(`  ok   ${label}`); }
  else { fail++; console.log(`  FAIL ${label}${detail ? " — " + detail : ""}`); }
}

// ── 1. Budget renewal ──────────────────────────────────────────────────────
function burner() {
  const e = new Engine();
  const r = e.initScript("function burn(n){let i=0;let s=0;while(i<n){s=s+i;i=i+1}return s}");
  if (r && r.error) throw new Error("initScript: " + r.error);
  return e;
}

// Without renewal the budget is spent after about seven calls of ~8M steps.
{
  const e = burner();
  let calls = 0;
  try { for (let i = 0; i < 40; i++) { e.callFunction("burn", [1_000_000]); calls++; } } catch {}
  check("unrenewed, the lifetime budget still stops the engine", calls > 0 && calls < 40, `survived ${calls}`);
  try { e.dispose(); } catch {}
}

// Renewing before each call, the same work continues indefinitely.
{
  const e = burner();
  let calls = 0;
  let err = "";
  try {
    for (let i = 0; i < 60; i++) {
      check.renewed = e.renewInstructionBudget();
      e.callFunction("burn", [1_000_000]);
      calls++;
    }
  } catch (x) { err = String(x && x.message ? x.message : x); }
  check("renewed each call, 60 calls (~480M steps) all run", calls === 60, `survived ${calls}${err ? " — " + err : ""}`);
  try { e.dispose(); } catch {}
}

// A host may SIZE the budget. An embedder that runs the same script on more than
// one runtime needs their budgets to agree; this is the knob. 20 calls of ~8M
// steps is ~160M — well past the 50M default, well inside a 200M request.
{
  const e = burner();
  const accepted = e.setInstructionBudget(200_000_000);
  let calls = 0;
  let err = "";
  try { for (let i = 0; i < 20; i++) { e.callFunction("burn", [1_000_000]); calls++; } }
  catch (x) { err = String(x && x.message ? x.message : x); }
  check("setInstructionBudget is accepted on a fresh engine", accepted === true);
  check("a 200M budget runs 20 calls (~160M steps) that the default could not", calls === 20, `survived ${calls}${err ? " — " + err : ""}`);
  try { e.dispose(); } catch {}
}

// The fuse stays: a request beyond the ceiling is clamped, never unbounded, and
// a budget that has been spent cannot be re-sized after the fact.
{
  const e = burner();
  let calls = 0;
  try { for (let i = 0; i < 40; i++) { e.callFunction("burn", [1_000_000]); calls++; } } catch {}
  const late = e.setInstructionBudget(200_000_000);
  check("a spent budget cannot be re-sized", late === false && calls < 40, `late=${late} calls=${calls}`);
  try { e.dispose(); } catch {}
}
{
  const e = burner();
  const huge = e.setInstructionBudget(Number.MAX_VALUE);
  const nan = e.setInstructionBudget(NaN);
  check("an absurd request is clamped rather than refused (the engine keeps a budget)", huge === true && nan === true, `huge=${huge} nan=${nan}`);
  try { e.dispose(); } catch {}
}

// THE IMPORTANT ONE: renewal must not restore any OTHER ceiling. set_limits
// would have reset heap_limit and output_limit to unlimited, because setup
// applies those after it.
{
  const e = new Engine();
  const r = e.initScript(`
    let held = []
    function hoard(rounds) {
      let i = 0
      try {
        while (i < rounds) { held.push(new Uint8Array(8 * 1024 * 1024)); i = i + 1 }
        return "survived:" + held.length
      } catch (err) { return "stopped:" + held.length }
    }
  `);
  if (r && r.error) throw new Error("initScript: " + r.error);
  for (let i = 0; i < 20; i++) e.renewInstructionBudget();
  let verdict;
  try { verdict = "returned:" + e.callFunction("hoard", [64]); }
  catch (x) { verdict = "host-threw:" + String(x && x.message ? x.message : x); }
  check("the memory budget survives repeated renewal", /memory budget/i.test(verdict), verdict);
  try { e.dispose(); } catch {}
}

// A spent budget stays spent: renewal must not resurrect a torn-down engine.
{
  const e = burner();
  try { for (let i = 0; i < 40; i++) e.callFunction("burn", [1_000_000]); } catch {}
  let renewed = true;
  try { renewed = e.renewInstructionBudget(); } catch { renewed = false; }
  check("renewal refuses an engine that already spent a budget", renewed === false, String(renewed));
  try { e.dispose(); } catch {}
}

// The host chooses the size of the renewed budget. A call the default refuses
// — WarbleWire's decoder, a modem correlating a whole recording in one entry,
// needs more than 50M — goes through when the host grants more, and a smaller
// grant refuses sooner; the guest still cannot reach either.
{
  const e = burner();
  let refused = false;
  try { e.renewInstructionBudget(); e.callFunction("burn", [8_000_000]); } catch { refused = true; }
  check("the default budget refuses ~64M steps in one call", refused);
  try { e.dispose(); } catch {}
}
{
  const e = burner();
  let ok = false;
  try { e.renewInstructionBudget(200_000_000); ok = e.callFunction("burn", [8_000_000]) > 0; } catch {}
  check("a 200M grant admits the same call", ok);
  // And the grant is per re-entry: the next call, renewed at the default, is refused again.
  let refusedAgain = false;
  try { e.renewInstructionBudget(); e.callFunction("burn", [8_000_000]); } catch { refusedAgain = true; }
  check("the next renewal at the default refuses it again", refusedAgain);
  try { e.dispose(); } catch {}
}
{
  const e = burner();
  let refused = false;
  try { e.renewInstructionBudget(1_000); e.callFunction("burn", [100_000]); } catch { refused = true; }
  check("a 1,000-step grant refuses 100k iterations", refused);
  try { e.dispose(); } catch {}
}
{
  const e = burner();
  let ok = false;
  try { e.renewInstructionBudget(Number.NaN); ok = e.callFunction("burn", [1_000_000]) > 0; } catch {}
  check("a NaN grant means the default", ok);
  try { e.dispose(); } catch {}
}

// ── 2. The keyed fingerprint ───────────────────────────────────────────────
// Constructed against the unkeyed mixer by inverting it: two different values
// with byte-identical digests. Keyed, the host must see them differ.
const COLLISION_A = "[1, 1.5]";
const COLLISION_B = "[2, 8.51056402837524553247e-19]";

function digestOf(seedLo, seedHi, literal) {
  const e = new Engine();
  const r = e.initScript(`let state = ${literal}`);
  if (r && r.error) throw new Error("initScript: " + r.error);
  if (seedLo !== null) e.setFingerprintSeed(seedLo, seedHi);
  const idx = r.state ? r.state.index : (r["state"] && r["state"].index);
  const fp = e.getGlobalsFingerprint([idx])[0];
  e.dispose();
  return fp;
}

{
  // Unkeyed, the constructed pair collides. This is the defect, pinned so a
  // future change cannot quietly reintroduce it by dropping the key.
  const a0 = digestOf(null, null, COLLISION_A);
  const b0 = digestOf(null, null, COLLISION_B);
  check("unkeyed, the constructed pair still collides (the defect)", a0 === b0, `${a0} vs ${b0}`);

  // Keyed with the same key, they separate.
  const a1 = digestOf(0x9e3779b9, 0x7f4a7c15, COLLISION_A);
  const b1 = digestOf(0x9e3779b9, 0x7f4a7c15, COLLISION_B);
  check("keyed, the constructed collision separates", a1 !== b1, `${a1} vs ${b1}`);

  // The key changes the digest, so an attacker cannot precompute against it.
  const a2 = digestOf(0x12345678, 0x9abcdef0, COLLISION_A);
  check("a different key gives a different digest", a1 !== a2, `${a1} vs ${a2}`);

  // Same key, same value, same digest — the property the host relies on.
  const a3 = digestOf(0x9e3779b9, 0x7f4a7c15, COLLISION_A);
  check("same key and value still digest identically", a1 === a3, `${a1} vs ${a3}`);
}

console.log(`\n${pass} passed, ${fail} failed`);
if (fail > 0) process.exit(1);
