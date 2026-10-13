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
work="$(mktemp -d "${input}.opt.XXXXXX")"
trap 'rm -rf -- "$work"' EXIT
features=(--enable-bulk-memory --enable-multivalue --enable-mutable-globals
  --enable-nontrapping-float-to-int --enable-reference-types --enable-sign-ext)
# `-O1` twice. A second run of the same pipeline finds what the first exposed
# and is smaller on BOTH measures for every variant (JavaScript 5,225,008 /
# 1,291,816 after one run, 5,221,923 / 1,289,944 after two; Lite 2,829,432 /
# 753,410 against 2,824,616 / 751,787). A third run, `--converge`, and the
# passes that only shrink the raw module (code folding, constant hoisting,
# local CSE, -O2's inlining) each cost Brotli bytes and are not used.
#
# Before it, two passes that -O1 leaves out (-O2 and -O3 have them, with the
# inlining that does not pay here) and that shrink both measures: constant
# propagation through locals, and redundant-set elimination with a vacuum
# after it. Then the functions are laid out by signature and body (see
# order-functions.cjs), which only moves them. Measured 2026-10-14 against
# -O1 -O1 alone, raw / Brotli: Lite -7,057 / -12,415 bytes, JavaScript
# -21,873 / -15,022, Python -37,304 / -19,531, Python + Torch -39,351 /
# -20,633; the layout is about 9 to 12 KB of each Brotli figure.
#
# Dead-argument elimination (-O2's `--dae-optimizing`) took 2.7 to 4.2 KB
# more off each full module's Brotli, but the JavaScript module ran its
# polymorphic property reads and array callbacks 2 to 4% slower with it,
# and is not used.
wasm-opt "$input" --precompute-propagate --rse --vacuum -O1 -O1 \
  "${features[@]}" -o "$work/optimized.wasm"
# The temporary names that carry the order are not kept.
node "$root/tests/node/order-functions.cjs" "$work/optimized.wasm" "$work/keyed.wasm"
wasm-opt "$work/keyed.wasm" --reorder-functions-by-name --strip-debug "${features[@]}" \
  -o "$work/candidate.wasm"
node "$root/tests/node/select-smaller-wasm.cjs" "$input" "$work/candidate.wasm" "${@:2}"
node "$root/tests/node/check-wasm-memory.cjs" "$input"
