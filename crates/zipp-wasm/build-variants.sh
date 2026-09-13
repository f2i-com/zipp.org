#!/usr/bin/env bash
# Build the two zipp-wasm artifacts — JavaScript-only (the shipped default)
# and the combined JavaScript + Python module — with the README's exact
# post-processing, and print their raw and Brotli sizes side by side.
#
#   cd crates/zipp-wasm
#   ./build-variants.sh              # both, into dist/<variant>/
#   ./build-variants.sh javascript   # one of: javascript | all | interop
#
# Each variant is built in its own target directory so the feature switch does
# not thrash one incremental cache, and each `dist/<variant>/` is a complete
# `wasm-bindgen --target web` package plus a `.wasm.br` for `Content-Encoding:
# br` serving. Requires the pinned wasm-bindgen CLI (see README.md); `brotli`
# is optional (sizes fall back to node's zlib).
set -euo pipefail
cd "$(dirname "$0")"

TARGET=wasm32-unknown-unknown
export RUSTFLAGS="${RUSTFLAGS:--C link-arg=--max-memory=1073741824 -C link-arg=-zstack-size=16777216}"

declare -A FEATURES=(
  [javascript]=""
  [all]="--features python"
  [interop]="--features python-js-interop"
)
variants=("$@")
[ ${#variants[@]} -eq 0 ] && variants=(javascript all)

for v in "${variants[@]}"; do
  [ -n "${FEATURES[$v]+x}" ] || { echo "unknown variant '$v' (javascript | all | interop)" >&2; exit 2; }
done

sizes=()
for v in "${variants[@]}"; do
  echo "=== $v: cargo build --release --target $TARGET ${FEATURES[$v]}"
  # shellcheck disable=SC2086
  cargo +1.92.0 build --locked --release --target "$TARGET" --target-dir "target/variants/$v" ${FEATURES[$v]}
  out="dist/$v"
  rm -rf "$out"
  wasm-bindgen --target web --out-dir "$out" \
    --remove-name-section --remove-producers-section \
    "target/variants/$v/$TARGET/release/zipp_wasm.wasm"
  node tests/node/strip-target-features.cjs "$out/zipp_wasm_bg.wasm" "$out/zipp_wasm_bg.stripped.wasm"
  mv "$out/zipp_wasm_bg.stripped.wasm" "$out/zipp_wasm_bg.wasm"
  node tests/node/check-wasm-memory.cjs "$out/zipp_wasm_bg.wasm"
  if command -v brotli >/dev/null 2>&1; then
    brotli -q 11 -f -o "$out/zipp_wasm_bg.wasm.br" "$out/zipp_wasm_bg.wasm"
  else
    node -e 'const z=require("zlib"),f=require("fs");f.writeFileSync(process.argv[2],z.brotliCompressSync(f.readFileSync(process.argv[1]),{params:{[z.constants.BROTLI_PARAM_QUALITY]:11}}))' \
      "$out/zipp_wasm_bg.wasm" "$out/zipp_wasm_bg.wasm.br"
  fi
  raw=$(wc -c < "$out/zipp_wasm_bg.wasm")
  br=$(wc -c < "$out/zipp_wasm_bg.wasm.br")
  sizes+=("$v $raw $br")
done

echo
printf '%-12s %14s %14s\n' variant raw brotli-11
for s in "${sizes[@]}"; do
  # shellcheck disable=SC2086
  set -- $s
  printf '%-12s %14s %14s\n' "$1" "$2" "$3"
done
