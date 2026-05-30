// Byte-for-byte the same kernel as closures.ts, for a V8 / Bun comparison. A
// capturing closure (LCG step over mul/inc) called ~20M times. Values stay
// < 2^53 so the integer checksum is exact in f64 `number`.
function main() {
  const mul = 1000003;
  const inc = 12345;
  const step = (x) => (x * mul + inc) % 2147483648;
  let seed = 1;
  let acc = 0;
  for (let i = 0; i < 20000000; i++) {
    seed = step(seed);
    acc = (acc + (seed % 100)) % 1000000007;
  }
  return acc;
}
const eng = typeof Bun !== "undefined" ? "bun/JSC" : "node/V8";
const t0 = process.hrtime.bigint();
const r = main();
const t1 = process.hrtime.bigint();
console.log(r);
console.log(`=> ${r} (${eng}, ran in ${Number(t1 - t0) / 1e6} ms)`);
