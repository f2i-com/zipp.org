#!/usr/bin/env bash
# Long-form variant of bench/run.sh: same six workloads scaled 10-1000x so
# compute dominates startup (hundreds of ms per run) and the zipp-vs-node
# ratio is stable. Same method: COMPUTE = best-of-N wall time minus the
# engine's empty-program startup; correctness compares stdout.
# Trusted developer benchmark only; use `zipp sandbox` for unreviewed scripts.
set -euo pipefail
cd "$(dirname "$0")/.." || exit 1
ZIPP=./target/release/zipp.exe
ENGINE=${ENGINE:-js-vm}
RESULT_DIR=target/bench-results
mkdir -p "$RESULT_DIR"
OUT=$RESULT_DIR/legacy-long.txt
ITERS=${ITERS:-7}
BENCHES="fib loop array string object sort"
[[ $ITERS =~ ^[1-9][0-9]*$ ]] || { echo "ITERS must be a positive integer" >&2; exit 2; }

ms(){
  local s e rc
  s=$(date +%s%N)
  if "$@" >/dev/null 2>&1; then rc=0; else rc=$?; fi
  (( rc == 0 )) || { printf 'benchmark command failed (%d):' "$rc" >&2; printf ' %q' "$@" >&2; printf '\n' >&2; return "$rc"; }
  e=$(date +%s%N)
  echo $(( (e-s)/1000000 ))
}
best(){
  local m=99999999 v i
  for ((i=0;i<ITERS;i++)); do v=$(ms "$@"); (( v<m )) && m=$v; done
  echo "$m"
}

: > "$OUT"
echo "=== correctness (zipp stdout must equal node stdout; engine=$ENGINE) ===" >> "$OUT"
ALLOK=1
for b in $BENCHES; do
  n=$(node bench/long/$b.js 2>/dev/null | tr -d '\200-\377')
  z=$($ZIPP $ENGINE bench/long/$b.js 2>/dev/null | tr -d '\200-\377')
  if [ "$n" = "$z" ]; then echo "$b: OK ($n)" >> "$OUT"; else echo "$b: MISMATCH node=[$n] zipp=[$z]" >> "$OUT"; ALLOK=0; fi
done
echo "ALL_CORRECT=$ALLOK" >> "$OUT"

ne=$(best node bench/long/empty.js); ze=$(best $ZIPP $ENGINE bench/long/empty.js)
echo "" >> "$OUT"
echo "startup(ms): node=$ne  zipp=$ze   (best of $ITERS)" >> "$OUT"
echo "" >> "$OUT"
printf '%-8s %12s %12s %10s %14s %14s\n' bench node_cmp zipp_cmp ratio node_e2e zipp_e2e >> "$OUT"
printf '%-8s %12s %12s %10s %14s %14s\n' -------- ------- ------- ----- ------- ------- >> "$OUT"
for b in $BENCHES; do
  nw=$(best node bench/long/$b.js); zw=$(best $ZIPP $ENGINE bench/long/$b.js)
  nc=$(( nw-ne )); (( nc<1 )) && nc=1
  zc=$(( zw-ze )); (( zc<1 )) && zc=1
  ratio=$(awk "BEGIN{printf \"%.2f\", $zc/$nc}")
  printf '%-8s %10dms %10dms %9sx %12dms %12dms\n' "$b" "$nc" "$zc" "$ratio" "$nw" "$zw" >> "$OUT"
done
echo "DONE" >> "$OUT"
