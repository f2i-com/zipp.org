# Cost census, 22 October 2026

WP06 of the 22 October implementation plan: what the engine's memory and time
go to on real workloads, before choosing a structural performance experiment.
The raw evidence is `target/bench-results/impl-plan-2026-10-22/census/`
(`cost-census.json` and one census per workload; ignored, not committed).

## Method

- **Storage:** `zipp` built with the `census` feature (off in every shipped and
  timed build) and run with `ZIPP_CENSUS=<file>`. At the end of the run it
  walks the VM once and records, at capacity, each function's code and tables
  (counted once however many lists name it), the interpreter's and the JIT's
  inline caches, the frames and registers, the heap's object table, payload
  estimate and objects by kind, and the JIT's machine code
  (`crates/zipp-vm/src/vm/census.rs`).
- **Denominator:** the process's peak working set, read with
  `GetProcessMemoryInfo` after it exits.
- **Time:** `ZIPP_PROF=1` phase sampling on the release build of `1aecb40b`
  (`target/bench-binaries/audit-new-1aecb40b.exe`), one run each; the shares
  are of samples, process start included.
- **Workloads:** the seven bundled applications (TypeScript, Babel and
  Prettier at their published sizes, `misc`, and the Vue, React and Preact
  server renderers), the 13 `bench/real` rows, and the six application-shaped
  Python programs (`tools/python_bench/macro`, torch excluded).
- **Machine:** Windows x86-64, Rust 1.92.0, the same quiet machine as the
  day's other captures.

## Layout

| Type | x86-64 | wasm32 |
|---|---:|---:|
| `Value` | 8 | 8 |
| `Instr` | 32 | 32 |
| `FuncProto` | 360 | 196 |
| `FusedCode` | 72 | 36 |
| `Frame` | 88 | 72 |
| `SiteIc` (8 ways) | 584 | 584 |
| `IcEntry` | 72 | 72 |
| `HeapObj` | 80 | 64 |

wasm32 sizes come from a check of the browser module's own feature set
(`safe-sandbox,meter-only,wasm-no-fs-loader,wasm-single-agent,wasm-compact-regex`)
with a temporary size probe, removed afterwards.

## What the large applications hold

Shares of peak working set; time shares are of `ZIPP_PROF` samples.

| Workload | Peak WS | IC | One-way sites | Instructions | Source text | Heap table | Parse + compile | GC |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| TypeScript | 349 MiB | 2.6% | 92% | 9.3% (32.6 MiB) | 13.1% (45.7 MiB) | 11.5% | 29.8% | 2.3% |
| Babel | 250 MiB | 3.4% | 83% | 6.9% | 7.6% | 16.0% | 3.0% | 6.2% |
| Prettier | 159 MiB | 4.8% | 91% | 9.4% | 2.8% | 12.6% | 20.3% | 6.0% |
| misc | 212 MiB | 2.5% | 79% | 4.1% | 2.1% | 9.4% | 9.6% | 6.4% |

On the 13 `bench/real` rows every one of these is under 1%, except the heap
table (up to 49% on `async-promise-chain`) and GC (up to 14%). The Python
programs hold 2.4–3.4% in source text, 2.7–4.1% in inline caches, and read
their compiled code from the code cache (no parse or compile samples). The
four large applications ran with the JavaScript JIT off (256 functions or
more), so the JIT holds nothing there; the most any workload's machine code
took was 0.3 MiB.

## Candidates, ranked

1. **Per-function source text (not in the plan): selected.** Every function
   keeps its own copy of its source for `Function.prototype.toString`, so a
   nested function's text is held again by every enclosing one: 45.7 MiB on
   TypeScript, 4.6 times its 10 MB bundle, and 6.4% of peak working set on
   average over the four large applications. *Experiment A:* hold each
   function's text as a range of one shared copy of its script, with
   `toString` and `FuncProto`'s size unchanged. Gate: code storage at least
   20% lower on TypeScript and Prettier, peak working set lower by at least
   5% on TypeScript, identical output, Test262 unchanged.
