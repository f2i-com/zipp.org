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

**Conformance — 99.71% of test262**, 95,574 of 95,848 required executions
(ECMA-262 + `staging`, run in both sloppy and strict mode as `INTERPRETING.md`
requires). Both tiers produce a **byte-identical** failure set, which is the
cheapest evidence that a JIT change has not quietly diverged:

| slice | executions | pass |
|---|---|---|
| ECMA-262 + staging, both modes | 95,848 | 95,574 (99.71%) |
| intl402 (opt-in, `--include-intl402`) | 6,682 | 4,250 (63.6%) |

That is up from 96.97% under `oxc_parser`, and the increase is the whole reason
the engine grew its own front end.

274 executions still fail. The two largest items are known and bounded:

* **Decorators (~34).** Not implemented, and `@dec` is an honest SyntaxError
  rather than a parser that accepts the syntax and silently ignores it. The
  runtime is the blocker, not the grammar: a decorator context object needs
  `addInitializer` and `access.{has,get,set}` as callable values carrying
  state, and this engine has no generic native-closure — every stateful native
  is its own `HeapObj` variant, with ~18 touch points where omitting one is a
  wrong answer rather than a compile error. Auto-accessors (`accessor x = v`),
  which share the cluster, *are* implemented.
* **Annex B block functions named `arguments` (2).** Left failing on purpose.
  B.3.2.4 step 1.a.ii's `parameterNames does not contain F` condition gates the
  whole step and FunctionDeclarationInstantiation puts `"arguments"` in
  parameterNames, so zipp is correct here and node is not. Matching node would
  mean becoming non-conformant.

`zipp js` parses the CommonJS-shaped script by default — top-level `return` is
legal, and an ESM-shaped `.js` falls back to the Module goal — because node
wraps a `.js` file in a function and real packages rely on both. Pass
`--script-goal` for the pure Script goal, which is what the test262 runner uses.

**intl402** is mostly a Temporal calendar suite in disguise: before non-ISO
calendars existed, 79% of its failures were one defect — every calendar but
`iso8601` was rejected. Nine arithmetic calendars now exist (gregory, buddhist,
roc, japanese, coptic, ethiopic, ethioaa, islamic-civil, islamic-tbla), all
closed-form, no CLDR data. `chinese`, `dangi` and `hebrew` still raise
RangeError rather than approximating, and `islamic-umalqura` needs a ~1,500
entry month-length table. Formatting output, collation order and plural
categories need real locale data the engine does not carry.

`tools/test262-expected-failures.txt` is the checked-in baseline, so a
regression is a diff rather than a remembered number. Run both tiers — a JIT
change that only *appears* correct is the common failure mode here.

**Performance — geomean 2.57× slower than node (V8)** on the ten real-world
benchmarks in `bench/real/`, paired medians of 7, every output byte-identical
to node:

| bench | node | zipp | ratio |
|---|---|---|---|
| map-set-heavy | 594ms | 723ms | 1.22× |
| json-large | 230ms | 484ms | 2.10× |
| async-promise-chain | 301ms | 693ms | 2.31× |
| polymorphic-objects | 300ms | 708ms | 2.36× |
| parse-large-js | 241ms | 583ms | 2.42× |
| class-prototype-hot | 275ms | 741ms | 2.69× |
| markdown-render | 238ms | 645ms | 2.71× |
| sparse-array | 50ms | 151ms | 3.03× |
| typedarray-math | 172ms | 674ms | 3.93× |
| regex-log-scan | 411ms | 1741ms | 4.24× |

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

One known blind spot in the suite: **all ten benchmarks open with
`"use strict"`.** That is realistic for modern bundles and unrepresentative of
the long tail — a change worth 7.7× to sloppy-mode function calls landed in this
repo and moved the suite 0.1%, because none of the ten could see it. Read a
0% suite delta with that in mind before concluding a change did nothing.

### Why it trails, honestly

This section has been rewritten four times because measurement refuted what it
said. `PERF_ROADMAP.md` keeps the numbers and, deliberately, the negative
results — of the last fourteen probes, twelve were refutations, and four of
those refuted an earlier entry in that same file.

**Start with the scale, because it disciplines everything else.** Computed from
the ten ratios above:

| scenario | geomean |
|---|---|
| today | 2.57× |
| `regex-log-scan` made *exactly* as fast as V8 | 2.22× |
| `typedarray-math` made *exactly* as fast as V8 | 2.24× |
| **both of the two worst at V8 parity** | **1.94×** |
| the uniform alternative | every bench 22.1% faster |

So beating the worst benchmark outright is not enough, and no stack of 1–2%
wins closes the gap. A plan that neither matches V8 on two benchmarks nor moves
every operation is not a plan for 2×, whatever else it is worth.

**One number explains most of the rest: the general (boxed) JIT tier costs
~3.5ns per op.** A property read measured at 21ns is not a 21ns read — it is a
six-operation iteration where every operation pays that rate. The INT tier does
the same arithmetic at ~1ns/op and V8's optimised code at under 0.3ns/op. This
is why making one operation dramatically cheaper keeps not showing up: a **7.7×**
improvement to plain-function call dispatch moved the suite **0.1%**, because it
made one op of six cheaper.

Where the time actually is:

