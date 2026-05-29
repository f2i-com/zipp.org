// Test polymorphism with reduce
function main(): i64 {
  const xs = [1, 2, 3];
  
  const add = (a: i64, b: i64): i64 => a + b;
  const mul = (a: i64, b: i64): i64 => a * b;
  
  const sum = xs.reduce(add, 0);    // 1 + 2 + 3 = 6
  const prod = xs.reduce(mul, 1);   // 1 * 2 * 3 = 6
  
  return sum + prod;  // 6 + 6 = 12
}
