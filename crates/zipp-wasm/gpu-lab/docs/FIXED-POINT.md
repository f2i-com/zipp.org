# `matmul_fixed`: a product a proof system can check

Every other operation in Graph v2 is bit-for-bit across backends by *agreement*.
Each one rounds float32 in the same order, and `tests/backend-bits.test.mjs`
holds them there over whole training steps. That agreement is real and it is
tested, but it is an agreement: it has to be maintained by hand in four
languages, and it is a property of the implementations rather than of the
arithmetic.

`matmul_fixed` is bit-for-bit by *construction*. Both sides are quantized to
int16 and the products are summed as integers. Integer addition is associative,
so once two implementations agree on the quants, no ordering, no vectorisation
and no scheduling can make their sums differ. There is nothing to maintain.

That is worth a separate operation because of what it makes possible. A STARK or
sumcheck argument works over a prime field. A prime field can express an integer
sum directly; it cannot express IEEE-754 rounding without emulating it, at
hundreds of constraints per multiply-add. So the obstacle to proving that a
remote machine really ran the matmul it was paid to run is not the volume of the
trace — it is that float32 is the wrong algebra. This operation is the right one.

## What it computes

Given `A [M, K]` and a weight `B [N, K]` (float32, Q4_K or Q6_K):

1. Each row of `A` and each row of `B` gets a scale of its own,
   `s = max|row| / 32767`, taken over the row's actual values. A quantized
   weight is requantized from what it decodes to, not from the block's own
   scales — Q4_K and Q6_K carry sub-block scales that no single per-row integer
   scale can stand in for.
2. Each value becomes `floor(x / s + 0.5)`, clamped to ±32767.
3. `acc = Σ qa[j] * qw[j]`, exactly, as an integer.
4. The output is `f32(acc) * f32(sa * sw)`, in that order.

Per-row scales rather than one scale per tensor: one scale for a whole matrix is
set by its largest weight and wastes the range of every other row. It is the
smaller of the two decisions — see the measurements below, where the width of
the quants dominates — but it is free, so it is taken.

## Two forms, one answer

Steps 1 and 2 depend only on the weight, so they need not happen on every step.

- **Run time.** `matmul_fixed(a, W)` where `W` is float32, Q4_K or Q6_K. Each
  output column is decoded and quantized as it is used. Nothing is stored.
- **Bind time.** `quantizeWeight(W, n, k, dtype)` in `src/quant.mjs` does steps 1
  and 2 once and returns `{quants, scales}`. The quants become an input of
  dtype `i16` and the scales a plain float32 `[N]` input, passed as `c`:
  `matmul_fixed(a, quants, scales)`. Nothing is decoded or quantized per step.

These are the same arithmetic at different times, so they produce **identical
float32 bits** — `tests/fixed-point.test.mjs` asserts `Object.is` between them
on both backends for all three weight sources. A checker comparing two peers
therefore does not need to know which form either of them ran.

The trade is memory. `i16` is two bytes a value against Q4_K's 0.5625, so a
resident weight is **3.56x larger**: for Qwen3-0.6B, 373 MB becomes 1.33 GB.
That is a hosted stage's trade to make and not a browser peer's, which is why
both forms exist and neither is the default. The graph's own accounting shows
it — `logicalBytes` counts an `i16` weight as the bytes it is.

`i16` is this protocol's own format and not a checkpoint one. A float32 `matmul`
refuses it: its kernels are named after the dtype, so the alternative is a
lookup that finds nothing and an output left at zero.

`quantizeRow` in `src/kernel-math.mjs` is the statement of steps 1 and 2 that
every backend mirrors. Three details in it carry the whole guarantee:

- **`floor(x + 0.5)`, not a rounding intrinsic.** JavaScript's `Math.round`
  rounds a half away from zero; WGSL's `round` rounds it to even. Those disagree
  on exactly the values a quantiser lands on most often. `floor` means the same
  thing in all four languages. The `+ 0.5` is exact in float32 here, because the
  operand is under 2^15 and float32 is exact to 2^24, so it makes no difference
  whether a backend adds in float32 or in double.
- **One division by the scale.** Correctly rounded in JavaScript, in Rust and in
  WGSL. Not in GLSL ES — see below.
- **A max taken with `>`.** A NaN is then skipped in every language rather than
  propagating in some and not others.

A row with no magnitude has no scale to derive; its quants are zero and so is its
contribution, and returning a zero scale keeps that true without a branch at the
point of use.

## Accumulator width

Each product of two int16s is under 2^30. `k` of them reach `k * 2^30`: 2^40 for
a 1024-wide projection, 2^43 for 8192. The graph validator refuses `k` past 2^23,
where the sum could exceed what a double holds exactly.

