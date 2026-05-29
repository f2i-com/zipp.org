// Default parameter values, applied at the call site when an argument is
// omitted. A default must be constant (a literal, an enum member, etc.).

function pad(width: i64, amount: i64 = 4): i64 {
  return width + amount * 2;
}

class Box {
  w: i64;
  constructor(w: i64 = 10) {
    this.w = w;
  }
  grow(by: i64 = 1): i64 {
    this.w = this.w + by;
    return this.w;
  }
}

function main(): i64 {
  console.log(pad(100)); // 108 — amount defaults to 4
  console.log(pad(100, 1)); // 102
  const b = new Box(); // w defaults to 10
  b.grow(); // 11
  b.grow(9); // 20
  return pad(100) + b.w; // 108 + 20 = 128
}
