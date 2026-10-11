//! Element stores into arrays that have survived a collection.
//!
//! Compiled code stored into a pinned dense array inline only while the
//! array was young; an old one went to a helper for the write barrier,
//! whatever was being stored. A number needs no barrier, so every fill of a
//! long-lived array of numbers paid a call an element: a million-element fill
//! took 15 ms after a collection and 4 ms before one, and
//! `bench/real/sparse-array` moved by 13% with where its first collection
//! fell. The inline store now also takes a value that is not a heap value
//! into an old array.
//!
//! What must hold: a heap value stored into an old array still goes through
//! the barrier (every object stored here must be intact after the
//! collections that follow), and a hole is still only filled where the
//! array's flags allow it (a frozen array refuses, a setter on the prototype
//! runs). The expected lines are Node 24's output for the same program.

use zipp_vm::embed;

fn run(source: &str) -> Vec<String> {
    let mut state = embed::compile_script(source).expect("source compiles");
    #[cfg(feature = "safe-sandbox")]
    state.set_limits(4_000_000_000, None);
    state.run_init().expect("source runs");
    state.take_output()
}

const PROGRAM: &str = r####"// Element stores into arrays that have survived collections, from loops hot
// enough to be compiled. Numbers may be stored without a barrier; objects may
// not: every object stored into an old array must still be there, intact,
// after the collections that follow.
var out = [];
function churn(n) { var keep = null; for (var i = 0; i < n; i++) keep = { i: i, pad: [i, i + 1] }; return keep; }

// 1. numbers into an array made old by the garbage around it
(function () {
  var a = new Array(50000);
  for (var i = 0; i < a.length; i++) a[i] = i;
  churn(200000);
  var s = 0;
  for (var r = 0; r < 6; r++) {
    for (var i = 0; i < a.length; i++) a[i] = (a[i] * 3 + r) % 1009;
    churn(30000);
  }
  for (var i = 0; i < a.length; i++) s = (s + a[i]) | 0;
  out.push("numbers " + s);
})();

// 2. young objects into an old array, with collections between and after
(function () {
  var a = new Array(20000);
  for (var i = 0; i < a.length; i++) a[i] = 0;
  churn(200000);
  var bad = 0;
  for (var r = 0; r < 5; r++) {
    for (var i = 0; i < a.length; i++) a[i] = (i + r) % 3 === 0 ? { v: i + r, tag: "o" + (i % 7) } : i + r;
    churn(60000);
    for (var i = 0; i < a.length; i++) {
      var x = a[i];
      if ((i + r) % 3 === 0) { if (!x || x.v !== i + r || x.tag !== "o" + (i % 7)) bad++; }
      else if (x !== i + r) bad++;
    }
  }
  out.push("objects bad=" + bad);
})();

// 3. strings, booleans, null and undefined into an old array
(function () {
  var a = new Array(9000);
  for (var i = 0; i < a.length; i++) a[i] = i;
  churn(200000);
  var kinds = [true, false, null, undefined, 1.5, -0, NaN, "lit"];
  for (var r = 0; r < 4; r++) {
    for (var i = 0; i < a.length; i++) a[i] = i % 9 === 8 ? "s" + i + "-" + r : kinds[i % 8];
    churn(40000);
  }
  var h = 0;
  for (var i = 0; i < a.length; i++) {
    var x = a[i];
    h = (h * 31 + (typeof x === "string" ? x.length : x === null ? 3 : x === undefined ? 5 : x !== x ? 7 : typeof x === "boolean" ? (x ? 11 : 13) : Object.is(x, -0) ? 17 : 19)) | 0;
  }
  out.push("kinds " + h + " " + a[8] + " " + a[8999]);
})();

// 4. holes in an old array: filled, and the fill refused where it must be
(function () {
  var a = new Array(8000);
  churn(200000);
  var filled = 0;
  for (var i = 0; i < a.length; i += 2) { a[i] = i; filled++; }
  var holes = 0;
  for (var i = 0; i < a.length; i++) if (!(i in a)) holes++;
  var b = new Array(8000);
  for (var i = 0; i < 4000; i++) b[i] = i;
  churn(200000);
  Object.freeze(b);
  var threw = 0;
  function strictStore(arr, i, v) { "use strict"; arr[i] = v; }
  for (var i = 0; i < 8000; i++) { try { strictStore(b, i, -1); } catch (e) { threw++; } }
  var c = new Array(6000);
  churn(200000);
  var setterHits = 0;
  Object.defineProperty(Array.prototype, 4321, { set: function (v) { setterHits++; }, get: function () { return "proto"; }, configurable: true });
  for (var i = 4000; i < 5000; i++) c[i] = i;
  var own = Object.prototype.hasOwnProperty.call(c, 4321);
  delete Array.prototype[4321];
  out.push("holes filled=" + filled + " left=" + holes + " frozenThrew=" + threw + " b0=" + b[0] + " b7999=" + b[7999] + " setterHits=" + setterHits + " own4321=" + own);
})();

// 5. an old array of arrays: the inner arrays are replaced while collections run
(function () {
  var grid = [];
  for (var i = 0; i < 300; i++) grid.push([i]);
  churn(200000);
  var bad = 0;
  for (var r = 0; r < 40; r++) {
    for (var i = 0; i < grid.length; i++) grid[i] = [i, r, "row" + r];
    if (r % 4 === 0) churn(50000);
    for (var i = 0; i < grid.length; i++) if (grid[i][0] !== i || grid[i][1] !== r || grid[i][2] !== "row" + r) bad++;
  }
  out.push("grid bad=" + bad);
})();

console.log(out.join("\n"));
"####;

#[test]
fn stores_into_old_arrays_match_node() {
    let expected: &[&str] = &[
        r####"numbers 25201394"####,
        r####"objects bad=0"####,
        r####"kinds 360332691 s8-3 s8999-3"####,
        r####"holes filled=4000 left=4000 frozenThrew=8000 b0=0 b7999=undefined setterHits=1 own4321=false"####,
        r####"grid bad=0"####,
    ];
    let out = run(PROGRAM);
    let got: Vec<&str> = out.iter().flat_map(|chunk| chunk.lines()).collect();
    assert_eq!(got.len(), expected.len(), "{got:#?}");
    for (got, want) in got.iter().zip(expected) {
        assert_eq!(got, want);
    }
}
