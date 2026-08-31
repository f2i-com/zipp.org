// Throughput harness for the wasm build of the engine.
//
// `cargo bench` measures the native engine with its JIT. This measures the
// artifact a browser actually runs: interpreter-only, `safe-sandbox`, with the
// wasm host engine underneath it. Those are different machines and their numbers
// are not interchangeable, so this reports them side by side rather than in
// isolation.
//
//   wasm-bindgen --target nodejs --out-dir tests/node/pkg \
//     target/wasm32-unknown-unknown/release/zipp_wasm.wasm
//   node tests/node/bench.cjs                    # tests/node/pkg
//   node tests/node/bench.cjs ../../other/pkg    # compare two builds
//   node tests/node/bench.cjs pkg --json         # machine-readable
//
// Two costs are reported separately because they are separate problems:
//
//   BOOT     what a host pays before the first line of a tenant's script runs —
//            module compile + instantiate, `new Engine()`, preamble evaluation.
//            Paid per Worker (module) and per tenant (engine).
//   STEADY   what a script pays once it is running. This is interpreter
//            throughput and it is what a long-running script is bounded by.
//
// A host that spawns a fresh Worker per run pays BOOT every time; one that keeps
// an Engine alive pays it once. Conflating them is why "it feels slow" is
// ambiguous.

const fs = require('fs')
const os = require('os')
const path = require('path')

const args = process.argv.slice(2).filter((a) => a !== '--json')
const asJson = process.argv.includes('--json')
const pkgDirs = args.length ? args : [path.join(__dirname, 'pkg')]

// V8 compiles wasm with Liftoff first and only tiers up to TurboFan after a
// function has been called enough. Measuring before that finishes reports the
// baseline compiler and understates the engine by a wide margin, so every
// workload is run to WARMUP before any sample is kept.
const WARMUP = 3
const SAMPLES = 9

// Each workload isolates one cost. Keep them small enough that a run is quick
// and large enough that per-call overhead is not what is being measured.
const WORKLOADS = [
  {
    name: 'arith-int',
    note: 'integer add in a tight loop — raw dispatch cost',
    src: 'function w(n){let s=0;for(let i=0;i<n;i++)s+=i;return s}',
    arg: 200000,
  },
  {
    name: 'arith-mod',
    note: 'JS % — wasm has no float-remainder instruction, so this can hit a software fmod',
    src: 'function w(n){let s=0;for(let i=0;i<n;i++)s+=i%7;return s}',
    arg: 200000,
  },
  {
    name: 'arith-float',
    note: 'f64 math — native wasm instructions',
    src: 'function w(n){let s=0.5;for(let i=0;i<n;i++)s+=i*1.5;return s}',
    arg: 200000,
  },
  {
    name: 'prop-mono',
    note: 'monomorphic property access — inline-cache hit path',
    src: 'function P(x){this.x=x} function w(n){let a=0;const p=new P(1);for(let i=0;i<n;i++)a+=p.x;return a}',
    arg: 200000,
  },
  {
    name: 'prop-poly',
    note: 'polymorphic shapes — inline-cache miss path',
    src: 'function w(n){const o=[{a:1},{a:1,b:2},{a:1,b:2,c:3}];let s=0;for(let i=0;i<n;i++)s+=o[i%3].a;return s}',
    arg: 120000,
  },
  {
    name: 'alloc-object',
    note: 'allocation and GC pressure',
    src: 'function w(n){let s=0;for(let i=0;i<n;i++){const o={a:i,b:i+1};s+=o.a+o.b}return s}',
    arg: 80000,
  },
  {
    name: 'array-build',
    note: 'array growth and element stores',
    src: 'function w(n){const a=[];for(let i=0;i<n;i++)a.push(i);let s=0;for(let i=0;i<a.length;i++)s+=a[i];return s}',
    arg: 80000,
  },
  {
    name: 'array-hof',
    note: 'map/filter/reduce — closure call overhead',
    src: 'function w(n){const a=[];for(let i=0;i<n;i++)a.push(i);return a.map(v=>v+1).filter(v=>v%3===0).reduce((x,y)=>x+y,0)}',
    arg: 40000,
  },
  {
    name: 'call-deep',
    note: 'recursive calls — frame setup and teardown',
    src: 'function fib(n){return n<2?n:fib(n-1)+fib(n-2)} function w(n){return fib(n)}',
    arg: 21,
  },
  {
    name: 'string-build',
    note: 'string concatenation — watch for quadratic behaviour',
    src: 'function w(n){let s="";for(let i=0;i<n;i++)s+="ab"+i;return s.length}',
    arg: 20000,
  },
  {
    name: 'regex',
    note: 'the regex engine under bounded-backtracking',
    src: 'function w(n){let s="";for(let i=0;i<n;i++)s+="k"+i+";";const m=s.match(/\\d+/g);let t=0;for(let i=0;i<m.length;i++)t+=m[i].length;return t}',
    arg: 4000,
  },
  {
    name: 'json',
    note: 'JSON stringify/parse round trip',
    src: 'function w(n){let t=0;for(let i=0;i<n;i++){const o=JSON.parse(JSON.stringify({a:i,b:[1,2,3],c:"x"+i}));t+=o.a}return t}',
    arg: 8000,
  },
]

function stats(samples) {
  const a = samples.slice().sort((x, y) => x - y)
  const best = a[0]
  const median = a[Math.floor(a.length / 2)]
  // Spread between best and median is the noise indicator. A run where they
  // diverge widely was measured on a busy machine and should not be quoted.
  return {
    best: +best.toFixed(3),
    median: +median.toFixed(3),
    spread_pct: +(((median - best) / best) * 100).toFixed(1),
  }
}

