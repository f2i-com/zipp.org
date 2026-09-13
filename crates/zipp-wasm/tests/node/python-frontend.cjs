// The experimental Python frontend at the wasm boundary: `initSource(source,
// "python")` and the JS-only ABI methods' refusal of a Python state.
//
// Adapts to the artifact under test via `zippProfile().languages`, so the one
// file serves both build variants (JavaScript-only, JavaScript + Python):
//   - without "python": Python must be REFUSED with the feature message, and
//     the engine must be disposed the way any failed initialization is;
//   - with "python": the fibonacci program must run to `832040`, output must
//     drain through `takeOutput`, and the global-slot / call / eval methods
//     must reject the state rather than reading the private JS bootstrap.
// JavaScript is present in every artifact: `initScript` and `initSource(...,
// "javascript")` must always work.
"use strict";
const { Engine, zippProfile } = require("./pkg/zipp_wasm.js");

const profile = JSON.parse(zippProfile());
const languages = profile.languages;

let pass = 0, fail = 0;
function ok(label, cond, extra = "") {
  if (cond) { pass++; console.log(`  ok   ${label}`); }
  else { fail++; console.log(`  FAIL ${label} ${extra}`); }
}
function eq(label, got, want) {
  const g = JSON.stringify(got), w = JSON.stringify(want);
  if (g === w) { pass++; console.log(`  ok   ${label}`); }
  else { fail++; console.log(`  FAIL ${label}\n         got  ${g}\n         want ${w}`); }
}
function thrown(fn) {
  try { fn(); return null; } catch (e) { return String(e); }
}

const FIB = [
  "def fib(n):",
  "    a = 0",
  "    b = 1",
  "    for i in range(n):",
  "        a, b = b, a + b",
  "    return a",
  "print(fib(30))",
  "",
].join("\n");

ok("profile lists the artifact's languages, JavaScript always first",
  Array.isArray(languages) && languages[0] === "javascript"
    && languages.every((l) => l === "javascript" || l === "python"),
  JSON.stringify(languages));

// ---- an unknown language is refused before anything is compiled ------------
{
  const e = new Engine();
  const err = thrown(() => e.initSource("print(1)", "cobol"));
  ok("unknown language is refused", err !== null && /unknown language/.test(err), err);
  e.dispose();
}

