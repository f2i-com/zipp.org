//! A compiled loop that builds strings and makes no call must still collect.
//!
//! The collector runs at safe points: loop back-edges and frame transitions
//! in the interpreter, and the helpers compiled code calls. Objects and
//! arrays made in a compiled loop are collected through their allocation
//! helpers' polls, but string concatenation (`jit_concat`, and the fused
//! `a + (b + c)` helpers) polled nothing, so `var x = "k" + i` three million
//! times in a compiled function ran one collection, at the end, and held
//! 675 MB where Node holds 30. `Promise.resolve(i)` (`jit_static_fn`) was
//! the same. The helpers now poll on entry, while their operands still sit
//! in the caller's frame window.
//!
//! One test function: the counters are process-wide, and tests of one binary
//! run on parallel threads.

#[test]
fn string_building_loops_collect() {
    let src = r#"
function keys(n) {
  var s = 0;
  for (var i = 0; i < n; i++) { var x = "k" + i; s = (s + x.length) | 0; }
  return s;
}
function pairs(n) {
  var t = 0;
  for (var i = 0; i < n; i++) { var x = "<td>" + ("p" + i); t = (t + x.length) | 0; }
  return t;
}
function promises(n) {
  var c = 0;
  for (var i = 0; i < n; i++) { var p = Promise.resolve(i); c = (c + 1) | 0; }
  return c;
}
var out = keys(1500000) + pairs(1500000) + promises(1500000);
var html = "";
for (var r = 0; r < 300; r++) {
  html = "";
  for (var i = 0; i < 1000; i++) html += "<td>" + i;
  out = (out + html.length) | 0;
}
console.log(out);
"#;
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected error: {:?}", out.error);
    assert_eq!(out.output, vec!["31344780".to_string()]);
    let (minors, majors, _, _, _, _, _, peak_slots) = zipp_vm::gc_nursery_stats();
    // About six million strings and ropes: unbounded, the heap reaches
    // millions of slots and runs a handful of collections.
    assert!(minors + majors >= 50, "{minors} minors and {majors} majors");
    assert!(peak_slots < 1_000_000, "peak slots {peak_slots}");
}
