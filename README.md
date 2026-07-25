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

**Performance — geomean 4.2× slower than node (V8)** on the ten real-world
benchmarks in `bench/real/`, best-of-7, every output byte-identical to node:

| bench | node | zipp | ratio |
|---|---|---|---|
| map-set-heavy | 811ms | 1424ms | 1.76× |
| class-prototype-hot | 262ms | 808ms | 3.08× |
| parse-large-js | 245ms | 791ms | 3.23× |
| json-large | 256ms | 906ms | 3.54× |
| typedarray-math | 182ms | 699ms | 3.84× |
| polymorphic-objects | 295ms | 1136ms | 3.85× |
| async-promise-chain | 311ms | 1349ms | 4.34× |
| markdown-render | 248ms | 1180ms | 4.76× |
| sparse-array | 49ms | 417ms | 8.51× |
| regex-log-scan | 438ms | 4706ms | 10.74× |

Startup is ~1.9× faster than node (30ms vs 58ms — no snapshot to load).

zipp does beat V8 on specific shapes: scalar-numeric kernels, self-recursive
integer functions, `s += …` string accumulation, and non-capturing arithmetic
array pipelines all compile to native code with no per-element call. Those wins
do not carry to the benches above, which are dominated by property access,
allocation and enumeration.

Run `bash bench/run_real.sh` to reproduce; results land in
`bench/results_real.txt`. Note run-to-run variance on these is ±10–17%, so
treat small differences as noise.

### Why it trails, honestly

Three structural reasons, in order of how much they cost:

1. **No shared hidden classes.** Every object owns an `ObjMap { keys:
   Vec<String>, … }` and every property insertion mallocs a fresh `String`
   (`heap.rs`). Measured: ~513ns to build a 4-property object literal and
   ~180ns per key to enumerate one, against low-nanosecond figures in V8.
   This is the dominant term in five of the ten benches.
2. **JIT inline caches key on receiver identity, not shape**, with 8 ways — so
   nine same-shape instances thrash a cache V8 keeps flat.
3. **Regions decline allocation.** `NewObject`/`NewArray`/`MakeClosure` are not
   admitted into a JIT region, so a single object literal in a hot loop keeps
   the whole loop interpreted.

Property-name interning, then shared shapes, is the next major step.
`PERF_ROADMAP.md` is the durable plan, with per-task file:line anchors.

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
  codegen/     native x86-64 JIT, dynasm (15 modules)
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
cargo test --release                                   # 249 tests, 0 failed
python tools/run_test262.py --t262 <path> --dump-fails fails.txt
diff <(sort fails.txt) <(sort tools/test262-expected-failures.txt)   # REG=0
ZIPP_NOJIT=1 python tools/run_test262.py …             # and again, interpreter only
bash bench/run_real.sh                                 # ALL_CORRECT=1
```

`ZIPP_NOJIT=1` disables native codegen; `ZIPP_GC_STRESS=1` collects on every
allocation. Any change touching the JIT must produce identical output both ways
— `assert_jit_matches` in the test suite pins that per case.

## License

Apache-2.0.
