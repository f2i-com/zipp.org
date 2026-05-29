// Test polymorphism across different map callbacks
function main(): i64 {
  const xs = [1, 2, 3];
  
  const double = (x: i64): i64 => x * 2;
  const triple = (x: i64): i64 => x * 3;
  
  const ys = xs.map(double);  // [2, 4, 6]
  const zs = xs.map(triple);  // [3, 6, 9]
  
  return ys[0] + zs[0];  // 2 + 3 = 5
}
