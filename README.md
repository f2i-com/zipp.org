# zipp

A clean-sheet JavaScript engine written in Rust — a NaN-boxed, explicit-frame
register VM with per-call-site inline caches and a native x86-64 OSR JIT.

```sh
cargo build --release
./target/release/zipp js file.js
./target/release/zipp mjs file.mjs      # ES module entry (top-level await)
```

The project is `crates/zipp-vm`. `zipp-cli` is a thin front end over it, and
`crates/regress-fork` is the ECMAScript regex engine — our fork of regress
0.11.1, which adds an API the engine needs plus three test262 correctness
fixes (see its `FORK.md`).

> The workspace used to carry a separate ahead-of-time language (`zippc`, plus
> Cranelift/LLVM/WASM/zk back ends and a TypeScript frontend). That predates the
> engine and the objective moved; those crates have been removed.

## Where it actually stands

Both figures below are measured on this repo, not estimated. Neither is a
finished result — they are the current state.

**Conformance — 96.97% of test262**, 93,122 of 96,029 required executions
(ECMA-262 + `staging`, run in both sloppy and strict mode as
`INTERPRETING.md` requires):

| slice | executions | pass |
|---|---|---|
| ECMA-262 + staging, both modes | 96,029 | 93,122 (96.97%) |
| intl402 (opt-in, `--include-intl402`) | 3,341 | 563 (16.9%) |

2,214 of the 2,907 failures are **static-semantics early errors the engine does
not raise at all** — `let x; let x;`, `let x; var x;`, duplicate class
constructors and duplicate labels all currently run instead of being a
`SyntaxError`. The cause is that `zipp-vm` pulls `oxc_parser` but not
`oxc_semantic`. The dominant intl402 cause is that `Intl.DateTimeFormat` cannot
be constructed at all (`vm/intl.rs:436`).

`tools/test262-expected-failures.txt` is the checked-in baseline, so a
regression is a diff rather than a remembered number.

**Performance — geomean 2.82× slower than node (V8)** on the ten real-world
benchmarks in `bench/real/`, best-of-5, every output byte-identical to node:

| bench | node | zipp | ratio |
|---|---|---|---|
| map-set-heavy | 552ms | 790ms | 1.43× |
| async-promise-chain | 305ms | 752ms | 2.47× |
| json-large | 220ms | 545ms | 2.48× |
| polymorphic-objects | 291ms | 731ms | 2.51× |
| parse-large-js | 235ms | 609ms | 2.59× |
| class-prototype-hot | 262ms | 750ms | 2.86× |
| markdown-render | 231ms | 730ms | 3.16× |
| sparse-array | 49ms | 166ms | 3.39× |
| typedarray-math | 172ms | 677ms | 3.94× |
| regex-log-scan | 412ms | 1909ms | 4.63× |

Startup is ~2× faster than node (25ms vs 51ms — no snapshot to load).

zipp does beat V8 on specific shapes: scalar-numeric kernels, self-recursive
integer functions, `s += …` string accumulation, non-capturing arithmetic array
pipelines, `for-of` over TypedArray elements, and — measured — **regex scanning**
(a 2000-char no-match `test` runs 25ms against node's 42ms). Those wins do not
carry to the benches above, which are dominated by object construction, property
access and enumeration.

Run `bash bench/run_real.sh` to reproduce; results land in
`bench/results_real.txt`. Run-to-run variance is ±10%, so treat small
differences as noise — `bench/quick.sh` is a 4-bench subset for iteration and
`bench/tiers.sh` reports which JIT tier each loop region reached.

### Why it trails, honestly

This section used to list three causes. Two of them were wrong, and measuring
them is what produced the current list. Details and the numbers are in
`PERF_ROADMAP.md`; the short version:

1. **Object construction, ~143× off.** `new Pt(x,y)` costs 287ns and
   `{a:1,b:2,c:3}` 108ns, against roughly 2ns. Decomposed, that is ~21ns for the
   heap slot plus ~58ns for the FIRST property and ~17ns for each one after.
   Property *reads* are fine by comparison — 2.6ns, about 6.5× off.
2. **Boxed values in the general JIT tier.** Every non-integer loop round-trips
   values through NaN-boxing with a tag guard per operation. Only an optimizing
   tier that keeps values unboxed across operations removes it.
3. **Regions decline `for-of` and allocation.** A `for-of` loop is declined
   outright — not because of the iterator op, but because it desugars to a
   try/finally and the region contains the `PushHandler`. Every `for-of` in the
   engine therefore runs interpreted.

**What is NOT the problem**, each ruled out by measurement rather than argument:

- *The regex engine.* It was previously called "41.8% of everything left, and it
  is the MATCHER". Matching cost is flat in subject length (the memchr prefilter
  works) and we are faster than V8 on a pure scan. The gap is per-call dispatch
  and result-object construction.
