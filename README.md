# zipp

A clean-sheet JavaScript engine written in Rust — a NaN-boxed, explicit-frame
register VM with per-call-site inline caches and a native x86-64 OSR JIT.

```sh
cargo build --release
./target/release/zipp js file.js
./target/release/zipp mjs file.mjs      # ES module entry (top-level await)
```

The project is `crates/zipp-vm`. It owns the whole pipeline — its own lexer,
parser and AST (`src/parse/`), the bytecode compiler, the interpreter and the
JIT — with no third-party parser. `zipp-cli` is a thin front end over it,
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

Both figures are measured on this repo, not estimated. Neither is finished —
they are the current state.

**Conformance — 98.1% of test262**, 94,217 of 96,029 required executions
(ECMA-262 + `staging`, run in both sloppy and strict mode as `INTERPRETING.md`
requires):

| slice | executions | pass |
|---|---|---|
| ECMA-262 + staging, both modes | 96,029 | 94,217 (98.1%) |
| intl402 (opt-in, `--include-intl402`) | 3,341 | 563 (16.9%) |

That is up from 96.97% under `oxc_parser`, and the increase is the whole reason
the engine grew its own front end. 1,812 executions still fail, across 1,088
distinct files; **607 of those files are parse-phase negative tests** — static
semantics the parser does not yet enforce. It is still the largest single
category, but it used to be all of it.

The dominant intl402 cause is unrelated: `Intl.DateTimeFormat` cannot be
constructed at all (`vm/intl.rs:436`).

`tools/test262-expected-failures.txt` is the checked-in baseline, so a
regression is a diff rather than a remembered number. Run both tiers — a JIT
change that only *appears* correct is the common failure mode here.

**Performance — geomean 2.59× slower than node (V8)** on the ten real-world
benchmarks in `bench/real/`, paired medians of 5, every output byte-identical
to node:

| bench | node | zipp | ratio |
|---|---|---|---|
| map-set-heavy | 606ms | 726ms | 1.20× |
| json-large | 230ms | 472ms | 2.05× |
| async-promise-chain | 301ms | 695ms | 2.31× |
| polymorphic-objects | 297ms | 712ms | 2.40× |
| parse-large-js | 243ms | 591ms | 2.43× |
| markdown-render | 239ms | 631ms | 2.63× |
| class-prototype-hot | 263ms | 762ms | 2.90× |
| sparse-array | 50ms | 156ms | 3.10× |
| typedarray-math | 172ms | 681ms | 3.97× |
| regex-log-scan | 408ms | 1754ms | 4.30× |

Startup is ~4× faster than node (7ms vs 30ms — no snapshot to load).

zipp beats V8 on specific shapes: scalar-numeric kernels, self-recursive integer
functions, `s += …` string accumulation, dense-integer `Array` loops
(`for (i < a.length) s += a[i]` runs at 18ms against node's 12ms over 20M
elements), polymorphic method calls, and regex scanning that does not match.
Those wins do not carry to the benches above, which are dominated by object
construction, property access, and building result objects.

Use `python tools/bench.py` — NOT `bench/run_real.sh`, which is kept only for
its historical series. The Python harness runs engines paired (so machine drift
lands on all of them), reports medians with p10/p90, keeps raw samples, and
compares output as exact bytes. The shell script takes best-of-N and pipes both
outputs through `tr -d '-ÿ'` before comparing, which silently deleted
every non-ASCII byte — its "byte-identical" claim was not true. Back-to-back
runs of the same binary drift 3–10%, so `--ab old.exe new.exe` is the only
reliable way to judge a change under a few percent.

### Why it trails, honestly

This section has been rewritten three times because measurement refuted what it
said. `PERF_ROADMAP.md` has the numbers and, deliberately, the negative results.

1. **Object construction and result objects.** An `ObjMap` is three parallel
   `Vec`s plus a `String` per key, so a small object is ~6 allocations. The
   sharpest instance: `RegExp.prototype.exec` spends **316ns attaching
   `index`/`input`/`groups`** to its result — more than the match itself —
   split 128ns hash insert, 146ns three `Vec` allocations, 42ns the keys.
2. **The regex matcher is an interpreter.** Matching costs 234ns where V8's
   Irregexp, which compiles each pattern to native code, takes 22ns. Note this
   is the opposite of the scanning win above: we are faster at *not* matching
   (the memchr prefilter) and 10× slower at matching.
3. **Boxed values in the general JIT tier.** Every non-integer loop round-trips
   through NaN-boxing with a tag guard per operation.
4. **GC sweep is proportional to the heap, not the garbage.** Timing the phases:
   sweep dominates mark, and its cost is `free_slot` dropping each dead object's
   `Box` and three `Vec`s. That is what a generational nursery removes.

**What is NOT the problem**, each ruled out by measurement:

- *Property-name interning.* Refuted three independent ways: an `Rc<str>` intern
  table measured 5–8% slower, the regex-result decomposition put the three keys
  at 42ns of 316, and ablating the per-key `String` moved a 3-property literal
  114 → 108ns.
