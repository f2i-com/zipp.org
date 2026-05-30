// Closures: arrow lambdas that capture enclosing variables.
//
// Creating the closure snapshots the captured variable's current value into a
// small env struct, and the lifted function reads it back out — so `adder(10)`
// returns a function that remembers `n = 10`. (v0 captures by value.) Closures
// run natively on --jit via the env-pointer calling convention.

function adder(n: i64): (x: i64) => i64 {
  return (x: i64) => x + n; // captures the parameter `n`
}

function applyTwice(f: (n: i64) => i64, x: i64): i64 {
  return f(f(x));
}

function main(): i64 {
  const add10 = adder(10);
  const add100 = adder(100);
  console.log(add10(5)); // 15
  console.log(add100(5)); // 105

  const base = 3;
  const scale = (x: i64) => x * base; // captures the local `base`
  console.log(applyTwice(scale, 2)); // scale(scale(2)) = 18

  return add10(5) + add100(5) + applyTwice(scale, 2); // 15 + 105 + 18 = 138
}
