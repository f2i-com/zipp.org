// Nullable heap references: `str | null` and `T[] | null`. Like nullable
// structs, these are just a pointer that may be null — no boxing — so they run
// natively on --jit and --llvm too. Default with `??` or narrow with
// `if (x !== null)`.

function greet(name: str | null): str {
  return "hi " + (name ?? "guest");
}

function firstTwo(xs: i64[] | null): i64 {
  if (xs !== null) {
    return xs[0] + xs[1]; // narrowed to i64[]
  }
  return 0;
}

function main(): i64 {
  console.log(greet("Ada")); // "hi Ada"
  console.log(greet(null)); // "hi guest"

  const some: i64[] | null = [10, 20, 30];
  const none: i64[] | null = null;
  console.log(firstTwo(some)); // 30
  console.log(firstTwo(none)); // 0

  // len("hi Ada")=6, len("hi guest")=8 → 14, plus 30 + 0
  return len(greet("Ada")) + len(greet(null)) + firstTwo(some) + firstTwo(none);
}
