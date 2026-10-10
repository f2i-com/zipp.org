# WebAssembly interpreter: register access, shared Python runtime, dead copies, thirteenth pass - 2026-10-10

Continues `codex/engine-size-reduction` from `56efa661` (the record that the
twelfth pass's modules were accepted) to `1c119bc8`. Everything here was
measured and gated at `8485c9e5`; the one commit after it changes two tests
and a comment, and builds the same modules byte for byte. Nothing is pushed
and CI has not run. The user accepted the twelfth-pass modules on 10 October;
they are the modules every WebAssembly figure here is measured against, and
"accepted" below means them. Native figures are from an ordinary release
build (`cargo build --release -p zipp-cli`, default features, not
profile-guided), so the published table is unchanged.

## Outcome

- **WebAssembly speed.** Against the accepted modules, one process a
  case, four runs: Lite takes 0.61 of the time over its corpus, the
  JavaScript module 0.89 to 0.90, Python-base 0.85 to 0.87 and
  Python + Torch 0.85 ([the screen](#webassembly-speed-screen)). No row of
  any module is flagged slower, the four the twelfth pass flagged included.
  An integer loop takes 0.17 to 0.18 of the time in Lite, where every
  register access had been a call, and 0.63 to 0.71 in the JavaScript
  module; a Python engine starts in 0.65 to 0.69 of the time and the two
  short Python programs run in 0.71 to 0.76.
- **WebAssembly size.** All four modules are smaller: Lite 20,777 bytes raw and 5,460 after
  Brotli, JavaScript 21,477 bytes raw and 1,534 after Brotli, Python-base
  45,241 bytes raw and 3,653 after Brotli and Python + Torch 45,247 bytes
  raw and 2,827 after Brotli ([the table](#webassembly-size)). The register
  file gave the bytes, and the flat function lookup 8 KB more in the
  JavaScript module (it adds 300 bytes to Lite); the shared function table
  took 24 KB in the JavaScript and Lite modules until `Copy` instructions
  gave it back.
- **Two update expressions changed bindings they may not.** `k++` on a
  `const` local left the constant incremented after the TypeError was
  caught, and `f++` inside `function f` replaced the function with NaN.
  Both are fixed in every build ([below](#two-bindings-an-update-changed)).
- **Native.** `c913b447` to `8485c9e5` is -0.07% [-1.08%, +4.76%] over the 13 real
  rows in ordinary release builds, and the headline ten are at 0.986x
  [0.980, 0.993] of Node 24. The JIT builds keep their bytecode.
- **Python.** The native suite is at 1.79x of CPython 3.13 in-script and 1.00x of
  the previous build. In the WebAssembly modules an engine start and a
  short program are a quarter to a third faster, above.
- **Correctness.** Test262 passes with the expected-failure manifest
  (95,671 of 95,680 executions, nothing unexpected, stale or skipped), and
  passes identically on a CLI built without the JIT, before and after the
  dead copies went. The default-feature `zipp-vm` suite passes: 2,194 tests
  in 309 binaries. The same suite in the `safe-sandbox` profile passes 1,695
  and fails the one test that failed before the pass, after four stale
  expectations it and the Lite-profile suite found were corrected
  ([below](#what-the-checked-profiles-suites-caught)). The Node boundary
  suite passes 35/35 on three modules and 33/35 on Python-base, whose two
  failures are the existing Torch-dependent checks.

## What the profiles showed

The twelfth pass profiled the JavaScript module. This pass profiled all
three kinds again in their accepted builds, the same way (function names
kept, the profiler started after warm-up).

| Module | Case | Where the time was |
| --- | --- | --- |
| Lite | arith-int | 27% in `<Vec as IndexMut<RangeTo>>::index_mut` and 11% in `<Vec as Index<RangeTo>>::index`: every register write and read was a call |
| Lite | prop-mono | 24% and 9% in the same two; 5% in `str::starts_with` under `is_private_key` |
| Lite | arith-int, after the first fix | 4% in `maybe_gc`, called at every back-edge |
| Lite | prop-mono, after it | 10% in `starts_with`, `is_private_key` and the UTF-8 encode between them; 6% in `ic_site`; 3% in `Vm::func` |
| Python-base | python-list-e2e | 12.8% in `<FuncProto as Clone>::clone`, 1.9% in `String::clone` and 1.7% in `malloc` under it, 5.2% freeing the copy in `drop_in_place<Program>` |
| JavaScript | error-typedarray | 3.4% in a hash-set insert under `make_error`, 3.5% in the removal under `prune_freed` |
| JavaScript | alloc-object | 4.5% in `resident_payload_bytes`, which allocation called twice for each new object and the audit once a slot at each poll |
| every module | arith-int | six instructions an iteration, two of them `Move`s of a value nothing reads |

The twelfth pass left "Lite runs an integer loop three times slower than the
JavaScript module" open, unattributed. The first row is the attribution.
`RegisterFile` derefs to the live prefix of a longer backing store,
`&storage[..logical_len]`; a size-first build does not inline `Vec`'s range
index, and the mutable one calls the slice's in turn.

## The changes

Each is one commit.

- **A register access is one bounds check in the checked profile**
  (`09bf2c18`). The longer backing store exists for emitted code, which
  revives a frame's window without writing it. The checked profile emits no
  code, so its register file is the `Vec` itself: the length is the live
  length, growing writes the fill value as before, and an access is one
  check through a slice the compiler indexes itself. The full modules, which
  inlined the two checks, lose one of them.
- **A private name is tested by its first byte; the collector's poll is
  inlined** (`0a2fdfd0`). `starts_with('#')` encodes the pattern and
  compares slices; it runs on every method call.
- **[[ErrorData]] is a bit for each heap slot** (`2bc3ae23`, `4a65b2c9`).
  It was a hashed set of slots under SipHash, written for every Error made
  and removed for every Error collected.
- **A Program's functions are shared prototypes** (`4a5dc9ac`). The Python
  frontend builds each program on a copy of the compiled runtime: 2,320
  prototypes, 3.4 MB of instructions, 1.6 MB of source text and 17,684
  string constants in 28,750 allocations, copied for each program and freed
  with its engine. `Program::functions` is now a `FuncTable`, a vector of
  shared pointers indexed and iterated as the vector of prototypes was;
  copying it copies pointers, and a prototype reached mutably is copied
  first if another table holds it. Each live engine also no longer holds
  its own 5 MB of the runtime.
- **A function is looked up in one flat table** (`8485c9e5`). A lookup
  compared the id against the program's function count and indexed one of
  two tables, and behind the shared pointers the program's branch took a
  second load: the native Python suite, which runs the runtime's functions
  in the interpreter, read 1.01x on its 44 programs with it. The VM now
  keeps every function's address by unified id; a lookup is one
  bounds-checked load, and the suite reads 1.00x. Its call-heavy programs
  read 1.02x to 1.04x before and after ([below](#python-suite)), so the
  lookup is not shown to be what moved them.
- **A bytecode instruction is `Copy`** (`2ae7c41e`). The shared table made
  the JavaScript and Lite modules link a prototype's clone for the first
  time, and the instruction type derived `Clone` alone: 24 KB of a 229-way
  branch in each. As `Copy` the clone is a block copy and the bytes came
  back.
- **An allocated object is sized once** (`e5f3d896`), **the dispatch loop
  keeps the running function's string constants** and **the inline-cache
  site lookup is inlined** (`eaaf44af`, `ec86780a`).
- **A statement's value is not copied when nobody reads it** (`7190396a`).
  `s += i` and `i++` compute in the local's own register and then copied the
  result into a scratch register as the expression's value. In a build
  without the JIT, a discarded update or non-logical assignment of a plain
  register local now names the local as its own result. A constant, a
  captured local, a function's own name, a parameter in its dead zone, a
  name a `with` could shadow and every use of the value keep the ordinary
  lowering. Builds with the JIT keep the copies, because its planners and
  stream recognisers match the compiler's exact output.

Quick looks at the steps (the whole corpus in one process, both orders,
trimmed means; not the screen), each over the build before it:

| Step | Module | Geomean | Rows that moved most |
| --- | --- | --- | --- |
| shared table, one sizing, kept constants, inlined site lookup | Lite | 0.97 | prop-mono 0.81 to 0.84, prop-poly 0.91 to 0.92, arith-mod 0.88 to 0.98, alloc-object 0.91 to 0.96, array-build 0.93 to 0.94 |
|  | JavaScript | 1.00 | enumeration 0.91 to 0.98, regex 0.96 to 0.97, regexp-matchall 0.94 to 0.99, property-delete 0.95 to 0.99, string-append-index 0.96 to 0.98 |
| update fix, no dead copies, bit set | Lite | 0.94 to 0.95 | arith-int 0.72 to 0.73, arith-mod 0.81, prop-mono 0.82 to 0.85, arith-float 0.84 to 0.85, prop-poly 0.91 to 0.93; slower: promise-resolve 1.03 to 1.08 |
|  | JavaScript | 0.93 to 0.94 | arith-int 0.74 to 0.77, prop-poly 0.76 to 0.83, arith-mod 0.82 to 0.83, prop-mono 0.84 to 0.89, arith-float 0.84 to 0.90 |
| everything after the shared table | Python-base | 0.94 to 0.95 | arith-int 0.73 to 0.76, prop-mono 0.79 to 0.80, prop-poly 0.80 to 0.83, arith-mod 0.82 to 0.83, arith-float 0.86 |

The one row a step reads slower is promise-resolve in Lite at the dead-copy
step, 1.03 to 1.08 in that quick look; the JavaScript module's reads 0.94
to 0.95 at the same step. Over the whole pass the row is faster on the
screen in every module (0.80 to 0.83 in Lite). It was not profiled at that
step.

### The loop kernels follow the compiler

The WebAssembly interpreter has exact kernels that recognise a slice of
compiler output and replace it with its closed form, charging the meter the
instructions it skipped. Two of them are loops (`vm/counted_loop.rs`,
`vm/object_loop.rs`) and both slices included the dead copies. They now
match the slices without them, five instructions for the counted sum and
fifteen for the object-property cycle, and charge those counts; their own
tests compare the kernel's billing with ordinary dispatch of a near-miss
loop and pin the exact and one-short budget edges.

A program charges fewer steps than it did wherever it has such statements,
because it runs fewer instructions: the arith-int loop charges 800,004
steps for 200,000 iterations where it charged 1,200,004. A host that tuned
an instruction budget to a workload gets more work for it.

## Two bindings an update changed

Found while writing the tests for the change above; both are in every
build and in the accepted modules, and both are fixed in `cc074ade`.

| Program | Node | Before | Now |
| --- | --- | --- | --- |
| `const k = 1; try { k++ } catch (e) { e.name + k }` | TypeError1 | TypeError2 | TypeError1 |
| `(function f() { f++; return typeof f })()` | function | number | function |
| the same in strict code, `try { f++ } catch (e) { e.name }` | TypeError | no throw | TypeError |

`update` read a const local from its own register, computed in place and
only then reached the store that throws. It had no test for a function
expression's own name, which assignment already refused. Both now compute
on a copy in a temp and write the result register only after the store,
so `r = k++` leaves `r` as it was when the store throws. Six further probes
of an assignment whose right-hand side throws part-way (a member update and
a negation with a throwing `valueOf`, array and object literals, a call)
leave the target local unchanged before and after this pass, as Node does.

## WebAssembly size

Rust 1.92.0, wasm-bindgen 0.2.126, Binaryen 125 (`-O1 -O1`), Node 24.19.0.
Brotli is quality 11 with the size hint, as the README states it. Module
bytes, raw / Brotli.

| Variant | Accepted | This pass | Raw | Brotli | Before post-processing |
| --- | ---: | ---: | ---: | ---: | ---: |
| Lite | 2,655,190 / 715,896 | 2,634,413 / 710,436 | -20,777 (-0.78%) | -5,460 (-0.76%) | 2,747,542 / 713,274 |
| JavaScript | 4,898,060 / 1,228,986 | 4,876,583 / 1,227,452 | -21,477 (-0.44%) | -1,534 (-0.12%) | 5,037,794 / 1,231,836 |
| Python-base | 6,564,659 / 1,601,071 | 6,519,418 / 1,597,418 | -45,241 (-0.69%) | -3,653 (-0.23%) | 6,713,904 / 1,604,884 |
| Python + Torch | 8,488,699 / 1,952,907 | 8,443,452 / 1,950,080 | -45,247 (-0.53%) | -2,827 (-0.14%) | 8,642,288 / 1,956,948 |

By step, Lite and JavaScript raw bytes: the register file 2,655,190 to
2,630,139 and 4,898,060 to 4,880,891; the shared table 4,881,845 to
4,905,351 in the JavaScript module, and with the single sizing 2,630,895 to
2,655,021 in Lite; `Copy` instructions 2,655,722 to 2,634,120 and 4,904,700
to 4,884,792; the flat lookup 2,634,113 to 2,634,413 and 4,884,767 to
4,876,583.

### Where the read-only data is

A linker map of the JavaScript module (`--Map`), ranked: 774 KB of
read-only data, 219 KB of it text. The largest items are a text table of
language, script and region triples (86 KB; likely subtags by its
content), a packed table of 80 KB with a 16 KB index of 48-byte records
beside it (time zone data by its shape; neither was traced to a symbol),
the regex properties of strings (37 KB), and core's two case-conversion
tables (12 KB each). `core::panic::Location`
records are 3,617 in that module, 58 KB with 6 KB of paths (Lite: 3,393,
54 KB). None of these is changed here.

### Two build settings measured and not adopted

| Setting | Module | Raw / Brotli | Corpus, quick look |
| --- | --- | --- | --- |
| `-C llvm-args=-inline-threshold=10` | Lite | +59,488 / +15,373 (2.3% / 2.2%) | 0.94 and 0.97 of the time |
| `-C llvm-args=-inline-threshold=25` | Lite | +127,704 / +33,908 (4.8% / 4.8%) | 0.93 |
| `opt-level = "s"` | JavaScript | -636,396 / -142,485 (13.0% / 11.6%) | 1.16; integer rows 0.96, calls and strings 1.2 to 1.3, RegExp compile 1.65 |
| `opt-level = 2` | JavaScript | -39,429 / -10,298 (0.8% / 0.8%) | 1.07 |

Each is one commit built twice, as shipped and with the one setting: the
Lite pair at `8485c9e5`, the JavaScript pair at `eaaf44af`. Lite is the
size-first build, and what its out-of-line helpers cost is better bought
one helper at a time, as the register file was: that change made Lite
faster and smaller. The size-first level takes an eighth off the JavaScript
module for a sixth more time; it is a different product, not a saving, and
is recorded for whoever wants that trade.

## WebAssembly speed screen

`node --no-liftoff`, one pinned processor, one process per case, both load
orders on two processors; ratios are this pass over accepted, medians of 31
batches. A case is flagged when it is more than 2.5% slower in three of the
four runs, the ninth pass's rule.

| Module | Geomean, four runs | Flagged slower | Its four readings |
| --- | --- | --- | --- |
| Lite | 0.613, 0.612, 0.614, 0.615 | none | |
| JavaScript | 0.896, 0.892, 0.902, 0.902 | none | |
| Python-base | 0.869, 0.851, 0.859, 0.853 | none | |
| Python + Torch | 0.852, 0.854, 0.855, 0.854 | none | |

- Lite, faster in three of four runs: alloc-object 0.61 to 0.61, arith-float 0.25 to 0.25, arith-int 0.17 to 0.18, arith-mod 0.20 to 0.21, array-build 0.43 to 0.45, array-hof 0.67 to 0.68, enumeration 0.70 to 0.78, error-range 0.76 to 0.86, error-type 0.80 to 0.84, error-typedarray 0.78 to 0.84, json 0.89 to 0.91, promise-resolve 0.80 to 0.83, prop-mono 0.25 to 0.26, prop-poly 0.37 to 0.38, property-delete 0.70 to 0.76, recursive-calls 0.60 to 0.61, regex 0.84 to 0.90, regexp-matchall 0.92 to 0.96, string-append-index 0.60 to 0.67, string-build 0.77 to 0.79.
- JavaScript, faster in three of four runs: alloc-object 0.88 to 0.91, arith-float 0.79 to 0.80, arith-int 0.63 to 0.71, arith-mod 0.74 to 0.76, array-build 0.72 to 0.90, array-hof 0.94 to 0.97, enumeration 0.91 to 0.95, error-range 0.91 to 0.95, error-type 0.86 to 0.89, error-typedarray 0.85 to 0.89, json 0.95 to 0.99, promise-resolve 0.89 to 0.90, prop-mono 0.74 to 0.77, prop-poly 0.74 to 0.81, property-delete 0.90 to 1.01, recursive-calls 0.95 to 0.97, string-append-index 0.92 to 1.01, string-build 0.86 to 0.95.
- Python-base, faster in three of four runs: alloc-object 0.87 to 0.91, arith-float 0.76 to 0.81, arith-int 0.62 to 0.92, arith-mod 0.72 to 0.75, array-build 0.78 to 0.90, array-hof 0.93 to 0.97, error-range 0.81 to 0.84, error-type 0.85 to 0.86, error-typedarray 0.83 to 0.84, promise-resolve 0.83 to 0.91, prop-mono 0.72 to 0.76, prop-poly 0.73 to 0.79, property-delete 0.91 to 0.98, python-arithmetic-e2e 0.74 to 0.75, python-dictionary-e2e 0.83 to 0.85, python-list-e2e 0.71 to 0.73, python_init_1_functions 0.66 to 0.69, python_init_200_functions 0.72 to 0.76, recursive-calls 0.91 to 0.95, string-append-index 0.91 to 0.95, string-build 0.87 to 0.93.
- Python + Torch, faster in three of four runs: alloc-object 0.87 to 0.89, arith-float 0.77 to 0.78, arith-int 0.65 to 0.65, arith-mod 0.72 to 0.77, array-build 0.75 to 0.97, array-hof 0.93 to 0.96, enumeration 0.92 to 0.96, error-range 0.79 to 0.88, error-type 0.86 to 0.87, error-typedarray 0.82 to 0.85, json 0.90 to 0.98, promise-resolve 0.87 to 0.87, prop-mono 0.73 to 0.78, prop-poly 0.74 to 0.77, property-delete 0.92 to 0.93, python-arithmetic-e2e 0.74 to 0.76, python-dictionary-e2e 0.75 to 0.82, python-list-e2e 0.73 to 0.75, python_init_1_functions 0.65 to 0.67, python_init_200_functions 0.73 to 0.73, recursive-calls 0.90 to 0.98, regexp-matchall 0.97 to 0.98, string-append-index 0.76 to 0.91, string-build 0.88 to 0.96.

Another session on the machine ran builds and headless-browser jobs in
bursts through these runs (total load 8% to 20% between quiet spells; a case
waited at most a minute for under 4%). The samples are paired within a
process and pinned, and a flag needs three readings of four, but single
readings show the bursts: Python-base arith-int reads 0.92 once and 0.62 to
0.66 three times, and array-build ranges from 0.72 to 0.97 across the full
modules.

The four rows the twelfth pass flagged slower are faster here: Lite
string-build 0.77 to 0.79 and json 0.89 to 0.91, JavaScript promise-resolve
0.89 to 0.90, Python + Torch error-typedarray 0.82 to 0.85.

call-deep does not move in any module (0.98 to 1.03, one Lite reading
0.91). Its call is answered by the interpreter's exact kernel for a numeric
`fib` (`try_metered_numeric_fib`) in 7 microseconds while charging 230,237
steps, so what the row times is the heap walk the poll takes when that
charge lands ([open](#not-claimed)). The two `javascript_init` rows and
regexp-unicode-compile are level; nothing on their paths changed.

The twelfth pass's cliff and RegExp-garbage probes were run again on these
modules and read as they did (the sticky tokenizer 163 ms in the JavaScript
module and 185 ms in Lite; the dropped-RegExp loops steady at 1 to 5 MiB).

## Native measurements

`tools/bench.py`, cold metric, ordinary release builds, one pinned processor,
with the other session's bursts as above.

- `c913b447` to `8485c9e5`, 12 paired repetitions: geomean -0.07%
  [-1.08%, +4.76%] over the 13 rows; start 6.8 and 6.8 ms. Three rows'
  intervals exclude zero: async-promise-chain +1.4% [+0.3%, +2.1%] (466 to
  473 ms), polymorphic-objects +0.8% [+0.2%, +2.1%] and regex-log-scan
  -2.0% [-4.1%, -0.3%]. The wide upper bound is two short rows caught by a
  burst, polymorphic-objects-v2 [-8.2%, +32.4%] and property-ic-shapes
  [-2.3%, +49.0%] (28 and 10 ms). The same comparison one commit earlier,
  `ec86780a`, before the flat function table, read -0.18% [-0.54%, +0.25%]
  with no interval excluding zero and the first two rows at -0.2%; the two
  runs differ by about what a row moves between runs.
- `8485c9e5` against Node 24.19.0, 10 paired repetitions: headline ten
  0.986x [0.980, 0.993], all 13 rows 0.683x [0.680, 0.688]. Behind Node:
  async-promise-chain 1.39x [1.36, 1.43], polymorphic-objects 1.26x
  [1.25, 1.28], json-large 1.16x [1.13, 1.17], map-set-heavy 1.06x
  [1.03, 1.11].

This pass did not look for native speed. A build with the JIT emits the
bytecode it did. What it has of this pass is the shared function table and
the flat lookup, `Copy` instructions, the running function and its string
constants kept across a frame, the first-byte private-name test, the bit
set, the single sizing and the two update fixes.

## Python suite

`tools/python_bench.py`, 7 repetitions, `c913b447` and `8485c9e5` against
CPython 3.13: in-script time 1.79x of CPython on the 36 programs both run
(micro 1.61x, macro 3.00x), 1.00x of the previous build on all 44 (micro
1.00x, macro 1.01x); process overhead 21.5 ms by the tool's median over
programs, and 21.9 ms for each build over all observations.

One commit earlier, with the shared function table and without the flat
lookup, the same comparison read 1.01x of the previous build (micro 1.01x)
and 1.81x of CPython, which is why the flat lookup exists. The call-heavy
programs read 1.02x to 1.04x in both runs (call_simple, call_defaults,
method_call, fib_recursive) and property_read 1.06x then 0.97x. A single
row of this suite has moved by more than that between two links of one
commit, so the rows are recorded and the geomean is what is claimed.

A native `zipp py` process compiles one program and keeps no second copy of
the runtime, so the copy this pass removes was never in these figures; it
is in the WebAssembly rows, where an engine is made for each program.

## Correctness

All on `8485c9e5` with a clean tree, run one after another with nothing else
of this session building. `1c119bc8`, the last code commit, changes two
tests and a comment after that run, each line for a line; the Lite and
JavaScript modules it builds are byte-identical to the ones measured.

- Test262, the full corpus with `tools/test262-expected-failures.txt`: 95,671
  of 95,680 executions pass, the nine failures are the manifest's, and there
  is no unexpected failure, stale expectation or skip. The local corpus is an
  exported tree under `target/`, so the runner prints the engine repository's
  commit as the corpus revision; the manifest pins corpus `42496613`.
- `cargo test -p zipp-vm --no-fail-fast` with the `tools/ci_quarantine.py`
  skips: 2,194 passed, 0 failed, 22 ignored, in 309 test binaries.
- The same with `--no-default-features --features safe-sandbox`: 1,695
  passed, 19 ignored, and two tests failed.
  `audit_20260915_props::props_modes_match_node` fails as it has since
  before the twelfth pass (its child process reaches "Maximum call stack
  size exceeded" in a debug build of this profile).
  `direct_local_method_arg::bytecode_shape_uses_the_local_directly_only_when_safe`
  counted the dead copy; `1c119bc8` states its expectation for each kind of
  build and it passes in the default, `safe-sandbox` and `wasm-lite`
  profiles.
- The same with `--no-default-features --features wasm-lite`: 1,662 passed
  and 47 failed in 24 binaries. By name those are the 45 that fail at
  `c913b447`, Intl, Temporal and module-loading tests of features Lite
  omits, and two this pass made stale: the shape test above and
  `array_copy_elision_tests::exact_numeric_sub_sort_preflights_and_restores_register_window`,
  which read the register file's backing length. Both are corrected in
  `1c119bc8` and pass there.
- `cargo test -p zipp-cli`: in the run of record one test,
  `build_source_identity::clean_identity_is_preserved_and_all_dirty_content_changes_the_sha256`,
  failed creating its scratch directory ("file already exists": the name is
  the process id and a nanosecond clock, and two tests of one process share
  both) and cargo stopped there. Run again whole: 80 passed, 1 ignored.
- `cargo test -p zipp-vm --features python --lib`: 625 passed, 5 ignored.
- `cargo test -p zipp-regress`: 108 passed, 1 ignored; with
  `utf16 compact-utf16 bounded-backtracking`: 124 passed, 1 ignored.
- `zipp-vm` checks with warnings denied in the default, `safe-sandbox`,
  `python`, `wasm-lite` and `safe-sandbox` + `wasm-compact-regex` profiles;
  the sandbox workspace's check against its lockfile and its 8 tests; the
  11 unit tests of `zipp-wasm`.
- The Node boundary suite on the exact modules of the size table: JavaScript
  35/35, Lite 35/35, Python + Torch 35/35, Python-base 33/35. The two
  Python-base failures are `audit-2026-09-15-ml-transport.cjs` and
  `python-gpu.cjs`, which need the optional Torch package, as in every pass
  since the ninth.

New tests: `tests/discarded_statement_values.rs` (seven tests of what an
assignment or update yields and leaves behind: plain locals, values that are
read, right-hand sides that read their own target, constants, a function's
own name, `arguments`, `with`, globals, dead zones, coercion order, eval
completion values); unit tests of the function table (a copy shares, a write
separates only the prototype written), of the slot bit set against a hash
set, of the checked register file (truncate, regrow, split, an access past
the live length is refused), of the compiler's output without the JIT (no
copy for a statement, a copy for a value that is read, the string
accumulator still appends in place) and of the counted-loop kernel against
the old seven-instruction slice; and a test that three Python programs
alive together run their own code.

### What the checked profiles' suites caught

Four tests that the default suite and Test262 pass, all from this pass. The
first two were found by a run of the `safe-sandbox` suite at `eaaf44af`,
the others by the runs of record at `8485c9e5`.

- `closure_side_table_gc_tests::register_gc_roots_stop_at_logical_len_not_initialized_high_water`
  asserted that a truncated register file still held its stale slot. The
  checked profile's file now ends at its live length; the precondition is
  stated for each profile (`19b722b0`) and what the test checks, that a
  truncated register is not a root, holds in both.
- `regex_execution_limits::deferred_legacy_statics_preflight_aggregate_and_handoff_to_heap`
  bounds the heap after a rejected reservation to 4 KiB over its baseline.
  The first form of the [[ErrorData]] change used the paged slot index,
  whose pages count toward the heap, so the first Error an engine made
  raised its heap by a page. The table is a bit set now (`4a65b2c9`) and
  the test passes unchanged.
- `direct_local_method_arg::bytecode_shape_uses_the_local_directly_only_when_safe`
  counted two copies in `index = text.charCodeAt(index)`: the one that
  stages the argument, which it guards, and the statement's dead copy.
- `array_copy_elision_tests::exact_numeric_sub_sort_preflights_and_restores_register_window`
  (compiled only with `meter-only`) asserted that a register growth
  survived as the backing store's length; it survives as the allocation.

The second of these was a change in what the heap figure counts, which no
default gate could have seen. The other three were tests tied to a
representation, corrected in `19b722b0` and `1c119bc8`.

### Test262 on the interpreter alone

Test262 runs on the native CLI, which has the JIT and so keeps the copies.
To put the new lowering through it, the CLI was built with `zipp-vm`'s
default features off (a one-line change to its manifest in a scratch
worktree, not committed) at `cc074ade`, before the change, and at
`2ae7c41e`, after it. Both pass 95,671 of 95,680 with the same nine
expected failures and identical failure lists.

The Python runtime is JavaScript the same compiler lowers. In a build with
`python` and `safe-sandbox` and no JIT, of eight Python test files five
pass whole and three fail one test each, and those three fail the same way
at `c913b447`: `sleep` does not block; one of the 156 corpus programs,
`py_numbers_int_arith.py`, prints nothing for a three-million-bit shift
(the corpus test runs every program and names only that one, at both
commits); and the fast-path comparison names that same program and no
other.

## Not claimed

- CI has not run. Nothing was measured on Linux, macOS or in a browser, and
  no profile-guided capture was taken.
- The JIT builds keep the dead copies. Removing them there means teaching
  the region planner and the stream recognisers the shorter slices.
- Native Python's call-heavy programs read 1.02x to 1.04x of the previous
  build. Some JIT call paths still index the program's table directly
  (`jit_calls.rs`, `helpers_misc.rs`, `ic.rs`) and take the shared
  pointer's second load; whether that, or anything, is behind those rows
  was not measured.
- The audit walk at each poll is 3.8% of an integer loop in the JavaScript
  module and 6.2% in Lite (`resident_payload_bytes` and `audit_heap_bytes`
  in profiles of this pass's modules at `ec86780a`), a larger share than
  before because the loop around it is shorter. It is the price of the heap ceiling on a small
  heap, and a sound way to skip it needs a signal that nothing was allocated
  since the last walk, which the VM does not have.
- Lite still calls out of line for `ObjMap::val_at`, `Heap::get` and an
  `Option` comparison on a property read (3% to 4% each of prop-mono). The
  first two have 223 and 1,458 call sites, so inlining them is not free.
- A Python engine start is 3.5 ms of a short program's 4.6 to 5.2 ms in
  the Python-base module (5.1 of 6.5 to 7.1 in the accepted one). What it
  spends now was not profiled.
- `free_slot` and `alloc_settled` are 5% to 10% each of the allocating rows
  and were not examined beyond the double sizing.
