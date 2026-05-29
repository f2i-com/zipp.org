// Generic functions are monomorphized: each call is specialized to concrete
// types (inferred, or explicit `f<T>(...)`), so the native backends only ever
// compile concrete code — zero runtime overhead, like Rust/C++ templates.
// (The `extends number` constraints are for tsc; ZIPP monomorphizes either way.)

function identity<T>(x: T): T {
  return x;
}

function maxOf<T extends number>(a: T, b: T): T {
  if (a > b) {
    return a;
  }
  return b;
}

// clamp instantiates maxOf at whatever type clamp itself is used at
function clamp<T extends number>(x: T, lo: T, hi: T): T {
  const low = maxOf<T>(x, lo);
  if (low > hi) {
    return hi;
  }
  return low;
}

function main(): i64 {
  const a = identity(10); // identity<i64>
  const b = maxOf(a, 7); // maxOf<i64> -> 10
  const c = maxOf(2.5, 4.0); // maxOf<f64> -> 4.0 (a separate specialization)
  const d = clamp<i64>(99, 0, 50); // clamp<i64> -> 50
  console.log(b); // 10
  console.log(d); // 50
  return b + d + i64(c); // 10 + 50 + 4 = 64
}
