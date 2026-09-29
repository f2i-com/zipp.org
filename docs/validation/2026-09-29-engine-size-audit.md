# Engine size audit and ZIPP Lite — 29 September 2026

The full JavaScript and Python variants retain their features. A separate
`lite` WASM artifact removes Intl, Temporal, Python and Torch, and prioritizes
code size over execution speed. Lite still includes eval/Function, Unicode
regex, normalization, BigInt, proxies, collections, typed arrays, promises and
the existing hardened Engine embedding. It does **not** yet match QuickJS size.

## Reproducible comparison

Baseline: commit `09862c666dc30e9d510ecd4fe85bda6a75eaea7f`, freshly built rather
than inferred from an older README. Windows x86-64, Rust 1.92.0,
wasm-bindgen 0.2.126, Node 24.19.0, fat LTO, one codegen unit. Full variants use
release optimization; Lite uses the separate `lite` profile (`opt-level="z"`).
Overflow checks, abort-on-panic, sandbox resource ceilings, 16 MiB shadow stack
and 1 GiB linked memory maximum remain enabled.

Sizes are the WASM module alone after wasm-bindgen and removal of name,
producer and target_features sections. Brotli means quality 11. JavaScript
glue, HTTP headers, instantiated memory and native executables are different
measurements. Machine-readable results, hashes, timings and attribution are in
[the evidence file](2026-09-29-engine-size-evidence.json).

| Variant | Baseline raw | Final raw | Baseline Brotli | Final Brotli |
| --- | ---: | ---: | ---: | ---: |
| JavaScript full | 5,749,516 | 5,534,466 | 1,326,687 | 1,325,640 |
| Python-base | 7,473,393 | 7,258,351 | 1,703,770 | 1,701,116 |
| Python + Torch | 9,432,864 | 9,217,822 | 2,050,540 | 2,048,552 |
| ZIPP Lite | — | 3,076,218 | — | 780,480 |

Lite saves 46.5% raw and 41.2% Brotli versus the fresh full-JavaScript baseline. Full variants save approximately 215 KB raw each; their already-compressed transfers improve only slightly.

Build the variants with:

```sh
cd crates/zipp-wasm
./build-variants.sh lite javascript python all
```

The script emits separate packages under `dist/<variant>/`, including `.wasm.br`.
`node tools/measure_wasm_size.mjs <file.wasm>...` from the repository root
reproduces raw, gzip, Brotli, section sizes and hashes. The existing
`tests/node/bench.cjs` takes Node package directories; the focused
`tests/node/bench-packed-tables.cjs` takes baseline and candidate package
directories and reproduces the table timings below.
For the same Lite features with faster optimization, use
`cargo +1.92.0 build --locked --release --target wasm32-unknown-unknown --features lite`.
The `lite` feature is intentionally subtractive and isolated in the WASM
workspace; it must not be unified with a full embedder's Cargo features.

## What changed

1. **Real Lite feature boundaries.** Namespace construction, native dispatch,
   constructor paths, property access and generic Temporal value formatting
   are gated. Hiding globals alone left timezone code reachable through value
   formatting; that retaining path was removed too. Date keeps its ordinary ISO
   parsing. Locale methods remain present with deterministic core-language
   defaults, documented in the WASM README. `zippProfile()` exposes `variant`
   and `omittedFeatures`; the feature-contract test checks the actual guest APIs.
2. **Lossless regex property storage, shared by every variant.** Generated
   Interval literals are const-encoded as gap/width varints. The parser decodes
   directly into the owned CodePointSet allocation it already needed. Matching
   uses exactly the same representation. ID_Start and ID_Continue remain
   directly indexed so each named-group character does not require decoding a
   table. All 367 source tables have exhaustive round-trip tests. Regeneration
   is documented in `crates/regress-fork/FORK.md` and automated by
   `tools/pack_regex_tables.py`. This saves raw bytes much more than Brotli bytes.
3. **Lossless timezone transition storage, shared by full variants.** All
   16,287 transitions remain. Blocks of 32 use signed timestamp deltas and an
   offset palette; checkpoint lookup bounds decoding without expanding the
   database into a heap allocation. Cached per-zone tails keep contemporary
   annual-rule lookups out of the historical decoder. The generator is updated;
   original timestamp/offset arrays are retained only under `cfg(test)`.
   Tests compare every record and both partition predicates at t-1, t and t+1
   for every zone, including empty lists and extreme search instants.
4. **Browser-only diagnostic exclusion.** Native environment-controlled
   bytecode dumps are excluded on wasm32-unknown-unknown, which has no process
   environment for enabling them. Native debugging remains available. This is
   a very small change, not a major source of savings.

## Performance and rejected experiments