| holder | exact to | verdict |
| --- | --- | --- |
| f32 accumulator | 2^24 | nowhere near enough — this is why the float path is order-dependent at all |
| i32 | 2^31 | not enough for two products |
| JS double (cpu-js) | 2^53 | fine |
| i64 (wasm) | 2^63 | fine |
| f128 field element | 2^127 | fine, with 87 bits to spare |

The field is not the constraint. Nothing about proving this is limited by how
wide the accumulator has to be.

## What it costs against float32

Measured on `blk.13.attn_q.weight` from Qwen3-0.6B-Q4_K_M — a projection from the
middle of a real model, 1024 wide — against activations with the statistics real
ones have, including eight outliers at 30 sigma, which is the case that makes
activation quantisation hard.

| accumulation | worst abs | median rel | cosine | accumulator |
| --- | --- | --- | --- | --- |
| float32, what `matmul` does | 4.35e-06 | 2.35e-07 | 1.000000 | — |
| fixed, a8 w8, one scale | 7.28e-01 | 7.60e-02 | 0.997657 | 2^24 |
| fixed, a8 w8, per row | 7.21e-01 | 5.28e-02 | 0.998621 | 2^24 |
| fixed, a16 w8, per row | 1.16e-01 | 1.05e-02 | 0.999946 | 2^32 |
| **fixed, a16 w16, per row** | **2.25e-03** | **1.83e-04** | **1.000000** | **2^40** |
| fixed, a32 w16, per row | 5.09e-04 | 4.04e-05 | 1.000000 | 2^56 |

a16w16 is what this implements, and both halves of that choice are in the table.
The *weight* width is what dominates — a16w8 to a16w16 is fifty times better,
while per-row scaling at a8w8 barely moves the worst case. And widening the
activations past 16 bits is where it stops: a32w16 buys another 4.4× and pushes
the accumulator to 2^56, past the 2^53 where a JavaScript double is still exact,
so the cpu-js reference could no longer be the oracle. 16 bits either side is the
last row where the two backends that implement this can hold the sum in a type
they already have.

The error is *relative*, not absolute: both sides are quantized against their own
row's maximum, so scaling a tensor scales the answer and the error together. The
absolute figure above belongs to that tensor's magnitude; `tests/fixed-point.test.mjs`
asserts the scale-free form and checks that scaling the weights by 0.02 leaves
the ratio unchanged.

Whether 1.8e-04 median relative error is acceptable is a question about the model,
not about this operation.

## Where it stops being close to float32

Exactness and accuracy are different claims, and only the first is unconditional.
The answer is always the same on every backend; how near it lands to the float32
answer depends on the dynamic range inside a single row.

A row's scale is set by its largest magnitude, so one value a thousand times the
rest crushes everything else towards a quant of zero. Usually that is harmless,
because a value that large dominates the output too and is carried exactly. It
stops being harmless when the weights that value meets are near zero, so the
answer is made entirely of the terms that were crushed. Measured against a
64-column projection with one activation inflated and its whole weight column
zeroed — the adversarial construction, not a natural one:

| activation outlier | worst difference / output RMS | cosine |
| --- | --- | --- |
| none | 6.9e-05 | 1.000000 |
| 100x | 1.8e-03 | 1.000000 |
| 10,000x | 2.0e-01 | 0.997449 |
| 1,000,000x | the output is zero | — |

A residual stream's outliers are 30 to 100 times the typical magnitude, which is
the second row. Past about 10^3 of in-row dynamic range this operation is no
longer a usable stand-in for `matmul`, and the fix if that is ever needed is
per-block scales rather than per-row — the same answer Q4_K itself reaches.
`tests/fixed-point.test.mjs` pins the first two rows so a change that makes them
worse is caught rather than discovered.

Two smaller edges, both checked and both consistent across backends: a row whose
maximum is subnormal underflows its scale to zero and contributes nothing, and a
row of zeros does the same. Neither produces a NaN on either backend.

## Speed

A Q4_K `[m, 1024] @ [1024, 1024]` projection on the wasm backend, measured the
way a stage actually runs one: `prepare()` once so the weight is uploaded and
held, then timed `run()` steps. (Timing `execute()` instead re-uploads the
weight on every call and inflates every figure here, unevenly.)

| m | `matmul` | `matmul_fixed` run time | `matmul_fixed` bind time |
| --- | --- | --- | --- |
| 1 (one decode step) | 0.30 ms | 2.13 ms | **0.21 ms** |
| 8 | 1.20 ms | 3.50 ms | 1.61 ms |
| 32 | 4.43 ms | 8.32 ms | 6.44 ms |

Fitting each line between m = 8 and m = 32 separates a per-output-column cost
from a per-activation-row cost, and that is where the shape of it is:

