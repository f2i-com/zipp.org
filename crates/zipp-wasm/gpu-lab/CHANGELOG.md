# Integrated GPU Lab

The original 0.1.0 standalone graph runtime is integrated into Zipp's Python
playground. The JavaScript guest adapter remains an explicit embedder option.
Added hardware adapter reporting, real Python/WASM acceptance, and a separate
WebGL texture budget including RGBA32F padding and reduction scratch.
The native NCA research app now lives in `f2i-com/neuralautomata.com`.

## `matmul_fixed`

A second matrix product, accumulated in integers instead of float32. Both sides
are quantized to int16 against a per-row maximum, the k products are summed
exactly, and the total is rescaled once. Integer addition is associative, so
cpu-js and wasm reach an identical result by construction rather than by
agreeing about float32 rounding order — the property a STARK or sumcheck
argument needs, since a prime field can express an integer sum and cannot
express IEEE-754 rounding.

It is a separate operation and not a mode of `matmul`, because it makes a
different promise: 1.8e-04 median relative error against the float32 answer on a
real projection, and 7 times slower on wasm for a one-token decode step. Nine
tenths of that gap is requantising a resident weight on every step, which a
bind-time derivation removes (see the next entry); the scalar integer dot itself
is 1.5 times slower than the SIMD float one. WebGPU and WebGL2 have no 64-bit
integer type to hold a 2^40 accumulator and declare it unsupported; the runtime
now refuses a graph a backend has no kernel for, rather than leaving an output
buffer at zero. `docs/FIXED-POINT.md` has the measurements and what the GPU
backends would need.

## An i16 weight, quantized once

`matmul_fixed` gained a second form. The weight can arrive already quantized --
a new `i16` input dtype, two little-endian bytes a value, with its per-row
scales as a plain float32 [N] input -- so that the decode and requantise happen
once when the graph is built rather than on every step. `quantizeWeight` in
quant.mjs produces both halves from a float32, Q4_K or Q6_K tensor.

The two forms are the same arithmetic at different times and produce identical
float32 bits, which the tests assert with Object.is rather than a tolerance. So
a stage may mix them freely and a checker need not know which one a peer ran.

Measured as a stage runs it -- prepared once, weight held, then timed steps --
one [1,1024] @ [1024,1024] Q4_K projection: 2.13 ms run-time, 0.21 ms
bind-time, against 0.30 ms for the float32 matmul. Faster than the operation it
replaces, because it skips a decode the float path cannot skip. It costs 3.56
times the resident bytes, which the graph's own logicalBytes accounting shows.

A float32 matmul refuses an i16 weight: its kernels are named after the dtype,
so the alternative is a lookup that finds nothing and an output left at zero.