The final full JavaScript build's 12-workload elapsed-time geomean is 1.007× baseline (within about 1%). Lite's is 2.16× baseline: substantially slower, as expected from the explicit size-first profile. This is elapsed time, so larger is slower.

| Focused full-build workload (100 operations) | Baseline median ms | Final median ms | Final / baseline |
| --- | ---: | ---: | ---: |
| regex_compile | 21.565 | 22.963 | 1.065× |
| named_timezone | 0.302 | 0.311 | 1.032× |

Caching the timezone tail reduced the initial packed implementation’s approximately 37% overhead to approximately 3% on this case. Unicode property decoding adds approximately 6% to regex compilation here; matching retains its original representation.

These are screening measurements, not a comprehensive performance ranking or
a promise for every workload. The steady-state harness uses three warmups and
nine samples per case. The table benchmark uses five warmups and 21 alternating
paired samples, with fresh regex pattern text to avoid merely timing a cache.
It validates results before retaining measurements.

The full builds deliberately keep optimization level 3. A parser-only
`opt-level="s"` experiment saved just 24,137 raw / 3,356 Brotli bytes in the
Python-base artifact. A narrow warm Python initialization comparison was
6.691 versus 7.191 ms for a small program but 8.983 versus 8.772 ms for a
200-function program. That is not enough evidence to change the production
parser profile. A regex-only `s` override actually grew the JavaScript WASM
by 5,155 raw / 9,233 Brotli bytes, so it was rejected.

Binaryen 125 `wasm-opt -Oz` on an earlier Lite candidate reduced raw bytes from
3,076,218 to 2,802,310 but increased Brotli from 779,710 to 806,703. It is not
part of the standard build: browser transfer size gets worse. The script's
post-processing remains section stripping and compression.

## What still makes the engine large

Twiggy's baseline JavaScript **shallow** attribution found approximately:

| Area | Bytes |
| --- | ---: |
| Other VM code, including dispatch and built-ins | 1,951,208 |
| Data segments | 1,170,831 |
| Rust/dependency code not attributed to other rows | 662,906 |
| JavaScript compiler | 453,282 |
| Regex engine | 440,307 |
| JavaScript parser | 345,926 |
| Temporal code | 244,612 |
| Intl code | 179,867 |
| Heap, shapes and GC | 174,075 |
| WASM host/glue | 71,187 |

These are not independently removable sizes. Inlining hides some ownership,
data is not attributed to its owner, and generic symbols group work from
multiple subsystems. Debug names used for analysis are stripped from shipping
sizes. The baseline interpreter dispatch alone is about 279 KB and native
dispatch about 133 KB. Deleting unused source files or shortening identifiers
does not remove these costs; linker reachability and code generation matter.

The next substantial **same-functionality** work should be:

| Priority | Change | Why it matters / acceptance test |
| --- | --- | --- |
| 1 | Reduce duplicated generic regex control flow | Baseline repeated `with_scm_loop_impl`, lookaround and `try_at_pos` bodies total roughly 215 KB. Share control flow while retaining ASCII, UTF-16 and Unicode-scalar indexing semantics; compare matching throughput and the Unicode/backreference/lookbehind corpus. These bytes are a target area, not a promised saving. |
| 2 | Factor cold interpreter and built-in paths | Large dispatch functions contain repeated error handling and coercion paths. Keep hot cases inline, share cold code, then measure raw size, Brotli and dispatch throughput. Blanket `inline(never)` or size flags can lose too much speed. |
| 3 | Consolidate overlapping Unicode support | Identifier classification, normalization, case conversion and regex properties have separate tables. Establish Unicode-version alignment before sharing any data; exhaustively compare all scalar values and preserve surrogate behavior. |
| 4 | Share immutable program storage between instances | Function bytecode and retained source can be shared, while mutable inline caches and realm state remain instance-local. This primarily reduces live memory, not the WASM download. |
| 5 | Explore a compact bytecode representation for the interpreter | Keep the JIT's richer representation where needed; use measured instruction distributions to choose operand widths. This is an architectural project with decode-speed, debug-position and GC implications, not a safe one-line optimization. |

Already absent from browser WASM: native JIT machine-code generators and the
filesystem loader. Already applied: fat LTO, one codegen unit, section stripping,
and Python source comment stripping. Removing them again is not another saving.

## Python frontend and runtime findings

Python's cost is not mostly the parser. Baseline Python-base adds 1.724 MB raw /
377 KB Brotli to JavaScript. Adding built-in Torch adds another 1.959 MB raw /
347 KB Brotli. Shallow parser code is approximately 141 KB, the arena-AST adapter
45 KB, Python lowering 311 KB and Python VM helpers 183 KB; embedded library
source accounts for much of the remaining difference.

