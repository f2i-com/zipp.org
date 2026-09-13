// Nested interpreter re-entry in the hardened wasm build: a builtin callback,
// a generator resumption, a Proxy trap, a sort comparator each run the guest
// inside a nested run loop, and the safe profile caps that nesting
// (MAX_RUN_LOOP_DEPTH in crates/zipp-vm/src/vm/mod.rs) so runaway nesting is
// a catchable error long before either stack overflows. Two halves: the
// shapes real programs use (a research script's generator inside `sum`
// inside a tensor's generator `__iter__`) must run comfortably deep, and
// hostile depth must come back as an ordinary error with the engine still
// usable, never as a trap.
//
// Measured with both caps lifted and a 16 MiB shadow stack (2026-09-13):
// every shape below ran 256 deep and failed between 384 and 512 on V8's
// machine stack; with `node --stack-size=8000` the shadow stack held past
// 1536. A cap of 32 stays an order of magnitude under the first limit.
const path = require("node:path");
const pkg = process.argv[2] || path.join(__dirname, "pkg", "zipp_wasm.js");
const { Engine } = require(pkg);

let pass = 0, fail = 0;
function ok(label, cond, extra = "") {
  if (cond) { pass++; console.log(`  ok   ${label}`); } else { fail++; console.log(`  FAIL ${label} ${extra}`); }
}
const hasPython = (() => { const e = new Engine(); try { e.initSource("x = 1\n", "python"); return true; } catch { return false; } finally { try { e.dispose(); } catch {} } })();

const WORKING_DEPTH = 20;
const HOSTILE_DEPTH = 200;
const shapes = {
  js_proxy_traps: ["javascript", (d) => `let t = {x: 1}; for (let i = 0; i < ${d}; i++) { const inner = t; t = new Proxy({}, { get: (o, k) => inner[k] }); } console.log(t.x);`, "1"],
  js_generators: ["javascript", (d) => `function* g(d) { if (d === 0) { yield 1; return; } for (const v of g(d - 1)) yield v; } console.log([...g(${d})][0]);`, "1"],
  js_sort_comparators: ["javascript", (d) => `function probe(d) { if (d === 0) return 0; return [2, 1].sort((a, b) => probe(d - 1) + a - b)[0]; } console.log(probe(${d}));`, "1"],
  js_replace_callbacks: ["javascript", (d) => `function probe(d) { if (d === 0) return "0"; return "a".replace(/a/, () => probe(d - 1)); } console.log(probe(${d}));`, "0"],
};
if (hasPython) {
  shapes.py_generator_in_sum = ["python", (d) => `def probe(d):\n    if d == 0:\n        return 1\n    return sum(probe(d - 1) for _ in range(1))\nprint(probe(${d}))\n`, "1"];
  shapes.py_iter_chain = ["python", (d) => `class T:\n    def __init__(s, d): s.d = d\n    def __iter__(s):\n        if s.d == 0:\n            yield 0\n        else:\n            for v in T(s.d - 1):\n                yield v + 1\nprint(list(T(${d}))[0])\n`, String(WORKING_DEPTH)];
}

function run(lang, source) {
  const e = new Engine();
  try { e.setInstructionBudget(2e9); } catch {}
  try {
    e.initSource(source, lang);
    const out = e.takeOutput().join("");
    e.dispose();
    return { ok: true, out };
  } catch (err) {
    return { ok: false, error: String(err && err.message !== undefined ? err.message : err), disposed: e.disposed };
  }
}
function healthy() {
  const r = run("javascript", "console.log(6 * 7);");
  return r.ok && r.out === "42";
}

for (const [name, [lang, make, expected]] of Object.entries(shapes)) {
  const shallow = run(lang, make(WORKING_DEPTH));
  ok(`${name}: ${WORKING_DEPTH} nested re-entries run`, shallow.ok && shallow.out.startsWith(expected), shallow.ok ? shallow.out : shallow.error);
  const deep = run(lang, make(HOSTILE_DEPTH));
  const message = deep.ok ? "" : deep.error;
  ok(`${name}: ${HOSTILE_DEPTH} is refused as a catchable error, not a trap`, !deep.ok && /call stack|RecursionError|recursion/i.test(message), deep.ok ? "ran" : message.slice(0, 80));
  ok(`${name}: the instance stays healthy afterwards`, healthy());
}
if (hasPython) {
  // Runtime paths that call back into Python from a loop of their own (a
  // `sorted` key, `list.count` with a user `__eq__`, a dataclass `__eq__`)
  // spend no nesting at all, so user recursion through them runs deep.
  const sortKey = run("python", "def probe(d):\n    if d == 0:\n        return 0\n    return sorted([1], key=lambda v: probe(d - 1) + v)[0]\nprint(probe(200))\n");
  ok("a sorted key function recursing 200 deep spends no re-entry budget", sortKey.ok && sortKey.out === "1", sortKey.ok ? sortKey.out : sortKey.error);
  const count = run("python", "class V:\n    def __init__(s, d): s.d = d\n    def __eq__(s, o):\n        return s.d == 0 or [V(s.d - 1)].count(V(s.d - 1)) == 1\nprint([V(200)].count(V(200)))\n");
  ok("list.count with a recursive __eq__ 200 deep runs on a plain loop", count.ok && count.out === "1", count.ok ? count.out : count.error);
  // The catchable form reaches the program: a `try` around the runaway sees a
  // RecursionError, and the engine keeps running after it.
  const r = run("python", "def probe(d):\n    return sum(probe(d - 1) for _ in range(1)) if d else 1\ntry:\n    probe(500)\nexcept RecursionError as e:\n    print('caught', type(e).__name__)\nprint(probe(4))\n");
  ok("python catches the depth error as RecursionError and continues", r.ok && r.out === "caught RecursionError1", r.ok ? r.out : r.error);
} else {
  console.log("  note python is not built into this package; the Python shapes were skipped");
}
console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
