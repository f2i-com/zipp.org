# Zipp performance roadmap

> **Goal:** become faster than Node, Bun, and Deno on every maintained
> benchmark while preserving exact JavaScript semantics and tier parity.

This file is the current performance board. The append-only B001–B252 ledger,
including the negative results and original line-level evidence, is archived at
[`docs/archive/PERF_LEDGER-B001-B252.md`](docs/archive/PERF_LEDGER-B001-B252.md).
Search that file before reopening an older idea.

## Evidence legend

| Label | Meaning |
|---|---|
| **CANONICAL** | Clean, HEAD-matching, provenance-stamped PGO capture with the complete required engine/suite protocol and `publishable:true`. May update public Node/Bun/Deno ratios. |
| **DIAGNOSTIC A/B** | Focused, filtered, same-binary, dirty-comparator, non-PGO, or incomplete-engine evidence. Useful for decisions; never a public engine score. |
| **LANDED** | Default-on code is committed and pushed after its correctness and performance gates. |
| **VERIFIED** | Mechanism and result are established; the surrounding campaign or public capture may still be pending. |
| **REFUTED** | Built or measured and rejected. Do not retry without new evidence that invalidates the kill reason. |
| **REVERTED** | Experimental code is absent from the default tree. |

Cold wall time is the headline. Startup-adjusted time is diagnostic, especially
on short cases. Ratios are new/old for binary A/Bs and Zipp/competitor for engine
tables; below `1.0×` is faster.

## Current status — 2026-09-02

### Public canonical capture

The public score is the clean four-engine PGO capture at `c28781cf`:

| Corpus | Node | Bun | Deno | Node point wins |
|---|---:|---:|---:|---:|
| retained ten | **0.886×** [0.880, 0.892] | 0.757× [0.752, 0.764] | 0.770× [0.761, 0.778] | 8 / 10 |
| diagnostics three | **0.189×** [0.186, 0.192] | 0.171× [0.169, 0.175] | 0.149× [0.146, 0.153] | 3 / 3 |
| all 13 | **0.620×** [0.616, 0.624] | 0.537× [0.534, 0.542] | 0.527× [0.521, 0.532] | 11 / 13 |
| hostile all 17, ordinary | **0.824×** [0.800, 0.838] | 0.657× [0.648, 0.666] | 0.429× [0.422, 0.436] | 10 / 17 |
| hostile category-balanced | **0.860×** [0.834, 0.873] | 0.676× [0.663, 0.683] | 0.442× [0.435, 0.449] | — |
| all 30, equal row weight | **0.729×** [0.716, 0.736] | 0.602× [0.597, 0.607] | 0.469× [0.464, 0.474] | 21 / 30 |

Both source artifacts are `publishable:true`, `ALL_CORRECT=1`, use 15
counterbalanced repetitions and 10,000 bootstrap samples, and have empty drift
and failure lists. The all-30 interval is a stratified descriptive bootstrap:
normal and hostile repetitions are resampled independently, while every row
inside one suite shares the same repetition indices. Every aggregate is a
series best; the previous canonical pair (`b65aa353`, earlier on 2026-09-02)
read 0.903× / 0.187× / 0.628× / 0.836× / 0.870× / 0.739×, and `21288c1`
(2026-08-30) 0.921× / 0.192× / 0.642× / 0.881× / 0.913× / 0.768×.

