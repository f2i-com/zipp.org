// The host-boundary contracts the 11 September 2026 audit found wanting,
// each pinned against the production artifact. The audit's own probe bundle
// (`run-wasm-audit.cjs`, 15 cases) ran red on v0.0.15 for 13 of them; every
// one of its cases is reproduced here, alongside the contract decisions the
// fixes made, so the artifact and not a Node model is what proves them.
//
//   ZIPP-01  the guest's "use strict" survives the preamble: undeclared
//            assignment is a ReferenceError, a strict function's `this` is
//            undefined, duplicate parameters are an early error; sloppy
//            guests, escaped/expression strings, comments, a BOM and a
//            hashbang behave as they would standalone.
//   ZIPP-02  draining the host.call queue is transactional: a request is
//            delivered exactly once, stays queued, or is rejected with its
//            callback settled — never lost with its callback pending.
//   ZIPP-04  fingerprints have a work budget shared across the batch: a
//            value the batched read refuses is unknown (NaN), not walked.
//   ZIPP-07  setFingerprintSeed before initScript is retained.
//   ZIPP-08  accel.make validates the whole spec (and the identifier) before
//            the adapter sees anything or a buffer is pinned.
//   ZIPP-10  setGlobalsBatch is strict about arity and duplicate indices,
//            and rejects before converting or writing anything.
//   ZIPP-12  evalInContext is a documented JSON projection, and a guest that
//            replaces JSON.stringify gets an error rather than undefined.
//   ZIPP-13  host.call ids are JavaScript Numbers end to end; completion,
//            duplicate completion and cancellation are explicit.
//   ZIPP-14  takeOutput is chronological across stdout and stderr; takeConsole
//            tags each line with its stream.
//   ZIPP-17  window.dispatchEvent delivers to the listeners registered on
//            window, and says so; it no longer returns true without doing
//            anything.
//   ZIPP-18  zippProfile reports provenance, parse/strictness policy, and the
//            host-boundary work limits.
//   ZIPP-24  guests are compiled under a stated grammar goal (the CommonJS-
//            shaped compatibility script) regardless of process state.
const { Engine, zippProfile } = require("./pkg/zipp_wasm.js");

let pass = 0, fail = 0;
function check(label, ok, detail) {
  if (ok) { pass++; console.log(`  ok   ${label}`); }
  else { fail++; console.log(`  FAIL ${label}${detail ? " — " + detail : ""}`); }
}
function same(label, got, want) {
  const g = JSON.stringify(got), w = JSON.stringify(want);
  check(label, g === w, `got ${g}, want ${w}`);
}
function close(e) { try { e.dispose(); } catch {} try { e.free(); } catch {} }
function init(source, setup) {
  const e = new Engine();
  if (setup) setup(e);
  const symbols = e.initScript(source);
  return { e, symbols };
}
function initError(source, setup) {
  const e = new Engine();
  try {
    if (setup) setup(e);
    e.initScript(source);
    return "";
  } catch (x) {
    return String(x && x.message ? x.message : x);
  } finally { close(e); }
}
function thrown(fn) {
  try { fn(); return ""; } catch (x) { return String(x && x.message ? x.message : x); }
}

