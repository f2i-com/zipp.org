//! B207 (the B194–B206 adversarial review): the fused `Math.random()*k|0`
//! lane raw-accesses the recognized override's STATE GLOBAL — a slot that
//! never appears in the caller's own bytecode, so entry revalidation cannot
//! see a route change on it. The route-epoch guard added by the review is
//! what declines the lane once `globalThis`'s own properties are redefined.
//!
//! The expected line is node-oracled (v24.12.0): after the accessor
//! redefinition, every xorshift step must read the getter's value and fire
//! the setter — 50 ids × 21 chars × 3 steps = 3150 setter hits.

const SRC: &str = r#"
seed = 1;
Math.random = function () {
  seed ^= seed << 13;
  seed ^= seed >>> 17;
  seed ^= seed << 5;
  return (seed >>> 0) / 4294967296;
};
var A = "useandom26T198340PX75pxJACKVERYMINDBUSHWOLFGQZbfghjklqvwyzrict00";
function gen() {
  var s = "";
  var i = 21;
  while (i-- > 0) {
    s += A[(Math.random() * 64) | 0];
  }
  return s;
}
var acc = 0;
for (var k = 0; k < 200; k++) {
  var id = gen();
  for (var j = 0; j < id.length; j++) acc = (Math.imul(acc, 31) + id.charCodeAt(j)) | 0;
}
var log = 0;
Object.defineProperty(globalThis, "seed", {
  get: function () { return 7; },
  set: function (v) { log++; },
});
for (var k2 = 0; k2 < 50; k2++) {
  var id2 = gen();
  for (var j2 = 0; j2 < id2.length; j2++) acc = (Math.imul(acc, 31) + id2.charCodeAt(j2)) | 0;
}
console.log(acc + " " + log + " " + seed);
"#;

const EXPECTED: &str = "-424228910 3150 7";

#[test]
fn fused_random_lane_declines_after_state_global_route_change() {
    let out = zipp_vm::run(SRC).expect("runs");
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(out.output, vec![EXPECTED.to_string()]);
}
