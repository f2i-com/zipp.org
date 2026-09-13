#!/usr/bin/env sh
set -eu
cd "$(dirname "$0")/.."
"${CLANG:-clang}" --target=wasm32 -O3 -fno-builtin -ffp-contract=off -nostdlib \
  -Wl,--no-entry -Wl,--export-memory -Wl,--export=__heap_base \
  -Wl,--initial-memory=131072 -Wl,--max-memory=134217728 \
  -o wasm/kernels.wasm wasm/kernels.c
