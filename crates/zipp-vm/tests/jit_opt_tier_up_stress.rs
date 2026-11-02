//! jit_opt_tier_up's program with every third Tier O guard check failing.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const EXPECTED: &str = r####"late 911494
sort 0s0 1008s7
chain 1679784
walk 20720
guarded 15925570"####;

const PROGRAM: &str = r####"// Tier O after tier-up: bodies whose later branches first run long after
// their first compile, a callback that tiers up inside a builtin, and a
// recursion that tiers up while its own activations are live.
const out = [];

class P {
  constructor(x, y) {
    this.x = x;
    this.y = y;
  }
  add(o) {
    return new P(this.x + o.x, this.y + o.y);
  }
  get len2() {
    return this.x * this.x + this.y * this.y;
  }
}

// The branch past the early return runs only from call 3000 on.
function late(i, scene) {
  if (i < 3000) return i & 7;
  const hit = { p: new P(i, 1), n: scene.normal, m: scene.mats[i % 3] };
  const q = hit.p.add(hit.n);
  return q.len2 % 1000 + hit.m.k + scene.lookup.get(hit.m.name);
}
{
  const scene = {
    normal: new P(0, 1),
    mats: [{ k: 1, name: "a" }, { k: 2, name: "b" }, { k: 3, name: "c" }],
    lookup: new Map([["a", 10], ["b", 20], ["c", 30]]),
  };
  let t = 0;
  for (let i = 0; i < 9000; i++) t = (t + late(i, scene)) % 1000003;
  out.push("late " + t);
}

// A comparator that tiers up in the middle of one sort.
{
  const xs = [];
  for (let i = 0; i < 5000; i++) xs.push({ k: (i * 7919) % 1009, s: "s" + (i % 17) });
  const sorted = xs.slice().sort((a, b) => a.k - b.k || (a.s < b.s ? -1 : a.s > b.s ? 1 : 0));
  out.push("sort " + sorted[0].k + sorted[0].s + " " + sorted[4999].k + sorted[4999].s);
  const total = xs.map((o) => o.k * 2).filter((v) => v % 3 === 0).reduce((a, v) => a + v, 0);
  out.push("chain " + total);
}

// A recursion that crosses the count in its own depth.
function walk(n, acc) {
  if (n === 0) return acc.x + acc.y;
  const next = n & 1 ? acc.add(new P(1, 0)) : acc.add(new P(0, 2));
  return walk(n - 1, next);
}
{
  let t = 0;
  for (let r = 0; r < 40; r++) t = (t + walk(300 + r, new P(r, r))) % 1000003;
  out.push("walk " + t);
}

// Tiered up, then fed a new shape and a throw through finally.
function guarded(o) {
  let r = 0;
  try {
    if (o.bad) throw new Error("bad " + o.v);
    r = o.v * 2;
  } finally {
    r += 1;
  }
  return r;
}
{
  let t = 0;
  for (let i = 0; i < 4000; i++) {
    const o = i < 3500 ? { v: i } : { bad: i % 50 === 0, v: i, extra: 1 };
    try {
      t += guarded(o);
    } catch (e) {
      t += e.message.length;
    }
  }
  out.push("guarded " + t);
}

console.log(out.join("\n"));
"####;

#[test]
fn matches_node() {
    // Read once per process, before the first VM: this file's only test.
    std::env::set_var("ZIPP_OPT", "1");
    std::env::set_var("ZIPP_OPT_DEOPT_EVERY", "3");
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), EXPECTED);
}
