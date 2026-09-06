// Method-call shapes bench.cjs does not contain: calls whose argument reads
// a global, an element or a property, or does arithmetic on a local — the
// shapes that take the spec-order (captured) lowering under the strict
// default call order (B280), against the same calls over plain locals, and
// the bare property reads the captured lowering pays for. This is the
// measurement behind HANDOFF.md's B289 table.
//
//   wasm-bindgen --target nodejs --out-dir tests/node/pkg \
//     target/wasm32-unknown-unknown/release/zipp_wasm.wasm
//   node tests/node/bench-shapes.cjs                      # tests/node/pkg
//   node tests/node/bench-shapes.cjs ../old/pkg tests/node/pkg   # A/B, best of 20
//
// Run an A/B in both orders and quote the agreeing numbers; a row whose two
// orders disagree by more than a few percent was measured on a busy machine.
// Each engine gets the maximum instruction budget after initScript, since a
// kernel runs the same function 25 times in one engine.
const path = require('path')
const SAMPLES = 20, WARMUP = 5
const K = [
  { name: 'imul-global', arg: 25,
    src: 'var h = 0; var a = []; for (var i = 0; i < 4096; i++) a.push(i % 251);' +
      'function w(n){ h = 2166136261; for (var r = 0; r < n; r++) for (var ti = 0; ti < 4096; ti++) { h = (h ^ a[ti]) | 0; h = Math.imul(h, 16777619) >>> 0; } return h }' },
  { name: 'imul-local', arg: 25,
    src: 'var a = []; for (var i = 0; i < 4096; i++) a.push(i % 251);' +
      'function w(n){ var h = 2166136261; var b = a; for (var r = 0; r < n; r++) for (var ti = 0; ti < 4096; ti++) { h = (h ^ b[ti]) | 0; h = Math.imul(h, 16777619) >>> 0; } return h }' },
  { name: 'method-global-args', arg: 100000,
    src: 'var ctx = { fillRect: function (x, y, w, h) { return x + y + w + h; } }; var px = 3, py = 4, size = 5;' +
      'function w(n){ var s = 0; for (var i = 0; i < n; i++) s += ctx.fillRect(px, py, size, i); return s }' },
  { name: 'method-local-args', arg: 100000,
    src: 'var ctx = { fillRect: function (x, y, w, h) { return x + y + w + h; } };' +
      'function w(n){ var s = 0, px = 3, py = 4, size = 5; for (var i = 0; i < n; i++) s += ctx.fillRect(px, py, size, i); return s }' },
  { name: 'charcode-elem-arg', arg: 20,
    src: 'var src = ""; for (var i = 0; i < 64; i++) src += "abcdefghijklmnopqrstuvwxyz0123456789 "; var starts = []; for (var i = 0; i < 4096; i++) starts.push(i % 2000); var h = 0;' +
      'function w(n){ h = 2166136261; for (var r = 0; r < n; r++) for (var ti = 0; ti < 4096; ti++) { h = (h ^ src.charCodeAt(starts[ti])) | 0; h = Math.imul(h, 16777619) >>> 0; } return h }' },
  { name: 'charcode-local-arg', arg: 20,
    src: 'var src = ""; for (var i = 0; i < 64; i++) src += "abcdefghijklmnopqrstuvwxyz0123456789 ";' +
      'function w(n){ var h = 2166136261, s = src; for (var r = 0; r < n; r++) for (var ti = 0; ti < 2000; ti++) { h = (h ^ s.charCodeAt(ti)) | 0; h = Math.imul(h, 16777619) >>> 0; } return h }' },
  { name: 'push-global-arg', arg: 100000,
    src: 'var arr = []; var g = 0;' +
      'function w(n){ arr.length = 0; for (g = 0; g < n; g++) arr.push(g % 13); return arr.length }' },
  { name: 'push-local-arg', arg: 100000,
    src: 'var arr = [];' +
      'function w(n){ arr.length = 0; var a = arr; for (var i = 0; i < n; i++) a.push(i % 13); return a.length }' },
  { name: 'map-get-expr-arg', arg: 100000,
    src: 'var m = new Map(); for (var i = 0; i < 64; i++) m.set(i, i * 3);' +
      'function w(n){ var s = 0; for (var i = 0; i < n; i++) s += m.get((i & 63) + 0); return s }' },
  { name: 'map-get-local-arg', arg: 100000,
    src: 'var m = new Map(); for (var i = 0; i < 64; i++) m.set(i, i * 3);' +
      'function w(n){ var s = 0, mm = m; for (var i = 0; i < n; i++) { var k = i & 63; s += mm.get(k); } return s }' },
  { name: 'ta-fill-expr-arg', arg: 20000,
    src: 'var ta = new Uint8Array(64);' +
      'function w(n){ var s = 0; for (var i = 0; i < n; i++) { ta.fill(i & 255); s += ta[3]; } return s }' },
  { name: 'indexof-expr-arg', arg: 100000,
    src: 'var e = [5, 6, 7, 8, 9, 10, 11, 12]; var w0 = 6;' +
      'function w(n){ var s = 0; for (var i = 0; i < n; i++) s += e.indexOf(w0 + (i & 3)); return s }' },
  { name: 'getprop-arr-push', arg: 100000,
    src: 'var arr = [];' +
      'function w(n){ var f; for (var i = 0; i < n; i++) f = arr.push; return typeof f }' },
  { name: 'getprop-str-cca', arg: 100000,
    src: 'var s = "abc";' +
      'function w(n){ var f; for (var i = 0; i < n; i++) f = s.charCodeAt; return typeof f }' },
  { name: 'getprop-obj-own', arg: 100000,
    src: 'var o = { push: function () {} };' +
      'function w(n){ var f; for (var i = 0; i < n; i++) f = o.push; return typeof f }' },
]

