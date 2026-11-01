//! An object taken ready-made (`Heap::ready`), in compiled code or the
//! interpreter, behaves as one built property by property: every mutation, every
//! slab class, heap values through collections, side tables left by the object
//! it recycled. Node's output.

const EXPECTED: &str = r####"599970000
6 299000 -250000
4636500
168000
499501500
300000
400000
450000
50000
1990000
499500
499500
1999000"####;

#[test]
fn ready_objects_child() {
    if std::env::var_os("ZIPP_READY_OBJECTS_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, EXPECTED.lines().collect::<Vec<_>>());
}

#[test]
fn ready_objects_match_node_in_every_mode() {
    if std::env::var_os("ZIPP_READY_OBJECTS_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("jit", None),
        ("nojit", Some(("ZIPP_NOJIT", "1"))),
        ("forced", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gcstress", Some(("ZIPP_GC_STRESS", "1"))),
        ("verify", Some(("ZIPP_NURSERY_VERIFY", "1"))),
        ("off", Some(("ZIPP_NO_READY_OBJECTS", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "ready_objects_child", "--nocapture"])
            .env("ZIPP_READY_OBJECTS_CHILD", "1")
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

const PROGRAM: &str = r####"// Object literals made by every path, then used every way an object can be:
// each must behave as an ordinary object made one property at a time.
function lit(a, b) { return { x: a, y: b }; }
function keep(n) { var out = []; for (var i = 0; i < n; i++) out.push(lit(i, i * 2)); return out; }
var kept = keep(20000);
var sum = 0;
for (var i = 0; i < kept.length; i++) sum += kept[i].x + kept[i].y;
console.log(sum);

// Churn past several collections, with survivors in old holders.
var old = [];
for (var i = 0; i < 300000; i++) {
  var o = lit(i, -i);
  if (i % 50000 === 0) old.push(o);
  if (i % 1000 === 0) old[0].last = o;
}
console.log(old.length, old[0].last.x, old[old.length - 1].y);

// Every mutation of a fresh literal.
function fresh(i) { return { a: i, b: i + 1, c: i + 2 }; }
function exercise(n) {
  var r = 0;
  for (var i = 0; i < n; i++) {
    var o = fresh(i);
    o.d = 1; // append within the cell
    o.e = 2; // append past it
    delete o.a;
    r += Object.keys(o).length + (o.b | 0);
    var p = fresh(i);
    Object.defineProperty(p, "b", { get() { return 7; } });
    r += p.b;
    var q = fresh(i);
    Object.freeze(q);
    q.a = 99;
    r += q.a === i ? 1 : 0;
    var s = fresh(i);
    Object.setPrototypeOf(s, { z: 5 });
    r += s.z;
    var t = fresh(i);
    t.__proto__ = null;
    r += "toString" in t ? 0 : 1;
    r += JSON.stringify(fresh(1)).length;
    for (var k in fresh(2)) r += k.length;
    var { a, ...rest } = fresh(3);
    r += Object.keys(rest).length;
    r += Object.entries({ ...fresh(4) }).length;
  }
  return r;
}
console.log(exercise(3000));

// Sizes around every slab class, kept and read back.
function sized() {
  var all = [];
  for (var i = 0; i < 2000; i++) {
    all.push({ a: 1 }, { a: 1, b: 2, c: 3, d: 4 }, { a: 1, b: 2, c: 3, d: 4, e: 5 },
      { a: 1, b: 2, c: 3, d: 4, e: 5, f: 6 }, { a: 1, b: 2, c: 3, d: 4, e: 5, f: 6, g: 7 },
      { a: 1, b: 2, c: 3, d: 4, e: 5, f: 6, g: 7, h: 8 },
      { a: 1, b: 2, c: 3, d: 4, e: 5, f: 6, g: 7, h: 8, i: 9, j: 10, k: 11 });
  }
  var r = 0;
  for (var o of all) { var ks = Object.keys(o); r += ks.length + o[ks[ks.length - 1]]; }
  return r;
}
console.log(sized());

// Heap values in a literal, young and old, through a collection.
function nest(n) {
  var head = null;
  for (var i = 0; i < n; i++) head = { v: "s" + (i % 7), next: i % 1000 ? head : null, arr: [i], f: function () { return i; } };
  var c = 0;
  for (var p = head; p; p = p.next) { c += p.v.length + p.arr[0] + p.f(); }
  return c;
}
console.log(nest(250000));

// A literal's result in a getter, a class field, a default, a closure.
class K { f = { p: 1, q: 2 }; get g() { return { r: this.f.p }; } }
var total = 0;
for (var i = 0; i < 100000; i++) { var k = new K(); total += k.g.r + k.f.q; }
console.log(total);
function dflt(o = { w: 1 }) { return o.w; }
for (var i = 0; i < 100000; i++) total += dflt();
console.log(total);
var mk = (function () { var n = 0; return function () { return { n: n++ }; }; })();
for (var i = 0; i < 100000; i++) total += mk().n & 1;
console.log(total);

// WeakRef-free identity: distinct objects, each its own.
var seen = new Set();
for (var i = 0; i < 50000; i++) seen.add({ i: i });
console.log(seen.size);
var m = new WeakMap();
var ks = [];
for (var i = 0; i < 20000; i++) { var key = { i: i }; m.set(key, i); if (i % 100 === 0) ks.push(key); }
console.log(ks.reduce((a, k) => a + m.get(k), 0));

// A literal in a generator and across awaits.
function* gen() { for (var i = 0; i < 1000; i++) yield { i: i }; }
var g = 0;
for (var o of gen()) g += o.i;
console.log(g);
(async function () {
  var a = 0;
  for (var i = 0; i < 2000; i++) { var o = await { i: i }; a += o.i; }
  console.log(a);
})();

// A child realm: its literals have that realm's Object.prototype. (ShadowRealm
// where it exists; nothing printed differs otherwise.)
try {
  if (typeof ShadowRealm === "function") { new ShadowRealm().evaluate("({a:1}).a"); }
} catch (e) {}
var after = 0;
for (var i = 0; i < 1000; i++) after += ({ a: i }).a;
console.log(after);
"####;
