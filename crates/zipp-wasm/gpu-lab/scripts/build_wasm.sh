#!/usr/bin/env sh
set -eu
cd "$(dirname "$0")/.."
# Compiling and linking separately, rather than letting the Clang driver link.
# The wasm32 driver runs whatever `wasm-opt` it finds on PATH as an optional
# post-link step, and on Windows that is often the npm shim, which cannot be
# executed and leaves a deleted output behind. wasm-ld is taken from the same
# directory as the compiler so a non-PATH LLVM works too.
CC="${CLANG:-clang}"
case "$CC" in
  */*) LD="${WASM_LD:-$(dirname "$CC")/wasm-ld}" ;;
  *)   LD="${WASM_LD:-wasm-ld}" ;;
esac
OBJECT="${TMPDIR:-/tmp}/zipp-gpu-kernels-$$.o"
trap 'rm -f "$OBJECT"' EXIT
"$CC" --target=wasm32 -O3 -msimd128 -fno-builtin -ffp-contract=off -c wasm/kernels.c -o "$OBJECT"
"$LD" --no-entry --export-memory --export=__heap_base \
  --initial-memory=131072 --max-memory=134217728 \
  -o wasm/kernels.wasm "$OBJECT"
