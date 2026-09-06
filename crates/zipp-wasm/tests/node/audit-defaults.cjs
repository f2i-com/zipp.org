// The host-boundary defaults the 6 September 2026 audit found wanting, each
// pinned against the production artifact:
//
//   Z02  accel.make forwards only the public binding grammar; a guest cannot
//        hand the adapter an `r:` region of its own, and every entry is
//        validated before the adapter is invoked.
//   Z04  the artifact reports its own limits and semantics (zippProfile).
//   Z06  setInstructionBudget before initScript governs top-level execution,
//        with defined handling of NaN, negatives and fractions.
//   Z07  the monotonic clock calls performance.now with its receiver, so a
//        receiver-strict host is used rather than silently fallen back from.
//   Z08  event names that collide with Object.prototype are ordinary names.
//   Z09  a host.call whose argument conversion throws registers nothing.
//
// The clock stub must be in place before the module is instantiated: the
// engine reads the clock at start.
const realPerformance = globalThis.performance;
const clock = { calls: 0, illegal: 0 };
globalThis.performance = {
  now() {
    if (this !== globalThis.performance) { clock.illegal++; throw new TypeError("Illegal invocation"); }
    clock.calls++;
    return realPerformance.now();
  },
};

const { Engine, zippProfile } = require("./pkg/zipp_wasm.js");

let pass = 0, fail = 0;
function check(label, ok, detail) {
  if (ok) { pass++; console.log(`  ok   ${label}`); }
  else { fail++; console.log(`  FAIL ${label}${detail ? " — " + detail : ""}`); }
}
function init(source, setup) {
  const e = new Engine();
  if (setup) setup(e);
  const r = e.initScript(source);
  if (r && r.error) throw new Error("initScript: " + r.error);
  return e;
}
function initError(source, setup) {
  const e = new Engine();
  if (setup) setup(e);
  try {
    const r = e.initScript(source);
    if (r && r.error) return String(r.error);
    return "";
  } catch (x) {
    return String(x && x.message ? x.message : x);
  }
}

// ── Z07: the clock reaches performance.now with its receiver ───────────────
{
  const before = clock.calls;
  const e = init("var t0 = performance.now(); var t1 = performance.now(); var monotonic = t1 >= t0 && typeof t1 === 'number';");
  check("performance.now is invoked with the Performance object as receiver", clock.calls > before && clock.illegal === 0, `calls ${clock.calls - before}, illegal ${clock.illegal}`);
  check("the guest's performance.now reads the host clock", e.evalInContext("monotonic") === true);
  e.dispose();
}

// ── Z06: a budget chosen before initialization governs the top level ───────
const TOP_LEVEL_WORK = "function burn(n){let i=0;let s=0;while(i<n){s=s+i;i=i+1}return s} var total = burn(10000000);";
{
  // ~80M steps of top-level work: past the 50M default, inside a 200M request.
  const byDefault = initError(TOP_LEVEL_WORK);
  check("the default allowance stops ~80M steps of top-level work", byDefault !== "", "ran to completion");
  const widened = initError(TOP_LEVEL_WORK, (e) => check.accepted = e.setInstructionBudget(200_000_000));
  check("setInstructionBudget before initScript is accepted", check.accepted === true);
  check("a larger allowance set before initScript applies to the top level", widened === "", widened);
  const narrowed = initError("var x = 1; for (var i = 0; i < 1000; i++) x = x + i;", (e) => e.setInstructionBudget(20));
  check("a smaller allowance set before initScript applies to the top level", narrowed !== "", "ran to completion");
}
{
  // Defined handling of the odd values: NaN and Infinity mean the default,
  // negatives and zero clamp to a single step, fractions truncate.
  check("NaN selects the default allowance", initError("var x = 1;", (e) => e.setInstructionBudget(NaN)) === "");
  check("Infinity selects the default allowance", initError("var x = 1;", (e) => e.setInstructionBudget(Infinity)) === "");
  check("a negative allowance is one step", initError("var x = 0; for (var i = 0; i < 100; i++) x++;", (e) => e.setInstructionBudget(-5)) !== "");
  check("a fractional allowance truncates", initError("var x = 0; for (var i = 0; i < 100; i++) x++;", (e) => e.setInstructionBudget(1.9)) !== "");
}
{
  // After initialization the knob still resizes the running budget, and a
  // renewal restores the size the host chose.
  const e = init("function burn(n){let i=0;let s=0;while(i<n){s=s+i;i=i+1}return s}");
  check("setInstructionBudget after initScript is accepted", e.setInstructionBudget(200_000_000) === true);
  let calls = 0;
  try { for (let i = 0; i < 20; i++) { e.callFunction("burn", [1_000_000]); calls++; } } catch {}
  check("20 calls (~160M steps) run inside a 200M allowance", calls === 20, `survived ${calls}`);
  check("renewal restores the chosen size", e.renewInstructionBudget() === true);
  calls = 0;
  try { for (let i = 0; i < 20; i++) { e.callFunction("burn", [1_000_000]); calls++; } } catch {}
  check("and the renewed budget is the chosen 200M, not the 50M default", calls === 20, `survived ${calls}`);
  e.dispose();
  check("a disposed engine refuses a budget", e.setInstructionBudget(1000) === false);
}