2. **WP12, a startup cache: selected, as a measurement first.** Parsing and
   compiling take 29.8% of TypeScript's samples and 20.3% of Prettier's.
   *Experiment B:* time encoding and decoding those two programs with the
   existing bytecode codec against their parse and compile; build the cache
   only if loading, validating and instantiating cost at most a third of
   parse and compile, so the plan's 10% first-output gate can be met.

   *Measured* (release build, in process, median of five; a temporary probe,
   not committed). Decoding costs a twelfth of parse and compile, well inside
   the gate:

   | Bundle | Parse | Compile | Encode (once) | Decode | Encoded size |
   |---|---:|---:|---:|---:|---:|
   | TypeScript | 179.8 ms | 319.9 ms | 439.3 ms | 39.3 ms | 7.4 MB |
   | Prettier | 61.3 ms | 90.6 ms | 37.5 ms | 13.1 ms | 3.4 MB |
   | Babel | 82.2 ms | 129.7 ms | 99.8 ms | 16.5 ms | 4.0 MB |
   | misc | 38.1 ms | 57.2 ms | 30.7 ms | 7.5 ms | 2.2 MB |

   A cache for `zipp js` is therefore worth building, behind the plan's
   conditions (a key over the engine build, features, source and options;
   no cache in a sandbox profile; bounded decoding). Asked for it, the
   command keeps one by default, as `zipp py` does (see Results).
3. **The heap object table.** 9–16% of peak working set on the applications
   (80-byte slots, grown by doubling, counted at capacity). It is the GC's own
   structure; a later, separately designed experiment, not one to start
   under a census.
4. **Instruction storage (WP11): rejected for now.** Instructions and their
   fused copies are 7.4% of peak working set on the large applications, so
   the plan's 25% packing gate is worth about 1.9% for a high-risk change of
   the interpreter's representation. One part is nearly free: 1.7% is `Vec`
   capacity left after compiling (TypeScript's 662,162 instructions use
   20.2 of the 28.5 MiB allocated for them), which a shrink after
   compilation would return.
5. **Inline-cache storage (WP07): deferred.** 3.3% of peak working set on the
   large applications; 79–92% of sites fill one way of eight, and holding
   those compactly would save about 2.1%. It meets the plan's 20%-of-IC gate
   on paper but is small in absolute terms; it waits behind A and B.
6. **Calls (WP08), JIT policy (WP10), Python helpers (WP09):** not decided by
   this census. Earlier stack profiles (21–22 October) put TypeScript's
   `setup_call_lean` at 4.7% and its dispatch match at 4.2%; the JIT-policy
   ledger is negative on every bundle; `ZIPP_PY_PROF` was not run here.

## Results

Both experiments were built: `176832e7` (A) and `63f2e776` (B). Raw
evidence is in `target/bench-results/impl-plan-2026-10-22/`
(`expA-census.json`, `expB-cache-states.json`).

**A, one shared copy of each script's source.** A function's or class's
text is now a range of one shared copy of the script it was compiled from
(`SourceText`, the size of the `String` it replaces). The bytecode codec
stores those ranges without searching for them, so encoding TypeScript's
program went from 439 ms to 15 ms. Census builds of `b7b118f5` and
`63f2e776`, cache off, peak working set the median of three:

| Bundle | Source text | Code and source text | Peak working set |
|---|---:|---:|---:|
| TypeScript | 45.7 → 9.9 MiB | 94.2 → 58.4 MiB (−38.0%) | 349 → 310 MiB (−11.3%) |
| Babel | 19.0 → 4.4 MiB | 46.6 → 32.0 MiB (−31.3%) | 250 → 238 MiB (−4.8%) |
| misc | 4.4 → 2.0 MiB | 17.4 → 15.0 MiB (−13.9%) | 212 → 186 MiB (−12.1%) |
| Prettier | 4.4 → 2.2 MiB | 26.4 → 24.1 MiB (−8.5%) | 159 → 161 MiB (+1.0%) |

