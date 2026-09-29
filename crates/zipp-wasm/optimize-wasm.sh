#!/usr/bin/env bash
# Conservative, measured post-processing. Keep a candidate only when both its
# raw size and Brotli download size improve; stronger optimization can hurt the
# latter. The same pipeline is used by local variants and release packaging.
set -euo pipefail
root="$(cd "$(dirname "$0")" && pwd)"
case "$(wasm-opt --version)" in
  'wasm-opt version 125' | 'wasm-opt version 125 (version_125)') ;;
  *) echo 'Install the measured optimizer: npm install --global binaryen@125.0.0' >&2; exit 1 ;;
esac
input="${1:?usage: optimize-wasm.sh MODULE.wasm}"
candidate="$(mktemp "${input}.opt.XXXXXX")"
trap 'rm -f -- "$candidate"' EXIT
wasm-opt "$input" -O1 \
  --enable-bulk-memory --enable-multivalue --enable-mutable-globals \
  --enable-nontrapping-float-to-int --enable-reference-types --enable-sign-ext \
  -o "$candidate"
node "$root/tests/node/select-smaller-wasm.cjs" "$input" "$candidate"
node "$root/tests/node/check-wasm-memory.cjs" "$input"
