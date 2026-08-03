#!/usr/bin/env bash
# Profile-guided release build — the published-headline build since B114.
#
# Measured 2026-08-03 (B114): headline geomean 0.8672x [0.863, 0.870] against
# the identical-source non-PGO build, 21 counterbalanced pairs — the largest
# single lever ever measured in this repo. The gains sit exactly where PGO can
# reach: AOT Rust (interpreter dispatch, MEM-tier helpers, regress exec, JSON,
# GC). The two >=85%-jit-native rows measured flat, which is the expected blind
# spot (runtime-emitted dynasm pages are untouched by construction).
#
# The binary is fully portable (no ISA raise; x86-64-v3 measured NULL, same
# entry). Cost is two builds plus a ~20s training pass. The profile lives under
# target/pgo-data and MUST be retrained after any nontrivial engine change —
# a stale profile silently degrades toward the non-PGO number, so a headline
# capture should always run this script first, from a clean tree.
#
# Usage: bash tools/pgo.sh        # leaves the binary at
#        target/x86_64-pc-windows-msvc/release/zipp.exe
# (The explicit --target keeps RUSTFLAGS off host proc-macros — dynasm.)
set -euo pipefail
cd "$(dirname "$0")/.."

TRIPLE=x86_64-pc-windows-msvc
PGODIR="$(pwd)/target/pgo-data"
PROFDATA="$(pwd)/target/merged.profdata"
LLVM_PROFDATA="$HOME/.rustup/toolchains/stable-$TRIPLE/lib/rustlib/$TRIPLE/bin/llvm-profdata.exe"

rm -rf "$PGODIR" "$PROFDATA"
mkdir -p "$PGODIR"

echo "== stage 1/4: instrumented build =="
RUSTFLAGS="-Cprofile-generate=$PGODIR" cargo build --release --target "$TRIPLE"

echo "== stage 2/4: training (13 real + 6 micro benches) =="
BIN="target/$TRIPLE/release/zipp.exe"
for f in bench/real/*.js; do "$BIN" js "$f" > /dev/null 2>&1; done
for f in bench/fib.js bench/loop.js bench/array.js bench/string.js bench/object.js bench/sort.js; do
  "$BIN" js "$f" > /dev/null 2>&1
done

echo "== stage 3/4: merge profiles =="
"$LLVM_PROFDATA" merge -o "$PROFDATA" "$PGODIR"

echo "== stage 4/4: optimized build =="
RUSTFLAGS="-Cprofile-use=$PROFDATA" cargo build --release --target "$TRIPLE"

echo "PGO binary ready: $BIN"
"$BIN" --version | head -3