// ── Z08: reserved-looking event names are ordinary names ───────────────────
{
  const e = init(`
    var seen = [];
    window.addEventListener("constructor", function (ev) { seen.push("constructor:" + ev.key); });
    window.addEventListener("toString", function (ev) { seen.push("toString"); });
    window.addEventListener("__proto__", function (ev) { seen.push("__proto__"); });
    window.addEventListener("hasOwnProperty", function () { seen.push("hasOwnProperty"); });
    window.addEventListener("click", "not a function");
    window.removeEventListener("never-registered", function () {});
    window.removeEventListener("valueOf", function () {});
    function selfRemoving(ev) { window.removeEventListener("tick", selfRemoving); seen.push("self"); }
    window.addEventListener("tick", selfRemoving);
    window.addEventListener("tick", function () { seen.push("after-self"); });
  `);
  check("dispatch to 'constructor' reaches its listener", e.dispatchEvent("constructor", { type: "constructor", key: "k" }) === 1);
  check("dispatch to 'toString' reaches its listener", e.dispatchEvent("toString", { type: "toString" }) === 1);
  check("dispatch to '__proto__' reaches its listener", e.dispatchEvent("__proto__", { type: "__proto__" }) === 1);
  check("dispatch to an unknown reserved-looking name is harmless", e.dispatchEvent("valueOf", { type: "valueOf" }) === 0);
  check("dispatch to 'click' with only a non-function listener is harmless", e.dispatchEvent("click", { type: "click" }) === 0);
  check("a listener removing itself does not skip the one after it", e.dispatchEvent("tick", { type: "tick" }) === 2);
  check("and is gone next time", e.dispatchEvent("tick", { type: "tick" }) === 1);
  check("listeners fired in order", JSON.stringify(e.evalInContext("seen")) === JSON.stringify(["constructor:k", "toString", "__proto__", "self", "after-self", "after-self"]), JSON.stringify(e.evalInContext("seen")));
  const types = e.getEventListenerTypes();
  check("listener types are the registered names, no inherited keys", Array.isArray(types) && types.includes("constructor") && types.includes("tick") && !types.includes("valueOf"), JSON.stringify(types));
  e.dispose();
}

// ── Z09: a request that fails to normalize registers nothing ───────────────
{
  const e = init(`
    var got = [];
    var threw = "";
    try {
      host.call("kind", [{ toString: function () { throw new Error("boom"); } }], function (r) { got.push("never:" + r); });
    } catch (x) { threw = x.message; }
    host.call("ok", ["a", 1], function (r) { got.push("ok:" + r); });
  `);
  check("the throwing conversion surfaces to the guest", e.evalInContext("threw") === "boom");
  const drained = e.drainPendingHostCalls();
  check("only the whole request was queued", Array.isArray(drained) && drained.length === 1 && drained[0].kind === "ok" && JSON.stringify(drained[0].args) === '["a","1"]', JSON.stringify(drained));
  const okId = drained[0].id;
  check("exactly one callback waits, for the request that was queued", e.evalInContext("__zHostPending") === 1);
  e.resolveHostCallback(okId - 1, "late");
  e.resolveHostCallback(okId + 1, "late");
  check("a completion for an id that never registered delivers nothing", JSON.stringify(e.evalInContext("got")) === "[]" && e.evalInContext("__zHostPending") === 1);
  e.resolveHostCallback(okId, "reply");
  check("the whole request's callback still completes", JSON.stringify(e.evalInContext("got")) === '["ok:reply"]' && e.evalInContext("__zHostPending") === 0);
  e.resolveHostCallback(okId, "again");
  check("a completion is delivered once", JSON.stringify(e.evalInContext("got")) === '["ok:reply"]');
  e.dispose();
}
{
  // The queue is bounded: a guest that never lets the host drain cannot grow it without limit.
  const err = initError("for (var i = 0; i < 5000; i++) host.call('k', [i]);");
  check("more than 4096 queued requests between drains is a RangeError", /RangeError|too many requests/.test(err), err);
}