if (languages.includes("python")) {
  const e = new Engine();
  let symbols;
  const err = thrown(() => { symbols = e.initSource(FIB, "python"); });
  ok("python initSource succeeds", err === null, err);
  eq("python output drains through takeOutput", e.takeOutput(), ["832040"]);
  // The only JS bindings in a Python program are the private runtime
  // bootstrap; the symbol map must not hand the host a slot for them.
  eq("a Python state exposes no global slots", Object.keys(symbols || {}), []);
  for (const [label, call] of [
    ["getGlobalByIndex", () => e.getGlobalByIndex(0)],
    ["setGlobalByIndex", () => e.setGlobalByIndex(0, 1)],
    ["getGlobalsBatch", () => e.getGlobalsBatch([0])],
    ["setGlobalsBatch", () => e.setGlobalsBatch([0], [1])],
    ["getGlobalsFingerprint", () => e.getGlobalsFingerprint([0])],
    ["callFunction", () => e.callFunction("fib", [5])],
    ["evalInContext", () => e.evalInContext("__zipp_py")],
    ["evalInContextRich", () => e.evalInContextRich("__zipp_py")],
  ]) {
    const err = thrown(call);
    ok(`${label} rejects a Python state`,
      err !== null && /unavailable for the experimental Python frontend/.test(err), err);
  }
  ok("the engine is still live after the rejections", !e.disposed);
  e.dispose();

  // A compile error is reported as a source error and disposes the engine,
  // like a JavaScript SyntaxError does.
  const bad = new Engine();
  const compileErr = thrown(() => bad.initSource("x = = 2\n", "python"));
  ok("a Python syntax error is a compile error", compileErr !== null && /SyntaxError/.test(compileErr), compileErr);
  ok("a failed Python initialization disposes the engine", bad.disposed);

  // A runtime error keeps the output produced before it.
  const rt = new Engine();
  const rtErr = thrown(() => rt.initSource("print('before')\nprint(missing)\n", "python"));
  ok("a Python NameError surfaces as the init error", rtErr !== null && /NameError/.test(rtErr), rtErr);
  rt.dispose();

  // ---- projects, the `ui` module and the frame hooks ----------------------
  const p = new Engine();
  const files = {
    main: [
      "import ui",
      "from shapes import box",
      "ticks = [0]",
      "def update():",
      "    ticks[0] = ticks[0] + 1",
      "def draw():",
      "    ui.clear('#123')",
      "    box(10, 20, '#f80')",
      "    ui.text(1, 2, 'tick ' + str(ticks[0]), 'white')",
      "    return ui.button(0, 0, 50, 20, 'go')",
      "def on_key(k):",
      "    return k + '!'",
      "print('project up')",
      "",
    ].join("\n"),
    shapes: "import ui\ndef box(x, y, c):\n    ui.rect(x, y, 30, 30, c)\n",
  };
  const perr = thrown(() => p.initPythonProject(files, "main"));
  ok("initPythonProject runs a multi-file project", perr === null, perr);
  eq("project output drains through takeOutput", p.takeOutput(), ["project up"]);
  eq("pythonHas sees the entry module's functions", [p.pythonHas("draw"), p.pythonHas("update"), p.pythonHas("nope")], [true, true, false]);
  eq("takeUi is empty before a frame", p.takeUi(), []);
  eq("pythonCall returns host data", p.pythonCall("on_key", ["a"]), "a!");
  p.pythonCall("update", []);
  eq("pythonCall without a click returns false from ui.button", p.pythonCall("draw", []), false);
  eq("takeUi drains one frame's commands in order", p.takeUi(), [
    ["clear", "#123"], ["rect", 10, 20, 30, 30, "#f80"], ["text", 1, 2, "tick 1", "white"], ["button", 0, 0, 50, 20, "go"],
  ]);
  eq("takeUi drains", p.takeUi(), []);
  p.setPythonInput(JSON.stringify({ mx: 5, my: 5, down: true, clicked: true, keys: {}, w: 300, h: 200 }));
  eq("setPythonInput drives ui.button", p.pythonCall("draw", []), true);
  const callErr = thrown(() => p.pythonCall("nope", []));
  ok("pythonCall of a missing function is a NameError", callErr !== null && /NameError/.test(callErr), callErr);
  ok("the engine survives a failed pythonCall", !p.disposed);
  eq("lastErrorKind classifies the failed call as guest", p.lastErrorKind(), "guest");
  p.dispose();

  const badProject = new Engine();
  const badErr = thrown(() => badProject.initPythonProject({ main: "import missing\n" }, "main"));
  ok("an unknown import is a compile error", badErr !== null && /No module named 'missing'/.test(badErr), badErr);
  ok("a failed project initialization disposes the engine", badProject.disposed);

  const js = new Engine();
  js.initSource("var x = 1;", "javascript");
  const jsErr = thrown(() => js.pythonCall("draw", []));
  ok("the Python hooks refuse a JavaScript state", jsErr !== null && /needs a Python state/.test(jsErr), jsErr);
  js.dispose();
} else {
  const e = new Engine();
  const err = thrown(() => e.initSource(FIB, "python"));
  ok("python is refused when not built in", err !== null && /python.*feature/i.test(err), err);
  ok("the refused initialization disposes the engine", e.disposed);
}

{
  const e = new Engine();
  ok("initScript is exported", typeof e.initScript === "function");
  const err = thrown(() => e.initSource("console.log(6 * 7);", "javascript"));
  ok("javascript initSource succeeds", err === null, err);
  eq("javascript output drains through takeOutput", e.takeOutput(), ["42"]);
  e.dispose();
}

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
