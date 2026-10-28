//! Arithmetic compiled code has run on doubles, recompiled by Tier C as
//! double-only ops over its operand cache (`FloatCache`), then given what
//! the cache must not get wrong: Ints, -0, NaN, Infinity, strings, BigInts
//! and objects with `valueOf` (whose calls must happen in order), registers
//! rewritten by other ops between reads, values carried round loops, more
//! live values than the cache has registers, and an `Add` that concatenates
//! after warming on doubles. Run interpreted, at the default thresholds, with
//! every function compiled at once, under GC stress and with the cache off;
//! the expected lines are Node 24's.

const EXPECTED: &str = "446720267849.6347
210 -108.1875 42 1576.3200532973424 0.9285714285714285
0 0 NaN NaN NaN 0 NaN 2 NaN
72.18441403926235 TypeError 70 abca
446720267849.6347
210 -108.1875 42 1576.3200532973424 0.9285714285714285
0 0 NaN NaN NaN 0 NaN 2 NaN
72.18441403926235 TypeError 70 abca
446720267849.6347
210 -108.1875 42 1576.3200532973424 0.9285714285714285
0 0 NaN NaN NaN 0 NaN 2 NaN
72.18441403926235 TypeError 70 abca
2251500
x10.5
1.5y0.5
0";

#[test]
fn float_cache_child() {
    if std::env::var_os("ZIPP_FLOAT_CACHE_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, EXPECTED.lines().collect::<Vec<_>>());
}

#[test]
fn compiled_double_ops_match_the_interpreter() {
    if std::env::var_os("ZIPP_FLOAT_CACHE_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("cache-off", Some(("ZIPP_NO_FLOAT_CACHE", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "float_cache_child", "--nocapture"])
            .env("ZIPP_FLOAT_CACHE_CHILD", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS");
        if let Some((k, v)) = env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("re-run test binary");
        assert!(
            out.status.success(),
            "{mode}:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

const PROGRAM: &str = r####"// Arithmetic compiled code has seen on doubles, recompiled as double-only ops
// over Tier C's operand cache, then fed what the cache must not get wrong:
// Ints, -0, NaN, Infinity, strings, BigInts and objects with valueOf in the
// same ops; registers rewritten by other ops between reads; values carried
// round loops; more live values than the cache has registers.
"use strict";
const out = [];
const show = (v) => (Object.is(v, -0) ? "-0" : typeof v === "bigint" ? v + "n" : String(v));

// One straight run of double ops reading each other's results.
function chain(a, b, c) {
  const x = a * b;
  const y = x - c;
  const z = y / (a + 0.5);
  const w = x * x + y * y - z * z;
  return x + y + z + w;
}
// Ten live values at once (eight cache registers).
function wide(a, b) {
  const v0 = a * 1.5, v1 = b * 2.5, v2 = a - b, v3 = a + b, v4 = v0 * v1;
  const v5 = v2 * v3, v6 = v4 - v5, v7 = v0 + v1, v8 = v6 * 0.25, v9 = v7 - v8;
  return v0 + v1 + v2 + v3 + v4 + v5 + v6 + v7 + v8 + v9 + (v0 * v9 - v1 * v8 + v2 * v7 - v3 * v6);
}
// A register rewritten by a non-double op between two double reads.
function rewrite(a, b) {
  let x = a * 1.25;
  let y = x * b;
  x = (x | 0) + 1;
  y = y - x * 0.5;
  x = typeof y === "number" ? x * 2.5 : 0;
  return x + y;
}
// Values carried round a loop and read after it.
function loop(n, a) {
  let s = 0.5, t = a * 0.75;
  for (let i = 0; i < n; i++) {
    s = s * 0.5 + t;
    t = t - s * 0.125;
    if (i % 7 === 3) t = t + 1;
  }
  return s * 1000 + t;
}
// The same ops on objects' fields (GetProp results feed them).
function fields(p, q) {
  const dx = p.x - q.x, dy = p.y - q.y;
  const d2 = dx * dx + dy * dy;
  const k = 1 / (d2 + 1);
  p.vx -= dx * k;
  q.vx += dx * k * 0.5;
  return d2 * k;
}

function warm() {
  let s = 0;
  for (let i = 0; i < 3000; i++) {
    s += chain(i * 0.5 + 0.25, 1.5, 0.75) + wide(i * 0.25 + 0.1, 0.3) + rewrite(i * 0.5 + 0.5, 1.75);
    s += loop(10, i * 0.125 + 0.5) + fields({ x: i * 0.5, y: 1.5, vx: 0.5 }, { x: 0.25, y: i * 0.125, vx: 0.75 });
  }
  return s;
}
for (let round = 0; round < 3; round++) {
  out.push(show(warm()));
  // Ints into the double ops: the same numbers.
  out.push([chain(3, 4, 5), wide(2, 3), rewrite(4, 6), loop(20, 3), fields({ x: 3, y: 4, vx: 1 }, { x: 1, y: 1, vx: 2 })].map(show).join(" "));
  // -0, NaN, Infinity.
  out.push([chain(-0, 1, 0), chain(0, -1, 0), chain(NaN, 1, 1), chain(Infinity, 1, 1), chain(-Infinity, 0, 0), wide(-0, 0), wide(1e308, 1e308), rewrite(-0, 0), loop(5, NaN)].map(show).join(" "));
  // A string, BigInt and valueOf object reach a double-only op: the interpreter's result.
  let r = [];
  try { r.push(show(chain("2", "3", "1"))); } catch (e) { r.push(e.constructor.name); }
  try { r.push(show(chain(2n, 3n, 1n))); } catch (e) { r.push(e.constructor.name); }
  const order = [];
  const o = (v, tag) => ({ valueOf() { order.push(tag); return v; } });
  try { r.push(show(chain(o(2, "a"), o(3, "b"), o(1, "c")))); } catch (e) { r.push(e.constructor.name); }
  r.push(order.join(""));
  out.push(r.join(" "));
}
// An Add warmed on doubles that then concatenates.
function addMix(a, b) { return a + b + 0.5; }
let am = 0;
for (let i = 0; i < 3000; i++) am += addMix(i * 0.5, 0.25);
out.push(show(am), addMix("x", 1), addMix(1.5, "y"), show(addMix(-0, -0.5)));
console.log(out.join("\n"));
"####;
