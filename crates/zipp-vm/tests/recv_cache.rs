//! Property reads and writes through a receiver compiled code has already
//! verified at its shape (the receiver cache, `RecvCache`), with what may
//! change the receiver between them: another object's getter that deletes,
//! adds and redefines its properties, a setter that freezes it, the register
//! reassigned or aliased, `(x + y) | 0` and compare-and-branch folds between
//! accesses, and young values stored through it with collections between
//! rounds. Run interpreted, at the default thresholds, with every function
//! compiled at once, under GC stress and with the cache off; the expected
//! lines are Node 24's.

const EXPECTED: &str = "315000.379275
21069.429129
0.904435
60220000
1102,3046,5000,7108,9052,11006,13114,15058,17012
400000000
TypeError
5
0
381881
1600000000
39999
39999/40000
2000";

#[test]
fn recv_cache_child() {
    if std::env::var_os("ZIPP_RECV_CACHE_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, EXPECTED.lines().collect::<Vec<_>>());
}

#[test]
fn verified_receivers_see_every_change() {
    if std::env::var_os("ZIPP_RECV_CACHE_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("cache-off", Some(("ZIPP_NO_RECV_CACHE", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "recv_cache_child", "--nocapture"])
            .env("ZIPP_RECV_CACHE_CHILD", "1")
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

const PROGRAM: &str = r####"// Property reads and writes through a receiver compiled code has already
// verified, with what may change that receiver between them: another
// object's getter that deletes, adds or redefines its properties, a setter
// that freezes it, the register reassigned or aliased, `(x + y) | 0` and
// compare-and-branch folds between accesses, and collections.
"use strict";
class P { constructor(x, y, z) { this.x = x; this.y = y; this.z = z; } }
const out = [];

// 1. A straight run over two receivers, as nbody's inner loop.
function pair(a, b, k) {
  const dx = a.x - b.x, dy = a.y - b.y, dz = a.z - b.z;
  const d = dx * dx + dy * dy + dz * dz + 1;
  a.x -= dx / d * k; a.y -= dy / d * k; a.z -= dz / d * k;
  b.x += dx / d * k; b.y += dy / d * k; b.z += dz / d * k;
  return a.x + a.y + a.z + b.x + b.y + b.z;
}
{
  const ps = [new P(1, 2, 3), new P(4.5, -1, 2), new P(-3, 0.25, 7)];
  let s = 0;
  for (let i = 0; i < 30000; i++) s += pair(ps[i % 3], ps[(i + 1) % 3], 0.001);
  out.push(s.toFixed(6));
  // The same object as both receivers: writes through one are reads of the other.
  let t = 0;
  for (let i = 0; i < 2000; i++) t += pair(ps[0], ps[0], 0.5);
  out.push(t.toFixed(6), ps[0].x.toFixed(6));
}

// 2. Between two reads of `a`, a read of `b` that warms as data and later
// meets a getter that deletes, adds and redefines `a`'s properties.
function between(a, b) {
  const first = a.x + a.y;
  const mid = b.v;
  const second = a.x + a.y + (a.z === undefined ? 100 : a.z);
  return first * 1000 + second + (typeof mid === "number" ? mid : 0);
}
{
  const a = new P(1, 2, 3);
  const plain = { v: 5 };
  let s = 0;
  for (let i = 0; i < 20000; i++) s += between(a, plain);
  out.push(s);
  const victims = [];
  const evil = {
    get v() {
      const n = victims.length;
      const t = victims[n - 1];
      if (n % 3 === 1) delete t.z;
      else if (n % 3 === 2) { delete t.x; t.x = 40; t.w = 9; }
      else Object.defineProperty(t, "y", { get() { return -7; } });
      return 1;
    },
  };
  const rs = [];
  for (let i = 0; i < 9; i++) {
    const t = new P(i, i + 1, i + 2);
    victims.push(t);
    rs.push(between(t, evil));
  }
  out.push(rs.join(","));
}

// 3. A setter on the second receiver freezes the first; the write after it
// must fail as a frozen object's does (a TypeError in strict code).
function frozenAfter(a, b, v) {
  a.x = v;
  b.s = v;
  a.y = v + 1;
  return a.x + a.y;
}
{
  const a = new P(0, 0, 0);
  const plain = { s: 0 };
  let s = 0;
  for (let i = 0; i < 20000; i++) s += frozenAfter(a, plain, i);
  out.push(s);
  const b = new P(0, 0, 0);
  const freezer = { set s(_) { Object.freeze(b); } };
  let msg = "none";
  try { frozenAfter(b, freezer, 5); } catch (e) { msg = e.constructor.name; }
  out.push(msg, b.x, b.y);
}

// 4. The receiver register reassigned between accesses (`o = o.next`), and
// folds between accesses.
function walk(head, n) {
  let o = head, acc = 0;
  for (let i = 0; i < n; i++) {
    acc = (acc + o.x) | 0;
    const big = o.y > 2 ? o.y : o.z;
    o = o.next;
    acc = (acc + big + o.x) | 0;
  }
  return acc;
}
{
  const nodes = [];
  for (let i = 0; i < 16; i++) nodes.push({ x: i, y: i % 5, z: 3, next: null });
  for (let i = 0; i < 16; i++) nodes[i].next = nodes[(i + 1) % 16];
  let s = 0;
  for (let r = 0; r < 3000; r++) s = (s + walk(nodes[r % 16], 7)) | 0;
  out.push(s);
}

// 5. Young objects stored into an old one's fields through a verified
// receiver, with allocation churn so collections run between rounds.
function store(holder, i) {
  holder.a = { i };
  holder.b = [i, i + 1];
  holder.c = holder.a.i + holder.b[1];
  return holder.c;
}
{
  const holder = { a: null, b: null, c: 0 };
  let s = 0, junk = [];
  for (let i = 0; i < 40000; i++) {
    s += store(holder, i);
    if (i % 1000 === 0) { junk = []; for (let j = 0; j < 2000; j++) junk.push({ j }); }
  }
  out.push(s, holder.a.i, holder.b.join("/"), junk.length);
}
console.log(out.join("\n"));
"####;
