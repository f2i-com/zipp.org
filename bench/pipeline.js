// Byte-for-byte the same kernel as pipeline.ts, for a V8 / Bun comparison.
// Builds an array via push, then map (capturing k) → filter → reduce each rep,
// so the intermediate arrays are allocated every rep (GC pressure). All values
// stay < 2^53 so the integer checksum is exact in f64 `number`.
function pipeline(reps, n) {
  let acc = 0;
  for (let r = 0; r < reps; r++) {
    const xs = [];
    for (let i = 0; i < n; i++) {
      xs.push((i + r) % 1000);
    }
    const k = r % 7;
    const mapped = xs.map((x) => (x * 3 + k) % 9973);
    const evens = mapped.filter((x) => x % 2 === 0);
    const s = evens.reduce((a, b) => a + b, 0);
    acc = (acc + s) % 1000000007;
  }
  return acc;
}

function main() {
  return pipeline(2000, 2000);
}
const eng = typeof Bun !== "undefined" ? "bun/JSC" : "node/V8";
const t0 = process.hrtime.bigint();
const r = main();
const t1 = process.hrtime.bigint();
console.log(r);
console.log(`=> ${r} (${eng}, ran in ${Number(t1 - t0) / 1e6} ms)`);