// ── ZIPP-01: the directive prologue ────────────────────────────────────────
{
  check("strict: assignment to an undeclared name is a ReferenceError",
    /ReferenceError|not defined/i.test(initError('"use strict"; auditUndeclared = 1;')));
  check("strict: duplicate parameters are an early error",
    /SyntaxError|duplicate/i.test(initError('"use strict"; function auditDuplicate(a, a) { return a; }')));
  check("strict: `with` is an early error",
    /SyntaxError/i.test(initError('"use strict"; with (Math) {}')));
  check("sloppy: an undeclared assignment still creates the global",
    initError("auditUndeclared = 1;") === "");
  for (const [label, source, want] of [
    ["strict function this is undefined", '"use strict"; function auditThis(){ return this === undefined; }', true],
    ["sloppy function this is the global object", "function auditThis(){ return this === undefined; }", false],
    ["an escaped string is not a directive", String.raw`"use\x20strict"; function auditThis(){ return this === undefined; }`, false],
    ["a string in an expression is not a directive", '"use strict" + 1; function auditThis(){ return this === undefined; }', false],
    ["comments ahead of the prologue are skipped", '// note\n/* block */ "use strict"; function auditThis(){ return this === undefined; }', true],
    ["a BOM ahead of the prologue is whitespace", '\ufeff"use strict"; function auditThis(){ return this === undefined; }', true],
    ["a hashbang ahead of the prologue is a comment", '#!/usr/bin/env zipp\n"use strict"; function auditThis(){ return this === undefined; }', true],
    ["a second directive after use strict keeps it", '"use strict"; "another"; function auditThis(){ return this === undefined; }', true],
  ]) {
    const { e } = init(source);
    same(label, e.callFunction("auditThis", []), want);
    close(e);
  }
  // Positions are those of the concatenation, exactly as before, so the
  // preamble line count still corrects them.
  const { e } = init('"use strict"; var here = 1;');
  check("preambleLines is still the offset a host corrects by", e.preambleLines > 0);
  close(e);
  const { e: g } = init('"use strict"; var persistent = 5; function readIt(){ return persistent; }');
  same("strict guests keep persistent globals and preamble helpers", [g.callFunction("readIt", []), g.getEventListenerTypes()], [5, []]);
  close(g);
}

