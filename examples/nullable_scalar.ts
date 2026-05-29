// Nullable scalars: `i64 | null`, `f64 | null`, `bool | null`. Scalars have no
// spare null value, so these run on the interpreter (the native --jit/--llvm
// tiers fall back — they'd need boxing). Use `?? default` or narrow with
// `if (x !== null)`.

interface Config {
  retries: i64 | null;
  ratio: f64 | null;
}

function effectiveRetries(c: Config): i64 {
  return c.retries ?? 3; // default 3 when unset
}

function main(): i64 {
  const tuned: Config = { retries: 10, ratio: 1.5 };
  const blank: Config = { retries: null, ratio: null };

  console.log(effectiveRetries(tuned)); // 10
  console.log(effectiveRetries(blank)); // 3

  let maybe: i64 | null = 5;
  let total: i64 = 0;
  if (maybe !== null) {
    total = total + maybe; // narrowed to i64
  }
  maybe = null;
  total = total + (maybe ?? 100); // 5 + 100

  return effectiveRetries(tuned) + effectiveRetries(blank) + total; // 10 + 3 + 105
}
