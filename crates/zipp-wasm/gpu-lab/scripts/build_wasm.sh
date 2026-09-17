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

# Where this was built must not be part of what was built. `gguf-quants` comes
# from a git dependency, so rustc records a panic location inside the cargo
# checkout -- an absolute path naming whoever ran this. That is 40-odd bytes
# nobody else can reproduce, in a binary that is committed and meant to be
# checkable by rebuilding it. Remapped to a fixed stand-in instead.
#
# The prefix has to match the form rustc records, which on Windows is
# `C:\Users\...` and not the `/c/Users/...` this shell uses, hence cygpath.
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
if command -v cygpath >/dev/null 2>&1; then
  cargo_home=$(cygpath -w "$cargo_home")
fi

RUSTFLAGS="-C target-feature=+simd128 \
 --remap-path-prefix=$cargo_home=/cargo \
 -C link-arg=--export-memory \
 -C link-arg=--export=__heap_base \
 -C link-arg=--initial-memory=2097152 \
 -C link-arg=--max-memory=2147483648" \
  cargo build --release --target wasm32-unknown-unknown \
    --manifest-path "$CRATE/Cargo.toml" -p zipp-kernels

cp "$CRATE/target/wasm32-unknown-unknown/release/zipp_kernels.wasm" "$OUT"
echo "$OUT: $(wc -c < "$OUT") bytes"
