# Execution profiles

## Choose the right execution profile

| Input | Use | Boundary |
|---|---|---|
| Trusted programs and benchmarks | `zipp js` / `zipp mjs` | Maximum-throughput native CLI with JITs enabled. |
| Arbitrary browser-hosted code | [`zipp-wasm`](../crates/zipp-wasm/README.md) in a dedicated Worker | Interpreter-only `safe-sandbox` build; terminate and replace the Worker at the wall deadline and between tenants. |
| Hardened native execution | [`zipp-sandbox`](../crates/zipp-sandbox/README.md) | Separately resolved, no-JIT, unsafe-forbidden engine with instruction, heap, output, import, and wall-time limits. |

Build the hardened native runner separately so Cargo cannot unify its safety
features with the ordinary JIT workspace:

```sh
cargo build --locked --release --manifest-path crates/zipp-sandbox/Cargo.toml
./crates/zipp-sandbox/target/release/zipp-sandbox script.js
```

Imports are denied unless the host supplies one canonical root. The native
runner is language/process/resource containment, not a kernel sandbox; use a
restricted account, container, or OS sandbox when the threat model requires
one. See [`SECURITY.md`](../SECURITY.md) for the full deployment checklist.

### Semantics switches

<details>
<summary><strong>Call evaluation order and diagnostic switches</strong></summary>

Every shipping profile evaluates a method call's reference before its
arguments, as EvaluateCall requires: `receiver.m(input.value)` runs the
getter on `m` before the getter on `value`, an argument's coercion cannot
replace the method already fetched, and when both sides throw the method's
exception is the one seen. The fused `CallMethod` lowering — the op the method
inline caches, intrinsic arms and inlining key on — is used only for
arguments that provably cannot observe the order (literals, register-resident
locals, arithmetic over literals, array/object/closure literals of such
parts); every other argument shape takes the captured `GetProp` +
`CallWithThis` path. That path is not the slow one: the interpreter serves
a captured boot intrinsic (`arr.push(i % 13)`, `s.charCodeAt(a[i])`,
`m.get(k + 1)`) through the same inline and name-dispatched builtin lanes as
the fused form once the captured value is proven identical to the live
prototype intrinsic, and answers the read itself from the same proof; a
captured value that differs — an own shadow, an override installed before or
by the arguments, a subclass method — is invoked exactly as captured. Two
environment variables exist for diagnostics and benchmarking, read once per
process:

| Variable | Effect |
|---|---|
| `ZIPP_RELAXED_CALL_ORDER=1` | Re-admits the pre-audit "primitive-operand" class (global, cell and property reads and arithmetic over them) to the fused lowering. Faster on some rows; observably wrong for a getter or proxy trap on either side of the call. Never a shipping profile. |
| `ZIPP_STRICT_CALL_ORDER=1` | Forces the default, and wins when both are set. |

`crates/zipp-vm/tests/call_order_default.rs` runs the audit's probes under the
default, the interpreter, forced JIT, GC stress and both switches in clean
child processes.

</details>
