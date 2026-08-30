# Zipp reference

This is the durable technical reference. The public overview and canonical
scoreboard are in [`README.md`](README.md); current experiments are in
[`PERF_ROADMAP.md`](PERF_ROADMAP.md); the previous long-form snapshot is
preserved at [`docs/archive/DOC-through-B252.md`](docs/archive/DOC-through-B252.md).

## Contents

- [Build and workspace](#build-and-workspace)
- [Command-line goals](#command-line-goals)
- [Conformance and language coverage](#conformance-and-language-coverage)
- [Frontend design](#frontend-design)
- [Runtime and object model](#runtime-and-object-model)
- [Native JITs](#native-jits)
- [Garbage collection](#garbage-collection)
- [Embedding](#embedding)
- [Security profiles](#security-profiles)
- [Source layout](#source-layout)
- [Development gate](#development-gate)
- [Performance evidence](#performance-evidence)

## Build and workspace

The ordinary CLI requires stable Rust:

```sh
git clone https://github.com/f2i-com/zipp.org.git zipp
cd zipp
cargo build --release
./target/release/zipp js file.js
```

Release builds use optimization level 3, fat LTO, one codegen unit, and aborting
panics. The final link is intentionally slower than a development build; the
resulting executable has no runtime data-file dependency.

| Path | Role |
|---|---|
| `crates/zipp-vm` | Lexer, parser, compiler, bytecode, interpreter, runtime, GC, and native JITs. |
| `crates/zipp-cli` | Thin `zipp` command-line front end. |
| `crates/regress-fork` | Project-maintained ECMAScript regex engine fork. |
| `crates/zipp-wasm` | Separately resolved wasm32/browser embedding. |
| `crates/zipp-sandbox` | Separately resolved hardened native runner. |

The WASM and hardened runner workspaces are excluded from the root workspace on
purpose. Cargo unifies features within a workspace; including them would combine
the ordinary CLI's JIT with their interpreter-only `safe-sandbox` profile and
weaken the compile-time boundary.

Native code generation is feature-gated. `--no-default-features` builds the
pure interpreter on a native target. wasm32 uses the interpreter; x86-64 and
ARM64 enable their respective native backends where supported.

## Command-line goals

```text
zipp js  file.js    CommonJS-shaped script goal, with ESM-shaped fallback
zipp mjs file.mjs   ES module goal, including top-level await
```

`zipp js` permits top-level `return`, matching the CommonJS-shaped source real
packages expect. `--script-goal` selects the pure ECMAScript Script goal used by
the test262 harness.

The ordinary CLI is for trusted programs. `zipp sandbox` and `zipp js
--sandbox` remain compatibility aliases with resource supervision, but their
executable still contains the native JIT machinery. Use the separately built
`zipp-sandbox` when compile-time JIT/unsafe exclusion matters.

## Conformance and language coverage

The current required test262 result is **95,939 / 95,942 (99.997%)** on the
pinned suite revision. The expected-failure file is authoritative:
[`tools/test262-expected-failures.txt`](tools/test262-expected-failures.txt).

The remaining three executions are:

- one Annex B test that asserts wording removed after ES2017; the paired staging
  test and current specification require the behavior Zipp implements; and
- two `staging/sm/String/internalUsage.js` modes that require German CLDR date
  formatting, while Zipp deliberately ships only the `en` locale today.

Errored module cycles, repeat dynamic import of those cycles, deferred-module
top-level-await ordering, module-source path handling, and cross-realm
`Array.from` / `TypedArray.from` behavior are fixed; older descriptions of those
items are historical.

Zipp implements modern ES2015–ES2025 language and runtime features, including:

- classes, private elements, static blocks, computed keys, and all eight
  decorator kinds;
- destructuring, spread/rest, generators, async generators, promises, iterator
  helpers, and `using` / `await using`;
- `Map`, `Set`, weak collections, weak references, and finalization registries;
- 12 TypedArray kinds including `Float16Array`, `DataView`, resizable and
  transferable buffers, shared memory, and atomics;
- `BigInt`, symbols, proxies, all 13 Reflect traps with invariant checks, and a
  real `console` global;
- modern RegExp syntax and behavior: named groups, lookbehind, `/d` indices,
  and `/v` Unicode sets;
- modules with top-level await, dynamic import, import attributes, and
  typed/deferred/source-phase forms;
- `eval`, `Function`, `ShadowRealm`, `Temporal`, and iterator/set proposals used
  by the pinned suite.

Temporal includes fifteen calendars. Twelve are closed-form; Chinese and Dangi
use astronomical calculation, and Umm al-Qura follows the required month data.
The IANA time-zone database is generated from pinned upstream data. Broad
Intl/CLDR locale data is the material remaining platform gap; the project does
not add one-off guessed locale patterns merely to make individual tests green.

The conformance gate is intentionally multi-mode: default JIT,
interpreter-only, forced-JIT, and majors-only-GC runs must produce the same
failure identities. Tier-differential fuzzing additionally compares generated
self-checking programs across Node, the interpreter, and JIT-tier controls.

## Frontend design

`crates/zipp-vm/src/parse` is a hand-written lexer and recursive-descent parser.
It owns its AST and strings, so parsed programs can be cached and shared without
an arena lifetime. The compiler builds a separate binding/scope model; parser
scope state exists only long enough to enforce static semantics and early
errors.

Several choices follow directly from ECMAScript rather than from parser style:

- A call can be represented as an assignment target because Annex B requires
  some sloppy call assignments to parse and fail at runtime.
- Strings use the engine's `StrVal`, preserving lone UTF-16 surrogates.
- Parenthesization is recorded only where it is observable instead of wrapping
  every expression in another node.
- Arrow/object/destructuring cover grammars are parsed as a permissive superset
  once; deferred errors are discharged when the grammar resolves.
- The lexer receives whether a regular expression is grammatically allowed,
  avoiding an unreliable “previous token” guess for `/`.
- Template scanning yields to the parser at `${` and resumes at the matching
  `}`, which naturally handles nested templates.

The frontend was built primarily for correct early errors. Binding collisions,
strict directive prologues, Annex B declaration positions, private-name
environments, parameter uniqueness, and class property-name rules depend on
context available while parsing and cannot be reconstructed reliably from a
context-free tree afterward.

## Runtime and object model

Values are NaN-boxed into 64 bits. Heap values carry stable slot indices; the
heap stores object payloads separately and maintains parallel generation and
version data used by guards, garbage collection, and inline caches.

Objects use shared shapes for stable layouts and dictionary behavior for
layouts that no longer benefit from shape sharing. Per-call-site caches guard
receiver identity or exact shape plus live versions and prototype-hop versions.
Every optimized property route has a precise slow fallback for accessors,
proxies, exotic objects, custom prototypes, mutation, and cross-realm cases.

The bytecode VM uses explicit register frames. Calls, exceptions, async jobs,
modules, realm state, and host hooks are runtime structures rather than Rust
stack recursion. This is also why native deoptimization can flush register
homes and resume the interpreter at an exact bytecode instruction.

## Native JITs

### x86-64

Hot loop backedges can enter OSR regions; hot functions can enter a
whole-function tier. A loop is offered to progressively broader plans:

| Tier | Representation | Typical work |
|---|---|---|
| SROA | promoted scalars | narrow local aggregate shapes |
| INT / INT-GPR | unboxed integers | arithmetic, comparisons, pinned arrays/strings, selected inlined calls |
| REGALLOC / DOUBLE | unboxed doubles in register homes | fractional numeric loops and typed access |
| MEM | boxed `Value`s | broad bytecode coverage with guarded inline caches and helpers |

Plans can pin immutable or version-guarded receiver facts at entry. Each access
rechecks the identity, version, kind, length, or bounds that licenses its direct
load. A failed proof takes the generic route or a precise side exit; it does not
turn a speculative assumption into JavaScript semantics.

Integer homes must preserve the exact-number boundary and cannot encode `-0`.
Operations that can leave the exact ±2^53 range, introduce negative zero, or
observe a different live intrinsic bail to the interpreter. Side exits flush
every live home, identify the exact resume instruction, and contribute to a
bounded deopt budget; repeatedly unstable regions are evicted.

Useful diagnostics include:

```text
ZIPP_JITLOG=1       tier decisions, deopts, evictions, and mechanisms
ZIPP_JITDECLINE=1   the planner check that rejected a region
ZIPP_PROF=1         runtime phase sampling
ZIPP_PROF_PC=1      Windows x86-64 emitted/native PC attribution
ZIPP_NOJIT=1        interpreter-only (presence checked; unset for JIT)
```

Feature-ablation environment probes are cached outside hot paths; calling
`std::env` in a per-operation path is itself large enough to invalidate a
microbenchmark.

### ARM64

The guarded ARM64 baseline is intentionally smaller: bounded call-free integer
functions and numeric loops, with exact-ip fallback on type mismatch, overflow,
or unsupported bytecode. It has no helper calls or native metering in this
tier. Attaching instrumentation disables native execution. Executable bytes,
allocation count, repeated bailouts, and cache lifetime are bounded.

Linux, Windows, and macOS ARM64 workflows execute native mechanism tests. A
cross-compile alone proves only that code builds, not that emitted instructions
run correctly.

## Garbage collection

The heap uses stable indices, a non-moving generational nursery, and mark-sweep
old space. Minor collections trace young reachability plus remembered
old-to-young edges; major collections trace the complete graph. Allocation,
promotion, recycled shells, write barriers, weak edges, finalizers, and external
payload accounting share the same stable slot identity.

The design basis and edge inventory remain in
[`NURSERY_DESIGN.md`](NURSERY_DESIGN.md). That document began as a proposal but
its non-moving index-state nursery is now implemented; historical estimates in
it are not current benchmark claims.

Useful controls:

```text
ZIPP_NO_NURSERY=1          majors-only collector
ZIPP_GC_STRESS=1           collect on every allocation
ZIPP_NURSERY_VERIFY=1      compare minor liveness with a full mark
ZIPP_NURSERY_YOUNG_BUDGET  pin the young allocation budget
ZIPP_NO_NURSERY_ADAPT=1    disable adaptive budget sizing
```

## Embedding

`embed::ScriptState` keeps one compiled VM alive across host calls. It can
evaluate in the live global context, call functions, and address top-level
bindings by stable slot index.

Values cross the boundary as owned `HostValue` trees, never as VM-local
`Value`s. A `Value` contains a heap slot meaningful only inside its originating
live VM. Arrays and plain objects can be marshalled structurally; functions,
classes, maps, dates, proxies, cycles, and other non-data shapes cross as
`Opaque`. Writing a structural object back declines to overwrite opaque slots
or properties, preventing a host read/modify/write cycle from deleting methods
it could not represent.

Prefer `call_slot` in repeated host loops. `call_global` resolves by compiling a
name expression and is suitable for occasional lookup, but dynamic function
definitions retain stable VM-lifetime addresses for JIT references; compiling a
fresh lookup at display-frame frequency is unnecessary lifetime growth.

`crates/zipp-wasm` exposes this API through wasm-bindgen and installs host
bridges. wasm32 has no usable `std::time::Instant` implementation, so
`vm/clock.rs` accepts host clock hooks; native targets re-export standard time.
Untrusted browser runs need one dedicated Worker/WASM instance per tenant and a
host-enforced wall deadline. Destroying the Worker is the complete lifetime and
memory reclamation boundary.

## Security profiles

The ordinary CLI favors throughput and contains unsafe native code generation.
It is not the sole boundary for arbitrary hostile code.

The separately resolved native sandbox disables default VM features, enables
`safe-sandbox`, forbids unsafe engine/regex code at compile time, uses the secure
allocator profile, meters instructions and heap growth, caps output and runtime
compilation, confines opt-in imports, and supervises the child with a hard wall
deadline. It still runs as the invoking OS user.

The WASM profile additionally relies on WebAssembly memory isolation and a hard
linear-memory maximum, but wall time remains a host responsibility. Full threat
models, limits, import caveats, terminal sanitization, and deployment recipes
are in [`SECURITY.md`](SECURITY.md) and the two runner READMEs.

## Source layout

```text
crates/zipp-vm/src/
  parse/            source -> owned AST
  compile/          AST -> register bytecode
  bytecode.rs       instruction definitions
  value.rs          NaN-boxed Value
  heap.rs           object storage, shapes, generations, GC substrate
  vm/               interpreter and runtime services
  codegen/          x86-64 plans and emission
  codegen_aarch64.rs
  embed.rs          persistent host API

crates/zipp-cli/    ordinary command-line executable
crates/zipp-wasm/   browser/Worker embedding workspace
crates/zipp-sandbox hardened native workspace
tools/              conformance, PGO, benchmark, fuzz, and maintenance tools
bench/              scored, hostile, research, and training workloads
```

Large Rust modules are split with `tools/split_rs.py`; its output must
concatenate byte-identically to the original. `tools/remap_anchors.py` can
translate historical `file.rs:line` references after a split.

## Development gate

Minimum release checks for engine work:

```sh
cargo build --release
cargo test --workspace --release
cargo check -p zipp-vm --no-default-features
cargo check -p zipp-vm --no-default-features --features safe-sandbox
```

Changes to the separately resolved workspaces need their own locked release
tests. JIT, deopt, object-model, property, realm, or GC changes need targeted
Node parity cases, forced-tier/GC modes, and the tier-differential fuzz slice.

The full conformance gate runs `tools/run_test262.py` against a freshly rebuilt
release CLI, then compares the sorted failures with the checked-in expected
list. On Windows set `PYTHONUTF8=1`; otherwise one non-ASCII failure can abort
console decoding after the run has done its work.

```text
default
ZIPP_NOJIT=1
ZIPP_JIT_THRESHOLD=1
ZIPP_NO_NURSERY=1
```

All four must return the same expected identities. The forced-JIT pass matters:
most test262 programs are short and do not naturally cross the ordinary hotness
threshold, so default conformance alone cannot certify native helper paths.

Benchmark changes follow [`bench/README.md`](bench/README.md). Routine outputs
belong under ignored `target/bench-results/`; public claims require a clean,
complete, provenance-stamped capture.

## Performance evidence

The canonical engine table lives in the root README. Its current clean PGO
inputs are `bench/real13_21288c1_pgo_2026-08-30.json` and
`bench/hostile/head_clean_21288c1_pgo_2026-08-30.json`; both are complete
four-engine captures with `publishable:true` and exact output throughout.

At engine commit `21288c1`, Zipp's paired cold geomean is **0.642× Node** across
the normal 13 and **0.881× Node** across the hostile 17. The explicitly derived
equal-row all-30 view is **0.768× Node** [0.764, 0.771]. That aggregate does not
erase suite ownership: its point is
`exp((13 × ln(G13) + 17 × ln(G17)) / 30)`, and its descriptive bootstrap treats
the two separately captured suites as independent strata. Thirteen Node point
gaps remain, so it is not an every-row superiority claim.

Current optimization evidence and next work live in `PERF_ROADMAP.md`. Detailed
B001–B252 measurements and refutations live in the archive.

The distinction is deliberate:

- a same-binary off-switch A/B attributes a mechanism;
- a frozen old/new binary A/B checks final layout and generalisation;
- a filtered Node run prices the remaining row gap; and
- only a clean full PGO Node/Bun/Deno/Zipp capture updates public ratios.

Always report comparator identity, binary hash, commit/dirty state, switches,
repetitions and order, exact correctness, ratio and interval, and any unavailable
analysis. Keep neutral and negative results; they are part of the engineering
record, not failed documentation.
