// Functional/allocation-heavy benchmark — the regime the newly-native features
// live in (growable arrays + closures + map/filter/reduce), as opposed to the
// scalar numeric kernels (mandelbrot/matmul). Each rep builds an array with
// push, then runs a map (capturing a per-rep constant) → filter → reduce
// pipeline, so map/filter allocate fresh arrays every rep (real GC pressure).
//
// All intermediate values stay < 2^53 so ZIPP's i64 and JS's f64 `number` agree
// bit-for-bit on the checksum. Byte-for-byte the same kernel as pipeline.js.
function pipeline(reps: i64, n: i64): i64 {
  let acc = 0;
  let r = 0;
  while (r < reps) {
    let xs: i64[] = [];
    let i = 0;
    while (i < n) {
      xs.push((i + r) % 1000);
      i = i + 1;
    }
    const k = r % 7;
    const mapped = xs.map((x: i64) => (x * 3 + k) % 9973);
    const evens = mapped.filter((x: i64) => x % 2 === 0);
    const s = evens.reduce((a: i64, b: i64) => a + b, 0);
    acc = (acc + s) % 1000000007;
    r = r + 1;
  }
  return acc;
}

function main(): i64 {
  return pipeline(2000, 2000);
}
