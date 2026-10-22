// Positions the engine reports for the host's script, against the production
// artifact. An error's `stack` and a syntax error from `initScript` count
// lines and columns from the host's own source, as `zipp js` and Node count
// a file, though the engine compiles it behind its preamble; a frame of the
// preamble's code is named `zipp:preamble`; `evalInContext` and
// `evalInContextRich` count from the expression and leave their wrapper out;
// `setScriptOrigin` names the script and offsets its positions as Node's
// `vm.Script` options do. Every expected position is Node 24's for the same
// text (the app below run as a file, the expression as a script of its own).
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

const APP = [
  "function f() {",
  "  return new Error('x').stack;",
  "}",
  "var first = f();",
  "function g() {",
  "  throw new TypeError('y');",
  "}",
  "var second; try { g(); } catch (e) { second = e.stack; }",
  "window.addEventListener('ping', function onPing() { throw new Error('in listener'); });",
  "var third; try { window.dispatchEvent({ type: 'ping' }); } catch (e) { third = e.stack; }",
  "var recorded; window.addEventListener('rec', function onRec() { recorded = new Error('r').stack; });",
  "",
].join("\n");

{
  const e = new Engine();
  same("preambleLines is the correction a host still needs: none", e.preambleLines, 0);
  e.initScript(APP);
  same("a stack counts lines from the host's source",
    e.evalInContext("first"), "Error: x\n    at f (<anonymous>:2:10)\n    at <anonymous>:4:13");
  same("so does a thrown error's",
    e.evalInContext("second"), "TypeError: y\n    at g (<anonymous>:6:9)\n    at <anonymous>:8:19");
  const third = e.evalInContext("third");
  // `window.dispatchEvent` delivers through the preamble's own dispatcher, so
  // two of its frames sit between the listener and the app's call.
  check("a frame of the preamble's code is named zipp:preamble",
    /^Error: in listener\n    at onPing \(<anonymous>:9:59\)\n    at __zDispatchEvent \(zipp:preamble:\d+:\d+\)\n    at dispatchEvent \(zipp:preamble:\d+:\d+\)\n    at <anonymous>:10:25$/.test(third),
    JSON.stringify(third));
  e.dispatchEvent("rec", {});
  check("a host-dispatched event's listener sits on the preamble's dispatcher",
    /^Error: r\n    at onRec \(<anonymous>:11:76\)\n    at __zDispatchEvent \(zipp:preamble:\d+:\d+\)$/.test(e.evalInContext("recorded")),
    JSON.stringify(e.evalInContext("recorded")));
  same("evalInContext counts from the expression and leaves its wrapper out",
    e.evalInContext("f()"), "Error: x\n    at f (<anonymous>:2:10)\n    at <anonymous>:1:1");
  same("so does evalInContextRich",
    e.evalInContextRich("  f()"), "Error: x\n    at f (<anonymous>:2:10)\n    at <anonymous>:1:3");
  close(e);
}

{
  const e = new Engine();
  e.setScriptOrigin("app.js", -1, 0);
  e.initScript("// the host's own first line\n" + APP);
  same("setScriptOrigin names the script and moves its lines as vm.Script's lineOffset",
    e.evalInContext("first"), "Error: x\n    at f (app.js:2:10)\n    at app.js:4:13");
  check("the origin is fixed once the script is compiled",
    /immutable after initialization/.test(thrown(() => e.setScriptOrigin("other.js"))));
  close(e);
}

{
  const e = new Engine();
  e.setScriptOrigin("cols.js", 0, 4);
  e.initScript("var c = new Error('c').stack;\nvar d = new Error('d').stack;\n");
  same("columnOffset moves the first line's columns only",
    [e.evalInContext("c"), e.evalInContext("d")],
    ["Error: c\n    at cols.js:1:13", "Error: d\n    at cols.js:2:9"]);
  close(e);
}

{
  const e = new Engine();
  check("an offset must be an integer",
    /RangeError: lineOffset must be an integer/.test(thrown(() => e.setScriptOrigin("a.js", 1.5))));
  check("a name has a size limit",
    /RangeError: a script name may be at most/.test(thrown(() => e.setScriptOrigin("x".repeat(5000)))));
  close(e);
}

{
  const syntax = (source, origin) => {
    const e = new Engine();
    if (origin) e.setScriptOrigin(...origin);
    const err = thrown(() => e.initScript(source));
    close(e);
    return err;
  };
  same("a syntax error says where it is in the host's source",
    syntax("let a = 1;\nlet b = ;\n"), "SyntaxError: unexpected token ';'\n    at <anonymous>:2:9");
  same("under the script's name and offsets",
    syntax("let a = 1;\nlet b = ;\n", ["app.js", 10, 0]), "SyntaxError: unexpected token ';'\n    at app.js:12:9");
  same("a class declaration needs a name",
    syntax("class { }\nconsole.log('ran');\n"), "SyntaxError: unexpected token '{'\n    at <anonymous>:1:7");
}

console.log(`\nstack positions: ${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
