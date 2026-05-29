// The ternary `cond ? a : b` is a real conditional expression in ZIPP: it lowers
// to a branch that writes a single result register (like `&&`/`||`), so only the
// taken branch is evaluated — which is why the recursive `fib` below terminates.

function mag(n: i64): i64 {
  return n < 0 ? 0 - n : n;
}

function fib(n: i64): i64 {
  return n < 2 ? n : fib(n - 1) + fib(n - 2);
}

function grade(score: i64): i64 {
  return score >= 90 ? 1 : score >= 80 ? 2 : score >= 70 ? 3 : 5;
}

function main(): i64 {
  console.log(mag(-12)); // 12
  console.log(fib(10)); // 55
  console.log(grade(85)); // 2
  return mag(-12) + fib(10) + grade(85); // 12 + 55 + 2 = 69
}