1. **The regex matcher is an interpreter.** `regress` backtracks at 6.9ns per
   failed match attempt where V8's Irregexp, which compiles each pattern to
   native code, takes 0.37ns. A `\d` or `[a-z]` start predicate yields dozens of
   candidate positions per log line and each is a full interpreted attempt. Note
   this is the opposite of the scanning win noted above: we are faster at *not*
   matching (the memchr prefilter) and ~10× slower at matching. Decomposed,
   `regex-log-scan` is only 59% regex — the other 41% is corpus generation and a
   `charCodeAt` hash — so even an infinitely fast matcher leaves it at 2.9×.
2. **One allocation deoptimises its whole loop.** A loop containing `{}` is
   declined and runs interpreted. With five integer ops in the body: 15.2ns
   compiled, **80.0ns** once a single `{}` is added — the arithmetic did not
   change, it just lost its tier. This is also why `Promise.resolve(x)` measures
   40ns against node's 8ns despite allocating one heap slot and no `Vec`s.
3. **Numeric kernels miss the unboxed tiers for planner reasons, not
   representation ones.** `typedarray-math`'s phases decline with *"pinned
   receiver reg not cleanly excludable"* and *"read-only live-in used where a
   number isn't required"*. One of those causes turned out to be the bytecode
   for `i++` taking one register more than `++i` — fixed — and the rest are
   specified in `PERF_ROADMAP.md` B32.

**What is NOT the problem**, each ruled out by measurement:

- *Property-name interning.* Refuted four independent ways, most recently by
  interning the strings `for-in` hands out — which removes 7 of the ~13
  allocations an 8-key enumeration performs — for **+0.1%**. Counting
  allocations does not locate time in this engine.
- *A cheaper object layout.* `{}` costs 33.2ns here and **0.5ns** in node, flat
  in property count. V8 is not allocating faster; V8 is not allocating. Escape
  analysis deletes the object. A cheaper object is still an object.
- *A naively slow compiled tier.* Audited rather than assumed: constant-key
  property reads already use an 8-way inline cache that is call-free on a hit,
  and pinned dense-Array and TypedArray element access, monomorphic method calls
  and leaf calls are all inlined. Only a property access with a *non-constant*
  key still goes through a helper.
- *A lazier GC.* Tripling the collection threshold again (on top of an earlier
  3× that did pay) measured **+1.2% slower** — a larger live heap costs more in
  cache misses than the skipped collections save.

The single change with the right shape is hidden classes: a shape-keyed guard
lifts the existing inline cache off its 8-receiver identity cliff, and the same
infrastructure gives escape analysis and inline bump allocation. Everything
above is a symptom of its absence.

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

`using` / `await using` declarations are supported, including the rules that
make them awkward: disposal is scoped to a block, so the declaration is barred
from a Script's top level, an eval's top level and a bare `CaseClause`; each
declarator binds a plain identifier rather than a pattern; and there is no
for-in form. 148 of 152 `using` executions and 182 of 184 `await using` pass.

Known gaps: **decorators** (the parser has no `@` yet — 34 executions), the
remaining static-semantics early errors (above), most of `Intl` beyond `en-US`
`NumberFormat`, `Float16Array`, and `console` is a compile-time pattern match
rather than a real global object (so `const log = console.log` throws).

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

### The early errors that took the longest to get right

Each of these was wrong in a way that ordinary code never notices, and each was
settled by diffing against node rather than by reading the spec alone:

- **Where a bare `FunctionDeclaration` is a Statement.** Annex B B.3.2 allows it
  as a `LabelledItem` and B.3.4 as an `if` clause — and nowhere else, so
  `while (x) function f(){}` is an error. The two allowances do not compose:
  §14.6.1 adds `IsLabelledFunction(Statement) is false`, so `if (x) l: function
  f(){}` is an error although both halves are legal alone. `StmtPos` carries
  which of the three positions a Statement sits in.
- **`async function` in a Statement position** was read as an async function
  EXPRESSION, which made `for (;;) async function f(){}` a legal infinite loop
  instead of a SyntaxError.
- **The Annex B duplicate carve-out names `FunctionDeclaration`,** so a
  generator or async declaration on *either* side re-arms the error:
  `{ function f(){} function* f(){} }` is a SyntaxError in every mode.
- **PropName is the key's STRING VALUE.** `class C { 'constructor'(){} }` IS the
  constructor, and `class C { static 'prototype'(){} }` is the banned name — but
  a computed key has no PropName, so `static ['prototype'](){}` is legal.
- **A private name may repeat exactly once,** and only as a getter/setter pair
  of matching staticness.
- **ClassHeritage is evaluated outside the class's PrivateEnvironment**, so
  `class C extends (this.#x) {}` is an error even when `#x` is declared right
  below — while a nested class inside the heritage still sees its own names.
- **`{ var f; let f }` is an error but `var f; { let f }` is not.** The Block
  rule compares that block's `LexicallyDeclaredNames` against the
  `VarDeclaredNames` of the SAME StatementList, so a `var` has to stay visible
  in the block it was written in on its way out to the function scope.
- **A parameter collides with the body's lexical names, not its var names.**
  `function f(a){ let a; }` is an error; `function f(a){ var a; }` is not.
- **Generator and async DECLARATIONS take plain `FormalParameters`,** so
  `async function f(a,a){}` is legal in sloppy code — the shape that looks most
  like a method is the exception. Methods and arrows take
  `UniqueFormalParameters` and never allow a repeat.
- **Annex B legacy string escapes cannot be judged by the lexer.** `""` is a
  strict-mode error, but a `"use strict"` directive later in the same prologue
  turns strictness on retroactively, by which time the token exists. The
  spelling is recorded on the `Token` and the parser decides.

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
