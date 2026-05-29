// Test polymorphism across different callbacks with same signature
function main(): i64 {
  const xs = [1, 2, 3, 4, 5];
  
  const f = (x: i64): bool => x > 2;
  const g = (x: i64): bool => x < 4;
  
  // If the bug exists, the second call will fail type checking
  // because __some_i64's parameter f has type Func(fid_f)
  // but we're trying to pass g with type Func(fid_g), and they're different.
  const r1 = xs.some(f);  // first call to some with f
  const r2 = xs.some(g);  // second call to some with g
  
  return r1 ? 1 : 0;
}
