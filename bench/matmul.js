// Byte-for-byte the same kernel as matmul.zipp (256x256, flat Float64Array),
// for a V8 / Bun comparison. Entries are small integers so the checksum is exact.
function main() {
  const n = 256;
  const a = new Float64Array(n * n);
  const b = new Float64Array(n * n);
  const c = new Float64Array(n * n);

  for (let i = 0; i < n; i++) {
    for (let j = 0; j < n; j++) {
      a[i * n + j] = (i + j) % 10;
      b[i * n + j] = (i * j) % 10;
    }
  }

  for (let i = 0; i < n; i++) {
    for (let j = 0; j < n; j++) {
      let s = 0.0;
      for (let k = 0; k < n; k++) {
        s = s + a[i * n + k] * b[k * n + j];
      }
      c[i * n + j] = s;
    }
  }

  let sum = 0;
  for (let i = 0; i < n * n; i++) {
    sum = sum + Math.trunc(c[i]);
  }
  return sum;
}
const eng = typeof Bun !== "undefined" ? "bun/JSC" : "node/V8";
const t0 = process.hrtime.bigint();
const r = main();
const t1 = process.hrtime.bigint();
console.log(r);
console.log(`=> ${r} (${eng}, ran in ${Number(t1 - t0) / 1e6} ms)`);
