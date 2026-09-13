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

  // A folder as the playground and CLI hand it over: paths as keys, packages
  // by folder, data files and a binary (base64), program arguments, and the
  // files the program writes read back through __zipp_py_vfs_changed.
  {
    const lab = new Engine();
    const blob = Buffer.from(Array.from({ length: 300 }, (_, i) => i % 256));
    const files = {
      "run.py": [
        "import sys, os, json",
        "from legacy.fast_memory import Memory",
        "import pkg.tools as tools",
        "from pkg import extra",
        "with open('data/config.json') as f:",
        "    cfg = json.load(f)",
        "with open('data/model.bin', 'rb') as f:",
        "    raw = f.read()",
        "print(sys.argv, cfg['steps'], Memory().name, tools.twice(cfg['steps']), extra.TAG)",
        "print(len(raw), raw[:3], raw[-1], sorted(os.listdir('data')))",
        "os.makedirs('out', exist_ok=True)",
        "with open('out/result.json', 'w') as f:",
        "    json.dump({'argv': sys.argv[1:], 'total': len(raw)}, f)",
        "with open('out/copy.bin', 'wb') as f:",
        "    f.write(raw[:4])",
        "def draw():",
        "    with open('out/frame.txt', 'a') as f:",
        "        f.write('tick\\n')",
        "",
      ].join("\n"),
      "legacy/fast_memory.py": "class Memory:\n    name = 'fast'\n",
      "pkg/__init__.py": "print('pkg ready')\n",
      "pkg/tools.py": "def twice(n):\n    return n * 2\n",
      "pkg/extra.py": "TAG = 'extra'\n",
      "data/config.json": JSON.stringify({ steps: 21 }),
      "data/model.bin": { base64: blob.toString("base64") },
      "README.md": "# not a module\n",
    };
    const labErr = thrown(() => lab.initPythonProject(files, "run.py", ["--steps", "7"]));
    ok("a path-keyed project with packages, data and a binary runs", labErr === null, labErr);
    eq("argv, packages and files reach the program", lab.takeOutput(), [
      "pkg ready",
      "['run.py', '--steps', '7'] 21 fast 42 extra",
      "300 b'\\x00\\x01\\x02' 43 ['config.json', 'model.bin']",
    ]);
    const written = JSON.parse(lab.pythonCall("__zipp_py_vfs_changed", [])).changes;
    eq("written files come back as path and base64", written.map(f => f.path).sort(), ["out/copy.bin", "out/result.json"]);
    const result = Object.fromEntries(written.map(f => [f.path, f.base64]));
    eq("a written text file decodes", JSON.parse(Buffer.from(result["out/result.json"], "base64").toString()), { argv: ["--steps", "7"], total: 300 });
    eq("a written binary file keeps its bytes", [...Buffer.from(result["out/copy.bin"], "base64")], [0, 1, 2, 3]);
    eq("reporting clears the change set", JSON.parse(lab.pythonCall("__zipp_py_vfs_changed", [])), {version: 1, changes: []});
    lab.pythonCall("draw", []);
    const again = JSON.parse(lab.pythonCall("__zipp_py_vfs_changed", [])).changes;
    ok("a frame's writes are reported on the next call", again.length === 1 && again[0].path === "out/frame.txt", again.join("|"));
    lab.dispose();

    const badBinary = new Engine();
    const b64Err = thrown(() => badBinary.initPythonProject({ "main.py": "print(1)\n", "x.bin": { base64: "***" } }, "main.py", []));
    ok("a malformed base64 file is a usage error", b64Err !== null && /base64|x\.bin/.test(b64Err), b64Err);

    const tests = new Engine();
    const testErr = thrown(() => tests.initPythonProject({ "tests/test_thing.py": "def test_math():\n    assert 2 + 2 == 4\n\ndef test_bad():\n    assert 1 == 2\n" }, "tests/test_thing.py", []));
    ok("a test_*.py entry runs its tests and a failure is a SystemExit", testErr !== null && /SystemExit/.test(testErr), testErr);
    ok("a failed initialization disposes the engine", tests.disposed);
    const testOut = tests.takeFailedConsole().map((e) => e.text).join("\n");
    ok("the test report survives the failed initialization through takeFailedConsole", /test_math PASSED/.test(testOut) && /1 failed, 1 passed/.test(testOut), testOut);
    eq("takeFailedConsole drains", tests.takeFailedConsole(), []);
  }

  {
    const fs = new Engine();
    fs.initPythonProject({"main.py": 'import os\nopen("empty.txt", "w").close()\nopen("truncate.txt", "w").close()\nos.remove("delete.txt")\nos.rename("rename.txt", "renamed.txt")\n',
      "truncate.txt": "old", "delete.txt": "old", "rename.txt": "moved"}, "main.py", []);
    const changes = JSON.parse(fs.pythonCall("__zipp_py_vfs_changed", []));
    eq("explicit versioned VFS changes", changes.version, 1);
    const byPath = Object.fromEntries(changes.changes.map(f => [f.path, f]));
    eq("create an empty file", byPath['empty.txt'], {path:'empty.txt',base64:''});
    eq("truncate to empty", byPath['truncate.txt'], {path:'truncate.txt',base64:''});
    eq("remove file", byPath['delete.txt'], {path:'delete.txt',deleted:true});
    eq("rename removes old path", byPath['rename.txt'], {path:'rename.txt',deleted:true});
    eq("rename retains contents", byPath['renamed.txt'], {path:'renamed.txt',base64:'bW92ZWQ='});
    fs.dispose();
    const dict = new Engine();
    dict.initPythonProject({"main.py": 'def payload():\n    return {"__proto__": {"tag": 42}, "constructor": 7}\n'}, "main.py", []);
    const value = dict.pythonCall('payload', []);
    ok("Python dict keeps an own __proto__ key", Object.hasOwn(value, '__proto__'));
    eq("Python dict preserves special-name values", [value.__proto__.tag, value.constructor], [42, 7]);
    ok("Python dict values do not become inherited properties", !('tag' in value));
    dict.dispose();
  }

  const badProject = new Engine();
  const badErr = thrown(() => badProject.initPythonProject({ main: "import missing\n" }, "main"));
  ok("an unknown import is a compile error", badErr !== null && /No module named 'missing'/.test(badErr), badErr);
  ok("a failed project initialization disposes the engine", badProject.disposed);
  const raising = new Engine();
  thrown(() => raising.initPythonProject({ main: "print('before')\nraise ValueError('boom')\n" }, "main"));
  eq("output printed before a top-level raise is kept for the host", raising.takeFailedConsole().map((e) => [e.stream, e.text]), [["stdout", "before"]]);

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