// ── ZIPP-02 / ZIPP-13: the asynchronous queue ──────────────────────────────
{
  // The audit's own probe: 300 requests sharing a 64 KiB argument.
  const { e } = init(`
    var auditChunk = 'x'.repeat(65536);
    for (var i = 0; i < 300; i++) host.call('probe', [auditChunk], function () {});
    function auditCounts() { return [__zHostQueue.length, __zHostPending]; }
  `);
  same("300 queued, 300 pending before the drain", e.callFunction("auditCounts", []), [300, 300]);
  const out = e.drainPendingHostCalls();
  check("the drain delivers a bounded prefix (here, everything)", Array.isArray(out) && out.length > 0 && out.length <= 300, String(out.length));
  same("what was delivered left the queue; nothing pending was lost", e.callFunction("auditCounts", []), [300 - out.length, 300]);
  check("delivered ids are unique", new Set(out.map((r) => r.id)).size === out.length);
  check("a delivered request carries its kind and arguments", out[0].kind === "probe" && out[0].args.length === 1 && out[0].args[0].length === 65536);
  close(e);
}
{
  // Past the per-drain byte allowance the rest STAYS QUEUED for the next
  // drain: 600 × 64 KiB is ~39 MB against a 32 MiB drain.
  const { e } = init(`
    var chunk = 'x'.repeat(65536);
    for (var i = 0; i < 600; i++) host.call('probe', [chunk], function () {});
    function auditCounts() { return [__zHostQueue.length, __zHostPending]; }
  `);
  const first = e.drainPendingHostCalls();
  const afterFirst = e.callFunction("auditCounts", []);
  check("an over-allowance queue drains a prefix and keeps the rest queued", first.length > 0 && first.length < 600 && afterFirst[0] === 600 - first.length && afterFirst[1] === 600, JSON.stringify({ first: first.length, afterFirst }));
  const second = e.drainPendingHostCalls();
  same("the next drain delivers the remainder, once", [first.length + second.length, e.callFunction("auditCounts", [])], [600, [0, 600]]);
  check("no id was delivered twice", new Set([...first, ...second].map((r) => r.id)).size === 600);
  close(e);
}
{
  // A single request that can never cross: rejected with explicit
  // settlement, and the requests around it are still delivered.
  const { e } = init(`
    var settled = [];
    var big = [];
    for (var i = 0; i < 40; i++) big.push('y'.repeat(1048576)); // 40 MiB: more than a whole drain may carry
    host.call('first', ['a'], function (r) { settled.push(['first', String(r)]); });
    // Bypass the guest-side size check the way a tampering guest could:
    var id = ++__zHostId;
    __zHostCbs[id] = function (r) { settled.push(['big', r instanceof RangeError, String(r)]); throw new Error('callback exploded'); };
    __zHostPending++;
    __zHostQueue.push({ id: id, kind: 'big', args: big });
    host.call('last', ['z'], function (r) { settled.push(['last', String(r)]); });
    function auditCounts() { return [__zHostQueue.length, __zHostPending]; }
    function auditSettled() { return settled; }
  `);
  same("three queued, three pending", e.callFunction("auditCounts", []), [3, 3]);
  const out = e.drainPendingHostCalls();
  same("the deliverable requests are delivered in order", out.map((r) => r.kind), ["first", "last"]);
  same("the oversized request is gone and its callback is no longer pending", e.callFunction("auditCounts", []), [0, 2]);
  const settled = e.callFunction("auditSettled", []);
  check("the rejected request's callback was settled with a RangeError", settled.length === 1 && settled[0][0] === "big" && settled[0][1] === true && /transport limit/.test(settled[0][2]), JSON.stringify(settled));
  const output = e.takeOutput();
  check("a throwing rejection callback is reported to the console, not to the drain", output.some((l) => /callback exploded/.test(l)), JSON.stringify(output));
  same("the delivered requests still complete normally", [e.resolveHostCallback(out[0].id, "ok1"), e.resolveHostCallback(out[1].id, "ok2"), e.callFunction("auditSettled", []).slice(1)], [true, true, [["first", "ok1"], ["last", "ok2"]]]);
  close(e);
}
{
  // The guest-side per-request ceiling refuses before anything registers.
  const { e } = init(`
    var s = 'q'.repeat(1048576);
    var err = '';
    try { host.call('huge', [s, s, s, s, s], function () {}); } catch (x) { err = String(x); }
    function auditState() { return [err, __zHostQueue.length, __zHostPending]; }
  `);
  const [err, queued, pending] = e.callFunction("auditState", []);
  check("host.call refuses a request over the code-unit ceiling synchronously and registers nothing", /RangeError.*transport limit/.test(err) && queued === 0 && pending === 0, JSON.stringify([err, queued, pending]));
  close(e);
}
{
  // Ids are Numbers end to end (the audit's rollover probe), completions are
  // explicit, and cancellation releases without invoking.
  const { e, symbols } = init(`
    __zHostId = 4294967295;
    var completed = [];
    host.call('probe', [], function (r) { completed.push(r); });
    host.call('probe', [], function (r) { completed.push(r); });
    host.call('probe', [], function (r) { completed.push(r); });
    function auditCompleted() { return completed; }
    function auditPending() { return __zHostPending; }
  `);
  const requests = e.drainPendingHostCalls();
  same("ids past 2^32 round-trip", requests.map((r) => r.id), [4294967296, 4294967297, 4294967298]);
  same("a completion reports that a callback ran", e.resolveHostCallback(4294967296, "ok"), true);
  same("a duplicate completion is a no-op that says so", e.resolveHostCallback(4294967296, "again"), false);
  same("an unknown id is a no-op that says so", e.resolveHostCallback(77, "nobody"), false);
  same("cancellation releases the callback without running it", [e.cancelHostCallback(4294967297), e.callFunction("auditPending", [])], [true, 1]);
  same("a completion after cancellation runs nothing", [e.resolveHostCallback(4294967297, "late"), e.callFunction("auditCompleted", [])], [false, ["ok"]]);
  same("cancelling twice is a no-op that says so", e.cancelHostCallback(4294967297), false);
  for (const bad of [-1, 1.5, NaN, Infinity, 2 ** 53]) {
    check(`an invalid id (${bad}) is a TypeError, not a truncation`, /TypeError/.test(thrown(() => e.resolveHostCallback(bad, "x"))));
  }
  same("id 0 was never issued: a no-op that says so", e.resolveHostCallback(0, "x"), false);
  same("the last request still completes", [e.resolveHostCallback(4294967298, "fin"), e.callFunction("auditPending", [])], [true, 0]);
  close(e);
}