- *Property-name interning.* The obvious fix for cause 1 above — `Rc<str>` keys
  behind an intern table — was built to completion and is **5-8% slower**: the
  hash-and-probe costs more than the small `malloc` it replaces.
- *Inline property storage.* `SmallVec` inline slots make construction 1.4-1.9×
  faster in isolation but regress the suite 2.82× → 3.05×, because
  `HeapObj::Object(ObjMap)` stores the map inline and every heap slot grows.
  Boxing `ObjMap` is the prerequisite.

The next concrete step is `HeapObj::Object(Box<ObjMap>)`, then inline storage on
top of it. `PERF_ROADMAP.md` carries the full plan with per-task file:line
anchors, and — deliberately — the negative results too, since each cost a day to
learn and none of them is visible from reading the code.

## Coverage

ES2015–ES2025 is essentially complete: closures, classes (`extends`,
getters/setters, private `#fields`, static blocks), destructuring, spread/rest,
generators and async generators, `async`/`await` and the full `Promise`
combinators, `Map`/`Set`/`WeakMap`/`WeakSet`/`WeakRef`/`FinalizationRegistry`,
all 11 TypedArray kinds plus `DataView`, resizable and transferable
`ArrayBuffer`, `SharedArrayBuffer` and all `Atomics` ops, `BigInt`, `Proxy` and
`Reflect` (all 13 traps with invariant checks), `Symbol` and the well-known
symbols, ES modules including top-level await and import attributes, `eval`,
`with`, labelled statements, `Temporal`, iterator helpers, `Set` methods, and
the modern `RegExp` surface (named groups, lookbehind, `/d` indices, `/v`
unicode sets).

Known gaps: static-semantics early errors (above), most of `Intl` beyond
`en-US` `NumberFormat`, `Float16Array`, and `console` is a compile-time pattern
match rather than a real global object (so `const log = console.log` throws).

## Layout

```
crates/zipp-vm/src/
  compile/     AST -> register bytecode (13 modules)
  codegen/     native x86-64 JIT, dynasm (16 modules)
  vm/          the runtime: dispatch, natives, props, construct, temporal, …
  heap.rs      object model, ObjMap, GC
  value.rs     NaN-boxed Value
  bytecode.rs  instruction set
```

Oversized files are split into module directories by `tools/split_rs.py`, a
lossless splitter that verifies the emitted pieces concatenate byte-identically
to what they replaced. `tools/remap_anchors.py` rewrites doc `file.rs:N` anchors
after a split.

## Development

The standing gate for any engine change — see `PERF_ROADMAP.md` §2:

```sh
cargo build --release
cargo test --release                                   # 270 tests, 0 failed
python tools/run_test262.py --t262 <path> --dump-fails fails.txt
diff <(sort fails.txt) <(sort tools/test262-expected-failures.txt)   # REG=0
ZIPP_NOJIT=1 python tools/run_test262.py …             # and again, interpreter only
bash bench/run_real.sh                                 # ALL_CORRECT=1
```

`ZIPP_NOJIT=1` disables native codegen (it is **presence**-checked, so
`ZIPP_NOJIT=0` also disables it — unset it for a JIT run); `ZIPP_GC_STRESS=1`
collects on every allocation; `ZIPP_JITLOG=1` reports tier decisions, deopts and
evictions; `ZIPP_JITDECLINE=1` names which planner check rejected a region.

Any change touching the JIT must produce identical output both ways —
`assert_jit_matches` in the test suite pins that per case. A JIT change that
only *appears* correct is the common failure mode here: several bugs found in
this engine returned right answers and were caught by a deopt counter or a
missing speedup, not by a test.

## License

Apache-2.0.
