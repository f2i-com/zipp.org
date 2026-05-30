// A data-analysis pipeline — exercises growable arrays (push), the full
// array-method stdlib (map/filter/reduce/some/every/findIndex), method chaining,
// and closures (including one that captures a local). This is interpreter-tier
// (closures + push), so the native backends fall back to the interpreter.

// Build a deterministic dataset via push.
function build(n: i64): i64[] {
  const xs: i64[] = [];
  for (let i = 0; i < n; i = i + 1) {
    xs.push((i * 7 + 3) % 20);
  }
  return xs;
}

function main(): i64 {
  const xs = build(12);
  // values: 3, 10, 17, 4, 11, 18, 5, 12, 19, 6, 13, 0

  const sum = xs.reduce((a: i64, x: i64) => a + x, 0); // 118
  const evens = xs.filter((x: i64) => x % 2 === 0); // [10,4,18,12,6,0]
  const doubled = xs.map((x: i64) => x * 2); // [6,20,...]
  const allNonNeg = xs.every((x: i64) => x >= 0) ? 1 : 0; // 1
  const anyBig = xs.some((x: i64) => x > 18) ? 1 : 0; // 1  (19)
  const firstBig = xs.findIndex((x: i64) => x > 15); // 2  (17 at index 2)

  // a closure that captures a local, plus chaining (filter → map → reduce)
  const threshold = 10;
  const above = len(xs.filter((x: i64) => x > threshold)); // 6
  const scaledSumAbove = xs
    .filter((x: i64) => x > threshold)
    .map((x: i64) => x - threshold)
    .reduce((a: i64, x: i64) => a + x, 0); // (17+11+18+12+19+13) - 6*10 = 90 - 60 = 30

  console.log(sum); // 118
  console.log(len(evens)); // 6
  console.log(doubled[0]); // 6
  console.log(allNonNeg); // 1
  console.log(anyBig); // 1
  console.log(firstBig); // 2
  console.log(above); // 6
  console.log(scaledSumAbove); // 30

  return sum + above + scaledSumAbove; // 118 + 6 + 30 = 154
}
