//! for-in and Object.keys over shaped objects take their key list from a
//! per-shape cache (`Vm::enum_key_cache`), and a for-in over such an object
//! answers each visit's liveness from the receiver's version.
//!
//! A shape names a key sequence with its attributes, so every object of one
//! shape enumerates the same list; what must not change is anything a
//! program can see: integer-like keys first and in order, escaped and long
//! keys, keys deleted before their visit (skipped) and added during the
//! loop, non-enumerable, accessor and symbol-keyed properties,
//! `Object.prototype` gaining an enumerable key part way through, other
//! prototypes, class instances and null-prototype objects, objects that
//! leave their shape, more shapes than the cache holds, and arrays, strings
//! and functions in the same loops, and objects in dictionary mode (a key
//! deleted before the loop) with keys deleted, re-added and added during it.
//! The loops run past the tier-up count.
//! The expected lines are Node 24's output for the same program.

use zipp_vm::embed;

fn run(source: &str) -> Vec<String> {
    let mut state = embed::compile_script(source).expect("source compiles");
    #[cfg(feature = "safe-sandbox")]
    state.set_limits(4_000_000_000, None);
    state.run_init().expect("source runs");
    state.take_output()
}

const PROGRAM: &str = r####"// for-in and Object.keys over shaped objects, in loops hot enough to be
// compiled, with everything that can change what they must report.
var N = 3000;
var out = [];
function show(name, v) { out.push(name + ": " + v); }
function keysOf(o) { var ks = []; for (var k in o) ks.push(k); return ks.join(","); }

// 1. integer-like keys first and in order, then insertion order; escaped and long keys
(function () {
  var long = "k".repeat(70);
  var h = 0, last = "";
  for (var i = 0; i < N; i++) {
    var o = { b: 1, 2: 2, a: 3, 1: 4, "@@x": 5, "-1": 6, "01": 7, 4294967295: 8, 4294967294: 9 };
    o[long] = i;
    var s = keysOf(o) + "|" + Object.keys(o).join(",");
    h = (h * 31 + s.length) | 0;
    last = s;
  }
  show("order", h + " " + last.replace(long, "LONG").replace(long, "LONG"));
})();

// 2. deleting keys not yet visited, re-adding, and adding during the loop
(function () {
  var seen = [];
  for (var i = 0; i < N; i++) {
    var o = { a: 1, b: 2, c: 3, d: 4 };
    var ks = [];
    for (var k in o) {
      ks.push(k);
      if (k === "a") { delete o.c; o.e = 5; }
      if (k === "b" && i % 2) { o.c = 9; }
    }
    if (i < 2) seen.push(ks.join(""));
  }
  show("mutate", seen.join(" "));
})();

// 3. non-enumerable, accessor and symbol-keyed properties
(function () {
  var r = "";
  for (var i = 0; i < N; i++) {
    var o = { a: 1 };
    Object.defineProperty(o, "hidden", { value: 2, enumerable: false });
    Object.defineProperty(o, "acc", { get: function () { return 3; }, enumerable: true, configurable: true });
    o[Symbol.iterator] = 4;
    o.z = 5;
    r = keysOf(o) + "|" + Object.keys(o).join(",");
  }
  show("attrs", r);
})();

// 4. Object.prototype gains an enumerable key part way through, then loses it
(function () {
  var counts = [];
  for (var i = 0; i < N; i++) {
    if (i === 1000) Object.prototype.extra = 1;
    if (i === 2000) delete Object.prototype.extra;
    var o = { p: 1, q: 2 };
    var n = 0; for (var k in o) n++;
    if (i === 999 || i === 1000 || i === 1999 || i === 2000) counts.push(n + ":" + Object.keys(o).length);
  }
  show("proto-extra", counts.join(" "));
})();