The gate holds on TypeScript and not on Prettier, whose copies were only
twice its bundle: there was less to share. Output is identical; Test262's
function, class, method, arrow, eval-code and Annex B directories pass in
both modes, and the decoded program prints (`{:?}`) exactly as the compiled
one for every bundle, benchmark row and test script (69 files).

**B, the `zipp js` cache.** `zipp js` (and `zipp file.js`) loads a script
of 32 KiB or more from the code cache instead of parsing and compiling it,
and after a compile stores it from a thread of its own while the program
runs. Same folder, keys and bounds as `zipp py`'s; `ZIPP_CACHE_DIR` set but
empty turns it off. Benchmark rows (10 KB at most), Test262, harness runs,
PGO training, modules, embedders and sandboxes never use it. Release builds
of `1aecb40b` (base, the same engine as `b7b118f5`) and `63f2e776`, median
of five, interleaved; *off* is the new build with the cache off, *miss* a
fresh folder every run, *hit* a folder one run filled:

| Bundle | Base | Off | Miss | Hit | Peak working set, hit |
|---|---:|---:|---:|---:|---:|
| TypeScript | 1,174 ms | 1,145 ms | 1,185 ms | 838 ms (−29%) | 349 → 190 MiB |
| Prettier | 545 ms | 550 ms | 549 ms | 453 ms (−17%) | 159 → 116 MiB |
| Vue SSR | 91 ms | 90 ms | 92 ms | 74 ms (−19%) | 48 → 41 MiB |
| misc | 809 ms | 811 ms | 809 ms | 746 ms (−8%) | 212 → 173 MiB |
| React SSR | 71 ms | 72 ms | 72 ms | 66 ms (−7%) | 54 → 46 MiB |
| Preact SSR | 71 ms | 70 ms | 73 ms | 68 ms (−4%) | 39 → 36 MiB |
| Babel | 5,050 ms | 5,045 ms | 5,067 ms | 4,953 ms (−2%) | 250 → 171 MiB |

Times are to the first byte of output. A hit skips the syntax tree as well
as the compile, which is most of the memory it saves. A miss costs about 3%
on TypeScript (the encoding, on another core). Every run's stdout and exit
status matched the base's (React SSR prints its own timing to stderr). The
plan's gate, first output 10% sooner on TypeScript and Prettier, holds.

**Speed elsewhere.** Neither change runs inside the interpreter's loop, and
the geomeans say so; single rows move both ways by up to 5%, the code
placement of a build that grew (the JavaScript-only build carries the codec
now: 9.08 → 9.34 MB).

| Comparison | Geomean, new / base |
|---|---:|
| Canonical PGO, interpreter only, 51 rows, 5 reps | 1.005 |
| Canonical PGO, JIT, 51 rows, 5 reps | 0.997 |
| Release, real13 with the JIT, 12 paired reps | 1.002 [0.996, 1.010] |
| Release, real13 with `ZIPP_NOJIT=1`, 10 paired reps | 1.020 [1.017, 1.024] |
| Release, Python suite, 44 programs | 1.01 |

At nine reps, the rows that moved most were branch-control 1.050,
reactish-reconcile 1.046 and class-features 1.024 against sparse-array
0.950 and collections-map-mixed 0.964. The new canonical build's
interpreter takes 0.745 of QuickJS's time over the 51 rows and is faster on
40 of them.

## Not run

- The research reports' 21-case R1 corpus: not supplied with the plan.
- ARM64; browsers (the wasm32 figures are layouts only, not a census of the
  browser module at run time); Worker reuse.
- Call counts, register traffic and hardware counters: the census adds no
  hot-path counters by design.
