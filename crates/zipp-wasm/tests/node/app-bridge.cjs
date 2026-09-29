// The application bridge: `host.callSync(kind, ...args)` answered by
// `Engine.setAppBridge({ call(kind, args) })`, each operation granted by name
// as `app.<kind>`. What a Worker host (bot.computer's sandbox) builds its
// synchronous file and network access on.
"use strict";
const { Engine } = require("./pkg/zipp_wasm.js");

let pass = 0, fail = 0;
function eq(label, got, want) {
  const g = JSON.stringify(got), w = JSON.stringify(want);
  if (g === w) { pass++; console.log(`  ok   ${label}`); }
  else { fail++; console.log(`  FAIL ${label}\n         got  ${g}\n         want ${w}`); }
}
function ok(label, cond, extra = "") {
  if (cond) { pass++; console.log(`  ok   ${label}`); }
  else { fail++; console.log(`  FAIL ${label} ${extra}`); }
}
function throws(label, fn, pattern) {
  try { fn(); fail++; console.log(`  FAIL ${label}: did not throw`); }
  catch (e) {
    const m = String(e && e.message !== undefined ? e.message : e);
    if (pattern.test(m)) { pass++; console.log(`  ok   ${label}`); }
    else { fail++; console.log(`  FAIL ${label}: ${m}`); }
  }
}

const calls = [];
const files = new Map([["notes.txt", "hello"]]);
const bridge = {
  call(kind, args) {
    calls.push([kind, args]);
    switch (kind) {
      case "fs.read": return files.has(args[0]) ? { ok: files.get(args[0]) } : { err: `ENOENT: ${args[0]}` };
      case "fs.write": files.set(args[0], args[1]); return { ok: "" };
      case "echo": return args;
      case "boom": throw new Error("secret host detail");
      default: return { err: `unknown ${kind}` };
    }
  },
};

console.log("— granted calls reach the bridge, replies cross as JSON —");
const e = new Engine();
e.setAppBridge(bridge);
e.setSyncHostCapabilities(["app.fs.read", "app.fs.write", "app.echo", "app.boom"]);
e.initScript(`
  function read(p) { return host.callSync("fs.read", p); }
  function write(p, t) { return host.callSync("fs.write", p, t); }
  function echo() { return host.callSync.apply(host, ["echo"].concat(Array.prototype.slice.call(arguments))); }
  function boom() { try { host.callSync("boom"); return "no throw"; } catch (e) { return String(e.message || e); } }
  function denied() { try { host.callSync("fs.delete", "x"); return "no throw"; } catch (e) { return String(e.message || e); } }
  function badName() { try { host.callSync("Fs.Read"); return "no throw"; } catch (e) { return String(e.message || e); } }
  function direct() { return JSON.parse(__zippHostCall("app.fs.read", "notes.txt")); }
  var app = "a guest's own binding named app still compiles";
`);
eq("read", e.callFunction("read", ["notes.txt"]), { ok: "hello" });
eq("write then read", (e.callFunction("write", ["new.txt", "fresh"]), e.callFunction("read", ["new.txt"])), { ok: "fresh" });
eq("the bridge gets the kind without its prefix and string arguments", calls[0], ["fs.read", ["notes.txt"]]);
eq("any argument count within the envelope", e.callFunction("echo", ["a", "b", "c", 4]), ["a", "b", "c", "4"]);
eq("zero arguments", e.callFunction("echo", []), []);
eq("errors returned as data pass through", e.callFunction("read", ["missing"]), { err: "ENOENT: missing" });
const boom = e.callFunction("boom", []);
ok("a bridge throw is opaque to the guest", /host bridge call failed/.test(boom) && !/secret/.test(boom), boom);
ok("an ungranted kind is denied", /capability denied/.test(e.callFunction("denied", [])));
ok("a malformed kind is unknown", /unknown host call/.test(e.callFunction("badName", [])));
eq("the raw host call is the same channel", e.callFunction("direct", []), { ok: "hello" });
ok("no global named app is imposed on guests", e.evalInContext("app").startsWith("a guest's own"));

console.log("— configuration —");
throws("a malformed capability name is refused", () => new Engine().setSyncHostCapabilities(["app.fs.read", "app.Bad"]), /unknown synchronous host capability/);
throws("a bare app. capability is refused", () => new Engine().setSyncHostCapabilities(["app."]), /unknown synchronous host capability/);
throws("the bridge must be an object", () => new Engine().setAppBridge(null), /app bridge must be a non-null object/);
const noBridge = new Engine();
noBridge.setSyncHostCapabilities(["app.fs.read"]);
noBridge.initScript(`function r() { try { return host.callSync("fs.read", "x"); } catch (e) { return String(e.message || e); } }`);
ok("a grant without a bridge is refused cleanly", /bridge is unavailable/.test(noBridge.callFunction("r", [])));
const late = new Engine();
late.initScript("var x = 1;");
throws("the bridge cannot be installed after initialization", () => late.setAppBridge(bridge), /./);

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
