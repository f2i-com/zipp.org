// Focused interpreter SetProp IC microbenchmark.
//
// Run with:
//   ZIPP_NOJIT=1 ./target/release/zipp js bench/setprop-own-nojit.js
//
// `hot` is deliberately last in an 11-property map, below ObjMap's hash-index
// threshold: the benchmark isolates the linear lookup skipped by the probe.
const o = {
  p00: 0, p01: 0, p02: 0, p03: 0,
  p04: 0, p05: 0, p06: 0, p07: 0,
  p08: 0, p09: 0, hot: 0
};

let checksum = 0;
for (let round = 0; round < 5; round++) {
  for (let i = 0; i < 2000000; i++) {
    o.hot = i;
  }
  checksum += o.hot;
}
console.log(checksum);