Across Node, Bun, and Deno, the normal suite has 33 / 39 point and 29 / 39
exact-sign wins; hostile has 39 / 51 point and 34 / 51 exact-sign wins. The
literal all-row target remains false. See
[`README.md`](README.md#performance-measured-honestly) for the tables and raw
artifact links.


### Historical v0.0.5 QuickJS-NG and Boa diagnostic

The release-default native executable at engine commit `7cb7210` reports
v0.0.5, the exact source commit, `dirty:false`, `opt-level:3`, default features,
and no PGO or ad-hoc Rust flags. Six counterbalanced repetitions with exact
output and 10,000 bootstrap samples measured:

| Suite | Comparator | Zipp / comparator | point wins |
|---|---|---:|---:|
| native interpreter real13 | QuickJS-NG v0.16.2 | **0.6413×** [0.6386, 0.6452] | 12 / 13 |
| native interpreter micro5 | QuickJS-NG v0.16.2 | **0.8556×** [0.8405, 0.8761] | 5 / 5 |
| native interpreter micro5 | Boa v0.22.0 | **0.2539×** [0.2501, 0.2590] | 5 / 5 |
| WASM adjusted execution | QuickJS-NG reactor | **2.1074×** | 0 / 5 |
| WASM adjusted execution | Boa package | **0.2274×** | 5 / 5 |

The one native real13 QuickJS-NG point gap is sparse-array at 1.0099×. The
v0.0.5 WASM artifact is 5,595,833 bytes raw / 1,254,075 Brotli-11, versus
1,528,293 / 417,087 for the QuickJS-NG reactor and 21,296,176 / 5,484,164 for
Boa. The WASM interfaces and feature sets differ, so this is diagnostic
attribution rather than a universal engine ranking. Exact commands and hashes
are in [`bench/comparison/README.md`](bench/comparison/README.md).

### Current v0.0.6 native QuickJS-NG confirmation

The clean default-feature release binary at engine-source commit `e3acee352074`
reran the current real13 protocol for six counterbalanced rounds with
`ZIPP_NOJIT=1`. All canonicalized output matched after documented QuickJS
CRLF-to-LF normalization, and Zipp led QuickJS-NG v0.16.2 on all 13 point
medians. Cold Zipp / QuickJS-NG was `0.6089665×` [0.6072021, 0.6122180];
startup-adjusted was `0.6058409×` [0.6041440, 0.6090422], with descriptive 95%
intervals. This closes the historical sparse-array point gap for the current
native engine source. It remains diagnostic native CLI evidence and does not
imply a WASM win.

### v0.0.6 exact-suite WASM status

The committed production WebAssembly module is 5,558,860 bytes raw, 1,812,458
gzip-9, and 1,248,649 Brotli-11, SHA-256
`bd8614fe5f3a3b8ef67f4b917cdefebb3fe69afa39a9804a0d3f6b0b6b267126`.
QuickJS-NG's official reactor is 1,528,293 raw / 417,087 Brotli-11, so Zipp is
`3.586×` as large raw and `2.958×` as large at Brotli-11.

The clean six-round attempt over the exact, unscaled current v0.0.6 normal 13
and hostile 17 sources used by the v0.0.6 Node/Bun/Deno reruns is
`target/comparison/results/wasm-suites-v006-c77829269703-final-6.json`. It has
clean provenance and complete capture attempts, but records
`publishable:false`, `capture_usable:false`, and `evidence_usable:false`:

- the production Zipp WASM API cannot load the two module rows;
- the official QuickJS-NG reactor cannot drain pending jobs for three async rows;
- Zipp validated 7 / 28 script rows. Fourteen stopped at its fixed instruction
  budget, three at its fixed approximate-heap budget, and four at other engine
  errors.

There are no comparable normal-suite rows and only five comparable hostile
rows. Their available-only Zipp / QuickJS-NG geomean is `0.9604×` persistent
and `0.9567×` adjusted, with one Zipp point win. QuickJS-NG leads
`shapes-stable` (`1.2811×` adjusted), `allocation-ephemeral` (`1.0482×`),
`allocation-survival` (`1.0732×`), and `reactish-reconcile` (`1.1694×`); Zipp
leads `warm-router` (`0.4755×`). These values are row-level diagnostics, not
complete suite aggregates. QuickJS-NG has the faster separately sampled compile
median (1.795 ms versus 5.080 ms), while Zipp has the faster instantiation/start
median (0.397 ms versus 1.676 ms). The sums of those phase medians are not
measured end-to-end times.

The prior five-workload speed-kernel experiment measured `0.0954663913×`
QuickJS-NG persistent, but it is specialization-sensitive: the exact-lanes-off
control measured `1.815×` QuickJS-NG persistent, and three adjusted rows sat at
the subtraction noise floor. That dirty-tree control also measured `0.1992×`
Boa persistent with five point wins, but no Boa run exists for the current
normal-13-plus-hostile-17 WASM inventory. Preserve these as mechanism
attribution, never as general WASM speed headlines.

The highest-value WASM work is therefore coverage before aggregate tuning:
provide a separately explicit comparison configuration without weakening the
production safety defaults, fix the async trap and string/typed-array capacity
failures, then optimize the four measured gaps. Do not claim a suite win until
all cross-engine-supported rows complete with exact output.

### Correctness

- test262: **95,939 / 95,942** required executions (99.997%)
- expected failures: one stale Annex B assertion and two German-CLDR rows
- former errored-module-cycle and deferred top-level-await gaps: fixed
- default JIT, interpreter-only, forced-JIT, and majors-only-GC identities are
  the conformance gate
- tier-differential fuzzing remains mandatory for JIT work
- 2026-09-02: the forced-JIT identity over the benchmark corpus found a
  segfault the default threshold hid — the B205 random-scale fuse read the
  versions table through an unpinned r13 when its Tier-C body was entered from
  a CROSS3 caller (`nanoid` with a user `Math.random`); fixed in 18249988 by
  deriving the base through the VM-mirrored `versions_raw`. Run the four-mode
  output identity over every `bench/` and `tests/syntax-corpus` program (about
  170 runs, two minutes) before merging any emitter change; a `refetch_pinned`
  audit is the follow-up, since every r13 reader is enumerated by bytecode shape.

Do not edit historical B entries merely because their old pass counts were
correct for their own commit.

## Latest experiment registry

### B269 LANDED — RegExp exec under a heap ceiling stops auditing the heap

The safe-sandbox profile sizes every RegExp exec's match limits from the
remaining heap headroom, and that headroom read `heap_bytes()` — the exact
audit walk over the whole heap — on EVERY exec. The spec `split` loop runs a
sticky exec at almost every character, so `text.split(/[^a-z]+/)` over a
400 KB string (~450,000 execs) took 208 s in the browser build against 44 ms
natively; the landing page's text-processing example hit its deadline. The
per-exec paths (`instrument_regex_limits`, the transient charge) now read the
heap's O(1) resident figure plus the non-heap remainder the last exact audit
cached (`heap_bytes_estimate`), so a ceiling set from the exact figure convicts
at the same byte and only heap objects that grew in place wait for the strided
preflight audit or a host-boundary read. Native safe-sandbox runner: 10,000 sticky execs 2,004 →
230 ms; 60,000 global-exec matches from a 5 s timeout to 416 ms; the browser
build's split 208 s → 5.9 s. `tests/regex_exec_heap_estimate.rs` pins exact
counts and a generous wall-clock bound under a 256 MiB ceiling. Still open: the
remaining ~13 µs per sandbox exec (4 µs natively).

### B268 NULL — `JSON.parse` object birth from a recycled shell

Built and measured, not landed. `JSON.parse` built each object straight into a
recycled shell reset to an empty owned-key map, with key buffers from the key
pool and a settled allocation (no pairs buffer, no fresh `Box`). One-binary
latch A/B, 21 pairs: json-large +0.5% [−1.4, +2.1], controls null; exact
output, tests in nine modes, 188-run identity sweep clean. `ZIPP_GCSTATS`
told why before the numbers did: `[objpool] pushed=0 popped=0` on json-large —
a dying JSON object has OWNED keys and the pool admits only planned-key shells,
so the recycled box was never there to take — and `[keypool] served=8192
missed=436726`: the key pool's 8,192 cap is a small fraction of one parse
cycle's keys, which die together at a collection. The lever for json-large's
allocator share (~6%) is therefore pool admission for owned-key shells (drain
the keys, keep the box) plus a demand-sized key pool, not the parser. Reverted.

### B267 LANDED — `(x + y) | 0` fuses into the wrapping add in Tier C

`ZIPP_NO_TRUNC_OR_FUSE=1` restores the three-op emission. A truncation-only
`Add`/`AddInt` (B260's wrapping add) followed by `LoadInt 0; Bitwise Or` is
the `| 0` idiom; on the Int path the add now stores its wrapped Int into the
Or's destination (and the boxed 0 into the literal's register, so a later
deopt sees the same frame) and jumps past both followers. The f64 and concat
paths fall into the unchanged code. Admitted only when neither follower is a
jump or handler target and no step meter charges blocks. Gate (2026-09-02,
one binary, 16 pairs): calls-closures **−4.1%** [−6.7, −2.4] (two-binary
−3.9% [−4.9, −2.9]); bytecode-vm, typedarray-math, parse-large-js,
shapes-stable, json-large, warm-router null. `tests/trunc_or_fuse` is
node-oracled over both operand orders, doubles, strings/objects/undefined
operands, a deopt right after the triple and a loop-carried chain, in six
modes; 188-run identity sweep clean.

### B266 NULL — cross-function interning of equal string literals

Built and measured, not landed. Equal literals in different functions shared
one heap string through a content-keyed table beside the per-slot constant
cache (interpreter and JIT constants alike, gated by the same per-function
shareability rule), so the warm router's route names would compare by
identity in `Map.get`. One-binary latch A/B over all 21 rows: warm-router
−1.2% [−2.7, +0.1], every other row null, exact output. The router's Map
lookups are not bounded by the byte compare; the cost sits in the intrinsic
proof, the index lookup and the key hashing around it. Reverted.

### B265 REFUTED — slot resurrection (refit a dead literal shell in its slot)

Built and measured, not landed. A pool-eligible literal shell stayed in its
free slot at death (no tombstone write, no pool push) and the next literal was
refit where it lay; a non-literal reuse evicted the shell into the pool. A
first-N-deaths budget starved the mechanism (the LIFO free list buried the
kept shells under later tombstones: shapes-stable kept 4,096, refit 0); a
demand budget with a FIFO trim fixed the serve rate (refit 589k of 604k) and
then lost: one-binary latch A/B allocation-survival **−4.8%** but shapes-stable
**+15.7%**, shapes-megamorphic **+13.7%**, reactish-reconcile **+9.9%**,
warm-router +2.0% (two-binary +6.5%); every normal row null; exact output and
a 188-run identity sweep clean. B239's in-place refit from the pool had
already removed the cost this design targeted; the per-death flag and queue
writes and the eviction traffic on mixed rows cost more than the tombstone and
pool round trip they replaced. The birth/death pipeline's remaining cost is
not in slot bookkeeping. Do not re-derive this.

### B264 LANDED — inline pinned dense-Array store lane in MEM regions

`ZIPP_NO_INLINE_DENSE_STORE=1` restores the helper-only route. Every `a[i] = v`
on a pinned dense Array in a MEM region was a `jit_set_index` helper call
(3–5% of the shape rows) plus a barrier check. The lane stores directly when
the receiver matches its pin snapshot, the key is an in-range integer (never an
append), the holder is YOUNG (its generation byte, read through the new
`Heap::gen_raw` mirror, has no state bits — a young holder needs no barrier in
either barrier mode), and the element is present or the hole-fill is licensed
by the snapshot flags (`TA_SNAP_INDEX_ABSENT` and the new
`TA_SNAP_LEN_WRITABLE`, exactly the helper's `creates_new_index` deopt
conditions). Everything else takes the helper byte-for-byte as before; an
in-range store never moves the Vec, so no snapshot refetch follows. The lane
is not emitted with the nursery off or the GC oracle on.

Gate (2026-09-02, one binary, 16 interleaved pairs, exact output):
sparse-array **−12.1%** [−13.0, −11.8], async-promise-chain **−6.0%** [−6.6,
−5.2], shapes-stable **−7.2%** [−10.9, −4.8], shapes-megamorphic **−5.1%**
[−5.9, −4.1], allocation-survival −1.7% [−4.8, +1.4] (two-binary −5.1%),
every other row inside its interval. Correctness: `tests/inline_dense_store`
(node-oracled hole fills, appends, an OLD holder across minors, a non-writable
`length`, an indexed `Array.prototype` setter, a custom prototype, fractional
and string keys) in nine modes including `ZIPP_NURSERY_VERIFY=1` and
`ZIPP_GC_STRESS=1`; the nursery, finalize, pool and tier suites; every feature
configuration; a 188-run four-mode output identity against node.

### B263 LANDED — register classes: booleans and global receivers leave the scratch stack

`ZIPP_NO_REG_CLASSES=1` restores the v0.0.5 allocation. The v0.0.5 release
(`7cb72106`) started reclaiming scratch registers across statements, and the
INT region tier — which types every register once per loop and pins a
global-loaded receiver only when its register has exactly one definition — then
declined whole tokenizer loops: a `LoadGlobal src` slot reused for a literal, an
`Eq` result slot reused for a number ("type conflict on a reused register").
parse-large-js went from 44 INT regions to 9 and 251 → 396 ms (0.89× → 1.24×
node). Now `expr()` places a syntactically boolean expression in a BOOL class
register and a global-identifier receiver in a RECV class register: provisional
numbers above the ordinary stack, never reclaimed, renumbered to the top of the
frame at finalisation by the generated, exhaustive `compile/remap.rs` (the
`NO_REG` / `BARE_MATH_BY_NAME` sentinels are left alone). Ordinary scratch keeps
the v0.0.5 reclaim; a per-register kind history keeps numeric temporaries off
boolean slots and places argument windows where no slot conflicts. On the
planner side the DataView endian-flag fusion now admits a DEAD flag register
(no other definition, no other use, every outside read preceded by its own
definition) and its use scans skip the fused flag operand — without this the
swizzle loop's flag slots became real Bool homes and the region lost two GPRs.
Rejected with numbers: monotone allocation inside loops (bytecode-vm +434% from
xmm/GPR pool exhaustion, regex-log-scan frames 76 → 256 registers) and one
shared multi-definition receiver register per global (parse-large-js worse than
base). New diagnostics: `[jit] INT-GPR region … guard kept: first guarded op`
under `ZIPP_JITLOG`, and `ZIPP_ABSINT_LOG=1` (+ `ZIPP_ABSINT_GLOB=<slot>`) for
the interval prover.

Gate (2026-09-02, base `37c7fbfa` vs the new binary, 16 interleaved pairs,
exact output): parse-large-js **−38.9%** [−39.3, −38.1], typedarray-math
**−5.4%** [−6.7, −4.3]; every other normal row within ±1.3% and inside its
interval; hostile eight-row geomean −0.2% [−0.9, +0.6] (bytecode-vm −2.3%,
async-lived +3.4% two-binary / −0.7% one-binary). The one-binary latch A/B
reproduces both wins (−39.0%, −5.7%). Correctness: 534 lib tests, the compiler
and tier suites (`reg_classes`, `jit_tier_parity`, `jit_tier_fuzz`,
`typeof_alias`, `int_split`, `int_gpr_homes`, `int_splice`, `local_sroa`,
`double_mod`, `real_program_corpus`), every feature configuration, and a
188-run four-mode output identity against node over every bench and
syntax-corpus program. Canonical PGO capture at `b65aa353` (2026-09-02): parse-large-js **0.875× Node** (was 1.239× at `37c7fbfa`, 0.89× at `21288c1`), typedarray-math 0.729×, retained ten **0.903×** [0.899, 0.908], all 13 **0.628×**, hostile 0.836×, all 30 **0.739×** — every aggregate a series best; see the current-status table.

Follow-up (2026-09-02): the interpreter-only array numeric callback classifier (`vm/array_ops.rs`, `cfg(not(jit))`) matches exact bytecode shapes, and the boolean class renumbered `x => x % 3 === 0` so its compare result lands in the last register; the FilterMod3 shape was updated to what the compiler emits, which had left the security workflow's safe-sandbox lib test red and the browser build's mod-3 filter lane unengaged since 07b400dc.
### B262 LANDED — `typeof` aliases fuse into `TypeOfIs`, answered inline from the tag

`ZIPP_NO_TYPEOF_ALIAS=1` (compiler) and `ZIPP_NO_TYPEOF_IS_INLINE=1` (JIT)
restore the old lowering. `var t = typeof v` over two plain locals records a
compile-time fact; a following `t ===/!==/==/!= "lit"` lowers to
`TypeOfIs {a: v}` and a `TypeOf` nobody reads to `LoadUndefined`. Facts die on
any emitted write to either register, at the end of the enclosing statement,
on loop/switch/try entry, in each case/catch/finally body and after a `for`
init; sloppy parameters are refused once `arguments` is mentioned. In both JIT
tiers a non-heap tag answers `TypeOfIs` with a constant store (double/Int →
number, Bool, Undefined-tagged, exact null → object); heap values still call
`jit_typeof_is`. json-large's `walk` now runs two `TypeOfIs` where it ran one
`TypeOf` and three `LoadConst`/`Eq` pairs.

Gate (2026-09-02, one binary at `e46c7e69`, 21 interleaved pairs, exact
output): json-large **−2.6%** [−4.2, −2.3]; markdown-render +0.3% [−1.0, +1.0],
polymorphic-objects −0.4% [−1.0, +0.7], parse-large-js −0.4% [−0.6, +0.5]
(null). Correctness: a 206-run four-mode output identity against node over
every normal, hostile, PGO-training and syntax-corpus program (the one
mismatch is the pre-existing forced-JIT segfault on the nanoid row, below);
`tests/typeof_alias.rs` spawns children under every latch and mode;
`jit_tier_fuzz`, `jit_tier_parity`, `instr_uses_exhaustive`, `local_sroa`,
`real_program_corpus` and the accumulator suites pass (`accum_may_read`
gained `IsArray`/`ForInKeys`/`LenOf`/`ForInLive`).

### B261 LANDED — JSON views parser-proved ASCII as `&str` and parses short integers directly

`ZIPP_NO_JSON_ASCII_UNCHECKED=1` restores every `from_utf8`;
`ZIPP_NO_JSON_INT_FAST=1` restores `parse::<f64>` for every number token. The
plain-key scanner tracks whether any byte was ≥ 0x80 and takes an unchecked
`&str` view for ASCII names; number tokens are ASCII by grammar; the
serializer passes the flat string's `ascii` flag so ASCII values take the
`&str` quoter without a second validation or a per-byte surrogate probe. A
`-?digits` token of at most 15 digits accumulates as `u64` and converts
exactly (`-0` stays a negative-zero double). The hardened profile keeps the
checked conversion.

Gate (2026-09-02, one binary at `e46c7e69`, 21 interleaved pairs, exact
output): json-large **−2.8%** [−3.6, −1.3]; markdown-render +1.2% [−0.3, +2.2],
parse-large-js −0.0% [−0.7, +0.3] (null). Both JSON levers together against
the `ab1f85b3` base: json-large −3.7% [−4.7, −1.5], regex-log-scan −0.3%,
shapes-stable −0.3%, calls-closures −0.4%. Measured and declined:
`ZIPP_SHAPESTATS` shows the shape tree at its 4,096-node cap with ~1,800
distinct keys on this row, so a key-sequence shape cache would not hit.

### B260 LANDED — the i32 add whose only observer is a ToInt32 truncation wraps

`ZIPP_NO_INT32_TRUNC_ADD=1` restores the overflow branch. calls-closures spent
9.9% of the row in the f64 overflow path of `(rotate(value) + 1013904223) | 0`
and 5.0% in the double-to-int32 `Or` that followed. `trunc_only_arith_ips`
walks the function's CFG forward from each `Add`/`Sub`/`AddInt` and admits, as
a least fixpoint, the producers whose destination is read only by `Bitwise`
operands or other admitted producers before redefinition (handler targets are
universal successors; direct eval, `with` and `arguments`-mapped parameters
decline). An admitted site keeps both Int tag checks but emits the 32-bit
add/sub with no `jo` and no double: the exact result is an i33 every consumer
reduces mod 2^32, and the wrapped i32 is that residue — ToInt32 of what the
interpreter, the latch-off binary or a post-deopt resume computes. Tier-C and
the MEM region tier; the INT-GPR tier's i53 guard is untouched (it needs an
operand-range proof, not a consumer proof).

