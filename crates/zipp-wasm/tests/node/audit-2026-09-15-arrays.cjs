// Regressions from the 15 September 2026 review, track E-arrays, pinned
// against the production artifact.
//
//   Z-06  A buffer the accelerator pinned (its raw region was handed to the
//         host) is never grown, resized, transferred or detached. `grow` —
//         which a plain ArrayBuffer does not even have — reallocated the
//         store under the host's region, and the host's next write landed in
//         freed linear memory and killed the instance. `transferToImmutable`
//         detached a pinned buffer.
//   R193  `concat`/`flat` over many references to one large array admit
//         their result against the heap ceiling before building it; they
//         trapped `unreachable` at the linked memory maximum and left the
//         Engine unusable.
//   Z-11  arrays past the dense cap (2^22 here) are filled, iterated and
//         sliced by their JS length; `join` keeps surrogate halves.
"use strict";
const { Engine } = require("./pkg/zipp_wasm.js");

let pass = 0, fail = 0;
function check(label, ok, detail) {
  if (ok) { pass++; console.log(`  ok   ${label}`); }
  else { fail++; console.log(`  FAIL ${label}${detail ? " — " + detail : ""}`); }
}
function same(label, got, want) {
  const g = JSON.stringify(got), w = JSON.stringify(want);
  check(label, g === w, `got ${g}, want ${w}`);
}
function thrown(fn) {
  try { fn(); return ""; } catch (x) { return String(x && x.message ? x.message : x); }
}
function close(e) { try { e.dispose(); } catch {} try { e.free(); } catch {} }

// ── Z-06: pinned buffers stay where the host was told they are ────────────
{
  const specs = [];
  const accel = Object.freeze({
    compile: () => 1,
    make: (id, spec) => { specs.push(spec); return 1; },
    state: () => {},
    run: () => 0,
    install: () => {},
  });
  const e = new Engine();
  e.setSyncHostCapabilities(["accel.make"]);
  e.setAccelBridge(accel);
  // This artifact is single-agent; SharedArrayBuffer is checked only where the
  // build provides it.
  e.initScript(`
    var shared = typeof SharedArrayBuffer === 'function';
    var ab = new ArrayBuffer(64, { maxByteLength: 1 << 20 });
    var buf = new Uint8Array(ab, 0, 64);
    var sab = shared ? new SharedArrayBuffer(64, { maxByteLength: 1 << 20 }) : null;
    var sbuf = shared ? new Uint8Array(sab, 0, 64) : null;
    function hasShared() { return shared; }
    function pin() { return String(accel.make('1', shared ? 'a=g:buf,b=g:sbuf' : 'a=g:buf')); }
    function attempt(what) {
      try {
        if (what === 'grow') ab.grow(1 << 20);
        else if (what === 'grow-call') SharedArrayBuffer.prototype.grow.call(ab, 1 << 20);
        else if (what === 'shared-grow') sab.grow(1 << 20);
        else ab[what]();
        return 'completed';
      } catch (x) { return x.name + ': ' + x.message; }
    }
    function state() { return [ab.byteLength, ab.detached, shared ? sab.byteLength : 64]; }
  `);
  const shared = e.callFunction("hasShared", []);
  same("the pin request reaches the adapter", e.callFunction("pin", []), "1");
  const first = specs[0];
  check("…with every region resolved", (shared ? /^a=r:\d+:64:1,b=r:\d+:64:1$/ : /^a=r:\d+:64:1$/).test(first), first);
  check("a plain ArrayBuffer has no grow", /^TypeError/.test(e.callFunction("attempt", ["grow"])), e.callFunction("attempt", ["grow"]));
  const attempts = ["resize", "transfer", "transferToFixedLength", "transferToImmutable"];
  if (shared) {
    check("SharedArrayBuffer's grow refuses a plain buffer", /^TypeError/.test(e.callFunction("attempt", ["grow-call"])), e.callFunction("attempt", ["grow-call"]));
    attempts.push("shared-grow");
  }
  for (const what of attempts) {
    const r = e.callFunction("attempt", [what]);
    check(`${what} meets the pin`, /^TypeError: .*pinned/.test(r), r);
  }
  same("no pinned buffer changed", e.callFunction("state", []), [64, false, 64]);
  e.callFunction("pin", []);
  same("re-resolving finds the same regions", specs[1], first);
  close(e);
}

// ── R193: concat/flat meet the heap ceiling, and the Engine survives ───────
for (const [label, body] of [
  ["concat", "Array.prototype.concat.apply([], b)"],
  ["flat", "b.flat()"],
]) {
  const e = new Engine();
  e.setInstructionBudget(500_000_000);
  e.initScript(`
    var a = new Array(1 << 22).fill(0); var b = [];
    function prep(n) { for (var i = 0; i < n; i++) b.push(a); return n; }
    function go() { return (${body}).length; }
  `);
  e.callFunction("prep", [16]);
  const r = thrown(() => e.callFunction("go", []));
  check(`${label} of 16 references stops at the heap ceiling`, /memory budget/.test(r), r);
  const after = thrown(() => e.evalInContext("1 + 1"));
  check(`…and the Engine reports a clean disposal, not a poisoned instance`, !/recursive use|unreachable/.test(after), after);
  close(e);
}

// ── Z-11: arrays past the dense cap, and join's surrogate halves ───────────
{
  const e = new Engine();
  e.setInstructionBudget(500_000_000);
  e.initScript(`
    var N = (1 << 22) + 1;
    function big() {
      var a = new Array(N).fill(1);
      var n = 0; for (var x of a) n += x;
      var s = new Array(N); s[N - 1] = 'z';
      var spread;
      try { spread = [...s].length; } catch (x) { spread = x.name; }
      return [a[0], a[N - 1], n, s.slice(-1)[0], s.at(-1), s.join('').length, spread];
    }
    function joinPair() { var t = 'hi \\u{1F600}!'; return t.split('').join('') === t; }
  `);
  // This profile materializes at most 2^22 elements from a length, so the
  // spread is refused outright rather than truncated to nothing.
  same("fill, for-of, slice and join see the whole length", e.callFunction("big", []), [1, 1, (1 << 22) + 1, "z", "z", 1, "RangeError"]);
  same("split('').join('') round-trips an astral character", e.callFunction("joinPair", []), true);
  close(e);
}

console.log(`\n${pass} passed, ${fail} failed`);
if (fail) process.exit(1);
