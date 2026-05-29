// TypeScript interfaces become ZIPP structs. Construction via a typed `let` or
// `{...} as T`; field read/write and struct params/returns all work.
interface Point {
  x: i64;
  y: i64;
}

function dist2(p: Point): i64 {
  return p.x * p.x + p.y * p.y;
}

function main(): i64 {
  const a: Point = { x: 3, y: 4 };
  const b = { x: 6, y: 8 } as Point;
  console.log(dist2(a)); // 25
  console.log(dist2(b)); // 100
  return dist2(a) + dist2(b); // 125
}
