// Byte-for-byte the same kernel as mandelbrot.zipp, for a V8 (Node) comparison.
// Run: node bench/mandelbrot.js
function main() {
  const width = 1000;
  const height = 1000;
  const maxIter = 256;
  let total = 0;
  for (let py = 0; py < height; py++) {
    for (let px = 0; px < width; px++) {
      const cx = (px / width) * 3.5 - 2.5;
      const cy = (py / height) * 2.0 - 1.0;
      let x = 0.0;
      let y = 0.0;
      let iter = 0;
      while (x * x + y * y <= 4.0 && iter < maxIter) {
        const xt = x * x - y * y + cx;
        y = 2.0 * x * y + cy;
        x = xt;
        iter++;
      }
      total += iter;
    }
  }
  return total;
}
const t0 = process.hrtime.bigint();
const r = main();
const t1 = process.hrtime.bigint();
console.log(r);
console.log(`=> ${r} (node/V8, ran in ${Number(t1 - t0) / 1e6} ms)`);