Gate (2026-09-02, one binary at `86c513ee`, 21 interleaved pairs, exact
output): calls-closures **−7.3%** [−8.5, −6.2]; bytecode-vm +0.0%
[−1.7, +3.3], shapes-stable −0.2% [−4.7, +2.0], allocation-survival +1.9%
[−3.5, +4.2], warm-router −2.2% [−2.8, +0.4] (all null). Two-binary against
the `52961c42` base: calls-closures −6.8% [−8.2, −6.5], json-large +0.2%
[−1.5, +2.1], markdown-render +0.1% [−0.5, +2.7]. Unit tests pin the ToInt32
identity over the edge lattice plus four million random pairs against a
checked-i64 reference, and the admission/decline of each idiom on compiled
shapes; `tests/int32_trunc_add.rs` compares every mode and the latch against
node. Left on the table: the `LoadInt 0 / Or` itself still executes at the
admitted sites, and the arrow body's five dead-register materialisations.

### B259 LANDED — the warm pristine-Promise re-check answers from bit compares

`ZIPP_NO_PRISTINE_LEAN=1` restores the per-call re-proof. The pristine
`Promise.prototype` slot cache guarded layout with version compares but still
re-read three key strings, three out-of-line `PropAttrs::at` results and three
unpacked heap identities on every `.then`, `await` and `Promise.all` element,
because in-place data writes bump nothing. `promise_pristine_bit_identical` now
answers the warm case from the two owner versions, the value bits at the
`then`/`constructor`/`@@species` slots against the fill-time bits, the accessor
bits (a new inlineable `ObjMap::is_accessor_at`) and the species getter's
version; it only ever answers `true` and defers every mismatch to the unchanged
re-check, so an in-place patch is caught by the bits and an accessor
redefinition by its version bump.

