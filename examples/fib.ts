// Real TypeScript, run on ZIPP via the oxc frontend. `i64` is ZIPP's integer
// type (a tsc-checkable alias would live in a zipp.d.ts); `number` maps to f64.
function fib(n: i64): i64 {
  if (n < 2) {
    return n;
  }
  return fib(n - 1) + fib(n - 2);
}

function main(): i64 {
  const r = fib(20);
  console.log(r);
  return r;
}
