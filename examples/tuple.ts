// Tuple types `[T0, T1, ...]` — fixed-size and heterogeneous. They lower to a
// struct with positional fields ("0", "1", …), so they run natively on every
// struct-capable backend. Construct with an array literal in a tuple context;
// read by index or destructuring.

function divmod(a: i64, b: i64): [i64, i64] {
  return [a / b, a % b]; // return multiple values
}

function label(id: i64, name: str): [i64, str] {
  return [id, name];
}

function main(): i64 {
  const [q, r] = divmod(17, 5); // destructure: q = 3, r = 2
  console.log(q);
  console.log(r);

  const tag = label(7, "widget");
  console.log(tag[0]); // 7
  console.log(tag[1]); // "widget"

  return q * 100 + r + tag[0] + len(tag[1]); // 302 + 7 + 6 = 315
}
