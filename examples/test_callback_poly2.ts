// Test with inline arrow functions
function main(): i64 {
  const xs = [1, 2, 3, 4, 5];
  
  // Inline arrows with same signature
  const r1 = xs.some((x: i64): bool => x > 2);
  const r2 = xs.some((x: i64): bool => x < 4);
  const r3 = xs.every((x: i64): bool => x > 0);
  
  return (r1 ? 1 : 0) + (r2 ? 1 : 0) + (r3 ? 1 : 0);
}