// 5. prototypes other than the default, class instances, null-prototype objects
(function () {
  class P { constructor() { this.own = 1; } method() {} }
  P.prototype.shared = 2;
  var base = { inherited: 1 };
  var r = [];
  for (var i = 0; i < N; i++) {
    var a = Object.create(base); a.own = 2;
    var b = new P();
    var c = Object.create(null); c.x = 1; c.y = 2;
    if (i === N - 1) r.push(keysOf(a), Object.keys(a).join(","), keysOf(b), Object.keys(b).join(","), keysOf(c), Object.keys(c).join(","));
  }
  show("chains", r.join(" | "));
})();

// 6. many shapes, one object each, and objects that leave their shape
(function () {
  var h = 0;
  for (var i = 0; i < 6000; i++) {
    var o = {}; o["s" + (i % 5000)] = 1; o.t = 2;
    if (i % 3 === 0) delete o.t;
    if (i % 7 === 0) Object.freeze(o);
    h = (h * 31 + keysOf(o).length + Object.keys(o).length) | 0;
  }
  show("shapes", h);
})();

// 7. the values read through the keys, and a value replaced during the loop
(function () {
  var sum = 0;
  for (var i = 0; i < N; i++) {
    var o = { a: i, b: 2 * i, c: 3 };
    for (var k in o) { if (k === "a") o.b = -1; sum = (sum + o[k]) | 0; }
  }
  show("values", sum);
})();

// 8. arrays, strings and functions in the same loops
(function () {
  var r = "";
  for (var i = 0; i < N; i++) {
    var arr = [1, 2]; arr.x = 3;
    var f = function () {}; f.y = 1;
    r = keysOf(arr) + "|" + keysOf("ab") + "|" + keysOf(f) + "|" + Object.keys(arr).join(",") + "|" + Object.keys(f).join(",");
  }
  show("others", r);
})();

// 9. dictionary-mode objects (a key deleted before the loop): deletes,
// re-adds and adds during the loop, sixty keys, integer-like keys
(function () {
  var r = [], h = 0;
  for (var i = 0; i < N; i++) {
    var o = {};
    for (var p = 0; p < 60; p++) o["prop_" + p] = p;
    o[7] = "seven"; o[2] = "two";
    delete o.prop_0;
    var ks = [];
    for (var k in o) {
      ks.push(k);
      if (k === "prop_1") { delete o.prop_5; delete o.prop_2; o.prop_2 = "back"; o.added = 1; }
      if (k === "prop_10" && i % 2) delete o.prop_59;
    }
    h = (h * 31 + ks.length) | 0;
    if (i < 2) r.push(ks.slice(0, 8).join(",") + "..." + ks.slice(-3).join(",") + "#" + ks.length);
  }
  show("dict-mode", h + " " + r.join(" | "));
})();

console.log(out.join("\n"));
"####;

#[test]
fn shaped_enumeration_matches_node() {
    let expected: &[&str] = &[
        r####"order: -1487712896 1,2,4294967294,b,a,@@x,-1,01,4294967295,LONG|1,2,4294967294,b,a,@@x,-1,01,4294967295,LONG"####,
        r####"mutate: abd abcd"####,
        r####"attrs: a,acc,z|a,acc,z"####,
        r####"proto-extra: 2:2 3:2 3:2 2:2"####,
        r####"chains: own,inherited | own | own,shared | own | x,y | x,y"####,
        r####"shapes: 3702592"####,
        r####"values: 4504500"####,
        r####"others: 0,1,x|0,1|y|0,1,x|y"####,
        r####"dict-mode: -420091228 2,7,prop_1,prop_2,prop_3,prop_4,prop_6,prop_7...prop_57,prop_58,prop_59#60 | 2,7,prop_1,prop_2,prop_3,prop_4,prop_6,prop_7...prop_56,prop_57,prop_58#59"####,
    ];
    let out = run(PROGRAM);
    let got: Vec<&str> = out.iter().flat_map(|chunk| chunk.lines()).collect();
    assert_eq!(got.len(), expected.len(), "{got:#?}");
    for (got, want) in got.iter().zip(expected) {
        assert_eq!(got, want);
    }
}