function timeIt(fn) {
  for (let i = 0; i < WARMUP; i++) fn()
  const out = []
  for (let i = 0; i < SAMPLES; i++) {
    const a = process.hrtime.bigint()
    fn()
    out.push(Number(process.hrtime.bigint() - a) / 1e6)
  }
  out.sort((x, y) => x - y)
  return { best: out[0], median: out[Math.floor(out.length / 2)] }
}

const dirs = process.argv.slice(2)
if (dirs.length === 0) dirs.push(path.join(__dirname, 'pkg'))
const results = {}
for (const dir of dirs) {
  const { Engine } = require(path.join(path.resolve(dir), 'zipp_wasm.js'))
  results[dir] = {}
  for (const k of K) {
    const e = new Engine()
    e.initScript(k.src)
    e.setInstructionBudget(2000000000)
    const first = e.callFunction('w', [k.arg])
    results[dir][k.name] = { value: String(first), ...timeIt(() => e.callFunction('w', [k.arg])) }
    e.dispose()
  }
}
// pkg → node → tests → zipp-wasm → crates → the checkout: name the checkout.
const label = (d) => path.basename(path.resolve(d, '..', '..', '..', '..', '..'))
console.log('kernel (best of ' + SAMPLES + ', ms)'.padEnd(28) + dirs.map((d) => label(d).padStart(14)).join('') + (dirs.length === 2 ? '     delta' : ''))
for (const k of K) {
  const cells = dirs.map((d) => results[d][k.name].best.toFixed(2).padStart(14))
  let delta = ''
  if (dirs.length === 2) {
    const a = results[dirs[0]][k.name], b = results[dirs[1]][k.name]
    delta = a.value !== b.value
      ? '  VALUE MISMATCH ' + a.value + ' vs ' + b.value
      : ((b.best - a.best) / a.best * 100).toFixed(1).padStart(8) + '%'
  }
  console.log(k.name.padEnd(28) + cells.join('') + delta)
}
