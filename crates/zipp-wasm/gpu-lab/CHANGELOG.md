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
real projection, and 4.1 times slower on wasm for a one-token decode step.
Four fifths of that gap is requantising a resident weight on every step, which a
bind-time derivation would remove; the scalar integer dot itself is only 1.5
times slower than the SIMD float one. WebGPU and WebGL2 have no 64-bit
integer type to hold a 2^40 accumulator and declare it unsupported; the runtime
now refuses a graph a backend has no kernel for, rather than leaving an output
buffer at zero. `docs/FIXED-POINT.md` has the measurements and what the GPU
backends would need.
