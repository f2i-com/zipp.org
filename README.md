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
- **Builtins**: `len`; `abs`/`min`/`max`/`pow` (integers); `sqrt`/`floor`/`ceil` (floats)
- Lowering to **register-machine bytecode**; functions, recursion, `if` / `while`
- A **VM** that runs it (`zipp run`)
- The **optional zk-STARK profile** (`zipp run --prove`): Winterfell proof +
  verification over the VM execution trace
- An integration **test suite** (`cargo test`)
- **Positioned errors** — parse errors report `line:col`, type errors report the
  statement line (e.g. `type error: arithmetic Add on I64 and Bool [line 2]`)

🚧 Roadmap (see `../ZIPP.md` for the full plan):
- Types: sized integers (`i32`/`u32`/`u64`)
- Runtime-error positions (bytecode → source mapping; compile errors are done)
- Frontend: swap the hand-written parser for **oxc/SWC** (real TS/JSX)
- IR: split into ZHIR + ZMIR (monomorphization, comptime, escape analysis, SoA)
- Backends: **Cranelift** tier-0 JIT, **LLVM** release (+LTO/PGO/SIMD), **WASM-contract**
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

# run the language test suite
cargo test

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
│   └── zipp-cli/     # the `zipp` binary
└── examples/         # add, sum, fib, bits, pi, arrays, hello, fizzbuzz, math
```

The modules inside `zippc` map onto the separate crates in `../ZIPP.md §15`
(zipp-frontend / zipp-checker / zipp-zhir / zipp-zmir / zipp-runtime); they're
kept together for v0 and can be split as each gains weight.

## zk soundness boundary (v0 — honest)

The v0 AIR constrains the **arithmetic** of the trace (`Const`/`Add`/`Sub`/`Mul`),
selector booleanity/exclusivity, a monotonic clock, and a boundary assertion
binding the final value to the public result. Control-flow / memory steps are
recorded but **not yet** constrained (no PC-integrity or memory-permutation
argument; no 64-bit range checks). Proven arithmetic must be non-negative and
non-overflowing (true for the bundled examples). Hardening this is the roadmap —
the same path `zk-formlogic` took to its 78-column trace.

## License

Apache-2.0