// ── Z02: accel.make forwards only the public grammar ───────────────────────
{
  const seen = [];
  const adapter = {
    compile() { return 0; },
    make(id, spec) { seen.push(spec); return 1; },
    state() {},
    run() { return 0; },
    install() {},
  };
  const source = `
    var buf = new Float64Array(4);
    var ints = new Int32Array(2);
    function cb(x) { return x; }
    function attempt(spec) {
      try { accel.make(0, spec); return "ok"; }
      catch (x) { return String(x && x.message ? x.message : x); }
    }
    var results = {
      region: attempt("a=r:1:2:3"),
      regionAmongOthers: attempt("x=g:buf,y=r:0:1:8"),
      bad: attempt("x=z:1"),
      noName: attempt("=g:buf"),
      badName: attempt("1x=g:buf"),
      dup: attempt("x=g:buf,x=g:ints"),
      badId: attempt("f=a:x"),
      badNumber: attempt("n=n:abc"),
      badTable: attempt("t=t:1"),
      notTyped: attempt("x=g:cb"),
      good: attempt("x=g:buf,y=g:ints,f=a:0,c=c:cb,k=n:1.5,tab=t"),
      empty: attempt(""),
    };
  `;
  const e = init(source, (engine) => {
    engine.setAccelBridge(adapter);
    engine.setSyncHostCapabilities(["accel.compile", "accel.make", "accel.state", "accel.run", "accel.install"]);
  });
  const results = e.evalInContext("results");
  const refused = (key) => typeof results[key] === "string" && results[key] !== "ok" && /accel\.make/.test(results[key]);
  check("a guest-written r: region is refused", refused("region"), results.region);
  check("refused even beside a legitimate g: entry", refused("regionAmongOthers"), results.regionAmongOthers);
  check("an unknown tag is refused", refused("bad"), results.bad);
  check("an empty name is refused", refused("noName"), results.noName);
  check("a non-identifier name is refused", refused("badName"), results.badName);
  check("a name bound twice is refused", refused("dup"), results.dup);
  check("a non-integer a: id is refused", refused("badId"), results.badId);
  check("a non-numeric n: operand is refused", refused("badNumber"), results.badNumber);
  check("t with an operand is refused", refused("badTable"), results.badTable);
  check("g: of a non-typed-array global is an error the guest sees", results.notTyped !== "ok" && /not a typed array/.test(results.notTyped), results.notTyped);
  check("the public grammar goes through", results.good === "ok" && results.empty === "ok", `${results.good} / ${results.empty}`);
  const forwarded = seen.filter((s) => s !== "");
  check("the adapter saw only the resolved public entries", forwarded.length === 1 && /^x=r:\d+:4:8,y=r:\d+:2:5,f=a:0,c=c:cb,k=n:1\.5,tab=t$/.test(forwarded[0]), JSON.stringify(seen));
  check("no refused spec reached the adapter", seen.every((s) => !/r:1:2:3|r:0:1:8|z:1|1x=|x=g:buf,x=|a:x|n:abc|t:1/.test(s)), JSON.stringify(seen));
  e.dispose();
}

// ── Z04: the artifact describes itself ─────────────────────────────────────
{
  let profile = null;
  try { profile = JSON.parse(zippProfile()); } catch (x) { check("zippProfile is JSON", false, String(x)); }
  if (profile) {
    check("zippProfile names the engine and version", profile.engine === "zipp-wasm" && /^\d+\.\d+\.\d+$/.test(profile.version), JSON.stringify(profile.version));
    check("zippProfile lists the isolated features", Array.isArray(profile.features) && profile.features.includes("safe-sandbox"));
    check("zippProfile states strict call order", profile.semantics && profile.semantics.callOrder === "strict");
    const l = profile.limits || {};
    check("zippProfile carries the limits the engine enforces", [l.initialSourceBytes, l.lifetimeSteps, l.maxInstructionBudgetSteps, l.approxHeapBytes, l.linkedMemoryMaxBytes, l.lifetimeOutputBytes, l.syncBridgeBytes].every(Number.isInteger));
    // The budget knob's clamp is the profile's number.
    const e = new Engine();
    check("the budget clamp matches the profile", e.setInstructionBudget(l.maxInstructionBudgetSteps * 10) === true);
    e.dispose();
  }
}

globalThis.performance = realPerformance;
console.log(`\n${pass} passed, ${fail} failed`);
if (fail) process.exit(1);