// ── ZIPP-04: the fingerprint budget ────────────────────────────────────────
{
  const { e, symbols } = init("var auditSparse = new Array(2000001);");
  const indices = [symbols.auditSparse.index];
  check("a 2,000,001-hole array is refused by the batched read", thrown(() => e.getGlobalsBatch(indices)) !== "");
  check("…and is unknown to the fingerprint rather than walked", Number.isNaN(e.getGlobalsFingerprint(indices)[0]));
  close(e);
}
{
  const { e, symbols } = init("var auditChunk = 'x'.repeat(65536); var auditStrings = []; for (var i = 0; i < 300; i++) auditStrings.push(auditChunk);");
  const indices = [symbols.auditStrings.index];
  check("300 shared 64 KiB strings are refused by the batched read", thrown(() => e.getGlobalsBatch(indices)) !== "");
  check("…and are unknown to the fingerprint", Number.isNaN(e.getGlobalsFingerprint(indices)[0]));
  close(e);
}
{
  // The batch shares one budget, duplicates included: many copies of a
  // value that fits alone stop fitting together, and unknown never means
  // equal.
  const { e, symbols } = init("var a = []; for (var i = 0; i < 1000000; i++) a.push(i); var b = { x: 1 };");
  const one = e.getGlobalsFingerprint([symbols.a.index]);
  check("a million-element array digests alone", Number.isFinite(one[0]));
  const three = e.getGlobalsFingerprint([symbols.a.index, symbols.a.index, symbols.a.index, symbols.b.index]);
  check("a second copy in the same batch does not fit the shared budget", Number.isFinite(three[0]) && three[0] === one[0] && Number.isNaN(three[1]) && Number.isNaN(three[2]), JSON.stringify(three));
  check("a refused slot is not walked: a small later slot still fits what remains", Number.isFinite(three[3]), JSON.stringify(three));
  close(e);
}

// ── ZIPP-07: a seed set before initScript ──────────────────────────────────
{
  const lo = 0x12345678, hi = 0x87654321;
  const { e, symbols } = init("var auditState = { x: 1, y: 2 };", (x) => x.setFingerprintSeed(lo, hi));
  const indices = [symbols.auditState.index];
  const before = e.getGlobalsFingerprint(indices);
  e.setFingerprintSeed(lo, hi);
  const after = e.getGlobalsFingerprint(indices);
  same("a pre-init seed is applied: re-setting the same seed changes nothing", before, after);
  const { e: unkeyed, symbols: s2 } = init("var auditState = { x: 1, y: 2 };");
  check("…and it was really applied: the unkeyed digest differs", unkeyed.getGlobalsFingerprint([s2.auditState.index])[0] !== before[0]);
  close(unkeyed);
  e.setFingerprintSeed(1, 2);
  check("changing the seed changes the digest (host caches must be dropped)", e.getGlobalsFingerprint(indices)[0] !== before[0]);
  close(e);
}

// ── ZIPP-10: strict batch arity ────────────────────────────────────────────
{
  const { e, symbols } = init("var auditA = 1; var auditB = 2;");
  const indices = [symbols.auditA.index, symbols.auditB.index];
  check("a short values array is a TypeError", /TypeError.*same length/.test(thrown(() => e.setGlobalsBatch(indices, [99]))));
  same("…and nothing was written", e.getGlobalsBatch(indices), [1, 2]);
  check("a long values array is a TypeError", /TypeError.*same length/.test(thrown(() => e.setGlobalsBatch([symbols.auditA.index], [99, 100]))));
  same("…and nothing was written", e.getGlobalByIndex(symbols.auditA.index), 1);
  check("a duplicate index is a TypeError", /TypeError.*more than once/.test(thrown(() => e.setGlobalsBatch([symbols.auditA.index, symbols.auditA.index], [5, 6]))));
  same("…and nothing was written", e.getGlobalByIndex(symbols.auditA.index), 1);
  let converted = 0;
  const spy = { get v() { converted++; return 7; } };
  check("arity is checked before any value is converted", /TypeError/.test(thrown(() => e.setGlobalsBatch(indices, [spy]))) && converted === 0, String(converted));
  e.setGlobalsBatch(indices, [10, 20]);
  same("an exact batch writes", e.getGlobalsBatch(indices), [10, 20]);
  e.setGlobalsBatch(indices, [30, ,]);
  same("a hole is an explicit undefined", e.getGlobalsBatch(indices), [30, null]);
  e.setGlobalsBatch([], []);
  same("an empty batch is fine", e.getGlobalsBatch(indices), [30, null]);
  close(e);
}

