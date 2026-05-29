// A loop with sized integers + arrays, in TypeScript. Sums 1..=100 in a u64,
// and also sums an array via `.length` indexing.
function main(): i64 {
  let total: u64 = u64(0);
  let i = 1;
  while (i <= 100) {
    total = total + u64(i);
    i = i + 1;
  }
  const xs = [10, 20, 30, 40];
  let s = 0;
  let j = 0;
  while (j < xs.length) {
    s = s + xs[j];
    j = j + 1;
  }
  console.log(i64(total)); // 5050
  console.log(s);          // 100
  return i64(total) + s;   // 5150
}
