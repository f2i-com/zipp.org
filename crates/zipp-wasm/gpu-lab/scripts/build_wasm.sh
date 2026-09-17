#!/usr/bin/env sh
# Build wasm/kernels.wasm from the `zipp-kernels` crate.
#
# Cargo links this itself, so there is no separate compile-and-link dance and
# no `wasm-opt` post-link step to be tripped up by an npm shim on PATH. The
# module is freestanding: no allocator, no libc, no imports. The host reads
# `memory` and `__heap_base` and runs its own arena over them.
#
# The 2 GiB ceiling is a ceiling, not a reservation: memory grows on demand.
# It is what a real checkpoint needs -- a 0.6B Qwen3 is 373 MB of blocks, and
# the arena is a bump allocator that does not reclaim within one execution.
#
# `+simd128` is what makes the matmul panels vectorise. Float determinism needs
# no flag here: Rust never contracts a multiply and an add into an FMA, so
# every operation rounds on its own, which is what keeps this bit-for-bit with
# the JavaScript reference. (The C this replaced needed -ffp-contract=off to
# promise the same thing.)
set -eu
cd "$(dirname "$0")/.."
CRATE="../rust"
OUT="wasm/kernels.wasm"

RUSTFLAGS="-C target-feature=+simd128 \
 -C link-arg=--export-memory \
 -C link-arg=--export=__heap_base \
 -C link-arg=--initial-memory=2097152 \
 -C link-arg=--max-memory=2147483648" \
  cargo build --release --target wasm32-unknown-unknown \
    --manifest-path "$CRATE/Cargo.toml" -p zipp-kernels

cp "$CRATE/target/wasm32-unknown-unknown/release/zipp_kernels.wasm" "$OUT"
echo "$OUT: $(wc -c < "$OUT") bytes"
