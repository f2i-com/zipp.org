// Growable arrays + array methods. `push`/`pop` mutate; `map`/`filter`/`reduce`
// take callbacks (closures welcome) and build/fold arrays. Runs natively on
// --jit and --llvm: arrays use a Vec-style {len,cap,data} header, and the
// method helpers lower to push + indirect closure calls (both native).

function isEven(n: i64): bool {
  return n % 2 === 0;
}

function main(): i64 {
  // build a growable array with push
  const xs: i64[] = [];
  for (let i = 1; i <= 5; i = i + 1) {
    xs.push(i * i); // 1, 4, 9, 16, 25
  }
  console.log(len(xs)); // 5
  console.log(xs.pop()!); // 25  →  xs is now [1, 4, 9, 16]

  // map: double each element
  const doubled = xs.map((x: i64) => x * 2); // [2, 8, 18, 32]
  console.log(doubled[3]); // 32

  // filter with a named predicate
  const evens = xs.filter(isEven); // [4, 16]
  console.log(len(evens)); // 2

  // reduce to a sum
  const total = xs.reduce((acc: i64, x: i64) => acc + x, 0); // 30

  // reduce with a closure that captures a local
  const factor = 10;
  const scaled = xs.reduce((acc: i64, x: i64) => acc + x * factor, 0); // 300

  console.log(total); // 30
  return total + scaled; // 330
}
