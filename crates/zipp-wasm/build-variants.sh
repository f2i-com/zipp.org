#!/usr/bin/env bash
# Build the zipp-wasm artifacts with the README's exact post-processing, and
# print their raw and Brotli sizes side by side:
#
#   javascript  JavaScript only (the shipped default)
#   python      JavaScript + Python, without the torch package
#   torch       the torch package for `python`: zipp_torch.wasm + its loader
#   all         JavaScript + Python with torch built in (python + torch in one)
#   interop     `all` plus the trusted Python/JavaScript interop
#
#   cd crates/zipp-wasm
#   ./build-variants.sh              # javascript and all, into dist/<variant>/
#   ./build-variants.sh python torch # any of the above
#
# Each engine variant is built in its own target directory so the feature
# switch does not thrash one incremental cache, and each `dist/<variant>/` is
# a complete `wasm-bindgen --target web` package plus a `.wasm.br` for
# `Content-Encoding: br` serving. `dist/torch/` holds zipp_torch.wasm (+ .br)
# and zipp_torch.js. Requires the pinned wasm-bindgen CLI (see README.md);
# `brotli` is optional (sizes fall back to node's zlib).
set -euo pipefail
cd "$(dirname "$0")"

TARGET=wasm32-unknown-unknown
export RUSTFLAGS="${RUSTFLAGS:--C link-arg=--max-memory=1073741824 -C link-arg=-zstack-size=16777216}"

declare -A FEATURES=(
  [javascript]=""
  [python]="--features python-base"
  [all]="--features python"
  [interop]="--features python-js-interop"
  [torch]=""
)
variants=("$@")
[ ${#variants[@]} -eq 0 ] && variants=(javascript all)

for v in "${variants[@]}"; do
  [ -n "${FEATURES[$v]+x}" ] || { echo "unknown variant '$v' (javascript | python | torch | all | interop)" >&2; exit 2; }
done

compress() {
  if command -v brotli >/dev/null 2>&1; then
    brotli -q 11 -f -o "$1.br" "$1"
  else
    node -e 'const z=require("zlib"),f=require("fs");f.writeFileSync(process.argv[2],z.brotliCompressSync(f.readFileSync(process.argv[1]),{params:{[z.constants.BROTLI_PARAM_QUALITY]:11}}))' \
      "$1" "$1.br"
  fi
}

sizes=()
for v in "${variants[@]}"; do
  out="dist/$v"
  if [ "$v" = torch ]; then
    # The package module: no wasm-bindgen (it has no imports); the linker
    # strips every custom section (names, producers, target_features).
    echo "=== torch: cargo build --release --target $TARGET (torch/)"
    RUSTFLAGS="-C link-arg=--strip-all" \
      cargo +1.92.0 build --locked --release --target "$TARGET" --manifest-path torch/Cargo.toml --target-dir torch/target
    rm -rf "$out"
    mkdir -p "$out"
    cp "torch/target/$TARGET/release/zipp_torch.wasm" "$out/zipp_torch.wasm"
    cp torch/zipp_torch.js "$out/zipp_torch.js"
    compress "$out/zipp_torch.wasm"
    sizes+=("$v $(wc -c < "$out/zipp_torch.wasm") $(wc -c < "$out/zipp_torch.wasm.br")")
    continue
  fi
  echo "=== $v: cargo build --release --target $TARGET ${FEATURES[$v]}"
  # shellcheck disable=SC2086
  cargo +1.92.0 build --locked --release --target "$TARGET" --target-dir "target/variants/$v" ${FEATURES[$v]}
  rm -rf "$out"
  wasm-bindgen --target web --out-dir "$out" \
    --remove-name-section --remove-producers-section \
    "target/variants/$v/$TARGET/release/zipp_wasm.wasm"
  node tests/node/strip-target-features.cjs "$out/zipp_wasm_bg.wasm" "$out/zipp_wasm_bg.stripped.wasm"
  mv "$out/zipp_wasm_bg.stripped.wasm" "$out/zipp_wasm_bg.wasm"
  node tests/node/check-wasm-memory.cjs "$out/zipp_wasm_bg.wasm"
  compress "$out/zipp_wasm_bg.wasm"
  sizes+=("$v $(wc -c < "$out/zipp_wasm_bg.wasm") $(wc -c < "$out/zipp_wasm_bg.wasm.br")")
done

echo
printf '%-12s %14s %14s\n' variant raw brotli-11
for s in "${sizes[@]}"; do
  # shellcheck disable=SC2086
  set -- $s
  printf '%-12s %14s %14s\n' "$1" "$2" "$3"
done
