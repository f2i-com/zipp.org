# ZIPP language

A sound-TypeScript-subset language, AOT-compiled, in Rust. The full design lives
in [`../ZIPP.md`](../ZIPP.md); this repo is the standalone implementation.

ZIPP has **three execution profiles** from one frontend + IR:

| Profile | Target | Optimized for | Competes with |
|---|---|---|---|
| **native / dApp** | native + WASM (Cranelift/LLVM) | raw speed | V8 |
| **contract** | deterministic gas-metered WASM | determinism, code size | the EVM |
| **provable** *(optional)* | zk-STARK over a register-VM trace | verifiable execution | Cairo / a STARK VM |

The **provable** profile is the new third lane (this commit). It's `--prove`: run
a program, then produce *and verify* a zk-STARK proof that the execution was
computed correctly — the same application-specific-STARK approach the ZIPP chain
uses for FormLogic, but over ZIPP's own VM. That makes ZIPP a candidate successor
to FormLogic as the chain's contract language (a "Cairo for ZIPP"), while the
native profile targets off-chain dApp/tooling speed. zk is **optional**: build
with `--no-default-features` and the language still runs.

## Status (v0 — a working vertical slice, not the finished language)

✅ Working end-to-end today:
- Lexer → recursive-descent parser → **sound-subset type checker** (`i64`/`f64`/`bool`/`str`,
  no implicit coercions, arity/return checking)
- **`f64` floating-point** with `i64()` / `f64()` casts (the zk profile stays
  integer-only — `--prove` rejects f64, per PLAN.md §7)
- **Lexical block scoping with shadowing**; `while` / **`for`** loops,
  `break` / `continue`; **short-circuit** `&&` / `||`
- Full operator set incl. **bitwise/shift** `& | ^ << >> ~`
- **Arrays**: literals `[a, b, c]`, repeat `[v; n]`, indexing read/write, `len()`,
  runtime bounds checks (reference types; also `--prove`-gated for now)
- **Strings**: literals with escapes, `+` concat, `==`/`!=`, `len()`, `print`
  (heap-backed, immutable; `--prove`-gated)
- **Structs**: `struct Point { x: i64, y: i64 }`, construction, field read/write,
  nesting (heap-backed reference types; `--prove`-gated)
- **Builtins**: `len`; `abs`/`min`/`max`/`pow` (integers); `sqrt`/`floor`/`ceil` (floats)
- Lowering to **register-machine bytecode**; functions, recursion, `if` / `while`
- A **VM** that runs it (`zipp run`)
- The **optional zk-STARK profile** (`zipp run --prove`): Winterfell proof +
  verification over the VM execution trace
- A **native JIT** (`zipp run --jit`, Cranelift): compiles the **entire
  language** — scalars (`i64`/`f64`, casts), arrays, strings, structs, math
  builtins — to machine code; nothing falls back. A fast-compile tier-0 that
  beats V8 on integer loops (see Performance below)
- An **LLVM release tier** (`zipp run --llvm`): emits LLVM IR and compiles it
  with `clang -O3 -march=native` — same coverage, **matches V8 on dense f64**
  and **beats V8 and Bun on dense-f64 arrays** (matmul). No `llvm-sys` linkage;
  it shells out to `clang`.
- An integration **test suite** (`cargo test`)
- **Positioned errors** — parse errors report `line:col`, type errors report the
  statement line (e.g. `type error: arithmetic Add on I64 and Bool [line 2]`)

🚧 Roadmap (see `../ZIPP.md` for the full plan):
- Types: sized integers (`i32`/`u32`/`u64`)
- Runtime-error positions (bytecode → source mapping; compile errors are done)
- Frontend: swap the hand-written parser for **oxc/SWC** (real TS/JSX)
- IR: split into ZHIR + ZMIR (monomorphization, comptime, escape analysis, SoA)
- Backends: **Cranelift** tier-0 JIT and an **LLVM** release tier (`clang -O3`)
  — *scalars + arrays + strings + structs done* (matches V8 on dense f64; beats
  V8/Bun on matmul); the **whole language** compiles natively now — next: a GC
  (heap currently leaks), LTO/PGO, and a **WASM-contract** target
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
│   ├── zipp-jit/     # OPTIONAL native backend (Cranelift JIT, scalar subset, tier-0)
│   ├── zipp-llvm/    # OPTIONAL release tier (emit LLVM IR, compile with clang -O3)
│   └── zipp-cli/     # the `zipp` binary
└── examples/         # add, sum, fib, bits, pi, arrays, hello, fizzbuzz, math, structs
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

The whole language now compiles natively; next is a GC (the array/string/struct
heap currently leaks). One kernel isn't the whole story (PLAN.md §6/§11 —
workloads differ, and V8's hand-tuned stdlib is a separate battle), but this is
real, reproducible evidence the thesis holds where the design predicts.

## License

Apache-2.0
