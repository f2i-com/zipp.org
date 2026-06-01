# ZIPP language

A sound-TypeScript-subset language, AOT-compiled, in Rust — you can write it in
**real TypeScript** (`.ts`, parsed by `oxc`) or ZIPP's own syntax. The full
design lives in [`../ZIPP.md`](../ZIPP.md); this repo is the standalone
implementation.

It also ships a **separate dynamic JavaScript engine** (`zipp js`) that runs
ordinary `.js` and is competitive with V8 — see
[Dynamic JavaScript engine](#dynamic-javascript-engine-zipp-js).

ZIPP has **three execution profiles** from one frontend + IR — all three are
implemented:

| Profile | Flag | Target | Competes with |
|---|---|---|---|
| **native / dApp** | `--jit` / `--llvm` | machine code (Cranelift / clang -O3) | V8 |
| **contract** | `--wasm` | deterministic, gas-metered WebAssembly | the EVM |
| **provable** *(optional)* | `--prove` | zk-STARK over a register-VM trace | Cairo / a STARK VM |

- **native**: the JIT and LLVM tiers (above) — match/beat V8 & Bun, GC'd.
- **contract**: `--wasm` emits a small gas-metered WASM module that runs anywhere
  (deterministic, sandboxed) and traps when it runs out of gas — the on-chain
  contract lane, a candidate successor to FormLogic ("Cairo for ZIPP").
- **provable**: `--prove` produces *and verifies* a zk-STARK proof that the
  execution was computed correctly, the same application-specific-STARK approach
  the ZIPP chain uses for FormLogic, over ZIPP's own VM.

The optional profiles build out by default; `--no-default-features` drops the
heavy deps (Winterfell/Cranelift) and the language still runs on the interpreter.

## Status (v0 — a working vertical slice, not the finished language)

✅ Working end-to-end today:
- Two front-ends: the built-in lexer/recursive-descent parser (`.zipp`) **and a
  real TypeScript frontend** (`.ts`, via `oxc`) that lowers the sound subset to
  the same AST — `zipp run app.ts` runs on every backend below
- **Sound-subset type checker** (`i64`/`f64`/`bool`/`str`, no implicit coercions,
  arity/return checking)
- **`f64` floating-point** with `i64()` / `f64()` casts (the zk profile stays
  integer-only — `--prove` rejects f64, per PLAN.md §7)
- **Sized integers** `i32` / `u32` / `u64` (default is `i64`), reached via casts
  (`u32(e)` …) with Rust `as` semantics — wrapping arithmetic, signed/unsigned
  div/compare/shift, exact across interpreter/`--jit`/`--llvm` (see `examples/fnv.zipp`)
- **Lexical block scoping with shadowing**; `while` / **`for`** loops,
  `break` / `continue`; **short-circuit** `&&` / `||`
- Full operator set incl. **bitwise/shift** `& | ^ << >> ~`
- **Arrays**: literals `[a, b, c]`, repeat `[v; n]`, indexing read/write, `len()`,
  runtime bounds checks (reference types; also `--prove`-gated for now)
- **Strings**: literals with escapes, `+` concat, `==`/`!=`, `len()`, `print`, and
  a native **method stdlib** —
  `charCodeAt`/`charAt`/`slice`/`indexOf`/`lastIndexOf`/`includes`/`startsWith`/`endsWith`/`repeat`
  (byte-level / ASCII-exact vs TS; heap-backed, immutable; `--prove`-gated)
- **Structs**: `struct Point { x: i64, y: i64 }`, construction, field read/write,
  nesting (heap-backed reference types; `--prove`-gated)
- **Builtins**: `len`; `abs`/`min`/`max`/`pow` (integers); `sqrt`/`floor`/`ceil` (floats)
- Lowering to **register-machine bytecode**; functions, recursion, `if` / `while`
- A **VM** that runs it (`zipp run`)
- The **optional zk-STARK profile** (`zipp run --prove`): Winterfell proof +
  verification over the VM execution trace
- A **native JIT** (`zipp run --jit`, Cranelift): compiles the **entire
  language** — scalars (`i64`/`f64`, casts), arrays, strings, structs, math
  builtins — to machine code with a **conservative mark-sweep GC**; nothing
  falls back. A fast-compile tier-0 that beats V8 on integer loops (see below)
- An **LLVM release tier** (`zipp run --llvm`): emits LLVM IR and compiles it
  with `clang -O3 -march=native` — same coverage (incl. the GC), **matches V8 on
  dense f64** and **beats V8 and Bun on dense-f64 arrays** (matmul). No
  `llvm-sys` linkage; it shells out to `clang` and links the shared runtime.
- A **WASM contract profile** (`zipp run --wasm`): emits **deterministic,
  gas-metered WebAssembly** for the scalar subset — the ZIPP.md §7 lane that
  competes with the EVM. Pure-Rust assembler (`wat`); a per-basic-block gas
  counter traps on exhaustion (`--gas N`). Heap types fall back to the interpreter.
- An integration **test suite** (`cargo test`)
- **Positioned errors** — parse errors report `line:col`, type errors report the
  statement line (e.g. `type error: arithmetic Add on I64 and Bool [line 2]`)

🚧 Roadmap (see `../ZIPP.md` for the full plan):
- Runtime-error positions (bytecode → source mapping; compile errors are done)
- Frontend: swap the hand-written parser for **oxc/SWC** (real TS/JSX)
- IR: split into ZHIR + ZMIR (monomorphization, comptime, escape analysis, SoA)
- Backends: **Cranelift** tier-0 JIT and an **LLVM** release tier (`clang -O3`)
  — the **whole language** compiles natively (matches V8 on dense f64; beats
  V8/Bun on matmul), and **both share one runtime with a conservative mark-sweep
  GC** (`zipp-rt`). Next: LTO/PGO and a **WASM-contract** target
- Parallel work-stealing scheduler (the §5.8 flagship), GC/arenas, fast stdlib
- zk hardening: PC-integrity + memory-permutation arguments, 64-bit range checks

## Quickstart

```bash
cargo build --release

# run
./target/release/zipp run examples/fib.zipp        # => 55
./target/release/zipp run examples/sum.zipp         # => 385
./target/release/zipp run examples/bits.zipp        # => 247  (bitwise/shift)
./target/release/zipp run examples/pi.zipp          # => 3.14159...  (f64 + casts)
./target/release/zipp run examples/arrays.zipp      # bubble sort (arrays + len)
./target/release/zipp run examples/hello.zipp       # strings: concat, len
./target/release/zipp run examples/fizzbuzz.zipp    # for + % + else-if + strings
./target/release/zipp run examples/math.zipp        # abs/min/max/pow/sqrt/floor/ceil
./target/release/zipp run examples/structs.zipp     # records + field access
./target/release/zipp run examples/fnv.zipp          # 32-bit FNV-1a hash (u32 wrapping)

# real TypeScript, run on any backend (oxc frontend → ZIPP AST)
./target/release/zipp run examples/fib.ts            # => 6765
./target/release/zipp run --jit examples/sum.ts      # loop + u64 + array
./target/release/zipp run --llvm examples/point.ts   # interface -> struct
./target/release/zipp run --jit examples/account.ts  # class: methods + new + this
./target/release/zipp run --llvm examples/generic.ts # generic functions
./target/release/zipp run --jit examples/box.ts      # generic classes Box<T>/Pair<A,B>
./target/release/zipp run --llvm examples/tuple.ts   # tuples [i64,str] + destructuring
./target/release/zipp run examples/cards.ts          # numeric enum + for...of
./target/release/zipp run --wasm examples/calc.ts    # switch (runs in the contract profile)
./target/release/zipp run --jit examples/narrow.ts   # string switch + early-return narrowing
./target/release/zipp run --jit examples/ternary.ts  # ternary ?: (a lazy recursive fib)
./target/release/zipp run examples/destructure.ts    # string enum + destructuring
./target/release/zipp run examples/defaults.ts       # default parameter values
./target/release/zipp run examples/optional.ts       # T | null, ??, narrowing
./target/release/zipp run --llvm examples/chain.ts   # optional chaining a?.b?.c ?? d
./target/release/zipp run --jit examples/optcall.ts  # optional method calls a?.m() ?? d
./target/release/zipp run --jit examples/nullable_heap.ts # str | null, T[] | null (native)
./target/release/zipp run examples/nullable_scalar.ts # i64 | null (interpreter tier)
./target/release/zipp run examples/lambda.ts         # first-class functions + arrow lambdas
./target/release/zipp run examples/closure.ts        # closures (arrows capture enclosing vars)
./target/release/zipp run examples/arraymethods.ts   # growable arrays + map/filter/reduce/push/pop
./target/release/zipp run examples/arraymethods2.ts  # some/every/findIndex/slice/concat/reverse/fill
./target/release/zipp run --jit examples/arraysearch.ts # indexOf/includes/reverse/fill run NATIVE
./target/release/zipp run --jit examples/strings.ts  # string methods (slice/indexOf/… run NATIVE)
./target/release/zipp run --jit examples/calculator.ts # recursive-descent calculator (runs NATIVE)
./target/release/zipp run examples/analytics.ts      # data pipeline: closures + map/filter/reduce
./target/release/zipp run --jit examples/sort.ts     # in-place sort + binary search (runs NATIVE)

# run the language test suite
cargo test

# run natively via the Cranelift JIT (scalar i64/f64 programs; falls back otherwise)
./target/release/zipp run --jit examples/fib.zipp   # i64
./target/release/zipp run --jit examples/pi.zipp    # f64 + casts
./target/release/zipp run --jit bench/loop.zipp

# opt-in FMA contraction (faster dense f64; changes float rounding, so not default)
./target/release/zipp run --jit --ffast-math bench/mandelbrot_fma.zipp

# LLVM release tier: emit LLVM IR + compile with clang -O3 (needs clang on PATH,
# or set ZIPP_CLANG). Matches V8 on dense f64; beats V8/Bun on dense-f64 arrays.
./target/release/zipp run --llvm bench/mandelbrot.zipp
./target/release/zipp run --llvm bench/matmul.zipp     # 256x256 f64 array kernel
./target/release/zipp run --jit  examples/arrays.zipp  # arrays JIT too (bubble sort)

# contract profile: deterministic, gas-metered WebAssembly (needs node to run)
./target/release/zipp run --wasm examples/fib.zipp           # => 55  (… N gas)
./target/release/zipp run --wasm --gas 1000 bench/loop.zipp  # traps: out of gas

# run + zk-STARK prove + verify the execution
./target/release/zipp run --prove examples/add.zipp
./target/release/zipp run --prove examples/sum.zipp

# build without the zk profile (drops the Winterfell dependency)
cargo build --release -p zipp-cli --no-default-features

# dynamic JavaScript — the separate JS engine (zipp-vm), like `node file.js`
./target/release/zipp js bench/fib.js          # recursion
./target/release/zipp js bench/loop.js         # hot integer loop (runs native)
./target/release/zipp js bench/array.js        # closures + map/filter/reduce
```

A ZIPP program (`examples/fib.zipp`):

```ts
fn fib(n: i64): i64 {
    if (n < 2) { return n; }
    return fib(n - 1) + fib(n - 2);
}
fn main(): i64 {
    let r = fib(10);
    print(r);
    return r;
}
```

## Layout

```
zipp-lang/
├── crates/
│   ├── zippc/        # compiler core + register VM: lexer, ast, parser, check, ir, vm
│   ├── zipp-zk/      # OPTIONAL zk-STARK profile (Winterfell prover/verifier over the trace)
│   ├── zipp-rt/      # shared native runtime: allocator + conservative GC (rlib + staticlib)
│   ├── zipp-jit/     # OPTIONAL native backend (Cranelift JIT, tier-0)
│   ├── zipp-llvm/    # OPTIONAL release tier (emit LLVM IR, compile with clang -O3)
│   ├── zipp-wasm/    # OPTIONAL contract profile (emit gas-metered WebAssembly)
│   ├── zipp-ts/      # OPTIONAL TypeScript frontend (oxc → ZIPP AST, sound subset)
│   ├── zipp-vm/      # dynamic JavaScript engine: explicit-frame register VM + native x86-64 OSR JIT (`zipp js`)
│   └── zipp-cli/     # the `zipp` binary
└── examples/         # add, sum, fib, bits, pi, arrays, hello, fizzbuzz, math, structs, fnv
```

The modules inside `zippc` map onto the separate crates in `../ZIPP.md §15`
(zipp-frontend / zipp-checker / zipp-zhir / zipp-zmir / zipp-runtime); they're
kept together for v0 and can be split as each gains weight.

## zk soundness boundary (v0 — honest)

The v0 AIR constrains the **arithmetic** of the trace (`Const`/`Add`/`Sub`/`Mul`),
selector booleanity/exclusivity, a monotonic clock, a boundary assertion binding
the final value to the public result, and a **program-hash binding** (a constant
column tied to the public inputs, so a proof for one program won't verify as
another). Control-flow / memory steps are recorded but **not yet** constrained
(no PC-integrity or memory-permutation argument; no 64-bit range checks). Proven arithmetic must be non-negative and
non-overflowing (true for the bundled examples). Hardening this is the roadmap —
the same path `zk-formlogic` took to its 78-column trace.

## Performance (honest, measured)

Vs Node 24 (V8) and Bun 1.3 (JavaScriptCore). All engines compute the identical
result on each kernel (the comparison is apples-to-apples); kernels are in
`bench/` with `.js` twins. **execute = the generated code's runtime, measured
separately from compile** — the JIT splits its own timing; the `--llvm` exe
self-times the kernel with `clock()` (so its number excludes process startup,
like JS `hrtime`). Cranelift compile is ~1 ms; the `--llvm` clang invocation is
~100 ms one-shot AOT.

**Dense f64** — 1000×1000 Mandelbrot, 256-iter cap (`bench/mandelbrot.zipp`).
This is the meaningful parity test: the computation is chaotic, so no optimizer
can shortcut it.

| engine | execute | vs V8 |
|---|---|---|
| ZIPP `--jit` (Cranelift, tier-0) | ~127 ms | ~1.27× slower |
| Bun 1.3 (JavaScriptCore) | ~107 ms | ~1.06× slower |
| Node 24 (V8 JIT) | ~101 ms | 1× |
| **ZIPP `--llvm` (clang -O3 -march=native)** | **~99 ms** | **~1.0× — on par** |

**Dense f64 — arrays** — 256×256 matrix multiply (`bench/matmul.zipp`), flat
`[f64]`, computed-index load/store in a hot triple loop. This exercises the
native **array** path (repeat-alloc, bounds-checked indexing):

| engine | execute | vs V8 |
|---|---|---|
| ZIPP `--jit` (Cranelift) | ~16 ms | ~1.1× faster |
| Bun 1.3 (JavaScriptCore) | ~17 ms | ~1.05× faster |
| Node 24 (V8 JIT) | ~18 ms | 1× |
| **ZIPP `--llvm` (clang -O3)** | **~14 ms** | **~1.3× faster** |

On this array kernel **both** ZIPP native backends beat V8 *and* Bun — `--llvm`
fastest. (Interpreter: ~455 ms, so the JIT/LLVM are ~28–32× over it.)

**Integer** — 50M-iteration sum loop (`bench/loop.zipp`):

| engine | execute | vs V8 |
|---|---|---|
| ZIPP interpreter | ~566 ms | ~19× slower |
| Node 24 (V8 JIT) | ~30 ms | 1× |
| ZIPP `--jit` (Cranelift) | ~10 ms | ~3× faster |
| ZIPP `--llvm` (clang -O3) | ~0 ms* | loop solved at compile time |

**Closures — call throughput** — 20M calls through a capturing closure
(`bench/closures.{ts,js}`); stresses the env-pointer indirect-call ABI:

| engine | execute | vs V8 |
|---|---|---|
| Node 24 (V8 JIT) | ~370 ms | 1× |
| Bun 1.3 (JSC) | ~192 ms | ~1.9× faster |
| ZIPP `--jit` | ~75 ms | ~4.9× faster |
| **ZIPP `--llvm`** | **~43 ms** | **~8.6× faster** |

**Functional pipeline** — 2000× a `map → filter → reduce` over a 2000-element
array (`bench/pipeline.{ts,js}`); allocation-heavy *and* closure-heavy:

| engine | execute | vs V8 |
|---|---|---|
| ZIPP interpreter | ~383 ms | ~12× slower |
| Bun 1.3 (JSC) | ~34 ms | ~1.1× slower |
| Node 24 (V8 JIT) | ~30 ms | 1× |
| ZIPP `--jit` | ~39 ms | ~1.3× slower |
| **ZIPP `--llvm`** | **~26 ms** | **~1.15× faster** |

A fluent `xs.map(f).filter(g).reduce(h, init)` chain is **fused** (deforestation)
into a single pass with no intermediate arrays — so `--llvm` edges out V8 even on
the alloc-heavy functional workload, the one regime AOT normally trails a tracing
JIT. (Closures lower to a cheap `call_indirect`, no deopt/inline-cache overhead,
on native `i64` — hence the lopsided closure-throughput win.)

ZIPP has **three native paths**: the Cranelift **`--jit`** (PLAN.md tier-0 — a
fast-*compile* baseline) and the **`--llvm`** release tier (`clang -O3`),
plus the interpreter fallback (now only used when you don't pass `--jit`/`--llvm`).
Both native backends cover the **entire language** — scalars (`i64` + `f64`,
casts), 1-D arrays, strings, structs, math builtins, **first-class functions +
closures, and growable arrays + the `map`/`filter`/`reduce` stdlib** (only
nullable *scalars* still fall back to the interpreter).

- **`--llvm` matches V8 on dense f64** (~99 ms vs ~101 ms) and **beats V8/Bun on
  dense-f64 arrays** (matmul). `-O3` auto-FMAs, schedules, reassociates and
  vectorizes; on this CPU it lands level-with or ahead-of TurboFan.
- **`--llvm` beats V8 on closures (~8.6×) and the functional pipeline (~1.15×).**
  Closures compile to a cheap `call_indirect` (env-pointer ABI, no deopt/IC
  overhead) on native `i64`; fluent `map/filter/reduce` chains are **fused** into
  one pass with no intermediate arrays — so even the alloc-heavy functional case
  edges out V8, the one regime AOT usually trails a tracing JIT.
- **`--jit` beats V8 ~3× on the integer loop** (~10 ms vs ~30 ms) and edges it on
  matmul, but trails it ~27% on dense-f64 Mandelbrot (~127 ms) — Cranelift is a
  fast-compile *baseline* compiler, not an optimizer (see below).
- **Bun vs V8?** On these numeric kernels they're within a few percent of each
  other (Bun/JSC ~107 ms vs V8 ~101 ms on Mandelbrot; tied on the integer loop).
  Bun's edge is startup / bundling / I/O, not hot-loop numeric throughput — so
  it is **not** faster than V8 here.
- *The integer loop on `--llvm` reports ~0 ms because `-O3` recognized the
  induction variable and folded the whole sum to a **closed-form constant** — a
  correct result, but the loop was optimized away, so it's evidence of LLVM's
  optimizer strength, **not** a like-for-like "ran the loop faster" claim. (V8
  runs the loop; it doesn't do this algebraic fold.) The Mandelbrot row is the
  honest like-for-like comparison.

### Why the *JIT* trails on dense f64 — the diagnosis

Worth recording, because it explains the tier split. The Mandelbrot inner loop
is a dependency chain (each iteration's `x,y` depend on the last), so wall-clock
is the **critical path**, not throughput.

1. **Compile time is negligible** (~1 ms) — the JIT gap is the generated code.
2. **FMA contraction alone doesn't help.** `--ffast-math` fuses `a*b ± c` into
   one rounded `vfmadd`. It fires (3 pairs) but naive left-to-right fusion only
   shortens the `y` update; the *binding* path is the `x` update
   `x*x - y*y + cx`, unchanged → no speedup.
3. **Reassociation is the lever.** Written `(cx - y*y) + x*x`, the fuser builds
   a *nested* `fma(x, x, fma(-y, y, cx))`, shortening that path →
   `bench/mandelbrot_fma.zipp` runs at ~109 ms on the JIT (within ~7% of V8).
   This is exactly what `--llvm` (`-O3`) does **automatically** without hand-
   reassociated source — which is why the release tier reaches parity and the
   baseline JIT does not.

So the two tiers do what they're for: **Cranelift** = fast compile, great for
startup/integer code (beats V8 there); **LLVM** = slow compile, peak code,
matches V8 on dense f64. Both knob sets are maxed (Cranelift: `opt_level=speed`,
host AVX/FMA, verifier off, opt-in FMA; LLVM: `-O3 -march=native`).

`--ffast-math` is **opt-in** on both backends: FMA/reassociation change float
rounding (last-bit), so the strict default keeps native output bit-identical to
the interpreter — which matters for the deterministic contract/provable profiles.

One kernel isn't the whole story (PLAN.md §6/§11 — workloads differ, and V8's
hand-tuned stdlib is a separate battle), but this is real, reproducible evidence
the thesis holds where the design predicts.

### Garbage collection (both backends)

Both native backends share one runtime crate, **`zipp-rt`** (linked as an rlib
into the JIT and as a static lib into the LLVM exe), which provides the allocator
and a **conservative mark-sweep GC**. ZIPP heap handles (arrays/strings/structs)
are always `i64`, so they only ever live in GPRs or on the stack — never in float
registers — which bounds the root set: the collector captures the callee-saved
GPRs and conservatively scans the machine stack, marks through reachable objects
(so a struct field holding an array keeps it alive), and sweeps the rest. It's
sound for this value model with no codegen changes or stack maps; it's per-thread
(matching ZIPP's single-threaded execution); and it never moves objects, so
handles stay valid. The same collector runs in-process under `--jit` and inside
the compiled exe under `--llvm`.

On `bench/churn.zipp` (a fresh 64-element array every iteration, 2M iterations,
only the current one live), peak RSS — GC on vs off (`ZIPP_GC=0`):

| backend | GC on | GC off | result |
|---|---|---|---|
| `--jit` | ~11 MB | ~1.1 GB | ✓ |
| `--llvm` | ~8 MB | ~1.1 GB | ✓ |

~100–140× less memory, same answer. String *literals* are allocated outside the
GC (immortal — the JIT bakes them into the code, the LLVM tier puts them in the
exe's read-only data). Set `ZIPP_GC=0` to disable collection.

## Contract profile (WASM, gas-metered)

`zipp run --wasm` compiles the scalar subset to a small, deterministic WASM
module and runs it (under Node) with a **gas budget**. Gas is emitted *into* the
module — a `$gas` global is charged per basic block and traps (`unreachable`)
when it hits zero — so metering is part of the artifact, not the runtime, exactly
like an on-chain VM. Arbitrary control flow is lowered with the standard
`br_table` dispatch loop (no relooper). Heap types fall back to the interpreter.

| program | module | gas | result |
|---|---|---|---|
| `examples/fib.zipp` | 559 B | 1,773 | 55 |
| `examples/bits.zipp` | 404 B | 20 | 247 |
| `examples/pi.zipp` (f64) | 604 B | 18,000,016 | 3.14159… |
| `bench/loop.zipp` (50M) | 446 B | 450,000,011 | 1250000025000000 |

`--gas N` sets the budget; a runaway loop traps "out of gas" (e.g.
`zipp run --wasm --gas 1000 bench/loop.zipp`). Results match the interpreter
exactly. The pure-Rust `wat` crate assembles the text to a `.wasm`; no external
toolchain. *(Future: a gas-priced heap/runtime to lift the scalar-only limit.)*

## TypeScript frontend

`zipp run app.ts` parses **real TypeScript** with `oxc` and lowers the *sound
subset* to ZIPP's AST — then the existing checker, IR, GC and all four targets
run unchanged. Only the front-end is new:

```
app.ts ─[oxc]→ TS AST ─[lower the subset]→ ZIPP AST ─→ check → IR → { interp · jit · llvm · wasm · prove }
```

`oxc` parses *all* TS syntax; we lower what maps to ZIPP's sound core and reject
the rest with a line-numbered error. **Supported (v0):** typed functions +
recursion (with **default parameters**), `let`/`const` (incl. array/object
**destructuring**),
`if`/`while`/`for`/`for…of`/`switch`, `break`/`continue`, the operator set +
ternary `?:`, numeric casts (`i64(x)`/`u32(x)`/…), arrays (`T[]`, indexing,
`.length`, **growable** with `push`/`pop`, and a **method stdlib**:
`map`/`filter`/`reduce`/`some`/`every`/`findIndex`/`indexOf`/`includes`/`slice`/`concat`/`reverse`/`fill`),
**strings** (`+`, `==`, `len`, and a native method stdlib
`charCodeAt`/`charAt`/`slice`/`indexOf`/`lastIndexOf`/`includes`/`startsWith`/`endsWith`/`repeat`
— byte-level/ASCII-exact vs TS),
**tuples** (`[i64, str]` — positional indexing + destructuring),
numeric **and string `enum`s**, **`interface`s and `class`es → structs**
(interfaces construct with `let p: T = {…}` / `{…} as T`; classes give you
fields, a constructor, methods, `this`, and `new C(…)` — a class lowers to a
factory plus methods taking `this`), field read/write, **generics** — both
functions (`f<T>(…)`) and classes (`class Box<T>`) — monomorphized per use to
concrete types (inferred or explicit), so the backends only ever see concrete
code (a `Box<i64>` and a `Box<bool>` are two distinct structs), **optionals** —
`T | null` for heap types (structs, `str`, arrays) and nullable scalars
`i64 | null` / `f64 | null` / `bool | null`, plus `null`, `x ?? y`, `=== null`,
flow narrowing (`if (x !== null) {…}` and early-return `if (x === null) return;`),
and optional chaining `a?.b` / `a?.m()` / `… ?? default`, **first-class
functions** — pass functions as values, function-typed parameters
(`f: (n: i64) => i64`), arrow lambdas (`(x: i64) => x * 2`), **closures** (arrows
capture enclosing variables, e.g. `adder(n) = (x) => x + n`), indirect calls, and
currying (`add(1)(2)(3)`) — `console.log`, math builtins. (`switch` works on
numbers, strings, and enums.)
**Type mapping:** `number`→f64, `bigint`→i64,
`boolean`→bool,
`string`→str, and `i64`/`i32`/`u32`/`u64`/`f64` and your
`interface`/`class`/`enum` names usable directly. `examples/fib.ts` runs
identically on all four backends; `sum.ts` shows a `u64` loop + arrays;
`tuple.ts` tuples + destructuring; `point.ts` interfaces; `account.ts` a class with methods; `generic.ts`
monomorphized generic functions; `box.ts` generic classes; `cards.ts` an enum +
`for…of`; `calc.ts` a `switch`; `ternary.ts` the `?:` operator; `destructure.ts`
a string enum + array/object destructuring; `defaults.ts` default parameters;
`optional.ts` nullable references (`T | null`, `??`, narrowing); `chain.ts`
optional chaining; `nullable_heap.ts` `str | null` + `T[] | null`;
`nullable_scalar.ts` `i64 | null`; `lambda.ts` first-class functions + arrow
lambdas; `closure.ts` closures (capture); `arraymethods.ts` + `arraymethods2.ts`
the array method stdlib; `arraysearch.ts` the search/in-place methods running
native; `strings.ts` the native string-method stdlib; `calculator.ts` a
recursive-descent arithmetic evaluator (classes + recursion + `charCodeAt`,
running natively); `analytics.ts` a data pipeline (growable arrays + closures +
`map`/`filter`/`reduce` chaining); `sort.ts` in-place insertion sort + recursive
binary search (a native imperative algorithm). Most run on all four backends;
**nullable
*heap* types — structs, `str`, arrays — run natively on `--jit` and `--llvm`**
(a null is a 0/null pointer; `=== null` is a pointer compare). **Nullable
*scalars* (`i64 | null`) run on the interpreter** — scalars have no spare null
value, so the native tiers fall back (they'd need boxing). **First-class
functions and closures run natively on `--jit` and `--llvm`** via an
env-pointer calling convention: a function value is a `{code, env}` block, a
capturing closure stashes its captures in a small env struct that the lifted
function reads back, and each indirect call dispatches on the env slot (a bare
function and a closure can even reach the same call site). A closure captures
**by value** (snapshotting the variable at creation; reassigning it afterward
doesn't change the closure); `--wasm` still falls back (no heap). **Growable
arrays (`push`/`pop`) run natively on `--jit` and `--llvm`** too: an array is a
stable Vec-style header `[len | cap | data]` plus a separate data buffer, so
`push` reallocs the buffer (cap ×2) without moving the handle — aliases see the
append. **The closure-based array methods** (`map`/`filter`/`reduce`/`some`/
`every`/`findIndex`/`slice`/`concat`, which lower to synthesized per-element-
type loop helpers built on `push` + indirect closure calls) and the pure ones
(`indexOf`/`includes`/`reverse`/`fill`) **all run native** on `--jit`/`--llvm`
now that both closures and growable arrays do. The only feature still on the
interpreter is **nullable *scalars*** (`i64 | null` — they'd need boxing);
everything else compiles to native code on both backends. **String methods run
natively too** — each is a `zipp_str_*`
runtime call shared by the interpreter and both native backends. They are
**byte-level (UTF-8)**: `len`, indices, and `charCodeAt` are byte offsets,
ASCII-exact vs TypeScript; on non-ASCII, indices are byte (not UTF-16-code-unit)
positions and `charCodeAt` returns a byte. Index accessors are total —
`charCodeAt(out-of-range)` is `-1` (TS `NaN`), `charAt(out-of-range)` is `""`,
`slice` clamps — while `repeat(negative)` is a runtime error. `--wasm` falls back
for all nullable and for strings (its contract profile stays scalar-only).

**Editor + `tsc` support:** the repo ships a `zipp.d.ts` declaring the
`i64`/`u32`/… types, the cast functions, the math builtins, `print`/`console`,
and `len`. Point a `tsconfig.json` at it (`"include": ["zipp.d.ts", "**/*.ts"]`,
`"lib": ["ES2020"]`, `"types": []`) and `tsc --noEmit` checks your ZIPP programs
— autocomplete, arity, return types, undefined names, interface fields. (The
numeric types alias to `number` so arithmetic still checks; ZIPP's own checker
enforces width/signedness.) The bundled `tsconfig.json` type-checks `examples/`
clean: `npx -y -p typescript tsc --noEmit -p tsconfig.json`.

The `zipp run` pipeline above is the **AssemblyScript model** — a typed subset
compiled to fast/provable code. Its AOT speed, gas-metered determinism, and
zk-provability depend on static types and no dynamic dispatch, so *arbitrary*
dynamic JS stays out of that lane by design.

Dynamic JS lives in a **separate engine** (`zipp js`, the `zipp-vm` crate) —
described below.

## Dynamic JavaScript engine (`zipp js`)

`zipp js file.js` runs ordinary dynamic JavaScript, like `node file.js`. It is a
clean-sheet engine — a NaN-boxed value model over an **explicit-frame register
VM** (recursion lives in an explicit frame stack, not the native one, so deep
recursion throws a catchable `RangeError` instead of segfaulting) with a native
**x86-64 OSR JIT** (int64 hot loops, inline caches + object scalar-replacement,
native callbacks). Coverage includes closures, classes (incl. `extends`,
get/set, private `#fields`), `for-of`/`for-in`, destructuring, spread/rest,
generators, `async`/`await` + Promises (`all`/`race`/`allSettled`/`any`),
`Map`/`Set`, tagged templates, `Symbol.iterator` iterables, `Date`, and the
common Array/String/Object/Math/JSON/Number builtins.

**Performance vs V8 (Node 24), compute-only, large-N, best-of-9** (every output
byte-identical to node):

| workload | V8 | zipp | ratio |
|---|---|---|---|
| 2M array `map` | 23.8 ms | 5.8 ms | **0.24× (beats V8)** |
| 2M array `reduce` | 21.4 ms | 6.0 ms | **0.28× (beats V8)** |
| 2M array `map→filter→reduce` | 39.9 ms | 17.5 ms | **0.44× (2.3× faster)** |
| 2M array `filter` | 18.2 ms | 10.1 ms | **0.55× (beats V8)** |
| 2M `arr.push(i)` loop | 18.5 ms | 10.5 ms | **0.57× (beats V8)** |
| 100k comparator sort | 10.2 ms | 7.4 ms | **0.73× (beats V8)** |
| 50M integer loop | 27.4 ms | 26.4 ms | **0.97× (beats V8)** |
| 200k `charCodeAt` scan loop | 2.2 ms | 2.9 ms | 1.28× (sub-3ms) |
| 200k `s[i]===c` scan loop | 2.3 ms | 2.9 ms | 1.27× (sub-3ms) |
| 4M object field read/write | 5.1 ms | 5.1 ms | **1.01× (parity)** |
| 100k string concat + scan | 6.8 ms | 4.6 ms | **0.69× (beats V8)** |
| 1M string concat (`s += …`) | 27.0 ms | 28.6 ms | **1.06× (parity)** |
| fib(37) recursion | 141.5 ms | 129.9 ms | **0.92× (beats V8)** |

zipp **beats V8 across the whole array `map`/`filter`/`reduce` pipeline** (~2×
faster end-to-end): each is compiled to a *fused native kernel* — a tight loop
that inlines the callback body per element with no per-element call (the same
thing V8's TurboFan does), computing in f64/SSE so it handles both small-int and
double arrays. It also **beats V8 on `push`-heavy loops** (builtin method calls
JIT inside OSR regions), **on the comparator sort** (native-callback comparator +
O(n log n) merge sort), and now **beats V8 on the integer loop** (native int64 OSR
JIT; integer `%` also compiles in-region via `idiv`, so `i % k` loops JIT too).
**String scans now JIT too:** both `s.charCodeAt(i)===n` and `s[i]==="c"` compile
in the OSR region (the region's `===` is polymorphic — numeric operands compare as
f64, interned single-char strings compare by NaN-boxed bits), turning a former ~20×
gap into ~1.3× (sub-3ms absolute). **Self-recursion now beats V8:**
a recursive call compiles to a direct native call to the function's own entry (an
inline depth guard bounds the native stack; runaway recursion still deopts to a
catchable `RangeError`). On top of that, a function whose base case returns its
argument unchanged (`fib`: `n<2 ? n`) has that base case **inlined at the call
site**, so the ~half of calls that hit a leaf skip the call/prologue/epilogue
entirely — taking fib(37) from ~2.8× off V8 to **~0.92× (faster than V8)**. **String
concat now JITs:** a hot `s += …` loop used to run fully interpreted (a string `+`
is a heap op outside the numeric OSR region, so the loop paid dispatch every
iteration). A compile pass detects the `s = s + x` accumulator and emits a
`StrConcat` op (semantically identical to `Add`) that routes the loop into the
helper-call OSR region — control flow runs native, the concat is a lean
`jit_concat` helper (the same O(1) cons-string rope `+` builds). That **beats V8
on the realistic concat-and-scan workload (0.69×)** and brings the pure 1M `s += …`
build from ~1.8× to **parity (~1.06×)** — the remaining sliver is V8's
bump-allocated GC edging the 64 B `Cons`-node churn. **Object field access is at
parity (~1.0×):** non-escaping loop objects are scalar-replaced (SROA) so fields
become registers, and a dead-code pass drops the now-unused object-ref loads SROA
leaves behind (which also frees register homes, keeping the loop on the higher-ILP
allocation path). It still trails on the sub-3ms string-scan loops (absolute times
under 3 ms). **Startup is ~10× faster** (≈21 ms vs ≈218 ms — no V8
snapshot/warmup), so end-to-end (incl. startup) zipp finishes every benchmark
first. Run `bench/run.sh` to reproduce.

## License

Apache-2.0
