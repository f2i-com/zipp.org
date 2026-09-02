// The output budget has to bound LINES as well as bytes.
//
// A buffered line is a String in a Vec that later becomes one node of the array
// takeOutput() returns, so it costs something even when it says nothing. While
// an empty line was charged one byte, an 8 MiB budget admitted 8.4 million of
// them: past the host's node cap, past what the instance could hold, and past
// any guard that would have said so. `while (true) console.log("")` did not
// raise an error — it trapped the WebAssembly instance on `unreachable`, which
// a host cannot catch and cannot distinguish from a bug in the engine.
//
// Lives in the Node suite because it is a property of the shipped artifact's
// configured limits, not of the VM crate in isolation.
const { Engine } = require("./pkg/zipp_wasm.js");

let pass = 0, fail = 0;
function check(label, ok, detail) {
  if (ok) { pass++; console.log(`  ok   ${label}`); }
  else { fail++; console.log(`  FAIL ${label}${detail ? " — " + detail : ""}`); }
}

const LIMIT = 8 * 1024 * 1024;
const OVERHEAD = 8;
const MAX_LINES = LIMIT / OVERHEAD;   // 1,048,576

function spew(width, n) {
  const e = new Engine();
  const r = e.initScript(`
    let W = ""
    function mkw(w) { let i=0; W=""; while(i<w){ W=W+"x"; i=i+1 } return W.length }
    function spew(n) { let i=0; while(i<n){ console.log(W); i=i+1 } return i }
  `);
  if (r && r.error) throw new Error("initScript: " + r.error);
  e.renewInstructionBudget();
  e.callFunction("mkw", [width]);
  let verdict, lines = -1;
  try {
    e.renewInstructionBudget();
    e.callFunction("spew", [n]);
    verdict = "wrote";
  } catch (err) {
    const m = String(err && err.message ? err.message : err);
    verdict = /output budget/.test(m) ? "budget"
            : /unreachable|RuntimeError|memory access/i.test(m) ? "TRAP"
            : "other:" + m.slice(0, 60);
  }
  if (verdict === "wrote") {
    try { e.renewInstructionBudget(); lines = e.takeOutput().length; }
    catch (err) { verdict = "unretrievable:" + String(err && err.message ? err.message : err).slice(0, 50); }
  }
  try { e.dispose(); } catch { /* a spent guard disposes the engine */ }
  return { verdict, lines };
}

// 1. THE ONE THAT MATTERS. A flood of empty lines must be refused, not trap.
{
  const r = spew(0, MAX_LINES * 4);
  check("4x the line budget of empty lines is refused by the budget",
    r.verdict === "budget", JSON.stringify(r));
  check("and it is NOT a WebAssembly trap", r.verdict !== "TRAP", JSON.stringify(r));
}

// 2. The boundary is where the arithmetic says, and everything under it comes back.
{
  const under = spew(0, MAX_LINES);
  check(`${MAX_LINES} empty lines are allowed`, under.verdict === "wrote", JSON.stringify(under));
  check("and takeOutput returns every one of them", under.lines === MAX_LINES,
    `got ${under.lines}`);
  const over = spew(0, MAX_LINES + 1000);
  check("one thousand past it is refused", over.verdict === "budget", JSON.stringify(over));
}

// 3. Real text still gets the byte budget it always had.
{
  const wide = spew(100, Math.floor(LIMIT / (100 + OVERHEAD) * 0.9));
  check("6.7 MB of real text still prints", wide.verdict === "wrote", JSON.stringify(wide));
  check("and is retrievable in one takeOutput", wide.lines > 60000, `got ${wide.lines}`);
}

// 4. The guard is sticky: a script cannot catch it and carry on.
{
  const e = new Engine();
  const r = e.initScript(`
    function flood(n) {
      let i = 0
      try { while (i < n) { console.log(""); i = i + 1 } return "survived" }
      catch (err) { return "caught:" + err }
    }
  `);
  if (r && r.error) throw new Error(r.error);
  let out;
  try { e.renewInstructionBudget(); out = "returned:" + e.callFunction("flood", [MAX_LINES * 4]); }
  catch (err) { out = "host-threw:" + String(err && err.message ? err.message : err); }
  check("the script cannot catch the output guard", out.startsWith("host-threw:"), out.slice(0, 90));
  try { e.dispose(); } catch { /* torn down */ }
}

console.log(`\n${pass} passed, ${fail} failed`);
if (fail > 0) process.exit(1);
