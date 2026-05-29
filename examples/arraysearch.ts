// Pure search / in-place array methods — no closures, no push — so this runs
// NATIVELY on --jit and --llvm (not just the interpreter).

function main(): i64 {
  const xs = [5, 3, 8, 1, 9];
  xs.reverse(); // [9, 1, 8, 3, 5]   (in place)
  xs.fill(0, 3); // [9, 1, 8, 0, 0]   (in place, from index 3)
  const at = xs.indexOf(8); // 2
  const has = xs.includes(9) ? 1 : 0; // 1
  return at * 10 + has + xs[0]; // 2*10 + 1 + 9 = 30
}
