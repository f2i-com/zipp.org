#!/usr/bin/env bash
# Real-world JS benchmark suite: five-engine comparison
#   node (V8) | bun (JSC) | deno (V8) | zipp JIT | zipp interpreter (NOJIT)
# Same method as bench/run_long.sh: COMPUTE = best-of-N wall time minus the
# engine's empty-program startup; correctness compares zipp stdout vs node.
# Env overrides: ITERS (best-of-N, default 5); BENCHES (subset of table rows);
# APPEND=1 (skip truncate/correctness/header: append rows to an existing
# results file); FINAL=0 (suppress the trailing DONE when chunking).
cd "$(dirname "$0")/.." || exit 1
ZIPP=${ZIPP:-./target/release/zipp.exe}
OUT=bench/results_real.txt
ITERS=${ITERS:-5}
ALLBENCHES="parse-large-js json-large markdown-render map-set-heavy typedarray-math regex-log-scan class-prototype-hot async-promise-chain polymorphic-objects sparse-array"
BENCHES=${BENCHES:-$ALLBENCHES}
APPEND=${APPEND:-0}
FINAL=${FINAL:-1}

run_node(){ node "$1"; }
run_bun(){ bun "$1"; }
run_deno(){ deno run "$1"; }
run_zipp(){ "$ZIPP" js "$1"; }
run_nojit(){ ZIPP_NOJIT=1 "$ZIPP" js "$1"; }

ms(){ local s e; s=$(date +%s%N); "$@" >/dev/null 2>&1; e=$(date +%s%N); echo $(( (e-s)/1000000 )); }
best(){
  local m=99999999 v i
  for ((i=0;i<ITERS;i++)); do v=$(ms "$@"); (( v<m )) && m=$v; done
  echo "$m"
}

EMPTY=bench/long/empty.js
if [ "$APPEND" = "0" ]; then
  : > "$OUT"
  echo "=== correctness (zipp stdout must equal node stdout) ===" >> "$OUT"
  ALLOK=1
  for b in $ALLBENCHES; do
    n=$(node bench/real/$b.js 2>/dev/null | tr -d '\200-\377')
    z=$("$ZIPP" js bench/real/$b.js 2>/dev/null | tr -d '\200-\377')
    if [ "$n" = "$z" ]; then echo "$b: OK" >> "$OUT"; else echo "$b: MISMATCH node=[$n] zipp=[$z]" >> "$OUT"; ALLOK=0; fi
  done
  echo "ALL_CORRECT=$ALLOK" >> "$OUT"

  ne=$(best run_node  $EMPTY)
  be=$(best run_bun   $EMPTY)
  de=$(best run_deno  $EMPTY)
  ze=$(best run_zipp  $EMPTY)
  je=$(best run_nojit $EMPTY)
  echo "" >> "$OUT"
  echo "startup(ms): node=$ne bun=$be deno=$de zipp=$ze zipp_nojit=$je   (best of $ITERS)" >> "$OUT"
  echo "" >> "$OUT"
  printf '%-22s %8s %8s %8s %8s %12s %10s\n' bench node bun deno zipp zipp_nojit zipp/node >> "$OUT"
  printf '%-22s %8s %8s %8s %8s %12s %10s\n' ---------------------- ------ ------ ------ ------ ---------- --------- >> "$OUT"
else
  ne=$(best run_node  $EMPTY)
  be=$(best run_bun   $EMPTY)
  de=$(best run_deno  $EMPTY)
  ze=$(best run_zipp  $EMPTY)
  je=$(best run_nojit $EMPTY)
fi

for b in $BENCHES; do
  f=bench/real/$b.js
  nw=$(best run_node  "$f"); nc=$(( nw-ne )); (( nc<1 )) && nc=1
  bw=$(best run_bun   "$f"); bc=$(( bw-be )); (( bc<1 )) && bc=1
  dw=$(best run_deno  "$f"); dc=$(( dw-de )); (( dc<1 )) && dc=1
  zw=$(best run_zipp  "$f"); zc=$(( zw-ze )); (( zc<1 )) && zc=1
  jw=$(best run_nojit "$f"); jc=$(( jw-je )); (( jc<1 )) && jc=1
  ratio=$(awk "BEGIN{printf \"%.2f\", $zc/$nc}")
  printf '%-22s %6dms %6dms %6dms %6dms %10dms %9sx\n' "$b" "$nc" "$bc" "$dc" "$zc" "$jc" "$ratio" >> "$OUT"
done
if [ "$FINAL" = "1" ]; then echo "DONE" >> "$OUT"; fi