Gate (2026-09-02, one binary at `85a299f3`, 21 interleaved pairs, exact
output): async-promise-chain **−5.1%** [−5.8, −4.5], async-lived **−3.9%**
[−4.5, −2.7]. Two-binary against the `52961c42` base: async-promise-chain
−6.3% [−7.0, −5.2], json-large +0.3% [−1.6, +1.6], shapes-stable +0.5%
[+0.3, +2.5] — a row the change cannot reach, read as the fat-LTO layout
confound two-binary comparisons carry (B240). `tests/promise_pristine_lean.rs`
holds 17 node-oracled parity cases and the latch/mode children; microtask
order matched node byte-for-byte on the async rows across five modes. Found
alongside: zipp shares one `@@species` getter object across constructors
(node: one per constructor), and the per-await SipHash is `gen_callee` /
`gen_args_obj` in `std::collections::HashMap` — both left for a follow-up.

### B258 LANDED — pooled literal shells keep their slab value cell

`ZIPP_NO_SHELL_CELL=1` restores the old path. Every finalize-born literal paid
a decoupled slab round-trip per object: the sweep's `free_slot` returned the
dying shell's value cell to its class free list (`strip_slab` → `free_cell`)
before pooling the shell, and the very next `alloc_finalized_thin` popped a cell
straight back out (`alloc_cell`, 2.3% of shapes-stable's samples on its own) to
refit that same shell. A pool-bound shell now keeps its cell at death; the thin
birth path pops the shell first and copies the new values into the cell in
place when the class matches, stripping on a mismatch, at the general
`alloc_finalized` path, and at the courier trim, so the courier never sees a
cell. Folded into `Heap::shell_cell` with the B257 thin fold; every non-thin
configuration keeps its exact old path.

Gate (2026-09-02, one binary at `51019ac2`, 21 interleaved pairs, exact
output): shapes-stable **−4.1%** [−6.3, −1.7], shapes-megamorphic **−2.0%**
[−3.3, −1.0], warm-router **−2.2%** [−3.9, −1.0], reactish-reconcile **−1.3%**
[−2.3, −0.4], allocation-survival −0.7% [−3.4, +2.9] (null); object-row geomean
0.9795× [−3.18%, −0.95%]. Two-binary control against the `70702c84` base:
json-large +0.7% [−0.6, +1.1], calls-closures +0.6% [−0.9, +1.6] (both cross
parity), shapes-stable −3.5% [−5.6, −2.8]. `tests/shell_cell.rs` pins the
in-place fill, the mismatch strip and mode parity; `[shellcell]` counters under
`ZIPP_GCSTATS=1`. B257 (the thin literal-allocation paths, `ZIPP_NO_THIN_ALLOC`)
landed in the v0.0.6 release without a registry entry; its latch and counters
are documented in `heap.rs`.

### B256 LANDED — dense-array pin snapshots stay warm across stores and helper calls

Commit `b1435c7`. PC-profiling the object-lifetime cluster (a `profiling`
build with the linker map, 30 accumulated `ZIPP_PROF_PC` runs per row,
`tools/pcmap.py` over the numeric dump files only) showed
`jit_ta_snapshot` + `dense_array_snap_flags` at 7–8.5% of allocation-survival,
shapes-stable and shapes-megamorphic: every cross call AND every
`jit_set_index` / method-IC helper re-derived the region's dense-Array pin
snapshots through the helper. Two causes, both fixed behind latches:

- `Heap::get_mut` advances `array_snapshot_epoch` on every Array borrow, and
  `jit_set_index` used it for an in-range element store, so B244's per-call
  epoch cache never hit inside a loop storing into the array it pins. An
  in-range store (present element, or a hole-fill below `len` the protector
  checks already licensed) now writes through `Heap::array_store_in_place`,
  which leaves base and length — the only facts a snapshot licenses —
  untouched. `ZIPP_NO_ARRAY_STORE_NOBUMP=1` restores the bump.
- Twenty-plus post-helper refetch sites in `region_mem.rs` / `inline.rs`
  called `emit_refetch_ta` unconditionally; they now go through
  `emit_cross_refetch_ta` (epoch + live-source identity check), the
  `ta_refetch` tuple carrying the region's epoch-cache offset.
- `emit_refetch_pinned` re-pins r13/r14 from the `versions_raw` /
  `ic_table_raw` mirrors (the B250 Tier-C entry loads) instead of two helper
  calls. `ZIPP_NO_DIRECT_REFETCH_BASES=1` restores the calls.

| DIAGNOSTIC A/B (same binary unless noted, 16 pairs, exact output) | Result |
|---|---|
| store no-bump latch: allocation-survival | **−3.7%** [−5.7, −1.7] |
| store no-bump latch: shapes-stable / shapes-megamorphic | **−2.4%** [−3.2, −2.0] / **−3.2%** [−4.8, −2.0] |
| cached post-helper refetch, two-binary over the first: shapes-stable / megamorphic / survival | **−4.5%** [−5.3, −3.6] / **−4.1%** [−4.8, −3.2] / −3.1% [−4.2, +0.1] |
| direct base refetch latch: warm-router / reactish-reconcile | **−3.7%** [−5.3, −2.5] / **−1.5%** [−2.8, −0.7] |
| all other cluster rows | null |

Method lessons recorded: a build-then-copy chain must gate the copy on the
`Finished` line (a failed build silently re-measured the previous binary,
producing plausible "results" for two runs); and `tools/pcmap.py` must be
fed only the numeric dump files, never the `[profpc]` stderr summaries.

### B255 LANDED — hostile-path routing and append cursor

Commit `21288c1` brings four independently guarded mechanisms together:

- stable paired-`typeof` fusion (`ZIPP_NO_TYPEOF_SAME=1` restores the prior
  route);
- call-free Tier-C loose-null checks
  (`ZIPP_NO_TIERC_LOOSE_NULL_INLINE=1`);
- a polymorphic-function-id Cross3 call router
  (`ZIPP_NO_CROSS3_POLY_FID=1`); and
- a deferred flat-ASCII append cursor (`ZIPP_NO_STR_APPEND_CURSOR=1`).

The same commit fixes protected returns that cross `finally` and adds focused
coverage for that control-flow boundary. The final PGO executable is
`c2ddb9e6…6a3cb5`, built from source `21288c1` with profile
`fbe16992…e91743`. Its complete normal and hostile captures are the current
public results above. Exact output held in all 1,800 four-engine observations.

The aggregate result is not the finish line. Current Node point gaps are the
four normal rows async-promise-chain, json-large, regex-log-scan, and
sparse-array, plus nine hostile rows: calls-closures, shapes-stable,
shapes-megamorphic, allocation-survival, async-lived, reactish-reconcile,
warm-router, bytecode-vm, and npm-nanoid.

### B254 LANDED — pinned call-result string length

Commit `4ff3bdf` uses the MEM string pin's immutable `{obj_bits, bytes, units}`
snapshot to answer `.length` before the eight-way identity IC and B190 helper.
The emitted lane checks the exact receiver identity, rejects an invalid
snapshot, loads UTF-16 units, boxes the value, and otherwise falls through to
the unchanged generic path. `ZIPP_NO_PINNED_STR_LEN=1` restores the prior route.

Final reviewed binary: commit `2869e91`, SHA-256 `bf85742b…f85952`, clean
release build.

| DIAGNOSTIC A/B | Result |
|---|---|
| same-binary NanoID, 32 pairs | **−13.11%**, 0.868869 [0.860467, 0.873758], 31 / 32 wins |
| final vs B252 NanoID, 32 pairs | **−12.85%**, 0.871516 [0.866427, 0.881037], 32 / 32 wins |
| final vs frozen B253, hostile all-17 | **−1.28%**, 0.987230 [0.984695, 0.991308] |
| final vs frozen B253, category-balanced | **−1.74%**, 0.982579 [0.979742, 0.987197] |
| final vs frozen B253, NanoID | **−12.88%**, 0.871198 [0.866817, 0.877921] |
| final vs frozen B253, React | **−2.12%**, 0.978798 [0.974462, 0.991749] |
| final vs frozen B253, normal all-13 | **−0.50%**, 0.9950 [0.9873, 1.0017], neutral |

Every output was exact and no cold row had a supported regression. The full
hostile artifact's adjusted analysis is incomplete only because the very short
allocation-ephemeral case had no usable adjusted ratio; the cold analysis is
complete. The B253 comparator is a frozen dirty feature build, so the sweep is
not publication evidence.

At the B254 checkpoint, filtered 16-pair Node diagnostics put NanoID at
**1.169× Node** [1.160, 1.195] and React at **1.673× Node** [1.618, 1.715].
Those dated figures are retained as attribution evidence; B255's canonical
ratios above supersede them for the current engine comparison.

### B254 hardening LANDED — impossible identity-IC empty marker

Commit `6f945ce` replaces zero in fresh identity-cache ways with
`0x7FFE000000000001`, a NaN bit pattern outside all five `Value` tags. Numeric
boxing canonicalizes it, so raw f64 `+0` cannot match an empty way when pinned
length or quick length is switched off. Direct shape-pair sites deliberately
retain zero because their packed pattern slots use it as the free marker.

Release tests prove raw `+0` reaches the target MEM property region under all
three feature off-switches. This is a safety fix with no speed claim.

### B253 LANDED — stable concat suffix memo

Commit `ff1c737` adds a weak, lazy 256-entry suffix memo for a size-bounded
B212-frozen `const + int` head followed by one pinned immutable one-byte ASCII
suffix. Exact keys plus left/result versions cover ABA; results stay frozen and
cannot seed recursively. The emitter admits only an adjacent terminal
`StrConcatChain` followed by the lowering's exact `Move`, and the runtime proves
the final leaf is tagged `Int`. `ZIPP_NO_CONCAT_SUFFIX_MEMO=1` restores the old
route.

Same-binary React evidence, 32 pairs: **−4.14%**, 0.958636 [0.949720,
0.965530], 31 / 32 wins. Complete normal and hostile safety sweeps were neutral.

### B253 hardening LANDED — validate both concat links

Commit `2869e91` requires both current and next chain links to satisfy
`dst == a == accumulator`, and the trailing `Move` to consume that accumulator,
before emitting the suffix hint. The compiler already generated that invariant;
the validation protects future rewrites. Focused invariant and all 19 concat
parity/mechanism tests pass. No speed claim.

### B252 LANDED — merge monotonic recycled-object address runs

Commit `f5c33df` proves an ascending retained prefix and monotonic newly-dead
suffix, then joins them with reverse/rotate; uncertain layouts retain the full
sort. `ZIPP_NO_OBJ_POOL_RUN_SORT=1` restores the old route. Allocation survival
improved **2.93%** [0.28%, 3.34%] across 31 pairs; shapes and the normal suite
were neutral. A MEM-to-nested-INT NanoID handoff was separately **REFUTED** at
**+86.11% slower** and fully reverted.

### B251 LANDED — scalar singleton `[[IsHTMLDDA]]`

Commit `c575105` mirrors the production VM's one HTMLDDA exotic as a scalar
comparison instead of paying a hash lookup in `typeof`, loose-null, truthiness,
callability, and Annex B paths. React improved **4.11%** [3.75%, 5.05%]; normal
all-13 was neutral. `ZIPP_NO_HTMLDDA_SCALAR=1` restores the old path.

### B250 VERIFIED — call-result string pins and call-entry cleanup

B250 admitted call-written string receivers only under a forward MUST
reaching-definition proof, refreshed snapshots after calls, loaded Tier-C bases
directly, and emitted same-`FuncProto` guards. Its combined exact hostile A/B
improved **3.07%** [1.57%, 4.30%], with NanoID and closure-call gains; the
normal all-13 sweep was neutral. B254 removes the remaining `.length` IC/helper
cost from that string-pin path.

### B249 CANONICAL — captured-call scaffolding closed

B249 restored fused direct and computed method calls where argument evaluation
cannot observe the property read, stopped recycling captured temporaries, and
added bare guarded `MathOp` lowering. The clean `0bff482` PGO capture moved the
retained-ten result to 0.918× Node, all-13 to 0.635×, and hostile ordinary to
0.866×. It was the public baseline before B255's `21288c1` capture.

Earlier entries, designs, hazards, and refutations remain searchable in the
[historical ledger](docs/archive/PERF_LEDGER-B001-B252.md).

## Active targets

Priorities are ordered by current measured gap and by whether the next question
can be answered with a bounded experiment.

### 1. Broader-corpus WASM robustness and transfer size

The exact v0.0.6 attempt exposes a coverage problem before a throughput ranking:
there are no comparable normal rows and only five comparable hostile rows, with
QuickJS-NG ahead on four. First make the fixed production-limit boundary
explicitly configurable for a separate diagnostic without weakening shipped
defaults, repair the async trap and string/typed-array capacity failures, and
require exact output across every cross-engine-supported row. Keep the existing
persistent-module boundary and frozen work/control sources as the continuity
anchor; do not tune only to the five speed-kernel shapes.

The Zipp artifact remains `3.586×` as large raw and `2.958×` as large at
Brotli-11 as QuickJS-NG's reactor.
Attribute host glue, Rust/runtime footprint, and interpreter code separately.
Compressed transfer size is the shipping objective. The remaining absolute gap
is large enough to justify a fresh symbol/section and feature-footprint audit.

### 2. Guard the native real13 win over QuickJS-NG

The clean final-engine-source diagnostic now leads all 13 QuickJS-NG point
medians, at `0.6058409×` adjusted overall; the closest row is sparse-array at
`0.9312122×`. Keep exact output and this full 13-row protocol as the regression
gate. Do not profile a native QuickJS gap unless a future clean rerun reveals
one; the present priority is WASM coverage and its four measured point gaps.

### 3. Allocation survival — 1.772× Node

This is the largest current Node gap and one of three hostile rows above the
aspirational 1.50× individual cap, alongside warm-router and
reactish-reconcile. Profile nursery promotion, survivor tracing, and
free-list/pool maintenance separately; do not infer the term from the much
faster allocation-ephemeral row.

### 4. Warm router and React reconcile — 1.674× / 1.646× Node

Both application-shaped rows need fresh attribution on B255. For the router,
separate closure dispatch from property and URL/string work. For React, measure
recursive framed calls, handler-bearing loops, shape checks, and allocation
before attempting another combined shortcut.

### 5. Object shapes — 1.493× / 1.484× Node

Stable and megamorphic shapes are almost equally slow, which argues against
assuming this is only an IC-polymorphism problem. Compare lookup, guard failure,
transition, and allocation counters on the same binary.

### 6. Calls, async, and retained normal gaps

Closure calls are **1.307× Node** and async-promise-chain is **1.232×**.
Long-lived hostile async is **1.082×**. JSON and sparse-array are about
**1.05×**, while regex is **1.017×** with an interval crossing parity. Scout one
mechanism at a time and require a complete normal-plus-hostile safety sweep.

Bytecode-vm (**1.014×**) and NanoID (**1.000×**) are point gaps but currently
near parity; neither should outrank a supported double-digit gap without a
cheap, isolated explanation.

## Standing gate

### Correctness

```powershell
cargo test --workspace --release
cargo check -p zipp-vm --no-default-features
cargo check -p zipp-vm --no-default-features --features safe-sandbox
cargo test -p zipp-vm --no-default-features --features safe-sandbox --no-fail-fast
```

The last line is the whole hardened-profile suite and is expected clean
(209 binaries, 0 failures as of 2026-09-02): the JIT-pinning suites carry the
x86-64 JIT cfg, the limit tests size themselves from `zipp_vm::safe_native_limits`,
and the drains that need sixty-seven million iterator steps run only under
`--release`. For changes that touch sandbox-only code, also run the standalone
sandbox workspace. For interpreter/JIT semantics, compare the test262 expected-failure
identity in default, `ZIPP_NOJIT=1`, `ZIPP_JIT_THRESHOLD=1`, and
`ZIPP_NO_NURSERY=1` modes. Run the tier-differential fuzz slice for codegen,
register-planning, deopt, or heap-layout changes.

### Focused performance

1. Add an off-switch where a same-binary A/B is practical.
2. Use at least 16 exactly balanced pairs; use 32 for marginal or
   layout-sensitive decisions.
3. Require exact output and empty health/correctness/drift failure lists.
4. Re-run the affected normal and hostile safety sets.
5. Fail a candidate when a supported unrelated cold regression exceeds 0.5%
   unless a documented trade is explicitly accepted.
6. Freeze and hash the final clean binary; do not benchmark a moving target.

Routine artifacts go under ignored `target/bench-results/`. See
[`bench/README.md`](bench/README.md) for commands and publication policy.

### Public capture

Use `tools/pgo.sh`, then the complete `tools/bench.py` and
`tools/bench_hostile.py` engine protocols. `bench/run_real.sh` is legacy and is
not the publication runner. Promote an artifact only after an independent audit
confirms `publishable:true`, clean source/binary provenance, full engine order,
complete suite selection, exact output, adequate repetitions/bootstrap samples,
and no drift.

## Experiment discipline

- Profile the current binary before editing; old attribution expires quickly.
- Change one mechanism at a time and push each verified small win.
- Prefer same-binary attribution, then a frozen-binary layout/generalisation
  comparison, then a fresh engine diagnostic.
- Record the exact comparator, binary hash, commit, switches, repetitions,
  ordering, medians, ratio, interval, strict wins, and correctness status.
- Neutral results are useful. Revert experiments that miss their gate and keep
  the kill reason here.
- Never use an aggregate to imply that every row wins.
- Never convert a security-policy change into a performance default without an
  explicit policy decision.

## Historical lookup

The archived ledger retains B001–B252, old section numbers, detailed hazard
proofs, and refuted experiments. Source comments that cite “`PERF_ROADMAP.md`
B59”, B9, B29, or another historical ID refer to that archive. The durable
nursery design remains at [`NURSERY_DESIGN.md`](NURSERY_DESIGN.md) because code
comments cite its numbered sections directly.
