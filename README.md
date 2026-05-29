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

Two kernels, vs Node 24 (V8). ZIPP `--jit` times include compilation; all
engines compute the identical result on each kernel (so the comparison is
apples-to-apples). Both kernels are in `bench/` with byte-identical `.js` twins.

**Integer** — 50M-iteration sum loop (`bench/loop.zipp`):

| engine | time | vs V8 |
|---|---|---|
| ZIPP interpreter | ~566 ms | ~19× slower |
| Node 24 (V8 JIT) | ~30 ms | 1× |
| **ZIPP `--jit` (Cranelift, native)** | **~10 ms** | **~3× faster** |

**Dense f64** — 1000×1000 Mandelbrot, 256-iter cap (`bench/mandelbrot.zipp`):

| engine | time | vs V8 |
|---|---|---|
| Node 24 (V8 JIT) | ~100 ms | 1× |
| **ZIPP `--jit` (Cranelift, native)** | **~127 ms** | **~1.27× slower** |

The story these two tell is the honest one. The **native JIT** (`--jit`,
PLAN.md tier-0) compiles the scalar subset (`i64` + `f64`, casts, arithmetic,
control flow, functions, `print`) to machine code; arrays / strings / structs
still fall back to the interpreter.

- On the **integer** loop there's nothing for an optimizing compiler to do, so
  ZIPP's AOT-no-deopt-guards code **beats V8 ~3×** — the §6 sweet spot.
- On **dense f64**, V8's optimizing tier (TurboFan — better register
  allocation, and almost certainly FMA contraction of the `a*b + c` terms)
  pulls **~30% ahead**. Cranelift is a *baseline* compiler (even at
  `opt_level="speed"`, which is enabled here); it does no FMA contraction
  (float semantics) and lighter scheduling. So ZIPP is "on par" — same
  ballpark, within ~1.3× — but not yet ahead on this class.

Closing the dense-f64 gap is the planned **LLVM release tier** (Phases 8–9:
`-O3` + LTO/PGO/SIMD), which is the engine meant to win this race. Per-kernel
f64 also already crushes the interpreter — the Leibniz-π loop (`examples/pi.zipp`)
is ~1.7 ms native vs ~29 ms interpreted (~17×). Next on the JIT itself: heap
types via a runtime (unlocks the array benchmarks — n-body, spectral-norm).

One kernel isn't the whole story (PLAN.md §6/§11 — different workloads favour
different engines, and V8's hand-tuned stdlib is a separate battle), but it's
real evidence the thesis holds where the design predicts it should.

## License

Apache-2.0
