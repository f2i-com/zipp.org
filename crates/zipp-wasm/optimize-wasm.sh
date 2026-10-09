#!/usr/bin/env bash
# Conservative, measured post-processing. Full builds require raw savings
# without Brotli growth; Lite explicitly opts into a bounded Brotli tradeoff.
# The same pipeline is used by local variants and release packaging.
set -euo pipefail
root="$(cd "$(dirname "$0")" && pwd)"
case "$(wasm-opt --version)" in
  'wasm-opt version 125' | 'wasm-opt version 125 (version_125)') ;;
  *) echo 'Install the measured optimizer: npm install --global binaryen@125.0.0' >&2; exit 1 ;;
esac
input="${1:?usage: optimize-wasm.sh MODULE.wasm [--prefer-raw-size]}"
candidate="$(mktemp "${input}.opt.XXXXXX")"
trap 'rm -f -- "$candidate"' EXIT
# `-O1` twice. A second run of the same pipeline finds what the first exposed
# and is smaller on BOTH measures for every variant (JavaScript 5,225,008 /
# 1,291,816 after one run, 5,221,923 / 1,289,944 after two; Lite 2,829,432 /
# 753,410 against 2,824,616 / 751,787). A third run, `--converge`, and the
# passes that only shrink the raw module (code folding, function reordering,
# constant hoisting, local CSE) each cost Brotli bytes and are not used.
wasm-opt "$input" -O1 -O1 \
  --enable-bulk-memory --enable-multivalue --enable-mutable-globals \
  --enable-nontrapping-float-to-int --enable-reference-types --enable-sign-ext \
  -o "$candidate"
node "$root/tests/node/select-smaller-wasm.cjs" "$input" "$candidate" "${@:2}"
node "$root/tests/node/check-wasm-memory.cjs" "$input"
