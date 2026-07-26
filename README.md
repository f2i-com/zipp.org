# zipp

A clean-sheet JavaScript engine written in Rust — a NaN-boxed, explicit-frame
register VM with per-call-site inline caches and a native x86-64 OSR JIT.

```sh
cargo build --release
./target/release/zipp js file.js
./target/release/zipp mjs file.mjs      # ES module entry (top-level await)
```

The project is `crates/zipp-vm`. `zipp-cli` is a thin front end over it,
`crates/zipp-wasm` embeds it in WebAssembly for browser hosts, and
`crates/regress-fork` is the ECMAScript regex engine — our fork of regress
0.11.1, which adds an API the engine needs plus three test262 correctness
fixes (see its `FORK.md`).

The JIT is x86-64 only and feature-gated; every other target builds a pure
interpreter (`--no-default-features` does the same on x86-64). aarch64 and
wasm32 are built and tested.

> The workspace used to carry a separate ahead-of-time language (`zippc`, plus
> Cranelift/LLVM/WASM/zk back ends and a TypeScript frontend). That predates the
> engine and the objective moved; those crates have been removed.

## Where it actually stands

Both figures below are measured on this repo, not estimated. Neither is a
finished result — they are the current state.

**Conformance — 97.72% of test262**, 93,835 of 96,029 required executions
(ECMA-262 + `staging`, run in both sloppy and strict mode as
`INTERPRETING.md` requires):

| slice | executions | pass |
|---|---|---|
| ECMA-262 + staging, both modes | 96,029 | 93,835 (97.72%) |
| intl402 (opt-in, `--include-intl402`) | 3,341 | 563 (16.9%) |

The 2,194 remaining failures are 1,265 distinct files (most run in both sloppy
and strict mode). **845 of those are static-semantics early errors the engine
does not raise at all** — `let x; let x;`, `let x; var x;`, duplicate class
constructors and duplicate labels all currently run instead of being a
`SyntaxError`. The cause is that `zipp-vm` pulls `oxc_parser` but not
`oxc_semantic`, which is one of the reasons the engine is growing its own front
end (`src/parse/`): a parser that raises early errors as it goes closes the
single largest category left. The dominant intl402 cause is separate —
`Intl.DateTimeFormat` cannot be constructed at all (`vm/intl.rs:436`).

`tools/test262-expected-failures.txt` is the checked-in baseline, so a
regression is a diff rather than a remembered number.

**Performance — geomean 2.73× slower than node (V8)** on the ten real-world
benchmarks in `bench/real/`, best-of-5, every output byte-identical to node:

| bench | node | zipp | ratio |
|---|---|---|---|
| map-set-heavy | 572ms | 733ms | 1.28× |
| polymorphic-objects | 299ms | 726ms | 2.43× |
| async-promise-chain | 300ms | 733ms | 2.44× |
| json-large | 223ms | 558ms | 2.50× |
| parse-large-js | 232ms | 607ms | 2.62× |
| markdown-render | 240ms | 669ms | 2.79× |
| class-prototype-hot | 258ms | 762ms | 2.95× |
| sparse-array | 47ms | 161ms | 3.43× |
| typedarray-math | 170ms | 676ms | 3.98× |
| regex-log-scan | 419ms | 1738ms | 4.15× |

Startup is ~2× faster than node (26ms vs 53ms — no snapshot to load).

zipp does beat V8 on specific shapes: scalar-numeric kernels, self-recursive
integer functions, `s += …` string accumulation, non-capturing arithmetic array
pipelines, `for-of` over TypedArray elements, and — measured — **regex scanning
that does not match** (a 2000-char no-match `test` runs 25ms against node's
42ms). Those wins do not carry to the benches above, which are dominated by
object construction, property access, and building result objects.

Run `bash bench/run_real.sh` to reproduce; results land in
`bench/results_real.txt`. Run-to-run variance is ±10%, so treat small
differences as noise — `bench/quick.sh` is a 4-bench subset for iteration and
`bench/tiers.sh` reports which JIT tier each loop region reached.

### Why it trails, honestly

This section has been rewritten twice because measurement refuted what it said.
Details and numbers are in `PERF_ROADMAP.md`; the short version:

1. **Object construction, and building result objects.** `new Pt(x,y)` and
   `{a:1,b:2,c:3}` cost tens of nanoseconds against roughly 2ns. An `ObjMap` is
   three separate `Vec`s plus a `String` per key, so a small object is ~6
   allocations. The clearest single instance: `RegExp.prototype.exec` spends
   **316ns attaching `index`/`input`/`groups`** to its result — more than the
   match itself — of which 128ns is one hash insert into the array side table and
   146ns is those three `Vec` allocations. Property *reads* are fine by
   comparison (~2.6ns monomorphic).
