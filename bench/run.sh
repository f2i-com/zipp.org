#!/usr/bin/env bash
# Sequential micro-benchmark: zipp (the new register-VM + JIT engine, `js-vm`)
# vs node (V8). Reports COMPUTE time = best wall time minus the engine's own
# empty-program startup. Override the engine with ENGINE=js (old engine) etc.
# Correctness compares STDOUT only: some benches print a non-deterministic
# elapsed-ms line to STDERR (their own internal timer), which can never match.
cd "$(dirname "$0")/.." || exit 1
ZIPP=./target/release/zipp.exe
ENGINE=${ENGINE:-js-vm}
OUT=bench/results.txt
ITERS=5
BENCHES="fib loop array string object sort"

ms(){ local s e; s=$(date +%s%N); "$@" >/dev/null 2>&1; e=$(date +%s%N); echo $(( (e-s)/1000000 )); }
best(){ # min of ITERS runs of: $@
  local m=99999999 v i
  for ((i=0;i<ITERS;i++)); do v=$(ms "$@"); (( v<m )) && m=$v; done
  echo "$m"
}

: > "$OUT"
echo "=== correctness (zipp stdout must equal node stdout; engine=$ENGINE) ===" >> "$OUT"
ALLOK=1
for b in $BENCHES; do
  n=$(node bench/$b.js 2>/dev/null | tr -d '\200-\377')
  z=$($ZIPP $ENGINE bench/$b.js 2>/dev/null | tr -d '\200-\377')
  if [ "$n" = "$z" ]; then echo "$b: OK ($n)" >> "$OUT"; else echo "$b: MISMATCH node=[$n] zipp=[$z]" >> "$OUT"; ALLOK=0; fi
done
echo "ALL_CORRECT=$ALLOK" >> "$OUT"

ne=$(best node bench/empty.js); ze=$(best $ZIPP $ENGINE bench/empty.js)
echo "" >> "$OUT"
echo "startup(ms): node=$ne  zipp=$ze" >> "$OUT"
echo "" >> "$OUT"
printf '%-8s %12s %12s %10s %14s %14s\n' bench node_cmp zipp_cmp ratio node_e2e zipp_e2e >> "$OUT"
printf '%-8s %12s %12s %10s %14s %14s\n' -------- ------- ------- ----- ------- ------- >> "$OUT"
for b in $BENCHES; do
  nw=$(best node bench/$b.js); zw=$(best $ZIPP $ENGINE bench/$b.js)
  nc=$(( nw-ne )); (( nc<1 )) && nc=1
  zc=$(( zw-ze )); (( zc<1 )) && zc=1
  ratio=$(awk "BEGIN{printf \"%.1f\", $zc/$nc}")
  printf '%-8s %10dms %10dms %9sx %12dms %12dms\n' "$b" "$nc" "$zc" "$ratio" "$nw" "$zw" >> "$OUT"
done
echo "DONE" >> "$OUT"
