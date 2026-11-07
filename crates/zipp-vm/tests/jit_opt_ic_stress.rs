//! jit_opt_ic's program with every third Tier O guard check failing.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const EXPECTED: &str = r####"k0:12|k96:0
k0:12|k96:0
k0:12|k96:0
k0:12|k96:0
k0:12|k96:0
k0:12|k96:0
store 208893
paths -908482204
step 296435
g 10000"####;

const PROGRAM: &str = r####"// Tier O: a sort comparator that changes body mid-sort, split Map / Set
// calls on captured collections, reads over four shapes, and the inline
// cache lane over Map receivers, fresh records and class fields.
const out = [];

// A comparator compiled with an int compare first meets strings mid-sort.
{
  const rows = [];
  for (let i = 0; i < 600; i++) rows.push([`k${(i * 7919) % 97}`, i % 13]);
  for (let r = 0; r < 6; r++) {
    const top = rows.slice().sort((x, y) => y[1] - x[1] || (x[0] < y[0] ? -1 : x[0] > y[0] ? 1 : 0));
    out.push(top[0].join(":") + "|" + top[top.length - 1].join(":"));
  }
}

// Split member calls on collections a closure captured.
function makeStore() {
  const byKey = new Map();
  const seen = new Set();
  let n = 0;
  return function put(k, v) {
    n++;
    const old = byKey.get(k);
    byKey.set(k, old === undefined ? v : old + v);
    if (!seen.has(k)) seen.add(k);
    if ((n & 255) === 0) byKey.delete("k3");
    return byKey.size * 1000 + seen.size;
  };
}
{
  const put = makeStore();
  let s = 0;
  for (let i = 0; i < 4000; i++) s = (s + put("k" + (i % 37), i)) % 1000003;
  out.push("store " + s);
}

// One read site over four layouts, a fifth late.
function pathOf(req) {
  return req.path + ":" + req.id;
}
{
  const shapes = [
    (i) => ({ path: "/a", id: i, user: "u" }),
    (i) => ({ id: i, q: "x", path: "/b" }),
    (i) => ({ code: 1, ok: true, id: i, path: "/c" }),
    (i) => ({ extra: i, path: "/d", id: i }),
  ];
  let h = 0;
  for (let i = 0; i < 6000; i++) {
    const r = i < 5000 ? shapes[i % 4](i) : { z: 1, y: 2, id: i, path: "/e" };
    const p = pathOf(r);
    h = (h * 31 + p.length + p.charCodeAt(1)) | 0;
  }
  out.push("paths " + h);
}

// Fresh records, class fields and a Map read through a captured binding,
// in a function compiled before its later branches ever ran.
class V {
  x;
  y;
  constructor(x, y) {
    this.x = x;
    this.y = y;
  }
  plus(o) {
    return new V(this.x + o.x, this.y + o.y);
  }
}
const table = new Map([["a", 1], ["b", 2]]);
function step(i) {
  if (i < 20) return 0;
  const rec = { v: new V(i, i + 1), w: new V(1, 2), tag: i & 1 ? "a" : "b" };
  const sum = rec.v.plus(rec.w);
  return sum.x + sum.y + table.get(rec.tag);
}
{
  let t = 0;
  for (let i = 0; i < 5000; i++) t = (t + step(i)) % 1000003;
  table.set("a", 10);
  for (let i = 0; i < 500; i++) t = (t + step(i + 20)) % 1000003;
  out.push("step " + t);
}

// A getter on the prototype a read site later meets.
{
  const base = { get g() { return 7; } };
  function readG(o) {
    return o.g;
  }
  let t = 0;
  for (let i = 0; i < 3000; i++) t += readG(i < 2000 ? { g: i & 3 } : Object.create(base));
  out.push("g " + t);
}

console.log(out.join("\n"));
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
