// First-class functions: pass functions as values, call them indirectly, and
// take function-typed parameters. Arrow lambdas `(x: T) => expr` are supported.
//
// v0 runs function values on the interpreter (the native JIT/LLVM tiers fall
// back for programs that use them); arrows can't yet capture enclosing locals.

function applyTwice(f: (n: i64) => i64, x: i64): i64 {
  return f(f(x));
}

function inc(n: i64): i64 {
  return n + 1;
}

function main(): i64 {
  const g = inc; // a named function used as a value
  const double = (n: i64) => n * 2; // an arrow lambda (return type inferred)

  console.log(applyTwice(double, 5)); // double(double(5)) = 20
  console.log(applyTwice(g, 10)); // inc(inc(10)) = 12
  console.log(applyTwice(inc, 100)); // pass a function directly: 102

  return applyTwice(double, 5) + applyTwice(g, 10); // 20 + 12 = 32
}