// ── ZIPP-12: evalInContext is a JSON projection ────────────────────────────
{
  const { e } = init("var cyc = {}; cyc.self = cyc; var obj = { a: 1, get g() { return 'getter'; }, toJSON: function () { return 'json'; } };");
  same("undefined projects to undefined", e.evalInContext("undefined"), undefined);
  same("a function projects to undefined", e.evalInContext("(function () {})"), undefined);
  same("NaN and the infinities project to null", e.evalInContext("[NaN, Infinity, -Infinity]"), [null, null, null]);
  same("-0 projects to 0", e.evalInContext("-0"), 0);
  same("toJSON runs", e.evalInContext("obj"), "json");
  same("getters are evaluated and own data crosses", e.evalInContext("({ a: 1, get g() { return 2; } })"), { a: 1, g: 2 });
  check("a BigInt is the guest's own TypeError", /TypeError/.test(thrown(() => e.evalInContext("10n"))));
  check("a cycle is the guest's own TypeError", /TypeError/.test(thrown(() => e.evalInContext("cyc"))));
  e.evalInContext("JSON.stringify = function () { return 'not json at all'; }");
  check("a replaced JSON.stringify is reported, not silently undefined", /SyntaxError.*not valid JSON/.test(thrown(() => e.evalInContext("1"))));
  close(e);
}

// ── ZIPP-14: chronological console output ──────────────────────────────────
{
  const { e } = init('console.log("first"); console.error("second"); console.log("third"); Promise.resolve().then(function () { console.warn("fourth"); console.info("fifth"); }); function later() { console.log("sixth"); console.error("seventh"); throw new Error("x"); }');
  same("takeOutput keeps the order across streams, microtasks included", e.takeOutput(), ["first", "second", "third", "fourth", "fifth"]);
  same("draining empties it", e.takeOutput(), []);
  thrown(() => e.callFunction("later", []));
  same("takeConsole tags each line with its stream, in order, through a throw", e.takeConsole(), [{ stream: "stdout", text: "sixth" }, { stream: "stderr", text: "seventh" }]);
  close(e);
}

// ── ZIPP-08: accelerator spec validation ───────────────────────────────────
{
  const calls = [];
  const accel = Object.freeze({
    compile: (params, body) => { calls.push(["compile", params, body]); return 1; },
    make: (id, spec) => { calls.push(["make", id, spec]); return 2; },
    state: (ptr, len, kind) => { calls.push(["state", ptr, len, kind]); },
    run: (id, h) => { calls.push(["run", id, h]); return 0; },
    install: (slot, id) => { calls.push(["install", slot, id]); },
  });
  const { e } = init(`
    var buf = new Int32Array(4); var other = new Float64Array(2);
    function attempt(id, spec) { try { return String(accel.make(id, spec)); } catch (x) { return 'threw:' + x; } }
    function transferable(name) { try { (name === 'buf' ? buf : other).buffer.transfer(); return 'moved'; } catch (x) { return String(x); } }
  `, (x) => { x.setSyncHostCapabilities(["accel.make", "accel.install", "accel.run"]); x.setAccelBridge(accel); });
  for (const [label, id, spec] of [
    ["an r: region", "1", "a=g:buf,b=r:1:2:3"],
    ["a duplicate name", "1", "a=g:buf,a=g:other"],
    ["a later invalid entry", "1", "a=g:buf,b=nope"],
    ["a non-typed-array global after a valid one", "1", "a=g:buf,b=g:attempt"],
    ["a fractional id operand", "1", "a=g:buf,b=a:1.5"],
    ["a fractional function identifier", "1.5", "a=g:buf"],
    ["a NaN function identifier", "NaN", "a=g:buf"],
    ["a negative function identifier", "-1", "a=g:buf"],
  ]) {
    const r = e.callFunction("attempt", [id, spec]);
    check(`accel.make with ${label} is refused before the adapter runs`, /^threw:/.test(r) && calls.length === 0, `${r} calls=${JSON.stringify(calls)}`);
    check(`…and pinned nothing (${label})`, e.callFunction("transferable", ["buf"]) === "moved" || /detached/.test(e.callFunction("transferable", ["buf"])), e.callFunction("transferable", ["buf"]));
  }
  // A fresh buffer, since the transfer checks above detached the old one.
  e.evalInContext("(buf = new Int32Array(4), other = new Float64Array(2), 0)");
  const ok = e.callFunction("attempt", ["7", "a=g:buf,f=c:attempt,k=a:3,x=n:2.5,tr=t,b=g:other"]);
  check("a valid spec reaches the adapter with every g: entry resolved", ok === "2" && calls.length === 1 && calls[0][0] === "make" && calls[0][1] === 7 && /^a=r:\d+:4:5,f=c:attempt,k=a:3,x=n:2\.5,tr=t,b=r:\d+:2:8$/.test(calls[0][2]), JSON.stringify([ok, calls]));
  check("…and pins both buffers", /pinned/.test(e.callFunction("transferable", ["buf"])) && /pinned/.test(e.callFunction("transferable", ["other"])));
  close(e);
}

