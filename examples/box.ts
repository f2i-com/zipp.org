// Generic classes are monomorphized: each `Box<T>` / `Pair<A, B>` the program
// uses becomes its own struct with its own factory and methods — no boxing and
// no runtime type info, like Rust/C++. `Box<i64>` and `Box<bool>` below are two
// entirely separate specializations.

class Box<T> {
  value: T;
  constructor(v: T) {
    this.value = v;
  }
  get(): T {
    return this.value;
  }
  set(v: T): T {
    this.value = v;
    return v;
  }
}

class Pair<A, B> {
  first: A;
  second: B;
  constructor(a: A, b: B) {
    this.first = a;
    this.second = b;
  }
}

function main(): i64 {
  const n = new Box<i64>(10); // Box<i64>
  n.set(25);
  const flag = new Box<bool>(true); // Box<bool> — a distinct struct
  const p = new Pair<i64, bool>(7, false); // Pair<i64, bool>

  let total: i64 = n.get(); // 25
  if (flag.get()) {
    total = total + p.first; // 25 + 7
  }
  console.log(total); // 32
  return total; // 32
}
