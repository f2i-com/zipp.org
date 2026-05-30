// More array methods: searching (indexOf/includes/findIndex), predicates
// (some/every), copying (slice/concat), and in-place ops (reverse/fill).
//
// All of these run natively on --jit and --llvm: the closure-based ones
// (some/every/findIndex) and the push-based ones (slice/concat) lower to
// per-element-type loop helpers over native push + indirect closure calls,
// while indexOf/includes/reverse/fill are plain native loops.

function main(): i64 {
  const xs = [5, 3, 8, 1, 9, 2];

  // searching
  console.log(xs.indexOf(8)); // 2
  console.log(xs.includes(9) ? 1 : 0); // 1

  // predicates (closures)
  console.log(xs.some((x: i64) => x > 8) ? 1 : 0); // 1  (9 > 8)
  console.log(xs.every((x: i64) => x > 0) ? 1 : 0); // 1
  console.log(xs.findIndex((x: i64) => x === 1)); // 3

  // slice (fresh copy, negative index) + concat
  const head = xs.slice(0, 3); // [5, 3, 8]
  const tail = xs.slice(-2); // [9, 2]
  const joined = head.concat(tail); // [5, 3, 8, 9, 2]
  console.log(len(joined)); // 5

  // in-place reverse + fill (mutate the receiver)
  const ys = [1, 2, 3, 4];
  ys.reverse(); // [4, 3, 2, 1]
  ys.fill(0, 2); // [4, 3, 0, 0]
  console.log(ys[0] * 1000 + ys[1] * 100 + ys[2] * 10 + ys[3]); // 4300

  return xs.indexOf(8) + len(joined) + ys[0]; // 2 + 5 + 4 = 11
}