The stripped Python-base source bundle measured 757,051 bytes; its largest
members were `runtime/stdlib.js` (227,498), `builtins.js` (139,615), `core.js`
(106,490), `types.js` (84,730) and `zipp_gpu.py` (87,560). The Torch source bundle
measured 1,747,973 bytes, led by `torch.py`, distributions, tensor.js, nn and gpu.
Comment stripping already preserves source positions and docstrings; removing
docstrings or source observable through reflection would change functionality.

Priority follow-ups:

* **Share the cached runtime seed's immutable code.**
  `frontend/python/mod.rs::runtime_seed` clones an owned seed for each program.
  `FuncProto` owns a `Vec<Instr>` and source strings. Use shared immutable code
  and source slices, with per-VM mutable caches kept separate. Avoid sharing
  heap IDs, globals, feedback or realm objects. Validate multi-instance
  isolation, GC accounting, eviction and debugger/source behavior.
* **Use source spans into one shared source allocation.** JavaScript nested
  functions also copy source through `compile/compiler.rs::src_slice`.
  `Function.prototype.toString` must still return exact source. Shared spans
  can reduce retained memory without deleting reflection.
* **Lower directly from the Python parser's arena.** The current adapter builds
  another AST before emission. Replacing it can reduce peak parse memory and
  approximately 45 KB of adapter code, but requires preserving scope analysis,
  syntax-error locations and existing frontend golden tests.
* **Measure compressed embedded sources with lazy decoding.** A prototype
  reduced base source to 174,585 gzip bytes and Torch source to 367,414.
  Those are source-only numbers, not achievable WASM or wire savings: a decoder
  costs code and memory, and the transport already compresses the whole module.
  Do not eagerly decompress all modules or count the gzip ratio as a Brotli win.
* **Keep Torch optional where appropriate.** The existing `python` plus separate
  `zipp_torch.wasm` is already the correct route for hosts that do not always
  need Torch. Standard `all` remains available with everything included.

For a trivial `print(42)` program, the resource meter reported baseline heap
bytes 399,298 / 2,652,585 / 2,746,712 for JS / Python-base / all respectively;
program bytecode bytes 20,256 / 2,694,496 / 3,445,440 and retained source bytes
5,678 / 1,275,445 / 1,616,858. These are separate accounting categories, not RSS
and not figures to blindly sum. They make immutable seed/source sharing a more
promising per-instance memory project than swapping the parser.

The same-functionality table changes implemented here benefit both Python
variants through their shared VM. No Python syntax or library was removed.

## QuickJS comparison

QuickJS's [homepage](https://bellard.org/quickjs/) advertises 367 KiB of x86
hello-world code; its [manual](https://bellard.org/quickjs/quickjs.html) still
says 210 KiB and states that ECMA-402 is unsupported. Neither number is a full
WASM embedding or a Brotli download. Its optional compiler feature stripping
also changes the comparison. Do not label either figure a like-for-like target.

The repository's WASM README records QuickJS-NG 0.16.2's official reactor at
1,528,293 raw / 417,087 Brotli bytes. That is a useful historical target, not a
fresh same-toolchain build. Lite gets substantially closer but remains about
twice its raw size and 1.9 times its compressed size. Reaching parity while
retaining JavaScript core needs the shared-code work above. No matched speed
or standards-compliance victory is claimed.

## Validation

* Full VM: 498 unit tests passed, one ignored. All five timezone tests passed,
  including exhaustive decoding, partition search and cached-tail checks.
* Regex: all unit and integration suites passed with UTF-16 and the hardened
  profile, including the 367-table round trips; no-default-features/no_std check passed.
* Full JavaScript WASM: all 28 production host-boundary suites passed.
* Lite WASM: all 28 production host-boundary suites passed, including the new
  feature contract, hostile syntax corpus, resource limits, callbacks and host bridge.
* Python-base: 57 frontend checks, 16 app-bridge checks, profile and full-feature
  contract checks passed. The GPU fixture's Torch portion requires the `all`
  artifact; it is not applicable to Python-base without loading the Torch package.
* Python + Torch: frontend, app bridge, GPU and training suites passed, including
  hosted/chained training comparisons with the checked-in PyTorch oracle.
* All four final modules passed the host-import and linked-memory inspection.
  Combining `lite,python` was correctly rejected at compile time.
* Symbol inspection confirms no timezone database or ZonedDateTime formatting
  symbols remain in Lite. Shared ordinary Date parsing helpers remain available.

The work does not claim a fresh complete test262 run. The baseline native
Windows CLI audit was 8,742,400 bytes for JavaScript + JIT without Python/GPU,
and 17,339,904 bytes for the default Python/GPU/JIT CLI; those are not the
same artifact as QuickJS's native hello-world example. Native CLI sizes were
not used to claim the WASM savings.
