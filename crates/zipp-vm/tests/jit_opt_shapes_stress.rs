//! jit_opt_shapes's program with every third Tier O guard check failing.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const EXPECTED: &str = r####"63524 69071.606949 50000 59"####;

const PROGRAM: &str = r####"// Tier O: truthiness of heap values, shape checks reused within a block
// (and forgotten across what may reshape), array destructuring.
const out = [];
function truthy(xs) {
  let t = 0;
  for (let i = 0; i < xs.length; i++) { const x = xs[i]; if (x) t += 1 << (i % 20); if (!x) t -= 1; }
  return t;
}
const vals = [{}, [], "", "a", 0n, 1n, null, undefined, 0, -0, NaN, 1.5, "0", Symbol("s"), function () {}, [0]];
let tt = 0;
for (let k = 0; k < 300; k++) tt = truthy(vals);
out.push(tt);
function body(p, q, dt) {
  // Several reads and writes of the same objects in one block.
  const dx = p.x - q.x, dy = p.y - q.y;
  const d2 = dx * dx + dy * dy + 0.01;
  p.vx -= dx * q.m / d2 * dt; p.vy -= dy * q.m / d2 * dt;
  q.vx += dx * p.m / d2 * dt; q.vy += dy * p.m / d2 * dt;
  p.x += p.vx * dt; p.y += p.vy * dt;
  return p.x + p.y + q.x;
}
function grow(o) { o.extra = 1; delete o.extra; return 0; }
function reshape(p, q) {
  const a = p.x;
  grow(p);              // may add and remove a property between the reads
  return a + p.x + q.vx;
}
const ps = [];
for (let i = 0; i < 8; i++) ps.push({ x: i, y: i * 2, vx: 0, vy: 0, m: 1 + i });
let bs = 0;
for (let k = 0; k < 400; k++) for (let i = 0; i < 7; i++) bs += body(ps[i], ps[i + 1], 0.01) + reshape(ps[i], ps[i + 1]);
out.push(bs.toFixed(6));
function destr(pairs) { let t = 0; for (let i = 0; i < pairs.length; i++) { const [a, b, c = 5] = pairs[i]; t += a * b + c; } return t; }
const pairs = [[1, 2], [3, 4, 6], [5, 6], [7, 8, 9]];
let dt = 0;
for (let k = 0; k < 400; k++) dt += destr(pairs);
out.push(dt, destr([[1, 2], "34", new Set([5, 6])]));
console.log(out.join(" "));
"####;

#[test]
fn matches_node() {
    // Read once per process, before the first VM: this file's only test.
    std::env::set_var("ZIPP_OPT", "1");
    std::env::set_var("ZIPP_NO_OPT_TIER_UP", "1");
    std::env::set_var("ZIPP_OPT_DEOPT_EVERY", "3");
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), EXPECTED);
}