| | per column | per row of activations |
| --- | --- | --- |
| `matmul` | 0.12 ms (decode) | 0.135 ms |
| `matmul_fixed`, run time | 1.90 ms (decode, then requantise) | 0.201 ms |
| `matmul_fixed`, bind time | 0.00 ms | 0.201 ms |

Three things fall out.

**The bind-time form has no per-column cost at all**, which is what it is for.
At m = 1 it is 0.21 ms against the float32 matmul's 0.30 — *faster than the
operation it replaces*, because it skips the Q4_K decode that the float path
cannot skip.

**The run-time form spends 1.90 ms per column requantising**, against 0.12 ms
to decode. Nine tenths of a one-token decode step, repeated every step for a
weight that never changes. That is the entire reason the bind-time form exists.

**The scalar integer dot is 1.5x the SIMD float one** — 0.201 against 0.135 ms a
row — and that is the whole remaining gap. It is why the bind-time form loses
its lead as `m` grows: at m = 32 it is 1.5x slower. Decode is m = 1, which is
the case a hosted stage lives in; prefill is not, and would want this closed.
wasm SIMD has no 64-bit multiply-accumulate, so closing it means paired i32
lanes, or splitting each int16 into two int8 halves to use `i32x4_dot_i16x8`
with a carry pass. Neither is done.

## Why the GPU backends refuse it

**Neither WGSL nor GLSL ES has a 64-bit integer type.** Products reach 2^30 and
their sums 2^40, and `i32` tops out at 2^31 - 1, so a GPU implementation needs
64-bit arithmetic emulated in paired `u32`s: a widening multiply into lo/hi, an
add with carry, and a defined conversion of the pair to f32 at the end.

Rather than leave those backends with no case for the operation - which would
leave the output buffer at zero and hand back a plausible tensor of nothing -
they declare it unsupported, and `ComputeRuntime` refuses the graph before
anything is uploaded. For a protocol whose entire purpose is to be *checkable*,
a silent wrong answer is the worst available failure.

Three things to settle before implementing either:

- **The emulated 64-bit path needs testing on its own**, against a table of
  known products, before it goes anywhere near a kernel. It is about fifteen
  lines and every one of them is a place to be wrong by one bit.
- **GLSL ES does not require a correctly rounded divide.** Its precision table
  allows 2.5 ULP on a division - the same figure `GL_ARB_shader_precision`
  states for desktop GL - while WGSL and the two CPU backends are correctly
  rounded. WebGL2 could therefore land on the other side of a `.5` boundary and
  choose a different integer, and over millions of elements that is not a rare
  event. The fix is to reformulate the quantisation so a divide is not on the
  critical path, and it is the reason WebGL2 may end up out of scope for this
  operation rather than merely waiting on it.
- **`backend: 'auto'` picks a backend before it has seen a graph.** It selects
  WebGPU where WebGPU exists, so a graph containing `matmul_fixed` would be
  refused rather than falling back. Whoever switches a model plugin onto this
  operation has to make the caller's fallback chain - `controller.mjs` in the
  LEASED kit - treat `UNSUPPORTED` as "try the next backend", the way it already
  treats a backend that fails to create.

WebGPU is the one worth doing first: correctly rounded division, and the only
GPU backend where the emulation is the whole of the work.

## What does not implement it

- **WebGPU and WebGL2**, as above: they refuse the graph.
- **`zipp_gpu.py`**, the Python authoring and local-execution library. A graph
  containing `matmul_fixed` falls out of its kernel path and its reference path
  rejects it — `ComputeError: OP: Unsupported operation: matmul_fixed` — which
  is the right failure, not a silent one. A Python implementation would be a
  third numeric path to keep exact, and there is no caller for it yet.
- **The Qwen3 model plugin**, which still emits `matmul`. Deliberately: the
  operation is proven on its own before a whole model moves onto it. When it
  does, `bindGraph` is where `quantizeWeight` belongs — it already walks every
  weight once — and the decision of *which* weights get the `i16` treatment is a
  memory budget, not a correctness question. A stage can mix the two forms
  freely, because they give the same answer.

## Where this goes

The operation exists so that a peer's compute can be *proven* rather than
*re-run*. The step after this one is a sumcheck or GKR argument over the integer
dot products, which is the standard primitive for exactly this shape and is what
`zk-zipp` should have been proving instead of ZIPP bytecode.

Between here and there is a smaller and more immediately useful consequence. The
LEASED kit currently cross-checks a hosted stage by having a second peer redo the
work and comparing within `AGREEMENT = 1e-3`, a tolerance that exists only
because two backends running float32 need one. For a stage built on
`matmul_fixed`, the tolerance is zero: a checker either reproduces the bits or
the peer did something else. That is a much sharper instrument, and it needs no
proof system at all.

Neither is built. The Qwen3 plugin still emits `matmul`, deliberately — the
operation is proven here first, and switching the plugin is one line once
somebody wants the whole model on this path.