function timeIt(fn, samples, warmup) {
  for (let i = 0; i < warmup; i++) fn()
  const out = []
  for (let i = 0; i < samples; i++) {
    const a = process.hrtime.bigint()
    fn()
    const b = process.hrtime.bigint()
    out.push(Number(b - a) / 1e6)
  }
  return stats(out)
}

function measure(pkgDir) {
  const resolved = path.resolve(pkgDir)
  const gluePath = path.join(resolved, 'zipp_wasm.js')
  if (!fs.existsSync(gluePath)) {
    throw new Error(
      'no zipp_wasm.js in ' + resolved + '\n' +
      'Build it first:\n' +
      '  cargo build --locked --release --target wasm32-unknown-unknown\n' +
      '  wasm-bindgen --target nodejs --out-dir ' + pkgDir +
      ' target/wasm32-unknown-unknown/release/zipp_wasm.wasm',
    )
  }
  const wasmPath = path.join(resolved, 'zipp_wasm_bg.wasm')
  const wasmBytes = fs.statSync(wasmPath).size

  // require() compiles and instantiates the module synchronously. Only the first
  // require of a given path does the work, so this is measured once and is the
  // per-Worker cost, not a per-run one.
  const t0 = process.hrtime.bigint()
  const { Engine } = require(gluePath)
  const t1 = process.hrtime.bigint()
  const moduleLoadMs = Number(t1 - t0) / 1e6

  const boot = {
    module_compile_instantiate: { once: +moduleLoadMs.toFixed(3), bytes: wasmBytes },
    engine_new: timeIt(() => { new Engine().dispose() }, SAMPLES, WARMUP),
    init_empty_script: timeIt(() => {
      const e = new Engine()
      e.initScript('')
      e.dispose()
    }, SAMPLES, WARMUP),
  }

  const steady = {}
  for (const w of WORKLOADS) {
    let engine
    try {
      engine = new Engine()
      engine.initScript(w.src)
      // One call outside the timer so first-call compilation of the function
      // body is not attributed to steady-state throughput.
      engine.callFunction('w', [w.arg])
      steady[w.name] = { note: w.note, ...timeIt(() => engine.callFunction('w', [w.arg]), SAMPLES, WARMUP) }
    } catch (err) {
      steady[w.name] = { note: w.note, error: String((err && err.message) || err) }
    } finally {
      try { engine && engine.dispose() } catch { /* already disposed */ }
    }
  }

  return { dir: pkgDir, wasmBytes, boot, steady }
}

// The same workloads on the host's own JS engine. This is a JIT versus an
// interpreter, so it is not a fair fight — it is a scale reference, so a number
// like "18 ms" becomes "this many times the host engine".
function measureHost() {
  const out = {}
  for (const w of WORKLOADS) {
    try {
      // eslint-disable-next-line no-new-func
      const w_ = new Function(w.src + '; return w')()
      for (let i = 0; i < 30; i++) w_(w.arg)
      out[w.name] = timeIt(() => w_(w.arg), SAMPLES, WARMUP)
    } catch (err) {
      out[w.name] = { error: String((err && err.message) || err) }
    }
  }
  return out
}

const env = {
  node: process.version,
  v8: process.versions.v8,
  platform: process.platform,
  arch: process.arch,
  cpu: (os.cpus()[0] || {}).model,
  cores: os.cpus().length,
}

const host = measureHost()
const builds = pkgDirs.map(measure)

if (asJson) {
  console.log(JSON.stringify({ env, host, builds }, null, 2))
} else {
  console.log('zipp wasm throughput')
  console.log('  ' + env.node + ' / V8 ' + env.v8 + ' / ' + env.platform + '-' + env.arch)
  console.log('  ' + env.cpu + ' (' + env.cores + ' cores)')
  console.log('')
  for (const b of builds) {
    console.log(b.dir + '  (' + b.wasmBytes.toLocaleString() + ' bytes)')
    console.log('  BOOT')
    console.log('    module compile+instantiate  ' +
      b.boot.module_compile_instantiate.once.toFixed(2).padStart(9) + ' ms  (once per Worker)')
    console.log('    new Engine()                ' +
      b.boot.engine_new.best.toFixed(3).padStart(9) + ' ms')
    console.log('    initScript("")              ' +
      b.boot.init_empty_script.best.toFixed(3).padStart(9) + ' ms  (preamble)')
    console.log('  STEADY STATE   best ms   vs host')
    for (const [name, r] of Object.entries(b.steady)) {
      if (r.error) {
        console.log('    ' + name.padEnd(16) + '  ERROR ' + r.error.slice(0, 60))
        continue
      }
      const h = host[name]
      const ratio = h && !h.error && h.best > 0 ? (r.best / h.best).toFixed(0) + 'x' : '-'
      const noisy = r.spread_pct > 15 ? '  (noisy, spread ' + r.spread_pct + '%)' : ''
      console.log('    ' + name.padEnd(16) + r.best.toFixed(2).padStart(9) +
        ratio.padStart(9) + noisy)
    }
    console.log('')
  }
  console.log('"vs host" is this engine against the JS engine hosting it — an')
  console.log('interpreter against a tiering JIT. Track it over time; do not read')
  console.log('it as a defect. Compare builds to each other for real signal.')
}
