#!/usr/bin/env bash
# Tier census: how many regions reach each JIT tier across the real benches.
# A shift from "INT decline" to "INT region ... compiled" is the signal that a
# planner admission change actually landed; the timings in quick.sh/run_real.sh
# say whether it was worth it.
# Trusted developer benchmark only; use `zipp sandbox` for unreviewed scripts.
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1
Z=${ZIPP:-./target/release/zipp.exe}
printf "%-24s %5s %5s %5s %5s\n" bench INT INTdec MEM evict
ti=0; td=0; tm=0; te=0
for f in bench/real/*.js; do
  b=$(basename "$f" .js)
  log=$(ZIPP_JITLOG=1 "$Z" js "$f" 2>&1 >/dev/null)
  i=$(printf '%s' "$log" | grep -c "INT region")
  d=$(printf '%s' "$log" | grep -c "INT decline")
  m=$(printf '%s' "$log" | grep -c "DOUBLE/MEM region")
  v=$(printf '%s' "$log" | grep -c "EVICTED")
  printf "%-24s %5s %5s %5s %5s\n" "$b" "$i" "$d" "$m" "$v"
  ti=$((ti+i)); td=$((td+d)); tm=$((tm+m)); te=$((te+v))
done
printf "%-24s %5s %5s %5s %5s\n" TOTAL "$ti" "$td" "$tm" "$te"
