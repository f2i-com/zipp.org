#!/usr/bin/env sh
# Build the GGUF WebAssembly module that `src/gguf.mjs` reads checkpoints with.
#
# Two targets from one crate: `web` for a browser (ES module, `fetch`-loaded)
# and `nodejs` for the test suite, which needs `require`. Neither is committed
# as source -- they are build outputs of `../rust/zipp-model-wasm`.
#
# The CLI version must match the `wasm-bindgen` dependency pinned in that
# crate's Cargo.toml exactly, or the generated glue will not match the module.
set -eu
cd "$(dirname "$0")/.."
CRATE="../rust"
WASM="$CRATE/target/wasm32-unknown-unknown/release/zipp_model_wasm.wasm"

want=$(grep -o '"=0\.2\.[0-9]*"' "$CRATE/zipp-model-wasm/Cargo.toml" | tr -d '"=')
have=$(wasm-bindgen --version | awk '{print $2}')
[ "$want" = "$have" ] || { echo "wasm-bindgen CLI is $have, the crate pins $want" >&2; exit 1; }

cargo build --release --target wasm32-unknown-unknown \
  --manifest-path "$CRATE/Cargo.toml" -p zipp-model-wasm

for target in web nodejs; do
  case "$target" in web) out=wasm/gguf ;; nodejs) out=wasm/gguf-node ;; esac
  wasm-bindgen --target "$target" --out-dir "$out" "$WASM"
  echo "$out: $(du -k "$out" | tail -1 | awk '{print $1}') KB"
done
# This package is "type": "module", and the nodejs target emits CommonJS. A
# scoped manifest says so for that directory only, which is what lets the test
# suite `require` it.
echo '{"type":"commonjs"}' > wasm/gguf-node/package.json
