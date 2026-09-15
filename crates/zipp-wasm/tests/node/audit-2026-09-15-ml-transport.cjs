// Binary tensor transport at the wasm boundary (the 15 September 2026 ML
// transport track): a guest Float32Array reaches the host as a Float32Array
// copy, bit for bit; a host Float32Array arrives as a fresh guest array (and,
// in a Python state, as float32 tensor storage); every other typed array
// still reads as null; and a detached or over-budget host array is a
// controlled error that leaves the engine usable.
"use strict";
const { Engine, zippProfile } = require("./pkg/zipp_wasm.js");

let pass = 0;
function same(label, got, want) {
  const actual = JSON.stringify(got);
  const expected = JSON.stringify(want);
  if (actual !== expected) throw new Error(`${label}: got ${actual}, want ${expected}`);
  pass++;
  console.log(`  ok   ${label}`);
}
function bits(a) { return Array.from(new Uint32Array(a.buffer, a.byteOffset, a.length)); }
function f32bits(v) { return new Uint32Array(Float32Array.of(v).buffer)[0]; }
function failure(fn) { try { fn(); return ""; } catch (error) { return String(error && error.message || error); } }

{
  const engine = new Engine();
  engine.setInstructionBudget(500_000_000);
  const syms = engine.initScript(`
    var f = new Float32Array([1.5, -0, 3e38, 0]);
    new Uint32Array(f.buffer)[3] = 0x7fc00001;
    var sub = new Float32Array([0, 1, 2, 3, 4, 5]).subarray(2, 5);
    var d = new Float64Array(2), u = new Uint8Array(2);
    var state = { t: f, n: 1 };
    var x = 0;
    function info(a) { return [a instanceof Float32Array, a.length, a[0], a.buffer.byteLength]; }
    function make(n) { var r = new Float32Array(n); for (var i = 0; i < n; i++) r[i] = i / 4; return r; }
    function checkX() { return [x instanceof Float32Array, x.length, new Uint32Array(x.buffer)[1] === 0x7fc00001, Object.is(x[2], -0)].join(); }
    function sameT() { return state.t === f; }
    function big() { return new Float32Array(5000000); }
  `);
  const [fv, subv, dv, uv, statev] = engine.getGlobalsBatch(
    [syms.f.index, syms.sub.index, syms.d.index, syms.u.index, syms.state.index]);
  same("a guest Float32Array reads as a host Float32Array", fv instanceof Float32Array, true);
  same("its elements cross bit for bit (signed zero, NaN payload)", bits(fv),
    [f32bits(1.5), 0x80000000, f32bits(3e38), 0x7fc00001]);
  same("a view reads its own window of the buffer", [subv instanceof Float32Array, Array.from(subv)], [true, [2, 3, 4]]);
  same("other typed arrays still read as null", [dv, uv], [null, null]);
  same("nested inside data", [statev.t instanceof Float32Array, statev.t.length, statev.n], [true, 4, 1]);
  engine.setGlobalsBatch([syms.state.index], [statev]);
  same("an unchanged echo keeps the guest's own array", engine.callFunction("sameT", []), true);
  const host = new Float32Array([1, 0, -0]);
  new Uint32Array(host.buffer)[1] = 0x7fc00001;
  engine.setGlobalsBatch([syms.x.index], [host]);
  same("a written Float32Array arrives bit for bit", engine.callFunction("checkX", []), "true,3,true,true");
  same("as a call argument", engine.callFunction("info", [new Float32Array([2.5, 3.5])]), [true, 2, 2.5, 8]);
  const made = engine.callFunction("make", [3]);
  same("as a call result", [made instanceof Float32Array, Array.from(made)], [true, [0, 0.25, 0.5]]);

  const buffer = new ArrayBuffer(8);
  const detached = new Float32Array(buffer);
  structuredClone(buffer, { transfer: [buffer] });
  same("a detached host view is a controlled inspection error",
    failure(() => engine.callFunction("info", [detached])).includes("could not be inspected safely"), true);
  same("the engine stays usable after it", engine.callFunction("info", [new Float32Array([1])]), [true, 1, 1, 4]);
  same("a host array past the byte budget is refused",
    failure(() => engine.callFunction("info", [new Float32Array(5_000_000)])).includes("string limit"), true);
  same("a guest array past the byte budget is refused",
    failure(() => engine.callFunction("big", [])).includes("string limit"), true);
  same("and the engine is still usable", engine.callFunction("info", [new Float32Array([2])]), [true, 1, 2, 4]);
  engine.dispose();
}

if (JSON.parse(zippProfile()).languages.includes("python")) {
  const engine = new Engine();
  engine.setInstructionBudget(500_000_000);
  engine.initPythonProject({ main: [
    "import _zipp_tensor as k",
    "def kind(x):",
    "    return [type(x).__name__, k.dtype(x), k.size(x), k.to_list(x)]",
    "def make():",
    "    return k.from_flat('float32', [0.5, -2.0])",
    "def wide():",
    "    return k.from_flat('float64', [0.5])",
    "def draw():",
    "    pass",
    "",
  ].join("\n") }, "main");
  same("a host Float32Array reaches Python as float32 tensor storage",
    engine.pythonCall("kind", [new Float32Array([1.5, 2])]), ["_Storage", "float32", 2, [1.5, 2]]);
  const out = engine.pythonCall("make", []);
  same("float32 storage leaves Python as a Float32Array", [out instanceof Float32Array, Array.from(out)], [true, [0.5, -2]]);
  same("other storage does not cross as bytes", typeof engine.pythonCall("wide", []), "string");
  engine.dispose();
}

console.log(`\n${pass} passed, 0 failed`);