// ── ZIPP-17: window.dispatchEvent ──────────────────────────────────────────
{
  const { e } = init(`
    var seen = [];
    window.addEventListener('ping', function (ev) { seen.push('a:' + ev.detail); });
    window.addEventListener('ping', function (ev) { seen.push('b:' + ev.detail); });
    function fire(type, detail) { return window.dispatchEvent({ type: type, detail: detail }); }
    function bad() { try { window.dispatchEvent('ping'); return 'accepted'; } catch (x) { return String(x); } }
    function auditSeen() { return seen; }
  `);
  same("dispatchEvent delivers to the listeners registered on window", [e.callFunction("fire", ["ping", 1]), e.callFunction("auditSeen", [])], [true, ["a:1", "b:1"]]);
  same("an unlistened type dispatches to nobody and still reports uncancelled", [e.callFunction("fire", ["other", 2]), e.callFunction("auditSeen", [])], [true, ["a:1", "b:1"]]);
  check("a non-event argument is a TypeError", /TypeError/.test(e.callFunction("bad", [])));
  close(e);
}

// ── ZIPP-18 / ZIPP-24: provenance and policy in the profile ────────────────
{
  const profile = JSON.parse(zippProfile());
  same("profileVersion 2", profile.profileVersion, 2);
  check("source.sha is a hex revision or null", profile.source.sha === null || /^[0-9a-f]{7,64}$/.test(profile.source.sha), String(profile.source.sha));
  same("the grammar goal and strictness policy are stated", [profile.semantics.parseGoal, profile.semantics.topLevelReturn, profile.semantics.guestStrictMode], ["script-compat", true, "directive-prologue"]);
  same("the boundary contracts are stated", [profile.semantics.stringTransport, profile.semantics.batchWriteArity, profile.semantics.hostCallIdBits, profile.semantics.consoleOutput, profile.semantics.hostCallDrain], ["unicode-scalar", "strict", 53, "chronological", "transactional"]);
  for (const key of ["hostValueNodes", "hostValueStringBytes", "fingerprintNodes", "fingerprintStringBytes", "hostCallQueue", "hostCallPending", "hostCallRequestUnits", "hostCallDrainRequests", "hostCallDrainStringBytes", "accelSpecBytes", "accelSpecEntries", "accelSpecNameBytes"]) {
    check(`limits.${key} is a positive integer`, Number.isSafeInteger(profile.limits[key]) && profile.limits[key] > 0, String(profile.limits[key]));
  }
  const { e } = init("var top = 1; return top;");
  same("guests are compiled under the compatibility grammar: top-level return is legal", e.evalInContext("top"), 1);
  close(e);
}

console.log(`\n${pass} passed, ${fail} failed`);
if (fail > 0) process.exit(1);
