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
- A **native JIT** (`zipp run --jit`, Cranelift): compiles the scalar subset
  (`i64` + `f64`, incl. casts) to machine code — on a tight loop it beats V8
  (see Performance below)
- An integration **test suite** (`cargo test`)
- **Positioned errors** — parse errors report `line:col`, type errors report the
  statement line (e.g. `type error: arithmetic Add on I64 and Bool [line 2]`)

🚧 Roadmap (see `../ZIPP.md` for the full plan):
- Types: sized integers (`i32`/`u32`/`u64`)
- Runtime-error positions (bytecode → source mapping; compile errors are done)
- Frontend: swap the hand-written parser for **oxc/SWC** (real TS/JSX)
- IR: split into ZHIR + ZMIR (monomorphization, comptime, escape analysis, SoA)
- Backends: **Cranelift** tier-0 JIT — *scalar subset (`i64` + `f64`, casts) done*;
  extend to heap types (arrays/strings/structs via a runtime), then **LLVM**
  release (+LTO/PGO/SIMD) and **WASM-contract**
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
│   ├── zipp-jit/     # OPTIONAL native backend (Cranelift JIT for the integer subset)
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

Two kernels, vs Node 24 (V8). All engines compute the identical result on each
kernel (the comparison is apples-to-apples); kernels are in `bench/` with
`.js` twins. The JIT reports **compile and execute separately** — the headline
"as fast as" question is about the *generated code* (execute), and JIT compile
is a one-shot ~1 ms either way.

**Integer** — 50M-iteration sum loop (`bench/loop.zipp`):

| engine | execute | vs V8 |
|---|---|---|
| ZIPP interpreter | ~566 ms | ~19× slower |
| Node 24 (V8 JIT) | ~30 ms | 1× |
| **ZIPP `--jit` (Cranelift, native)** | **~10 ms** | **~3× faster** |

**Dense f64** — 1000×1000 Mandelbrot, 256-iter cap (`bench/mandelbrot.zipp`):

| engine | execute | vs V8 |
|---|---|---|
| Node 24 (V8 JIT) | ~100 ms | 1× |
| ZIPP `--jit` (Cranelift, strict IEEE) | ~127 ms | ~1.27× slower |
| **ZIPP `--jit --ffast-math`, reassociated** (`bench/mandelbrot_fma.zipp`) | **~109 ms** | **~1.07× slower** |

The **native JIT** (`--jit`, PLAN.md tier-0) compiles the scalar subset (`i64` +
`f64`, casts, arithmetic, control flow, functions, `print`) to machine code;
arrays / strings / structs still fall back to the interpreter.

- On the **integer** loop there's nothing for an optimizing compiler to do, so
  ZIPP's AOT-no-deopt-guards code **beats V8 ~3×** — the §6 sweet spot.
- On **dense f64**, V8's optimizing tier (TurboFan) is ~27% ahead of strict
  Cranelift. We chased that gap empirically (see below): it's a *latency-bound
  recurrence* + FMA story, and reassociated `--ffast-math` closes it to ~7%.

### Why dense f64 trails — and what closes it

The Mandelbrot inner loop is a dependency chain: each iteration's `x,y` depend
on the last, so wall-clock is set by the **critical path**, not throughput.

1. **Compile time is negligible** (~1 ms) — the gap is the generated code.
2. **FMA contraction alone doesn't help.** `--ffast-math` fuses `a*b ± c` into
   one rounded multiply-add (CPU `vfmadd`). It fires (3 pairs in Mandelbrot)
   but naive left-to-right fusion only shortens the `y` update; the *binding*
   path is the `x` update `x*x - y*y + cx`, which is unchanged → no speedup.
3. **Reassociation is the lever.** Writing the `x` update as `(cx - y*y) + x*x`
   lets the fuser build a *nested* FMA `fma(x, x, fma(-y, y, cx))`, shortening
   that path. Result: ~109 ms — within ~7% of V8. This is exactly the transform
   LLVM does automatically at `-O3 -ffast-math`; Cranelift (a fast-compile
   *baseline* JIT) does not auto-reassociate, so today it's opt-in via source.

So the knobs *inside* Cranelift are now set — `opt_level="speed"`, host-ISA
features (AVX/FMA via `cranelift-native`), verifier off, and opt-in FMA
(`--ffast-math`). To reliably **match or beat** V8 on dense f64 without
hand-reassociated source, the plan is the **LLVM release tier** (Phases 8–9:
`-O3`, auto FMA + reassociation + auto-vectorization + scheduling). Cranelift
stays as the fast-startup tier — where ZIPP already wins. Per-kernel f64 also
already crushes the interpreter: the Leibniz-π loop (`examples/pi.zipp`) is
~0.8 ms native (execute) vs ~29 ms interpreted (~36×).

Next on the JIT itself: heap types via a runtime (unlocks array benchmarks —
n-body, spectral-norm). One kernel isn't the whole story (PLAN.md §6/§11 —
different workloads favour different engines, and V8's hand-tuned stdlib is a
separate battle), but this is real, reproducible evidence of where the design
stands.

`--ffast-math` is **opt-in** and `--jit`-only: FMA/reassociation change float
rounding (last-bit), so the strict default keeps the JIT bit-identical to the
interpreter — which matters for the deterministic contract/provable profiles.

## License

Apache-2.0