2. **The regex matcher is an interpreter.** Matching alone costs 234ns where V8's
   Irregexp — which compiles each pattern to native code — takes 22ns. Note this
   is the opposite conclusion from the "regex scanning" win above: we are faster
   at *not* matching (the memchr prefilter) and 10× slower at matching.
3. **Boxed values in the general JIT tier.** Every non-integer loop round-trips
   values through NaN-boxing with a tag guard per operation. Only an optimizing
   tier that keeps values unboxed across operations removes it.
4. **Regions decline `for-of`.** Not because of the iterator op — that was tried
   and changed nothing — but because `for-of` desugars to a try/finally and the
   region contains the per-iteration `PushHandler`. Compiled code would need
   exception-handler state. Worth knowing before spending on it: for-of accounts
   for only ~7% of the `matchAll` loop it appears in; the regex work is the rest.

**What is NOT the problem**, each ruled out by measurement rather than argument:

- *Property-name interning.* Built to completion behind an `Rc<str>` intern
  table and it is **5-8% slower** — the hash-and-probe costs more than the small
  `malloc` it replaces. Independently confirmed later: of the 316ns above, the
  three `String` keys are only 42ns of it.
- *Inline property storage.* `SmallVec` inline slots make construction 1.4-1.9×
  faster in isolation but regressed the suite, because every `HeapObj` slot grew.
  Slot size turned out to be a first-order lever in its own right — adding 64
  bytes of pure padding to `HeapObj` cost 7.9% — so the fat variants are now
  boxed (`Object(Box<ObjMap>)`, `Combinator(Box<…>)`), 112 → 80 bytes.
- *The regex engine's scanning.* Matching cost is flat in subject length and we
  beat V8 on a pure no-match scan. It is capture-and-build that is slow, not the
  search.

The next concrete steps are the stable paged/slab object arena with hidden
shapes, and an SSA optimizing tier. `PERF_ROADMAP.md` carries the full plan with
per-task `file.rs:line` anchors and — deliberately — the negative results too,
since each cost a day to learn and none is visible from reading the code.

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

## How the JIT is organised

Compilation is triggered by a loop back-edge (OSR) after 8 trips, or
whole-function once a function is hot enough. A loop region is offered to four
tiers in order, and takes the first that accepts it:

| tier | value representation | accepts |
|---|---|---|
| SROA | scalars promoted out of memory | the narrowest shapes |
| INT | raw `i64` in the low half of an xmm home | provably integer loops |
| REGALLOC | `f64` in xmm homes | numeric loops with fractions |
| MEM | boxed `Value`s, per-site inline caches | almost anything else |

The INT tier is the one that beats V8, so most performance work is really about
widening what it will accept. It reaches integer arithmetic and bitwise ops,
`Math.imul`, pinned `Int32Array` elements, flat-ASCII `str.charCodeAt`/`.length`,
and dense all-integer `Array` reads plus `.length`. A "pin" snapshots a
receiver's identity, base pointer and length in the prologue; every access
re-checks identity and bounds, so a wrong or stale pin degrades to the generic
helper rather than to a wrong answer.

Correctness rests on two invariants worth knowing before touching it. Every
add/sub is range-checked against ±2^53 and bails to the interpreter if it leaves
the range where f64 is exact. And an `i64` home cannot represent `-0`, so every
path that could introduce one — `Neg` of zero, an entry load of `-0.0`, which
`ucomisd` reports equal to `+0.0` — must bail instead.

Everything the tier cannot represent becomes a *side exit*: the region flushes
every home back to the register file and resumes the interpreter at an exact ip.
Deopts are counted per region; 64 of them evict the region and it recompiles a
tier down. That counter is the most useful debugging signal in the engine — a
change that makes something slower while still printing the right answer usually
shows up there first.

## Layout

```
crates/zipp-vm/src/
  compile/     AST -> register bytecode (14 modules)
  codegen/     native x86-64 JIT, dynasm (15 modules)
  vm/          the runtime: dispatch, natives, props, construct, temporal, …
  vm/clock.rs  the platform time boundary (see Embedding)
  vm/host_api.rs  structural marshalling + slot-addressed globals
  heap.rs      object model, ObjMap, GC
  value.rs     NaN-boxed Value
  bytecode.rs  instruction set
  embed.rs     the embedding API
crates/zipp-wasm/  wasm-bindgen layer: a persistent VM for browser hosts
```

Oversized files are split into module directories by `tools/split_rs.py`, a
lossless splitter that verifies the emitted pieces concatenate byte-identically
to what they replaced. `tools/remap_anchors.py` rewrites doc `file.rs:N` anchors
after a split.

## Embedding

