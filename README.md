# ZIPP language

A sound-TypeScript-subset language, AOT-compiled, in Rust — you can write it in
**real TypeScript** (`.ts`, parsed by `oxc`) or ZIPP's own syntax. The full
design lives in [`../ZIPP.md`](../ZIPP.md); this repo is the standalone
implementation.

ZIPP has **three execution profiles** from one frontend + IR:

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

ZIPP has **three native paths**: the Cranelift **`--jit`** (PLAN.md tier-0 — a
fast-*compile* baseline) and the **`--llvm`** release tier (`clang -O3`),
plus the interpreter fallback (now only used when you don't pass `--jit`/`--llvm`).
Both native backends cover the **entire language** — scalars (`i64` + `f64`,
casts), 1-D arrays, strings, structs, and math builtins.

- **`--llvm` matches V8 on dense f64** (~99 ms vs ~101 ms) and **beats V8/Bun on
  dense-f64 arrays** (matmul). `-O3` auto-FMAs, schedules, reassociates and
  vectorizes; on this CPU it lands level-with or ahead-of TurboFan.
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
`map`/`filter`/`reduce` chaining). Most run on all four backends;
**nullable
*heap* types — structs, `str`, arrays — run natively on `--jit` and `--llvm`**
(a null is a 0/null pointer; `=== null` is a pointer compare). **Nullable
*scalars* (`i64 | null`) run on the interpreter** — scalars have no spare null
value, so the native tiers fall back (they'd need boxing). **First-class
functions, closures, and growable/closure array methods also run on the
interpreter** in v0 (`--jit`/`--llvm`/`--wasm` fall back) — a closure captures
**by value** (snapshotting the variable at creation; reassigning it afterward
doesn't change the closure), and the higher-order array methods lower to
synthesized per-element-type loop helpers. The **pure array methods**
(`indexOf`/`includes`/`reverse`/`fill` — no closure, no `push`) stay **native** on
`--jit`/`--llvm`. **String methods run natively too** — each is a `zipp_str_*`
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

This is the **AssemblyScript model** — a typed subset compiled to fast/provable
code — *not* a JavaScript engine. Running *arbitrary* dynamic JS (`any`,
prototypes, `eval`, exceptions, async) is an explicit non-goal: it would be
slower than V8 (you can't out-V8 V8 by reimplementing it) and would forfeit
ZIPP's AOT speed, gas-metered determinism, and zk-provability — all of which
depend on static types and no dynamic dispatch. Coverage grows toward more
idiomatic typed TS (classes, generics via monomorphization); the dynamic core
stays out by design.

## License

Apache-2.0
