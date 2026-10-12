//! `o[k]` with a string key predicts the slot from `Vm::keyed_slot_memo`
//! (filed under the receiver's shape, or the receiver itself in dictionary
//! mode) and verifies the prediction with one key compare before using it.
//!
//! Nothing about a prediction may be trusted: the cases below make each one
//! go stale. Many objects of one shape and objects of other shapes (other
//! slots for the same key) interleaved; a key redefined as an accessor or as
//! read-only, deleted, re-added, and the object frozen; key strings appended
//! to in place between accesses; dictionary-mode objects churned while read;
//! and prototypes, null prototypes, class instances and the global object.
//! The loops run past the tier-up count. The expected lines are Node 24's
//! output for the same program.

use zipp_vm::embed;

fn run(source: &str) -> Vec<String> {
    let mut state = embed::compile_script(source).expect("source compiles");
    #[cfg(feature = "safe-sandbox")]
    state.set_limits(4_000_000_000, None);
    state.run_init().expect("source runs");
    state.take_output()
}

const PROGRAM: &str = r####"// Keyed reads and writes whose predicted slot (by shape, or by object in
// dictionary mode) can go stale: every line must print what Node prints.
var N = 4000;
var out = [];
function show(name, v) { out.push(name + ": " + v); }

// 1. many objects of one shape, read through the same keys
(function () {
  var keys = ["a", "b", "c", "d"];
  var objs = [];
  for (var i = 0; i < 300; i++) objs.push({ a: i, b: 2 * i, c: "x" + i, d: i % 3 });
  var s = 0;
  for (var r = 0; r < N; r++) { var o = objs[r % 300]; var k = keys[r & 3]; var v = o[k]; s = (s + (typeof v === "number" ? v : v.length)) | 0; o[k] = typeof v === "number" ? v + 1 : v; }
  show("same-shape", s);
})();

// 2. the same keys on objects of other shapes (other slots), interleaved
(function () {
  var shapes = [{ a: 1, b: 2 }, { b: 3, a: 4 }, { z: 0, a: 5, b: 6 }, { b: 7 }, { a: 8 }];
  var s = 0;
  for (var r = 0; r < N; r++) {
    var o = shapes[r % 5];
    var k = r & 1 ? "a" : "b";
    var v = o[k];
    s = (s + (v === undefined ? 100 : v)) | 0;
    if (v !== undefined) o[k] = v;
  }
  show("other-slots", s);
})();

// 3. a key that becomes an accessor, read-only, deleted, re-added; an object frozen
(function () {
  var o = { p: 1, q: 2, r: 3 };
  var gets = 0, s = 0, threw = 0;
  function strictSet(obj, k, v) { "use strict"; obj[k] = v; }
  for (var r = 0; r < N; r++) {
    var k = r % 3 === 0 ? "p" : r % 3 === 1 ? "q" : "r";
    if (r === 1000) Object.defineProperty(o, "q", { get: function () { gets++; return 20; }, set: function (v) { s += 1000; }, configurable: true });
    if (r === 2000) Object.defineProperty(o, "r", { value: 30, writable: false, configurable: true });
    if (r === 2500) delete o.p;
    if (r === 3000) o.p = 7;
    if (r === 3500) Object.freeze(o);
    var v = o[k];
    s = (s + (v === undefined ? 1 : v)) | 0;
    try { strictSet(o, k, (v | 0) + 1); } catch (e) { threw++; }
  }
  show("redefine", s + " gets=" + gets + " threw=" + threw + " final=" + JSON.stringify(Object.getOwnPropertyDescriptors(o)).length);
})();

// 4. key strings appended to in place, and keys that are ropes
(function () {
  var o = { ab: 1, abc: 2, abcd: 3 };
  var s = 0;
  for (var r = 0; r < N; r++) {
    var k = "ab";
    s = (s + o[k]) | 0;
    k += "c";
    s = (s + o[k]) | 0;
    k += "d";
    s = (s + o[k]) | 0;
    o[k] = r;
  }
  show("appended-keys", s + " " + o.abcd);
})();

// 5. dictionary-mode objects churned while read, and many receivers
(function () {
  var s = 0;
  for (var r = 0; r < 2000; r++) {
    var o = {};
    for (var p = 0; p < 20; p++) o["k" + p] = p;
    delete o.k3;
    for (var p = 0; p < 20; p++) { var k = "k" + p; var v = o[k]; s = (s + (v === undefined ? 50 : v)) | 0; o[k] = (v | 0) + r; }
    delete o.k7; o.k3 = 1;
    for (var p = 0; p < 20; p++) { var v2 = o["k" + p]; s = (s + (v2 === undefined ? 9 : v2)) | 0; }
  }
  show("dict-churn", s);
})();

// 6. prototypes, null prototypes, class instances and the global object
(function () {
  var base = { inherited: 5 };
  var child = Object.create(base); child.own = 1;
  var bare = Object.create(null); bare.x = 2;
  class K { constructor() { this.f = 3; } }
  var kk = new K();
  globalThis.keyedMemoGlobal = 4;
  var s = 0;
  for (var r = 0; r < N; r++) {
    s = (s + child["own"] + child["inherited"] + bare["x"] + kk["f"] + globalThis["keyedMemoGlobal"]) | 0;
    if (r === 2000) { base.inherited = 6; globalThis.keyedMemoGlobal = 8; kk.f = 9; }
    child["own"] = r & 3;
  }
  delete globalThis.keyedMemoGlobal;
  show("chains", s);
})();

console.log(out.join("\n"));
"####;

#[test]
fn keyed_access_with_stale_predictions_matches_node() {
    let expected: &[&str] = &[
        r####"same-shape: 461646"####,
        r####"other-slots: 94400"####,
        r####"redefine: 1725376 gets=1000 threw=834 final=193"####,
        r####"appended-keys: 8006004 3999"####,
        r####"dict-churn: 36836000"####,
        r####"chains: 83987"####,
    ];
    let out = run(PROGRAM);
    let got: Vec<&str> = out.iter().flat_map(|chunk| chunk.lines()).collect();
    assert_eq!(got.len(), expected.len(), "{got:#?}");
    for (got, want) in got.iter().zip(expected) {
        assert_eq!(got, want);
    }
}
