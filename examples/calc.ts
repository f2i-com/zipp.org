// `switch` lowers to an if/else-if chain over a hoisted discriminant. The sound
// subset forbids fall-through (each non-empty case ends with break/return), but
// empty cases may stack (`case 9: case 10:`), and `default` is the final else.

enum Op {
  Add,
  Sub,
  Mul,
  Div,
}

function apply(op: Op, a: i64, b: i64): i64 {
  switch (op) {
    case Op.Add:
      return a + b;
    case Op.Sub:
      return a - b;
    case Op.Mul:
      return a * b;
    case Op.Div:
      return a / b;
    default:
      return 0;
  }
}

function grade(score: i64): i64 {
  let g: i64 = 0;
  switch (score) {
    case 10:
    case 9:
      g = 1; // 9 and 10 both land here
      break;
    case 8:
      g = 2;
      break;
    default:
      g = 5;
      break;
  }
  return g;
}

function main(): i64 {
  const x = apply(Op.Mul, 6, 7); // 42
  console.log(x);
  console.log(grade(9)); // 1
  console.log(grade(3)); // 5 (default)
  return x + grade(9) + grade(3); // 42 + 1 + 5 = 48
}
