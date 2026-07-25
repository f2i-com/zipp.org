#!/usr/bin/env bash
# Light iteration harness: best-of-3 on a 4-bench subset chosen to cover the
# distinct engine paths (property/alloc, string, numeric/JIT, regex) without the
# ~4 minutes the full suite costs. NOT a substitute for bench/run_real.sh —
# re-run that before quoting a geomean.
cd "$(dirname "$0")/.." || exit 1
Z=${ZIPP:-./target/release/zipp.exe}
BENCHES=${BENCHES:-"json-large class-prototype-hot typedarray-math sparse-array"}
ms(){ local s e; s=$(date +%s%N); "$@" >/dev/null 2>&1; e=$(date +%s%N); echo $(( (e-s)/1000000 )); }
best(){ local m=99999999 v i; for ((i=0;i<3;i++)); do v=$(ms "$@"); (( v<m )) && m=$v; done; echo "$m"; }
printf "%-22s %8s %8s %7s\n" bench zipp node ratio
ratios=""
for b in $BENCHES; do
  z=$(best "$Z" js bench/real/$b.js); nd=$(best node bench/real/$b.js)
  printf "%-22s %7sms %7sms %6.2fx\n" "$b" "$z" "$nd" \
    "$(awk -v a="$z" -v b="$nd" 'BEGIN{printf "%.2f", a/b}')"
  ratios="$ratios $z/$nd"
done
awk -v r="$ratios" 'BEGIN{
  n=split(r,a," "); s=0
  for(i=1;i<=n;i++){ split(a[i],p,"/"); s+=log(p[1]/p[2]) }
  printf "%-22s %24.3fx  (geomean of %d)\n","GEOMEAN",exp(s/n),n }'