- *Allocation admitted into JIT regions.* Built and measured: `{}` went 35 → 62ns
  **slower**, because one win64 call costs more than the interpreter's own
  `NewObject` arm. Four consecutive tier-admission attempts confirmed the rule —
  admitting an op only wins when that tier's representation makes the op cheaper
  than the tier it displaces.
- *The microbenchmark object gap.* node reports `{}` at 0.2ns because escape
  analysis removes the allocation entirely. Comparing 35ns against that and
  calling the difference allocation cost was wrong.

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

Known gaps: the remaining static-semantics early errors (above), decorators
(the parser has no `@` yet), `using`/`await using` declarations, most of `Intl`
beyond `en-US` `NumberFormat`, `Float16Array`, and `console` is a compile-time
pattern match rather than a real global object (so `const log = console.log`
throws).

## The front end

`src/parse/` is a hand-written lexer and recursive-descent parser producing
`src/parse/ast.rs`. It replaced `oxc_parser` outright — the workspace has no
`oxc_*` dependency left.

Not for parsing speed, which is ~12% of getting from source to bytecode. For
the **early errors**: roughly 2,200 static-semantics `SyntaxError`s the engine
could not raise, every one needing binding, strictness or positional state that
exists only *while parsing*. A tree handed over after the fact cannot
reconstruct "was this the second `let x` in this scope".

Five decisions follow from serving an engine rather than a toolchain, and each
deletes something the old arrangement paid for:

- **Owned `Box`/`Vec`, no arena, no lifetime**, so the tree is `Send` and can
  live in an `Arc`. `vm/agents.rs` used to re-parse in-thread purely because
  `oxc_allocator` is not `Send`, and the module loader wanted a cache it could
  not have for the same reason.
- **A call can be an assignment target.** Annex B makes
  `AssignmentTargetType(CallExpression)` *simple* in sloppy code, so `f() = 1`
  must parse and throw a `ReferenceError` at RUNTIME. `Target::Call` says that
  directly, replacing a workaround that rewrote source text and reparsed.
- **Strings are `StrVal`**, so a lone surrogate is representable and the
  parallel WTF-8 buffer the compiler carried to recover them is gone.
- **No parenthesized-expression node.** Parenthesization is observable in
  exactly two places, so it is a `bool` on `Target` rather than a wrapper 13
  sites must peel.
- **No scope or binding state on nodes.** The parser raises early errors as it
  goes and discards its scope tree; the compiler builds its own.

The three hard parts, and how they are handled:

**Cover grammars.** `( a, b )` is a parenthesized expression until a `=>`
arbitrarily far away proves it arrow parameters; `{ a = 1 }` is an object
literal (a SyntaxError) until a `=` proves it a destructuring target; `async` is
an identifier until what follows says otherwise. The technique is the spec's
own: parse the permissive superset ONCE, *record* the errors that are fatal in
only one reading, and discharge the losing set when the ambiguity resolves.
Never backtrack, never re-lex.

**The `/` ambiguity.** Whether `/` starts a division or a regex is decidable
only from grammatical position, so `Lexer::next_token` takes a `regex_allowed`
flag from the parser rather than guessing from the previous token. One case
cannot be answered when the token is produced: the `}` closing a function or
class body is scanned by code shared between a declaration (statement — regex
follows) and an expression (operand — division follows). The statement layer
re-lexes that single token afterwards.

**Templates nest.** `` `a${ `b${c}` }d` `` cannot be scanned in one pass, so the
lexer hands control back at each `${` and is resumed by the parser at the
matching `}`.

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
  parse/       source -> AST: lexer, tokens, AST, parser (9 modules)
  front.rs     the source -> AST entry point
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
cargo test --release                                   # 337 tests, 0 failed
python tools/run_test262.py --t262 <path> --dump-fails fails.txt
diff <(sort fails.txt) <(sort tools/test262-expected-failures.txt)   # REG=0
ZIPP_NOJIT=1 python tools/run_test262.py …             # and again, interpreter only
python tools/bench.py --reps 7                         # ALL_CORRECT=1
```

On Windows the test262 runner needs `PYTHONUTF8=1` — a failing test can print a
non-ASCII character, and the default console codec kills the whole run with a
`UnicodeEncodeError` after it has already done the work.

The two tiers must produce IDENTICAL failure sets. They currently do, exactly,
which is worth re-checking rather than assuming: it is the cheapest evidence
that a JIT change did not silently alter semantics.

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
- **Check the tier before quoting a microbenchmark.** Several loops are declined
  by the call-mix gate and run interpreted, so their timings measure
  interpretation, not the operation. `ZIPP_JITLOG=1` says which. "The loop was
  interpreted" looks exactly like "the operation is slow".
- **Rebuild before measuring.** Windows will not replace a running `zipp.exe`,
  and `cargo build` reports the failure on a line that scrolls past; the next
  measurement then silently describes the OLD binary.

test262 is the real gate for anything touching property semantics; the unit
suite will not catch it. Two examples from this repo: a fast path for ordinary
property writes assumed `%Object.prototype%` carries no accessor for an ordinary
key (a program can install one), and missed that class accessors live in
`ClassData` rather than the prototype's `ObjMap`. Both returned right answers on
every hand-written test.

## License

Apache-2.0.