`zipp js file.js` runs a program and exits. A host that keeps talking to a
script — a UI runtime that renders, waits, then calls a click handler and asks
what changed — needs the VM to outlive the run. `embed::ScriptState` is that:
compile once, then call functions, evaluate in the live global context, and
read or write top-level bindings by slot index.

Two things in it are worth knowing before using it.

**Values cross as data, never as references.** A `Value` is a NaN-boxed heap
*index* whose meaning depends on the live VM, so handing one to a host would be
handing out a dangling reference the moment the collector moves. `HostValue`
is therefore an owned tree — nested arrays and plain objects included — and
anything that cannot be data (functions, classes, `Map`, `Date`, proxies) crosses
as `Opaque`. Writes then *decline* to overwrite an opaque slot, and the same rule
applies one level deeper to object properties: a host that reads an object,
spreads it, and writes it back is echoing the methods it could not see, not
deleting them. Doing this with JSON is not an option — `JSON.stringify` drops
function-valued properties and throws on a cycle, so it cannot express either
rule.

**Prefer `call_slot` to `call_global`.** `call_global` resolves its callee by
compiling the name as a fresh program, and compiled functions are interned for
the VM's lifetime (the JIT holds raw pointers into them). That is fine once and
a leak at 60 Hz. `call_slot` compiles nothing.

`crates/zipp-wasm` is this API over wasm-bindgen, plus a JS preamble supplying
host bridges. Its `README.md` covers the two host channels and why they differ.

### wasm32 has no clock

`Instant::now()` and `SystemTime::now()` on `wasm32-unknown-unknown` are std
stubs that **panic**, and `Vm::new` records a start instant — so an un-shimmed
engine traps on construction, before running a line of JS. `vm/clock.rs` is the
boundary: native targets re-export `std::time` unchanged, and wasm reads clocks
the host installs via `install_clock` (a no-op elsewhere, so an embedder can call
it unconditionally). A hook rather than a `[target.wasm32]` js-sys dependency,
because the engine should not have to assume its wasm host is a browser.

## Development

The standing gate for any engine change — see `PERF_ROADMAP.md` §2:

```sh
cargo build --release
cargo test --release                                   # 270 tests, 0 failed
python tools/run_test262.py --t262 <path> --dump-fails fails.txt
diff <(sort fails.txt) <(sort tools/test262-expected-failures.txt)   # REG=0
ZIPP_NOJIT=1 python tools/run_test262.py …             # and again, interpreter only
bash bench/run_real.sh                                 # ALL_CORRECT=1
```

Anything touching the embedding API or the wasm layer also has to clear the node
harnesses in `crates/zipp-wasm/tests/node/` — `cargo test` covers the Rust side
but not the wasm-bindgen boundary (marshalling, the bridge closures, the host
queue), and that boundary is where the interesting bugs live. One of them drives
a real third-party bundle unmodified.

A change that builds on x86-64 can still break every other target: the `jit`
feature and `target_arch = "x86_64"` gate ~120 sites, so an attribute that drifts
onto the wrong item takes aarch64 and wasm32 down without x86-64 noticing. Cheap
insurance:

```sh
cargo build --target wasm32-unknown-unknown -p zipp-vm --no-default-features
```

`ZIPP_NOJIT=1` disables native codegen (it is **presence**-checked, so
`ZIPP_NOJIT=0` also disables it — unset it for a JIT run); `ZIPP_GC_STRESS=1`
collects on every allocation; `ZIPP_JITLOG=1` reports tier decisions, deopts and
evictions; `ZIPP_JITDECLINE=1` names which planner check rejected a region.

Any change touching the JIT must produce identical output both ways —
`assert_jit_matches` in the test suite pins that per case. A JIT change that
only *appears* correct is the common failure mode here: several bugs found in
this engine returned right answers and were caught by a deopt counter or a
missing speedup, not by a test.

Two habits earned the hard way, both worth keeping:

- **Never put an `std::env::var` probe on a hot path**, not even to instrument an
  ablation. Doing so inflated every variant of one measurement by ~90ns and
  produced a confidently wrong conclusion — twice. Read it once into a
  `OnceLock`.
- **Guard on a value, not a tag.** An intrinsic gated on `is_int()` looked
  correct and made its benchmark faster while quietly causing 150 deopts and two
  region evictions, because an integral value can be double-tagged.

test262 is the real gate for anything touching property semantics; the unit
suite will not catch it. Two examples from this repo: a fast path for ordinary
property writes assumed `%Object.prototype%` carries no accessor for an ordinary
key (a program can install one), and missed that class accessors live in
`ClassData` rather than the prototype's `ObjMap`. Both returned right answers on
every hand-written test.

## License

Apache-2.0.
