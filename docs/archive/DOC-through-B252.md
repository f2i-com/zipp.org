# zipp — reference

> **Archived snapshot through B252.** This document contains stale present-tense
> performance and conformance statements by design. Use the current root
> [`DOC.md`](../../DOC.md) for live guidance.

Detail that used to live in `README.md`. The README is the current state at a
glance; this file is the long form. Measured figures and the reasoning behind
them live in `PERF_ROADMAP.md`, which is the experiment ledger — it keeps the
negative results too, and most entries in it are refutations.

## Contents

- [Building and layout of the workspace](#building-and-layout-of-the-workspace)
- [Conformance in detail](#conformance-in-detail)
- [Measurement discipline](#measurement-discipline)
- [Why it trails, honestly](#why-it-trails-honestly)
- [Language coverage](#language-coverage)
- [The front end](#the-front-end)
- [How the JIT is organised](#how-the-jit-is-organised)
- [Source layout](#source-layout)
- [Embedding](#embedding)
- [Development and the standing gate](#development-and-the-standing-gate)

## Building and layout of the workspace

Build from source (stable Rust; no other dependencies):

```sh
git clone https://github.com/f2i-com/zipp.org.git zipp
cd zipp
cargo build --release
./target/release/zipp js file.js
./target/release/zipp mjs file.mjs      # ES module entry (top-level await)
```

Release builds use fat LTO with one codegen unit (measured ~2% on the benchmark
suite), so expect the link step to take a minute. Put `target/release` on your
`PATH`, or copy the single `zipp` executable wherever you like — it has no
runtime files.

The project is `crates/zipp-vm`. It owns the whole pipeline — its own lexer,
parser and AST (`src/parse/`), the bytecode compiler, the interpreter and the
JIT — with no third-party parser. `zipp-cli` is a thin front end over it,
`crates/zipp-wasm` embeds it in WebAssembly for browser hosts, and
`crates/regress-fork` is the ECMAScript regex engine — our fork of regress
0.11.1, which adds an API the engine needs plus three test262 correctness
fixes (see its `FORK.md`).

Native code generation is feature-gated. x86-64 has the mature multi-tier JIT;
ARM64 has a guarded whole-function integer baseline for call-free functions and
numeric loops. Other targets build a pure interpreter
(`--no-default-features` does the same on a native target). wasm32 is built and
tested as interpreter-only.

> The workspace used to carry a separate ahead-of-time language (`zippc`, plus
> Cranelift/LLVM/WASM/zk back ends and a TypeScript frontend). That predates the
> engine and the objective moved; those crates have been removed.

## Conformance in detail

**Conformance — 99.994% of test262**, 95,936 of 95,942 required executions
(ECMA-262 + `staging`, run in both sloppy and strict mode as `INTERPRETING.md`
requires). Both tiers produce a **byte-identical** failure set, which is the
cheapest evidence that a JIT change has not quietly diverged:

| slice | executions | pass |
|---|---|---|
| ECMA-262 + staging, both modes | 95,942 | 95,936 (99.994%) |
| intl402 (opt-in, `--include-intl402`) | 6,714 | 6,502 (96.8%) |

Measured against tc39/test262 `defaaf15` (2026-07-27). The suite moves, so the
commit is part of the number: it is worth re-pinning whenever this figure is.

That is up from 96.97% under `oxc_parser`, and the increase is the whole reason
the engine grew its own front end.

6 executions still fail:

* **The suite contains two tests with opposite expectations (1).**
  `annexB/language/function-code/block-decl-func-skip-arguments.js` quotes the
  ES2017 step `Append "arguments" to parameterNames`, deleted in ES2018 when the
  arguments object moved to a separate `paramBindings` list, and requires that a
  block `function arguments(){}` NOT overwrite the arguments object.
  `staging/sm/regress/regress-602621.js` requires that it DOES, which is the
  current text. No engine can pass both; V8 fails the same one. zipp used to pass
  it, and this README used to cite that as being more conformant than node — it
  was the reverse. This one is red on purpose, and it puts a hard ceiling of
  95,941 on this checkout.
* **Only the `en` CLDR locale ships (2).** `staging/sm/String/internalUsage.js`
  wants `Intl.DateTimeFormat("de").format(t)` to give `2.1.1970`. Carrying one
  hand-written German pattern to turn this green is exactly the approximation
  this project refuses (see the intl402 note below); it stays red until real
  CLDR data lands.
* **Module evaluation errors are not memoised across a cycle (2).** A module's
  `[[EvaluationError]]` is recorded only on the SYNCHRONOUS rejection path, so a
  module that suspends at top-level `await` and rejects later leaves nothing
  behind, and a subsequent `import()` of an already-fulfilled member of an
  errored cycle resolves instead of re-throwing the original error. Open.
* **Top-level-await ordering in a cycle containing a deferred module (1).**
  `import-defer`'s eager async sweep evaluates the deferred graph's async
  subgraph at load time, but interleaves it with the importer's own suspension
  differently from the proposal. Open.

Getting here meant fixing the harness as well as the engine, and the two engine
bugs that mattered most were both **tier divergences** — the JIT disagreeing with
the interpreter, which is the failure mode this project gates hardest against:

* `$262.evalScript`'s var/function bindings live as own properties of the global
  object with the slot left `UNINITIALIZED`, and every JIT tier reads the slot
  directly. A harness function called from a loop therefore worked for the
  interpreted iterations and became `undefined is not a function` the instant the
  region tiered up — always at the same iteration, which reads like a scoping bug
  and is not one.
* `jit_get_prop_miss` indexed `program.functions[func_id]`, but a JIT-compiled
  function can be an *eval* function living past `main_func_count`. It panicked
  with `len is 3 but the index is 45` the moment such a function got hot enough
  to compile and took a property miss.

Both were latent long before this run and reachable from ordinary
`$262.evalScript`; running the harness as a real separate script is what finally
made them fire.

Moving the vendored suite forward a month (`de8e621c` → `defaaf15`) added 96
executions and surfaced two genuine gaps, both now closed:
**`Iterator.prototype.join`** was simply unimplemented (36 executions — the
helper is new enough that V8 24.12 does not have it either, so the suite, not
node, is the oracle), and a **`using`/`await using` at a module's top level was
never disposed** (6). It bound to a module slot, which took the global-binding
path in the compiler rather than the local one that registers disposables — so
the body dutifully opened a resource scope, ran `DisposeScope` on exit, and had
nothing in it. The declaration silently behaved like `const`. Block and function
scope were always correct, which is exactly why it went unnoticed.

Decorators — for a long time the single largest gap, at ~34 executions — are
now implemented end to end: the parser, the decoration runtime, and the
prerequisite that had actually been blocking them. A decorator context object
needs `addInitializer` and `access.{has,get,set}` as callable values that carry
state, and every stateful native here used to be its own `HeapObj` variant.
`HeapObj::NativeClosure` makes that a general mechanism, so a stateful builtin
now costs one match arm rather than ~18 touch points. All eight kinds work
(class, method, getter, setter, field, auto-accessor, static, private),
including on computed keys.

`zipp js` parses the CommonJS-shaped script by default — top-level `return` is
legal, and an ESM-shaped `.js` falls back to the Module goal — because node
wraps a `.js` file in a function and real packages rely on both. Pass
`--script-goal` for the pure Script goal, which is what the test262 runner uses.

**intl402** was mostly a Temporal calendar suite in disguise: before non-ISO
calendars existed, 79% of its failures were one defect — every calendar but
`iso8601` was rejected. Fifteen calendars now exist. Twelve are closed-form
and need no data at all (gregory, buddhist, roc, japanese, coptic, ethiopic,
ethioaa, islamic-civil, islamic-tbla, persian, indian, and hebrew — lunisolar
but purely computational, being the Metonic cycle plus the four dechiyot).
`islamic-umalqura` uses ICU's own month-length table, re-derived independently
from node date-by-date to confirm it.

`chinese` and `dangi` are implemented too, and are the only calendars that are
not closed-form: month starts and leap-month placement follow true new moons
and major solar terms, computed from truncated astronomical series rather than
a table, because test262 exercises years far outside any tabulated window.
Where they disagree with ICU (22 date-runs over 1900-2100) test262's own
expectations back zipp — including 1987 resolving to M06L where ICU says M07L,
and pre-1912 dates where the calendar was actually computed on Beijing local
mean time while ICU4C applies a flat +8.

The IANA time zone database is vendored — `vm/temporal/tzdata.rs`, generated by
`tools/gen_tzdata.py` from release 2026c, with the source URL and sha512 in the
header. Generated rather than hand-written so the provenance is checkable.

Two further intl402 divergences are deliberate and were proved rather than
assumed: the ICU4C `hebrew` calendar (6 dates) and a V8 `DurationFormat` 2^53
bound. They live here, in the intl402 slice — an earlier version of this section
listed them, and the `chinese` disagreements above, among the *main-suite*
failures, which was simply a filing error.

What remains is genuinely data-bound and is left failing rather than guessed
at: CLDR content (date patterns, unit display names with plural selection,
collation order, plural categories) and the Unicode algorithm data behind
`Segmenter` (UAX #29) and `Collator` (UCA/DUCET). The 208 failures concentrate
where that data is thickest — `NumberFormat` 88, `DateTimeFormat` 40, Temporal
30, `RelativeTimeFormat` 14, `ListFormat` 10, and a tail of 26 across
`Segmenter`, `Collator`, `PluralRules`, `DurationFormat` and `BigInt`. Where a
table *is* carried, it comes from the real upstream source and is verified
value-by-value against node's ICU.

`tools/test262-expected-failures.txt` is the checked-in baseline, so a
regression is a diff rather than a remembered number. Run both tiers — a JIT
change that only *appears* correct is the common failure mode here, and the two
bugs that took longest in this suite were both the JIT disagreeing with the
interpreter. On a Windows checkout that baseline is stored LF and checked out
CRLF, so a plain `diff` against the runner's LF output reports **every** line as
changed and the gate reads as a total regression; strip `\r` before comparing.

**Clone test262 with `core.autocrlf=false`.** Some tests assert the exact byte
length of a fixture they import, so a checkout that rewrites LF→CRLF fails them
for reasons that have nothing to do with the engine. If an existing clone has it
on: `git config core.autocrlf false && git rm --cached -r -q . && git reset
--hard`.

### Measurement discipline

The current figures live in `README.md`; the full ledger, including every
refuted experiment, lives in `PERF_ROADMAP.md`. What follows is the reasoning
behind how those numbers are taken.

This capture is `bench/head_clean_d06e81f_pgo.json`, marked `publishable: true`
like its predecessors (`bench/head_clean_e839613.json` was the first artifact
to earn that flag). The flag means the harness checked, *before
measuring*, that the engine reported a build identity, that its tree was not
dirty, and that its commit equalled the workspace HEAD — and checked again
afterwards that neither the binary's hash nor its reported source had changed
mid-run. The artifact carries `workspace_source`, `engine_source_before`,
`engine_source_after`, both binary hashes, and a sha256 of every benchmark
program run.

Re-measuring at a later commit gives similar ratios but not the same absolute
milliseconds, because the box and the Node build both move. The wave-9 capture
(1.2117×) is the cautionary tale: its box ran both engines 10–20% slower in
absolute terms, which flattered the ratio; at the wave-10 capture Node's
absolute times returned to the wave-8 baseline and the headline read 1.2547×.
The wave-12 capture is the same tale in the other direction: the bundle proved
wave 12 worth −0.59% on the headline ten (regex −4.6%, markdown −2.7%, parse
−1.4%, one binary, 21 pairs), and zipp's absolute times fell on every touched
row — yet the headline reads 1.2511× against wave 11's 1.2426× because Node's
absolute times fell 2.7% between the two captures while zipp's fell 1.4%.
Neither jump is an engine change. The comparison that means something is
same-conditions capture to same-conditions capture — wave 8's 1.2832× to wave
10's 1.2547×, −2.2% across two waves — against the waves' own one-binary
attributable sum (wave 9: bundle −0.5% plus the nursery retrial's −0.7%;
wave 10: bundle −0.05% with async −2.4%; wave 11: bundle −2.0% headline;
wave 12: bundle −0.59%; wave 13: bundle −7.3%; wave 14: bundle 0.0% on the ten
and −10.4% on the diagnostics; waves 15–18 measured against scratch-built baselines rather than Node, summing to about −3% on the ten, with two of those waves costing performance on purpose to buy correctness), which agrees to within the drift floor. Wave 13 is the clearest case in the series of why both numbers
are kept: its one-binary bundle attributes −7.3% on the headline ten
(`regex-log-scan` −20.8%, `typedarray-math` −33.5%, both far outside their
intervals), while the capture moved 1.2511× → 1.1962×, i.e. −4.4% — SMALLER
than the attributable win, because Node got faster on this box between the two
captures too. The per-wave `--ab-env` bundles, not the vs-node captures, are
the attribution instrument; the captures are the score.

Use `python tools/bench.py` — NOT `bench/run_real.sh`, which is kept only for
its historical series. The Python harness counterbalances two-engine AB/BA
order, deterministically shuffles benchmark order (and larger engine sets),
pairs an empty launch with every full launch, reports paired medians and
bootstrap 95% intervals, retains the complete schedule and raw observations,
and compares output as exact bytes. The shell script takes best-of-N and pipes
both outputs through `tr -d '-ÿ'` before comparing, which silently deleted
every non-ASCII byte — its "byte-identical" claim was not true. Use at least 15
pairs for a change expected under 10%, and 21 for a marginal decision. A
same-binary A/A check reversed the regex row from −0.4% to +1.1% on an
independent rerun while both nominal intervals excluded zero, so a result around
1% still needs independent reproduction.

zipp beats V8 on specific shapes: scalar-numeric kernels, self-recursive integer
functions, `s += …` string accumulation, dense-integer `Array` loops
(`for (i < a.length) s += a[i]` runs at 18ms against node's 12ms over 20M
elements), polymorphic method calls, and regex scanning that does not match.
Those wins do not carry to the benches above, which are dominated by object
construction, property access, and building result objects.

## Why it trails, honestly

This section has been rewritten four times because measurement refuted what it
said. `PERF_ROADMAP.md` keeps the numbers and, deliberately, the negative
results — the large majority of probes in it are refutations, several refuted an
earlier entry in that same file, and two refuted the file's own instruments.

**Start with the scale, because it disciplines everything else.** Computed from
the ten ratios above:

| scenario | geomean |
|---|---|
| today (cold total) | 1.16× |
| `regex-log-scan` at Node parity | **1.12×** |
| `polymorphic-objects` at Node parity | **1.12×** |
| **both of the two worst at Node parity** | **1.08×** |

(The *shape* of this arithmetic is what matters and it does not move as the
headline does: the two worst rows going to parity is worth ~0.11 of geomean,
and no contained fix reaches that. What wave 13 showed is that a row CAN be
taken to parity by a contained fix — `typedarray-math` went 1.59× → 1.09× —
but only after its cost was decomposed rather than guessed at, and only on the
third attempt at that row.)

The cold score being near 1.16× is not general parity: nine rows remain
slower and the two worst are 1.42× and 1.40×. The contained fixes in
`PERF_ROADMAP.md` are safe substrate, and one of the three architectural items
has now landed — the generational nursery is the DEFAULT collector since wave
9 (B122), and wave 10 rebuilt its internals (value-grain remembered set, a
cached minor mark vector, a survival-adaptive young budget — B123) to the
point where the remaining GC cost is mostly the churn-proportional sweep.
Moving toward 1× still requires the other two: stable shape metadata and an
optimizing CFG/SSA tier, rather than a stack of unmeasured 1–2% tweaks.

**One number explains most of the rest: the general (boxed) JIT tier costs
~3.5ns per op.** A property read measured at 21ns is not a 21ns read — it is a
six-operation iteration where every operation pays that rate. The INT tier does
the same arithmetic at ~1ns/op and V8's optimised code at under 0.3ns/op. This
is why making one operation dramatically cheaper keeps not showing up: a **7.7×**
improvement to plain-function call dispatch moved the suite **0.1%**, because it
made one op of six cheaper.

Where the time actually is:

1. **A successful regex match costs far more than the matching.** Measured with
   `test` only, so no result object exists anywhere and the pattern is the only
   variable: `/^2026-/` — anchored, hits at index 0, five literal bytes, nothing
   to search — costs **197ns against Node's 7ns**. The same regex that costs 343ns
   when it hits costs 107ns when it misses. Splitting that out gives a ~113ns
   fixed per-call floor, ~85ns of success bookkeeping, ~60ns per capture group,
   and actual matching only ~4× off. So the interpreted matcher is the *smallest*
   of the four terms, and a compiled backend is aimed at the wrong one — which is
   why the experimental regular-subset tier moved the row 2.82%. Two of the real
   terms are priced: the Annex B legacy statics (**landed, −8.5%** — they copied
   `leftContext` + `rightContext`, ~87% of the subject, on every successful match
   including `test`, for values almost nothing reads) and the result array's
   `index`/`input`/`groups`, which live in a side hash map (−13.5%, open). A
   further 27% of that row's gap is corpus generation, which contains no regex at
   all. Full decomposition in `PERF_ROADMAP.md` B60.
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

- *Builtin method dispatch by name.* A jump table was an open plan item whose
  gain was recorded as "the largest single term in markdown-render". Counting
  the dispatches that actually reach the generic chain settled it:
  `parse-large-js` makes **89**, `polymorphic-objects` makes **zero**, and
  markdown-render's 252,669 are ~2.3% of that row.
- *Array `push`.* 6.20ns against node's 9.20ns — zipp wins, so the tokenizer's
  ~14M pushes are not `parse-large-js`'s gap.
- *The collector.* An allocation micro showed per-object cost rising 74.5 →
  122.5ns purely from a larger live set, and that was written up as the largest
  systemic cost. Per-phase GC timing then priced collection at **2–12%** of real
  rows: their live sets never reach where that curve bites. The claim was wrong
  and is recorded as wrong.
- *Property-name interning.* Refuted four independent ways, most recently by
  interning the strings `for-in` hands out — which removes 7 of the ~13
  allocations an 8-key enumeration performs — for **+0.1%**. Counting
  allocations does not locate time in this engine.
- *A cheaper object layout.* `{}` costs ~30ns here and **0.5ns** in node, flat
  in property count. V8 is not allocating faster; V8 is not allocating. Escape
  analysis deletes the object. A cheaper object is still an object — an `ObjMap`
  recycle pool that made construction **35%** cheaper still did not survive the
  suite and was reverted.
- *A naively slow compiled tier.* Audited rather than assumed: constant-key
  property reads already use an 8-way inline cache that is call-free on a hit,
  and pinned dense-Array and TypedArray element access, monomorphic method calls
  and leaf calls are all inlined. Only a property access with a *non-constant*
  key still goes through a helper.
- *A lazier GC.* Tripling the collection threshold again (on top of an earlier
  3× that did pay) measured **+1.2% slower** — a larger live heap costs more in
  cache misses than the skipped collections save.

**Hidden classes are still worth building, but an earlier version of this
section called them "the single change with the right shape", and measurement
does not support that for these ten rows.** A shape-keyed guard was priced
against this exact suite at **+0.4%**: no row here is megamorphic by identity
while monomorphic by shape. `polymorphic-objects` stops at exactly eight
receivers by construction, and `json-large`'s keys are random enough to blow the
shape table's cap. The infrastructure still buys escape analysis and inline bump
allocation, which is why it stays on the list — it is just not what these
benchmarks are waiting for.

**What they are waiting for is the general tier's code quality.** `ZIPP_PROF=1`
(a sampling profiler, added after a real −35% construction win had to be
reverted for want of attribution) puts **four of the ten rows at or above 85% of
their time inside native compiled code** — `class-prototype-hot` 99.9%,
`typedarray-math` 99.7%, `parse-large-js` 91.6%, `map-set-heavy` 84.8%. For
those, tier entry is solved and what remains is what the tier emits. The INT
tier already keeps values in xmm homes with a copy-elision peephole, and on
shapes it accepts zipp matches V8 (`s = s + 1.25` over 20M iterations:
0.45ns/iter against 0.40ns). The MEM tier — where the DataView kernel and most
of `parse-large-js` run — routes every intermediate through
`[rbx + dreg(r)]` and re-boxes at each step. Extending register homes to that
tier is the architectural item, and no peephole substitutes for it.

The three rows that are *not* native-bound each name their own subsystem rather
than a general defect: `regex-log-scan` its matcher (27% regex-exec, where a
successful `exec` is 187ns of scan plus 173ns of result object plus 133ns for
two capture groups, against node's 40ns total), `json-large` its serialiser
(24% `JSON.stringify`), and `async-promise-chain` its event loop (61%
interpreted callbacks plus 17% microtask machinery).

Read `interp/untagged` in that profiler as *"no phase tag was active"*, not
*"the interpreter was running bytecode"* — the distinction cost a wrong
conclusion before it was documented. Single-argument `JSON.stringify(v)` is
fused by the compiler into its own opcode and never reaches `call_native`, so a
tag placed there never fired and a stringify-only workload reported 100%
`interp`; `json-large` looked 40% interpreted when a quarter of it was
serialisation.

## Language coverage

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

The mature x86-64 backend is organised as follows.

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

ARM64 deliberately starts smaller. Once a function is hot, the backend accepts
only a bounded, call-free whole-function subset: tagged integer loads and
arithmetic, comparisons, branches, loops, and returns. Type mismatches,
overflow, and results that require a non-integer representation leave the
destination untouched and resume the interpreter at the exact bytecode ip.
There are no ARM helper calls, OSR regions, regex codegen, or native metering in
this first tier; attaching instrumentation disables it, and `zipp sandbox`
disables every native-code tier before parsing untrusted source. Both emitted
bytes and executable-allocation count are capped, and repeatedly bailing bodies
are evicted and blacklisted.

## Source layout

```
crates/zipp-vm/src/
  parse/       source -> AST: lexer, tokens, AST, parser (9 modules)
  front.rs     the source -> AST entry point
  compile/     AST -> register bytecode (14 modules)
  codegen/     native x86-64 JIT, dynasm (15 modules)
  codegen_aarch64.rs  guarded ARM64 whole-function baseline, dynasm
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
It is a deliberately separate Cargo workspace: its `safe-sandbox` dependency
cannot be feature-unified with the native CLI's JIT. The browser engine applies
lifetime instruction, payload-aware heap, output, host-value, host-bridge,
source, runtime-compilation, and retained-definition ceilings, with a 256 MiB
link-time maximum on WebAssembly linear memory as the allocator backstop.
Resource exhaustion is a typed terminal state even if guest code catches the
immediate exception or turns it into a rejected promise. These are resource
controls, not wall-time or OS isolation: run untrusted browser code in a
dedicated Web Worker and terminate the Worker at the host deadline. Dynamic
function/class definitions use VM-lifetime stable addresses, so tearing down
the Worker/WASM instance—not merely calling `Engine.dispose()`—is also the
complete reclamation boundary between tenants.

### wasm32 has no clock

`Instant::now()` and `SystemTime::now()` on `wasm32-unknown-unknown` are std
stubs that **panic**, and `Vm::new` records a start instant — so an un-shimmed
engine traps on construction, before running a line of JS. `vm/clock.rs` is the
boundary: native targets re-export `std::time` unchanged, and wasm reads clocks
the host installs via `install_clock` (a no-op elsewhere, so an embedder can call
it unconditionally). A hook rather than a `[target.wasm32]` js-sys dependency,
because the engine should not have to assume its wasm host is a browser.

## Development and the standing gate

The standing gate for any engine change — see `PERF_ROADMAP.md` §2:

```sh
cargo build --release
cargo test --workspace --release
(cd crates/zipp-wasm && cargo test --locked --release)
python tools/run_test262.py --t262 <path> --dump-fails fails.txt
diff <(sort fails.txt) \
     <(tr -d '\r' < tools/test262-expected-failures.txt | sort)      # REG=0
ZIPP_NOJIT=1 python tools/run_test262.py …             # and again, interpreter only
ZIPP_JIT_THRESHOLD=1 python tools/run_test262.py …     # and again, JIT forced on
ZIPP_NO_NURSERY=1 python tools/run_test262.py …        # and again, majors-only GC
python tools/bench.py --reps 15                        # ALL_CORRECT=1
python tools/bench.py --reps 15 --readme               # + the tables above
```

On Windows the test262 runner needs `PYTHONUTF8=1` — a failing test can print a
non-ASCII character, and the default console codec kills the whole run with a
`UnicodeEncodeError` after it has already done the work.

All four passes must produce IDENTICAL failure sets. They currently do, exactly,
which is worth re-checking rather than assuming: it is the cheapest evidence that
a JIT change did not silently alter semantics. The fourth pass exists because the
default collector is generational since wave 9 — a minor GC that missed a write
barrier would corrupt exactly the kind of long-lived graph test262 never builds,
so the majors-only sweep is the cross-check that the two collectors agree.

The third pass is the one people skip and should not. `ZIPP_JIT_THRESHOLD=<n>`
overrides both the function and loop compile thresholds, because **the default
run never reaches Tier C**: the region JIT compiles only hot loops, and test262
asserts once, straight-line. Without it, every JIT-only helper is gated by ten
benchmarks' stdout. The first run of it found `this.x = 0` globals reading as
`NaN` from compiled code — live at default thresholds, invisible to all 95,936
executions (`PERF_ROADMAP.md` B65).

Anything touching the embedding API or the wasm layer also has to clear the Node
harnesses in `crates/zipp-wasm/tests/node/` — `cargo test` covers the Rust side
but not the wasm-bindgen boundary (marshalling, the bridge closures, the host
queue), and that boundary is where the interesting bugs live. One of them drives
a real third-party bundle unmodified.

A change that builds on x86-64 can still break every other target: native JIT
code is target-gated, and an attribute that drifts onto the wrong item can take
ARM64 or wasm32 down without x86-64 noticing. Cheap local insurance:

```sh
cargo build --target wasm32-unknown-unknown -p zipp-vm --no-default-features
cargo check --target aarch64-linux-android -p zipp-vm --all-targets
```

The hosted ARM64 workflow executes the native mechanism/library tests on Linux,
Windows, and macOS, with the tier-differential and sandbox slices additionally
running on Linux; a cross-check alone proves compilation, not that generated
instructions run correctly.

`ZIPP_NOJIT=1` disables native codegen (it is **presence**-checked, so
`ZIPP_NOJIT=0` also disables it — unset it for a JIT run); `ZIPP_NO_NURSERY=1`
reverts to the majors-only collector (the generational nursery is default-on
since wave 9; its young budget ADAPTS to measured survival since wave 10 —
`ZIPP_NURSERY_YOUNG_BUDGET=<n>` pins it, `ZIPP_NO_NURSERY_ADAPT=1` pins the
16k default, and `ZIPP_NURSERY_VERIFY=1` re-runs the full mark beside every
minor and panics naming any slot the young-only trace missed);
`ZIPP_GC_STRESS=1` collects on every allocation; `ZIPP_JITLOG=1` reports tier
decisions, deopts and evictions; `ZIPP_JITDECLINE=1` names which planner
check rejected a region.

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
