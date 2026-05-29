# The provable profile (optional zk-STARK)

This extends `../../ZIPP.md` §7. Where §7 describes a **WASM-contract** profile
targeting *existing* chains, the provable profile makes ZIPP execution itself
**zk-STARK-provable** — the model a STARK chain (like the ZIPP chain, today on
FormLogic) actually needs, and the basis for ZIPP becoming that chain's contract
language ("Cairo for ZIPP").

It is **optional**: `cargo build --no-default-features` drops the `zipp-zk` crate
and the Winterfell dependency entirely; the language still compiles and runs.

## How it works

1. The VM (`zippc::vm`) emits a flat execution trace: one `TraceStep` per executed
   instruction, with operand **values** resolved (`{clk, op, a, b, dst, imm}`).
2. `zipp-zk` lays that trace into a power-of-two column matrix and runs an
   application-specific STARK (Winterfell 0.13, Blake3, f128, quadratic extension)
   — the same shape as the chain's `zk-formlogic`, but over ZIPP's VM rather than
   FormLogic bytecode.
3. `zipp run --prove <file>` runs, proves, and **verifies** the proof, reporting
   proof size and timings.

### Trace columns (v0, width 10)

```
0 clk | 1 sel_const | 2 sel_add | 3 sel_sub | 4 sel_mul | 5 sel_other
6 a   | 7 b         | 8 dst     | 9 imm
```

### AIR constraints (v0)

- **clk' − clk − 1 = 0** — monotonic clock (re-clocked to the row index).
- **sel·(sel − 1) = 0** for each selector — booleanity.
- **Σ sel − 1 = 0** — exactly one opcode class per row.
- **sel_const·(dst − imm) = 0**
- **sel_add·(dst − a − b) = 0**
- **sel_sub·(dst − a + b) = 0**
- **sel_mul·(dst − a·b) = 0**
- Boundary: `clk[0] = 0` and `dst[last] = result` (binds the proof to the public output).

## v0 soundness boundary (deliberately honest)

What's proven: the **arithmetic** of the execution (`Const/Add/Sub/Mul`) is
correct and the final value is the claimed public result.

What's **not** proven yet:
- **Control-flow / PC integrity** — that the next instruction is the right one
  (jumps, calls, returns). Control steps are recorded as `Other` and unconstrained.
- **Memory/register consistency** — no permutation argument (this is `zk-formlogic`'s
  multi-segment memory bus; v0 is single-segment).
- **Range agreement** — values are encoded `x as u64` into f128; the proven
  constraints hold for non-negative, non-overflowing arithmetic (true for the
  bundled examples). 64-bit range-check lookups are needed for full integer↔field
  agreement (`zk-formlogic` flags the same gap).

This is the same staged path `zk-formlogic` took to its 78-column trace.

## Hardening roadmap

1. **Add control selectors + PC integrity** (jump/call/return), call-depth column.
2. **Memory-permutation argument** (auxiliary segment + randomized bus) so reads
   return the last written value — mirror `zk-formlogic`'s aux trace.
3. **Range checks** to bind field values to 64-bit (or fixed-point) integers.
4. **Program binding**: commit a hash of the bytecode in public inputs (so a proof
   is tied to a specific program), like `zk-formlogic`'s program-hash columns.
5. **State roots**: boundary-assert before/after state roots to bind a proof to a
   chain state transition — the hook for on-chain use.
6. **GPU**: reuse the chain's `zk-formlogic-cuda` NTT kernels for the prover NTT.

## Why this profile (vs ZIPP.md §7's WASM)

The ZIPP chain's proving is **application-specific** to its VM's bytecode — that's
where its sub-ms GPU proofs come from (vs a general zkWASM/RISC-V VM, ~100× slower).
A WASM-contract profile would orphan that prover. The provable profile keeps ZIPP
on the STARK-native path, so the chain can eventually swap FormLogic for ZIPP
without giving up its zk-STARK differentiator.
