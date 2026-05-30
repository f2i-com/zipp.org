// Closure-call-throughput benchmark — isolates indirect-call overhead (the
// env-pointer ABI) from allocation. A capturing closure (an LCG step that
// closes over `mul`/`inc`) is called ~20M times in a tight loop. `x * mul` stays
// < 2^53 (mul is small) so ZIPP's i64 and JS's f64 agree on the checksum.
// Byte-for-byte the same kernel as closures.js.
function main(): i64 {
  const mul = 1000003;
  const inc = 12345;
  const step = (x: i64) => (x * mul + inc) % 2147483648;
  let seed = 1;
  let acc = 0;
  let i = 0;
  while (i < 20000000) {
    seed = step(seed);
    acc = (acc + (seed % 100)) % 1000000007;
    i = i + 1;
  }
  return acc;
}
