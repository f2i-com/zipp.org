// String enums (members are `str`) plus array and object destructuring.

enum Level {
  Low = "low",
  High = "high",
}

interface Point {
  x: i64;
  y: i64;
}

function tag(level: Level): str {
  return "level:" + level; // string enum value concatenates like any str
}

function main(): i64 {
  console.log(tag(Level.High)); // "level:high"

  const xs: i64[] = [10, 20, 30, 40];
  const [first, , third] = xs; // array destructuring, skipping the second
  console.log(first + third); // 40

  const p: Point = { x: 6, y: 7 };
  const { x, y: height } = p; // object destructuring (shorthand + renamed)
  console.log(x * height); // 42

  return first + third + x * height; // 40 + 42 = 82
}
